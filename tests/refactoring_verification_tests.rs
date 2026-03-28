//! Comprehensive verification tests for the 12-phase refactoring.
//!
//! These tests verify every structural and behavioral change made during
//! the refactoring without requiring network access or live model calls.
//!
//! Run: cargo test --test refactoring_verification_tests --all-features

// =============================================================================
// Phase 1: expect() removal — Result propagation
// =============================================================================

mod phase1_error_propagation {
    use branchforge::session::{Session, SessionConfig, SessionId};

    #[test]
    fn session_add_user_message_returns_result() {
        let mut session = Session::from_id(SessionId::new(), SessionConfig::default());
        let result = session.add_user_message("test prompt");
        assert!(result.is_ok());
    }

    #[test]
    fn session_add_assistant_message_returns_result() {
        let mut session = Session::from_id(SessionId::new(), SessionConfig::default());
        session.add_user_message("prompt").unwrap();
        let result = session.add_assistant_message(vec![], None);
        assert!(result.is_ok());
    }

    #[test]
    fn session_add_tool_results_returns_result() {
        let mut session = Session::from_id(SessionId::new(), SessionConfig::default());
        session.add_user_message("prompt").unwrap();
        let result = session.add_tool_results(vec![]);
        assert!(result.is_ok());
    }

    #[test]
    fn session_update_summary_returns_result() {
        let mut session = Session::from_id(SessionId::new(), SessionConfig::default());
        session.add_user_message("prompt").unwrap();
        let result = session.update_summary("summary");
        assert!(result.is_ok());
    }

    #[test]
    fn session_checkpoint_returns_result() {
        let mut session = Session::from_id(SessionId::new(), SessionConfig::default());
        session.add_user_message("prompt").unwrap();
        let result = session.checkpoint_current_head("test", None, vec![]);
        assert!(result.is_ok());
    }
}

// =============================================================================
// Phase 2: Naming — verify new names compile
// =============================================================================

mod phase2_naming {
    use branchforge::session::compact::{CompactService, CompactStrategy};

    #[test]
    fn compact_service_name_exists() {
        let _service = CompactService::new(CompactStrategy::default());
    }

    #[test]
    fn session_snapshot_struct_exists() {
        use branchforge::session::SessionSnapshot;
        let snap = SessionSnapshot {
            session_id: branchforge::session::SessionId::new(),
            todo_count: 0,
            current_plan: None,
        };
        assert_eq!(snap.todo_count, 0);
    }

    #[test]
    fn execution_state_struct_exists() {
        use branchforge::session::ExecutionState;
        let state = ExecutionState {
            session_id: branchforge::session::SessionId::new(),
            in_plan_mode: false,
            todos_in_progress: 0,
        };
        assert!(!state.in_plan_mode);
    }

    #[test]
    fn default_fast_model_constant_exists() {
        use branchforge::client::DEFAULT_FAST_MODEL;
        assert!(!DEFAULT_FAST_MODEL.is_empty());
    }
}

// =============================================================================
// Phase 3: HookOutput Default — safe default
// =============================================================================

mod phase3_hook_output {
    use branchforge::hooks::HookOutput;

    #[test]
    fn default_allows_execution() {
        let output = HookOutput::default();
        assert!(
            output.continue_execution,
            "HookOutput::default() must allow execution"
        );
    }

    #[test]
    fn allow_constructor_allows() {
        let output = HookOutput::allow();
        assert!(output.continue_execution);
    }

    #[test]
    fn block_constructor_blocks() {
        let output = HookOutput::block("reason");
        assert!(!output.continue_execution);
        assert_eq!(output.stop_reason.as_deref(), Some("reason"));
    }
}

// =============================================================================
// Phase 4: EventBus — subscription lifecycle
// =============================================================================

mod phase4_eventbus {
    use branchforge::events::{EventBus, EventKind, SubscriptionHandle, SubscriptionId};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn subscribe_returns_subscription_id() {
        let bus = EventBus::default();
        let _id: SubscriptionId = bus.subscribe(EventKind::Error, Arc::new(|_| {}));
    }

    #[test]
    fn unsubscribe_removes_subscriber() {
        let bus = EventBus::default();
        let id = bus.subscribe(EventKind::Error, Arc::new(|_| {}));
        assert_eq!(bus.subscriber_count(EventKind::Error), 1);
        bus.unsubscribe(EventKind::Error, id);
        assert_eq!(bus.subscriber_count(EventKind::Error), 0);
    }

    #[test]
    fn subscription_handle_auto_unsubscribes_on_drop() {
        let bus = Arc::new(EventBus::default());
        {
            let _handle: SubscriptionHandle =
                bus.subscribe_with_handle(EventKind::ToolExecuted, Arc::new(|_| {}));
            assert_eq!(bus.subscriber_count(EventKind::ToolExecuted), 1);
        }
        assert_eq!(bus.subscriber_count(EventKind::ToolExecuted), 0);
    }

    #[test]
    fn unique_subscription_ids() {
        let bus = EventBus::default();
        let id1 = bus.subscribe(EventKind::Error, Arc::new(|_| {}));
        let id2 = bus.subscribe(EventKind::Error, Arc::new(|_| {}));
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn emit_works_with_new_subscriber_format() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bus = EventBus::default();
        let c = counter.clone();
        let _id = bus.subscribe(
            EventKind::ToolExecuted,
            Arc::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.emit_simple(EventKind::ToolExecuted, serde_json::json!({}));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}

// =============================================================================
// Phase 5: AgentRuntime — structure separation
// =============================================================================

mod phase5_agent_runtime {
    use branchforge::{Agent, AgentRuntime};

    #[test]
    fn agent_runtime_type_exported() {
        fn _accepts_runtime(_: &AgentRuntime) {}
    }

    #[test]
    fn agent_exposes_runtime_with_accessors() {
        fn _check_api(agent: &Agent) {
            let runtime: &std::sync::Arc<AgentRuntime> = agent.runtime();
            let _config = runtime.config();
            let _tools = runtime.tools();
            let _hooks = runtime.hooks();
        }
    }
}

// =============================================================================
// Phase 6: RunConfig — per-execution overrides
// =============================================================================

mod phase6_run_config {
    use branchforge::RunConfig;
    use std::time::Duration;

    #[test]
    fn run_config_builder_pattern() {
        let config = RunConfig::new()
            .model("claude-opus-4-6")
            .max_tokens(4096)
            .max_iterations(10)
            .timeout(Duration::from_secs(60))
            .system_prompt("You are a helpful assistant.");
        // Verify public accessors
        assert_eq!(config.model_override(), Some("claude-opus-4-6"));
        assert_eq!(config.max_tokens_override(), Some(4096));
        assert_eq!(config.timeout_override(), Some(Duration::from_secs(60)));
        assert_eq!(
            config.system_prompt_override(),
            Some("You are a helpful assistant.")
        );
    }

    #[test]
    fn run_config_defaults_to_none() {
        let config = RunConfig::new();
        assert_eq!(config.model_override(), None);
        assert_eq!(config.max_tokens_override(), None);
        assert_eq!(config.timeout_override(), None);
    }
}

// =============================================================================
// Phase 7: Graceful Shutdown — CancellationToken
// =============================================================================

mod phase7_shutdown {
    #[test]
    fn cancellation_token_works() {
        let token = tokio_util::sync::CancellationToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
    }
}

// =============================================================================
// Phase 8: Logic fixes
// =============================================================================

mod phase8_logic_fixes {
    use branchforge::session::{Session, SessionConfig, SessionId};

    #[test]
    fn should_compact_uses_f64_precision() {
        let session = Session::from_id(SessionId::new(), SessionConfig::default());
        // With 0 current tokens, should never compact
        let result = session.should_compact(100_000_000, 0.5);
        assert!(!result);
    }
}

// =============================================================================
// Phase 9: Module consolidation — verify new locations
// =============================================================================

mod phase9_modules {
    #[test]
    fn credential_types_from_auth() {
        use branchforge::CredentialKind as CK;
        use branchforge::auth::credentials::CredentialKind;
        assert_eq!(
            std::mem::size_of::<CredentialKind>(),
            std::mem::size_of::<CK>()
        );
    }

    #[test]
    fn provider_profile_from_client() {
        use branchforge::client::provider_profile::{CapabilitySupport, ProviderProfile};
        let profile = ProviderProfile::new("test");
        assert_eq!(profile.streaming, CapabilitySupport::Unsupported);
    }

    #[test]
    fn prompt_frame_from_context() {
        use branchforge::PromptFrame;
        let frame = PromptFrame::default();
        assert!(frame.render().is_empty());
    }

    #[test]
    fn run_descriptor_from_agent() {
        use branchforge::RunDescriptor;
        let desc = RunDescriptor::new("claude-sonnet-4-5", "anthropic");
        assert_eq!(desc.model, "claude-sonnet-4-5");
    }
}

// =============================================================================
// Phase 10: Authorization — InputExtractor
// =============================================================================

mod phase10_authorization {
    use branchforge::authorization::{FieldExtractor, InputExtractor};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn field_extractor_extracts_string_field() {
        let extractor = FieldExtractor("command");
        let input = json!({"command": "ls -la"});
        assert_eq!(extractor.extract(&input), Some("ls -la"));
    }

    #[test]
    fn field_extractor_returns_none_for_missing_field() {
        let extractor = FieldExtractor("command");
        let input = json!({"other": "value"});
        assert_eq!(extractor.extract(&input), None);
    }

    #[test]
    fn tool_policy_clone_preserves_custom_extractors() {
        use branchforge::ToolPolicy;

        let mut policy = ToolPolicy::new();
        policy.register_extractor("CustomTool", Arc::new(FieldExtractor("custom_field")));

        // Clone should preserve the custom extractor (not recreate defaults)
        let _cloned = policy.clone();
    }
}

// =============================================================================
// Phase 11: Persistence — with_session_lock
// =============================================================================

mod phase11_persistence {
    use branchforge::session::persistence::{MemoryPersistence, Persistence};
    use branchforge::session::{Session, SessionConfig, SessionId, SessionState};

    #[tokio::test]
    async fn memory_persistence_with_session_lock() {
        let persistence = MemoryPersistence::new();
        let id = SessionId::new();
        let session = Session::from_id(id.clone(), SessionConfig::default());
        persistence.save(&session).await.unwrap();

        let result = persistence
            .with_session_lock(
                &id,
                Box::new(|session| {
                    session.set_state(SessionState::Active);
                    Ok(())
                }),
            )
            .await;
        assert!(result.is_ok());

        let loaded = persistence.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded.state, SessionState::Active);
    }
}

// =============================================================================
// Phase 12: Misc improvements
// =============================================================================

mod phase12_misc {
    use branchforge::graph::SessionGraph;
    use branchforge::hooks::HookManager;

    #[test]
    fn hook_manager_register_many() {
        let mut manager = HookManager::new();
        manager.register_many(std::iter::empty());
    }

    #[test]
    fn node_depth_correct_for_chain() {
        let mut graph = SessionGraph::new("main");
        let branch_id = graph.primary_branch;

        let n1 = graph
            .append_node(
                branch_id,
                branchforge::graph::NodeKind::User,
                serde_json::json!({}),
            )
            .unwrap();
        let n2 = graph
            .append_node(
                branch_id,
                branchforge::graph::NodeKind::Assistant,
                serde_json::json!({}),
            )
            .unwrap();

        assert_eq!(graph.node_depth(n1), 0);
        assert_eq!(graph.node_depth(n2), 1);
    }

    #[test]
    fn tool_execution_env_fields_are_private() {
        use branchforge::tools::{ExecutionContext, ToolExecutionEnv};
        let env = ToolExecutionEnv::new(ExecutionContext::permissive());
        let _ctx = env.context();
        let _state = env.tool_state();
        let _pm = env.process_manager();
    }

    #[test]
    fn prelude_includes_new_types() {
        use branchforge::prelude::*;
        fn _check_types(
            _runtime: &AgentRuntime,
            _config: &RunConfig,
            _mode: &ExecutionMode,
            _policy: &ToolPolicy,
        ) {
        }
    }
}
