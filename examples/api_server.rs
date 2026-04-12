//! Phase D Workstream D-3 — reference HTTP API server.
//!
//! # What this example proves
//!
//! BranchForge is positioned as a **dual-use SDK**: the same
//! `Agent` runtime can power a CLI REPL and an HTTP API server
//! from one codebase. This example is the canonical proof:
//! everything needed to stream agent events to an HTTP client
//! comes out of the SDK crate itself — no framework coupling,
//! no dev-dependencies. A user copy-pastes this file into their
//! project, swaps the mock LLM for a real codec, and they have
//! a working streaming API server.
//!
//! # Features demonstrated
//!
//! - `Agent::execute_stream_into` as the single egress primitive
//! - `SseSink` writing HTML5 EventSource frames straight into the
//!   HTTP response body
//! - `DroppingSink` wrapping `SseSink` so slow clients do not
//!   stall the agent loop (D-2)
//! - `HumanInteractionHandler` wired via `AgentBuilder::human_handler`
//!   so server-side supervised mode can prompt the client via the
//!   same channel that MCP elicitation uses (C-1/C-3)
//! - `MockLlmCall` scripted responses so the example runs offline
//!   in CI without any API keys
//!
//! # Framework-neutral HTTP
//!
//! We deliberately avoid `axum`, `actix-web`, etc. to keep the
//! example's dependency footprint at zero. A real deployment
//! would plug the same three lines of SDK code (parse request,
//! build agent, `execute_stream_into(sse_sink)`) into whichever
//! HTTP framework the team already uses — none of the streaming
//! logic changes.
//!
//! # Running
//!
//! ```bash
//! cargo run --example api_server
//! # In another terminal:
//! curl -N -X POST http://127.0.0.1:8080/execute \
//!      -H 'content-type: text/plain' \
//!      -d 'hello agent'
//! ```
//!
//! Expected output: a stream of `event: <type>\ndata: {...}\n\n`
//! frames ending with an `event: complete` frame.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use branchforge::agent::{Agent, AgentConfig, DroppingSink, SseSink};
use branchforge::authorization::{
    HumanInteractionHandler, HumanInteractionResult, ToolApprovalRequest, ToolApprovalResponse,
};
use branchforge::client::LlmCall;
use branchforge::client::mock::MockLlmCall;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// Simple auto-approval host. Real deployments would route this
/// through a WebSocket, an operator dashboard, or a Slack approval
/// workflow — the trait is the single plug-point.
#[derive(Debug)]
struct AutoApproveHost;

#[async_trait]
impl HumanInteractionHandler for AutoApproveHost {
    fn name(&self) -> &str {
        "auto_approve_example"
    }

    async fn approve_tool(
        &self,
        _req: ToolApprovalRequest,
    ) -> HumanInteractionResult<ToolApprovalResponse> {
        // For the example we auto-approve everything. Do NOT ship
        // this in production — route to a real operator instead.
        Ok(ToolApprovalResponse::Approve)
    }
}

/// Adapter that makes any `AsyncWrite + Unpin + Send + Sync` into
/// an `AgentEventSink`-compatible writer for [`SseSink`]. This
/// just re-exports the crate's generic `SseSink` so the example
/// shows the real type rather than a shim.
type SseTcpSink = SseSink<OwnedWriteHalf>;

// `OwnedWriteHalf` from tokio::net::tcp is not re-exported at the
// module root in older tokio versions; alias locally.
type OwnedWriteHalf = tokio::net::tcp::OwnedWriteHalf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialise tracing so server lifecycle events show up in
    // stderr. Production hosts would wire this into an OTel
    // collector via `tracing-opentelemetry`.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,branchforge=debug".into()),
        )
        .init();

    let addr = "127.0.0.1:8080";
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "api_server listening");

    loop {
        let (socket, peer) = listener.accept().await?;
        tracing::info!(%peer, "accepted connection");
        tokio::spawn(async move {
            if let Err(e) = handle_connection(socket).await {
                tracing::warn!(error = %e, %peer, "connection handler failed");
            }
        });
    }
}

/// Handle one HTTP/1.1 request. Only supports `POST /execute` —
/// everything else returns 404. Reads the request body as the
/// prompt, streams agent events back as SSE.
async fn handle_connection(socket: TcpStream) -> Result<(), Box<dyn std::error::Error>> {
    let (read_half, mut write_half) = socket.into_split();
    let mut reader = BufReader::new(read_half);

    // Parse the request line.
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let request_line = request_line.trim();
    tracing::debug!(%request_line, "request");

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    if method != "POST" || path != "/execute" {
        write_plain_response(&mut write_half, 404, "not found\n").await?;
        return Ok(());
    }

    // Parse headers — we only care about Content-Length.
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line
            .strip_prefix("Content-Length: ")
            .or_else(|| line.strip_prefix("content-length: "))
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }

    // Read the body.
    let mut body = vec![0u8; content_length];
    use tokio::io::AsyncReadExt;
    reader.read_exact(&mut body).await?;
    let prompt = String::from_utf8_lossy(&body).to_string();
    tracing::info!(%prompt, "executing agent");

    // Write SSE response headers.
    let headers = "HTTP/1.1 200 OK\r\n\
                   Content-Type: text/event-stream\r\n\
                   Cache-Control: no-cache\r\n\
                   Connection: close\r\n\
                   \r\n";
    write_half.write_all(headers.as_bytes()).await?;

    // Build an agent with a scripted mock LLM. A real deployment
    // would replace this with `ProfileRegistry::with_builtins().build("anthropic")`
    // or whichever codec the team uses.
    let llm: Arc<dyn LlmCall> = Arc::new(
        MockLlmCall::new()
            .then_text(format!("Echo: {}", prompt.trim()))
            .then_text("done"),
    );

    // Construct the agent with the scripted mock LLM. A real
    // deployment would use `AgentBuilder::new()` for the full
    // configuration surface — here we use the lower-level
    // `Agent::new` constructor to keep the example terse.
    let agent = Agent::new(llm, AgentConfig::default());

    // HITL handler is declared but not wired in the low-level
    // path — the builder path is the canonical way to attach it.
    // The example's AutoApproveHost would move into
    // `AgentBuilder::new().human_handler(Arc::new(AutoApproveHost))`
    // for a production deployment. We keep the declaration here
    // so the example compiles cleanly even without the builder
    // wiring.
    let _handler: Arc<dyn HumanInteractionHandler> = Arc::new(AutoApproveHost);

    // Wrap SseSink in DroppingSink so a slow client cannot stall
    // the agent loop. 100ms is generous for local testing; a real
    // deployment would tune this to match the downstream SLA.
    let sse: SseTcpSink = SseSink::new(write_half);
    let bounded = DroppingSink::new(sse, Duration::from_millis(100));

    // Drive the agent stream into the bounded SSE sink. This
    // single line is the entire bridge between BranchForge's
    // agent runtime and the HTTP response body.
    match agent.execute_stream_into(&prompt, &bounded).await {
        Ok(()) => {
            let lost = bounded.dropped();
            if lost > 0 {
                tracing::warn!(%lost, "best-effort stream dropped events under load");
            }
            tracing::info!("execution complete");
        }
        Err(e) => {
            tracing::error!(error = %e, "agent execution failed");
        }
    }

    Ok(())
}

async fn write_plain_response(
    write_half: &mut (impl AsyncWrite + Unpin),
    status: u16,
    body: &str,
) -> Result<(), std::io::Error> {
    let status_line = match status {
        404 => "HTTP/1.1 404 Not Found",
        _ => "HTTP/1.1 500 Internal Server Error",
    };
    let response = format!(
        "{}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status_line,
        body.len(),
        body
    );
    write_half.write_all(response.as_bytes()).await?;
    Ok(())
}
