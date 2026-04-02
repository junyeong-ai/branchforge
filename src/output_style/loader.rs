use std::path::Path;

use serde::{Deserialize, Serialize};

use super::OutputStyle;
use crate::common::{DocumentLoader, SourceType, is_markdown, parse_frontmatter};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputStyleFrontmatter {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Optional domain-specific instructions to inject into the system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_instructions: Option<String>,
    #[serde(default)]
    pub source_type: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OutputStyleLoader;

impl OutputStyleLoader {
    pub fn new() -> Self {
        Self
    }

    fn build_style(
        &self,
        fm: OutputStyleFrontmatter,
        body: String,
        _path: Option<&Path>,
    ) -> OutputStyle {
        let source_type = SourceType::from_str_opt(fm.source_type.as_deref());

        let mut style = OutputStyle::new(fm.name, fm.description, body).source_type(source_type);

        if let Some(instructions) = fm.domain_instructions {
            style = style.domain_instructions(instructions);
        }

        style
    }
}

impl DocumentLoader<OutputStyle> for OutputStyleLoader {
    fn parse_content(&self, content: &str, path: Option<&Path>) -> crate::Result<OutputStyle> {
        let doc = parse_frontmatter::<OutputStyleFrontmatter>(content)?;
        Ok(self.build_style(doc.frontmatter, doc.body, path))
    }

    fn doc_type_name(&self) -> &'static str {
        "output style"
    }

    fn file_filter(&self) -> fn(&Path) -> bool {
        is_markdown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::SourceType;

    #[test]
    fn test_parse_output_style_with_domain_instructions() {
        let content = r#"---
name: test-style
description: A test output style
domain_instructions: "Custom domain guidelines here"
---

# Custom Instructions

This is the custom prompt content.
"#;

        let loader = OutputStyleLoader::new();
        let style = loader.parse_content(content, None).unwrap();

        assert_eq!(style.name, "test-style");
        assert_eq!(style.description, "A test output style");
        assert!(style.has_domain_instructions());
        assert!(style.prompt.contains("Custom Instructions"));
    }

    #[test]
    fn test_parse_output_style_without_domain_instructions() {
        let content = r#"---
name: concise
description: Be concise
---

Be brief and to the point.
"#;

        let loader = OutputStyleLoader::new();
        let style = loader.parse_content(content, None).unwrap();

        assert_eq!(style.name, "concise");
        assert!(!style.has_domain_instructions());
    }

    #[test]
    fn test_parse_output_style_without_frontmatter() {
        let content = "Just some content without frontmatter";
        let loader = OutputStyleLoader::new();
        let result = loader.parse_content(content, None);

        assert!(result.is_err());
    }

    #[test]
    fn test_parse_output_style_with_source_type() {
        let content = r#"---
name: builtin-style
description: A builtin style
source_type: builtin
---

Content here.
"#;

        let loader = OutputStyleLoader::new();
        let style = loader.parse_content(content, None).unwrap();

        assert_eq!(style.source_type, SourceType::Builtin);
    }
}
