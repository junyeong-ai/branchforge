//! Phase D Workstream E-1 — prompt cache-break classifier.
//!
//! # Why
//!
//! Every commercial LLM provider now offers prompt caching: the
//! first request with a given prefix pays the full input cost,
//! subsequent requests reuse the cached prefix at a steep
//! discount (Anthropic: 10% of input rate for reads, 25% premium
//! for writes). In practice, long-horizon agent sessions with
//! many iterations depend on cache hits for economical operation
//! — a session that breaks cache on every turn can be 5–10×
//! more expensive than one that preserves it.
//!
//! When cache breaks, operators need to know **why**. The
//! existing "unexpected break" warning in `provider_client.rs`
//! says "we expected a cache hit but got 0 cached_input_tokens"
//! — useful, but it does not classify the root cause. Is it a
//! new model id? A system-prompt tweak? A tool definition that
//! changed between iterations? A TTL expiry? Each answer points
//! at a different fix.
//!
//! This module implements a **two-phase telemetry** pattern:
//!
//! 1. **Before** the request, snapshot the inputs that affect
//!    the cache prefix: model id, system prompt hash, sorted tool
//!    schema hashes, reasoning effort.
//! 2. **After** the response, compare the new snapshot against
//!    the previous one. When cache_read_tokens dropped
//!    unexpectedly, classify the cause by diffing the snapshots.
//!
//! The classifier is **pure** — no I/O, no allocation beyond
//! the hash table. The agent loop maintains a single
//! [`CacheBreakBaseline`] across iterations and calls
//! [`classify`] on each response.
//!
//! # Related patterns
//!
//! This is the first instance of the two-phase telemetry pattern
//! in BranchForge. Future uses (first-token latency regression,
//! cost-per-successful-call, retry-after-backoff trigger
//! classification) should follow the same shape: a baseline type
//! with `snapshot` / `compare` methods and a pure classifier
//! returning a typed cause enum.

#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::ir::{ModelRequest, ModelResponse};

/// Hash a byte slice to a 64-bit digest using the standard
/// library's `DefaultHasher`. BLAKE3 would give stronger
/// collision resistance but we do not need cryptographic
/// guarantees here — we only care whether two byte sequences
/// differ. `u64` collisions for unrelated inputs are orders of
/// magnitude rarer than a human operator noticing the difference
/// in a dashboard, and keeping std's hasher means no new crate
/// dependency.
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Root cause of a prompt cache break, as classified by comparing
/// two consecutive [`CacheBreakBaseline`] snapshots.
///
/// Categories are chosen so they map 1:1 to "what did the caller
/// change?" — if an operator sees `ModelChanged` on a dashboard,
/// they know to look at the per-turn model override path; if
/// they see `ToolSchemaChanged { tools: tool }`, they know one specific
/// tool's definition is flapping between turns.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheBreakCause {
    /// The request targeted a different model than the previous
    /// successful cache hit. Cache prefixes are keyed by model
    /// so any change invalidates the whole cache line.
    ModelChanged { previous: String, current: String },
    /// The rendered system prompt's hash differs from the
    /// previous turn's. A single byte flip — including whitespace
    /// normalisation, locale formatting, dynamic rule injection
    /// — invalidates the cache.
    SystemPromptChanged,
    /// One or more tool schemas changed. Most often this is
    /// because a dynamic tool catalogue (MCP progressive
    /// disclosure, on-demand skill loading) added or removed a
    /// tool between turns. All changed tools are listed so
    /// operators can diagnose the root cause without guessing.
    ToolSchemaChanged { tools: Vec<String> },
    /// Reasoning effort setting changed (e.g. `low` → `high`).
    /// Anthropic and OpenAI both key their cache on this field.
    ReasoningEffortChanged,
    /// Nothing observable changed in the baseline but the cache
    /// hit rate dropped to zero. Usually TTL expiry (Anthropic
    /// default 5 minutes, OpenAI 1 hour) or an upstream cache
    /// flush caused by a rolling deploy.
    TtlOrUpstream,
}

impl CacheBreakCause {
    /// Low-cardinality category label for OTel span attributes and
    /// metrics tagging. Compile-time constant per variant.
    pub fn category(&self) -> &'static str {
        match self {
            Self::ModelChanged { .. } => "model_changed",
            Self::SystemPromptChanged => "system_prompt_changed",
            Self::ToolSchemaChanged { .. } => "tool_schema_changed",
            Self::ReasoningEffortChanged => "reasoning_effort_changed",
            Self::TtlOrUpstream => "ttl_or_upstream",
        }
    }
}

impl crate::decision::DecisionReason for CacheBreakCause {
    fn category(&self) -> &'static str {
        Self::category(self)
    }

    fn summary(&self) -> String {
        match self {
            Self::ModelChanged { previous, current } => {
                format!("model changed: {previous} → {current}")
            }
            Self::SystemPromptChanged => "system prompt hash changed".into(),
            Self::ToolSchemaChanged { tools } => format!("tool schema changed: {}", tools.join(", ")),
            Self::ReasoningEffortChanged => "reasoning effort changed".into(),
            Self::TtlOrUpstream => "TTL expired or upstream cache flushed".into(),
        }
    }
}

/// Per-tool schema hash. Keys are sanitised tool names; values
/// are `u64` digests of the serialised tool definition (see
/// [`hash_bytes`] for rationale).
type ToolHashMap = BTreeMap<String, u64>;

/// Frozen point-in-time snapshot of the inputs that affect a
/// provider's prompt cache prefix. Cheap to compute (three hash
/// calls + a tree map insert per tool) and compact to store
/// across async boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheBreakBaseline {
    pub model: String,
    pub system_prompt_hash: u64,
    pub tool_hashes: ToolHashMap,
    pub reasoning_effort: Option<String>,
    /// `cache_read_tokens` reported by the previous response.
    /// Used to detect "baseline unchanged but cache_read dropped"
    /// (→ TTL/upstream).
    pub last_cache_read_tokens: u64,
}

impl CacheBreakBaseline {
    /// Snapshot the cache-affecting inputs from a request about
    /// to be sent. The `previous_cache_read_tokens` argument
    /// carries the last successful cache-read count forward so
    /// [`classify`] can detect the TTL-expiry case.
    pub fn from_request(request: &ModelRequest, previous_cache_read_tokens: u64) -> Self {
        let system_prompt_hash = match &request.system {
            Some(system) => hash_bytes(system.flatten().as_bytes()),
            None => hash_bytes(b""),
        };

        let mut tool_hashes = ToolHashMap::new();
        for tool in &request.tools {
            let mut parts: Vec<u8> = tool.name.as_bytes().to_vec();
            // serde_json::to_vec on the tool's JSON schema is
            // deterministic per serde's emit order; that's good
            // enough for "did this tool's definition change?".
            if let Ok(bytes) = serde_json::to_vec(&tool.parameters) {
                parts.extend_from_slice(&bytes);
            }
            if let Some(desc) = &tool.description {
                parts.extend_from_slice(desc.as_bytes());
            }
            tool_hashes.insert(sanitize_tool_name(&tool.name), hash_bytes(&parts));
        }

        let reasoning_effort = request
            .settings
            .reasoning
            .as_ref()
            .map(|r| format!("{:?}", r.effort));

        Self {
            model: request.model.clone(),
            system_prompt_hash,
            tool_hashes,
            reasoning_effort,
            last_cache_read_tokens: previous_cache_read_tokens,
        }
    }
}

/// Classify the root cause of a cache break between two
/// consecutive requests on the same session.
///
/// Returns `None` when no cache break occurred — either the
/// current response showed a healthy cache read, or the request
/// carried no cache markers to begin with (so there was nothing
/// to break).
///
/// When a break IS detected, the function diffs the two
/// baselines and returns the most specific cause. Order matters:
/// if both the model and a tool schema changed, the reported
/// cause is `ModelChanged` because that is the larger
/// invalidation blast radius and fixing it subsumes the tool
/// change. Classification order: model → reasoning → system prompt → tool → TTL.
pub fn classify(
    previous: Option<&CacheBreakBaseline>,
    current: &CacheBreakBaseline,
    response: &ModelResponse,
    request_had_cache_markers: bool,
) -> Option<CacheBreakCause> {
    let cache_read = response.usage.cached_input_tokens.unwrap_or(0);
    if cache_read > 0 {
        // Cache hit observed — nothing to explain.
        return None;
    }
    if !request_had_cache_markers {
        // No markers means the caller never asked for caching.
        // A zero `cache_read` is expected, not a break.
        return None;
    }

    let Some(prev) = previous else {
        // First request on a new session — there was no prior
        // cache line to break. Report nothing so dashboards don't
        // flag cold starts as "broken".
        return None;
    };

    if prev.model != current.model {
        return Some(CacheBreakCause::ModelChanged {
            previous: prev.model.clone(),
            current: current.model.clone(),
        });
    }
    if prev.reasoning_effort != current.reasoning_effort {
        return Some(CacheBreakCause::ReasoningEffortChanged);
    }
    if prev.system_prompt_hash != current.system_prompt_hash {
        return Some(CacheBreakCause::SystemPromptChanged);
    }
    // Tool schema drift: collect ALL changed/added/removed tools.
    let mut changed_tools = Vec::new();
    for (name, hash) in &current.tool_hashes {
        match prev.tool_hashes.get(name) {
            Some(prev_hash) if prev_hash == hash => {}
            _ => changed_tools.push(name.clone()),
        }
    }
    for name in prev.tool_hashes.keys() {
        if !current.tool_hashes.contains_key(name) {
            changed_tools.push(name.clone());
        }
    }
    if !changed_tools.is_empty() {
        return Some(CacheBreakCause::ToolSchemaChanged { tools: changed_tools });
    }

    // Everything structural is the same; the prior turn had a
    // non-zero cache read and this turn has zero. Most likely
    // cause is TTL expiry (5 min default for Anthropic) or an
    // upstream flush.
    if prev.last_cache_read_tokens > 0 {
        return Some(CacheBreakCause::TtlOrUpstream);
    }

    // Previous request also had zero cache read and nothing
    // changed — this is not a fresh break, it's a persistent
    // cold state that already reported itself. Return None to
    // avoid duplicate events.
    None
}

/// Strip MCP dynamic prefixes from tool names before hashing
/// them for the per-tool schema key. MCP tools are named
/// `mcp__<server>__<tool>` but the `<server>` token can change
/// between reconnects for the same logical server, which would
/// otherwise cause a false `ToolSchemaChanged` event on every
/// MCP reconnect. Stripping to `mcp__<tool>` keeps the key
/// stable across reconnects.
fn sanitize_tool_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("mcp__")
        && let Some((_server, tool)) = rest.split_once("__")
    {
        return format!("mcp__{tool}");
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Message, ModelRequest, ModelResponse, SystemPrompt, ToolDefinition, Usage};

    fn req_with_system(model: &str, system: &str) -> ModelRequest {
        let mut r = ModelRequest::new(model, vec![Message::user("hi")]);
        r.system = Some(SystemPrompt::Text(system.into()));
        r
    }

    fn resp_with_cache(cached: u64) -> ModelResponse {
        let mut r = ModelResponse::from_text("ok");
        r.usage = Usage {
            input_tokens: 100,
            output_tokens: 10,
            cached_input_tokens: Some(cached),
            ..Default::default()
        };
        r
    }

    #[test]
    fn healthy_cache_read_is_not_a_break() {
        let req = req_with_system("claude-sonnet-4-5", "you are helpful");
        let baseline = CacheBreakBaseline::from_request(&req, 0);
        let resp = resp_with_cache(900);
        assert_eq!(classify(None, &baseline, &resp, true), None);
    }

    #[test]
    fn no_cache_markers_no_classification() {
        let req = req_with_system("m", "s");
        let baseline = CacheBreakBaseline::from_request(&req, 0);
        let resp = resp_with_cache(0);
        assert_eq!(classify(None, &baseline, &resp, false), None);
    }

    #[test]
    fn first_request_on_session_no_classification() {
        // Previous baseline is None — nothing to diff against.
        let req = req_with_system("m", "s");
        let baseline = CacheBreakBaseline::from_request(&req, 0);
        let resp = resp_with_cache(0);
        assert_eq!(classify(None, &baseline, &resp, true), None);
    }

    #[test]
    fn model_changed_is_detected() {
        let prev_req = req_with_system("claude-sonnet-4-5", "s");
        let prev = CacheBreakBaseline::from_request(&prev_req, 900);

        let curr_req = req_with_system("claude-opus-4-6", "s");
        let curr = CacheBreakBaseline::from_request(&curr_req, 900);

        let resp = resp_with_cache(0);
        let cause = classify(Some(&prev), &curr, &resp, true).unwrap();
        match cause {
            CacheBreakCause::ModelChanged { previous, current } => {
                assert_eq!(previous, "claude-sonnet-4-5");
                assert_eq!(current, "claude-opus-4-6");
            }
            other => panic!("expected ModelChanged, got {other:?}"),
        }
    }

    #[test]
    fn system_prompt_changed_is_detected() {
        let prev_req = req_with_system("m", "first prompt");
        let prev = CacheBreakBaseline::from_request(&prev_req, 900);
        let curr_req = req_with_system("m", "second prompt");
        let curr = CacheBreakBaseline::from_request(&curr_req, 900);

        let resp = resp_with_cache(0);
        assert_eq!(
            classify(Some(&prev), &curr, &resp, true),
            Some(CacheBreakCause::SystemPromptChanged)
        );
    }

    fn tool(name: &str, parameters: serde_json::Value) -> ToolDefinition {
        let mut t = ToolDefinition::new(name, parameters);
        t.description = Some(format!("test tool {name}"));
        t
    }

    #[test]
    fn tool_schema_changed_is_detected() {
        let mut prev_req = req_with_system("m", "s");
        prev_req.tools.push(tool(
            "search",
            serde_json::json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        ));
        let prev = CacheBreakBaseline::from_request(&prev_req, 900);

        let mut curr_req = req_with_system("m", "s");
        curr_req.tools.push(tool(
            "search",
            serde_json::json!({
                "type": "object",
                "properties": {"q": {"type": "string"}, "k": {"type": "integer"}},
                "required": ["q", "k"]
            }),
        ));
        let curr = CacheBreakBaseline::from_request(&curr_req, 900);

        let resp = resp_with_cache(0);
        match classify(Some(&prev), &curr, &resp, true).unwrap() {
            CacheBreakCause::ToolSchemaChanged { tools } => assert!(tools.contains(&"search".to_string()), "expected {tools:?} to contain search"),
            other => panic!("expected ToolSchemaChanged, got {other:?}"),
        }
    }

    #[test]
    fn added_tool_counts_as_schema_change() {
        let prev_req = req_with_system("m", "s");
        let prev = CacheBreakBaseline::from_request(&prev_req, 900);

        let mut curr_req = req_with_system("m", "s");
        curr_req.tools.push(ToolDefinition::new(
            "brand_new",
            serde_json::json!({"type": "object"}),
        ));
        let curr = CacheBreakBaseline::from_request(&curr_req, 900);

        let resp = resp_with_cache(0);
        match classify(Some(&prev), &curr, &resp, true).unwrap() {
            CacheBreakCause::ToolSchemaChanged { tools } => assert!(tools.contains(&"brand_new".to_string()), "expected {tools:?} to contain brand_new"),
            other => panic!("expected ToolSchemaChanged (new tool), got {other:?}"),
        }
    }

    #[test]
    fn removed_tool_counts_as_schema_change() {
        let mut prev_req = req_with_system("m", "s");
        prev_req.tools.push(ToolDefinition::new(
            "old_tool",
            serde_json::json!({"type": "object"}),
        ));
        let prev = CacheBreakBaseline::from_request(&prev_req, 900);

        let curr_req = req_with_system("m", "s");
        let curr = CacheBreakBaseline::from_request(&curr_req, 900);

        let resp = resp_with_cache(0);
        match classify(Some(&prev), &curr, &resp, true).unwrap() {
            CacheBreakCause::ToolSchemaChanged { tools } => assert!(tools.contains(&"old_tool".to_string()), "expected {tools:?} to contain old_tool"),
            other => panic!("expected ToolSchemaChanged (removed tool), got {other:?}"),
        }
    }

    #[test]
    fn unchanged_baseline_with_prior_hit_maps_to_ttl() {
        let req = req_with_system("m", "s");
        let prev = CacheBreakBaseline::from_request(&req, 900);
        let curr = CacheBreakBaseline::from_request(&req, 900);

        let resp = resp_with_cache(0);
        assert_eq!(
            classify(Some(&prev), &curr, &resp, true),
            Some(CacheBreakCause::TtlOrUpstream)
        );
    }

    #[test]
    fn cold_baseline_with_zero_read_reports_nothing() {
        // Previous turn also had 0 cache read — this is a
        // persistent cold state, not a fresh break. We do NOT
        // want duplicate events on every turn.
        let req = req_with_system("m", "s");
        let prev = CacheBreakBaseline::from_request(&req, 0);
        let curr = CacheBreakBaseline::from_request(&req, 0);

        let resp = resp_with_cache(0);
        assert_eq!(classify(Some(&prev), &curr, &resp, true), None);
    }

    #[test]
    fn mcp_server_name_is_sanitized_in_tool_hash_key() {
        assert_eq!(sanitize_tool_name("mcp__server1__search"), "mcp__search");
        assert_eq!(
            sanitize_tool_name("mcp__another_server__query"),
            "mcp__query"
        );
        assert_eq!(sanitize_tool_name("Read"), "Read");
    }

    #[test]
    fn decision_reason_surface() {
        use crate::decision::DecisionReason;
        let cause = CacheBreakCause::ModelChanged {
            previous: "a".into(),
            current: "b".into(),
        };
        assert_eq!(<_ as DecisionReason>::category(&cause), "model_changed");
        let summary = <_ as DecisionReason>::summary(&cause);
        assert!(summary.contains("a"));
        assert!(summary.contains("b"));
    }
}
