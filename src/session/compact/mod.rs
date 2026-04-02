//! Context compaction subsystem.
//!
//! Provides pluggable strategies for reducing conversation context when
//! approaching token limits. The [`CompactionChain`] tries strategies in
//! order (cheapest first), protected by a circuit breaker.
//!
//! # Strategies
//!
//! - **[`FullCompaction`]**: LLM-based summarization. Permanent — appends a
//!   Summary node to the graph, moving the projection boundary forward.
//! - **[`MicroCompaction`]**: Truncates large tool results in the projection.
//!   Temporary — lost on session reload; graph remains unchanged.
//! - **[`TimeBasedCompaction`]**: Like MicroCompaction but triggered by idle
//!   time between interactions.
//!
//! # Example
//!
//! ```rust,no_run
//! use branchforge::session::compact::{
//!     CompactionChain, MicroCompaction, FullCompaction,
//! };
//!
//! let chain = CompactionChain::builder()
//!     .strategy(MicroCompaction::default())
//!     .strategy(FullCompaction::default())
//!     .build();
//! ```

pub mod chain;
pub mod full;
pub mod micro;
pub mod recovery;
mod service;
pub mod strategy;
pub mod time_based;

// Core strategy types
pub use strategy::{
    CompactionContext, CompactionPlan, CompactionStrategy, ContentOverrideEntry,
};

// Chain
pub use chain::{CompactionChain, CompactionChainBuilder};

// Strategy implementations
pub use full::FullCompaction;
pub use micro::MicroCompaction;
pub use time_based::TimeBasedCompaction;

pub use service::{
    CompactConfig, CompactService, DEFAULT_COMPACT_THRESHOLD, PreparedCompact,
};
