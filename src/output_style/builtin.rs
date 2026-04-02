//! Built-in output styles.
//!
//! These are the standard output styles that come bundled with the SDK.
//! When `coding-tools` feature is enabled, styles include coding domain
//! instructions by default.

use crate::common::SourceType;

use super::OutputStyle;

/// Conditionally attach coding instructions when the feature is enabled.
fn maybe_with_coding_instructions(style: OutputStyle) -> OutputStyle {
    #[cfg(feature = "coding-tools")]
    {
        style.domain_instructions(crate::prompts::coding::CODING_INSTRUCTIONS)
    }
    #[cfg(not(feature = "coding-tools"))]
    {
        style
    }
}

/// Default output style.
pub fn default_style() -> OutputStyle {
    maybe_with_coding_instructions(
        OutputStyle::new("default", "Standard mode", "")
            .source_type(SourceType::Builtin),
    )
}

/// Explanatory output style — adds educational insights.
pub fn explanatory_style() -> OutputStyle {
    maybe_with_coding_instructions(
        OutputStyle::new(
            "explanatory",
            "Educational mode that explains implementation choices",
            EXPLANATORY_PROMPT,
        )
        .source_type(SourceType::Builtin),
    )
}

/// Learning output style — collaborative learn-by-doing mode.
pub fn learning_style() -> OutputStyle {
    maybe_with_coding_instructions(
        OutputStyle::new(
            "learning",
            "Interactive learning mode with guided exercises",
            LEARNING_PROMPT,
        )
        .source_type(SourceType::Builtin),
    )
}

/// Returns all built-in styles.
pub fn builtin_styles() -> Vec<OutputStyle> {
    vec![default_style(), explanatory_style(), learning_style()]
}

/// Find a built-in style by name.
pub fn find_builtin(name: &str) -> Option<OutputStyle> {
    match name.to_lowercase().as_str() {
        "default" => Some(default_style()),
        "explanatory" => Some(explanatory_style()),
        "learning" => Some(learning_style()),
        _ => None,
    }
}

const EXPLANATORY_PROMPT: &str = r#"# Explanatory Mode

When working on tasks, provide educational insights that help the user understand:

## Between Tasks
After completing significant changes, add an **Insights** section that explains:
- Why you chose this particular approach over alternatives
- Key patterns or idioms you're using and why they're appropriate
- How this change fits into the broader architecture
- Any trade-offs you considered

## During Implementation
- Explain non-obvious patterns when you use them
- Point out important conventions you're following
- Highlight potential gotchas or edge cases
- Reference relevant documentation or best practices

Keep insights concise but informative."#;

const LEARNING_PROMPT: &str = r#"# Learning Mode

This is a collaborative learn-by-doing mode. Your goal is to guide the user through implementing solutions themselves, rather than doing everything for them.

## Approach

1. **Explain the Concept**: Start by explaining what needs to be done and why
2. **Show the Pattern**: Demonstrate with a small example if needed
3. **Guide Implementation**: Let the user implement the main solution
4. **Review and Improve**: Help refine their implementation

## Guidelines

- Break complex tasks into smaller, manageable steps
- Provide hints but not complete solutions
- Ask questions that lead to understanding
- Explain the "why" behind each decision

Remember: The goal is learning, not just task completion."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_style() {
        let style = default_style();
        assert_eq!(style.name, "default");
        assert!(style.is_default());
        assert_eq!(style.source_type, SourceType::Builtin);
    }

    #[test]
    fn test_explanatory_style() {
        let style = explanatory_style();
        assert_eq!(style.name, "explanatory");
        assert!(!style.is_default());
        assert!(style.prompt.contains("Insights"));
    }

    #[test]
    fn test_learning_style() {
        let style = learning_style();
        assert_eq!(style.name, "learning");
        assert!(!style.is_default());
        assert!(style.prompt.contains("learning"));
    }

    #[test]
    fn test_builtin_styles() {
        let styles = builtin_styles();
        assert_eq!(styles.len(), 3);
        assert!(styles.iter().all(|s| s.source_type == SourceType::Builtin));
    }

    #[test]
    fn test_find_builtin() {
        assert!(find_builtin("default").is_some());
        assert!(find_builtin("Default").is_some());
        assert!(find_builtin("explanatory").is_some());
        assert!(find_builtin("learning").is_some());
        assert!(find_builtin("nonexistent").is_none());
    }
}
