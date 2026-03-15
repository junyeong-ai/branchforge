//! Coding instructions for branchforge runtimes.

/// Software engineering guidance for coding tasks.
pub const CODING_INSTRUCTIONS: &str = r#"# Doing tasks

The user will primarily ask you to perform software engineering tasks such as fixing bugs, adding features, refactoring, or explaining code.

Use these principles:
- read code before proposing or applying changes
- keep changes focused on the requested outcome
- avoid speculative abstractions and backwards-compatibility shims
- prefer deletion over indirection when something is unused
- validate inputs and external boundaries, not impossible internal states
- fix security issues you introduce or uncover while working
- do not make workflow assumptions that are not required by the task"#;

/// Returns coding instructions for the selected model.
pub fn coding_instructions(_model_name: &str) -> String {
    CODING_INSTRUCTIONS.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coding_instructions() {
        let instructions = coding_instructions("branchforge");
        assert!(instructions.contains("# Doing tasks"));
        assert!(instructions.contains("read code before"));
        assert!(!instructions.contains("Co-Authored-By"));
        assert!(!instructions.contains("Creating pull requests"));
    }
}
