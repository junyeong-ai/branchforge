//! Tool access control definitions.

use std::collections::HashSet;

use crate::authorization::{ToolPolicy, ToolRule};
use crate::common::matches_tool_pattern;

/// Controls which tools are available to the agent.
#[derive(Debug, Clone, Default)]
pub enum ToolSurface {
    /// No tools are allowed.
    None,
    /// The minimal core tool surface.
    #[default]
    Core,
    /// All tools are allowed.
    All,
    /// Only the specified tools are allowed.
    Only(HashSet<String>),
    /// All tools except the specified ones are allowed.
    Except(HashSet<String>),
}

impl ToolSurface {
    pub const CORE_TOOLS: &[&str] = &[
        "Read",
        "Write",
        "Edit",
        "Glob",
        "Grep",
        "Bash",
        "KillShell",
        "Skill",
    ];

    pub fn all() -> Self {
        Self::All
    }

    pub fn none() -> Self {
        Self::None
    }

    pub fn core() -> Self {
        Self::Core
    }

    pub fn only(tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Only(tools.into_iter().map(Into::into).collect())
    }

    pub fn except(tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Except(tools.into_iter().map(Into::into).collect())
    }

    #[inline]
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        match self {
            Self::None => false,
            Self::Core => Self::CORE_TOOLS.contains(&tool_name),
            Self::All => true,
            Self::Only(allowed) => allowed
                .iter()
                .any(|pattern| matches_tool_pattern(pattern, tool_name)),
            Self::Except(denied) => !denied
                .iter()
                .any(|pattern| matches_tool_pattern(pattern, tool_name)),
        }
    }

    pub fn default_policy(&self) -> ToolPolicy {
        let mut builder = ToolPolicy::builder();
        match self {
            Self::None => builder.build(),
            Self::Core => {
                for tool in Self::CORE_TOOLS {
                    builder = builder.allow(*tool);
                }
                builder.build()
            }
            Self::All => builder.allow(".*").build(),
            Self::Only(allowed) => {
                for pattern in allowed {
                    builder = builder.allow(pattern.clone());
                }
                builder.build()
            }
            Self::Except(denied) => {
                let mut policy = builder.allow(".*").build();
                policy.rules.extend(
                    denied
                        .iter()
                        .map(|pattern| ToolRule::deny_pattern(pattern.clone())),
                );
                policy
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_allows_everything() {
        let access = ToolSurface::all();
        assert!(access.is_allowed("Read"));
        assert!(access.is_allowed("Write"));
        assert!(access.is_allowed("AnythingElse"));
    }

    #[test]
    fn test_core_exposes_minimal_runtime_surface() {
        let access = ToolSurface::core();
        assert!(access.is_allowed("Read"));
        assert!(access.is_allowed("Skill"));
        assert!(!access.is_allowed("Task"));
        assert!(!access.is_allowed("TodoWrite"));
    }

    #[test]
    fn test_none_denies_everything() {
        let access = ToolSurface::none();
        assert!(!access.is_allowed("Read"));
        assert!(!access.is_allowed("Write"));
    }

    #[test]
    fn test_only_allows_specified() {
        let access = ToolSurface::only(["Read", "Write"]);
        assert!(access.is_allowed("Read"));
        assert!(access.is_allowed("Write"));
        assert!(!access.is_allowed("Bash"));
        assert!(!access.is_allowed("Edit"));
    }

    #[test]
    fn test_only_allows_scoped_pattern_base_tool() {
        let access = ToolSurface::only(["Bash(git:*)"]);
        assert!(access.is_allowed("Bash"));
    }

    #[test]
    fn test_except_denies_specified() {
        let access = ToolSurface::except(["Bash", "KillShell"]);
        assert!(access.is_allowed("Read"));
        assert!(access.is_allowed("Write"));
        assert!(!access.is_allowed("Bash"));
        assert!(!access.is_allowed("KillShell"));
    }

    #[test]
    fn test_except_denies_scoped_pattern_base_tool() {
        let access = ToolSurface::except(["Bash(git:*)"]);
        assert!(!access.is_allowed("Bash"));
        assert!(access.is_allowed("Read"));
    }
}
