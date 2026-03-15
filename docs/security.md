# Security

The runtime includes multiple safety layers for local tool execution.

## Main Components

- `SecureFs` for TOCTOU-safe file access
- `BashAnalyzer` for command analysis
- OS sandbox support through platform-specific sandboxing
- authorization policies for tool-level control

## Secure File Access

File access uses a secure file layer designed to avoid unsafe path traversal and symlink surprises.

## Bash Analysis

Shell commands are analyzed before execution so the runtime can block or flag dangerous patterns.

## Sandboxing

- Linux: Landlock
- macOS: Seatbelt

See `sandbox.md` for sandbox-specific details.

## Related Guides

- `authorization.md`
- `sandbox.md`
