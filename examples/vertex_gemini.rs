//! Headline acceptance check for the new codec/transport stack:
//! `vertex-gemini` end-to-end against a real GCP project.
//!
//! Usage:
//!
//! ```bash
//! cargo run --example vertex_gemini --features gcp -- "say pong"
//! ```
//!
//! Required environment:
//! - `GOOGLE_CLOUD_PROJECT` — your GCP project id (e.g. `oy-gemini-enterprise-prd`)
//! - `GOOGLE_CLOUD_LOCATION` — region (e.g. `us-central1`)
//! - GCP Application Default Credentials (`gcloud auth application-default login`)
//!
//! Optional:
//! - `BRANCHFORGE_MODEL` — model id (defaults to `gemini-2.5-flash`)
//! - `GOOGLE_CLOUD_QUOTA_PROJECT` — overrides the auto-injected
//!   `x-goog-user-project` header (defaults to `GOOGLE_CLOUD_PROJECT`).

#[cfg(feature = "gcp")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use branchforge::client::preset::Preset;
    use branchforge::ir::{Message, ModelRequest, ModelSettings};

    // Disambiguate rustls CryptoProvider — both ring and aws-lc-rs are
    // pulled in transitively, so an explicit choice is required.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Reply with the single word: pong".to_string());
    let model = std::env::var("BRANCHFORGE_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".into());

    let project = std::env::var("GOOGLE_CLOUD_PROJECT").unwrap_or_default();
    let location = std::env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| "us-central1".into());
    eprintln!("=> preset: vertex-gemini");
    eprintln!("=> project: {project}");
    eprintln!("=> location: {location}");
    eprintln!("=> model: {model}");
    eprintln!("=> prompt: {prompt:?}");

    let client = Preset::VertexGemini.build_from_env().await?;
    eprintln!(
        "=> codec: {}, transport: {}",
        client.codec_id(),
        client.transport_id()
    );

    let request = ModelRequest {
        model,
        messages: vec![Message::user(prompt)],
        settings: ModelSettings::default().with_max_output_tokens(64),
        ..ModelRequest::new("ignored", vec![])
    };

    let response = client.send(&request).await?;
    println!("\n--- response ---");
    println!("{}", response.text());
    println!("\n--- usage ---");
    println!(
        "input={} output={} reasoning={:?}",
        response.usage.input_tokens, response.usage.output_tokens, response.usage.reasoning_tokens
    );
    if !response.warnings.is_empty() {
        println!("\n--- warnings ---");
        for w in &response.warnings {
            println!("{w:?}");
        }
    }
    Ok(())
}

#[cfg(not(feature = "gcp"))]
fn main() {
    eprintln!("This example requires `--features gcp`");
    std::process::exit(2);
}
