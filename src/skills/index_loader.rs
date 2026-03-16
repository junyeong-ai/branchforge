//! Skill index loader.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::SkillIndex;
use crate::common::{ContentSource, SourceType, is_skill_file, parse_frontmatter};
use crate::hooks::HookRule;

/// Frontmatter schema for skill files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default, alias = "allowed-tools")]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub source_type: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, alias = "argument-hint")]
    pub argument_hint: Option<String>,
    #[serde(default, alias = "disable-model-invocation")]
    pub disable_model_invocation: bool,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub hooks: Option<HashMap<String, Vec<HookRule>>>,
}

/// Loader for creating SkillIndex entries from files.
///
/// This loader extracts only the frontmatter metadata, creating a lightweight
/// index entry. The full content is loaded on-demand via ContentSource.
#[derive(Debug, Clone, Copy, Default)]
pub struct SkillIndexLoader;

impl SkillIndexLoader {
    /// Create a new skill index loader.
    pub fn new() -> Self {
        Self
    }

    pub fn parse_index(&self, content: &str, path: &Path) -> crate::Result<SkillIndex> {
        let doc = parse_frontmatter::<SkillFrontmatter>(content)?;
        Ok(self.build_index(doc.frontmatter, path))
    }

    fn build_index(&self, fm: SkillFrontmatter, path: &Path) -> SkillIndex {
        let source_type_val = SourceType::from_str_opt(fm.source_type.as_deref());

        let mut index = SkillIndex::new(fm.name, fm.description)
            .source(ContentSource::file(path))
            .source_type(source_type_val);

        if !fm.triggers.is_empty() {
            index = index.triggers(fm.triggers);
        }

        if !fm.allowed_tools.is_empty() {
            index = index.allowed_tools(fm.allowed_tools);
        }

        if let Some(model_val) = fm.model {
            index = index.model(model_val);
        }

        if let Some(hint) = fm.argument_hint {
            index = index.argument_hint(hint);
        }

        index.disable_model_invocation = fm.disable_model_invocation;
        index.context = fm.context;
        index.agent = fm.agent;
        index.hooks = fm.hooks;

        index
    }

    /// Load a skill index from a file.
    pub async fn load_file(&self, path: &Path) -> crate::Result<SkillIndex> {
        crate::common::index_loader::load_file(path, |c, p| self.parse_index(c, p), "skill").await
    }

    /// Scan a directory for skill files and create indices.
    ///
    /// This recursively scans the directory for skill files (.skill.md or SKILL.md)
    /// and creates SkillIndex entries for each.
    pub async fn scan_directory(&self, dir: &Path) -> crate::Result<Vec<SkillIndex>> {
        use crate::common::index_loader::{self, DirAction};

        let loader = Self::new();
        index_loader::scan_directory(
            dir,
            |p| Box::pin(async move { loader.load_file(p).await }),
            is_skill_file,
            |p| {
                let skill_file = p.join("SKILL.md");
                if skill_file.exists() {
                    DirAction::LoadFile(skill_file)
                } else {
                    DirAction::Recurse
                }
            },
        )
        .await
    }

    /// Create an inline skill index with content already available.
    pub fn create_inline(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content: impl Into<String>,
    ) -> SkillIndex {
        SkillIndex::new(name, description).source(ContentSource::in_memory(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter() {
        let content = r#"---
name: test-skill
description: A test skill
triggers:
  - /test
  - test please
allowed-tools:
  - Read
  - Grep
model: claude-haiku-4-5-20251001
---

This is the skill content that should NOT be loaded into memory during indexing.
"#;

        let loader = SkillIndexLoader::new();
        let index = loader
            .parse_index(content, Path::new("/skills/test.skill.md"))
            .unwrap();

        assert_eq!(index.name, "test-skill");
        assert_eq!(index.description, "A test skill");
        assert_eq!(index.triggers, vec!["/test", "test please"]);
        assert_eq!(index.allowed_tools, vec!["Read", "Grep"]);
        assert_eq!(index.model, Some("claude-haiku-4-5-20251001".to_string()));

        // Verify source is file-based (for lazy loading)
        assert!(index.source.is_file());
    }

    #[test]
    fn test_parse_minimal_frontmatter() {
        let content = r#"---
name: minimal
description: Minimal skill
---

Content here.
"#;

        let loader = SkillIndexLoader::new();
        let index = loader
            .parse_index(content, Path::new("/skills/minimal.skill.md"))
            .unwrap();

        assert_eq!(index.name, "minimal");
        assert!(index.triggers.is_empty());
        assert!(index.allowed_tools.is_empty());
        assert!(index.model.is_none());
    }

    #[test]
    fn test_create_inline() {
        let loader = SkillIndexLoader::new();
        let index = loader.create_inline("inline", "Inline skill", "Full content");

        assert_eq!(index.name, "inline");
        assert!(index.source.is_in_memory());
    }

    #[tokio::test]
    async fn test_load_file() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::with_suffix(".skill.md").unwrap();
        writeln!(
            file,
            r#"---
name: file-skill
description: From file
---

Content."#
        )
        .unwrap();

        let loader = SkillIndexLoader::new();
        let index = loader.load_file(file.path()).await.unwrap();

        assert_eq!(index.name, "file-skill");
        assert!(index.source.is_file());
    }

    #[tokio::test]
    async fn test_scan_directory() {
        use tempfile::tempdir;
        use tokio::fs;

        let dir = tempdir().unwrap();

        // Create test skill files
        fs::write(
            dir.path().join("skill1.skill.md"),
            r#"---
name: skill1
description: First skill
---
Content 1"#,
        )
        .await
        .unwrap();

        fs::write(
            dir.path().join("skill2.skill.md"),
            r#"---
name: skill2
description: Second skill
---
Content 2"#,
        )
        .await
        .unwrap();

        let loader = SkillIndexLoader::new();
        let indices = loader.scan_directory(dir.path()).await.unwrap();

        assert_eq!(indices.len(), 2);
        let names: Vec<&str> = indices.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"skill1"));
        assert!(names.contains(&"skill2"));
    }

    #[tokio::test]
    async fn test_scan_directory_with_skill_folder() {
        use tempfile::tempdir;
        use tokio::fs;

        let dir = tempdir().unwrap();

        // Create skill folder pattern
        let skill_dir = dir.path().join("my-skill");
        fs::create_dir(&skill_dir).await.unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
name: folder-skill
description: From folder
---
Content"#,
        )
        .await
        .unwrap();

        let loader = SkillIndexLoader::new();
        let indices = loader.scan_directory(dir.path()).await.unwrap();

        assert_eq!(indices.len(), 1);
        assert_eq!(indices[0].name, "folder-skill");
    }

    #[tokio::test]
    async fn test_scan_nonexistent_directory() {
        let loader = SkillIndexLoader::new();
        let indices = loader
            .scan_directory(Path::new("/nonexistent/path"))
            .await
            .unwrap();
        assert!(indices.is_empty());
    }

    #[test]
    fn test_parse_disable_model_invocation() {
        let content = r#"---
name: system-only
description: System only skill
disable-model-invocation: true
---
Content"#;

        let loader = SkillIndexLoader::new();
        let index = loader
            .parse_index(content, Path::new("/skills/system.skill.md"))
            .unwrap();

        assert!(index.disable_model_invocation);
    }

    #[test]
    fn test_parse_context_and_agent() {
        let content = r#"---
name: explore-skill
description: Explore codebase
context: fork
agent: explore
---
Content"#;

        let loader = SkillIndexLoader::new();
        let index = loader
            .parse_index(content, Path::new("/skills/explore.skill.md"))
            .unwrap();

        assert_eq!(index.context, Some("fork".to_string()));
        assert_eq!(index.agent, Some("explore".to_string()));
    }

    #[test]
    fn test_defaults_for_new_fields() {
        let content = r#"---
name: basic
description: Basic skill
---
Content"#;

        let loader = SkillIndexLoader::new();
        let index = loader
            .parse_index(content, Path::new("/skills/basic.skill.md"))
            .unwrap();

        assert!(!index.disable_model_invocation);
        assert!(index.context.is_none());
        assert!(index.agent.is_none());
    }
}
