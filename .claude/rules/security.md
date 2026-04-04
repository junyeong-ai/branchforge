---
paths:
  - "src/security/**"
---

# Security Module Rules

- `SecureFs` uses `openat()` + `O_NOFOLLOW` for TOCTOU-safe file operations. Never use `std::fs` directly for user-facing paths.
- `SecureFs::try_permissive()` returns `Result` — never panic in SDK code.
- `BashAnalyzer` uses tree-sitter AST parsing + regex patterns. Patterns are developer-config (not user input), so regex injection is low risk.
- `Sandbox`: Landlock on Linux, Seatbelt on macOS. Graceful degradation if unavailable.
- `ResourceLimits`: RLIMIT_NPROC is disabled by default (per-UID, not per-process). RLIMIT_DATA is Linux-only.
