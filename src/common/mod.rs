mod content_source;
mod directory;
#[cfg(feature = "cli-integration")]
mod file_provider;
mod frontmatter;
mod index;
pub(crate) mod index_loader;
mod index_registry;
mod path_matched;
mod provider;
pub(crate) mod serde_defaults;
mod source_type;
mod tool_matcher;

use std::path::PathBuf;

pub use content_source::ContentSource;
pub(crate) use directory::is_skill_file;
#[cfg(feature = "cli-integration")]
pub(crate) use directory::{is_markdown, load_files};

pub(crate) fn home_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|d| d.home_dir().to_path_buf())
}
#[cfg(feature = "cli-integration")]
pub(crate) use file_provider::{DocumentLoader, FileProvider, OutputStyleLookupStrategy};
pub(crate) use frontmatter::{parse_frontmatter, strip_frontmatter};
pub use index::Index;
pub use index_registry::{IndexRegistry, LoadedEntry};
pub use path_matched::PathMatched;
#[cfg(feature = "cli-integration")]
pub(crate) use provider::ChainProvider;
pub(crate) use provider::InMemoryProvider;
pub use provider::Provider;
pub use source_type::SourceType;
pub use tool_matcher::{is_tool_allowed, matches_tool_pattern};

pub trait Named {
    fn name(&self) -> &str;
}

pub trait ToolRestricted {
    fn allowed_tools(&self) -> &[String];

    fn has_tool_restrictions(&self) -> bool {
        !self.allowed_tools().is_empty()
    }

    fn is_tool_allowed(&self, tool_name: &str) -> bool {
        tool_matcher::is_tool_allowed(self.allowed_tools(), tool_name)
    }
}
