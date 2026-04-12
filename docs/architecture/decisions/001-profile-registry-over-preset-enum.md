# ADR-001: `ProfileRegistry` over `Preset` enum

**Status:** Accepted  •  **Date:** 2026-04-10

## Context

Early versions of the provider stack exposed preset combinations of
`(codec, transport, credential)` as a closed `Preset` enum with one
variant per supported combination (`Anthropic`, `OpenAi`, `Gemini`,
`VertexGemini`, `VertexAnthropic`, `BedrockAnthropic`, …). Adding a
new preset required editing the enum and every `match` that consumed
it. Third-party consumers could not register their own presets at
all — the set was closed at compile time.

At the same time, the 3-axis provider model (codec × transport ×
endpoint) was explicitly designed to let new combinations fall out
as free compositions. The closed enum was a direct violation of that
design invariant.

## Decision

Replace the closed `Preset` enum with an **open registry**:

```rust
pub struct ProfileRegistry { /* Arc<HashMap<String, ProviderProfile>> */ }
impl ProfileRegistry {
    pub fn register(&mut self, profile: ProviderProfile);
    pub fn get(&self, id: &str) -> Option<&ProviderProfile>;
}

pub struct ProviderProfile {
    pub id: &'static str,
    pub codec: Arc<dyn ModelCodec>,
    pub transport: Arc<dyn ModelTransport>,
    pub credential_hint: CredentialHint,
}
```

Builtins ship as one-shot registrations inside
`ProfileRegistry::with_builtins()`. Third parties register new
profiles via `registry.register(custom_profile)` and select them at
runtime via the `BRANCHFORGE_PROVIDER` environment variable. No new
code is required in the core to add a provider.

## Consequences

- **OCP**: adding a provider is additive. No core edits required.
- **Runtime registration**: test code and enterprise integrations can
  wire synthetic or tenant-specific profiles without patching the
  library.
- **Environment selection**: `BRANCHFORGE_PROVIDER=foo` looks up `foo`
  in the registry and fails cleanly if absent.
- **Cost**: one extra `HashMap` lookup per profile resolution and a
  slightly wider public API surface. Both are negligible.

## Alternatives rejected

- **Keep the enum, add a `Custom(CustomProfile)` variant.** Half-open,
  still requires every consumer to handle the `Custom` branch, and
  breaks `Serialize`/`Deserialize` round-trips.
- **`trait PresetFactory` with a global registry.** More ceremony,
  no additional flexibility, and forces profiles to be `'static`.
- **Feature-flag every provider.** Would double the feature matrix
  and still not allow runtime registration.
