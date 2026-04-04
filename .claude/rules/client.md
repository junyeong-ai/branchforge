---
paths:
  - "src/client/**"
---

# Client Module Rules

- `ProviderAdapter` trait abstracts all LLM providers. Each adapter (Anthropic, Bedrock, Vertex, Foundry, OpenAI, Gemini) is self-contained.
- `reqwest::Client` is created once in `Client` and passed to adapters — adapters never own the main HTTP client.
- `RetryPolicy` handles transient errors (429, 5xx) with exponential backoff on the same model. `FallbackConfig` switches to a different model after retries exhaust. These are separate concerns.
- `ProviderAdapter::profile()` returns `ProviderProfile` declaring provider capabilities. Default is all-Full; adapters override for unsupported features.
- `ensure_fresh_credentials()` runs before every streaming request. `with_auth_retry()` catches 401 and retries after refresh.
- Credential refresh is delegated to `CredentialProvider` via `AnthropicAdapter.credential_provider`.
- Base URLs are configurable via `ANTHROPIC_BASE_URL`, `OPENAI_BASE_URL`, `GEMINI_BASE_URL` environment variables.
