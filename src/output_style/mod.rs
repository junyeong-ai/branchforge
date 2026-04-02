mod builtin;
mod generator;
#[cfg(feature = "file-resources")]
mod loader;
mod provider;

pub use builtin::{builtin_styles, default_style, explanatory_style, find_builtin, learning_style};
pub use generator::SystemPromptGenerator;
#[cfg(feature = "file-resources")]
pub use loader::{OutputStyleFrontmatter, OutputStyleLoader};
pub use provider::InMemoryOutputStyleProvider;
#[cfg(feature = "file-resources")]
pub use provider::{ChainOutputStyleProvider, FileOutputStyleProvider, file_output_style_provider};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[cfg(feature = "file-resources")]
use crate::common::IndexRegistry;
#[cfg(feature = "file-resources")]
use crate::common::Provider;
use crate::common::{ContentSource, Index, Named, SourceType};

/// Definition of an output style.
///
/// Output styles customize agent behavior by modifying the system prompt.
///
/// `domain_instructions` contains optional domain-specific instructions
/// (e.g., coding guidelines) that are injected into the system prompt.
/// When `None`, only the base prompt and custom style prompt are used.
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
    /// Optional domain-specific instructions injected into the system prompt.
    /// For coding agents, this contains software engineering guidelines.
    /// For other agents, set to domain-appropriate instructions or leave None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_instructions: Option<String>,
}

impl OutputStyle {
    /// Create a new output style with the given name, description, and prompt.
    ///
    /// By default, `domain_instructions` is `None`. Use `.domain_instructions()`
    /// to inject domain-specific guidelines.
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
            domain_instructions: None,
        }
    }

    pub fn source_type(mut self, source_type: SourceType) -> Self {
        self.source_type = source_type;
        self
    }

    /// Set domain-specific instructions to include in the system prompt.
    pub fn domain_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.domain_instructions = Some(instructions.into());
        self
    }

    pub fn has_domain_instructions(&self) -> bool {
        self.domain_instructions.is_some()
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
#[cfg(feature = "file-resources")]
pub type OutputStyleRegistry = IndexRegistry<OutputStyle>;

#[cfg(feature = "file-resources")]
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
        assert!(style.domain_instructions.is_none());
    }

    #[test]
    fn test_output_style_builder() {
        let style = OutputStyle::new("custom", "Custom style", "Custom prompt")
            .source_type(SourceType::Project)
            .domain_instructions("Custom domain guidelines");

        assert_eq!(style.source_type, SourceType::Project);
        assert!(style.has_domain_instructions());
    }

    #[test]
    fn test_default_style() {
        let style = default_style();

        assert!(style.is_default());
        assert_eq!(style.name, "default");
        // Default style has domain_instructions set by builtin_styles()
        // which injects CODING_INSTRUCTIONS when coding-tools feature is enabled
    }

    #[test]
    fn test_source_type_display() {
        assert_eq!(SourceType::Builtin.to_string(), "builtin");
        assert_eq!(SourceType::User.to_string(), "user");
        assert_eq!(SourceType::Project.to_string(), "project");
        assert_eq!(SourceType::Managed.to_string(), "managed");
    }
}
