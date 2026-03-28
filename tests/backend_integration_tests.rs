//! Backend integration tests for local Dockerized PostgreSQL and Redis.
//!
//! These tests are ignored by default because they require local Docker-backed services.
//! Use `docker-compose.backends.local.yml` or `scripts/validate-local-backends.sh`.

#[cfg(any(feature = "postgres", feature = "redis-backend"))]
use std::sync::Arc;
#[cfg(any(feature = "postgres", feature = "redis-backend"))]
use std::time::Duration;

#[cfg(any(feature = "postgres", feature = "redis-backend"))]
use branchforge::session::Persistence;
#[cfg(any(feature = "jsonl", feature = "postgres", feature = "redis-backend"))]
use branchforge::session::SessionArchiveService;
#[cfg(feature = "jsonl")]
use branchforge::session::{ArchivePolicy, ExportPolicy, JsonlConfig, JsonlPersistence};
#[cfg(feature = "postgres")]
use branchforge::session::{PostgresConfig, PostgresPersistence};
#[cfg(feature = "redis-backend")]
use branchforge::session::{QueueItem, QueueOperation, QueueStatus};
#[cfg(feature = "redis-backend")]
use branchforge::session::{RedisConfig, RedisPersistence};
#[cfg(any(feature = "jsonl", feature = "postgres", feature = "redis-backend"))]
use branchforge::session::{Session, SessionConfig, SessionMessage};
#[cfg(any(feature = "jsonl", feature = "postgres", feature = "redis-backend"))]
use branchforge::types::ContentBlock;
#[cfg(feature = "redis-backend")]
use chrono::Utc;
#[cfg(feature = "jsonl")]
use tempfile::TempDir;
#[cfg(any(feature = "postgres", feature = "redis-backend"))]
use tokio::task::JoinSet;
#[cfg(any(feature = "postgres", feature = "redis-backend"))]
use tokio::time::sleep;
#[cfg(any(feature = "jsonl", feature = "postgres", feature = "redis-backend"))]
use uuid::Uuid;

#[cfg(feature = "postgres")]
fn postgres_url() -> String {
    std::env::var("BRANCHFORGE_TEST_POSTGRES_URL").unwrap_or_else(|_| {
        "postgres://branchforge:branchforge@127.0.0.1:55432/branchforge_test".to_string()
    })
}

#[cfg(feature = "redis-backend")]
fn redis_url() -> String {
    std::env::var("BRANCHFORGE_TEST_REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:56379/".to_string())
}

#[cfg(any(feature = "jsonl", feature = "postgres", feature = "redis-backend"))]
fn seeded_session() -> Session {
    let mut session = Session::new(SessionConfig::default());
    session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
    session
        .add_message(SessionMessage::user(vec![ContentBlock::text("hello")]))
        .unwrap();
    session
        .add_message(SessionMessage::assistant(vec![ContentBlock::text("world")]))
        .unwrap();
    session.bookmark_current_head("head", Some("bookmark".to_string()));
    session
        .checkpoint_current_head(
            "checkpoint",
            Some("saved".to_string()),
            vec!["tag".to_string()],
        )
        .unwrap();
    session
}

#[cfg(any(feature = "postgres", feature = "redis-backend"))]
async fn assert_concurrent_add_message_preserves_all_messages<P>(persistence: Arc<P>)
where
    P: Persistence + Send + Sync + 'static,
{
    let session = Session::new(SessionConfig::default());
    let session_id = session.id;
    persistence.save(&session).await.unwrap();

    const MESSAGE_COUNT: usize = 12;
    let mut set = JoinSet::new();

    for idx in 0..MESSAGE_COUNT {
        let persistence = Arc::clone(&persistence);
        set.spawn(async move {
            let message = SessionMessage::user(vec![ContentBlock::text(format!("message-{idx}"))]);
            persistence.add_message(&session_id, message).await.unwrap();
        });
    }

    while let Some(result) = set.join_next().await {
        result.expect("concurrent backend write should complete");
    }

    let loaded = persistence.load(&session_id).await.unwrap().unwrap();
    let messages = loaded.current_branch_messages();
    assert_eq!(messages.len(), MESSAGE_COUNT);

    let mut actual_texts: Vec<String> = messages
        .iter()
        .filter_map(|message| message.content.iter().find_map(|block| block.as_text()))
        .map(ToOwned::to_owned)
        .collect();
    let mut expected_texts: Vec<String> = (0..MESSAGE_COUNT)
        .map(|idx| format!("message-{idx}"))
        .collect();
    actual_texts.sort();
    expected_texts.sort();

    assert_eq!(actual_texts, expected_texts);
}

#[cfg(any(feature = "jsonl", feature = "postgres", feature = "redis-backend"))]
async fn assert_archive_restore_preserves_graph_queue_and_refuses_overwrite<P>(persistence: Arc<P>)
where
    P: Persistence + Send + Sync + 'static,
{
    let session = seeded_session();
    let pending = vec![
        branchforge::session::QueueItem::enqueue(session.id, "first").priority(10),
        branchforge::session::QueueItem::enqueue(session.id, "second").priority(1),
    ];

    let bundle = SessionArchiveService::export_bundle(
        &session,
        &ExportPolicy::default(),
        &ArchivePolicy {
            include_queue_state: true,
            ..ArchivePolicy::default()
        },
        pending.clone(),
    )
    .expect("bundle should be created");

    let restored = SessionArchiveService::restore_into(&bundle, persistence.as_ref())
        .await
        .expect("archive restore should succeed");
    assert_eq!(restored.tenant_id.as_deref(), Some("tenant-a"));
    assert_eq!(restored.principal_id.as_deref(), Some("user-1"));
    assert_eq!(restored.current_branch_messages().len(), 2);
    assert_eq!(restored.graph.bookmarks.len(), 1);
    assert_eq!(restored.graph.checkpoints.len(), 1);

    let restored_queue = persistence.pending_queue(&restored.id).await.unwrap();
    assert_eq!(restored_queue.len(), 2);
    assert_eq!(restored_queue[0].content, "first");
    assert_eq!(restored_queue[1].content, "second");

    let overwrite = SessionArchiveService::restore_into(&bundle, persistence.as_ref())
        .await
        .expect_err("restoring same bundle twice should refuse overwrite");
    let overwrite_message = overwrite.to_string();
    assert!(
        overwrite_message.contains("overwrite") || overwrite_message.contains("existing"),
        "unexpected overwrite error: {}",
        overwrite_message
    );
}

#[cfg(feature = "jsonl")]
async fn create_jsonl_persistence() -> (Arc<JsonlPersistence>, TempDir) {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let config = JsonlConfig::builder().base_dir(temp.path()).build();
    let persistence = JsonlPersistence::new(config)
        .await
        .expect("jsonl persistence should initialize");
    (Arc::new(persistence), temp)
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

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "Requires local Docker PostgreSQL"]
async fn test_postgres_backend_plan_clear_and_state_update() {
    let prefix = format!("itest_{}__", Uuid::new_v4().simple());
    let config = PostgresConfig::prefix(&prefix).unwrap();
    let persistence = PostgresPersistence::connect_and_migrate_with_config(&postgres_url(), config)
        .await
        .expect("postgres should connect and migrate");

    let mut session = Session::new(SessionConfig::default());
    session.enter_plan_mode(Some("demo".to_string()));
    session.update_plan_content("ship it".to_string());
    let session_id = session.id;
    persistence.save(&session).await.unwrap();

    session.cancel_plan();
    persistence.save(&session).await.unwrap();
    persistence
        .set_state(&session_id, branchforge::session::SessionState::Completed)
        .await
        .unwrap();

    let loaded = persistence.load(&session_id).await.unwrap().unwrap();
    assert!(loaded.current_plan.is_none());
    assert_eq!(loaded.state, branchforge::session::SessionState::Completed);
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "Requires local Docker PostgreSQL"]
async fn test_postgres_backend_concurrent_add_message_preserves_all_messages() {
    let prefix = format!("itest_{}__", Uuid::new_v4().simple());
    let config = PostgresConfig::prefix(&prefix).unwrap();
    let persistence = Arc::new(
        PostgresPersistence::connect_and_migrate_with_config(&postgres_url(), config)
            .await
            .expect("postgres should connect and migrate"),
    );

    assert_concurrent_add_message_preserves_all_messages(persistence).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "Requires local Docker PostgreSQL"]
async fn test_postgres_backend_archive_restore_preserves_graph_queue_and_refuses_overwrite() {
    let prefix = format!("itest_{}__", Uuid::new_v4().simple());
    let config = PostgresConfig::prefix(&prefix).unwrap();
    let persistence = Arc::new(
        PostgresPersistence::connect_and_migrate_with_config(&postgres_url(), config)
            .await
            .expect("postgres should connect and migrate"),
    );

    assert_archive_restore_preserves_graph_queue_and_refuses_overwrite(persistence).await;
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

#[cfg(feature = "redis-backend")]
#[tokio::test]
#[ignore = "Requires local Docker Redis"]
async fn test_redis_restore_preserves_queue_ttl_symmetry() {
    let prefix = format!("itest:{}:", Uuid::new_v4().simple());
    let ttl = Duration::from_secs(30);
    let persistence = RedisPersistence::from_config(
        &redis_url(),
        RedisConfig::default()
            .prefix(prefix.clone())
            .unwrap()
            .ttl(ttl),
    )
    .expect("redis should initialize");

    let session = Session::new(SessionConfig::default());
    let queue_item = QueueItem {
        id: Uuid::new_v4(),
        session_id: session.id,
        operation: QueueOperation::Enqueue,
        content: "pending".to_string(),
        priority: 5,
        status: QueueStatus::Pending,
        created_at: Utc::now(),
        processed_at: None,
    };

    persistence
        .restore_bundle(&session, std::slice::from_ref(&queue_item))
        .await
        .unwrap();

    let client = redis::Client::open(redis_url()).unwrap();
    let mut conn: redis::aio::MultiplexedConnection =
        client.get_multiplexed_async_connection().await.unwrap();
    let session_key = format!("{}{}", prefix, session.id);
    let queue_key = format!("{}queue:{}", prefix, session.id);

    let session_ttl: i64 = redis::cmd("TTL")
        .arg(&session_key)
        .query_async(&mut conn)
        .await
        .unwrap();
    let queue_ttl: i64 = redis::cmd("TTL")
        .arg(&queue_key)
        .query_async(&mut conn)
        .await
        .unwrap();

    assert!(session_ttl > 0, "restored session key should have TTL");
    assert!(queue_ttl > 0, "restored queue key should have TTL");
    assert!(
        (session_ttl - queue_ttl).abs() <= 1,
        "queue TTL should track session TTL: session={}, queue={}",
        session_ttl,
        queue_ttl
    );
}

#[cfg(feature = "redis-backend")]
#[tokio::test]
#[ignore = "Requires local Docker Redis"]
async fn test_redis_backend_concurrent_add_message_preserves_all_messages() {
    let prefix = format!("itest:{}:", Uuid::new_v4().simple());
    let persistence = Arc::new(
        RedisPersistence::from_config(&redis_url(), RedisConfig::default().prefix(prefix).unwrap())
            .expect("redis should initialize"),
    );

    assert_concurrent_add_message_preserves_all_messages(persistence).await;
}

#[cfg(feature = "redis-backend")]
#[tokio::test]
#[ignore = "Requires local Docker Redis"]
async fn test_redis_backend_archive_restore_preserves_graph_queue_and_refuses_overwrite() {
    let prefix = format!("itest:{}:", Uuid::new_v4().simple());
    let persistence = Arc::new(
        RedisPersistence::from_config(&redis_url(), RedisConfig::default().prefix(prefix).unwrap())
            .expect("redis should initialize"),
    );

    assert_archive_restore_preserves_graph_queue_and_refuses_overwrite(persistence).await;
}

#[cfg(feature = "jsonl")]
#[tokio::test]
async fn test_jsonl_backend_roundtrip_graph_identity() {
    let (persistence, _temp) = create_jsonl_persistence().await;

    let session = seeded_session();
    let session_id = session.id;
    persistence.save(&session).await.unwrap();

    let loaded = persistence.load(&session_id).await.unwrap().unwrap();
    assert_eq!(loaded.tenant_id.as_deref(), Some("tenant-a"));
    assert_eq!(loaded.principal_id.as_deref(), Some("user-1"));
    assert_eq!(loaded.current_branch_messages().len(), 2);
    assert_eq!(loaded.graph.bookmarks.len(), 1);
    assert_eq!(loaded.graph.checkpoints.len(), 1);
}

#[cfg(feature = "jsonl")]
#[tokio::test]
async fn test_jsonl_backend_concurrent_add_message_preserves_all_messages() {
    let (persistence, _temp) = create_jsonl_persistence().await;
    assert_concurrent_add_message_preserves_all_messages(persistence).await;
}

#[cfg(feature = "jsonl")]
#[tokio::test]
async fn test_jsonl_backend_archive_restore_preserves_graph_queue_and_refuses_overwrite() {
    let (persistence, _temp) = create_jsonl_persistence().await;
    assert_archive_restore_preserves_graph_queue_and_refuses_overwrite(persistence).await;
}

#[cfg(any(feature = "postgres", feature = "redis-backend"))]
#[tokio::test]
#[ignore = "Requires local Docker backend services"]
async fn test_backend_containers_are_reachable() {
    let mut failures: Vec<&str> = Vec::new();

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
