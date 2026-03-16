# Cloud Providers

`branchforge` supports multiple AI providers through the `ProviderAdapter` trait.

## Supported Providers

| Provider | Feature Flag | API Format | Auth |
|----------|-------------|------------|------|
| Anthropic Direct | default | Messages API | API Key, OAuth |
| AWS Bedrock | `aws` | Converse API | SigV4, Bearer Token |
| Google Vertex AI | `gcp` | Messages API | ADC |
| Azure AI Foundry | `azure` | Messages API | Entra ID, API Key |
| OpenAI | `openai` | Chat Completions | API Key |
| Google Gemini | `gemini` | generateContent | API Key, OAuth |

## Adapter Responsibilities

Each provider adapter handles:

- endpoint construction and URL formatting
- request transformation (Anthropic format → provider format)
- response transformation (provider format → `ApiResponse`)
- streaming event parsing (`parse_stream_event`)
- authentication headers or signing
- structured output mapping (`output_format` → provider-native)
- credential refresh (where applicable)

## Examples

```rust
use branchforge::{Agent, Auth};

// Anthropic Direct
let agent = Agent::builder().auth(Auth::from_env()).await?.build().await?;

// AWS Bedrock (Converse API)
let agent = Agent::builder().auth(Auth::bedrock("us-east-1")).await?.build().await?;

// Google Vertex AI
let agent = Agent::builder().auth(Auth::vertex("project", "us-central1")).await?.build().await?;

// Azure AI Foundry
let agent = Agent::builder().auth(Auth::foundry("resource")).await?.build().await?;

// OpenAI (GPT-4o, o3, or compatible endpoints)
let agent = Agent::builder().auth(Auth::openai("sk-...")).await?.build().await?;

// Google Gemini
let agent = Agent::builder().auth(Auth::gemini("key")).await?.build().await?;
```

To select a provider via environment variable, set `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, `CLAUDE_CODE_USE_FOUNDRY`, `CLAUDE_CODE_USE_OPENAI`, or `CLAUDE_CODE_USE_GEMINI`.

## See Also

- [Authentication](authentication.md)
- [Provider Capabilities](architecture/provider-capabilities.md)
