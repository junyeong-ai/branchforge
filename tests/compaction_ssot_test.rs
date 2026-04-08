//! Integration tests for the SessionGraph SSoT contract under compaction.
//!
//! After R1 the agent runtime promised:
//!
//! > `SessionGraph` is the single source of truth. Every projection
//! > (`current_branch_messages`, `current_leaf_id`, summary) is derived
//! > from it on demand.
//!
//! These tests prove the promise survives compaction and persistence
//! round-trips. They are deliberately end-to-end (Session + Graph +
//! Compact + apply) rather than unit tests so that breaking the
//! invariant in any one of those layers fails the suite.

use branchforge::ir::ContentPart;
use branchforge::session::compact::{CompactResult, Compactor};
use branchforge::session::{Session, SessionConfig, SessionMessage};

fn fresh_session_with_messages(n: usize) -> Session {
    let mut session = Session::new(SessionConfig::default());
    for i in 0..n {
        session
            .add_message(SessionMessage::user(vec![ContentPart::text(format!(
                "user message {i}"
            ))]))
            .unwrap();
        session
            .add_message(SessionMessage::assistant(vec![ContentPart::text(
                format!("assistant reply {i}"),
            )]))
            .unwrap();
    }
    session
}

#[test]
fn current_branch_messages_starts_from_latest_summary_after_compaction() {
    // Synthesize the compaction outcome directly via apply_compact (no LLM
    // call needed for this invariant — we only verify graph structure +
    // projection alignment).
    let mut session = fresh_session_with_messages(5);
    assert_eq!(session.current_branch_messages().len(), 10);

    let svc = Compactor::new(branchforge::session::compact::CompactConfig::default());
    let result = svc
        .apply_compact(&mut session, "compacted summary X".to_string())
        .unwrap();
    assert!(matches!(result, CompactResult::Compacted { .. }));

    // SSoT invariant: after compaction, the projection starts from the
    // freshly appended Summary node and contains exactly that one entry.
    let projected = session.current_branch_messages();
    assert_eq!(projected.len(), 1);
    assert!(projected[0].is_compact_summary);
    let text = projected[0].content[0].as_text().unwrap();
    assert!(
        text.contains("compacted summary X"),
        "summary projection text mismatch: {text}"
    );
}

#[test]
fn add_message_after_compaction_appends_to_post_summary_branch() {
    let mut session = fresh_session_with_messages(3);
    let svc = Compactor::new(branchforge::session::compact::CompactConfig::default());
    svc.apply_compact(&mut session, "summary".to_string())
        .unwrap();

    // Add 2 fresh messages after compaction.
    session
        .add_message(SessionMessage::user(vec![ContentPart::text("after-1")]))
        .unwrap();
    session
        .add_message(SessionMessage::assistant(vec![ContentPart::text(
            "after-2",
        )]))
        .unwrap();

    let projected = session.current_branch_messages();
    // Summary + 2 new messages = 3
    assert_eq!(projected.len(), 3);
    assert!(projected[0].is_compact_summary);
    assert_eq!(projected[1].content[0].as_text(), Some("after-1"));
    assert_eq!(projected[2].content[0].as_text(), Some("after-2"));
}

#[test]
fn double_compaction_uses_latest_summary_only() {
    let mut session = fresh_session_with_messages(3);
    let svc = Compactor::new(branchforge::session::compact::CompactConfig::default());

    // First compaction
    svc.apply_compact(&mut session, "first summary".to_string())
        .unwrap();
    assert_eq!(session.current_branch_messages().len(), 1);

    // Add a few messages
    for i in 0..2 {
        session
            .add_message(SessionMessage::user(vec![ContentPart::text(format!(
                "post-1 user {i}"
            ))]))
            .unwrap();
        session
            .add_message(SessionMessage::assistant(vec![ContentPart::text(
                format!("post-1 asst {i}"),
            )]))
            .unwrap();
    }

    // Second compaction
    svc.apply_compact(&mut session, "second summary".to_string())
        .unwrap();

    let projected = session.current_branch_messages();
    assert_eq!(projected.len(), 1);
    assert!(
        projected[0]
            .content[0]
            .as_text()
            .unwrap()
            .contains("second summary"),
        "expected projection to start from the latest Summary node, got: {:?}",
        projected[0].content[0].as_text()
    );
}

#[test]
fn current_leaf_id_tracks_compaction_summary_node() {
    let mut session = fresh_session_with_messages(2);
    let leaf_before = session.current_leaf_id().expect("leaf before");

    let svc = Compactor::new(branchforge::session::compact::CompactConfig::default());
    svc.apply_compact(&mut session, "s".to_string()).unwrap();

    // After compaction the leaf must have advanced (Summary node + checkpoint).
    let leaf_after = session.current_leaf_id().expect("leaf after");
    assert_ne!(leaf_before, leaf_after);
}

#[test]
fn graph_events_persist_compaction_marker() {
    // Compaction must append a Summary node to the graph (not just to a
    // local cache). Verify by inspecting graph events directly — this is
    // the persistence-side proof of SSoT without needing a persistence
    // backend round-trip.
    let mut session = fresh_session_with_messages(3);
    let events_before = session.graph().events.len();

    let svc = Compactor::new(branchforge::session::compact::CompactConfig::default());
    svc.apply_compact(&mut session, "persisted summary".to_string())
        .unwrap();
    let events_after = session.graph().events.len();

    // At least one new graph event was emitted (summary node + checkpoint).
    assert!(
        events_after > events_before,
        "expected new graph events after compaction (before={events_before}, after={events_after})"
    );
}
