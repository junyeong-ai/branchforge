//! Server-side Tools Verification (WebSearch, WebFetch)
//!
//! Tests Anthropic's server-side tools that execute on the API side.
//! Requires OAuth authentication.
//!
//! Run: cargo run --example server_tools

use branchforge::{Agent, Auth, ToolSurface, authorization::ToolPolicy};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

static PASSED: AtomicUsize = AtomicUsize::new(0);
static FAILED: AtomicUsize = AtomicUsize::new(0);

macro_rules! test {
    ($name:expr, $body:expr) => {{
        let start = Instant::now();
        match $body {
            Ok(()) => {
                println!("  [PASS] {} ({:.2?})", $name, start.elapsed());
                PASSED.fetch_add(1, Ordering::SeqCst);
            }
            Err(e) => {
                println!("  [FAIL] {} - {}", $name, e);
                FAILED.fetch_add(1, Ordering::SeqCst);
            }
        }
    }};
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("warn").init();

    println!("\n========================================================================");
    println!("              Server-side Tools Verification                            ");
    println!("========================================================================\n");

    test!("WebSearch", test_web_search().await);
    test!("WebFetch", test_web_fetch().await);

    let (passed, failed) = (PASSED.load(Ordering::SeqCst), FAILED.load(Ordering::SeqCst));
    println!("\n========================================================================");
    println!("  RESULTS: {} passed, {} failed", passed, failed);
    println!("========================================================================\n");

    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

async fn test_web_search() -> Result<(), String> {
    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .map_err(|e| format!("Auth: {}", e))?
        .tools(ToolSurface::all())
        .authorization_policy(ToolPolicy::permissive())
        .working_dir(".")
        .build()
        .await
        .map_err(|e| format!("Build: {}", e))?;

    let result = agent
        .execute("Search the web for the latest Rust programming news in 2025. Give one headline.")
        .await
        .map_err(|e| format!("Execute: {}", e))?;

    let web_search_count = result
        .usage
        .server_tool_invocations
        .as_ref()
        .and_then(|s| s.web_search)
        .unwrap_or(0);
    if web_search_count > 0 {
        println!("    WebSearch used {web_search_count} time(s)");
        Ok(())
    } else {
        Err("WebSearch not invoked".into())
    }
}

async fn test_web_fetch() -> Result<(), String> {
    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .map_err(|e| format!("Auth: {}", e))?
        .tools(ToolSurface::all())
        .authorization_policy(ToolPolicy::permissive())
        .working_dir(".")
        .build()
        .await
        .map_err(|e| format!("Build: {}", e))?;

    let result = agent
        .execute("Fetch https://httpbin.org/json and tell me what it contains.")
        .await
        .map_err(|e| format!("Execute: {}", e))?;

    let web_fetch_count = result
        .usage
        .server_tool_invocations
        .as_ref()
        .and_then(|s| s.web_fetch)
        .unwrap_or(0);
    if web_fetch_count > 0 {
        println!("    WebFetch used {web_fetch_count} time(s)");
        Ok(())
    } else {
        Err("WebFetch not invoked".into())
    }
}
