//! Conditional prompt guidelines that activate based on available tools.
//!
//! Guidelines live in the system prompt (not tool descriptions), keeping tool
//! descriptions slim (~1-3 sentences) while behavioral guidance adapts to the
//! active tool surface.

/// A prompt guideline that is conditionally included based on active tools.
#[derive(Debug, Clone)]
pub struct PromptGuideline {
    /// Unique identifier for this guideline.
    pub id: &'static str,
    /// The guideline content to include in the system prompt.
    pub content: &'static str,
    /// All of these tools must be active for this guideline to be included.
    /// Empty means always included.
    pub when_tools: &'static [&'static str],
}

impl PromptGuideline {
    /// Check whether this guideline should be active given the available tools.
    pub fn is_active(&self, active_tools: &[&str]) -> bool {
        self.when_tools.is_empty()
            || self
                .when_tools
                .iter()
                .all(|required| active_tools.contains(required))
    }
}

/// Collect active guidelines into a single prompt section.
pub fn build_guidelines_section(active_tools: &[&str]) -> String {
    let active: Vec<&str> = GUIDELINES
        .iter()
        .filter(|g| g.is_active(active_tools))
        .map(|g| g.content)
        .collect();

    if active.is_empty() {
        return String::new();
    }

    let mut section = String::from("# Tool usage guidelines\n");
    for content in active {
        section.push('\n');
        section.push_str(content);
        section.push('\n');
    }
    section
}

// ---------------------------------------------------------------------------
// Built-in guidelines
// ---------------------------------------------------------------------------

pub static GUIDELINES: &[PromptGuideline] = &[
    // -- TodoWrite behavioral guide (only when TodoWrite is active) ----------
    PromptGuideline {
        id: "todo-usage",
        content: r#"## Task tracking (TodoWrite)

Use TodoWrite proactively for:
- Complex multi-step tasks (3+ distinct steps)
- When the user provides multiple tasks (numbered or comma-separated)
- After receiving new instructions — capture requirements as todos immediately
- Mark tasks in_progress BEFORE starting, completed IMMEDIATELY after finishing

Skip TodoWrite for:
- Single straightforward tasks or trivial fixes
- Purely conversational or informational requests
- Tasks completable in fewer than 3 steps

Keep exactly ONE task in_progress at a time. Provide both `content` (imperative: "Fix bug") and `activeForm` (continuous: "Fixing bug") for each todo."#,
        when_tools: &["TodoWrite"],
    },
    // -- Plan mode behavioral guide (only when Plan is active) ---------------
    PromptGuideline {
        id: "plan-usage",
        content: r#"## Planning workflow (Plan)

Use Plan for non-trivial implementation tasks:
- New features, architectural decisions, multi-file changes
- Tasks with multiple valid approaches or unclear requirements

Skip Plan for simple tasks (single-line fixes, clear single-function additions).

Workflow: start → explore with Read/Glob/Grep → update with findings → complete to proceed.
During plan mode ONLY these tools work: Plan, Read, Glob, Grep, TodoWrite, GraphHistory. All other tools (Write, Edit, Bash, Task, etc.) will fail until you complete or cancel the plan."#,
        when_tools: &["Plan"],
    },
    // -- Dedicated tools over shell (when both Bash and search tools exist) --
    PromptGuideline {
        id: "prefer-dedicated-tools",
        content: r#"## Prefer dedicated tools over shell

When Glob, Grep, Read, Write, and Edit are available, prefer them over Bash for file operations:
- Use Read instead of cat/head/tail
- Use Edit instead of sed/awk
- Use Write instead of echo/cat heredoc
- Use Glob instead of find/ls
- Use Grep instead of grep/rg
Reserve Bash for system commands and terminal operations that require shell execution."#,
        when_tools: &["Bash", "Read"],
    },
    // -- Subagent delegation (when Task tool is active) ----------------------
    PromptGuideline {
        id: "subagent-delegation",
        content: r#"## Subagent delegation (Task)

Use Task to delegate independent work to specialized subagents.
- Set run_in_background=true for tasks that don't block your current work
- Each subagent gets its own session with restricted tools and permissions
- Use TaskOutput to retrieve background task results"#,
        when_tools: &["Task"],
    },
    // -- Bash command patterns (when Bash is active) -------------------------
    PromptGuideline {
        id: "bash-patterns",
        content: r#"## Bash command patterns

- Quote file paths with spaces: `cd "/path/with spaces"`
- Chain dependent commands with `&&`: `git add . && git commit -m "msg"`
- Use parallel tool calls for independent commands (separate Bash calls)
- Prefer absolute paths over `cd` to maintain working directory
- Verify parent directories with `ls` before creating files/directories"#,
        when_tools: &["Bash"],
    },
    // -- File editing workflow (when Edit + Read are active) ------------------
    PromptGuideline {
        id: "edit-workflow",
        content: r#"## File editing workflow

- Always Read a file before using Edit or Write on it
- Prefer Edit over Write for modifying existing files
- Preserve exact indentation when matching old_string in Edit
- If old_string is not unique, provide more surrounding context or use replace_all"#,
        when_tools: &["Edit", "Read"],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidelines_activate_based_on_tools() {
        let all_tools = vec!["Read", "Write", "Edit", "Bash", "TodoWrite", "Plan", "Task"];
        let section = build_guidelines_section(&all_tools);
        assert!(section.contains("Task tracking"));
        assert!(section.contains("Planning workflow"));
        assert!(section.contains("Prefer dedicated tools"));
        assert!(section.contains("Subagent delegation"));
        assert!(section.contains("Bash command patterns"));
        assert!(section.contains("File editing workflow"));
    }

    #[test]
    fn guidelines_skip_when_tools_absent() {
        let minimal = vec!["Read", "Bash"];
        let section = build_guidelines_section(&minimal);
        assert!(!section.contains("Task tracking"));
        assert!(!section.contains("Planning workflow"));
        assert!(section.contains("Prefer dedicated tools"));
        assert!(!section.contains("Subagent delegation"));
        assert!(section.contains("Bash command patterns"));
        assert!(!section.contains("File editing workflow")); // needs Edit + Read
    }

    #[test]
    fn empty_tools_produces_no_section() {
        let section = build_guidelines_section(&[]);
        assert!(section.is_empty());
    }

    #[test]
    fn guideline_is_active_checks_all_required() {
        let g = PromptGuideline {
            id: "test",
            content: "test",
            when_tools: &["A", "B"],
        };
        assert!(!g.is_active(&["A"]));
        assert!(g.is_active(&["A", "B", "C"]));
    }
}
