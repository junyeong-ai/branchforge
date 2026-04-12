//! Environment fact sources for system prompt construction.
//!
//! `EnvironmentSource` is a Layer 1 extension point for plugging additional
//! context into an agent's system prompt. A pure-core build exposes only
//! the trait and a small set of always-on facts (model, date, platform,
//! OS version). Layer 2a (`local-fs`) contributes a `WorkspaceEnvironmentSource`
//! that reports the current working directory and workspace metadata.
//! Layer 2b (`coding-tools`) contributes a `GitEnvironmentSource` that adds
//! git repository state. Third-party crates can implement their own sources
//! (for example to surface cloud-environment metadata, compliance tags, or
//! tenant identifiers) without touching the runtime.
//!
//! # Composition
//!
//! Sources collect into a list; a [`EnvironmentFact`] from each source is
//! keyed by its label and emitted in declaration order. Duplicate keys are
//! allowed — later sources can override earlier ones if they report the
//! same key, which matches how prompt builders want to handle layered
//! defaults.
//!
//! See `docs/architecture/layering.md` §2.1 for where each source lives.

#![allow(missing_docs)]

/// A single fact to include in the environment block of a system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentFact {
    /// Human-readable label shown verbatim in the prompt (e.g. "Platform",
    /// "Working directory").
    pub key: String,
    /// The fact's value.
    pub value: String,
    /// Declaration order hint — lower values are emitted first. Two facts
    /// with the same priority are emitted in the order their sources were
    /// registered. Defaults are spaced 10 apart so third-party sources can
    /// insert themselves between them without further plumbing.
    pub priority: i32,
}

impl EnvironmentFact {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            priority: 0,
        }
    }

    #[must_use]
    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }
}

/// A source of environment facts.
///
/// Implementations should be cheap to call — prompt generation may run
/// every turn. Sources that need expensive work (spawning a process, hitting
/// the network) should cache their results internally.
pub trait EnvironmentSource: Send + Sync {
    /// Human-readable source name used in tracing and diagnostics (e.g.
    /// "workspace", "git", "cloud-metadata").
    fn name(&self) -> &str;

    /// Return the facts this source contributes to the environment block.
    ///
    /// The default is empty — a source that has nothing to report in the
    /// current build configuration (for example a git source running on a
    /// non-repository directory) should return an empty vector rather than
    /// manufacturing "unknown" values.
    fn facts(&self) -> Vec<EnvironmentFact>;
}

/// Collect facts from several sources, sorting by priority.
///
/// This is a convenience helper for prompt builders that want to assemble
/// an environment block from a list of sources. Priority ties are broken by
/// the order in which the sources appear in `sources`.
pub fn collect_facts<'a, I>(sources: I) -> Vec<EnvironmentFact>
where
    I: IntoIterator<Item = &'a dyn EnvironmentSource>,
{
    let mut all: Vec<(usize, EnvironmentFact)> = Vec::new();
    for (idx, source) in sources.into_iter().enumerate() {
        for fact in source.facts() {
            all.push((idx, fact));
        }
    }
    all.sort_by(|a, b| a.1.priority.cmp(&b.1.priority).then_with(|| a.0.cmp(&b.0)));
    all.into_iter().map(|(_, fact)| fact).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Static {
        facts: Vec<EnvironmentFact>,
    }

    impl EnvironmentSource for Static {
        fn name(&self) -> &str {
            "static"
        }
        fn facts(&self) -> Vec<EnvironmentFact> {
            self.facts.clone()
        }
    }

    #[test]
    fn fact_builder_sets_fields() {
        let fact = EnvironmentFact::new("Platform", "darwin").priority(5);
        assert_eq!(fact.key, "Platform");
        assert_eq!(fact.value, "darwin");
        assert_eq!(fact.priority, 5);
    }

    #[test]
    fn default_priority_is_zero() {
        let fact = EnvironmentFact::new("x", "y");
        assert_eq!(fact.priority, 0);
    }

    #[test]
    fn collect_facts_sorts_by_priority_then_source_order() {
        let a = Static {
            facts: vec![
                EnvironmentFact::new("A1", "one").priority(10),
                EnvironmentFact::new("A2", "two").priority(0),
            ],
        };
        let b = Static {
            facts: vec![EnvironmentFact::new("B1", "three").priority(0)],
        };
        let sources: [&dyn EnvironmentSource; 2] = [&a, &b];
        let collected = collect_facts(sources);
        // Priority 0 comes first; tie broken by source order (a before b).
        assert_eq!(collected[0].key, "A2");
        assert_eq!(collected[1].key, "B1");
        assert_eq!(collected[2].key, "A1");
    }

    #[test]
    fn empty_source_list_yields_empty() {
        let sources: [&dyn EnvironmentSource; 0] = [];
        let result = collect_facts(sources);
        assert!(result.is_empty());
    }
}
