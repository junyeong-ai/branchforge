mod builtin;
mod generator;
#[cfg(feature = "cli-integration")]
mod loader;
mod provider;

pub use builtin::{builtin_styles, default_style, explanatory_style, find_builtin, learning_style};
pub use generator::SystemPromptGenerator;
#[cfg(feature = "cli-integration")]
pub use loader::{OutputStyleFrontmatter, OutputStyleLoader};
pub use provider::InMemoryOutputStyleProvider;
#[cfg(feature = "cli-integration")]
pub use provider::{ChainOutputStyleProvider, FileOutputStyleProvider, file_output_style_provider};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[cfg(feature = "cli-integration")]
use crate::common::IndexRegistry;
#[cfg(feature = "cli-integration")]
use crate::common::Provider;
use crate::common::{ContentSource, Index, Named, SourceType};

/// Definition of an output style.
///
/// Output styles customize Claude's behavior by modifying the system prompt.
/// The `keep_coding_instructions` flag determines whether standard coding
/// instructions are retained (true) or replaced by the custom prompt (false).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputStyle {
    pub name: String,
    pub description: String,
    /// The prompt content for this style.
    pub prompt: String,
    /// Content source for lazy loading (optional, defaults to InMemory from prompt).
    #[serde(default)]
    pub source: ContentSource,
    #[serde(default)]
    pub source_type: SourceType,
    #[serde(default, rename = "keep-coding-instructions")]
    pub keep_coding_instructions: bool,
}

impl OutputStyle {
    /// Create a new output style with the given name, description, and prompt.
    ///
    /// By default, `keep_coding_instructions` is `true` to match the behavior
    /// of the default style and `CompactStrategy::default()`.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        let prompt_str = prompt.into();
        Self {
            name: name.into(),
            description: description.into(),
            source: ContentSource::in_memory(&prompt_str),
            prompt: prompt_str,
            source_type: SourceType::default(),
            keep_coding_instructions: true,
        }
    }

    pub fn source_type(mut self, source_type: SourceType) -> Self {
        self.source_type = source_type;
        self
    }

    pub fn keep_coding_instructions(mut self, keep: bool) -> Self {
        self.keep_coding_instructions = keep;
        self
    }

    pub fn is_default(&self) -> bool {
        self.name == "default" && self.prompt.is_empty()
    }
}

impl Named for OutputStyle {
    fn name(&self) -> &str {
        &self.name
    }
}

#[async_trait]
impl Index for OutputStyle {
    fn source(&self) -> &ContentSource {
        &self.source
    }

    fn source_type(&self) -> SourceType {
        self.source_type
    }

    fn to_summary_line(&self) -> String {
        format!("- {}: {}", self.name, self.description)
    }

    fn description(&self) -> &str {
        &self.description
    }
}

/// Registry for output styles.
#[cfg(feature = "cli-integration")]
pub type OutputStyleRegistry = IndexRegistry<OutputStyle>;

#[cfg(feature = "cli-integration")]
impl OutputStyleRegistry {
    pub fn builtins() -> Self {
        let mut registry = Self::new();
        registry.register_all(builtin_styles());
        registry
    }

    pub async fn load_from_directories(
        &mut self,
        working_dir: Option<&std::path::Path>,
    ) -> crate::Result<()> {
        let builtins = InMemoryOutputStyleProvider::new()
            .items(builtin_styles())
            .priority(0)
            .source_type(SourceType::Builtin);

        let mut chain = ChainOutputStyleProvider::new().provider(builtins);

        if let Some(dir) = working_dir {
            let project = file_output_style_provider()
                .project_path(dir)
                .priority(20)
                .source_type(SourceType::Project);
            chain = chain.provider(project);
        }

        let user = file_output_style_provider()
            .user_path()
            .priority(10)
            .source_type(SourceType::User);
        let chain = chain.provider(user);

        let loaded = chain.load_all().await?;
        self.register_all(loaded);
        Ok(())
    }
}

impl Default for OutputStyle {
    fn default() -> Self {
        default_style()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_style_new() {
        let style = OutputStyle::new("test", "A test style", "Test prompt");

        assert_eq!(style.name, "test");
        assert_eq!(style.description, "A test style");
        assert_eq!(style.prompt, "Test prompt");
        assert_eq!(style.source_type, SourceType::User);
        // Default is now true for consistency with CompactStrategy
        assert!(style.keep_coding_instructions);
    }

    #[test]
    fn test_output_style_builder() {
        let style = OutputStyle::new("custom", "Custom style", "Custom prompt")
            .source_type(SourceType::Project)
            .keep_coding_instructions(true);

        assert_eq!(style.source_type, SourceType::Project);
        assert!(style.keep_coding_instructions);
    }

    #[test]
    fn test_default_style() {
        let style = default_style();

        assert!(style.is_default());
        assert_eq!(style.name, "default");
        assert!(style.keep_coding_instructions);
    }

    #[test]
    fn test_source_type_display() {
        assert_eq!(SourceType::Builtin.to_string(), "builtin");
        assert_eq!(SourceType::User.to_string(), "user");
        assert_eq!(SourceType::Project.to_string(), "project");
        assert_eq!(SourceType::Managed.to_string(), "managed");
    }
}
