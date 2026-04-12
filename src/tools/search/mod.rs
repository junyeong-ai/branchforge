//! Tool search functionality for progressive disclosure.

mod engine;
mod index;
mod manager;

pub use engine::{SearchEngine, SearchHit, SearchMode};
pub use index::{ToolIndex, ToolIndexEntry};
pub use manager::{PreparedTools, ToolSearchConfig, ToolSearchManager};
