//! Cross-cutting verification matrix tests.
//!
//! These tests intentionally exercise multiple design axes together:
//! - skills + rules + progressive disclosure + compaction budget signaling
//! - graph-first compaction + archive/restore + identity preservation
//! - subagent metadata + lazy prompt loading + tool/model restrictions

use std::path::Path;

use branchforge::common::ToolRestricted;
use branchforge::session::{
    ArchivePolicy, CompactExecutor, CompactStrategy, ExportPolicy, MemoryPersistence, Persistence,
    QueueItem, SessionArchiveService,
};
use branchforge::types::TokenUsage;
use branchforge::{
    ContentBlock, ContentSource, ContextBuilder, Index, ModelConfig, RuleIndex, Session,
    SessionConfig, SessionMessage, SkillIndex, SubagentIndex,
};

#[tokio::test]
async fn progressive_disclosure_matrix_preserves_manual_only_and_rule_scoping() {
    let review_skill = SkillIndex::new("review-pr", "Review a pull request")
        .source(ContentSource::in_memory("Review PR: $ARGUMENTS"))
        .triggers(["review trigger", "review keyword"]);

    let mut internal_skill = SkillIndex::new("internal-audit", "Internal only")
        .source(ContentSource::in_memory("Internal audit: $ARGUMENTS"))
        .triggers(["internal keyword", "audit trigger"]);
    internal_skill.disable_model_invocation = true;

    let global_rule = RuleIndex::new("global")
        .description("Always active")
        .source(ContentSource::in_memory("Global rule body"));
    let rust_rule = RuleIndex::new("rust")
        .description("Rust-only")
        .paths(vec!["**/*.rs".into()])
        .source(ContentSource::in_memory(
            "Use Result and explicit error handling",
        ));
    let ts_rule = RuleIndex::new("typescript")
        .description("TypeScript-only")
        .paths(vec!["**/*.ts".into()])
        .source(ContentSource::in_memory("Use strict mode"));

    let mut orchestrator = ContextBuilder::new()
        .claude_md("# Project\nVerification matrix")
        .skill(review_skill)
        .skill(internal_skill)
        .rule(global_rule)
        .rule(rust_rule)
        .rule(ts_rule)
        .build()
        .unwrap();

    let static_context = orchestrator.static_context();
    assert!(static_context.skill_summary.contains("review-pr"));
    assert!(!static_context.skill_summary.contains("internal-audit"));
    assert!(static_context.rules_summary.contains("global"));
    assert!(static_context.rules_summary.contains("rust"));
    assert!(static_context.rules_summary.contains("typescript"));

    let explicit = orchestrator
        .find_skill_by_command("/internal-audit inspect auth flow")
        .expect("manual-only skill should still support explicit invocation");
    assert_eq!(explicit.name, "internal-audit");

    let trigger_matches = orchestrator.find_skills_by_triggers("please use review keyword");
    assert_eq!(trigger_matches.len(), 1);
    assert_eq!(trigger_matches[0].name, "review-pr");
    assert!(
        orchestrator
            .find_skills_by_triggers("please use internal keyword")
            .is_empty(),
        "manual-only skills must not surface through trigger-based discovery"
    );

    let dynamic_context = orchestrator
        .build_dynamic_context(Some(Path::new("src/lib.rs")))
        .await;
    assert!(dynamic_context.contains("global"));
    assert!(dynamic_context.contains("rust"));
    assert!(!dynamic_context.contains("typescript"));

    let active_rules = orchestrator
        .activate_rules_for_file(Path::new("src/lib.rs"))
        .await;
    assert_eq!(active_rules.len(), 2);

    orchestrator.update_usage(&TokenUsage {
        input_tokens: 170_000,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
    });
    assert!(orchestrator.needs_compact());
}

#[tokio::test]
async fn graph_first_compaction_archive_roundtrip_preserves_identity_and_history() {
    let mut session = Session::new(SessionConfig::default());
    session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
    session
        .add_message(SessionMessage::user(vec![ContentBlock::text(
            "Investigate auth bug",
        )]))
        .unwrap();
    session
        .add_message(SessionMessage::assistant(vec![ContentBlock::text(
            "I traced it to session refresh ordering",
        )]))
        .unwrap();
    session
        .add_message(SessionMessage::user(vec![ContentBlock::text(
            "Preserve the findings and next steps",
        )]))
        .unwrap();
    session.current_input_tokens = 180_000;

    let executor = CompactExecutor::new(CompactStrategy::default());
    let compact = executor.apply_compact(
        &mut session,
        "Auth refresh bug traced to stale session ordering; next step is durable verification."
            .to_string(),
    );
    executor.record_compact(&mut session, &compact);

    assert_eq!(session.current_branch_messages().len(), 1);
    assert_eq!(session.compact_history.len(), 1);
    assert_eq!(session.graph.checkpoints.len(), 1);
    assert!(session.summary.is_some());
    assert!(
        session.graph.events.len() >= 4,
        "graph history should retain pre-compaction lineage"
    );

    let pending_queue = vec![
        QueueItem::enqueue(session.id, "first follow-up").priority(10),
        QueueItem::enqueue(session.id, "second follow-up").priority(1),
    ];
    let bundle = SessionArchiveService::export_bundle(
        &session,
        &ExportPolicy::default(),
        &ArchivePolicy {
            include_queue_state: true,
            include_compact_history: true,
            ..ArchivePolicy::default()
        },
        pending_queue.clone(),
    )
    .expect("archive export should succeed");

    let persistence = MemoryPersistence::new();
    let restored = SessionArchiveService::restore_into(&bundle, &persistence)
        .await
        .expect("archive restore should succeed");
    let restored_queue = persistence
        .pending_queue(&restored.id)
        .await
        .expect("queue load should succeed");

    assert_eq!(restored.tenant_id.as_deref(), Some("tenant-a"));
    assert_eq!(restored.principal_id.as_deref(), Some("user-1"));
    assert_eq!(restored.current_branch_messages().len(), 1);
    assert_eq!(restored.compact_history.len(), 1);
    assert_eq!(restored.graph.checkpoints.len(), 1);
    assert_eq!(restored_queue.len(), 2);
    assert_eq!(restored_queue[0].content, "first follow-up");
    assert_eq!(restored_queue[1].content, "second follow-up");
    assert_eq!(restored.current_input_tokens, session.current_input_tokens);
}

#[tokio::test]
async fn subagent_matrix_keeps_lazy_prompt_and_restricted_surface() {
    let subagent = SubagentIndex::new("reviewer", "Focused code reviewer")
        .source(ContentSource::in_memory(
            "You are a reviewer. Inspect code and report risks.",
        ))
        .tools(["Read", "Grep"])
        .skills(["review-pr"])
        .mcp_servers(["docs"])
        .model("haiku")
        .max_turns(3);

    assert!(subagent.has_tool_restrictions());
    assert!(subagent.is_tool_allowed("Read"));
    assert!(subagent.is_tool_allowed("Grep"));
    assert!(!subagent.is_tool_allowed("Write"));
    assert_eq!(subagent.skills, vec!["review-pr"]);
    assert_eq!(subagent.mcp_servers, vec!["docs"]);
    assert_eq!(subagent.max_turns, Some(3));
    assert!(
        subagent.to_summary_line().contains("MCP: docs"),
        "summary should surface MCP disclosure metadata"
    );

    let prompt = subagent
        .load_prompt()
        .await
        .expect("prompt should load lazily");
    assert!(prompt.contains("focused code reviewer") || prompt.contains("reviewer"));

    let model_config = ModelConfig::default();
    let model = subagent.resolve_model(&model_config);
    assert!(
        model.contains("haiku"),
        "model alias resolution should stay aligned with small-model expectations"
    );
}
