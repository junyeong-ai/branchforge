//! Backend integration tests for Dockerized PostgreSQL, Redis, and SurrealDB.
//!
//! These tests are ignored by default because they require local Docker-backed services.

use std::time::Duration;

#[cfg(any(feature = "postgres", feature = "redis-backend"))]
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
#[cfg(any(
    feature = "postgres",
    feature = "redis-backend",
    feature = "surrealdb-backend"
))]
use claude_agent::types::ContentBlock;
#[cfg(feature = "surrealdb-backend")]
use reqwest::Client;
use tokio::time::sleep;
#[cfg(any(
    feature = "postgres",
    feature = "redis-backend",
    feature = "surrealdb-backend"
))]
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
async fn test_surrealdb_graph_queries_and_prototype_export() {
    let session = seeded_session();
    let prototype = SurrealPersistence::new(Default::default());
    let records = prototype.export_graph_records(&session);

    assert!(
        records
            .iter()
            .any(|record| record.principal_id.as_deref() == Some("user-1"))
    );
    assert!(
        records
            .iter()
            .any(|record| record.tenant_id.as_deref() == Some("tenant-a"))
    );

    let client = Client::new();
    let namespace = "main";
    let suffix = Uuid::new_v4().simple().to_string();

    let sql = format!(
        "DEFINE TABLE session SCHEMALESS;\nDEFINE TABLE branch SCHEMALESS;\nDEFINE TABLE node SCHEMALESS;\nDEFINE TABLE edge_parent TYPE RELATION IN node OUT node;\nCREATE session:{suffix} SET tenant_id = 'tenant-a', principal_id = 'user-1';\nCREATE branch:{suffix}_main SET session = session:{suffix};\nCREATE branch:{suffix}_alt SET session = session:{suffix};\nCREATE node:{suffix}_root SET session = session:{suffix}, branch = branch:{suffix}_main, kind = 'user', text = 'root';\nCREATE node:{suffix}_reply SET session = session:{suffix}, branch = branch:{suffix}_main, kind = 'assistant', text = 'reply';\nCREATE node:{suffix}_alt_reply SET session = session:{suffix}, branch = branch:{suffix}_alt, kind = 'assistant', text = 'alt';\nRELATE node:{suffix}_root->edge_parent->node:{suffix}_reply;\nRELATE node:{suffix}_root->edge_parent->node:{suffix}_alt_reply;\nSELECT ->edge_parent->node AS children FROM node:{suffix}_root;\nSELECT count() AS total FROM node WHERE session = session:{suffix};"
    );

    let response = client
        .post(surreal_url())
        .basic_auth("root", Some("root"))
        .header("surreal-ns", namespace)
        .header("surreal-db", "main")
        .header("Accept", "application/json")
        .body(sql)
        .send()
        .await
        .expect("surreal request should succeed");

    let body = response.text().await.expect("response body");
    assert!(body.contains("children"));
    assert!(body.contains("reply"));
    assert!(body.contains("alt"));
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
