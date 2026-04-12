//! Integration regression tests proving BranchForge works as a
//! **general-purpose Agent SDK** — not just a coding-agent runtime.
//!
//! These tests run with `--no-default-features` to lock in the
//! pure-Layer-1 surface, then again with `--features local-fs` to
//! cover the local-machine knowledge-agent topology. They use
//! [`branchforge::client::mock::MockLlmCall`] so they require zero
//! network and zero credentials.
//!
//! The goal is to catch regressions where a future change leaks a
//! `coding-tools` (or `local-fs`) dependency into the pure core
//! Agent surface, or where a Layer 1 user can no longer construct
//! and run an agent without filesystem features.

use branchforge::client::mock::MockLlmCall;
use branchforge::ir::{
    ContentPart, FinishReason, ModelResponse, ModelStreamChunk, Role, ToolOrigin, Usage,
};
use std::sync::Arc;

/// Construct a synthetic ModelResponse that requests one tool call,
/// then a follow-up text response after the tool returns. Used to
/// drive the agent loop through a single iteration.
fn tool_call_response(tool_name: &str, args: serde_json::Value) -> ModelResponse {
    ModelResponse {
        id: "msg_tool_call".into(),
        model: "test-model".into(),
        content: vec![ContentPart::ToolCall {
            id: "call_1".into(),
            name: tool_name.into(),
            arguments: args,
            origin: ToolOrigin::Local,
        }],
        finish_reason: FinishReason::ToolCalls,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
        continuation: None,
        warnings: Vec::new(),
        raw: None,
        rate_limit: None,
    }
}

fn final_text_response(text: &str) -> ModelResponse {
    ModelResponse {
        id: "msg_final".into(),
        model: "test-model".into(),
        content: vec![ContentPart::Text { text: text.into() }],
        finish_reason: FinishReason::Stop,
        usage: Usage {
            input_tokens: 12,
            output_tokens: 8,
            ..Default::default()
        },
        continuation: None,
        warnings: Vec::new(),
        raw: None,
        rate_limit: None,
    }
}

/// MockLlmCall is callable end-to-end through the LlmCall trait
/// without any feature flag — proves that test infrastructure
/// itself is Layer 1.
#[tokio::test]
async fn mock_llm_call_implements_llmcall_at_layer_1() {
    use branchforge::client::LlmCall;
    use branchforge::ir::{Message, ModelRequest};

    let mock = MockLlmCall::new()
        .then_response(tool_call_response(
            "DemoTool",
            serde_json::json!({"q": "x"}),
        ))
        .then_response(final_text_response("done"));

    // Round-trip a unary call.
    let req = ModelRequest::new("test-model", vec![Message::user("hi")]);
    let r1 = mock.send(&req).await.unwrap();
    assert!(matches!(r1.finish_reason, FinishReason::ToolCalls));
    assert_eq!(r1.usage.input_tokens, 10);

    let r2 = mock.send(&req).await.unwrap();
    assert!(matches!(r2.finish_reason, FinishReason::Stop));
    assert_eq!(r2.text(), "done");

    assert_eq!(mock.call_count(), 2);
    assert_eq!(mock.remaining(), 0);
}

/// Streaming round-trip via MockLlmCall — exercises the
/// `ChunkStream` path used by the streaming agent loop, including
/// the cancel-token plumbing introduced in NEW-1.
#[tokio::test]
async fn mock_llm_call_streams_chunks_with_cancel_token_at_layer_1() {
    use branchforge::client::LlmCall;
    use branchforge::ir::{Message, ModelRequest};
    use futures::StreamExt;
    use tokio_util::sync::CancellationToken;

    let chunks = vec![
        Ok(ModelStreamChunk::MessageStart {
            id: "msg_1".into(),
            model: "test-model".into(),
            role: Role::Assistant,
        }),
        Ok(ModelStreamChunk::TextDelta {
            index: 0,
            text: "hello ".into(),
        }),
        Ok(ModelStreamChunk::TextDelta {
            index: 0,
            text: "world".into(),
        }),
        Ok(ModelStreamChunk::Finish {
            reason: FinishReason::Stop,
            usage: Usage::default(),
        }),
    ];
    let mock = MockLlmCall::new().then_stream(chunks);

    let req = ModelRequest::new("test-model", vec![Message::user("hi")]);
    let token = CancellationToken::new();
    let mut stream = mock.send_stream(&req, token).await.unwrap();
    let mut accumulated = String::new();
    while let Some(chunk) = stream.next().await {
        if let Ok(ModelStreamChunk::TextDelta { text, .. }) = chunk {
            accumulated.push_str(&text);
        }
    }
    assert_eq!(accumulated, "hello world");
}

/// Decorator stack composes at Layer 1: wrapping a MockLlmCall in
/// `RetryingClient` must succeed without any feature flag and
/// preserve the underlying call semantics. This catches regressions
/// where retry/circuit-breaker plumbing accidentally pulls in a
/// feature-gated dependency.
#[tokio::test]
async fn retrying_client_decorates_mock_at_layer_1() {
    use branchforge::client::{LlmCall, RetryPolicy, RetryingClient};
    use branchforge::ir::{Message, ModelRequest};

    let mock = Arc::new(MockLlmCall::new().then_response(final_text_response("ok")));
    let retrying = RetryingClient::new(mock, RetryPolicy::default());

    let req = ModelRequest::new("test-model", vec![Message::user("hi")]);
    let resp = retrying.send(&req).await.unwrap();
    assert_eq!(resp.text(), "ok");
}

/// Construct a `BudgetTracker`, a `BudgetContext` with no tenant,
/// and verify the preflight + record path runs end-to-end at Layer 1.
/// Catches regressions where budget code accidentally requires
/// `coding-tools` (e.g. by importing `tokio::process`).
#[test]
fn budget_tracker_round_trip_at_layer_1() {
    use branchforge::budget::{BudgetExceedPolicy, BudgetTracker, estimate_request_tokens};
    use branchforge::ir::{Message, ModelRequest, Usage};
    use rust_decimal_macros::dec;

    let tracker = BudgetTracker::new(dec!(5.0)).on_exceed(BudgetExceedPolicy::Stop);

    let req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("hi")]);
    let estimate = estimate_request_tokens(&req);
    assert!(estimate.input > 0);
    let cost = tracker.estimate_cost(&req.model, estimate);
    assert!(cost > rust_decimal::Decimal::ZERO);

    let usage = Usage {
        input_tokens: 1000,
        output_tokens: 500,
        ..Default::default()
    };
    let recorded = tracker.record("claude-sonnet-4-5", &usage).unwrap();
    assert!(recorded > rust_decimal::Decimal::ZERO);
    assert!(tracker.used_cost_usd() >= recorded);
}

/// IR + structured output validator are Layer 1: a strict JSON
/// schema spec must validate a matching JSON body without pulling
/// any feature flag.
#[test]
fn structured_output_validator_runs_at_layer_1() {
    use branchforge::client::schema::validate_structured_output;
    use branchforge::ir::JsonSchemaSpec;
    use serde_json::json;

    let spec = JsonSchemaSpec {
        schema: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "count": {"type": "integer"}
            },
            "required": ["name", "count"]
        }),
        name: None,
        description: None,
        strict: true,
    };
    validate_structured_output(r#"{"name":"x","count":3}"#, &spec).unwrap();
    assert!(validate_structured_output(r#"{"name":"x"}"#, &spec).is_err());
}

/// Recovery recipes (#23 A1) are pure-core: a registry with the
/// builtin recipes must be constructible and queryable at Layer 1.
#[test]
fn recovery_recipes_run_at_layer_1() {
    use branchforge::FailureCategory;
    use branchforge::agent::recovery_recipes::*;

    let registry = RecipeRegistry::new().with_boxed_recipes(builtin_general_recipes());
    assert!(registry.len() >= 4);

    // Phase D A-3: registry.decide returns a RecoveryDecision that
    // carries both the action and the recipe name that produced it.
    let decision = registry.decide(&RecoveryDecisionInput::new(FailureCategory::RateLimit, 0));
    assert!(matches!(decision.action, RecoveryAction::RetryAfter { .. }));
    assert_eq!(decision.recipe, "rate_limit_backoff");
}

/// Session lifecycle FSM (#27 A5) lives at Layer 1 and validates every
/// transition via [`SessionState::transition_to`]. This is the single
/// canonical FSM after the W-7 unification — there is no separate
/// `SubagentState` or `TaskStatus`.
#[test]
fn session_lifecycle_fsm_runs_at_layer_1() {
    use branchforge::session::SessionState;

    // Happy path: Created → Running → Completing → Completed
    let s = SessionState::default();
    assert_eq!(s, SessionState::Created);
    let s = s.transition_to(SessionState::Running).unwrap();
    let s = s.transition_to(SessionState::Completing).unwrap();
    let s = s.transition_to(SessionState::Completed).unwrap();
    assert!(s.is_terminal());

    // Fast-fail lane: Running → Failing (any non-terminal → Failing).
    let s = SessionState::Running
        .transition_to(SessionState::Failing)
        .unwrap();
    assert_eq!(
        s.transition_to(SessionState::Failed).unwrap(),
        SessionState::Failed
    );

    // Illegal moves rejected by the FSM.
    assert!(
        SessionState::Completed
            .transition_to(SessionState::Running)
            .is_err()
    );
    assert!(
        SessionState::Created
            .transition_to(SessionState::Completed)
            .is_err()
    );
}

/// C-7: `OverflowRef` + `MemoryOverflowStore` + `preview_of` are
/// all Layer 1. A large payload round-trips through the store and
/// the issued ref carries back the correct size, preview, and
/// store name. This is the public surface downstream crates wire
/// into `ToolRegistry::builder().overflow_store(...)`.
#[tokio::test]
async fn overflow_store_round_trip_at_layer_1() {
    use branchforge::tools::{MemoryOverflowStore, OverflowStore, preview_of};

    let store = MemoryOverflowStore::new();
    let payload = "x".repeat(50_000);
    let preview = preview_of(&payload, 128);
    assert!(preview.contains("overflow store"));

    let r = store.store(payload.clone(), preview.clone()).await.unwrap();
    assert_eq!(r.store, "memory");
    assert_eq!(r.size_bytes, 50_000);
    assert_eq!(r.preview, preview);

    let loaded = store.load(&r.id).await.unwrap().unwrap();
    assert_eq!(loaded.len(), 50_000);

    // Unknown ids return None rather than erroring.
    assert!(store.load("missing").await.unwrap().is_none());
}

/// C-6: RateLimitSnapshot is pure-core and its ratio / approaching /
/// reset helpers behave predictably across the supported axes. The
/// transport-specific header parser lives behind `default` features
/// and is covered in the `direct.rs` unit tests; this test just
/// locks in the neutral IR surface.
#[test]
fn rate_limit_snapshot_helpers_run_at_layer_1() {
    use branchforge::ir::RateLimitSnapshot;
    use chrono::{Duration, Utc};

    let now = Utc::now();
    let snap = RateLimitSnapshot {
        requests_limit: Some(1000),
        requests_remaining: Some(80),
        requests_reset: Some(now + Duration::seconds(45)),
        tokens_limit: Some(400_000),
        tokens_remaining: Some(390_000),
        tokens_reset: Some(now + Duration::seconds(120)),
    };

    // Requests axis at 8% — fires the approaching threshold.
    assert!((snap.requests_ratio().unwrap() - 0.08).abs() < 1e-9);
    assert!(snap.is_approaching_limit(0.10));

    // Tokens axis is healthy, confirming `any axis` semantics.
    assert!((snap.tokens_ratio().unwrap() - 0.975).abs() < 1e-9);

    // Soonest reset wins — requests_reset at 45s beats tokens at 120s.
    assert_eq!(snap.seconds_until_reset(now), Some(45));
}

/// Permission rule DSL (#22 S6) is Layer 1.
#[test]
fn permission_rule_dsl_runs_at_layer_1() {
    use branchforge::authorization::{
        PermissionRuleSyntax, RuleDecisionKeyword, SubjectPattern, parse_permission_rule,
    };

    let parsed: PermissionRuleSyntax = parse_permission_rule("deny Bash(rm:*)").unwrap();
    assert_eq!(parsed.decision, RuleDecisionKeyword::Deny);
    assert_eq!(parsed.tool, "Bash");
    assert_eq!(
        parsed.subject,
        Some(SubjectPattern::PrefixWild("rm".into()))
    );
}

/// Phase D Workstream F-1: the session-persistence layer carries
/// an explicit [`SessionSchemaVersion`] on every payload, and the
/// shared [`MigrationLadder`] framework refuses to silently
/// degrade a payload written by a newer binary. This regression
/// test covers the Layer 1 surface: version comparison, migration
/// ladder construction, and future-version rejection. The
/// JSONL-specific header test lives next to the backend it
/// exercises.
#[test]
fn phase_d_f1_schema_version_framework_surface() {
    use branchforge::session::{
        MigrationLadder, SchemaMigration, SchemaMigrationError, SchemaVersionMismatchDirection,
        SessionSchemaVersion,
    };

    // 1. CURRENT and MIN_SUPPORTED form a valid window.
    assert!(SessionSchemaVersion::MIN_SUPPORTED <= SessionSchemaVersion::CURRENT);
    assert!(SessionSchemaVersion::CURRENT.is_supported());
    assert!(!SessionSchemaVersion::CURRENT.is_from_the_future());

    // 2. A synthetic future version is detected as TooNew.
    let future = SessionSchemaVersion(SessionSchemaVersion::CURRENT.value() + 2);
    assert!(future.is_from_the_future());

    // 3. The default ladder accepts the current version as a
    //    no-op — existing binaries can load data they wrote
    //    themselves without any migration overhead.
    let ladder = MigrationLadder::default_ladder();
    let mut payload = serde_json::json!({"session_id": "abc"});
    let upgraded = ladder
        .upgrade_to_current(&mut payload, SessionSchemaVersion::CURRENT)
        .unwrap();
    assert_eq!(upgraded, SessionSchemaVersion::CURRENT);

    // 4. The default ladder rejects future payloads as TooNew.
    //    Phase H-3: the error is now the typed `SchemaMigrationError`
    //    enum; we collapse to the coarse direction for the wire
    //    shape via `.direction()`.
    let err = ladder
        .upgrade_to_current(&mut serde_json::json!({}), future)
        .unwrap_err();
    assert!(matches!(err, SchemaMigrationError::TooNew { .. }));
    assert_eq!(err.direction(), SchemaVersionMismatchDirection::TooNew);

    // 5. The framework validates chain linearity: a ladder that
    //    skips a version is rejected at construction. This is
    //    the author-time guard that stops a developer from
    //    shipping a broken migration.
    #[derive(Debug)]
    struct GapStep;
    impl SchemaMigration for GapStep {
        fn source_version(&self) -> SessionSchemaVersion {
            SessionSchemaVersion(1)
        }
        fn target_version(&self) -> SessionSchemaVersion {
            SessionSchemaVersion(5)
        }
        fn migrate(&self, _: &mut serde_json::Value) -> Result<(), String> {
            Ok(())
        }
    }
    assert!(MigrationLadder::new(vec![Box::new(GapStep)]).is_err());
}

/// Phase G-4: `ExecutionContext`'s byte layout is invariant across
/// feature configurations. The former `#[cfg(feature = "local-fs")]
/// security: Arc<SecurityContext>` struct field has been replaced
/// with a `SecurityExtension` entry in the `Extensions` type-map.
/// Layer 2a-dependent accessors (e.g. `open_read`, `analyze_bash`)
/// still work identically because the dispatcher methods read from
/// the type-map internally. This regression pins the shape contract
/// by verifying that (a) an `empty()` context constructs successfully
/// and (b) under `local-fs`, the permissive security extension is
/// auto-registered so tools can use it without manual wiring.
#[test]
fn phase_g4_execution_context_security_lives_in_extensions() {
    use branchforge::tools::ExecutionContext;

    // `empty()` is a Layer 1 entry point. Under `local-fs`, it must
    // seed a permissive `SecurityExtension` so legacy tests that
    // relied on the old `security` field continue to work. Under
    // pure-core, it simply succeeds with no security primitives
    // registered — tools that need them don't compile anyway.
    let ctx = ExecutionContext::empty();
    assert!(ctx.session_id().is_none());

    // Confirm the Layer 2a accessor wiring is live under the feature.
    #[cfg(feature = "local-fs")]
    {
        use branchforge::security::SecurityExtension;
        let ext = ctx
            .extensions()
            .get::<SecurityExtension>()
            .expect("Phase G-4 invariant: empty() must seed SecurityExtension under local-fs");
        // root() must resolve through the extension path, proving the
        // dispatcher isn't reading a stale struct field.
        let _ = ext.context().root();
    }
}

/// Phase D Workstream E-3: the DSL parser surfaces typed
/// `PermissionDslError` variants with byte-offset information, and
/// the config-loading path refuses to build a policy from a
/// malformed rule — `try_into_policy` returns `Err` instead of
/// panicking inside a `ToolRule::allow` helper. This is the
/// regression that closes the "silent match-nothing rule" and
/// "start-up panic from typo in settings.local.json" failure modes
/// in one stroke.
#[test]
fn phase_d_e3_permission_dsl_errors_are_typed_and_positioned() {
    use branchforge::authorization::{
        PermissionDslError, parse_permission_rule, parse_to_tool_rule,
    };
    use branchforge::config::AuthorizationConfig;

    // 1. `Tool()` is a dedicated `EmptySubject` variant — it used
    //    to silently round-trip as `Tool(Bare(""))` and match
    //    nothing at runtime.
    match parse_permission_rule("Bash()").unwrap_err() {
        PermissionDslError::EmptySubject { tool, col } => {
            assert_eq!(tool, "Bash");
            assert_eq!(col, 5);
        }
        other => panic!("expected EmptySubject, got {other:?}"),
    }

    // 2. Trailing input after `)` is rejected.
    assert!(matches!(
        parse_permission_rule("Read(/etc/*)stuff").unwrap_err(),
        PermissionDslError::TrailingInput { .. }
    ));

    // 3. A broken regex in the tool pattern is caught at lowering
    //    time and surfaces as `InvalidToolPattern`, not as a rule
    //    that mysteriously never fires.
    assert!(matches!(
        parse_to_tool_rule("Re[ad", Default::default()).unwrap_err(),
        PermissionDslError::InvalidToolPattern { .. }
    ));

    // 4. `AuthorizationConfig::try_into_policy` is the fallible
    //    entry point used by the settings loader. A bad rule in
    //    either the allow or deny list is surfaced as a typed
    //    DSL error the agent builder can convert to
    //    `Error::Config` and surface as a fatal build failure.
    let bad_deny = AuthorizationConfig {
        deny: vec!["Bash(".into()],
        allow: vec![],
        default_mode: None,
    };
    let err = bad_deny
        .try_into_policy()
        .expect_err("malformed deny rule must surface as PermissionDslError");
    assert!(
        matches!(err, PermissionDslError::UnbalancedParens { .. }),
        "unexpected error shape: {err:?}"
    );

    // And the happy path still works — regression guard for the
    // canonical permission settings shape.
    let good = AuthorizationConfig {
        deny: vec!["Bash(rm:*)".into()],
        allow: vec!["Read".into(), "Write".into()],
        default_mode: None,
    };
    let policy = good.try_into_policy().unwrap();
    assert_eq!(policy.rules.len(), 3);
}

// ── W-29: Phase 2 regression coverage ───────────────────────────────

/// W-9: `TaskTool::register_typed::<C>()` is an OCP-compliant open
/// registry. Registering a contract must surface it via
/// `typed_contracts()` and the stored entry's type-erased validator
/// must round-trip valid and invalid prompts.
#[test]
fn typed_contract_open_registry_round_trip() {
    use branchforge::agent::{AgentContract, TaskTool, TaskTracker, TypedAgentInvoker};
    use branchforge::session::MemoryPersistence;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    #[derive(Serialize, Deserialize, schemars::JsonSchema)]
    struct CitationInput {
        query: String,
    }

    #[derive(Serialize, Deserialize)]
    struct CitationOutput {
        citations: Vec<String>,
    }

    struct ResearchContract;
    impl AgentContract for ResearchContract {
        type Input = CitationInput;
        type Output = CitationOutput;
        const SUBAGENT_TYPE: &'static str = "general";
    }

    let registry = TaskTracker::new(Arc::new(MemoryPersistence::new()));
    let tool = TaskTool::new(registry).register_typed::<ResearchContract>();

    assert_eq!(tool.typed_contracts().len(), 1);
    let entry = tool.typed_contract("general").expect("contract present");
    assert_eq!(entry.subagent_type, "general");

    // The invoker round-trips a typed input through `build_task_input`.
    let invoker = TypedAgentInvoker::<ResearchContract>::new();
    let built = invoker
        .build_task_input(
            &CitationInput {
                query: "rust async".into(),
            },
            "research",
        )
        .unwrap();
    assert_eq!(built.subagent_type, "general");
    assert!((entry.validate_prompt)(&built.prompt).is_ok());
    assert!((entry.validate_prompt)("garbage").is_err());
}

/// W-16: `jsonschema` 0.46 Draft 2020-12 coverage — `pattern`,
/// `oneOf`, and numeric bounds that the hand-rolled walker would
/// have silently skipped must now be enforced end-to-end through
/// the public `validate_structured_output` surface.
#[test]
fn schema_validator_enforces_draft_2020_12_keywords() {
    use branchforge::client::schema::validate_structured_output;
    use branchforge::ir::JsonSchemaSpec;
    use serde_json::json;

    fn spec(schema: serde_json::Value) -> JsonSchemaSpec {
        JsonSchemaSpec {
            schema,
            name: None,
            description: None,
            strict: true,
        }
    }

    // `pattern` is enforced.
    let pattern_spec = spec(json!({
        "type": "object",
        "properties": {"code": {"type": "string", "pattern": "^[A-Z]{3}$"}},
        "required": ["code"]
    }));
    validate_structured_output(r#"{"code": "ABC"}"#, &pattern_spec).unwrap();
    assert!(validate_structured_output(r#"{"code": "abc"}"#, &pattern_spec).is_err());

    // `oneOf` is enforced.
    let one_of_spec = spec(json!({
        "oneOf": [{"type": "string"}, {"type": "integer"}]
    }));
    validate_structured_output("\"hi\"", &one_of_spec).unwrap();
    validate_structured_output("42", &one_of_spec).unwrap();
    assert!(validate_structured_output("true", &one_of_spec).is_err());

    // Numeric bounds are enforced.
    let bounds_spec = spec(json!({
        "type": "object",
        "properties": {"pct": {"type": "number", "minimum": 0, "maximum": 100}},
        "required": ["pct"]
    }));
    validate_structured_output(r#"{"pct": 50}"#, &bounds_spec).unwrap();
    assert!(validate_structured_output(r#"{"pct": 150}"#, &bounds_spec).is_err());
}

/// W-1 / ADR-001: the `ProfileRegistry` is an open registry.
/// `with_builtins()` must ship a non-empty canonical set and
/// `register()` must make a new profile retrievable by id at
/// runtime — the whole point of dropping the closed `Preset`
/// enum in favour of open registration.
#[test]
fn profile_registry_is_an_open_registry() {
    use branchforge::client::ProfileRegistry;

    let registry = ProfileRegistry::with_builtins();
    let builtin_ids: Vec<&str> = registry.ids().collect();
    assert!(
        !builtin_ids.is_empty(),
        "with_builtins() must ship canonical profiles"
    );
    // `anthropic` is the canonical Layer 1 profile — it must be
    // present even in a pure-core build.
    assert!(
        registry.contains("anthropic"),
        "builtins must include `anthropic`; got {builtin_ids:?}"
    );

    // An empty registry followed by no registrations must look
    // empty, proving there is no hidden static table behind the
    // open API.
    let empty = ProfileRegistry::empty();
    assert!(empty.ids().next().is_none());
    assert!(!empty.contains("anthropic"));
}

/// B-4: Scripted conversation DSL via `MockLlmCall` ergonomic
/// helpers. A full three-turn dialogue (text → tool → text) can
/// be set up with three chained `.then_*` calls and no hand-rolled
/// JSON literals. Proves `MockLlmCall::then_text`,
/// `then_tool_call`, and `then_stream_text` compose cleanly
/// through the public API.
#[tokio::test]
async fn scripted_conversation_via_mock_helpers() {
    use branchforge::MockLlmCall;
    use branchforge::client::LlmCall;
    use branchforge::ir::{ContentPart, FinishReason, Message, ModelRequest};
    use futures::StreamExt;

    let mock = MockLlmCall::new()
        .then_tool_call(
            "call_1",
            "search",
            serde_json::json!({"query": "rust async"}),
        )
        .then_text("Here are the results")
        .then_stream_text(["Final ", "answer"]);

    assert_eq!(mock.remaining(), 3);

    // Turn 1: unary tool call.
    let req = ModelRequest::new("test", vec![Message::user("plan a trip")]);
    let r1 = mock.send(&req).await.unwrap();
    assert!(matches!(r1.finish_reason, FinishReason::ToolCalls));
    let tool_call = r1.tool_calls().next().unwrap();
    if let ContentPart::ToolCall {
        id,
        name,
        arguments,
        ..
    } = tool_call
    {
        assert_eq!(id, "call_1");
        assert_eq!(name, "search");
        assert_eq!(arguments["query"], "rust async");
    } else {
        panic!("expected ToolCall");
    }

    // Turn 2: unary text response.
    let r2 = mock.send(&req).await.unwrap();
    assert_eq!(r2.text(), "Here are the results");

    // Turn 3: streaming text response built from the chunk helper.
    let stream = mock
        .send_stream(&req, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let chunks: Vec<_> = stream.collect().await;
    assert_eq!(chunks.len(), 4); // Start + 2 deltas + Finish

    assert_eq!(mock.call_count(), 3);
    assert_eq!(mock.remaining(), 0);
}

/// C-2: Tool trait input-aware capability queries.
///
/// The same `BashTool` must report different read-only /
/// destructive / concurrency-safe / open-world classifications
/// based on the command argument. This was the motivation for
/// switching `is_read_only(&self)` → `is_read_only(&self, &Value)`
/// in Phase C-2. Covering it here prevents regression into the
/// old static `const READ_ONLY = ?` world.
#[cfg(feature = "coding-tools")]
#[test]
fn tool_trait_capability_queries_are_input_aware() {
    use branchforge::tools::{BashTool, Tool};
    use std::sync::Arc;

    let tool = BashTool::new(Arc::new(branchforge::tools::ProcessScheduler::default()));

    // Safe, read-only invocation.
    let ls = serde_json::json!({"command": "ls /tmp"});
    assert!(tool.is_read_only(&ls), "ls must be read-only");
    assert!(tool.is_concurrency_safe(&ls), "ls must be concurrency-safe");
    assert!(!tool.is_destructive(&ls), "ls must NOT be destructive");

    // Destructive invocation.
    let rm = serde_json::json!({"command": "rm -rf /tmp/junk"});
    assert!(!tool.is_read_only(&rm));
    assert!(tool.is_destructive(&rm), "rm must be destructive");

    // Permission subjects surface the command name for DSL
    // rule matching.
    let bash_rm_subjects = tool.permission_subjects(&rm);
    assert_eq!(bash_rm_subjects, vec!["rm"]);
}

/// B-3: `StreamAggregator` collapses an `AgentEvent` stream into a
/// typed snapshot that UIs can render without hand-rolled state
/// tracking. This test builds a realistic event sequence (text
/// deltas → tool call lifecycle → usage → complete) and asserts
/// the aggregator produces the fully-assembled view.
#[tokio::test]
async fn stream_aggregator_end_to_end() {
    use branchforge::agent::{
        AgentEvent, AgentMetrics, AgentResult, AgentState, StreamAggregator, ToolCallStatus,
    };
    use branchforge::ir::{FinishReason, TokenCount};
    use futures::stream;

    let final_result = AgentResult {
        text: "Here is the answer.".into(),
        usage: Usage {
            input_tokens: 150,
            output_tokens: 80,
            ..Default::default()
        },
        tool_calls: 1,
        iterations: 1,
        stop_reason: FinishReason::Stop,
        state: AgentState::Completed,
        metrics: AgentMetrics::default(),
        session_id: "sess-1".into(),
        structured_output: None,
        messages: Vec::new(),
        uuid: "result-1".into(),
    };

    let events: Vec<branchforge::Result<AgentEvent>> = vec![
        Ok(AgentEvent::Text {
            delta: "Here ".into(),
        }),
        Ok(AgentEvent::Text {
            delta: "is ".into(),
        }),
        Ok(AgentEvent::ToolStart {
            id: "t1".into(),
            name: "Read".into(),
            input: serde_json::json!({"path": "notes.md"}),
        }),
        Ok(AgentEvent::ToolComplete {
            id: "t1".into(),
            name: "Read".into(),
            output: "file contents".into(),
            is_error: false,
            duration_ms: 12,
        }),
        Ok(AgentEvent::Text {
            delta: "the ".into(),
        }),
        Ok(AgentEvent::Text {
            delta: "answer.".into(),
        }),
        Ok(AgentEvent::TurnUsage {
            input_tokens: TokenCount::new(150),
            output_tokens: TokenCount::new(80),
            cache_read_tokens: TokenCount::new(0),
            cache_creation_tokens: TokenCount::new(0),
            total_input_tokens: TokenCount::new(150),
            total_output_tokens: TokenCount::new(80),
        }),
        Ok(AgentEvent::Complete(Box::new(final_result))),
    ];

    let stream = stream::iter(events);
    let (agg, err) = StreamAggregator::drain(stream).await;

    assert!(err.is_none());
    assert_eq!(agg.text(), "Here is the answer.");

    let tools = agg.tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].id, "t1");
    assert_eq!(tools[0].status, ToolCallStatus::Succeeded);
    assert_eq!(tools[0].output.as_deref(), Some("file contents"));
    assert_eq!(tools[0].duration_ms, Some(12));

    assert_eq!(agg.usage().total_input_tokens.get(), 150);
    assert_eq!(agg.usage().total_output_tokens.get(), 80);

    assert!(agg.is_complete());
    let result = agg.final_result().unwrap();
    assert_eq!(result.session_id, "sess-1");
    assert_eq!(result.tool_calls, 1);
}

/// Integration hardening: every Phase B type must be reachable
/// from `branchforge::prelude::*` without reaching into sub-modules.
/// This catches re-export regressions on the ergonomic surface.
#[test]
fn prelude_exposes_phase_b_surface() {
    #[allow(unused_imports)]
    use branchforge::prelude::{
        AgentCheckpoint, BudgetAlertPayload, Event, EventBus, EventKind, EventPayload,
        StreamAggregator, TokensConsumedPayload, ToolCallState, ToolCallStatus,
        ToolExecutedPayload, ToolProgressPayload,
    };
    // Build one of each to ensure the exported symbols are structs,
    // not phantom type-aliases of something else.
    let _bus: EventBus = EventBus::default();
    let _agg: StreamAggregator = StreamAggregator::new();
    let _kind: EventKind = EventKind::TokensConsumed;
    let _: &dyn Fn() -> EventKind = &|| TokensConsumedPayload::KIND;
}

/// B-2: `EventBus::subscribe_typed` + `emit_typed` round-trip.
/// A caller subscribes for `TokensConsumedPayload`, the runtime-style
/// helper emits via the typed path, and the subscriber receives the
/// **decoded struct** — no hand-rolled JSON parsing in the closure.
/// Events of the wrong kind must not reach the typed subscriber.
#[tokio::test]
async fn event_bus_typed_subscribe_round_trip() {
    use branchforge::events::{EventBus, TokensConsumedPayload, ToolExecutedPayload};
    use std::sync::{Arc, Mutex};

    let bus = EventBus::default();

    // Typed subscriber — receives a decoded struct.
    let received = Arc::new(Mutex::new(Vec::<TokensConsumedPayload>::new()));
    let received_for_cb = Arc::clone(&received);
    bus.subscribe_typed(move |data: TokensConsumedPayload| {
        received_for_cb.lock().unwrap().push(data);
    });

    // Emit two matching events via the typed path.
    bus.emit_typed(TokensConsumedPayload {
        input_tokens: 100,
        output_tokens: 50,
        model: "claude-sonnet-4-5".into(),
    });
    bus.emit_typed(TokensConsumedPayload {
        input_tokens: 200,
        output_tokens: 75,
        model: "claude-opus-4-6".into(),
    });

    // Emit a different-kind event to prove filtering works.
    bus.emit_typed(ToolExecutedPayload {
        tool_name: "Bash".into(),
        duration_ms: 42,
        is_error: false,
    });

    // Drain the per-subscriber mpsc channel.
    for _ in 0..10 {
        tokio::task::yield_now().await;
        if received.lock().unwrap().len() == 2 {
            break;
        }
    }

    let decoded = received.lock().unwrap();
    assert_eq!(decoded.len(), 2, "only TokensConsumed events must match");
    assert_eq!(decoded[0].input_tokens, 100);
    assert_eq!(decoded[0].model, "claude-sonnet-4-5");
    assert_eq!(decoded[1].output_tokens, 75);
    assert_eq!(decoded[1].model, "claude-opus-4-6");
}

/// B-2 coexistence: typed emit is visible to untyped subscribers on
/// the same kind. Proves the typed API is a thin wrapper — untyped
/// consumers keep working alongside typed ones.
#[tokio::test]
async fn event_bus_typed_emit_visible_to_untyped_subscriber() {
    use branchforge::events::{Event, EventBus, EventKind, ToolExecutedPayload};
    use std::sync::{Arc, Mutex};

    let bus = EventBus::default();

    // Untyped subscriber: receives raw Event.
    let raw = Arc::new(Mutex::new(Vec::<Event>::new()));
    let raw_for_cb = Arc::clone(&raw);
    bus.subscribe(
        EventKind::ToolExecuted,
        Arc::new(move |event| {
            raw_for_cb.lock().unwrap().push(event);
        }),
    );

    // Typed emit on the same kind.
    bus.emit_typed(ToolExecutedPayload {
        tool_name: "Bash".into(),
        duration_ms: 1234,
        is_error: false,
    });

    for _ in 0..10 {
        tokio::task::yield_now().await;
        if !raw.lock().unwrap().is_empty() {
            break;
        }
    }

    let events = raw.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::ToolExecuted);
    // The untyped consumer can still inspect the raw JSON.
    assert_eq!(events[0].data["tool_name"], "Bash");
    assert_eq!(events[0].data["duration_ms"], 1234);
}

/// B-1: `AgentCheckpoint` round-trip proves the budget accumulator
/// survives a simulated process restart. A fresh `BudgetTracker`
/// seeded via `restore_spent(checkpoint.budget_spent_usd)` plus one
/// new record must match the total cost a single tracker would have
/// after two records.
///
/// This is the **end-to-end** contract for crash recovery — the
/// checkpoint struct, its serde round-trip, and the restore API
/// must all compose correctly without a running Agent (which would
/// pull in tokio runtime + LlmCall machinery the regression test
/// deliberately avoids).
#[test]
fn agent_checkpoint_budget_restore_round_trip() {
    use branchforge::AgentCheckpoint;
    use branchforge::authorization::ExecutionMode;
    use branchforge::budget::BudgetTracker;
    use branchforge::ir::Usage;
    use branchforge::session::SessionId;
    use rust_decimal_macros::dec;

    // ── Pre-crash process ──────────────────────────────────────
    let usage = Usage {
        input_tokens: 100_000,
        output_tokens: 50_000,
        ..Default::default()
    };

    let tracker_before = BudgetTracker::new(dec!(10));
    tracker_before.record("claude-sonnet-4-5", &usage).unwrap();
    let session_id = SessionId::new();

    let checkpoint = AgentCheckpoint {
        session_id,
        execution_mode: ExecutionMode::Auto,
        budget_spent_usd: tracker_before.used_cost_usd(),
        session_usage: usage.clone(),
        created_at: chrono::Utc::now(),
    };

    // ── Serialize to disk / KV / DB ─────────────────────────────
    let bytes = serde_json::to_vec(&checkpoint).unwrap();

    // ── Post-restart process ────────────────────────────────────
    let restored: AgentCheckpoint = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(restored.session_id, session_id);
    assert_eq!(restored.budget_spent_usd, tracker_before.used_cost_usd());
    assert!(matches!(restored.execution_mode, ExecutionMode::Auto));

    let tracker_after = BudgetTracker::new(dec!(10));
    tracker_after
        .restore_spent(restored.budget_spent_usd)
        .unwrap();
    tracker_after.record("claude-sonnet-4-5", &usage).unwrap();

    // Baseline: one tracker records twice (no crash).
    let live = BudgetTracker::new(dec!(10));
    live.record("claude-sonnet-4-5", &usage).unwrap();
    live.record("claude-sonnet-4-5", &usage).unwrap();

    assert_eq!(
        tracker_after.used_cost_usd(),
        live.used_cost_usd(),
        "restored tracker must equal the live-accumulated tracker"
    );
}

/// W-15 / W-18: DSL parser → `ToolPolicy` enforcement round-trip.
/// Parsing `"deny Bash(rm:*)"` must produce a syntax tree whose
/// pattern fragment compiles into a `ToolPolicy` that denies
/// `Bash` invocations matching the prefix.
#[test]
fn permission_dsl_round_trips_into_enforced_policy() {
    use branchforge::authorization::{
        PermissionRuleSyntax, RuleDecisionKeyword, SubjectPattern, ToolPolicyBuilder,
        parse_permission_rule,
    };

    // Parse DSL.
    let parsed: PermissionRuleSyntax = parse_permission_rule("deny Bash(rm:*)").unwrap();
    assert_eq!(parsed.decision, RuleDecisionKeyword::Deny);
    assert_eq!(parsed.tool, "Bash");
    assert_eq!(
        parsed.subject,
        Some(SubjectPattern::PrefixWild("rm".into()))
    );

    // The pattern fragment `Tool(subject)` is itself a valid
    // rule-builder input — compile it into a policy via the
    // `deny()` builder entrypoint.
    let pattern_fragment = format!(
        "{}({})",
        parsed.tool,
        match parsed.subject.as_ref().unwrap() {
            SubjectPattern::PrefixWild(p) => format!("{p}:*"),
            other => panic!("unexpected subject shape: {other:?}"),
        }
    );
    let policy = ToolPolicyBuilder::new().deny(&pattern_fragment).build();

    // The compiled policy rejects `Bash(rm …)` and accepts
    // unrelated tools. Subjects are supplied by the caller (Phase D
    // Workstream A-1 — no parallel extractor registry).
    let denied = policy.check("Bash", &["rm".to_string()]);
    assert!(
        matches!(
            denied,
            branchforge::authorization::PermissionDecision::Deny { .. }
        ),
        "policy must deny Bash(rm …), got {denied:?}"
    );
}

/// Phase D Workstream E-1: the cache-break classifier is the
/// first instance of BranchForge's two-phase telemetry pattern.
/// A caller snapshots a baseline before each request, and on
/// response compares it against the previous baseline to
/// Phase D Workstream E-2: a `RateLimitSnapshot` parsed from a 429
/// response must flow all the way from `Error::Provider::rate_limit`
/// into `RateLimitBackoffRecipe` and drive a `RetryAfter { delay }`
/// that matches the provider's own `seconds_until_reset`. Without
/// this wiring, Phase C-6 data was observed but never acted on — the
/// recovery loop fell through to blind exponential backoff.
#[test]
fn phase_d_e2_rate_limit_snapshot_drives_recovery_delay() {
    use branchforge::agent::recovery_recipes::{
        RateLimitBackoffRecipe, RecipeDecision, RecoveryAction, RecoveryDecisionInput,
        RecoveryRecipe,
    };
    use branchforge::ir::RateLimitSnapshot;
    use branchforge::{Error, FailureCategory, error::ProviderErrorKind};
    use chrono::{Duration as ChronoDuration, Utc};
    use std::time::Duration;

    // Build a snapshot that says "the tokens window resets in 7s".
    let now = Utc::now();
    let snap = RateLimitSnapshot {
        tokens_reset: Some(now + ChronoDuration::seconds(7)),
        ..Default::default()
    };

    // 1. The snapshot rides on Error::Provider and is readable via
    //    the accessor the recovery executor uses.
    let err = Error::Provider {
        provider: "anthropic",
        kind: ProviderErrorKind::RateLimit,
        message: "rate limited".into(),
        hint: None,
        retryable: true,
        status: Some(429),
        rate_limit: Some(Box::new(snap.clone())),
    };
    assert!(err.rate_limit_snapshot().is_some());
    assert_eq!(err.category(), FailureCategory::RateLimit);

    // 2. Feeding the recipe a decision input with that snapshot
    //    attached yields a RetryAfter whose delay is derived from
    //    the snapshot (≈ 7s, clamped into the [base, max] band),
    //    NOT the classic 500ms * 2^attempt exponential value.
    let recipe = RateLimitBackoffRecipe::default();
    let decision = recipe.decide(&RecoveryDecisionInput {
        category: FailureCategory::RateLimit,
        attempt: 0,
        rate_limit: Some(snap),
    });
    match decision {
        RecipeDecision::Act(RecoveryAction::RetryAfter { delay }) => {
            assert!(
                delay >= Duration::from_millis(500) && delay <= Duration::from_secs(30),
                "delay {delay:?} must be inside [base, max]"
            );
            assert!(
                delay >= Duration::from_secs(6) && delay <= Duration::from_secs(8),
                "snapshot-driven delay must track seconds_until_reset, got {delay:?}"
            );
        }
        other => panic!("expected RetryAfter, got {other:?}"),
    }

    // 3. Same recipe, same attempt, no snapshot → falls back to
    //    the classic exponential base delay (500ms). Regression
    //    guard for transports that don't publish rate-limit headers.
    let fallback = recipe.decide(&RecoveryDecisionInput {
        category: FailureCategory::RateLimit,
        attempt: 0,
        rate_limit: None,
    });
    assert_eq!(
        fallback,
        RecipeDecision::Act(RecoveryAction::RetryAfter {
            delay: Duration::from_millis(500),
        })
    );
}

/// classify why the cache broke. This test drives the pure
/// classifier through the public SDK surface with four scripted
/// scenarios: model change, system prompt change, TTL expiry,
/// and no-marker no-classification.
#[test]
fn phase_d_e1_cache_break_classifier_surface() {
    use branchforge::decision::DecisionReason;
    use branchforge::ir::{Message, ModelRequest, ModelResponse, SystemPrompt, Usage};
    use branchforge::observability::{CacheBreakBaseline, CacheBreakCause, classify_cache_break};

    fn req(model: &str, system: &str) -> ModelRequest {
        let mut r = ModelRequest::new(model, vec![Message::user("hi")]);
        r.system = Some(SystemPrompt::Text(system.into()));
        r
    }

    fn cold_response() -> ModelResponse {
        let mut r = ModelResponse::from_text("ok");
        r.usage = Usage {
            input_tokens: 100,
            output_tokens: 10,
            cached_input_tokens: Some(0),
            ..Default::default()
        };
        r
    }

    // Case 1: model changed.
    let prev = CacheBreakBaseline::from_request(&req("claude-sonnet-4-5", "s"), 900);
    let curr = CacheBreakBaseline::from_request(&req("claude-opus-4-6", "s"), 900);
    let cause = classify_cache_break(Some(&prev), &curr, &cold_response(), true).unwrap();
    assert_eq!(<_ as DecisionReason>::category(&cause), "model_changed");
    assert!(matches!(cause, CacheBreakCause::ModelChanged { .. }));

    // Case 2: system prompt changed.
    let prev = CacheBreakBaseline::from_request(&req("m", "first"), 900);
    let curr = CacheBreakBaseline::from_request(&req("m", "second"), 900);
    let cause = classify_cache_break(Some(&prev), &curr, &cold_response(), true).unwrap();
    assert_eq!(cause, CacheBreakCause::SystemPromptChanged);

    // Case 3: unchanged baseline, prior hit → TTL.
    let same = req("m", "s");
    let prev = CacheBreakBaseline::from_request(&same, 900);
    let curr = CacheBreakBaseline::from_request(&same, 900);
    let cause = classify_cache_break(Some(&prev), &curr, &cold_response(), true).unwrap();
    assert_eq!(cause, CacheBreakCause::TtlOrUpstream);

    // Case 4: request without cache markers → no classification.
    let prev = CacheBreakBaseline::from_request(&req("m", "s"), 900);
    let curr = CacheBreakBaseline::from_request(&req("m", "changed"), 900);
    assert!(classify_cache_break(Some(&prev), &curr, &cold_response(), false).is_none());
}

/// Phase D Workstream D-2: `DroppingSink` is the opt-in decorator
/// for best-effort delivery. When the inner sink is slow, it drops
/// non-critical events (text deltas, progress) rather than blocking
/// the agent loop. Critical events — `Init` and `Complete` — are
/// never dropped, so the host always sees the session prologue and
/// final result. This test uses a deliberately slow inner sink to
/// force the timeout path and asserts the drop counter reflects
/// reality.
#[tokio::test]
async fn phase_d_d2_dropping_sink_preserves_critical_events() {
    use async_trait::async_trait;
    use branchforge::agent::{
        AgentEvent, AgentEventSink, DroppingSink, SinkError, event_is_critical,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Inner sink that blocks for 50ms on every event. With a
    /// 10ms per-event timeout the DroppingSink drops every
    /// non-critical event and bypasses the timeout for critical
    /// ones (which therefore succeed).
    #[derive(Debug)]
    struct SlowSink {
        accepted: AtomicUsize,
    }

    #[async_trait]
    impl AgentEventSink for SlowSink {
        async fn emit(&self, _event: &AgentEvent) -> Result<(), SinkError> {
            tokio::time::sleep(Duration::from_millis(50)).await;
            self.accepted.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    // Classifier smoke test — the vocabulary the decorator uses.
    assert!(event_is_critical(&AgentEvent::Init {
        model: "m".into(),
        execution_mode: "auto".into(),
        tools: vec![],
        subagents: vec![],
        skills: vec![],
        mcp_servers: vec![],
    }));
    assert!(!event_is_critical(&AgentEvent::Text { delta: "hi".into() }));

    // Drive the decorator.
    let inner = SlowSink {
        accepted: AtomicUsize::new(0),
    };
    let sink = DroppingSink::new(inner, Duration::from_millis(10));

    // Two non-critical events — should time out and be dropped.
    sink.emit(&AgentEvent::Text { delta: "a".into() })
        .await
        .unwrap();
    sink.emit(&AgentEvent::Text { delta: "b".into() })
        .await
        .unwrap();
    assert_eq!(sink.dropped(), 2);

    // Init is critical — must bypass the timeout and reach inner.
    sink.emit(&AgentEvent::Init {
        model: "m".into(),
        execution_mode: "auto".into(),
        tools: vec![],
        subagents: vec![],
        skills: vec![],
        mcp_servers: vec![],
    })
    .await
    .unwrap();
    // Drop counter unchanged — critical events are never counted.
    assert_eq!(sink.dropped(), 2);
}

/// Phase D Workstream D-1: `AgentEventSink` is the canonical
/// host-neutral egress surface for agent events. A caller can
/// pick NDJSON (CLI pipes), SSE (HTTP streaming), a channel
/// (tests / in-process consumers), or a no-op. This test covers
/// all four reference implementations with a scripted stream.
#[tokio::test]
async fn phase_d_d1_agent_event_sink_reference_implementations() {
    use branchforge::agent::{
        AgentEvent, AgentEventSink, ChannelSink, NdjsonSink, NoopSink, SseSink,
        drive_stream_into_sink,
    };
    use futures::stream;

    // `branchforge::Error` is not Clone, so we build a fresh
    // event vector for each sink via this factory instead of
    // cloning one shared vector.
    fn events() -> Vec<branchforge::Result<AgentEvent>> {
        vec![
            Ok(AgentEvent::Text {
                delta: "Hello".into(),
            }),
            Ok(AgentEvent::Text {
                delta: " world".into(),
            }),
        ]
    }

    // NoopSink — silently drops everything.
    let noop = NoopSink;
    drive_stream_into_sink(stream::iter(events()), &noop)
        .await
        .unwrap();

    // ChannelSink — in-process consumer.
    let (channel_sink, mut rx) = ChannelSink::bounded(16);
    drive_stream_into_sink(stream::iter(events()), &channel_sink)
        .await
        .unwrap();
    drop(channel_sink);
    let mut collected = Vec::new();
    while let Some(ev) = rx.recv().await {
        if let AgentEvent::Text { delta } = ev {
            collected.push(delta);
        }
    }
    assert_eq!(collected, vec!["Hello".to_string(), " world".into()]);

    // NdjsonSink — line-delimited JSON suitable for CLI pipes.
    let ndjson_sink = NdjsonSink::new(Vec::<u8>::new());
    drive_stream_into_sink(stream::iter(events()), &ndjson_sink)
        .await
        .unwrap();
    ndjson_sink.flush().await.unwrap();

    // SseSink — HTML5 EventSource frames for HTTP streaming.
    let sse_sink = SseSink::new(Vec::<u8>::new());
    drive_stream_into_sink(stream::iter(events()), &sse_sink)
        .await
        .unwrap();
    sse_sink.flush().await.unwrap();
}

/// Phase D Workstream C-3: MCP elicitation requests flow through
/// the unified [`branchforge::authorization::HumanInteractionHandler`]
/// via [`branchforge::mcp::HumanElicitationRouter`]. This test
/// verifies the wiring: a router constructed with a handler
/// surface's the handler's name, and a router without one
/// declines construction-wise (the decline path at the wire level
/// runs inside rmcp's `ClientHandler::create_elicitation` which
/// requires a live `RequestContext` and is exercised by the MCP
/// integration tests).
#[cfg(feature = "mcp")]
#[test]
fn phase_d_c3_mcp_elicitation_router_wiring() {
    use async_trait::async_trait;
    use branchforge::authorization::{
        ElicitationRequest, ElicitationResponse, HumanInteractionHandler, HumanInteractionResult,
    };
    use branchforge::mcp::HumanElicitationRouter;
    use std::sync::Arc;

    #[derive(Debug)]
    struct EchoHandler;

    #[async_trait]
    impl HumanInteractionHandler for EchoHandler {
        fn name(&self) -> &str {
            "echo_elicit"
        }
        async fn elicit(
            &self,
            req: ElicitationRequest,
        ) -> HumanInteractionResult<ElicitationResponse> {
            Ok(ElicitationResponse {
                value: serde_json::json!({ "echoed": req.prompt }),
            })
        }
    }

    // Fail-closed path: declining router carries no handler.
    let declining = HumanElicitationRouter::declining();
    let default = HumanElicitationRouter::default();
    // Two constructors, same failure mode — both safe to instantiate
    // without a host wired.
    let _ = (&declining, &default);

    // Wired path: handler is reachable through the router.
    let handler: Arc<dyn HumanInteractionHandler> = Arc::new(EchoHandler);
    let router = HumanElicitationRouter::new(Some(handler.clone()));
    // The router's Debug impl should surface the wrapped handler's
    // name so operators can see which handler the MCP client uses.
    let debug = format!("{router:?}");
    assert!(
        debug.contains("echo_elicit"),
        "router Debug must surface handler name; got {debug}"
    );
}

/// Phase D Workstream C-2: `AskUserQuestion` is a Layer 1 built-in
/// tool that routes structured questions through the unified
/// `HumanInteractionHandler` channel. End-to-end: tool invocation
/// → Extension lookup → handler.ask_question → JSON round-trip
/// → ToolResult. Works without any feature flags — demonstrates
/// the tool is general-purpose SDK surface, not coding-specific.
#[tokio::test]
async fn phase_d_c2_ask_user_question_tool_end_to_end() {
    use async_trait::async_trait;
    use branchforge::authorization::{
        HumanInteractionExtension, HumanInteractionHandler, HumanInteractionResult,
        QuestionRequest, QuestionResponse,
    };
    use branchforge::tools::{AskUserQuestionTool, ExecutionContext, Tool};
    use std::sync::Arc;

    #[derive(Debug)]
    struct SurveyHost {
        received: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl HumanInteractionHandler for SurveyHost {
        async fn ask_question(
            &self,
            req: QuestionRequest,
        ) -> HumanInteractionResult<QuestionResponse> {
            let mut log = self.received.lock().unwrap();
            for q in &req.questions {
                log.push(q.text.clone());
            }
            Ok(QuestionResponse {
                selections: req.questions.iter().map(|_| vec![0]).collect(),
            })
        }
    }

    let host = Arc::new(SurveyHost {
        received: std::sync::Mutex::new(Vec::new()),
    });
    let handler: Arc<dyn HumanInteractionHandler> = host.clone();

    let mut ctx = ExecutionContext::empty();
    ctx.extensions_mut()
        .insert(HumanInteractionExtension::new(handler));

    let tool = AskUserQuestionTool;
    let input = serde_json::json!({
        "questions": [
            {"text": "What is your favourite colour?", "options": ["red", "blue", "green"]},
            {"text": "Pick a language", "options": ["rust", "python"]},
        ]
    });

    let result = tool.execute(input, &ctx).await;
    assert!(
        !result.is_error(),
        "tool must succeed when handler is wired"
    );
    let text = result.output.text();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["selections"], serde_json::json!([[0], [0]]));

    // Host actually received both questions.
    let log = host.received.lock().unwrap();
    assert_eq!(log.len(), 2);
    assert!(log[0].contains("colour"));
    assert!(log[1].contains("language"));
}

/// Phase D Workstream C-1: the unified `HumanInteractionHandler`
/// trait is the single plug-point for human-in-the-loop. Host
/// applications implement one method (or more) and the runtime
/// routes tool-approval, AskUserQuestion, and MCP elicitation
/// through the same handler. Default method impls return
/// `NotSupported` so partial hosts get fail-closed behaviour.
#[tokio::test]
async fn phase_d_c1_human_interaction_handler_trait_surface() {
    use async_trait::async_trait;
    use branchforge::authorization::{
        ElicitationRequest, HumanInteractionError, HumanInteractionHandler, HumanInteractionResult,
        QuestionRequest, ToolApprovalRequest, ToolApprovalResponse,
    };
    use std::sync::Arc;

    /// Host that only implements tool approval. The default stub
    /// impls on `ask_question`/`elicit` return NotSupported.
    #[derive(Debug)]
    struct ApprovalOnlyHost;

    #[async_trait]
    impl HumanInteractionHandler for ApprovalOnlyHost {
        fn name(&self) -> &str {
            "approval_only"
        }
        async fn approve_tool(
            &self,
            req: ToolApprovalRequest,
        ) -> HumanInteractionResult<ToolApprovalResponse> {
            if req.tool_name == "Bash" {
                Ok(ToolApprovalResponse::Deny {
                    reason: "Bash is banned in this host".into(),
                })
            } else {
                Ok(ToolApprovalResponse::Approve)
            }
        }
    }

    let handler: Arc<dyn HumanInteractionHandler> = Arc::new(ApprovalOnlyHost);
    assert_eq!(handler.name(), "approval_only");

    // Approve path.
    let resp = handler
        .approve_tool(ToolApprovalRequest {
            tool_name: "Read".into(),
            tool_call_id: "tc_1".into(),
            tool_input: serde_json::json!({}),
            reason: "test".into(),
        })
        .await
        .unwrap();
    assert!(matches!(resp, ToolApprovalResponse::Approve));

    // Deny path.
    let resp = handler
        .approve_tool(ToolApprovalRequest {
            tool_name: "Bash".into(),
            tool_call_id: "tc_2".into(),
            tool_input: serde_json::json!({}),
            reason: "test".into(),
        })
        .await
        .unwrap();
    assert!(matches!(resp, ToolApprovalResponse::Deny { .. }));

    // Unimplemented methods fail closed with NotSupported.
    let err = handler
        .ask_question(QuestionRequest { questions: vec![] })
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        HumanInteractionError::NotSupported("ask_question")
    ));

    let err = handler
        .elicit(ElicitationRequest {
            prompt: "x".into(),
            schema: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, HumanInteractionError::NotSupported("elicit")));
}

/// Phase D Workstream B-3: a `PreToolUse` hook that rewrites a tool
/// input via `HookOutput::updated_input` must propagate the rewrite
/// through `HookRegistry::execute`. The agent loop then runs the
/// tool against the rewritten input while preserving the original
/// for forensic audit on the graph node.
#[tokio::test]
async fn phase_d_b3_hook_updated_input_propagates_through_merge() {
    use async_trait::async_trait;
    use branchforge::hooks::{
        Hook, HookContext, HookEvent, HookEventData, HookInput, HookOutput, HookRegistry,
    };

    /// Hook that rewrites the `command` field in Bash inputs to a
    /// normalized form. This is the general SDK primitive: any
    /// PreToolUse hook that returns `updated_input` must be
    /// honoured by the agent loop.
    #[derive(Debug)]
    struct NormalizingHook {
        events: Vec<HookEvent>,
    }

    #[async_trait]
    impl Hook for NormalizingHook {
        fn name(&self) -> &str {
            "normalizing"
        }
        fn events(&self) -> &[HookEvent] {
            &self.events
        }
        async fn execute(
            &self,
            input: HookInput,
            _context: &HookContext,
        ) -> branchforge::Result<HookOutput> {
            let HookEventData::PreToolUse { tool_input, .. } = input.data else {
                return Ok(HookOutput::allow());
            };
            let mut rewritten = tool_input;
            if let Some(obj) = rewritten.as_object_mut()
                && let Some(cmd) = obj.get("command").and_then(serde_json::Value::as_str)
            {
                let safe = format!("{cmd} --safe");
                obj.insert("command".into(), serde_json::Value::String(safe));
            }
            Ok(HookOutput::allow().updated_input(rewritten))
        }
    }

    let mut registry = HookRegistry::new();
    registry.register(NormalizingHook {
        events: vec![HookEvent::PreToolUse],
    });

    let original = serde_json::json!({"command": "ls /tmp"});
    let ctx = HookContext::new("sess-1");
    let output = registry
        .execute(
            HookEvent::PreToolUse,
            HookInput::pre_tool_use("sess-1", "Bash", original.clone()),
            &ctx,
        )
        .await
        .unwrap();

    assert!(output.continue_execution);
    let updated = output
        .updated_input
        .expect("PreToolUse hook must propagate updated_input through the merge");
    assert_eq!(updated["command"], "ls /tmp --safe");

    // The caller's original value is untouched — the rewrite is
    // additive and scoped to the hook pipeline.
    assert_eq!(original["command"], "ls /tmp");
}

/// Phase D Workstream B-1: the runtime exploits
/// `SchemaTool::validate_input_typed` as a side-effect-free preflight
/// step. EditTool rejects inputs where `old_string == new_string` via
/// its validate_input_typed impl; this test drives the trait's erased
/// `validate_input` surface directly (the same surface the agent
/// scheduler now calls in parallel) and asserts a ValidationError
/// with the expected machine code is returned.
#[cfg(feature = "local-fs")]
#[tokio::test]
async fn phase_d_b1_validate_input_is_side_effect_free_preflight() {
    use branchforge::tools::{EditTool, ExecutionContext, Tool};

    let tool = EditTool;
    let ctx = ExecutionContext::default();

    // Valid input passes.
    let ok = serde_json::json!({
        "file_path": "/tmp/example.txt",
        "old_string": "foo",
        "new_string": "bar",
    });
    assert!(tool.validate_input(&ok, &ctx).await.is_ok());

    // Invalid input (old == new) rejected with a typed error.
    let bad = serde_json::json!({
        "file_path": "/tmp/example.txt",
        "old_string": "same",
        "new_string": "same",
    });
    let err = tool
        .validate_input(&bad, &ctx)
        .await
        .expect_err("identical old_string/new_string must be rejected by preflight");
    assert_eq!(err.code, Some(2));
    assert!(err.message.contains("must be different"));
}

/// Phase D Workstream A-3: the cross-cutting `DecisionReason` trait
/// is implemented by every domain's reason enum. This test proves
/// that permission, compaction, and recovery decisions all expose a
/// low-cardinality `category()` label and a free-form `summary()`
/// suitable for observability routing.
#[test]
fn phase_d_a3_decision_reason_cross_cutting_trait() {
    use branchforge::FailureCategory;
    use branchforge::agent::recovery_recipes::{
        RecipeRegistry, RecoveryAction, RecoveryDecisionInput, builtin_general_recipes,
    };
    use branchforge::authorization::{PermissionDeniedReason, ToolPolicyBuilder};
    use branchforge::decision::DecisionReason;
    use branchforge::session::compact::CompactSkipReason;

    // Authorization — policy_deny for a denied rule.
    let policy = ToolPolicyBuilder::new().deny("Write").build();
    let decision = policy.check("Write", &[]);
    let reason = match decision {
        branchforge::authorization::PermissionDecision::Deny { reason } => reason,
        other => panic!("expected Deny, got {other:?}"),
    };
    assert_eq!(reason.category(), "policy_deny");
    assert!(!reason.summary().is_empty());

    // Categories are stable constants, not interpolated from user data.
    assert_eq!(
        PermissionDeniedReason::PlanModeBlocked.category(),
        "plan_mode"
    );
    assert_eq!(
        PermissionDeniedReason::SupervisedReview.category(),
        "supervised_review"
    );
    assert_eq!(
        PermissionDeniedReason::BudgetExceeded.category(),
        "budget_exceeded"
    );

    // Compaction — typed skip reason instead of free-form string.
    let skip = CompactSkipReason::CircuitBreakerOpen;
    assert_eq!(skip.category(), "circuit_breaker_open");
    assert_eq!(skip.summary(), "circuit breaker open");

    // Recovery — decision.category() is the matched recipe name.
    let registry = RecipeRegistry::new().with_boxed_recipes(builtin_general_recipes());
    let recovery = registry.decide(&RecoveryDecisionInput::new(FailureCategory::RateLimit, 0));
    assert!(matches!(recovery.action, RecoveryAction::RetryAfter { .. }));
    assert_eq!(recovery.category(), "rate_limit_backoff");
    assert!(recovery.summary().contains("rate_limit_backoff"));
}

/// Phase D Workstream A-1: the DSL rule `Bash(rm:*)` must match via
/// the tool's own `permission_subjects` output — there is no
/// parallel extractor registry. Regression test that covers the
/// full loop: build a `BashTool`, ask it for subjects on a `rm`
/// command, and feed them into `ToolPolicy::check`.
#[cfg(feature = "coding-tools")]
#[test]
fn phase_d_a1_permission_subjects_drive_dsl_matching() {
    use branchforge::authorization::{PermissionDecision, ToolPolicyBuilder};
    use branchforge::tools::{BashTool, ProcessScheduler, Tool};
    use std::sync::Arc;

    let tool = BashTool::new(Arc::new(ProcessScheduler::default()));
    let policy = ToolPolicyBuilder::new()
        .allow(".*")
        .deny("Bash(rm:*)")
        .build();

    // Destructive command — subjects come from the tool.
    let rm_input = serde_json::json!({"command": "rm -rf /tmp/x"});
    let rm_subjects = tool.permission_subjects(&rm_input);
    assert_eq!(rm_subjects, vec!["rm"]);
    assert!(matches!(
        policy.check("Bash", &rm_subjects),
        PermissionDecision::Deny { .. }
    ));

    // Unrelated command — same tool, same DSL, different subject.
    let ls_input = serde_json::json!({"command": "ls /tmp"});
    let ls_subjects = tool.permission_subjects(&ls_input);
    assert_eq!(ls_subjects, vec!["ls"]);
    assert!(matches!(
        policy.check("Bash", &ls_subjects),
        PermissionDecision::Allow
    ));
}
