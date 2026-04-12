//! System prompt generator.
//!
//! Generates customized system prompts based on output style configuration.
//! Assembles system prompts from base prompts, domain instructions, tool policies, and environment context.

use std::path::PathBuf;

#[cfg(feature = "file-resources")]
use super::{ChainOutputStyleProvider, file_output_style_provider};
use super::{InMemoryOutputStyleProvider, OutputStyle, builtin_styles, default_style};
use crate::agent::DEFAULT_MODEL;
use crate::common::Provider;
use crate::common::SourceType;
#[cfg(feature = "coding-tools")]
use crate::prompts::environment::is_git_repository;
use crate::prompts::{
    base::{BASE_SYSTEM_PROMPT, TOOL_USAGE_POLICY},
    environment::{current_platform, environment_block, os_version},
};

/// System prompt generator with output style support.
///
/// Assembles the **base system prompt body** from content components.
/// Auth-layer concerns (e.g. `CLI_IDENTITY` for OAuth) are handled by
/// `ProviderClient` via `auth_preamble` — this generator is purely
/// about content generation.
///
/// # System Prompt Structure
///
/// 1. **Base System Prompt** (always included)
///    - Tone and style, professional objectivity, task management
///
/// 2. **Tool Usage Policy** (always included)
///    - Tool-specific guidelines
///
/// 3. **Domain Instructions** (if `domain_instructions` is set)
///    - Software engineering instructions
///    - Git commit/PR protocols
///
/// 4. **Custom Prompt** (if output style has custom content)
///    - Style-specific instructions
///
/// 5. **Environment Block** (always included)
///    - Working directory, platform, model info
#[derive(Debug, Clone)]
pub struct SystemPromptGenerator {
    style: OutputStyle,
    working_dir: Option<PathBuf>,
    model_name: String,
    model_id: String,
}

impl Default for SystemPromptGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemPromptGenerator {
    /// Create a new generator with default style.
    pub fn new() -> Self {
        Self {
            style: default_style(),
            working_dir: None,
            model_name: "Claude".to_string(),
            model_id: DEFAULT_MODEL.to_string(),
        }
    }

    /// Set the output style directly.
    pub fn output_style(mut self, style: OutputStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the working directory for environment block.
    pub fn working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    /// Set the model information.
    pub fn model(mut self, model_id: impl Into<String>) -> Self {
        let id = model_id.into();
        self.model_name = derive_model_name(&id);
        self.model_id = id;
        self
    }

    /// Set the model name explicitly.
    pub fn model_name(mut self, name: impl Into<String>) -> Self {
        self.model_name = name.into();
        self
    }

    /// Load and set an output style by name from built-in styles.
    pub async fn style_name_builtin(mut self, name: &str) -> crate::Result<Self> {
        let builtins = InMemoryOutputStyleProvider::new()
            .items(builtin_styles())
            .priority(0)
            .source_type(SourceType::Builtin);

        if let Some(style) = builtins.get(name).await? {
            self.style = style;
            Ok(self)
        } else {
            Err(crate::Error::Config(format!(
                "Output style '{}' not found",
                name
            )))
        }
    }

    /// Load and set an output style by name from file-based and built-in providers.
    ///
    /// Searches in priority order:
    /// 1. Project styles (.claude/output-styles/) - highest priority
    /// 2. User styles (~/.claude/output-styles/)
    /// 3. Built-in styles - lowest priority
    #[cfg(feature = "file-resources")]
    pub async fn style_name(mut self, name: &str) -> crate::Result<Self> {
        let builtins = InMemoryOutputStyleProvider::new()
            .items(builtin_styles())
            .priority(0)
            .source_type(SourceType::Builtin);

        let mut chain = ChainOutputStyleProvider::new().provider(builtins);

        if let Some(ref working_dir) = self.working_dir {
            let project = file_output_style_provider()
                .project_path(working_dir)
                .priority(20)
                .source_type(SourceType::Project);
            chain = chain.provider(project);
        }

        let user = file_output_style_provider()
            .user_path()
            .priority(10)
            .source_type(SourceType::User);
        chain = chain.provider(user);

        if let Some(style) = chain.get(name).await? {
            self.style = style;
            Ok(self)
        } else {
            Err(crate::Error::Config(format!(
                "Output style '{}' not found",
                name
            )))
        }
    }

    /// Generate the system prompt body.
    ///
    /// # Prompt Assembly Logic
    ///
    /// - **Base System Prompt**: Always included
    /// - **Tool Usage Policy**: Always included
    /// - **Domain Instructions**: Only if `domain_instructions` is set
    /// - **Custom Prompt**: Only if style has non-empty prompt
    /// - **Environment Block**: Always included
    ///
    /// Auth-layer preambles (e.g. `CLI_IDENTITY` for OAuth) are not handled
    /// here — see `ProviderClient::with_preamble()`.
    pub fn generate(&self) -> String {
        let mut parts = Vec::new();

        // 1. Base System Prompt (always)
        parts.push(BASE_SYSTEM_PROMPT.to_string());

        // 2. Tool Usage Policy (always)
        parts.push(TOOL_USAGE_POLICY.to_string());

        // 3. Domain Instructions (conditional — injected by application)
        if let Some(ref instructions) = self.style.domain_instructions {
            parts.push(instructions.clone());
        }

        // 4. Custom Prompt (if present)
        if !self.style.prompt.is_empty() {
            parts.push(self.style.prompt.clone());
        }

        // 5. Environment Block (always)
        //
        // Git repository detection is a Layer 2b concern — only surfaced
        // when the `coding-tools` feature is active. Layer 1 / `local-fs`
        // builds pass `None` and the corresponding prompt line is omitted.
        #[cfg(feature = "coding-tools")]
        let is_git = Some(is_git_repository(self.working_dir.as_deref()));
        #[cfg(not(feature = "coding-tools"))]
        let is_git: Option<bool> = None;
        let platform = current_platform();
        let os_ver = os_version();

        parts.push(environment_block(
            self.working_dir.as_deref(),
            is_git,
            platform,
            &os_ver,
            &self.model_name,
            &self.model_id,
        ));

        parts.join("\n\n")
    }

    /// Generate the system prompt with additional dynamic context.
    ///
    /// This is used when rules or other dynamic content needs to be appended.
    pub fn generate_with_context(&self, additional_context: &str) -> String {
        let mut prompt = self.generate();
        if !additional_context.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(additional_context);
        }
        prompt
    }

    /// Get the current output style.
    pub fn style(&self) -> &OutputStyle {
        &self.style
    }

    /// Check if domain instructions are included in the generated prompt.
    pub fn has_domain_instructions(&self) -> bool {
        self.style.has_domain_instructions()
    }
}

/// Derive a friendly model name from model ID.
fn derive_model_name(model_id: &str) -> String {
    if model_id.contains("opus") {
        "Claude Opus 4.6".to_string()
    } else if model_id.contains("sonnet") {
        "Claude Sonnet 4.5".to_string()
    } else if model_id.contains("haiku") {
        "Claude Haiku 4.5".to_string()
    } else {
        "Claude".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_style::SourceType;

    #[test]
    fn test_generator_default_starts_with_base_prompt() {
        let prompt = SystemPromptGenerator::new().generate();

        assert!(prompt.starts_with(BASE_SYSTEM_PROMPT));
        assert!(prompt.contains(TOOL_USAGE_POLICY));
        assert!(prompt.contains("<env>")); // environment block
    }

    #[test]
    fn test_generator_does_not_include_cli_identity() {
        // CLI_IDENTITY is an auth-layer concern handled by ProviderClient,
        // not by the generator.
        let prompt = SystemPromptGenerator::new().generate();
        assert!(!prompt.contains("Claude Code"));
    }

    #[test]
    fn test_generator_with_custom_style_with_domain() {
        let style = OutputStyle::new("test", "Test style", "Custom instructions here")
            .source_type(SourceType::User)
            .domain_instructions("Domain-specific guidelines");

        let prompt = SystemPromptGenerator::new().output_style(style).generate();

        assert!(prompt.starts_with(BASE_SYSTEM_PROMPT));
        assert!(prompt.contains("Domain-specific guidelines"));
        assert!(prompt.contains("Custom instructions here"));
        assert!(prompt.contains("<env>"));
    }

    #[test]
    fn test_generator_with_custom_style_no_domain() {
        let style = OutputStyle::new("concise", "Be concise", "Keep responses short.")
            .source_type(SourceType::User);

        let prompt = SystemPromptGenerator::new().output_style(style).generate();

        assert!(prompt.starts_with(BASE_SYSTEM_PROMPT));
        assert!(!prompt.contains("Domain-specific"));
        assert!(prompt.contains("Keep responses short."));
        assert!(prompt.contains("<env>"));
    }

    #[test]
    fn test_generator_working_dir() {
        let prompt = SystemPromptGenerator::new()
            .working_dir("/test/project")
            .generate();

        assert!(prompt.contains("/test/project"));
    }

    #[test]
    fn test_generator_model() {
        let prompt = SystemPromptGenerator::new()
            .model("claude-opus-4-6")
            .generate();

        assert!(prompt.contains("claude-opus-4-6"));
        assert!(prompt.contains("Claude Opus 4.6"));
    }

    #[test]
    fn test_derive_model_name() {
        assert_eq!(derive_model_name("claude-opus-4-6"), "Claude Opus 4.6");
        assert_eq!(
            derive_model_name("claude-sonnet-4-5-20250929"),
            "Claude Sonnet 4.5"
        );
        assert_eq!(
            derive_model_name("claude-haiku-4-5-20251001"),
            "Claude Haiku 4.5"
        );
        assert_eq!(derive_model_name("unknown-model"), "Claude");
    }

    #[test]
    fn test_generator_with_context() {
        let prompt = SystemPromptGenerator::new()
            .generate_with_context("# Dynamic Rules\nSome dynamic content");

        assert!(prompt.contains("# Dynamic Rules"));
        assert!(prompt.contains("Some dynamic content"));
    }

    #[test]
    fn test_has_domain_instructions() {
        let style_with = OutputStyle::new("with", "", "").domain_instructions("some guidelines");
        let gen_with = SystemPromptGenerator::new().output_style(style_with);
        assert!(gen_with.has_domain_instructions());

        let style_without = OutputStyle::new("without", "", "");
        let gen_without = SystemPromptGenerator::new().output_style(style_without);
        assert!(!gen_without.has_domain_instructions());
    }
}
