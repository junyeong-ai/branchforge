//! Built-in subagent definitions, split across the 3-layer architecture.
//!
//! - `general_purpose_subagent` — Layer 1, always available. Has no tool
//!   restrictions; it inherits whatever tools the runtime registers, so it
//!   works equally well for a pure-API agent, a local research agent, or a
//!   full coding agent.
//! - `explore_subagent`, `plan_subagent` — Layer 2a (`local-fs`). Restrict
//!   themselves to `Read` / `Grep` / `Glob` / `TodoWrite` — file-exploration
//!   and planning primitives that are legitimate for *any* local-machine
//!   agent, not only coding ones. A research agent reading markdown notes,
//!   a data analyst inspecting CSV fixtures, or a knowledge worker grepping
//!   a personal wiki all benefit from the same tool set.
//! - `bash_subagent` — Layer 2b (`coding-tools`). Exclusively runs shell
//!   commands. This is coding-specific territory because bash execution
//!   carries a categorically different security posture than read-only
//!   filesystem traversal.
//!
//! The split ensures that enabling `local-fs` without `coding-tools`
//! produces a functional research-agent topology out of the box (`explore`
//! and `plan` are both available, `bash` is not), while a pure-core build
//! still has `general_purpose` as a baseline.
//!
//! Subagent names use lowercase convention (matching skills and execution
//! modes), distinct from tool names which use PascalCase.

use super::SubagentIndex;
use crate::agent::ModelType;
use crate::common::{ContentSource, SourceType};

// =============================================================================
// Layer 1 — always-on
// =============================================================================

/// General-purpose agent — full capability for complex tasks.
///
/// Has no tool restrictions, meaning it inherits whatever tool surface the
/// runtime registers. Works in any build (pure core, local-fs, coding).
pub fn general_purpose_subagent() -> SubagentIndex {
    SubagentIndex::new(
        "general",
        "General-purpose agent for researching complex questions, searching for code, and executing multi-step tasks. When you are searching for a keyword or file and are not confident that you will find the right match in the first few tries, use this agent to perform the search for you.",
    )
    .source(ContentSource::in_memory(
        r#"You are a general-purpose agent capable of handling complex, multi-step tasks.

You have full access to all tools and can:
- Read and modify files
- Execute shell commands
- Search and explore codebases
- Implement features and fix bugs
- Create and manage tasks

Work autonomously and methodically:
1. Understand the task requirements
2. Plan your approach
3. Execute step by step
4. Verify results
5. Return comprehensive results when complete"#,
    ))
    .source_type(SourceType::Builtin)
    .model_type(ModelType::Primary)
}

// =============================================================================
// Layer 2a — local-fs (filesystem exploration without shell execution)
// =============================================================================

/// Explore agent — fast information discovery over a filesystem tree.
///
/// Uses `Read` / `Grep` / `Glob` / `TodoWrite`. **Does not use `Bash`** —
/// file exploration is a local-fs concern, not a coding concern. A
/// research agent rummaging through markdown notes, a data analyst
/// inspecting CSV fixtures, and a coding agent grepping a source tree all
/// share the same essential need.
#[cfg(feature = "local-fs")]
pub fn explore_subagent() -> SubagentIndex {
    SubagentIndex::new(
        "explore",
        "Fast agent specialized for discovering information in a filesystem tree. Use this when you need to quickly find files by patterns, search content for keywords, or answer questions about a local document collection or codebase. When calling this agent, specify the desired thoroughness level: \"quick\" for basic searches, \"medium\" for moderate exploration, or \"very thorough\" for comprehensive analysis across multiple locations and naming conventions.",
    )
    .source(ContentSource::in_memory(
        r#"You are an Explore agent specialized for investigating a filesystem tree.

Your task is to quickly find relevant information through:
- Pattern matching with Glob (e.g., "notes/**/*.md", "src/components/**/*.tsx")
- Content search with Grep (e.g., "API endpoints", "function\\s+\\w+")
- File reading with Read

Thoroughness levels:
- "quick": Basic searches, first matches only
- "medium": Moderate exploration, check multiple locations
- "very thorough": Comprehensive analysis across multiple locations and naming conventions

You have Read, Grep, Glob, and TodoWrite available. You cannot run shell
commands — delegate those to a shell-capable agent if needed.

Be thorough but efficient. Return a concise summary of your findings."#,
    ))
    .source_type(SourceType::Builtin)
    .tools(["Read", "Grep", "Glob", "TodoWrite"])
    .model_type(ModelType::Small)
}

/// Plan agent — designs implementation or investigation strategies.
///
/// Read-only over the filesystem: `Read` / `Grep` / `Glob` / `TodoWrite`.
/// Useful for architectural planning in coding agents, but also for
/// research or analysis planning where the primary input is a local
/// document collection.
#[cfg(feature = "local-fs")]
pub fn plan_subagent() -> SubagentIndex {
    SubagentIndex::new(
        "plan",
        "Planning agent for designing implementation or investigation strategies. Use this when you need to plan the strategy for a task. Returns step-by-step plans, identifies critical files, and considers trade-offs.",
    )
    .source(ContentSource::in_memory(
        r#"You are a Plan agent for designing strategies.

Your task is to:
1. Understand the requirements thoroughly
2. Explore the filesystem to understand existing context and patterns
3. Identify critical files that will need reading or modification
4. Design a step-by-step plan
5. Consider trade-offs and potential issues

Present your plan clearly with:
- Numbered steps
- Files to be consulted or changed
- Potential risks or considerations
- Recommended approach with rationale

You have Read, Grep, Glob, and TodoWrite available. You cannot run shell
commands — if the plan needs one, call it out explicitly so the parent
agent can hand that step to a shell-capable agent."#,
    ))
    .source_type(SourceType::Builtin)
    .tools(["Read", "Grep", "Glob", "TodoWrite"])
    .model_type(ModelType::Primary)
}

// =============================================================================
// Layer 2b — coding-tools (shell execution)
// =============================================================================

/// Bash agent — command execution specialist.
///
/// Exclusively for running shell commands (git, build, test, system
/// operations). Only available under `coding-tools` because shell
/// execution is a coding-specific security posture.
#[cfg(feature = "coding-tools")]
pub fn bash_subagent() -> SubagentIndex {
    SubagentIndex::new(
        "bash",
        "Command execution specialist for running bash commands. Use this for git operations, command execution, and other terminal tasks.",
    )
    .source(ContentSource::in_memory(
        r#"You are a Bash agent specialized for command execution.

Your task is to execute shell commands efficiently and safely:
- Run git operations (status, diff, log, commit, push, etc.)
- Execute build and test commands
- Perform system operations

Always verify command safety before execution. Return clear, concise results."#,
    ))
    .source_type(SourceType::Builtin)
    .tools(["Bash", "KillShell"])
    .model_type(ModelType::Small)
}

// =============================================================================
// Composite helpers
// =============================================================================

/// Return all built-in subagents for the current build configuration.
///
/// - Pure core: only `general_purpose`.
/// - `local-fs`: adds `explore` and `plan`.
/// - `coding-tools`: adds `bash` on top of the above.
pub fn builtin_subagents() -> Vec<SubagentIndex> {
    #[allow(unused_mut)]
    let mut agents = vec![general_purpose_subagent()];
    #[cfg(feature = "local-fs")]
    {
        agents.push(explore_subagent());
        agents.push(plan_subagent());
    }
    #[cfg(feature = "coding-tools")]
    agents.push(bash_subagent());
    agents
}

/// Look up a built-in subagent by name.
///
/// Returns `None` if the name does not correspond to a built-in that is
/// available in the current build configuration.
pub fn find_builtin(name: &str) -> Option<SubagentIndex> {
    match name {
        "general" => Some(general_purpose_subagent()),
        #[cfg(feature = "local-fs")]
        "explore" => Some(explore_subagent()),
        #[cfg(feature = "local-fs")]
        "plan" => Some(plan_subagent()),
        #[cfg(feature = "coding-tools")]
        "bash" => Some(bash_subagent()),
        _ => None,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ToolRestricted;

    #[test]
    fn test_general_purpose_always_present() {
        assert!(find_builtin("general").is_some());
        assert!(find_builtin("nonexistent").is_none());
        let builtins = builtin_subagents();
        assert!(builtins.iter().any(|s| s.name == "general"));
    }

    #[test]
    fn test_general_purpose_no_restrictions() {
        let gp = general_purpose_subagent();
        assert!(!gp.has_tool_restrictions());
        assert!(gp.is_tool_allowed("Anything"));
    }

    // ---- Layer 2a (local-fs) tests -----------------------------------------

    #[cfg(feature = "local-fs")]
    #[test]
    fn test_explore_uses_filesystem_tools_only() {
        let explore = explore_subagent();
        assert!(explore.has_tool_restrictions());
        assert!(explore.is_tool_allowed("Read"));
        assert!(explore.is_tool_allowed("Grep"));
        assert!(explore.is_tool_allowed("Glob"));
        assert!(explore.is_tool_allowed("TodoWrite"));
        // Crucially: no shell execution. Bash belongs to Layer 2b.
        assert!(!explore.is_tool_allowed("Bash"));
        assert!(!explore.is_tool_allowed("KillShell"));
        // Read-only — no file mutation either.
        assert!(!explore.is_tool_allowed("Write"));
        assert!(!explore.is_tool_allowed("Edit"));
    }

    #[cfg(feature = "local-fs")]
    #[test]
    fn test_plan_uses_filesystem_tools_only() {
        let plan = plan_subagent();
        assert!(plan.has_tool_restrictions());
        assert!(plan.is_tool_allowed("Read"));
        assert!(plan.is_tool_allowed("Grep"));
        assert!(plan.is_tool_allowed("Glob"));
        assert!(plan.is_tool_allowed("TodoWrite"));
        assert!(!plan.is_tool_allowed("Bash"));
        assert!(!plan.is_tool_allowed("KillShell"));
        assert!(!plan.is_tool_allowed("Write"));
        assert!(!plan.is_tool_allowed("Edit"));
    }

    #[cfg(feature = "local-fs")]
    #[test]
    fn test_local_fs_builtins_are_registered() {
        let builtins = builtin_subagents();
        let names: Vec<&str> = builtins.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"explore"));
        assert!(names.contains(&"plan"));
        assert!(find_builtin("explore").is_some());
        assert!(find_builtin("plan").is_some());
    }

    // ---- Layer 2b (coding-tools) tests -------------------------------------

    #[cfg(feature = "coding-tools")]
    #[test]
    fn test_bash_only_allows_shell_tools() {
        let bash = bash_subagent();
        assert!(bash.has_tool_restrictions());
        assert!(bash.is_tool_allowed("Bash"));
        assert!(bash.is_tool_allowed("KillShell"));
        assert!(!bash.is_tool_allowed("Read"));
        assert!(!bash.is_tool_allowed("Write"));
    }

    #[cfg(feature = "coding-tools")]
    #[test]
    fn test_bash_registered_in_coding_builds() {
        assert!(find_builtin("bash").is_some());
        let builtins = builtin_subagents();
        assert!(builtins.iter().any(|s| s.name == "bash"));
    }

    // ---- Pure-core degradation tests ---------------------------------------

    #[cfg(not(feature = "local-fs"))]
    #[test]
    fn test_pure_core_omits_local_fs_builtins() {
        assert!(find_builtin("explore").is_none());
        assert!(find_builtin("plan").is_none());
        let builtins = builtin_subagents();
        assert_eq!(builtins.len(), 1);
        assert_eq!(builtins[0].name, "general");
    }

    #[cfg(not(feature = "coding-tools"))]
    #[test]
    fn test_non_coding_builds_omit_bash() {
        assert!(find_builtin("bash").is_none());
        let builtins = builtin_subagents();
        assert!(!builtins.iter().any(|s| s.name == "bash"));
    }
}
