//! Plugin system with namespace-based resource management.
//!
//! Plugins are directories with a `.claude-plugin/plugin.json` manifest,
//! containing any combination of:
//! - `skills/` — Skill definitions (loaded via `SkillIndexLoader`)
//! - `commands/` — Legacy skill markdown files (also loaded as skills)
//! - `agents/` — Subagent definitions (loaded via `SubagentIndexLoader`)
//! - `hooks/hooks.json` — Hook configurations
//! - `.mcp.json` — MCP server configurations
//!
//! All resources are namespaced as `plugin-name:resource-name` to avoid collisions.
//!
//! # Directory Structure
//!
//! ```text
//! ~/.claude/plugins/
//! └── my-plugin/
//!     ├── .claude-plugin/
//!     │   └── plugin.json
//!     ├── skills/
//!     │   └── commit/
//!     │       └── SKILL.md
//!     ├── commands/
//!     │   └── hello.md
//!     ├── agents/
//!     │   └── reviewer.md
//!     ├── hooks/
//!     │   └── hooks.json
//!     └── .mcp.json
//! ```

mod discovery;
mod error;
mod loader;
mod manager;
mod manifest;
pub mod namespace;

pub use discovery::PluginDiscovery;
pub use error::PluginError;
pub use loader::PluginHookEntry;
pub use manager::PluginLoader;
pub use manifest::{PluginAuthor, PluginDescriptor, PluginManifest};
