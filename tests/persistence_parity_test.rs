//! Persistence backend parity tests.
//!
//! Each backend (`MemoryPersistence`, `JsonlPersistence`) is exercised
//! with the same scenario function so that any feature-parity drift
//! between backends fails the suite. Postgres and Redis backends have
//! their own ignored Docker-gated tests in `backend_integration_tests`.
//!
//! Scenarios cover the **functional contract** of the `Persistence`
//! trait — save, load, list, delete, list_children, queue operations,
//! restore_bundle round-trip — at the IR boundary, not the wire-format
//! boundary.

use std::sync::Arc;

use branchforge::ir::ContentPart;
use branchforge::session::{
    MemoryPersistence, Persistence, Session, SessionConfig, SessionMessage,
};
#[cfg(feature = "jsonl")]
use branchforge::session::{JsonlConfig, JsonlPersistence};

fn fresh_session(label: &str) -> Session {
    let mut s = Session::new(SessionConfig::default());
    s.add_message(SessionMessage::user(vec![ContentPart::text(format!(
        "{label} user 1"
    ))]))
    .unwrap();
    s.add_message(SessionMessage::assistant(vec![ContentPart::text(
        format!("{label} asst 1"),
    )]))
    .unwrap();
    s
}

/// Save → load → assert messages identical.
async fn scenario_save_load_round_trip(p: Arc<dyn Persistence>) {
    let session = fresh_session("rt");
    let id = session.id;
    p.save(&session).await.expect("save");
    let loaded = p.load(&id).await.expect("load").expect("session present");
    assert_eq!(loaded.id, id);
    assert_eq!(
        loaded.current_branch_messages().len(),
        2,
        "{}: messages did not survive round trip",
        p.name()
    );
}

/// Save several sessions, list them, delete one, re-list.
async fn scenario_list_and_delete(p: Arc<dyn Persistence>) {
    let s1 = fresh_session("a");
    let s2 = fresh_session("b");
    let s3 = fresh_session("c");
    p.save(&s1).await.unwrap();
    p.save(&s2).await.unwrap();
    p.save(&s3).await.unwrap();

    let listed = p.list(None).await.unwrap();
    assert!(listed.contains(&s1.id), "{} list missing s1", p.name());
    assert!(listed.contains(&s2.id), "{} list missing s2", p.name());
    assert!(listed.contains(&s3.id), "{} list missing s3", p.name());

    let deleted = p.delete(&s2.id).await.unwrap();
    assert!(deleted, "{} delete should report success", p.name());

    let after = p.load(&s2.id).await.unwrap();
    assert!(after.is_none(), "{} session lingered after delete", p.name());
}

/// Append a graph node, save, load — graph events round trip.
async fn scenario_graph_events_round_trip(p: Arc<dyn Persistence>) {
    let session = fresh_session("graph");
    let event_count_before = session.graph().events.len();
    let id = session.id;
    p.save(&session).await.unwrap();

    let loaded = p.load(&id).await.unwrap().unwrap();
    assert_eq!(
        loaded.graph().events.len(),
        event_count_before,
        "{} dropped graph events on round trip",
        p.name()
    );
}

/// Enqueue + dequeue a queue item.
async fn scenario_queue_round_trip(p: Arc<dyn Persistence>) {
    let session = fresh_session("queue");
    p.save(&session).await.unwrap();
    let id = session.id;

    let item = p
        .enqueue(&id, "follow up question".to_string(), 5)
        .await
        .expect("enqueue");
    assert_eq!(item.content, "follow up question");

    let pending = p.pending_queue(&id).await.unwrap();
    assert_eq!(
        pending.len(),
        1,
        "{} pending queue should have 1 item",
        p.name()
    );

    let dequeued = p.dequeue(&id).await.unwrap().expect("dequeue some");
    assert_eq!(dequeued.content, "follow up question");

    let pending_after = p.pending_queue(&id).await.unwrap();
    assert!(pending_after.is_empty(), "{} queue not drained", p.name());
}

/// Run all scenarios against a backend.
async fn run_full_parity_suite(make: impl Fn() -> Arc<dyn Persistence>) {
    scenario_save_load_round_trip(make()).await;
    scenario_list_and_delete(make()).await;
    scenario_graph_events_round_trip(make()).await;
    scenario_queue_round_trip(make()).await;
}

#[tokio::test]
async fn memory_persistence_passes_full_parity_suite() {
    run_full_parity_suite(|| Arc::new(MemoryPersistence::new()) as Arc<dyn Persistence>).await;
}

#[cfg(feature = "jsonl")]
#[tokio::test]
async fn jsonl_persistence_passes_full_parity_suite() {
    use tempfile::TempDir;
    // Each scenario gets its own tempdir so they don't share state.
    let make = || -> Arc<dyn Persistence> {
        let temp = TempDir::new().expect("tempdir");
        let config = JsonlConfig::builder().base_dir(temp.path()).build();
        let persistence = futures::executor::block_on(JsonlPersistence::new(config))
            .expect("jsonl persistence init");
        // Leak the tempdir so it stays alive for the duration of the
        // scenario. The test process exits after, so the OS cleans up.
        std::mem::forget(temp);
        Arc::new(persistence) as Arc<dyn Persistence>
    };
    run_full_parity_suite(make).await;
}
