//! Test utilities for tools module.
//!
//! These helpers construct a real filesystem-backed `ExecutionContext` via
//! `from_path`, so they are only available when `coding-tools` is enabled
//! (which transitively enables `local-fs`). Their only consumer is the
//! Layer 2b `BashTool` test suite. Pure Layer 1 tests construct their
//! contexts directly via `ExecutionContext::empty()` and do not use this
//! module.

#[cfg(all(test, feature = "coding-tools"))]
pub mod helpers {
    use crate::tools::ExecutionContext;
    use std::path::PathBuf;
    use tempfile::TempDir;

    pub struct TestContext {
        pub dir: TempDir,
        pub context: ExecutionContext,
    }

    impl TestContext {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().expect("Failed to create temp directory");
            let root = std::fs::canonicalize(dir.path()).expect("Failed to canonicalize path");
            let context =
                ExecutionContext::from_path(&root).expect("Failed to create ExecutionContext");
            Self { dir, context }
        }

        pub fn write_file(&self, name: &str, content: &str) -> PathBuf {
            let path = self.dir.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("Failed to create parent directories");
            }
            std::fs::write(&path, content).expect("Failed to write file");
            std::fs::canonicalize(&path).expect("Failed to canonicalize path")
        }
    }

    impl Default for TestContext {
        fn default() -> Self {
            Self::new()
        }
    }
}
