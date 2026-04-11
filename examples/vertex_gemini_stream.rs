//! Streaming end-to-end check for `vertex-gemini` against a real GCP
//! project.
//!
//! Usage:
//!
//! ```bash
//! cargo run --example vertex_gemini_stream --features gcp -- "tell me a 1-sentence joke"
//! ```
//!
//! Same env requirements as `vertex_gemini.rs`.

#[cfg(feature = "gcp")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use branchforge::client::preset::ProfileRegistry;
    use branchforge::ir::{Message, ModelRequest, ModelSettings, ModelStreamChunk};
    use futures::StreamExt;

    let _ = rustls::crypto::ring::default_provider().install_default();

    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Reply with five words about the ocean.".to_string());
    let model = std::env::var("BRANCHFORGE_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".into());

    eprintln!("=> preset: vertex-gemini (streaming)");
    eprintln!(
        "=> project: {}",
        std::env::var("GOOGLE_CLOUD_PROJECT").unwrap_or_default()
    );
    eprintln!(
        "=> location: {}",
        std::env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_default()
    );
    eprintln!("=> model: {model}");
    eprintln!("=> prompt: {prompt:?}");

    let client = ProfileRegistry::with_builtins().build("vertex-gemini")?;
    eprintln!(
        "=> codec: {}, transport: {}",
        client.codec_id(),
        client.transport_id()
    );

    let request = ModelRequest {
        model,
        messages: vec![Message::user(prompt)],
        settings: ModelSettings::default().with_max_output_tokens(128),
        ..ModelRequest::new("ignored", vec![])
    };

    let mut stream = client
        .send_stream(&request, tokio_util::sync::CancellationToken::new())
        .await?;
    println!("\n--- streaming output ---");
    let mut text = String::new();
    let mut start_id = String::new();
    let mut finish_seen = false;
    let mut final_input = 0u64;
    let mut final_output = 0u64;
    let mut final_reasoning: Option<u64> = None;
    while let Some(chunk) = stream.next().await {
        match chunk? {
            ModelStreamChunk::MessageStart { id, model, .. } => {
                start_id = id.clone();
                eprintln!("[start id={id} model={model}]");
            }
            ModelStreamChunk::TextDelta { text: t, .. } => {
                use std::io::Write;
                print!("{t}");
                std::io::stdout().flush().ok();
                text.push_str(&t);
            }
            ModelStreamChunk::ReasoningDelta { text: t, .. } => {
                eprint!("[think:{t}]");
            }
            ModelStreamChunk::Finish { reason, usage } => {
                println!();
                eprintln!("[finish reason={reason:?}]");
                final_input = usage.input_tokens;
                final_output = usage.output_tokens;
                final_reasoning = usage.reasoning_tokens;
                finish_seen = true;
            }
            ModelStreamChunk::Warning(w) => eprintln!("[warning {w:?}]"),
            ModelStreamChunk::Error { kind, message } => eprintln!("[error {kind}: {message}]"),
            _ => {}
        }
    }
    println!();
    println!("--- summary ---");
    println!("start_id={start_id} finish={finish_seen}");
    println!(
        "text_len={} input={final_input} output={final_output} reasoning={final_reasoning:?}",
        text.len()
    );
    Ok(())
}

#[cfg(not(feature = "gcp"))]
fn main() {
    eprintln!("This example requires `--features gcp`");
    std::process::exit(2);
}
