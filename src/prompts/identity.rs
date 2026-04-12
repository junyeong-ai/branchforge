//! CLI Identity — required when using Claude CLI OAuth authentication.
//!
//! Consumed by `ProviderClient` which auto-injects the preamble as the first
//! system block for OAuth credentials, before codec encoding.

/// The CLI identity statement that MUST be included when using Claude CLI OAuth.
///
/// The Anthropic API rejects OAuth Bearer requests whose system prompt does
/// not begin with this identity statement.
pub const CLI_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
