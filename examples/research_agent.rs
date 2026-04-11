//! Local research agent — `local-fs` feature, no shell execution.
//!
//! Demonstrates that BranchForge is **genuinely useful for non-coding
//! local agents**. A research assistant reading markdown notes, a
//! knowledge worker grepping a second-brain directory, or an analyst
//! scanning local reports all need the same core capabilities:
//! filesystem search, file reading, and (optionally) report writing —
//! and none of them needs shell execution.
//!
//! This example wires up those capabilities and runs an agent over a
//! small markdown corpus stored in a temporary directory. The corpus is
//! created at runtime so the example is self-contained; in a real
//! deployment you would point the workspace at `~/notes` or similar.
//!
//! # What this demonstrates
//!
//! - `features = ["local-fs"]` enables the full `Read`/`Write`/`Edit`/
//!   `Glob`/`Grep` tool surface without also pulling in `coding-tools`.
//! - The `explore` builtin subagent (also `local-fs`-only) uses
//!   `Read`/`Grep`/`Glob`/`TodoWrite` and **cannot** invoke `Bash`. If
//!   you try to delegate shell work to it, the policy denies it at the
//!   registration boundary.
//! - No `CLAUDE.md` or `.claude/` convention is assumed — the agent
//!   works on any directory of markdown files.
//! - The `coding-tools` feature is *not* required and is *not* active
//!   in the default build of this example.
//!
//! # Running
//!
//! ```bash
//! # Default corpus (temp dir auto-populated):
//! ANTHROPIC_API_KEY=sk-ant-... \
//!   cargo run --example research_agent --features local-fs
//!
//! # Point at your own notes directory:
//! ANTHROPIC_API_KEY=sk-ant-... \
//!   cargo run --example research_agent --features local-fs -- \
//!     ~/notes "What did I conclude about tokenizer drift?"
//! ```
//!
//! Required features: `anthropic-direct` (default) + `local-fs`.

use branchforge::prelude::*;
use futures::StreamExt;
use std::path::{Path, PathBuf};
use std::pin::pin;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut args = std::env::args().skip(1);
    let workspace = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("branchforge_research_demo"));
    let question = args.next().unwrap_or_else(|| {
        "Summarize the main research themes in these notes. Which areas are repeated \
         across multiple files, and which ideas look unfinished?"
            .to_string()
    });

    // If the workspace is our auto-generated demo path, seed it with a
    // tiny corpus so the example runs out of the box.
    if workspace.starts_with(std::env::temp_dir())
        && workspace.file_name() == Some(std::ffi::OsStr::new("branchforge_research_demo"))
    {
        seed_demo_corpus(&workspace)?;
        eprintln!("=> seeded demo corpus at {}", workspace.display());
    } else if !workspace.exists() {
        eprintln!(
            "workspace {} does not exist. Pass an existing markdown directory as the first arg.",
            workspace.display()
        );
        return Ok(());
    }

    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!(
            "ANTHROPIC_API_KEY not set — showing workspace scan only.\n\
             Set ANTHROPIC_API_KEY=sk-ant-... to execute the agent."
        );
        scan_workspace(&workspace)?;
        return Ok(());
    }

    // Build the agent with the local-fs surface. Note: no `.as_coding_agent()`,
    // no `.with_git_context()`, no `coding-tools` feature anywhere.
    let agent = Agent::builder()
        .model("claude-sonnet-4-5")
        .working_dir(&workspace)
        .tools(ToolSurface::local_fs())
        .build()
        .await?;

    eprintln!("=> research agent ready");
    eprintln!("=> workspace: {}", workspace.display());
    eprintln!("=> question: {question}");
    eprintln!();

    let stream = agent.execute_stream(&question).await?;
    let mut stream = pin!(stream);

    while let Some(event) = stream.next().await {
        match event? {
            AgentEvent::Text { delta } => print!("{delta}"),
            AgentEvent::ToolStart { name, .. } => eprintln!("\n[tool start: {name}]"),
            AgentEvent::ToolComplete { name, .. } => eprintln!("[tool complete: {name}]"),
            AgentEvent::Complete(result) => {
                eprintln!(
                    "\n\n=> done · {} iteration(s) · {} tool call(s) · {} total tokens",
                    result.metrics.iterations,
                    result.metrics.tool_calls,
                    result.total_tokens()
                );
            }
            _ => {}
        }
    }

    Ok(())
}

fn seed_demo_corpus(root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    std::fs::write(
        root.join("tokenizer-drift.md"),
        "# Tokenizer drift investigation\n\
         \n\
         Observed 15% divergence between estimated and billed token counts \
         on long documents. Hypothesis: whitespace normalization differs \
         between tiktoken and the provider's internal tokenizer.\n\
         \n\
         TODO: collect 100 real requests and measure drift per model.",
    )?;
    std::fs::write(
        root.join("prompt-cache-notes.md"),
        "# Prompt cache notes\n\
         \n\
         Cache hit rate drops when the dynamic prefix exceeds 20% of the \
         full prompt. Moving volatile sections to the tail of the prompt \
         restored the hit rate to 94%.\n\
         \n\
         Related: unexpected cache breaks when refactoring shared system \
         prompt builders — worth adding a regression test.",
    )?;
    std::fs::write(
        root.join("agent-topology-ideas.md"),
        "# Agent topology ideas\n\
         \n\
         Three deployment profiles we keep seeing: pure API (no filesystem), \
         local knowledge worker (filesystem but no shell), and full coding \
         agent (filesystem + shell). The split maps cleanly onto Layer 1 / \
         Layer 2a / Layer 2b in the layering doc.\n\
         \n\
         TODO: document this more formally once the layer refactor lands.",
    )?;
    Ok(())
}

fn scan_workspace(root: &Path) -> std::io::Result<()> {
    eprintln!("\n--- workspace contents ---");
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            let size = std::fs::metadata(&path)?.len();
            eprintln!("  {} ({} bytes)", path.display(), size);
        }
    }
    Ok(())
}
