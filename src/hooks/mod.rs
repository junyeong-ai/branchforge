//! Hook system for intercepting agent execution.

mod command;
mod manager;
pub mod rule;
mod traits;

pub use command::CommandHook;
pub use manager::HookRegistry;
pub use rule::{HookAction, HookRule};
pub use traits::{
    FnHook, FnHookBuilder, Hook, HookContext, HookEvent, HookEventData, HookInput, HookMetadata,
    HookOutput, HookSource,
};
