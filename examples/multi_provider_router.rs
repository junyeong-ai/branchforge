//! Multi-provider capability router — pure core, zero filesystem.
//!
//! Demonstrates that BranchForge's **3-axis provider stack** (Codec ×
//! Transport × EndpointShape) is entirely a Layer 1 facility: the five
//! built-in codecs (Anthropic Messages, OpenAI Chat, OpenAI Responses,
//! Gemini generateContent, Bedrock Converse) all compile into the core
//! runtime with no feature gates, and their `ProviderCapabilities` are
//! exposed as static constants the caller can query without spinning up
//! any network connection.
//!
//! The example walks the five codecs, prints each one's capability
//! declaration, and — if the matching credentials are available in the
//! environment — sends the same IR `ModelRequest` through each one to
//! compare responses. Missing credentials are reported and skipped
//! rather than crashing, so the example is useful even in a pure
//! smoke-test context with no API keys at all.
//!
//! # What this demonstrates
//!
//! - `ProviderCapabilities` is a Layer 1 query surface — application
//!   code can inspect what each provider supports (streaming, tool
//!   calling, JSON schema, reasoning, prompt caching, vision, batch)
//!   without importing anything from `local-fs` or `coding-tools`.
//! - A single IR `ModelRequest` is portable across codecs; any
//!   provider-specific translation happens inside the codec via
//!   `prepare_schema` / `LossyEncode` warnings.
//! - The codec contract matrix (`tests/codec_contract.rs`) guarantees
//!   that every codec handles the same neutral IR input; this example
//!   exercises that guarantee at runtime.
//!
//! # Running
//!
//! ```bash
//! # Capability matrix only (no API keys required):
//! cargo run --example multi_provider_router
//!
//! # Live execution on whichever providers have credentials in env:
//! ANTHROPIC_API_KEY=sk-ant-... \
//!   OPENAI_API_KEY=sk-... \
//!   GEMINI_API_KEY=... \
//!   cargo run --example multi_provider_router -- "Write a haiku about Rust."
//! ```
//!
//! Required features: `anthropic-direct` (the default — no `--features`
//! flag needed). Gemini, Vertex, and Bedrock transports remain behind
//! their own feature gates; this example uses `Preset::from_id` so it
//! gracefully skips over providers whose transport is not compiled in.

use branchforge::client::codec::{
    AnthropicMessagesCodec, BedrockConverseCodec, GeminiGenerateCodec, ModelCodec, OpenAiChatCodec,
    OpenAiResponsesCodec,
};
use branchforge::client::preset::ProfileRegistry;
use branchforge::ir::{Message, ModelRequest};

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Write a haiku about the Rust programming language.".to_string());

    // =========================================================================
    // 1. Capability matrix — pure Layer 1, no network, no credentials needed.
    // =========================================================================
    eprintln!("== provider capability matrix ==\n");
    eprintln!(
        "{:<20} {:<10} {:<10} {:<12} {:<10} {:<10}",
        "codec", "streaming", "tools", "json_schema", "reasoning", "batch"
    );
    eprintln!("{}", "-".repeat(82));

    let anthropic = AnthropicMessagesCodec::new();
    let openai_chat = OpenAiChatCodec::new();
    let openai_responses = OpenAiResponsesCodec::new();
    let gemini = GeminiGenerateCodec::new();
    let bedrock = BedrockConverseCodec::new();

    let codecs: Vec<(&str, &dyn ModelCodec)> = vec![
        ("anthropic-messages", &anthropic),
        ("openai-chat", &openai_chat),
        ("openai-responses", &openai_responses),
        ("gemini-generate", &gemini),
        ("bedrock-converse", &bedrock),
    ];

    for (name, codec) in &codecs {
        let caps = codec.capabilities();
        eprintln!(
            "{:<20} {:<10} {:<10} {:<12} {:<10} {:<10}",
            name,
            render_support(caps.streaming),
            render_support(caps.tool_calls.mode),
            render_support(caps.structured_output.json_schema),
            render_support(caps.reasoning.mode),
            render_support(caps.batch),
        );
    }
    eprintln!();

    // =========================================================================
    // 2. Optional live execution — only runs where credentials exist.
    //
    // The ProfileRegistry replaces the old closed-set Preset enum:
    // we walk every registered profile id, try to build it, and
    // skip the ones whose credentials are missing. Adding a new
    // vendor is now a single `registry.register(...)` call (or a
    // builtin profile in `client::preset`) — no enum variant or
    // example match arm to update.
    // =========================================================================
    let request = ModelRequest::new("DEFAULT", vec![Message::user(&prompt)]).with_max_tokens(256);
    let registry = ProfileRegistry::with_builtins();

    eprintln!("== live comparison (skipping providers without credentials) ==\n");

    for id in registry.ids() {
        let profile = match registry.get(id) {
            Some(p) => p,
            None => continue,
        };
        match registry.build(id) {
            Ok(client) => {
                let mut req = request.clone();
                if let Some(default_model) = &profile.default_model {
                    req.model = default_model.clone();
                }
                match client.send(&req).await {
                    Ok(response) => {
                        let text = response.text();
                        let snippet: String = text.chars().take(80).collect();
                        eprintln!(
                            "+ {:<20} {} tokens · {}",
                            id,
                            response.usage.input_tokens + response.usage.output_tokens,
                            snippet.trim()
                        );
                        if !response.warnings.is_empty() {
                            for w in &response.warnings {
                                eprintln!("    warning: {w:?}");
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("! {:<20} {err}", id);
                    }
                }
            }
            Err(err) => {
                // Most common cause: missing credential env var. The
                // registry's error message already names the var.
                eprintln!("- {:<20} skipped: {err}", id);
            }
        }
    }

    Ok(())
}

fn render_support<S: std::fmt::Debug>(support: S) -> String {
    // ProviderCapabilities uses the `Support` enum (Native/Emulated/
    // Unsupported) or equivalent domain-specific enums. We render them
    // with the standard Debug representation for a compact table row.
    let s = format!("{support:?}");
    // Truncate overly long variants for table alignment.
    if s.len() > 10 {
        format!("{}…", &s[..9])
    } else {
        s
    }
}
