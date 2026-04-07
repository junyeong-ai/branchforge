---
paths:
  - "src/client/**"
  - "src/ir/**"
---

# Client Module Rules

## New stack (preferred for new code)

- **3-axis design**: `ModelCodec` (wire format encoder/decoder) × `ModelTransport` (endpoint + auth) × `EndpointShape` (URL pattern bridge). Never collapse these axes — the orthogonality is what makes new provider combinations fall out for free.
- `ProviderClient` composes one `Arc<dyn ModelCodec>` + one `Arc<dyn ModelTransport>`. Composition is validated at construction time (`pinned_transport`, `supports_codec`). Invalid pairings return `Error::InvalidComposition`.
- **5 codecs**: `AnthropicMessagesCodec`, `OpenAiChatCodec`, `OpenAiResponsesCodec`, `GeminiGenerateCodec`, `BedrockConverseCodec`. Codecs are pure (no HTTP, no auth, no state except `StreamDecodeState`).
- **4 transports**: `DirectTransport` (API key / bearer / query param), `VertexTransport` (GCP ADC, publisher routing by codec id), `BedrockTransport` (SigV4 / bearer), `FoundryTransport` (Azure Entra / api-key).
- `EndpointShape` is a `const`-friendly descriptor each codec exposes. Transports consume it via `resolve_endpoint(shape, model, mode) -> Endpoint`. The codec never knows the URL; the transport never knows the body shape.
- `ProviderCapabilities` defaults to `Unsupported` on all axes. Each codec returns `&'static ProviderCapabilities` (prefer `const`). Consumer code reads capabilities — never assumes feature parity.
- `ModelWarning` entries surface lossy encodes, unsupported settings, and dropped provider options. Warnings flow into `ModelResponse::warnings` (unary) and `ModelStreamChunk::Warning` (streaming).
- `Preset` enum maps named presets (`anthropic`, `openai`, `vertex-gemini`, `bedrock`, …) to `(codec, transport)` pairs with `build_from_env()` factories. `BRANCHFORGE_PROVIDER` env var selects the preset.
- `authorize(req, body_bytes: &[u8])` signature carries the serialised body so SigV4 transports can sign it. Non-SigV4 transports ignore `body_bytes`.

## Legacy stack (during migration)

- `ProviderAdapter` trait + `Client` + `ClientBuilder` are the old monolithic surface. They still work and are used by the agent runtime.
- `Client::with_provider_client(pc)` bridges the old Client to the new stack: `send_with_auth_retry` converts `CreateMessageRequest → ir::ModelRequest`, dispatches through `ProviderClient`, and converts `ir::ModelResponse → ApiResponse` back. Conversion lives in `src/ir/compat.rs`.
- `AgentBuilder::provider_client(pc)` wires this bridge automatically so agents use the new stack with zero runtime code changes.
- `RetryPolicy`, `FallbackConfig`, `CircuitBreaker` remain Client-level and apply regardless of which backend is active.
- `ensure_fresh_credentials()` / `with_auth_retry()` only apply to the old adapter path. The new `ProviderClient` handles credential refresh via `ModelTransport::refresh()`.

## Environment variables

- `BRANCHFORGE_PROVIDER` — selects a named preset (`anthropic`, `openai`, `openai-chat`, `gemini`, `vertex-gemini`, `vertex-anthropic`, `bedrock`, `foundry-anthropic`).
- `BRANCHFORGE_MODEL` / `BRANCHFORGE_SMALL_MODEL` / `BRANCHFORGE_REASONING_MODEL` — model overrides.
- Standard vendor vars are read verbatim: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, `AWS_REGION`, `AZURE_AI_RESOURCE`, etc.
