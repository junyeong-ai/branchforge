//! Workspace — a Layer 1 primitive representing a filesystem root for agent operations.
//!
//! # What this is
//!
//! `Workspace` is an inert data carrier holding a filesystem path (the "root"
//! or "working directory") plus any metadata an agent may want to associate
//! with that root. It is stored in
//! [`AgentConfig::extensions`][crate::AgentConfig::extensions] and
//! [`ExecutionContext::extensions`][crate::tools::ExecutionContext::extensions]
//! via the [`Extensions`][crate::common::Extensions] type-keyed container, so
//! that Layer 2a (filesystem tools) and Layer 2b (coding tools) can retrieve
//! it without the Layer 1 `AgentConfig` struct itself having a
//! `working_dir: Option<PathBuf>` field — a field that would pollute pure-API
//! agents (customer support bots, server-side automations, chat UIs) that
//! have no notion of a local directory.
//!
//! # Why this is Layer 1 even though filesystem tools are Layer 2a
//!
//! `Workspace` contains no file I/O. It is a `struct Workspace { root: Arc<Path> }`
//! and three accessor methods. It can be constructed in pure-core code and
//! passed around without pulling in any filesystem operations. The operations
//! (opening files, validating paths, sandboxing) live in Layer 2a behind
//! `local-fs` feature. This split follows the principle that **data types
//! that Layer 2 needs can live in Layer 1 as long as they carry no Layer 2
//! behaviour** — otherwise Layer 1 would need to feature-gate its data
//! definitions, which would fragment the core API surface.
//!
//! See `docs/architecture/layering.md` §4 for the Extensions TypeMap pattern
//! and how `Workspace` participates.
//!
//! # Example
//!
//! ```
//! use branchforge::Workspace;
//! use std::path::PathBuf;
//!
//! let ws = Workspace::new("/home/alice/project");
//! assert_eq!(ws.root(), std::path::Path::new("/home/alice/project"));
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A filesystem root for agent operations.
///
/// Stored in [`Extensions`][crate::common::Extensions] under its own type
/// key. Tools that need filesystem access (Read/Write/Edit/Glob/Grep under
/// `local-fs`, Bash under `coding-tools`) retrieve it via
/// `ctx.extensions().get::<Workspace>()`. Tools that do not need filesystem
/// access ignore it entirely — this is the whole point of the Extensions
/// pattern.
///
/// `Workspace` is `Clone` (cheap: `Arc<Path>` internally) and `Send + Sync`.
#[derive(Clone, Debug)]
pub struct Workspace {
    root: Arc<Path>,
}

impl Workspace {
    /// Construct a workspace rooted at the given path.
    ///
    /// No validation is performed — callers that need existence, permission,
    /// or `realpath` semantics must invoke the appropriate Layer 2a helper
    /// (`SecureFs::new`, `Sandbox::with_root`, …) separately. This constructor
    /// is deliberately inert so it can be called from pure-core builder code.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Arc::<Path>::from(root.into().as_path()),
        }
    }

    /// Construct a workspace from an already-allocated `Arc<Path>`.
    ///
    /// Useful when the same root is shared across many extensions or
    /// sub-agents and you want to avoid a second allocation.
    pub fn from_arc(root: Arc<Path>) -> Self {
        Self { root }
    }

    /// Returns the filesystem root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the root as an owned `PathBuf`, allocating.
    ///
    /// Prefer [`root`][Self::root] if a borrowed path suffices.
    pub fn root_buf(&self) -> PathBuf {
        self.root.to_path_buf()
    }

    /// Returns a cheaply-cloneable reference-counted root.
    pub fn root_arc(&self) -> Arc<Path> {
        Arc::clone(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stores_root() {
        let ws = Workspace::new("/tmp/project");
        assert_eq!(ws.root(), Path::new("/tmp/project"));
    }

    #[test]
    fn new_accepts_pathbuf() {
        let buf = PathBuf::from("/tmp/a/b");
        let ws = Workspace::new(buf);
        assert_eq!(ws.root(), Path::new("/tmp/a/b"));
    }

    #[test]
    fn clone_shares_inner_arc() {
        let ws = Workspace::new("/tmp/shared");
        let cloned = ws.clone();
        // Both should point to identical arcs.
        assert!(Arc::ptr_eq(&ws.root_arc(), &cloned.root_arc()));
    }

    #[test]
    fn from_arc_reuses_allocation() {
        let arc: Arc<Path> = Arc::from(Path::new("/tmp/reuse"));
        let ws = Workspace::from_arc(Arc::clone(&arc));
        assert!(Arc::ptr_eq(&arc, &ws.root_arc()));
    }

    #[test]
    fn root_buf_allocates_owned() {
        let ws = Workspace::new("/tmp/x");
        let owned: PathBuf = ws.root_buf();
        assert_eq!(owned, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn stored_in_extensions() {
        use crate::common::Extensions;

        let mut ext = Extensions::new();
        ext.insert(Workspace::new("/tmp/ext"));
        let ws = ext.get::<Workspace>().expect("should retrieve");
        assert_eq!(ws.root(), Path::new("/tmp/ext"));
    }
}
