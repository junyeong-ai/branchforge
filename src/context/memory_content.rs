//! Memory content data carrier.
//!
//! [`MemoryContent`] is a generic, domain-neutral container for textual
//! memory that feeds the agent's system prompt. It is produced by various
//! loaders (a generic markdown scanner, the Claude Code `CLAUDE.md`
//! convention loader, or in-memory providers) and consumed by
//! `PromptOrchestrator` and `StaticContext`.
//!
//! The field names (`claude_md`, `local_md`, `rule_indices`) reflect the
//! original Claude Code convention and are preserved here as a stable API
//! surface across the codebase. A future naming pass (Phase 6 N1/N2) may
//! rename these to fully domain-neutral terms (`shared`, `local`, `rules`)
//! once the rest of the codebase has converged on the Extensions-based
//! context propagation pattern.
//!
//! # Layering
//!
//! This struct is Layer 1 (pure core): it has no filesystem or shell
//! dependency. Loaders that *populate* it (such as the Layer 2b
//! `MemoryLoader` that walks a project tree looking for CLAUDE.md) live
//! in upper layers behind feature gates.

use super::rule_index::RuleIndex;

/// Loaded memory content to feed into an agent's system prompt.
#[derive(Debug, Default, Clone)]
pub struct MemoryContent {
    /// Shared/team-visible instruction files (originally CLAUDE.md).
    pub claude_md: Vec<String>,
    /// User-private instruction files (originally CLAUDE.local.md). Not
    /// typically committed to version control.
    pub local_md: Vec<String>,
    /// Rule indices loaded from a rules directory (originally
    /// `.claude/rules/`).
    pub rule_indices: Vec<RuleIndex>,
}

impl MemoryContent {
    /// Combine all shared and local content into a single newline-separated
    /// string, skipping empty entries.
    pub fn combined_claude_md(&self) -> String {
        self.claude_md
            .iter()
            .chain(self.local_md.iter())
            .filter(|c| !c.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Returns true if no content was loaded.
    pub fn is_empty(&self) -> bool {
        self.claude_md.is_empty() && self.local_md.is_empty() && self.rule_indices.is_empty()
    }

    /// Merge another container into this one, appending its vectors.
    pub fn merge(&mut self, other: MemoryContent) {
        self.claude_md.extend(other.claude_md);
        self.local_md.extend(other.local_md);
        self.rule_indices.extend(other.rule_indices);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_reports_no_content() {
        let content = MemoryContent::default();
        assert!(content.is_empty());
        assert!(content.combined_claude_md().is_empty());
    }

    #[test]
    fn merge_concatenates_all_fields() {
        let mut a = MemoryContent {
            claude_md: vec!["one".into()],
            local_md: vec!["alpha".into()],
            rule_indices: Vec::new(),
        };
        let b = MemoryContent {
            claude_md: vec!["two".into()],
            local_md: vec!["beta".into()],
            rule_indices: Vec::new(),
        };
        a.merge(b);
        assert_eq!(a.claude_md.len(), 2);
        assert_eq!(a.local_md.len(), 2);
    }

    #[test]
    fn combined_skips_empty_entries() {
        let content = MemoryContent {
            claude_md: vec!["first".into(), "   ".into(), "second".into()],
            local_md: vec!["third".into()],
            rule_indices: Vec::new(),
        };
        let combined = content.combined_claude_md();
        assert!(combined.contains("first"));
        assert!(combined.contains("second"));
        assert!(combined.contains("third"));
        // Blank entry stripped.
        assert_eq!(combined.matches("\n\n").count(), 2);
    }
}
