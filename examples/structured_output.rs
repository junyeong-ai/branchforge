//! End-to-end structured outputs demo.
//!
//! Derives a JSON Schema from a Rust struct via `schemars`, sends a
//! request asking Claude to populate it, and parses the response back
//! into the original struct with zero handwritten glue.
//!
//! # Usage
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-... cargo run --example structured_output
//! ANTHROPIC_API_KEY=sk-ant-... cargo run --example structured_output -- "Extract: Jane, jane@example.com, wants Basic plan"
//! ```
//!
//! Override the provider via `BRANCHFORGE_PROVIDER`:
//!
//! ```bash
//! BRANCHFORGE_PROVIDER=openai cargo run --example structured_output
//! ```
//!
//! Override the model via `BRANCHFORGE_MODEL`:
//!
//! ```bash
//! BRANCHFORGE_MODEL=claude-haiku-4-5 cargo run --example structured_output
//! ```

use branchforge::client::preset::ProfileRegistry;
use branchforge::ir::{JsonSchemaSpec, Message, ModelRequest, ResponseFormat};
use schemars::JsonSchema;
use serde::Deserialize;

/// The shape we want Claude to populate. `schemars` derives the JSON
/// Schema; `serde` handles deserialization of the response.
#[derive(JsonSchema, Deserialize, Debug)]
#[allow(dead_code)]
struct Contact {
    /// Full name of the person.
    name: String,
    /// Email address.
    email: String,
    /// Which product tier they are interested in.
    plan_interest: String,
    /// Whether the person requested a demo.
    demo_requested: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Disambiguate rustls CryptoProvider — both ring and aws-lc-rs are
    // pulled in transitively by reqwest feature flags, so an explicit
    // choice is required before any TLS handshake.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let prompt = std::env::args().nth(1).unwrap_or_else(|| {
        "Extract the key information from this email: John Smith (john@example.com) \
         is interested in our Enterprise plan and wants to schedule a demo for \
         next Tuesday at 2pm."
            .to_string()
    });

    let registry = ProfileRegistry::with_builtins();
    let profile_id = std::env::var("BRANCHFORGE_PROVIDER").unwrap_or_else(|_| "anthropic".into());
    let profile = registry
        .get(&profile_id)
        .ok_or_else(|| format!("unknown profile `{profile_id}`"))?;
    let model = std::env::var("BRANCHFORGE_MODEL")
        .ok()
        .or_else(|| profile.default_model.clone())
        .unwrap_or_else(|| "DEFAULT".into());

    eprintln!("=> profile: {profile_id}");
    eprintln!("=> model:   {model}");
    eprintln!("=> prompt:  {prompt:?}");

    let client = registry.build(&profile_id)?;
    eprintln!(
        "=> codec:  {}, transport: {}",
        client.codec_id(),
        client.transport_id()
    );

    let request = ModelRequest::new(model, vec![Message::user(prompt)])
        .with_max_tokens(1024)
        .with_response_format(ResponseFormat::JsonSchema(
            JsonSchemaSpec::from_type::<Contact>()
                .with_description("Extract contact information from unstructured text"),
        ));

    let response = client.send(&request).await?;

    if !response.warnings.is_empty() {
        eprintln!("\n--- codec warnings ---");
        for w in &response.warnings {
            eprintln!("warning: {w:?}");
        }
    }

    eprintln!("\n--- raw text ---");
    println!("{}", response.text());

    eprintln!("\n--- parsed ---");
    let contact: Contact = response.json()?;
    println!("{contact:#?}");

    eprintln!("\n--- usage ---");
    eprintln!(
        "input: {}, output: {}",
        response.usage.input_tokens, response.usage.output_tokens
    );

    Ok(())
}
