//! Backend integration tests for Dockerized PostgreSQL and Redis.
//!
//! These tests are ignored by default because they require local Docker-backed services.

use std::time::Duration;

#[cfg(any(feature = "postgres", feature = "redis-backend"))]
use claude_agent::session::Persistence;
#[cfg(feature = "postgres")]
use claude_agent::session::{PostgresConfig, PostgresPersistence};
#[cfg(feature = "redis-backend")]
use claude_agent::session::{RedisConfig, RedisPersistence};
#[cfg(any(feature = "postgres", feature = "redis-backend"))]
use claude_agent::session::{Session, SessionConfig, SessionMessage};
#[cfg(any(feature = "postgres", feature = "redis-backend"))]
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

#[cfg(any(feature = "postgres", feature = "redis-backend"))]
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

#[tokio::test]
#[ignore = "Requires local Docker backend services"]
async fn test_backend_containers_are_reachable() {
    let mut failures = Vec::new();

    #[cfg(feature = "postgres")]
    if tokio::net::TcpStream::connect("127.0.0.1:55432")
        .await
        .is_err()
    {
        failures.push("postgres:55432");
    }

    #[cfg(feature = "redis-backend")]
    if tokio::net::TcpStream::connect("127.0.0.1:56379")
        .await
        .is_err()
    {
        failures.push("redis:56379");
    }

    assert!(
        failures.is_empty(),
        "unreachable test backends: {:?}",
        failures
    );

    sleep(Duration::from_millis(50)).await;
}
