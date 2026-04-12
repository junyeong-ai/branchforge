# axis · provider-graph

## Defect patterns

### Provider stack
- **Axis collapse** — a `ModelCodec` doing auth / URL resolution, or a `ModelTransport` doing wire encoding
- **IR leakage** — `serde_json::Value` inside `src/ir/*` beyond schema-policy metadata
- **Capability dishonesty** — codec advertising `json_schema: Native` but `encode_response_format` path skips the emit
- **Schema pipeline bypass** — codec rolling its own `prepare_schema` instead of the shared helper
- **Streaming bypass** — codec decoding stream chunks outside `decode_stream_chunk` / `decode_eventstream_frame`
- **Transport error substring** — any `body.contains("...Exception")` for classification (invariant #6)
- **Pinned transport missing** — codec compatible with multiple transports without test coverage in `codec_contract.rs`

### Graph / session
- **Fork race** — any `fork_session` path bypassing `with_session_lock`
- **Tool-pair split** — compaction or archival removing `ToolCall` without the paired `ToolResult`
- **Token drift** — raw `u64`/`u32` arithmetic on tokens outside `TokenCount` newtype
- **Event replay non-determinism** — event decode depending on `HashMap` iteration order
- **Persistence schema drift** — backend writing events without `SessionSchemaVersion` header

## Rules to load additionally

- `.claude/rules/client.md`
- `.claude/rules/ir.md`
- `.claude/rules/schema.md`
- `.claude/rules/graph-session.md`

## Stopping criterion

All bullets visited. Codec × Transport matrix in `tests/codec_contract.rs` reviewed for pinning coverage.
