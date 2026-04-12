---
paths:
  - "src/auth/**"
---

# Auth Module Rules

- `CredentialProvider` trait is the extension point: `resolve()`, `refresh()`, `supports_refresh()`.
- `ClaudeCliProvider` refreshes OAuth tokens directly via HTTP (not CLI subprocess). Requires `cli-auth` feature.
- `refresh` and `storage` modules are gated behind `cli-auth` feature.
- Token refresh uses `tokio::sync::Mutex` to serialize concurrent attempts (prevents refresh_token rotation races).
- Storage write is atomic: temp file + rename for file, `-U` flag for keychain.
- `OAuthCredential.expires_at` may be in milliseconds (Claude Code CLI) or seconds (OAuth2 standard) — `expires_at_datetime()` auto-detects.
- `Credential::auth_preamble()` is the SSoT for protocol-required system prompt preambles. OAuth credentials return `Some(CLI_IDENTITY)`; all others return `None`. The returned value is passed to `ProviderClient::new()` which auto-injects it as the first `SystemBlock` on every `send()` / `send_stream()` call.
- Error messages follow the pattern: `"<what happened>. <what to do>"`.
- Environment overrides: `BRANCHFORGE_TOKEN_URL`, `BRANCHFORGE_OAUTH_CLIENT_ID`.
