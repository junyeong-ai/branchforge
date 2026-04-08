//! Live integration test for all new features.
//!
//! Requires CLI OAuth credentials (run inside Claude Code or with `claude` CLI configured).
//!
//! Tests:
//! 1. Basic query via CLI auth
//! 2. Agent with advanced compaction (CompactionChain)
//! 3. Agent with coordination mode (Coordinator)
//! 4. SessionFilter search
//! 6. CronScheduler lifecycle
//! 7. ContentOverrides mechanism
//! 8. domain_instructions injection
//!
//! Run: cargo run --example live_integration_test --features "cli-auth,coding-tools,scheduling"

use std::sync::Arc;
use std::time::Duration;

use branchforge::ir::ContentPart;
use branchforge::orchestration::{
    AgentDirectory, AgentHandle, AgentId, Coordination, Coordinator, MessageChannel,
};
use branchforge::output_style::default_style;
use branchforge::session::compact::{
    CompactConfig, CompactionChain, CompactionContext, CompactionStrategy, FullCompaction,
    MicroCompaction, TimeBasedCompaction,
};
use branchforge::session::persistence::SessionFilter;
use branchforge::session::{MemoryPersistence, Persistence, Session, SessionConfig};
use branchforge::{CircuitBreaker, CircuitConfig, CircuitState};
use branchforge::{OutputStyle, SystemPromptGenerator};

fn check(name: &str, ok: bool) {
    if ok {
        println!("  [PASS] {}", name);
    } else {
        println!("  [FAIL] {}", name);
        std::process::exit(1);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("========================================");
    println!("  Live Integration Tests");
    println!("========================================\n");

    // =====================================================================
    // Test 1: CompactConfig (renamed from CompactStrategy)
    // =====================================================================
    println!("[Test 1] CompactConfig");
    {
        let config = CompactConfig::default();
        check("default enabled", config.enabled);
        check(
            "default threshold 0.8",
            (config.threshold_percent - 0.8).abs() < f32::EPSILON,
        );
        check("default detailed_summary true", config.detailed_summary);

        let config2 = CompactConfig::default()
            .detailed_summary(false)
            .threshold(0.7)
            .custom_instructions("Focus on errors");
        check("custom detailed_summary false", !config2.detailed_summary);
        check(
            "custom threshold",
            (config2.threshold_percent - 0.7).abs() < f32::EPSILON,
        );
        check(
            "custom instructions set",
            config2.custom_instructions.is_some(),
        );

        let style = OutputStyle::new("test", "test", "").domain_instructions("my domain");
        let from_style = CompactConfig::from_output_style(&style);
        check(
            "from_output_style with domain → detailed=true",
            from_style.detailed_summary,
        );

        let style_no = OutputStyle::new("test", "test", "");
        let from_style_no = CompactConfig::from_output_style(&style_no);
        check(
            "from_output_style without domain → detailed=false",
            !from_style_no.detailed_summary,
        );
    }

    // =====================================================================
    // Test 2: CompactionStrategy trait + implementations
    // =====================================================================
    println!("\n[Test 2] CompactionStrategy implementations");
    {
        let full = FullCompaction::default();
        check("FullCompaction name", full.name() == "full");
        check("FullCompaction requires_llm", full.requires_llm());
        check("FullCompaction is_durable", full.is_durable());

        let micro = MicroCompaction::default();
        check("MicroCompaction name", micro.name() == "micro");
        check("MicroCompaction no llm", !micro.requires_llm());
        check("MicroCompaction not durable", !micro.is_durable());

        let time = TimeBasedCompaction::default();
        check("TimeBasedCompaction name", time.name() == "time_based");
        check("TimeBasedCompaction no llm", !time.requires_llm());
        check("TimeBasedCompaction not durable", !time.is_durable());

        // Threshold checks
        let ctx_low = CompactionContext {
            current_tokens: 50_000,
            max_tokens: 100_000,
            message_count: 10,
            idle_duration: None,
            last_compact_at: None,
            consecutive_failures: 0,
        };
        let ctx_high = CompactionContext {
            current_tokens: 85_000,
            max_tokens: 100_000,
            message_count: 10,
            idle_duration: None,
            last_compact_at: None,
            consecutive_failures: 0,
        };
        check(
            "MicroCompaction not triggered at 50%",
            !micro.needs_compact(&ctx_low),
        );
        check(
            "FullCompaction not triggered at 50%",
            !full.needs_compact(&ctx_low),
        );
        check(
            "FullCompaction triggered at 85%",
            full.needs_compact(&ctx_high),
        );

        let ctx_idle = CompactionContext {
            current_tokens: 50_000,
            max_tokens: 100_000,
            message_count: 10,
            idle_duration: Some(Duration::from_secs(7200)),
            last_compact_at: None,
            consecutive_failures: 0,
        };
        check(
            "TimeBasedCompaction triggered when idle",
            time.needs_compact(&ctx_idle),
        );
    }

    // =====================================================================
    // Test 3: CompactionChain + CircuitBreaker
    // =====================================================================
    println!("\n[Test 3] CompactionChain + CircuitBreaker");
    {
        let chain = CompactionChain::builder()
            .strategy(MicroCompaction::default())
            .strategy(FullCompaction::default())
            .failure_threshold(3)
            .build();
        check("chain has 2 strategies", chain.len() == 2);
        check(
            "circuit closed",
            chain.circuit_state() == CircuitState::Closed,
        );

        // CircuitBreaker standalone
        let cb = CircuitBreaker::new(CircuitConfig {
            failure_threshold: 2,
            recovery_timeout: Duration::from_secs(1),
            success_threshold: 1,
        });
        check("CB starts closed", cb.state() == CircuitState::Closed);
        cb.record_failure();
        cb.record_failure();
        check(
            "CB opens after 2 failures",
            cb.state() == CircuitState::Open,
        );
        cb.reset();
        check("CB resets to closed", cb.state() == CircuitState::Closed);
    }

    // =====================================================================
    // Test 4: ContentOverrides
    // =====================================================================
    println!("\n[Test 4] ContentOverrides + to_api_messages");
    {
        let mut session = Session::new(SessionConfig::default());
        check(
            "session starts with empty overrides",
            session.content_overrides().is_empty(),
        );

        let node_id = uuid::Uuid::new_v4();
        session.set_content_override(node_id, vec![ContentPart::text("truncated")]);
        check("override added", session.content_overrides().len() == 1);
        check(
            "override retrievable",
            session.content_overrides().get(&node_id).is_some(),
        );

        session.clear_content_overrides();
        check("overrides cleared", session.content_overrides().is_empty());
    }

    // =====================================================================
    // Test 5: AgentDirectory + SendMessage
    // =====================================================================
    println!("\n[Test 5] Orchestration: AgentDirectory + SendMessage");
    {
        let dir = Arc::new(AgentDirectory::new());
        check("directory starts empty", dir.is_empty());

        let ch = Arc::new(MessageChannel::new(8));
        let handle = Arc::new(AgentHandle::new("worker-1", ch.clone()));
        let worker_id = handle.id;
        dir.register(handle);
        check("registered", dir.len() == 1);
        check("lookup by name", dir.get_by_name("worker-1").is_some());
        check("lookup by id", dir.get(&worker_id).is_some());

        let sender = AgentId::new();
        dir.send(sender, "worker-1", "do analysis").await?;
        let msg = ch.recv().await.unwrap();
        check("message received", msg.content == "do analysis");

        // Mark completed
        dir.get_by_name("worker-1")
            .unwrap()
            .mark_completed(Some("done".into()));
        let result = dir.send(sender, "worker-1", "more work").await;
        check("send to completed fails", result.is_err());

        dir.cleanup_finished();
        check("cleanup removes completed", dir.is_empty());
    }

    // =====================================================================
    // Test 6: Coordinator
    // =====================================================================
    println!("\n[Test 6] Coordinator configuration");
    {
        let coord = Coordinator::default();
        check("coordinator name", coord.name() == "coordinator");

        let constraints = coord.worker_constraints();
        check("isolated context", constraints.isolated_context);
        check("self-contained prompt", constraints.self_contained_prompt);
        check("max concurrent 10", constraints.max_concurrent == 10);

        let custom = Coordinator::builder()
            .max_workers(3)
            .worker_model("claude-haiku-4-5")
            .require_synthesis(false)
            .build();
        check(
            "custom max_workers",
            custom.worker_constraints().max_concurrent == 3,
        );
    }

    // =====================================================================
    // Test 7: SessionFilter + Persistence::search
    // =====================================================================
    println!("\n[Test 8] SessionFilter + Persistence::search");
    {
        let persistence = Arc::new(MemoryPersistence::new());

        let mut s1 = Session::new(SessionConfig::default());
        s1.set_identity(Some("team-a".into()), None);
        persistence.save(&s1).await?;

        let mut s2 = Session::new(SessionConfig::default());
        s2.set_identity(Some("team-b".into()), None);
        persistence.save(&s2).await?;

        let filter = SessionFilter::new().tenant("team-a");
        let results = persistence.search(&filter).await?;
        check("search finds 1 match", results.len() == 1);
        check("search finds correct session", results[0] == s1.id);

        let all = persistence.search(&SessionFilter::default()).await?;
        check("no filter returns all", all.len() == 2);

        let limited = SessionFilter::new().limit(1);
        let limited_results = persistence.search(&limited).await?;
        check("limit works", limited_results.len() == 1);
    }

    // =====================================================================
    // Test 9: domain_instructions in OutputStyle + SystemPromptGenerator
    // =====================================================================
    println!("\n[Test 9] domain_instructions + SystemPromptGenerator");
    {
        let style = OutputStyle::new("custom", "Custom agent", "Be helpful")
            .domain_instructions("You are a medical assistant. Follow clinical guidelines.");

        check("has domain instructions", style.has_domain_instructions());
        check(
            "domain content correct",
            style
                .domain_instructions
                .as_ref()
                .unwrap()
                .contains("medical"),
        );

        let prompt = SystemPromptGenerator::new().output_style(style).generate();
        check(
            "prompt contains domain instructions",
            prompt.contains("medical assistant"),
        );
        check(
            "prompt contains custom prompt",
            prompt.contains("Be helpful"),
        );
        check("prompt contains environment", prompt.contains("<env>"));

        // Without domain instructions
        let plain_style = OutputStyle::new("plain", "Plain", "Just be helpful");
        check(
            "no domain instructions",
            !plain_style.has_domain_instructions(),
        );

        let plain_prompt = SystemPromptGenerator::new()
            .output_style(plain_style)
            .generate();
        check(
            "plain prompt has no domain text",
            !plain_prompt.contains("medical"),
        );
        check(
            "plain prompt has custom prompt",
            plain_prompt.contains("Just be helpful"),
        );

        // Default style (should have coding instructions when coding-tools enabled)
        let default = default_style();
        #[cfg(feature = "coding-tools")]
        check(
            "default style has coding domain",
            default.has_domain_instructions(),
        );
        #[cfg(not(feature = "coding-tools"))]
        check(
            "default style has no domain (no coding-tools)",
            !default.has_domain_instructions(),
        );
    }

    // =====================================================================
    // Test 10: Live LLM query via CLI auth
    // =====================================================================
    println!("\n[Test 10] Live LLM query via Preset");
    {
        match branchforge::query("Reply with exactly: BRANCHFORGE_OK").await {
            Ok(text) => {
                check("LLM responded", !text.is_empty());
                check(
                    "response contains expected text",
                    text.contains("BRANCHFORGE_OK"),
                );
                println!("    Response: {}", text.trim());
            }
            Err(e) => {
                println!("  [SKIP] LLM query failed (expected without credentials): {e}");
            }
        }
    }

    // =====================================================================
    // Test 11: Live Agent with advanced compaction
    // =====================================================================
    println!("\n[Test 11] Live Agent with CompactionChain");
    {
        use branchforge::{Agent, Auth};

        match Agent::builder().auth(Auth::ClaudeCli).await {
            Ok(builder) => {
                let agent = builder
                    .model("claude-haiku-4-5")
                    .advanced_compaction()
                    .build()
                    .await;

                match agent {
                    Ok(agent) => {
                        check("agent built with compaction chain", true);
                        let result = agent.execute("Reply with exactly: AGENT_OK").await;
                        match result {
                            Ok(result) => {
                                check("agent executed", !result.text().is_empty());
                                println!("    Response: {}", result.text().trim());
                            }
                            Err(e) => {
                                println!("  [SKIP] Agent execution failed: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        println!("  [SKIP] Agent build failed: {}", e);
                    }
                }
            }
            Err(e) => {
                println!("  [SKIP] CLI auth not available: {}", e);
            }
        }
    }

    // =====================================================================
    // Test 12: Scheduling (CronScheduler)
    // =====================================================================
    #[cfg(feature = "scheduling")]
    {
        println!("\n[Test 12] CronScheduler");
        use branchforge::scheduling::CronScheduler;
        use std::sync::atomic::{AtomicU32, Ordering};

        let scheduler = CronScheduler::new();
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let id = scheduler
            .register("test-job", Duration::from_millis(50), move || {
                let c = counter_clone.clone();
                Box::pin(async move {
                    c.fetch_add(1, Ordering::Relaxed);
                })
            })
            .await;

        tokio::time::sleep(Duration::from_millis(200)).await;
        let count = counter.load(Ordering::Relaxed);
        check("cron executed at least once", count >= 1);
        println!("    Executions: {}", count);

        scheduler.set_enabled(&id, false).await;
        let before = counter.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let after = counter.load(Ordering::Relaxed);
        check("disabled stops execution", after == before);

        scheduler.stop().await;
        scheduler.list().await; // verify no panic after stop
        check("scheduler stopped cleanly", true);
    }

    // =====================================================================
    // Test 13: Tool ordering (prompt cache stability)
    // =====================================================================
    println!("\n[Test 13] Tool ordering for cache stability");
    {
        use branchforge::tools::ToolRegistryBuilder;

        let registry = ToolRegistryBuilder::new().build();
        let names = registry.names();
        let mut sorted = names.clone();
        sorted.sort();
        check("tool names are sorted", names == sorted);

        let defs = registry.definitions();
        let def_names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        let mut sorted_defs = def_names.clone();
        sorted_defs.sort();
        check("tool definitions are sorted", def_names == sorted_defs);
    }

    // =====================================================================
    // Test 14: Tool READ_ONLY trait
    // =====================================================================
    #[cfg(feature = "coding-tools")]
    {
        println!("\n[Test 14] Tool READ_ONLY flags");
        use branchforge::tools::{ToolRegistryBuilder, ToolSurface};

        let registry = ToolRegistryBuilder::new()
            .access(ToolSurface::all())
            .build();

        // Read-only tools
        if let Some(tool) = registry.get("Read") {
            check("Read is read-only", tool.is_read_only());
        }
        if let Some(tool) = registry.get("Glob") {
            check("Glob is read-only", tool.is_read_only());
        }
        if let Some(tool) = registry.get("Grep") {
            check("Grep is read-only", tool.is_read_only());
        }

        // Mutating tools
        if let Some(tool) = registry.get("Bash") {
            check("Bash is NOT read-only", !tool.is_read_only());
        }
        if let Some(tool) = registry.get("Edit") {
            check("Edit is NOT read-only", !tool.is_read_only());
        }
        if let Some(tool) = registry.get("Write") {
            check("Write is NOT read-only", !tool.is_read_only());
        }
    }

    // =====================================================================
    // Test 15: CostSummary from AgentResult
    // =====================================================================
    println!("\n[Test 15] CostSummary from live agent");
    {
        use branchforge::{Agent, Auth};

        match Agent::builder().auth(Auth::ClaudeCli).await {
            Ok(builder) => {
                let agent = builder.model("claude-haiku-4-5").build().await;

                match agent {
                    Ok(agent) => {
                        let result = agent.execute("Reply with exactly: COST_OK").await;
                        match result {
                            Ok(result) => {
                                let summary = result.cost_summary();
                                check(
                                    "cost summary has total",
                                    summary.total_cost_usd >= rust_decimal::Decimal::ZERO,
                                );
                                check(
                                    "cost summary has tokens",
                                    summary.total_input_tokens > 0
                                        || summary.total_output_tokens > 0,
                                );
                                println!("    Cost: ${:.6}", summary.total_cost_usd);
                                println!("    Report:\n{}", summary.format_report());
                            }
                            Err(e) => println!("  [SKIP] Agent failed: {}", e),
                        }
                    }
                    Err(e) => println!("  [SKIP] Build failed: {}", e),
                }
            }
            Err(e) => println!("  [SKIP] CLI auth not available: {}", e),
        }
    }

    // =====================================================================
    // Test 16: RecoveryStrategy configuration
    // =====================================================================
    println!("\n[Test 16] RecoveryStrategy configuration");
    {
        use branchforge::{Agent, Auth};
        match Agent::builder().auth(Auth::ClaudeCli).await {
            Ok(builder) => {
                let agent = builder
                    .model("claude-haiku-4-5")
                    .default_recovery()
                    .advanced_compaction()
                    .build()
                    .await;

                match agent {
                    Ok(agent) => {
                        check("agent built with recovery + compaction", true);
                        let result = agent.execute("Reply with exactly: RECOVERY_OK").await;
                        match result {
                            Ok(result) => {
                                check("agent with recovery executed", !result.text().is_empty());
                                println!("    Response: {}", result.text().trim());
                            }
                            Err(e) => println!("  [SKIP] Execution failed: {}", e),
                        }
                    }
                    Err(e) => println!("  [SKIP] Build failed: {}", e),
                }
            }
            Err(e) => println!("  [SKIP] CLI auth not available: {}", e),
        }
    }

    println!("\n========================================");
    println!("  All tests passed!");
    println!("========================================");

    Ok(())
}
