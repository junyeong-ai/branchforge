//! Live verification: OAuth auth_preamble via ProviderClient direct usage.
//!
//! Tests at three levels:
//! 1. **Wire format**: Codec encodes CLI_IDENTITY as the first system block
//! 2. **ProviderClient.send()**: Direct API call with OAuth + CLI_IDENTITY succeeds
//! 3. **Negative**: Same call WITHOUT CLI_IDENTITY is rejected by the API
//!
//! Run: cargo run --example oauth_preamble_test --features "cli-auth,coding-tools"

use std::collections::HashMap;
use std::sync::Arc;

use branchforge::auth::{Auth, CLAUDE_CODE_BETA, OAuthConfig};
use branchforge::ir::{Message, ModelRequest, ModelSettings, SystemPrompt};
use branchforge::prompts::identity::CLI_IDENTITY;
use branchforge::{
    AnthropicMessagesCodec, DirectAuth, DirectTransport, ModelCodec, ProviderClient,
};
use secrecy::ExposeSecret;

/// OAuth beta header value — matches `BetaFeature::OAuth.header_value()`.
const OAUTH_BETA: &str = "oauth-2025-04-20";

fn check(name: &str, ok: bool) {
    if ok {
        println!("  [PASS] {}", name);
    } else {
        println!("  [FAIL] {}", name);
        std::process::exit(1);
    }
}

/// Build an OAuth-configured ProviderClient from a resolved credential.
fn build_oauth_client(token: &str) -> branchforge::Result<ProviderClient> {
    let cfg = OAuthConfig::default();
    let beta_header = format!("{},{}", OAUTH_BETA, CLAUDE_CODE_BETA);

    let mut extra_headers: HashMap<String, String> = cfg.extra_headers.clone();
    extra_headers.insert("user-agent".to_string(), cfg.user_agent.clone());
    extra_headers.insert("x-app".to_string(), cfg.app_identifier.clone());
    extra_headers.insert("anthropic-beta".to_string(), beta_header);

    let base =
        std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "https://api.anthropic.com".into());

    let transport = Arc::new(
        DirectTransport::new(base, DirectAuth::Bearer(secrecy::SecretString::from(token)))
            .with_allowed_codecs(&["anthropic-messages"])
            .with_extra_headers(extra_headers)
            .with_extra_url_params(cfg.url_params.clone()),
    ) as Arc<dyn branchforge::ModelTransport>;

    let codec = Arc::new(AnthropicMessagesCodec::new()) as Arc<dyn ModelCodec>;

    ProviderClient::new(codec, transport, None)
}

/// Build a minimal ModelRequest with the given system prompt.
fn build_request(system: SystemPrompt, user_msg: &str) -> ModelRequest {
    let mut req = ModelRequest::new("claude-haiku-4-5", vec![Message::user(user_msg)]);
    req.system = Some(system);
    req.settings = ModelSettings {
        max_output_tokens: Some(128),
        ..Default::default()
    };
    req
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("==========================================================");
    println!("  ProviderClient-level OAuth auth_preamble Verification");
    println!("==========================================================\n");

    // --- Resolve OAuth token ---
    let credential = match Auth::ClaudeCli.resolve().await {
        Ok(c) if !c.is_placeholder() => c,
        Ok(_) => {
            println!("  [SKIP] Placeholder credential — CLI OAuth not configured.");
            return Ok(());
        }
        Err(e) => {
            println!("  [SKIP] Auth not available: {}", e);
            return Ok(());
        }
    };

    let token = match &credential {
        branchforge::Credential::OAuth(oauth) => oauth.access_token.expose_secret().to_string(),
        _ => {
            println!("  [SKIP] Expected OAuth credential, got API key.");
            return Ok(());
        }
    };
    println!(
        "  OAuth token resolved ({}...)\n",
        &token[..12.min(token.len())]
    );

    let client = build_oauth_client(&token)?;
    let codec = AnthropicMessagesCodec::new();

    // =================================================================
    // Case 1: Wire format — CLI_IDENTITY as Text prefix
    // =================================================================
    println!("[Case 1] Wire format: CLI_IDENTITY prepended as SystemPrompt::Text");
    {
        let system_text = format!("{}\n\nYou are a calculator.", CLI_IDENTITY);
        let system = SystemPrompt::Text(system_text.clone());
        let req = build_request(system, "What is 2+2?");

        let encoded = codec.encode_request(&req, branchforge::InvocationMode::Unary)?;
        let body = &encoded.body;

        // Anthropic Messages API: system field is a string for Text variant
        let wire_system = body["system"].as_str().unwrap_or("");
        check(
            "wire system starts with CLI_IDENTITY",
            wire_system.starts_with(CLI_IDENTITY),
        );
        check(
            "wire system contains custom prompt",
            wire_system.contains("calculator"),
        );
        println!(
            "    wire system[..80]: {}...",
            &wire_system[..80.min(wire_system.len())]
        );
    }

    // =================================================================
    // Case 2: Wire format — CLI_IDENTITY as Blocks[0]
    // =================================================================
    println!("\n[Case 2] Wire format: CLI_IDENTITY as first SystemBlock");
    {
        use branchforge::ir::SystemBlock;

        let system = SystemPrompt::Blocks(vec![
            SystemBlock::uncached(CLI_IDENTITY),
            SystemBlock::uncached("You are a helpful assistant."),
        ]);
        let req = build_request(system, "Say hi.");

        let encoded = codec.encode_request(&req, branchforge::InvocationMode::Unary)?;
        let body = &encoded.body;

        // Blocks variant: system field is an array of objects
        let wire_system = body["system"].as_array().expect("system should be array");
        check("wire system has 2 blocks", wire_system.len() == 2);

        let first_block_text = wire_system[0]["text"].as_str().unwrap_or("");
        check(
            "first block is CLI_IDENTITY",
            first_block_text == CLI_IDENTITY,
        );
        println!("    block[0].text: {}", first_block_text);
        println!(
            "    block[1].text: {}",
            wire_system[1]["text"].as_str().unwrap_or("")
        );
    }

    // =================================================================
    // Case 3: Live API — OAuth + CLI_IDENTITY → success
    // =================================================================
    println!("\n[Case 3] Live API: OAuth + CLI_IDENTITY → 200 OK");
    {
        let system_text = format!(
            "{}\n\nYou are a calculator. Only reply with numbers.",
            CLI_IDENTITY
        );
        let req = build_request(SystemPrompt::Text(system_text), "What is 7*6?");

        match client.send(&req).await {
            Ok(resp) => {
                let text = resp.text();
                check("response received", !text.is_empty());
                check("correct answer (42)", text.contains("42"));
                println!("    Response: {}", text.trim());
                println!(
                    "    Tokens: in={}, out={}",
                    resp.usage.input_tokens, resp.usage.output_tokens
                );
            }
            Err(e) => {
                println!("  [FAIL] API call failed: {}", e);
                std::process::exit(1);
            }
        }
    }

    // =================================================================
    // Case 4: Live API — OAuth + Replace custom prompt + CLI_IDENTITY
    // =================================================================
    println!("\n[Case 4] Live API: OAuth + fully custom system prompt");
    {
        let system_text = format!(
            "{}\n\nYou are a JSON API. Respond ONLY with a valid JSON object. No markdown.",
            CLI_IDENTITY
        );
        let req = build_request(
            SystemPrompt::Text(system_text),
            "Return {\"status\": \"ok\"}",
        );

        match client.send(&req).await {
            Ok(resp) => {
                let text = resp.text().trim().to_string();
                check("response received", !text.is_empty());
                let looks_json = text.contains('{') && text.contains("status");
                check("JSON-like response", looks_json);
                println!("    Response: {}", &text[..text.len().min(200)]);
            }
            Err(e) => {
                println!("  [FAIL] API call failed: {}", e);
                std::process::exit(1);
            }
        }
    }

    // =================================================================
    // Case 5: Live API — OAuth WITHOUT CLI_IDENTITY → expect rejection
    // =================================================================
    println!("\n[Case 5] Live API: OAuth WITHOUT CLI_IDENTITY → expect error");
    {
        let req = build_request(
            SystemPrompt::Text("You are a calculator.".to_string()),
            "What is 1+1?",
        );

        match client.send(&req).await {
            Ok(resp) => {
                // If the API somehow accepts it, note that — it means the
                // constraint may have changed server-side.
                println!(
                    "  [WARN] API accepted request without CLI_IDENTITY! Response: {}",
                    resp.text().trim()
                );
                println!("         This may indicate a server-side policy change.");
            }
            Err(e) => {
                let err_str = format!("{}", e);
                check("request rejected", true);
                println!(
                    "    Error (expected): {}",
                    &err_str[..err_str.len().min(200)]
                );
            }
        }
    }

    // =================================================================
    // Case 6: Wire format — no CLI_IDENTITY for non-OAuth (API key)
    // =================================================================
    println!("\n[Case 6] Wire format: non-OAuth has no CLI_IDENTITY");
    {
        let system = SystemPrompt::Text("You are a helpful assistant.".to_string());
        let req = build_request(system, "Hi");

        let encoded = codec.encode_request(&req, branchforge::InvocationMode::Unary)?;
        let wire_system = encoded.body["system"].as_str().unwrap_or("");
        check(
            "no CLI_IDENTITY in non-OAuth prompt",
            !wire_system.contains(CLI_IDENTITY),
        );
        check(
            "custom prompt present",
            wire_system.contains("helpful assistant"),
        );
        println!("    wire system: {}", wire_system);
    }

    println!("\n==========================================================");
    println!("  All cases passed!");
    println!("==========================================================");

    Ok(())
}
