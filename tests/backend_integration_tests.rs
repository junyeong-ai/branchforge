//! Backend integration tests for Dockerized PostgreSQL, Redis, and SurrealDB.
//!
//! These tests are ignored by default because they require local Docker-backed services.

#[cfg(feature = "surrealdb-backend")]
use std::sync::Arc;
use std::time::Duration;
#[cfg(feature = "surrealdb-backend")]
use std::time::Instant;

#[cfg(any(
    feature = "postgres",
    feature = "redis-backend",
    feature = "surrealdb-backend"
))]
use claude_agent::session::Persistence;
#[cfg(feature = "surrealdb-backend")]
use claude_agent::session::SurrealPersistence;
#[cfg(feature = "postgres")]
use claude_agent::session::{PostgresConfig, PostgresPersistence};
#[cfg(feature = "redis-backend")]
use claude_agent::session::{RedisConfig, RedisPersistence};
#[cfg(any(
    feature = "postgres",
    feature = "redis-backend",
    feature = "surrealdb-backend"
))]
use claude_agent::session::{Session, SessionConfig, SessionMessage};
#[cfg(feature = "surrealdb-backend")]
use claude_agent::session::{SessionAccessScope, SessionManager};
#[cfg(any(
    feature = "postgres",
    feature = "redis-backend",
    feature = "surrealdb-backend"
))]
use claude_agent::types::ContentBlock;
use tokio::time::sleep;
#[cfg(any(feature = "postgres", feature = "redis-backend"))]
use uuid::Uuid;

#[cfg(feature = "postgres")]
fn postgres_url() -> String {
    std::env::var("CLAUDE_AGENT_TEST_POSTGRES_URL").unwrap_or_else(|_| {
        "postgres://claude:claude@127.0.0.1:55432/claude_agent_test".to_string()
    })
}

#[cfg(feature = "redis-backend")]
fn redis_url() -> String {
    std::env::var("CLAUDE_AGENT_TEST_REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:56379/".to_string())
}

#[cfg(feature = "surrealdb-backend")]
fn surreal_url() -> String {
    std::env::var("CLAUDE_AGENT_TEST_SURREAL_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:58000/sql".to_string())
}

#[cfg(feature = "surrealdb-backend")]
fn surreal_persistence() -> SurrealPersistence {
    SurrealPersistence::new(
        claude_agent::session::SurrealConfig::default()
            .namespace("main")
            .database("main")
            .endpoint(surreal_url())
            .credentials("root", "root"),
    )
}

#[cfg(any(
    feature = "postgres",
    feature = "redis-backend",
    feature = "surrealdb-backend"
))]
fn seeded_session() -> Session {
    let mut session = Session::new(SessionConfig::default());
    session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
    session.add_message(SessionMessage::user(vec![ContentBlock::text("hello")]));
    session.add_message(SessionMessage::assistant(vec![ContentBlock::text("world")]));
    session.bookmark_current_head("head", Some("bookmark".to_string()));
    session.graph.create_checkpoint(
        session.graph.primary_branch,
        "checkpoint",
        Some("saved".to_string()),
        vec!["tag".to_string()],
        session.principal_id.clone(),
        None,
    );
    session
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "Requires local Docker PostgreSQL"]
async fn test_postgres_backend_roundtrip_graph_identity() {
    let prefix = format!("itest_{}__", Uuid::new_v4().simple());
    let config = PostgresConfig::prefix(&prefix).unwrap();
    let persistence = PostgresPersistence::connect_and_migrate_with_config(&postgres_url(), config)
        .await
        .expect("postgres should connect and migrate");

    let session = seeded_session();
    let session_id = session.id;
    persistence.save(&session).await.unwrap();

    let loaded = persistence.load(&session_id).await.unwrap().unwrap();
    assert_eq!(loaded.tenant_id.as_deref(), Some("tenant-a"));
    assert_eq!(loaded.principal_id.as_deref(), Some("user-1"));
    assert_eq!(loaded.current_branch_messages().len(), 2);
    assert_eq!(loaded.graph.bookmarks.len(), 1);
    assert_eq!(loaded.graph.checkpoints.len(), 1);

    let tenant_list = persistence.list(Some("tenant-a")).await.unwrap();
    assert_eq!(tenant_list, vec![session_id]);
}

#[cfg(feature = "redis-backend")]
#[tokio::test]
#[ignore = "Requires local Docker Redis"]
async fn test_redis_backend_roundtrip_graph_identity() {
    let prefix = format!("itest:{}:", Uuid::new_v4().simple());
    let persistence =
        RedisPersistence::from_config(&redis_url(), RedisConfig::default().prefix(prefix).unwrap())
            .expect("redis should initialize");

    let session = seeded_session();
    let session_id = session.id;
    persistence.save(&session).await.unwrap();

    let loaded = persistence.load(&session_id).await.unwrap().unwrap();
    assert_eq!(loaded.tenant_id.as_deref(), Some("tenant-a"));
    assert_eq!(loaded.principal_id.as_deref(), Some("user-1"));
    assert_eq!(loaded.current_branch_messages().len(), 2);
    assert_eq!(loaded.graph.bookmarks.len(), 1);
    assert_eq!(loaded.graph.checkpoints.len(), 1);

    let tenant_list = persistence.list(Some("tenant-a")).await.unwrap();
    assert_eq!(tenant_list, vec![session_id]);
}

#[cfg(feature = "surrealdb-backend")]
#[tokio::test]
#[ignore = "Requires local Docker SurrealDB"]
async fn test_surrealdb_backend_roundtrip_graph_identity() {
    let mut session = seeded_session();
    let unique_tenant = format!("tenant-{}", uuid::Uuid::new_v4().simple());
    session.set_identity(Some(unique_tenant.clone()), Some("user-1".to_string()));
    let persistence = surreal_persistence();

    let session_id = session.id;
    persistence.save(&session).await.unwrap();

    let loaded = persistence.load(&session_id).await.unwrap().unwrap();
    assert_eq!(loaded.tenant_id.as_deref(), Some(unique_tenant.as_str()));
    assert_eq!(loaded.principal_id.as_deref(), Some("user-1"));
    assert_eq!(loaded.current_branch_messages().len(), 2);
    assert_eq!(loaded.graph.bookmarks.len(), 1);
    assert_eq!(loaded.graph.checkpoints.len(), 1);

    let tenant_list = persistence.list(Some(&unique_tenant)).await.unwrap();
    assert_eq!(tenant_list, vec![session_id]);
}

#[cfg(feature = "surrealdb-backend")]
#[tokio::test]
#[ignore = "Requires local Docker SurrealDB"]
async fn test_surrealdb_backend_concurrent_session_writes() {
    let mut base = seeded_session();
    let unique_tenant = format!("tenant-{}", uuid::Uuid::new_v4().simple());
    base.set_identity(Some(unique_tenant.clone()), Some("user-1".to_string()));
    let session_id = base.id;

    let persistence = Arc::new(surreal_persistence());
    persistence.save(&base).await.unwrap();

    let mut left = base.clone();
    left.add_message(SessionMessage::assistant(vec![ContentBlock::text("left")]));
    let mut right = base;
    right.add_message(SessionMessage::assistant(vec![ContentBlock::text("right")]));

    let p1 = persistence.clone();
    let p2 = persistence.clone();
    let t1 = tokio::spawn(async move { p1.save(&left).await });
    let t2 = tokio::spawn(async move { p2.save(&right).await });

    t1.await.unwrap().unwrap();
    t2.await.unwrap().unwrap();

    let loaded = persistence.load(&session_id).await.unwrap().unwrap();
    let messages = loaded.current_branch_messages();
    let text = messages
        .last()
        .and_then(|message| message.content.first())
        .and_then(|block| block.as_text())
        .unwrap_or_default();

    assert!(text == "left" || text == "right");
    assert_eq!(loaded.tenant_id.as_deref(), Some(unique_tenant.as_str()));
}

#[cfg(feature = "surrealdb-backend")]
#[tokio::test]
#[ignore = "Requires local Docker SurrealDB"]
async fn test_surrealdb_backend_compaction_roundtrip() {
    let mut session = seeded_session();
    let unique_tenant = format!("tenant-{}", uuid::Uuid::new_v4().simple());
    session.set_identity(Some(unique_tenant.clone()), Some("user-1".to_string()));
    let session_id = session.id;

    let executor = claude_agent::session::CompactExecutor::new(
        claude_agent::session::CompactStrategy::default(),
    );
    let result = executor.apply_compact(&mut session, "summarized context".to_string());
    assert!(matches!(
        result,
        claude_agent::types::CompactResult::Compacted { .. }
    ));

    let persistence = surreal_persistence();
    persistence.save(&session).await.unwrap();

    let loaded = persistence.load(&session_id).await.unwrap().unwrap();
    assert_eq!(loaded.graph.checkpoints.len(), 2);
    assert_eq!(loaded.current_branch_messages().len(), 1);
    assert!(loaded.current_branch_messages()[0].is_compact_summary);
}

#[cfg(feature = "surrealdb-backend")]
#[tokio::test]
#[ignore = "Requires local Docker SurrealDB"]
async fn test_surrealdb_backend_scoped_graph_search() {
    let persistence = Arc::new(surreal_persistence());
    let manager = SessionManager::new(persistence.clone());

    let session = manager
        .create_with_identity(SessionConfig::default(), "tenant-surreal", "user-1")
        .await
        .unwrap();
    manager
        .add_message(
            &session.id,
            SessionMessage::user(vec![ContentBlock::text("alpha")]),
        )
        .await
        .unwrap();

    let allowed = manager
        .graph_search_scoped(
            &session.id,
            &SessionAccessScope::default()
                .tenant("tenant-surreal")
                .principal("user-1"),
            &claude_agent::graph::GraphSearchQuery {
                text: Some("alpha".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let denied = manager
        .graph_search_scoped(
            &session.id,
            &SessionAccessScope::default()
                .tenant("tenant-surreal")
                .principal("user-2"),
            &claude_agent::graph::GraphSearchQuery {
                text: Some("alpha".to_string()),
                ..Default::default()
            },
        )
        .await;

    assert_eq!(allowed.len(), 1);
    assert!(denied.is_err());
}

#[cfg(feature = "surrealdb-backend")]
#[tokio::test]
#[ignore = "Requires local Docker SurrealDB"]
async fn test_surrealdb_large_graph_baseline() {
    let persistence = surreal_persistence();
    let mut session = seeded_session();
    let unique_tenant = format!("tenant-{}", uuid::Uuid::new_v4().simple());
    session.set_identity(Some(unique_tenant), Some("user-1".to_string()));

    for i in 0..500 {
        session.add_message(SessionMessage::user(vec![ContentBlock::text(format!(
            "user-{i}"
        ))]));
        session.add_message(SessionMessage::assistant(vec![ContentBlock::text(
            format!("assistant-{i}"),
        )]));
    }

    let session_id = session.id;

    let save_start = Instant::now();
    persistence.save(&session).await.unwrap();
    let save_elapsed = save_start.elapsed();

    let load_start = Instant::now();
    let loaded = persistence.load(&session_id).await.unwrap().unwrap();
    let load_elapsed = load_start.elapsed();

    let search_start = Instant::now();
    let manager = SessionManager::new(Arc::new(persistence));
    let matches = manager
        .graph_search(
            &session_id,
            &claude_agent::graph::GraphSearchQuery {
                text: Some("assistant-499".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let search_elapsed = search_start.elapsed();

    assert_eq!(loaded.current_branch_messages().len(), 1002);
    assert_eq!(matches.len(), 1);

    eprintln!(
        "surreal baseline: save={:?} load={:?} search={:?}",
        save_elapsed, load_elapsed, search_elapsed
    );
}

#[cfg(feature = "surrealdb-backend")]
#[tokio::test]
#[ignore = "Requires local Docker SurrealDB"]
async fn test_surrealdb_failure_mode_invalid_endpoint() {
    let persistence = SurrealPersistence::new(
        claude_agent::session::SurrealConfig::default()
            .namespace("main")
            .database("main")
            .endpoint("http://127.0.0.1:59999/sql")
            .credentials("root", "root"),
    );

    let session = seeded_session();
    let error = persistence
        .save(&session)
        .await
        .expect_err("save should fail");
    let message = error.to_string();

    assert!(
        message.contains("SurrealDB")
            || message.contains("Connection")
            || message.contains("request failed")
    );
}

#[cfg(feature = "surrealdb-backend")]
#[tokio::test]
#[ignore = "Requires local Docker SurrealDB"]
async fn test_surrealdb_queue_stress() {
    let persistence = surreal_persistence();
    let session = seeded_session();
    let session_id = session.id;
    persistence.save(&session).await.unwrap();

    for i in 0..20 {
        persistence
            .enqueue(&session_id, format!("item-{i}"), i)
            .await
            .unwrap();
    }

    let pending = persistence.pending_queue(&session_id).await.unwrap();
    assert_eq!(pending.len(), 20);

    let first = persistence.dequeue(&session_id).await.unwrap().unwrap();
    assert_eq!(first.priority, 19);

    let second = persistence.dequeue(&session_id).await.unwrap().unwrap();
    assert_eq!(second.priority, 18);
}

#[tokio::test]
#[ignore = "Requires local Docker backend services"]
async fn test_backend_containers_are_reachable() {
    let mut failures = Vec::new();

    let postgres = tokio::net::TcpStream::connect("127.0.0.1:55432").await;
    if postgres.is_err() {
        failures.push("postgres:55432");
    }

    let redis = tokio::net::TcpStream::connect("127.0.0.1:56379").await;
    if redis.is_err() {
        failures.push("redis:56379");
    }

    let surreal = tokio::net::TcpStream::connect("127.0.0.1:58000").await;
    if surreal.is_err() {
        failures.push("surrealdb:58000");
    }

    assert!(
        failures.is_empty(),
        "unreachable test backends: {:?}",
        failures
    );

    sleep(Duration::from_millis(50)).await;
}
