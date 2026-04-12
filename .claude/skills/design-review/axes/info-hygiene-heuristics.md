# axis · info-hygiene-heuristics

Combined because both axes concern signal-to-noise — one in the outbound payload to the model, one in internal classification rules.

## Info hygiene — defects in outbound payload to LLM

- `BASE_SYSTEM_PROMPT` always-on regardless of active tool set
- Tool description duplicated as both `ToolDefinition.description` **and** free-text block in `static_context.tool_summary()`
- `RuleIndex::build_priority_summary()` serializing inactive rules
- `ToolOutput::Empty` → `Text("")` empty string crossing the wire
- `ExecutionMetadata` 8 optional fields missing `#[serde(skip_serializing_if = "Option::is_none")]`
- `ProviderOptions` serialized with all fields `None`
- Dynamic rules marked uncached regardless of content hash (cache break on every turn)
- Boundary marker emitted as an empty `Role::Boundary` block instead of an implicit split
- `SessionMessage.environment` field allocated even when `None`
- `ReasoningSignature` field always present even for non-Anthropic providers

## Heuristic defects — internal classification false positives

- `body.contains("ThrottlingException" | "ServiceUnavailableException" | "AccessDeniedException")` in transport error classification (invariant #6)
- Regex boundary bugs — `\bsrm\b` matches `premium_srm_tool`; `[^a-z]reboot\b` matches `2reboot`; `\bmkfs(\.[a-z0-9]+)?\s` matches stray `./mkfs.ext4` in tool output
- `regex` + `tree-sitter` dual-system in `bash/parser.rs` (invariant #5)
- `first-match-wins` on multi-change events (cache-break schema changed → only first tool reported)
- Identity fallback `contains("opus"|"sonnet"|"haiku")` in `ModelRegistry::resolve` (invariant #7)
- Magic numeric threshold without Config field — `const DEFAULT_COMPACT_THRESHOLD: f32 = 0.8`, jitter `0.15 * (2*random - 1)`, `MAX_STRUCTURED_OUTPUT_RETRIES: u32 = 3`, `MAX_WATERMARK_WALKBACK: usize = 256`, `MAX_EXECUTION_LOG_SIZE: usize = 1000` (invariant #8)
- Retry jitter factor without named config field or distribution doc
- String-based system/user boundary markers that user input could spoof
- Marker-path project-root detection (`MARKERS.iter().filter(...).count() > 1`) instead of canonical root (`git rev-parse --show-toplevel` or `.claude/` discovery)

## Rules to load additionally

- `.claude/rules/client.md`
- `.claude/rules/schema.md`
- `.claude/rules/security.md`
- `.claude/rules/events.md`

## Stopping criterion

- For info hygiene: every call site building `ModelRequest` and every `System*` block constructor audited.
- For heuristics: grep `body.contains`, `\.contains\(.*:?(?i)(opus|sonnet|haiku|exception)`, `Regex::new`, `const \w+: (usize|f32|f64|Duration) =` inside `src/` — each hit triaged against invariant #6/#7/#8.
