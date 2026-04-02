//! Multi-agent orchestration.
//!
//! Provides pluggable coordination patterns for managing multiple agents
//! working on a shared task. The [`Coordination`] trait defines how agents
//! are organized, and [`AgentDirectory`] manages inter-agent communication.
//!
//! # Architecture
//!
//! ```text
//! orchestration (high-level — multi-agent coordination)
//!   └── uses subagents/ (mid-level — agent metadata & prompts)
//!        └── uses agent/ (low-level — single-agent execution)
//! ```
//!
//! # Built-in pattern
//!
//! - [`Coordinator`]: Research → Synthesis → Implementation → Verification workflow.
//!   Workers are isolated (no access to coordinator history) and receive
//!   self-contained prompts.
//!
//! # Example
//!
//! ```rust,no_run
//! use branchforge::{Agent, orchestration::Coordinator};
//!
//! # async fn example() -> Result<(), branchforge::Error> {
//! let agent = Agent::builder()
//!     .model("claude-sonnet-4-5")
//!     .coordination(Coordinator::default())
//!     .build()
//!     .await?;
//! # Ok(())
//! # }
//! ```

mod coordinator;
mod directory;
mod messaging;
mod send_message;
mod traits;
mod worker;

pub use coordinator::{Coordinator, CoordinatorBuilder};
pub use directory::{AgentDirectory, AgentHandle, AgentId, AgentStatus};
pub use messaging::{AgentMessage, MessageChannel};
pub use send_message::SendMessageTool;
pub use traits::{Coordination, CoordinationContext};
pub use worker::{WorkerConstraints, WorkerGroup, WorkerResult, WorkerSpec};
