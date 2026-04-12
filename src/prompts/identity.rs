//! CLI Identity — required when using Claude CLI OAuth authentication.
//!
//! Consumed by `AgentBuilder::auth()` which sets `PromptConfig::auth_preamble`
//! for OAuth credentials. `RequestBuilder` then prepends this unconditionally,
//! outside the user-controllable Replace/Append system prompt logic.

/// The CLI identity statement that MUST be included when using Claude CLI OAuth.
///
/// The Anthropic API rejects OAuth Bearer requests whose system prompt does
/// not begin with this identity statement.
pub const CLI_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
