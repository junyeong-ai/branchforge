//! Context & Session Tests
//!
//! Tests for memory loading, context building, session management, persistence,
//! settings, and compact strategy.
//!
//! Run: cargo nextest run --test context_and_session_tests --all-features

use claude_agent::context::{ContextBuilder, MemoryLoader};
use tempfile::tempdir;
use tokio::fs;

// =============================================================================
// Memory Loader
// =============================================================================

mod memory_loader_tests {
    use super::*;

    #[tokio::test]
    async fn test_memory_load_simple_claude_md() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "# Project\nTest content")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        assert_eq!(content.claude_md.len(), 1);
        assert!(content.claude_md[0].contains("Test content"));
    }

    #[tokio::test]
    async fn test_memory_load_claude_md_in_dot_claude_dir() {
        let dir = tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        fs::create_dir_all(&claude_dir).await.unwrap();
        fs::write(claude_dir.join("CLAUDE.md"), "Content in .claude dir")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        assert_eq!(content.claude_md.len(), 1);
        assert!(content.claude_md[0].contains(".claude dir"));
    }

    #[tokio::test]
    async fn test_memory_load_rules_directory() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".claude").join("rules");
        fs::create_dir_all(&rules_dir).await.unwrap();

        fs::write(rules_dir.join("rust.md"), "# Rust Rules\nUse snake_case")
            .await
            .unwrap();
        fs::write(
            rules_dir.join("security.md"),
            "# Security\nNo hardcoded secrets",
        )
        .await
        .unwrap();
        fs::write(
            rules_dir.join("typescript.md"),
            "# TypeScript\nUse strict mode",
        )
        .await
        .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        assert_eq!(content.rule_indices.len(), 3);
        let rule_names: Vec<_> = content
            .rule_indices
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert!(rule_names.contains(&"rust"));
        assert!(rule_names.contains(&"security"));
        assert!(rule_names.contains(&"typescript"));
    }

    #[tokio::test]
    async fn test_memory_import_syntax() {
        let dir = tempdir().unwrap();
        let docs_dir = dir.path().join("docs");
        fs::create_dir_all(&docs_dir).await.unwrap();
        fs::write(
            docs_dir.join("guidelines.md"),
            "## Guidelines\nFollow these rules",
        )
        .await
        .unwrap();

        fs::write(
            dir.path().join("CLAUDE.md"),
            "# Project\n@docs/guidelines.md\n# End",
        )
        .await
        .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        let combined = content.combined_claude_md();
        assert!(combined.contains("# Project"));
        assert!(combined.contains("## Guidelines"));
        assert!(combined.contains("Follow these rules"));
        assert!(combined.contains("# End"));
    }

    #[tokio::test]
    async fn test_memory_nested_imports() {
        let dir = tempdir().unwrap();
        let level1 = dir.path().join("level1");
        let level2 = level1.join("level2");
        fs::create_dir_all(&level2).await.unwrap();

        fs::write(level2.join("deep.md"), "Deep content")
            .await
            .unwrap();
        fs::write(level1.join("mid.md"), "Mid content\n@level2/deep.md")
            .await
            .unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "Top\n@level1/mid.md")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        let combined = content.combined_claude_md();
        assert!(combined.contains("Top"));
        assert!(combined.contains("Mid content"));
        assert!(combined.contains("Deep content"));
    }

    #[tokio::test]
    async fn test_memory_circular_import_prevention() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "A\n@b.md")
            .await
            .unwrap();
        fs::write(dir.path().join("b.md"), "B\n@a.md")
            .await
            .unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "@a.md")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let result = loader.load(dir.path()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_memory_missing_import_file() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "Content\n@nonexistent.md\nMore content",
        )
        .await
        .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        let combined = content.combined_claude_md();
        assert!(combined.contains("Content"));
        assert!(combined.contains("@nonexistent.md"));
        assert!(combined.contains("More content"));
    }

    #[tokio::test]
    async fn test_memory_combined_content_with_rules() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".claude").join("rules");
        fs::create_dir_all(&rules_dir).await.unwrap();

        fs::write(dir.path().join("CLAUDE.md"), "Main content")
            .await
            .unwrap();
        fs::write(rules_dir.join("test.md"), "Rule content")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        let combined = content.combined_claude_md();
        assert!(combined.contains("Main content"));
        assert!(!content.rule_indices.is_empty());
        assert!(content.rule_indices.iter().any(|r| r.name == "test"));
    }

    #[tokio::test]
    async fn test_memory_escape_at_syntax() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "Email: @@user@example.com\n@@ is escaped",
        )
        .await
        .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        let combined = content.combined_claude_md();
        assert!(combined.contains("@@user@example.com"));
        assert!(combined.contains("@@ is escaped"));
    }

    #[tokio::test]
    async fn test_memory_home_dir_import_syntax() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "# Project\n@~/nonexistent_file.md\n# End",
        )
        .await
        .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        let combined = content.combined_claude_md();
        assert!(combined.contains("# Project"));
        assert!(combined.contains("@~/nonexistent_file.md"));
    }
}

// =============================================================================
// Context Builder
// =============================================================================

mod context_builder_tests {
    use super::*;

    #[tokio::test]
    async fn test_context_builder_with_memory_loader() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "# Project Context\nImportant info",
        )
        .await
        .unwrap();

        let context = ContextBuilder::new()
            .load_from_directory(dir.path())
            .await
            .build()
            .unwrap();

        let static_ctx = context.static_context();
        assert!(static_ctx.claude_md.contains("Project Context"));
        assert!(static_ctx.claude_md.contains("Important info"));
    }

    #[tokio::test]
    async fn test_context_builder_with_all_sources() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".claude").join("rules");
        fs::create_dir_all(&rules_dir).await.unwrap();

        fs::write(dir.path().join("CLAUDE.md"), "Main content")
            .await
            .unwrap();
        fs::write(rules_dir.join("rule1.md"), "Rule 1 content")
            .await
            .unwrap();

        let context = ContextBuilder::new()
            .load_from_directory(dir.path())
            .await
            .build()
            .unwrap();

        let md = &context.static_context().claude_md;
        assert!(md.contains("Main content"));

        let summary = context.build_rules_summary().await;
        assert!(summary.contains("rule1"));
    }
}

// =============================================================================
// Static Context & System Blocks
// =============================================================================

mod static_context_tests {
    use claude_agent::context::StaticContext;

    #[test]
    fn test_static_context_builder() {
        let context = StaticContext::new()
            .system_prompt("You are a helpful assistant.")
            .claude_md("# Project")
            .skill_summary("Available skills: commit, review");

        assert_eq!(context.system_prompt, "You are a helpful assistant.");
        assert_eq!(context.claude_md, "# Project");
        assert_eq!(context.skill_summary, "Available skills: commit, review");
    }
}

// =============================================================================
// Session Management
// =============================================================================

mod session_tests {
    use claude_agent::session::{
        CompactExecutor, CompactStrategy, Session, SessionConfig, SessionManager, SessionMessage,
    };
    use claude_agent::types::ContentBlock;

    #[test]
    fn test_session_creation_and_messages() {
        let config = SessionConfig::default();
        let mut session = Session::new(config);

        let user_msg = SessionMessage::user(vec![ContentBlock::text("Hello")]);
        session.add_message(user_msg);

        let assistant_msg = SessionMessage::assistant(vec![ContentBlock::text("Hi there!")]);
        session.add_message(assistant_msg);

        assert_eq!(session.messages.len(), 2);
        assert!(session.current_leaf_id.is_some());
    }

    #[test]
    fn test_context_compaction_threshold() {
        let strategy = CompactStrategy::default().threshold(0.8);
        let executor = CompactExecutor::new(strategy);

        assert!(!executor.needs_compact(70_000, 100_000));
        assert!(executor.needs_compact(80_000, 100_000));
        assert!(executor.needs_compact(90_000, 100_000));
    }

    #[tokio::test]
    async fn test_session_manager_multi_session() {
        let manager = SessionManager::in_memory();

        let session1 = manager.create(SessionConfig::default()).await.unwrap();
        let session2 = manager.create(SessionConfig::default()).await.unwrap();

        assert_ne!(session1.id, session2.id);

        let found = manager.get(&session1.id).await;
        assert!(found.is_ok());
    }

    #[test]
    fn test_compact_strategy_default() {
        let strategy = CompactStrategy::default();
        assert!(strategy.enabled);
        assert_eq!(strategy.threshold_percent, 0.8);
    }

    #[test]
    fn test_compact_strategy_disabled() {
        let strategy = CompactStrategy::disabled();
        assert!(!strategy.enabled);
    }

    #[test]
    fn test_compact_strategy_custom() {
        let strategy = CompactStrategy::default()
            .threshold(0.9)
            .model("claude-haiku-4-5-20251001");

        assert_eq!(strategy.threshold_percent, 0.9);
        assert_eq!(strategy.summary_model, "claude-haiku-4-5-20251001");
    }
}

// =============================================================================
// Settings
// =============================================================================

mod settings_tests {
    use claude_agent::config::SettingsLoader;

    #[test]
    fn test_settings_loader_defaults() {
        let loader = SettingsLoader::new();
        let settings = loader.settings();
        assert!(settings.env.is_empty());
        assert!(settings.permissions.allow.is_empty());
        assert!(settings.permissions.deny.is_empty());
        assert_eq!(settings.permissions.default_mode, None);
    }

    #[tokio::test]
    async fn test_settings_loading() {
        use tempfile::tempdir;
        use tokio::fs;

        let dir = tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        fs::create_dir_all(&claude_dir).await.unwrap();

        fs::write(
            claude_dir.join("settings.json"),
            r#"{
                "env": {
                    "TEST_VAR": "test_value"
                },
                "permissions": {
                    "deny": ["Read(./.env)"]
                }
            }"#,
        )
        .await
        .unwrap();

        let mut loader = SettingsLoader::new();
        let settings = loader.load(dir.path()).await.unwrap();

        assert_eq!(
            settings.env.get("TEST_VAR"),
            Some(&"test_value".to_string())
        );
        assert!(
            settings
                .permissions
                .deny
                .contains(&"Read(./.env)".to_string())
        );
    }

    #[tokio::test]
    async fn test_settings_local_override() {
        use tempfile::tempdir;
        use tokio::fs;

        let dir = tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        fs::create_dir_all(&claude_dir).await.unwrap();

        fs::write(
            claude_dir.join("settings.json"),
            r#"{"env": {"VAR": "original"}}"#,
        )
        .await
        .unwrap();

        fs::write(
            claude_dir.join("settings.local.json"),
            r#"{"env": {"VAR": "overridden", "LOCAL_VAR": "local"}}"#,
        )
        .await
        .unwrap();

        let mut loader = SettingsLoader::new();
        let settings = loader.load(dir.path()).await.unwrap();

        assert_eq!(settings.env.get("VAR"), Some(&"overridden".to_string()));
        assert_eq!(settings.env.get("LOCAL_VAR"), Some(&"local".to_string()));
    }
}

// =============================================================================
// Live Tests (require CLI credentials)
// =============================================================================

mod live_tests {
    use claude_agent::{Agent, ToolAccess};
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    #[ignore = "Requires CLI credentials"]
    async fn test_live_agent_with_memory_context() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "# Test Project\nThis is a test project for verification.\nThe secret code is 42.",
        )
        .await
        .unwrap();

        fs::write(dir.path().join("data.txt"), "The answer is 42.")
            .await
            .unwrap();

        let agent = Agent::builder()
            .from_claude_code(dir.path())
            .await
            .expect("Auth failed")
            .tools(ToolAccess::only(["Read"]))
            .working_dir(dir.path())
            .max_iterations(3)
            .build()
            .await
            .expect("Failed to create agent");

        let result = agent
            .execute("Read data.txt and tell me the answer. Just the number.")
            .await
            .expect("Agent failed");

        assert!(result.tool_calls > 0 || result.text().contains("42"));
    }
}
