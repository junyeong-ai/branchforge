---
paths:
  - "src/client/**"
  - "src/ir/**"
---

# Client Module Rules

## Provider stack architecture

- **3-axis design**: `ModelCodec` (wire format encoder/decoder) × `ModelTransport` (endpoint + auth) × `EndpointShape` (URL pattern bridge). Never collapse these axes — the orthogonality is what makes new provider combinations fall out for free.
- `ProviderClient` composes one `Arc<dyn ModelCodec>` + one `Arc<dyn ModelTransport>`. Composition is validated at construction time (`pinned_transport`, `supports_codec`). Invalid pairings return `Error::InvalidComposition`.
- **5 codecs**: `AnthropicMessagesCodec`, `OpenAiChatCodec`, `OpenAiResponsesCodec`, `GeminiGenerateCodec`, `BedrockConverseCodec`. Codecs are pure (no HTTP, no auth, no state except `StreamDecodeState`).
- **4 transports**: `DirectTransport` (API key / bearer / query param + optional `CredentialProvider` for OAuth refresh), `VertexTransport` (GCP ADC, publisher routing by codec id), `BedrockTransport` (SigV4 / bearer), `FoundryTransport` (Azure Entra / api-key).
- `EndpointShape` is a `const`-friendly descriptor each codec exposes. Transports consume it via `resolve_endpoint(shape, model, mode) -> Endpoint`. The codec never knows the URL; the transport never knows the body shape.
- `ProviderCapabilities` defaults to `Unsupported` on all axes. Each codec returns `&'static ProviderCapabilities` (prefer `const`). Consumer code reads capabilities — never assumes feature parity. **Capability declarations must be honest**: if a codec advertises `json_schema: Native` it must actually emit `response_format` in `encode_request`.
- `ModelWarning` entries surface lossy encodes, unsupported settings, and dropped provider options. Warnings flow into `ModelResponse::warnings` (unary) and `ModelStreamChunk::Warning` (streaming).
- `Preset` enum maps named presets (`anthropic`, `openai`, `vertex-gemini`, `bedrock`, …) to `(codec, transport)` pairs with `build_from_env()` factories. `BRANCHFORGE_PROVIDER` env var selects the preset.
- `authorize(req, body_bytes: &[u8])` signature carries the serialised body so SigV4 transports can sign it. Non-SigV4 transports ignore `body_bytes`.
- `ModelTransport::classify_error` is the single OCP-friendly extension point for vendor-specific HTTP error patterns (Vertex quota project, Bedrock throttling, …). The central `provider_client::classify_response_error` only delegates — adding a new transport never requires editing the central function.

## Consumer surface

- `LlmCall` is the trait the agent runtime uses for all model invocations (`send` / `send_stream`). `ProviderClient` implements it, and decorator wrappers (`RetryingClient`, `FallingBackClient`, `CircuitBrokenClient`) compose around any `Arc<dyn LlmCall>`.
- The agent runtime holds `Arc<dyn LlmCall>` directly. There is no monolithic `Client` type any more — that legacy adapter layer was removed.

## Environment variables

- `BRANCHFORGE_PROVIDER` — selects a named preset (`anthropic`, `openai`, `openai-chat`, `gemini`, `vertex-gemini`, `vertex-anthropic`, `bedrock`, `foundry-anthropic`).
- `BRANCHFORGE_MODEL` / `BRANCHFORGE_SMALL_MODEL` / `BRANCHFORGE_REASONING_MODEL` — model overrides.
- `BRANCHFORGE_PRICING_<MODEL>_INPUT` / `..._OUTPUT` / `..._CACHE_READ` / `..._CACHE_WRITE` — generic per-model pricing overrides, applied on top of `with_anthropic_models()` / `with_openai_models()` / `with_gemini_models()` defaults.
- Standard vendor vars are read verbatim: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, `AWS_REGION`, `AZURE_AI_RESOURCE`, etc.
