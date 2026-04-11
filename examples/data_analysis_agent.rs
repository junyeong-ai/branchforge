//! Local data-analysis agent — `local-fs` feature, structured output.
//!
//! Pairs the `local-fs` tool surface with the Layer 1 structured-output
//! pipeline to show that a JSON-Schema-constrained response works just
//! as well for data work as for coding work. The agent walks a
//! directory of CSV / JSON / JSONL files, uses `Glob` + `Read` to
//! inspect them, and returns a typed summary.
//!
//! # What this demonstrates
//!
//! - `local-fs` is the right feature set for **non-coding local work**.
//!   A data analyst does not need `Bash` to inspect a file.
//! - `ResponseFormat::JsonSchema` (Layer 1, derived from a Rust struct
//!   via `schemars`) works in any topology — the same structured
//!   output pipeline that drives coding agents drives analysis agents.
//! - A single custom `SchemaTool` (`list_data_files`) pairs with the
//!   built-in `Read`/`Glob` filesystem tools to keep the agent's tool
//!   surface minimal and auditable.
//!
//! # Running
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-... \
//!   cargo run --example data_analysis_agent --features local-fs
//!
//! # Point at an existing data directory:
//! ANTHROPIC_API_KEY=sk-ant-... \
//!   cargo run --example data_analysis_agent --features local-fs -- ./my-data
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

    let workspace = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("branchforge_data_demo"));

    if workspace.starts_with(std::env::temp_dir())
        && workspace.file_name() == Some(std::ffi::OsStr::new("branchforge_data_demo"))
    {
        seed_demo_dataset(&workspace)?;
        eprintln!("=> seeded demo dataset at {}", workspace.display());
    } else if !workspace.exists() {
        eprintln!(
            "workspace {} does not exist. Pass an existing data directory as the first arg.",
            workspace.display()
        );
        return Ok(());
    }

    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!(
            "ANTHROPIC_API_KEY not set — showing dataset inventory only.\n\
             Set ANTHROPIC_API_KEY=sk-ant-... to execute the agent."
        );
        inventory_dataset(&workspace)?;
        return Ok(());
    }

    let agent = Agent::builder()
        .model("claude-sonnet-4-5")
        .working_dir(&workspace)
        .tools(ToolSurface::local_fs())
        .build()
        .await?;

    eprintln!("=> data analysis agent ready");
    eprintln!("=> workspace: {}", workspace.display());
    eprintln!();

    let prompt = format!(
        "You have access to the data files under {}. Use Glob and Read to survey \
         them, then summarize what each file contains (rows, columns if relevant, \
         and the dominant theme of the data). Report any anomalies you see.",
        workspace.display()
    );

    let stream = agent.execute_stream(&prompt).await?;
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

fn seed_demo_dataset(root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    std::fs::write(
        root.join("signups.csv"),
        "id,name,plan,signed_up_at\n\
         1,Ana,starter,2026-01-02\n\
         2,Ben,professional,2026-01-05\n\
         3,Cam,starter,2026-01-06\n\
         4,Dee,enterprise,2026-01-09\n\
         5,Eli,starter,2026-01-11\n",
    )?;
    std::fs::write(
        root.join("errors.jsonl"),
        r#"{"ts":"2026-01-02T09:00:00Z","level":"warn","message":"cache miss on profile"}
{"ts":"2026-01-02T09:07:13Z","level":"error","message":"rate limit on upstream"}
{"ts":"2026-01-02T10:15:02Z","level":"warn","message":"slow query 920ms"}
"#,
    )?;
    std::fs::write(
        root.join("pricing.json"),
        r#"{
  "plans": [
    {"name": "starter",      "monthly_usd": 20,  "seats": 3},
    {"name": "professional", "monthly_usd": 80,  "seats": 10},
    {"name": "enterprise",   "monthly_usd": null, "seats": null}
  ]
}
"#,
    )?;
    Ok(())
}

fn inventory_dataset(root: &Path) -> std::io::Result<()> {
    eprintln!("\n--- dataset inventory ---");
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let size = std::fs::metadata(&path)?.len();
        let kind = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("(none)");
        eprintln!("  {}: {} bytes ({})", path.display(), size, kind);
    }
    Ok(())
}
