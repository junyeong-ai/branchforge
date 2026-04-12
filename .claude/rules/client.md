---
paths:
  - "src/client/**"
---

# Client Module Rules

## Provider stack (3-axis)

- **Never collapse** `ModelCodec` × `ModelTransport` × `EndpointShape`. The orthogonality is what makes `vertex-gemini`, `vertex-anthropic`, `bedrock-converse`, and `foundry-anthropic` fall out as free compositions.
- `ProviderClient::new(codec, transport, auth_preamble)` validates the pairing via `codec.pinned_transport()` and `transport.supports_codec()`. Invalid pairings return `Error::InvalidComposition` at construction — never at send time. The optional `auth_preamble` is auto-injected as the first system block on every request.
- Codecs are **pure**: no HTTP, no auth, no state except `StreamDecodeState`. Transports are **stateful**: auth caches, token refresh, TLS client.
- `EndpointShape` is a `const`-friendly descriptor each codec exposes. Transports consume it via `resolve_endpoint(shape, model, mode) -> Endpoint`. The codec never knows the URL; the transport never knows the body shape.

## Codecs and transports

- **5 codecs**: `AnthropicMessagesCodec`, `OpenAiChatCodec`, `OpenAiResponsesCodec`, `GeminiGenerateCodec`, `BedrockConverseCodec`. All five ship native structured outputs via `SchemaPolicy` — see `.claude/rules/schema.md`.
- **4 transports**: `DirectTransport` (API key / bearer / query param + optional `CredentialProvider` for OAuth refresh), `VertexTransport` (GCP ADC + publisher routing by `codec_id`), `BedrockTransport` (SigV4 / bearer), `FoundryTransport` (Azure Entra / api-key).
- Each codec holds a `const SCHEMA_POLICY: SchemaPolicy = SchemaPolicy::X()` next to its other constants. The corresponding `encode_response_format` helper is codec-private.
- `authorize(req, body_bytes: &[u8])` carries the serialised body so SigV4 transports can sign it. Non-SigV4 transports ignore `body_bytes`.

## Capability honesty (enforced by tests)

- `ProviderCapabilities` defaults to `Unsupported` on every axis. Consumer code reads capabilities — never assume feature parity.
- If a codec declares `json_schema: Native` it **must** emit a wire-level schema reference in `encode_request`. `tests/codec_contract.rs::capability_honesty_response_format` walks all 5 codecs × (JsonSchema + JsonObject) and fails the build if a codec lies.
- Emulated variants must emit a `ModelWarning::CapabilityEmulated` with a `response_format.*`-prefixed capability string. The matrix's assertion uses `starts_with` so new variants are forward-compatible.

## Warnings and errors

- `ModelWarning::LossyEncode { field, reason }` — the codec transformed or dropped a user field.
- `ModelWarning::CapabilityEmulated { capability }` — the whole capability is emulated.
- `ModelWarning::UnsupportedSetting { setting, codec }` — a setting was dropped entirely.
- Encode-time warnings flow into `ModelResponse::warnings` via `ProviderClient::send` (`decoded.warnings.extend(encoded.warnings)`). Streaming routes them into `ModelStreamChunk::Warning`.
- `ModelTransport::classify_error(status, body)` is the single OCP extension point for vendor-specific HTTP failures (Vertex quota project, Bedrock throttling, Foundry Entra). The central `provider_client::classify_response_error` only delegates.

## LlmCall, LlmClient, and decorators

- `LlmCall` is the trait for all model invocations (`send` / `send_stream` / `capabilities` / `codec_id`). `ProviderClient` implements it; decorator wrappers (`RetryingClient`, `FallingBackClient`, `CircuitBrokenClient`) compose around any `Arc<dyn LlmCall>` and forward `capabilities()` / `codec_id()` to the inner client.
- `LlmClient::from_auth(auth)` is the standard entry point for consumers that need `Arc<dyn LlmCall>` without Agent overhead (custom loops, structured output extraction, RAG). `LlmClientBuilder` exposes retry policy and HTTP client customisation. Both `LlmClientBuilder` and `AgentBuilder` delegate to the same `build_direct_anthropic()` — no duplication.

## ProfileRegistry-based bootstrapping

- `ProfileRegistry` maps profile ids (`anthropic`, `openai`, `openai-chat`, `gemini`, `vertex-gemini`, `vertex-anthropic`, `bedrock`, `foundry-anthropic`, plus OpenAI-compatible third-party) to `(codec, transport_builder, credential_hint)` recipes. Cloud profiles are `cfg`-gated behind their feature flags. User-registered profiles are accepted at runtime via `registry.register(ProviderProfile { ... })`.
- `BRANCHFORGE_PROVIDER` env var selects the profile at runtime; `ProfileRegistry::build(&name)` is the programmatic entry point.

## Environment variables

- `BRANCHFORGE_PROVIDER` — preset selector (see above).
- `BRANCHFORGE_MODEL` / `BRANCHFORGE_SMALL_MODEL` / `BRANCHFORGE_REASONING_MODEL` — per-role model overrides.
- `BRANCHFORGE_PRICING_<MODEL>_INPUT` / `..._OUTPUT` / `..._CACHE_READ` / `..._CACHE_WRITE` — per-model pricing overrides layered on top of `with_anthropic_models()` / `with_openai_models()` / `with_gemini_models()` defaults.
- Vendor vars are read verbatim: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, `AWS_REGION`, `AZURE_AI_RESOURCE`.
