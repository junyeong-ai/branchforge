//! Live verification matrix for core runtime guarantees.
//!
//! These tests are ignored by default because they require live Claude CLI OAuth
//! credentials. They intentionally verify cross-cutting guarantees with real
//! model execution instead of only deterministic unit tests.

#![cfg(feature = "cli-auth")]

use branchforge::session::{
    ArchivePolicy, CompactService, CompactStrategy, ExportPolicy, MemoryPersistence, Persistence,
    SessionAccessScope, SessionArchiveService, SessionId, SessionManager,
};
use branchforge::types::CompactResult;
use branchforge::{Agent, Auth, Client, ContentBlock, ToolSurface};
use tempfile::tempdir;
use tokio::fs;

fn scoped_manager(manager: &SessionManager) -> branchforge::ScopedSessionManager {
    manager.scoped(
        SessionAccessScope::default()
            .tenant("tenant-live")
            .principal("user-live"),
    )
}

fn parse_session_id(value: &str) -> SessionId {
    SessionId::parse(value).expect("live result should contain a valid session id")
}

fn collect_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(ContentBlock::as_text)
        .collect::<Vec<_>>()
        .join(" ")
}

mod live_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires CLI credentials"]
    async fn test_live_task_subagent_persists_child_session() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("facts.txt"),
            "The verification number is 731.\nReturn it exactly.",
        )
        .await
        .unwrap();

        let manager = SessionManager::in_memory();
        let scoped = scoped_manager(&manager);

        let agent = Agent::builder()
            .auth(Auth::ClaudeCli)
            .await
            .expect("Failed to load CLI credentials")
            .session_manager(manager.clone())
            .tenant_id("tenant-live")
            .principal_id("user-live")
            .tools(ToolSurface::only(["Task", "Read", "Grep", "Glob"]))
            .working_dir(dir.path())
            .max_iterations(6)
            .build()
            .await
            .expect("Failed to build live agent");

        let result = agent
            .execute(
                "Your first action must be a Task tool call. Use subagent_type Explore with a \
                 prompt that reads facts.txt and returns only the verification number. Do not use \
                 Read, Grep, or Glob directly in the parent agent. After the Task tool returns, \
                 answer with only the number.",
            )
            .await
            .expect("Live task execution failed");

        let parent_id = parse_session_id(result.session_id());
        let parent = scoped
            .get(&parent_id)
            .await
            .expect("Parent session should be persisted");
        let sessions = scoped
            .list()
            .await
            .expect("Scoped session listing should succeed");

        let mut session_debug = Vec::new();
        let mut child_session = None;
        for id in sessions {
            let session = scoped
                .get(&id)
                .await
                .expect("Scoped child load should succeed");
            session_debug.push(format!(
                "{} parent={:?} state={:?} type={:?}",
                session.id, session.parent_id, session.state, session.session_type
            ));
            if session.parent_id == Some(parent.id) {
                child_session = Some(session);
                break;
            }
        }

        let child = child_session.unwrap_or_else(|| {
            panic!(
                "Task execution should persist a child subagent session. result_text={:?}, sessions={:?}",
                result.text(),
                session_debug
            )
        });
        let child_text = child
            .current_branch_messages()
            .iter()
            .map(|message| collect_text(&message.content))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(child.tenant_id.as_deref(), Some("tenant-live"));
        assert_eq!(child.principal_id.as_deref(), Some("user-live"));
        assert!(
            result.tool_calls > 0,
            "The live agent should have used the Task tool"
        );
        assert!(
            !child.current_branch_messages().is_empty(),
            "Delegated child session should retain its own conversation history"
        );
        assert!(
            child_text.contains("facts.txt"),
            "Delegated child session should preserve the task prompt context. parent_text={:?} child_text={:?}",
            result.text(),
            child_text
        );
    }

    #[tokio::test]
    #[ignore = "Requires CLI credentials"]
    async fn test_live_scoped_session_persistence_roundtrip() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("identity.txt"),
            "Scoped identity test value: 615",
        )
        .await
        .unwrap();

        let manager = SessionManager::in_memory();
        let scoped = scoped_manager(&manager);

        let agent = Agent::builder()
            .auth(Auth::ClaudeCli)
            .await
            .expect("Failed to load CLI credentials")
            .session_manager(manager.clone())
            .tenant_id("tenant-live")
            .principal_id("user-live")
            .tools(ToolSurface::only(["Read"]))
            .working_dir(dir.path())
            .max_iterations(4)
            .build()
            .await
            .expect("Failed to build live agent");

        let result = agent
            .execute("Read identity.txt and return only the numeric value.")
            .await
            .expect("Live agent execution failed");

        let session_id = parse_session_id(result.session_id());
        let session = scoped
            .get(&session_id)
            .await
            .expect("Scoped session should be retrievable");
        let replay = scoped
            .replay_input(&session_id, None)
            .await
            .expect("Scoped replay should succeed");

        assert_eq!(session.tenant_id.as_deref(), Some("tenant-live"));
        assert_eq!(session.principal_id.as_deref(), Some("user-live"));
        assert!(
            !session.graph.nodes.is_empty(),
            "Session graph should be persisted"
        );
        assert!(
            !replay.messages.is_empty(),
            "Replay input should preserve the live conversation"
        );
        assert!(
            result.text().contains("615"),
            "Expected the live model to read the file via persisted session context, got {:?}",
            result.text()
        );
    }

    #[tokio::test]
    #[ignore = "Requires CLI credentials"]
    async fn test_live_compaction_archive_roundtrip_after_real_conversation() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("alpha.txt"),
            "Alpha module note: queue rebuild happens before checkpoint restore.",
        )
        .await
        .unwrap();
        fs::write(
            dir.path().join("beta.txt"),
            "Beta module note: archive verification must happen before publish.",
        )
        .await
        .unwrap();

        let manager = SessionManager::in_memory();
        let scoped = scoped_manager(&manager);

        let agent = Agent::builder()
            .auth(Auth::ClaudeCli)
            .await
            .expect("Failed to load CLI credentials")
            .session_manager(manager.clone())
            .tenant_id("tenant-live")
            .principal_id("user-live")
            .tools(ToolSurface::only(["Read"]))
            .working_dir(dir.path())
            .max_iterations(4)
            .build()
            .await
            .expect("Failed to build live agent");

        let first = agent
            .execute("Read alpha.txt and summarize the main note in one sentence.")
            .await
            .expect("First live turn failed");
        let second = agent
            .execute("Now read beta.txt and explain how it complements the first note.")
            .await
            .expect("Second live turn failed");

        let session_id = parse_session_id(second.session_id());
        let mut session = scoped
            .get(&session_id)
            .await
            .expect("Live session should be persisted before compaction");

        let client = Client::builder()
            .auth(Auth::ClaudeCli)
            .await
            .expect("Failed to load CLI credentials")
            .build()
            .await
            .expect("Failed to build live client");

        let executor = CompactService::new(CompactStrategy::default());
        let compact_result = executor
            .execute(&mut session, &client)
            .await
            .expect("Live compaction request failed");

        assert!(
            matches!(compact_result, CompactResult::Compacted { .. }),
            "Expected live compaction to produce a summary"
        );

        scoped
            .persist_snapshot(&session)
            .await
            .expect("Compacted live session should persist");

        let bundle = scoped
            .archive_bundle(
                &session.id,
                &ExportPolicy::default(),
                &ArchivePolicy::default(),
            )
            .await
            .expect("Archive export should succeed");

        let restore_persistence = MemoryPersistence::new();
        let restored =
            SessionArchiveService::restore_into(&bundle, &restore_persistence as &dyn Persistence)
                .await
                .expect("Archive restore should succeed");

        let restored_text = restored
            .current_branch_messages()
            .iter()
            .map(|message| collect_text(&message.content))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(first.session_id(), second.session_id());
        assert_eq!(restored.tenant_id.as_deref(), Some("tenant-live"));
        assert_eq!(restored.principal_id.as_deref(), Some("user-live"));
        assert_eq!(restored.compact_history.len(), 1);
        assert!(
            !restored.graph.checkpoints.is_empty(),
            "Compaction should leave a durable checkpoint"
        );
        assert!(
            restored.summary.is_some(),
            "Compaction summary should survive archive roundtrip"
        );
        assert!(
            restored_text.contains("Alpha") || restored_text.contains("archive"),
            "Restored compacted session should preserve summarized live content, got {:?}",
            restored_text
        );
    }
}
