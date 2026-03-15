//! Authorization system for controlling tool execution.

mod modes;
mod rules;

pub use modes::AuthorizationMode;
pub use rules::{
    AuthorizationDecision, AuthorizationPolicy, AuthorizationPolicyBuilder, AuthorizationResult,
    AuthorizationRule, ToolLimits,
};

pub const READ_ONLY_TOOLS: &[&str] = &["Read", "Glob", "Grep", "WebSearch", "WebFetch"];
pub const FILE_TOOLS: &[&str] = &["Read", "Write", "Edit", "Glob", "Grep"];
pub const SHELL_TOOLS: &[&str] = &["Bash", "KillShell"];

pub fn is_read_only_tool(tool_name: &str) -> bool {
    READ_ONLY_TOOLS.contains(&tool_name)
}

pub fn is_file_tool(tool_name: &str) -> bool {
    FILE_TOOLS.contains(&tool_name)
}

pub fn is_shell_tool(tool_name: &str) -> bool {
    SHELL_TOOLS.contains(&tool_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_read_only_tool() {
        assert!(is_read_only_tool("Read"));
        assert!(is_read_only_tool("Glob"));
        assert!(is_read_only_tool("Grep"));
        assert!(!is_read_only_tool("Write"));
        assert!(!is_read_only_tool("Bash"));
    }

    #[test]
    fn test_is_file_tool() {
        assert!(is_file_tool("Read"));
        assert!(is_file_tool("Write"));
        assert!(is_file_tool("Edit"));
        assert!(!is_file_tool("Bash"));
        assert!(!is_file_tool("WebSearch"));
    }

    #[test]
    fn test_is_shell_tool() {
        assert!(is_shell_tool("Bash"));
        assert!(is_shell_tool("KillShell"));
        assert!(!is_shell_tool("Read"));
        assert!(!is_shell_tool("Write"));
    }
}
