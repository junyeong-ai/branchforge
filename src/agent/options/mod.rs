//! Agent configuration options.

mod build;
mod builder;
#[cfg(feature = "file-resources")]
mod cli;

pub use builder::{AgentBuilder, DEFAULT_COMPACT_KEEP_MESSAGES};
