//! Frontmatter parsing utilities for Progressive Disclosure.
//!
//! Provides generic frontmatter parsing for all Index types.
//! Supports YAML frontmatter delimited by `---` markers.

use serde::de::DeserializeOwned;

/// Parsed document containing frontmatter metadata and body content.
pub struct ParsedDocument<F> {
    pub frontmatter: F,
    /// Body content after frontmatter. Consumed by output_style/loader.
    #[cfg_attr(not(feature = "file-resources"), allow(dead_code))]
    pub body: String,
}

/// Strip YAML frontmatter from content, returning body only.
///
/// This is a lightweight operation that returns a slice (no allocation).
/// Use when you only need the body content without parsing metadata.
pub fn strip_frontmatter(content: &str) -> &str {
    if let Some(after_first) = content.strip_prefix("---")
        && let Some(end_pos) = after_first.find("---")
    {
        return after_first[end_pos + 3..].trim_start();
    }
    content
}

/// Parse frontmatter from content, returning structured metadata and body.
///
/// Returns an error if frontmatter is missing or malformed.
pub fn parse_frontmatter<F: DeserializeOwned>(content: &str) -> crate::Result<ParsedDocument<F>> {
    if !content.starts_with("---") {
        return Err(crate::Error::Config(
            "Document must have YAML frontmatter (starting with ---)".to_string(),
        ));
    }

    let after_first = &content[3..];
    let end_pos = after_first.find("---").ok_or_else(|| {
        crate::Error::Config("Frontmatter not properly terminated with ---".to_string())
    })?;

    let frontmatter_str = after_first[..end_pos].trim();
    let body = after_first[end_pos + 3..].trim().to_string();

    let frontmatter: F = serde_yaml_bw::from_str(frontmatter_str)
        .map_err(|e| crate::Error::Config(format!("Failed to parse frontmatter: {}", e)))?;

    Ok(ParsedDocument { frontmatter, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct TestFrontmatter {
        name: String,
        #[serde(default)]
        description: String,
    }

    #[test]
    fn test_parse_valid() {
        let content = r#"---
name: test
description: A test
---

Body content here."#;

        let doc = parse_frontmatter::<TestFrontmatter>(content).unwrap();
        assert_eq!(doc.frontmatter.name, "test");
        assert_eq!(doc.frontmatter.description, "A test");
        assert_eq!(doc.body, "Body content here.");
    }

    #[test]
    fn test_parse_no_frontmatter() {
        let content = "Just content without frontmatter";
        let result = parse_frontmatter::<TestFrontmatter>(content);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_unterminated() {
        let content = "---\nname: test\nNo closing delimiter";
        let result = parse_frontmatter::<TestFrontmatter>(content);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_empty_body() {
        let content = r#"---
name: minimal
---
"#;

        let doc = parse_frontmatter::<TestFrontmatter>(content).unwrap();
        assert_eq!(doc.frontmatter.name, "minimal");
        assert!(doc.body.is_empty());
    }
}
