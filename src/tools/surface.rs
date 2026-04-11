//! Tool access control definitions.
//!
//! `ToolSurface` names a *policy shape* — the set of tools an agent is
//! allowed to invoke. The policy is layered in lockstep with the feature
//! hierarchy documented in `docs/architecture/layering.md`:
//!
//! ```text
//! Core ⊂ LocalFs ⊂ Coding
//! ```
//!
//! - [`ToolSurface::Core`] — always-on Layer 1 primitives: skills, planning,
//!   task tracking, graph navigation, and subagent delegation. No filesystem,
//!   no shell. Works in pure-core builds.
//! - [`ToolSurface::LocalFs`] — `Core` plus Layer 2a filesystem tools
//!   (`Read`, `Write`, `Edit`, `Glob`, `Grep`). Requires the `local-fs`
//!   feature for the tools themselves to be available.
//! - [`ToolSurface::Coding`] — `LocalFs` plus Layer 2b shell tools (`Bash`,
//!   `KillShell`). Requires the `coding-tools` feature (which transitively
//!   enables `local-fs`).
//! - [`ToolSurface::All`] — every registered tool, including MCP and
//!   custom tools.
//! - [`ToolSurface::Only`] / [`ToolSurface::Except`] — explicit allow/deny
//!   lists for fine-grained control.
//!
//! The `*_tools()` static helpers return the tool name list for each tier;
//! they are compile-time constants and are safe to call in any build.

use std::collections::HashSet;

use crate::authorization::{ToolPolicy, ToolRule};
use crate::common::matches_tool_pattern;

/// Controls which tools are available to the agent.
#[derive(Debug, Clone, Default)]
pub enum ToolSurface {
    /// No tools are allowed.
    None,
    /// Layer 1 runtime primitives only: `Skill`, `Plan`, `TodoWrite`,
    /// `GraphHistory`, `Task`, `TaskOutput`. Works in pure-core builds.
    #[default]
    Core,
    /// `Core` plus Layer 2a filesystem tools (`Read`, `Write`, `Edit`,
    /// `Glob`, `Grep`). The tools themselves are only compiled when the
    /// `local-fs` feature is active; under a pure-core build this variant
    /// behaves identically to `Core`.
    LocalFs,
    /// `LocalFs` plus Layer 2b shell tools (`Bash`, `KillShell`). The
    /// tools themselves are only compiled when the `coding-tools` feature
    /// is active; under smaller builds this variant degrades gracefully to
    /// whichever subset is available.
    Coding,
    /// All registered tools are allowed.
    All,
    /// Only the specified tools are allowed. Supports scoped patterns such
    /// as `Bash(git:*)`.
    Only(HashSet<String>),
    /// All tools except the specified ones are allowed.
    Except(HashSet<String>),
}

impl ToolSurface {
    /// Returns the Layer 1 core tool names.
    ///
    /// These are the runtime primitives every agent (pure-core or otherwise)
    /// can reach: skill execution, planning scaffolding, todo tracking,
    /// session graph navigation, and subagent delegation. They have no
    /// filesystem or shell requirement.
    pub const fn core_tool_names() -> &'static [&'static str] {
        &[
            "Skill",
            "Plan",
            "TodoWrite",
            "GraphHistory",
            "Task",
            "TaskOutput",
        ]
    }

    /// Returns the Layer 2a (`local-fs`) tool names — filesystem read,
    /// write, edit, pattern-match, and content search.
    pub const fn local_fs_tool_names() -> &'static [&'static str] {
        &["Read", "Write", "Edit", "Glob", "Grep"]
    }

    /// Returns the Layer 2b (`coding-tools`) tool names — shell execution
    /// and process management.
    pub const fn coding_tool_names() -> &'static [&'static str] {
        &["Bash", "KillShell"]
    }

    /// Legacy entry point that returns the *effective* core tool set for
    /// the current build: always the Layer 1 primitives, plus Layer 2a
    /// filesystem tools when the `local-fs` feature is enabled, plus
    /// Layer 2b shell tools when `coding-tools` is enabled.
    ///
    /// Prefer [`core_tool_names`][Self::core_tool_names],
    /// [`local_fs_tool_names`][Self::local_fs_tool_names], and
    /// [`coding_tool_names`][Self::coding_tool_names] when you need the
    /// layer-specific list without feature branching.
    #[allow(unused_mut)]
    pub fn core_tools() -> Vec<&'static str> {
        let mut tools: Vec<&'static str> = Self::core_tool_names().to_vec();
        #[cfg(feature = "local-fs")]
        tools.extend(Self::local_fs_tool_names());
        #[cfg(feature = "coding-tools")]
        tools.extend(Self::coding_tool_names());
        tools
    }

    /// All tool names that belong to the `LocalFs` surface under the
    /// current build configuration (Core + filesystem tools when available).
    pub fn local_fs_surface_tools() -> Vec<&'static str> {
        #[allow(unused_mut)]
        let mut tools: Vec<&'static str> = Self::core_tool_names().to_vec();
        #[cfg(feature = "local-fs")]
        tools.extend(Self::local_fs_tool_names());
        tools
    }

    /// All tool names that belong to the `Coding` surface under the
    /// current build configuration (LocalFs + shell tools when available).
    pub fn coding_surface_tools() -> Vec<&'static str> {
        #[allow(unused_mut)]
        let mut tools = Self::local_fs_surface_tools();
        #[cfg(feature = "coding-tools")]
        tools.extend(Self::coding_tool_names());
        tools
    }

    pub fn all() -> Self {
        Self::All
    }

    pub fn none() -> Self {
        Self::None
    }

    pub fn core() -> Self {
        Self::Core
    }

    /// Build a [`LocalFs`][Self::LocalFs] surface — a local-machine general
    /// agent. Intended for research, knowledge-management, and data-analysis
    /// agents that need filesystem tools without shell execution.
    pub fn local_fs() -> Self {
        Self::LocalFs
    }

    /// Build a [`Coding`][Self::Coding] surface — a full coding agent with
    /// filesystem and shell access.
    pub fn coding() -> Self {
        Self::Coding
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
            Self::Core => Self::core_tool_names().contains(&tool_name),
            Self::LocalFs => Self::local_fs_surface_tools().contains(&tool_name),
            Self::Coding => Self::coding_surface_tools().contains(&tool_name),
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
                for tool in Self::core_tool_names() {
                    builder = builder.allow(tool);
                }
                builder.build()
            }
            Self::LocalFs => {
                for tool in Self::local_fs_surface_tools() {
                    builder = builder.allow(tool);
                }
                builder.build()
            }
            Self::Coding => {
                for tool in Self::coding_surface_tools() {
                    builder = builder.allow(tool);
                }
                builder.build()
            }
            Self::All => builder.allow(".*").build(),
            Self::Only(allowed) => {
                for pattern in allowed {
                    builder = builder.allow(pattern);
                }
                builder.build()
            }
            Self::Except(denied) => {
                let mut policy = builder.allow(".*").build();
                policy
                    .rules
                    .extend(denied.iter().map(|pattern| ToolRule::deny(pattern)));
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
    fn test_core_contains_runtime_primitives() {
        // Core is Layer 1 — the always-on runtime primitives. It works in
        // pure-core builds because none of these tools need filesystem or
        // shell access.
        let access = ToolSurface::core();
        assert!(access.is_allowed("Skill"));
        assert!(access.is_allowed("Plan"));
        assert!(access.is_allowed("TodoWrite"));
        assert!(access.is_allowed("GraphHistory"));
        assert!(access.is_allowed("Task"));
        assert!(access.is_allowed("TaskOutput"));
    }

    #[test]
    fn test_core_denies_layer_2_tools() {
        // Filesystem and shell tools are NOT in Core regardless of feature
        // activation — that separation is the whole point of the layering.
        let access = ToolSurface::core();
        assert!(!access.is_allowed("Read"));
        assert!(!access.is_allowed("Write"));
        assert!(!access.is_allowed("Edit"));
        assert!(!access.is_allowed("Glob"));
        assert!(!access.is_allowed("Grep"));
        assert!(!access.is_allowed("Bash"));
        assert!(!access.is_allowed("KillShell"));
    }

    #[test]
    fn test_local_fs_extends_core_with_filesystem_tools() {
        let access = ToolSurface::local_fs();
        // Core tools remain available.
        assert!(access.is_allowed("Skill"));
        assert!(access.is_allowed("Plan"));
        assert!(access.is_allowed("TodoWrite"));
        // Shell tools still denied.
        assert!(!access.is_allowed("Bash"));
        assert!(!access.is_allowed("KillShell"));
    }

    /// Filesystem tools are only surfaced by `LocalFs` when the `local-fs`
    /// feature is enabled; in pure-core builds `LocalFs` and `Core` are
    /// equivalent by design.
    #[cfg(feature = "local-fs")]
    #[test]
    fn test_local_fs_includes_filesystem_tools_when_feature_on() {
        let access = ToolSurface::local_fs();
        assert!(access.is_allowed("Read"));
        assert!(access.is_allowed("Write"));
        assert!(access.is_allowed("Edit"));
        assert!(access.is_allowed("Glob"));
        assert!(access.is_allowed("Grep"));
    }

    #[cfg(not(feature = "local-fs"))]
    #[test]
    fn test_local_fs_degrades_to_core_without_feature() {
        let access = ToolSurface::local_fs();
        assert!(!access.is_allowed("Read"));
        assert!(!access.is_allowed("Write"));
        // Still has the Core tools.
        assert!(access.is_allowed("Skill"));
    }

    #[test]
    fn test_coding_extends_local_fs() {
        let access = ToolSurface::coding();
        // Everything from Core is allowed.
        assert!(access.is_allowed("Skill"));
        assert!(access.is_allowed("Plan"));
    }

    #[cfg(feature = "coding-tools")]
    #[test]
    fn test_coding_includes_shell_tools_when_feature_on() {
        let access = ToolSurface::coding();
        // Shell tools are allowed under `coding-tools`.
        assert!(access.is_allowed("Bash"));
        assert!(access.is_allowed("KillShell"));
        // Transitively includes Layer 2a filesystem tools.
        assert!(access.is_allowed("Read"));
        assert!(access.is_allowed("Write"));
    }

    #[cfg(not(feature = "coding-tools"))]
    #[test]
    fn test_coding_degrades_without_feature() {
        let access = ToolSurface::coding();
        assert!(!access.is_allowed("Bash"));
        assert!(!access.is_allowed("KillShell"));
    }

    #[test]
    fn test_none_denies_everything() {
        let access = ToolSurface::none();
        assert!(!access.is_allowed("Read"));
        assert!(!access.is_allowed("Write"));
        assert!(!access.is_allowed("Skill"));
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

    #[test]
    fn test_tool_name_constants_are_distinct() {
        // Guards against accidental duplication between layers, which
        // would be a symptom of a mis-annotated tool.
        let core: HashSet<_> = ToolSurface::core_tool_names().iter().collect();
        let local_fs: HashSet<_> = ToolSurface::local_fs_tool_names().iter().collect();
        let coding: HashSet<_> = ToolSurface::coding_tool_names().iter().collect();
        assert!(core.is_disjoint(&local_fs));
        assert!(core.is_disjoint(&coding));
        assert!(local_fs.is_disjoint(&coding));
    }
}
