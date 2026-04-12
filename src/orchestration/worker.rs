//! Worker specification and result types.

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

/// Constraints applied when spawning worker agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConstraints {
    /// Workers cannot access the coordinator's conversation history.
    pub isolated_context: bool,
    /// Worker prompts must be self-contained (no implicit context).
    pub self_contained_prompt: bool,
    /// Maximum number of concurrent workers.
    pub max_concurrent: usize,
}

impl Default for WorkerConstraints {
    fn default() -> Self {
        Self {
            isolated_context: true,
            self_contained_prompt: true,
            max_concurrent: 10,
        }
    }
}

/// Specification for creating a worker agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSpec {
    /// Worker name (used for directory lookup).
    pub name: String,
    /// Task description for the worker.
    pub prompt: String,
    /// Subagent type to use (e.g., "explore", "general").
    pub subagent_type: Option<String>,
    /// Model override for this worker.
    pub model: Option<String>,
    /// Tool restrictions for this worker.
    pub allowed_tools: Vec<String>,
}

/// Result from a completed worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerResult {
    /// Worker name.
    pub name: String,
    /// Whether the worker succeeded.
    pub success: bool,
    /// Output text from the worker.
    pub text: Option<String>,
    /// Error message if failed.
    pub error: Option<String>,
    /// Token usage.
    pub total_tokens: u64,
}

/// Collection of worker handles for batch management.
pub struct WorkerGroup {
    names: Vec<String>,
}

impl WorkerGroup {
    pub fn new() -> Self {
        Self { names: Vec::new() }
    }

    pub fn add(&mut self, name: impl Into<String>) {
        self.names.push(name.into());
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

impl Default for WorkerGroup {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_constraints() {
        let c = WorkerConstraints::default();
        assert!(c.isolated_context);
        assert!(c.self_contained_prompt);
        assert_eq!(c.max_concurrent, 10);
    }

    #[test]
    fn worker_group_tracking() {
        let mut group = WorkerGroup::new();
        assert!(group.is_empty());
        group.add("worker-1");
        group.add("worker-2");
        assert_eq!(group.len(), 2);
        assert_eq!(group.names(), &["worker-1", "worker-2"]);
    }
}
