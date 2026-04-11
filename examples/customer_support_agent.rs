//! Pure-core customer support agent — no filesystem, no shell.
//!
//! This example is the canonical proof that BranchForge works as a
//! **general-purpose Agent SDK**. It runs under the default feature set
//! (`anthropic-direct`) with **no filesystem tools, no shell tools, no
//! cloud provider SDKs** — just the Layer 1 runtime plus a custom tool
//! the application provides itself.
//!
//! The agent impersonates a tier-1 support bot with access to a tiny
//! in-memory knowledge base. When the user asks about pricing, refunds,
//! or an SLA, the model calls the `kb_lookup` tool to retrieve the
//! relevant article from the embedded KB. There is no CLAUDE.md, no
//! working directory, no `SecureFs`, no `Bash` — and crucially no
//! opt-in to any of those things is needed.
//!
//! # What this demonstrates
//!
//! - `Agent::builder()` works without `.working_dir(...)` or `.tools(ToolSurface::coding())`.
//! - A user-defined `SchemaTool` implementation slots into the runtime via
//!   `.tool(...)` with zero filesystem assumptions.
//! - `ToolSurface::core()` is a real, useful surface on its own — the
//!   registered `SkillTool`, `PlanTool`, `TodoWriteTool`, and the custom
//!   `kb_lookup` are all the bot needs.
//! - Multi-turn conversation, streaming events, and per-tool progress all
//!   work in the pure-core topology.
//!
//! # Running
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-... cargo run --example customer_support_agent
//! ANTHROPIC_API_KEY=sk-ant-... cargo run --example customer_support_agent -- "What is your refund policy?"
//! ```
//!
//! Required features: `anthropic-direct` (the default — no `--features`
//! flag needed).

use async_trait::async_trait;
use branchforge::prelude::*;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use std::pin::pin;

// ---------------------------------------------------------------------------
// Knowledge base tool — a custom SchemaTool with zero filesystem access.
// ---------------------------------------------------------------------------

/// Input schema for the KB lookup. The model invokes this tool with a
/// topic string; we return the first KB article whose topic matches
/// (case-insensitive).
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
struct KbLookupInput {
    /// High-level topic the user is asking about. Examples: "pricing",
    /// "refund", "sla", "data residency".
    topic: String,
}

/// A tiny in-memory KB. In a real deployment this would be a vector
/// store, a SQL table, or an HTTP API call — none of which requires
/// BranchForge to bring its own filesystem tooling.
struct KbLookupTool {
    articles: Vec<KbArticle>,
}

struct KbArticle {
    topic: &'static str,
    title: &'static str,
    body: &'static str,
}

impl KbLookupTool {
    fn with_defaults() -> Self {
        Self {
            articles: vec![
                KbArticle {
                    topic: "pricing",
                    title: "Pricing tiers",
                    body: "Starter is $20/mo, Professional is $80/mo, \
                           Enterprise is custom-quoted. All tiers include \
                           email support and a 14-day free trial.",
                },
                KbArticle {
                    topic: "refund",
                    title: "Refund policy",
                    body: "Full refunds are available within 30 days of \
                           purchase for any reason. After 30 days, refunds \
                           are prorated to the unused portion of the \
                           current billing cycle.",
                },
                KbArticle {
                    topic: "sla",
                    title: "Service Level Agreement",
                    body: "Professional and Enterprise tiers include a \
                           99.9% uptime guarantee. Credit is issued at \
                           10x the outage duration against the next \
                           invoice if the guarantee is missed.",
                },
                KbArticle {
                    topic: "data residency",
                    title: "Data residency",
                    body: "Enterprise customers can pin their workspace \
                           to a specific region (US-East, EU-West, \
                           AP-Southeast). Starter and Professional \
                           workspaces are global by default.",
                },
            ],
        }
    }

    fn lookup(&self, topic: &str) -> Option<&KbArticle> {
        let needle = topic.to_lowercase();
        self.articles
            .iter()
            .find(|a| a.topic == needle || a.title.to_lowercase().contains(&needle))
    }
}

#[async_trait]
impl SchemaTool for KbLookupTool {
    type Input = KbLookupInput;

    const NAME: &'static str = "kb_lookup";
    const READ_ONLY: bool = true;
    const DESCRIPTION: &'static str = "Look up a knowledge-base article by topic. \
        Available topics include 'pricing', 'refund', 'sla', and 'data residency'. \
        Returns the article title and body as plain text.";

    async fn handle(&self, input: Self::Input, _ctx: &ExecutionContext) -> ToolResult {
        match self.lookup(&input.topic) {
            Some(article) => {
                ToolResult::success(format!("# {}\n\n{}", article.title, article.body))
            }
            None => ToolResult::success(format!(
                "No article found for topic {:?}. Available topics: pricing, refund, sla, data residency.",
                input.topic
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Main — build the agent, execute a query, print the streamed response.
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // reqwest pulls in both `ring` and `aws-lc-rs` transitively; explicitly
    // install one as the default CryptoProvider so the TLS handshake has a
    // unique backend.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Friendly no-op when the user just wants to verify the example builds.
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!(
            "ANTHROPIC_API_KEY not set — showing dry-run structure only.\n\
             Set ANTHROPIC_API_KEY=sk-ant-... to execute the agent."
        );
        let tool = KbLookupTool::with_defaults();
        let result = tool
            .handle(
                KbLookupInput {
                    topic: "refund".into(),
                },
                &ExecutionContext::empty(),
            )
            .await;
        eprintln!("\n--- kb_lookup dry run ---\n{result:#?}");
        return Ok(());
    }

    let question = std::env::args().nth(1).unwrap_or_else(|| {
        "Hi! I signed up last week on the Starter plan but wanted to try Enterprise. \
         What's the pricing, and can I get a refund on my current subscription?"
            .to_string()
    });

    let agent = Agent::builder()
        .model("claude-sonnet-4-5")
        // `ToolSurface::core()` = Layer 1 primitives only (Skill, Plan,
        // TodoWrite, GraphHistory, Task, TaskOutput). No filesystem, no
        // shell.
        .tools(ToolSurface::core())
        .tool(KbLookupTool::with_defaults())
        .build()
        .await?;

    eprintln!("=> pure-core customer support agent ready");
    eprintln!("=> question: {question}");
    eprintln!();

    let stream = agent.execute_stream(&question).await?;
    let mut stream = pin!(stream);

    while let Some(event) = stream.next().await {
        match event? {
            AgentEvent::Text { delta } => print!("{delta}"),
            AgentEvent::ToolStart { name, .. } => eprintln!("\n[tool start: {name}]"),
            AgentEvent::ToolComplete { name, .. } => eprintln!("\n[tool complete: {name}]"),
            AgentEvent::Complete(result) => {
                eprintln!(
                    "\n\n=> done · {} iteration(s) · {} total tokens",
                    result.metrics.iterations,
                    result.total_tokens()
                );
            }
            _ => {}
        }
    }

    Ok(())
}
