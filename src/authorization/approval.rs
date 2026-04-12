//! Shared constants for the human-in-the-loop approval flow.
//!
//! The channel-based approval API was deleted in Phase D Workstream
//! C-1 in favour of the unified [`super::human::HumanInteractionHandler`]
//! trait. The constants below remain because they are part of the
//! agent runtime's timeout contract and are consumed by both the
//! non-streaming and streaming execution paths.

/// Default timeout in seconds the runtime waits on a
/// [`super::human::HumanInteractionHandler::approve_tool`] response
/// before converting the call to a denied decision with a "timed
/// out" reason.
///
/// If the host needs a different window it should wrap the handler
/// in its own timeout logic — this constant is the runtime's
/// fail-closed ceiling.
pub const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 30;
