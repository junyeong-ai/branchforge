# Cloud Providers

`claude-agent-rs` supports multiple Anthropic delivery paths through provider adapters.

## Supported Providers

- Anthropic direct API
- AWS Bedrock
- Google Vertex AI
- Azure AI Foundry

## Adapter Responsibilities

Each provider adapter is responsible for:

- endpoint construction
- request lowering
- authentication headers or tokens
- provider-specific model IDs
- provider-specific streaming behavior

## Practical Guidance

- Use Anthropic direct API when you want the closest match to Anthropic-native features.
- Use Bedrock, Vertex AI, or Foundry when deployment policy requires cloud-provider-native credentials or endpoints.
- Do not assume perfect provider capability parity for every Anthropic feature.

## Examples

```rust
use claude_agent::{Auth, Client};

let anthropic = Client::builder().auth(Auth::from_env()).build().await?;
let bedrock = Client::builder().auth(Auth::bedrock("us-east-1")).build().await?;
let vertex = Client::builder().auth(Auth::vertex("my-project", "us-central1")).build().await?;
let foundry = Client::builder().auth(Auth::foundry("my-resource")).build().await?;
```

## See Also

- `authentication.md`
- `architecture/provider-capabilities.md`
