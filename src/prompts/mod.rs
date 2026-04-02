//! System prompt components for branchforge runtimes.
//!
//! Structure:
//! - `identity`: CLI identity (required for CLI OAuth authentication)
//! - `base`: Core behavioral guidelines (always included)
//! - `coding`: Software engineering instructions (feature = "coding-tools")
//! - `environment`: Runtime environment block (always included)

pub mod base;
#[cfg(feature = "coding-tools")]
pub mod coding;
pub mod environment;
pub mod guidelines;
pub mod identity;

pub use base::{BASE_SYSTEM_PROMPT, MCP_INSTRUCTIONS, TOOL_USAGE_POLICY};
#[cfg(feature = "coding-tools")]
pub use coding::{CODING_INSTRUCTIONS, coding_instructions};
pub use environment::environment_block;
pub use guidelines::{GUIDELINES, PromptGuideline, build_guidelines_section};
pub use identity::CLI_IDENTITY;
