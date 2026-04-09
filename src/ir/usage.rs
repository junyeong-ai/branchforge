//! Token usage and accounting.

use serde::{Deserialize, Serialize};

/// Token usage for a single model call.
///
/// All counts are `u64` because 1M-context Anthropic prompts already approach
/// `u32` limits when summed across a session.
///
/// The fields are typed (rather than stuffing everything into a JSON blob) so
/// that the budget tracker in `src/budget/` can read them directly without
/// stringly-indexing. The `raw` field is reserved for diagnostic display only
/// and **must not be read by pricing logic**.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input (prompt) tokens billed at the standard input rate.
    pub input_tokens: u64,
    /// Output (completion) tokens billed at the standard output rate.
    pub output_tokens: u64,
    /// Input tokens served from cache. Anthropic
    /// `cache_read_input_tokens`, OpenAI `cached_tokens`. Always cheaper than
    /// `input_tokens` at the pricing layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    /// Tokens written to cache on this call. Anthropic
    /// `cache_creation_input_tokens`. Typically billed at a premium over
    /// `input_tokens`. `None` for providers without write-through cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u64>,
    /// Reasoning / thinking tokens billed separately from `output_tokens`.
    /// OpenAI o-series, Gemini 2.5 thinking, Anthropic extended thinking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// Audio input tokens (for multi-modal voice inputs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_input_tokens: Option<u64>,
    /// Audio output tokens (for TTS-style outputs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_output_tokens: Option<u64>,
    /// Server-side tool invocations charged separately (Anthropic web search,
    /// OpenAI Responses built-in tools, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_tool_invocations: Option<ServerToolInvocations>,
    /// Verbatim provider usage payload, kept for debugging only.
    /// **Pricing logic must never read this field.**
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

impl Usage {
    /// Sum of all billable token counts that flow into output cost.
    pub fn total_output_tokens(&self) -> u64 {
        self.output_tokens
            + self.reasoning_tokens.unwrap_or(0)
            + self.audio_output_tokens.unwrap_or(0)
    }

    /// Sum of all billable token counts that flow into input cost. Note this
    /// includes `cached_input_tokens`; pricing logic discounts that subset.
    pub fn total_input_tokens(&self) -> u64 {
        self.input_tokens + self.audio_input_tokens.unwrap_or(0)
    }

    /// Add another usage record into this one. Used for accumulating
    /// streaming `PartialUsage` deltas into a final [`Usage`].
    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        add_opt(&mut self.cached_input_tokens, other.cached_input_tokens);
        add_opt(&mut self.cache_creation_tokens, other.cache_creation_tokens);
        add_opt(&mut self.reasoning_tokens, other.reasoning_tokens);
        add_opt(&mut self.audio_input_tokens, other.audio_input_tokens);
        add_opt(&mut self.audio_output_tokens, other.audio_output_tokens);
        if let Some(other_invocations) = &other.server_tool_invocations {
            self.server_tool_invocations
                .get_or_insert_with(ServerToolInvocations::default)
                .add(other_invocations);
        }
        // `raw` is intentionally not merged.
        debug_assert!(
            self.cached_input_tokens.unwrap_or(0) <= self.input_tokens,
            "Usage invariant violated: cached_input_tokens must be a subset of input_tokens \
             (cached={}, input={}). A codec is reporting cached tokens as a separate counter \
             instead of as a subset — fix the codec.",
            self.cached_input_tokens.unwrap_or(0),
            self.input_tokens,
        );
    }

    /// Input tokens that are billed at the *standard* (non-cached) input rate.
    /// Equivalent to `input_tokens - cached_input_tokens.unwrap_or(0)`.
    ///
    /// Pricing logic should call this rather than reading `input_tokens`
    /// directly, otherwise cached tokens will be billed at the full rate.
    pub fn billable_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_sub(self.cached_input_tokens.unwrap_or(0))
    }

    /// Tokens that count against the context window (input + cache reads +
    /// cache creation). Used by the token tracker for window-utilisation
    /// checks.
    pub fn context_usage(&self) -> u64 {
        self.input_tokens
            + self.cached_input_tokens.unwrap_or(0)
            + self.cache_creation_tokens.unwrap_or(0)
    }

    /// Total across both context and output tokens.
    pub fn total(&self) -> u64 {
        self.context_usage() + self.output_tokens
    }

    /// Whether all token counts are zero.
    pub fn is_empty(&self) -> bool {
        self.context_usage() == 0 && self.output_tokens == 0
    }
}

fn add_opt(target: &mut Option<u64>, other: Option<u64>) {
    if let Some(v) = other {
        *target = Some(target.unwrap_or(0) + v);
    }
}

/// Counts of server-side tool invocations charged on this call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToolInvocations {
    /// Web search invocations (Anthropic, OpenAI Responses, Gemini grounding).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<u64>,
    /// Web fetch / URL retrieval invocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_fetch: Option<u64>,
    /// Code interpreter / sandboxed code execution invocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_interpreter: Option<u64>,
    /// File search / vector store retrieval invocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_search: Option<u64>,
    /// Computer use invocations (OpenAI Responses computer_use_preview).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_use: Option<u64>,
}

impl ServerToolInvocations {
    /// Add another invocation tally into this one.
    pub fn add(&mut self, other: &ServerToolInvocations) {
        add_opt(&mut self.web_search, other.web_search);
        add_opt(&mut self.web_fetch, other.web_fetch);
        add_opt(&mut self.code_interpreter, other.code_interpreter);
        add_opt(&mut self.file_search, other.file_search);
        add_opt(&mut self.computer_use, other.computer_use);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_accumulates_typed_fields() {
        let mut a = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cached_input_tokens: Some(80),
            reasoning_tokens: Some(20),
            ..Default::default()
        };
        let b = Usage {
            input_tokens: 10,
            output_tokens: 5,
            cached_input_tokens: Some(8),
            reasoning_tokens: None,
            ..Default::default()
        };
        a.add(&b);
        assert_eq!(a.input_tokens, 110);
        assert_eq!(a.output_tokens, 55);
        assert_eq!(a.cached_input_tokens, Some(88));
        assert_eq!(a.reasoning_tokens, Some(20));
    }

    #[test]
    fn total_output_includes_reasoning_and_audio() {
        let u = Usage {
            output_tokens: 100,
            reasoning_tokens: Some(50),
            audio_output_tokens: Some(10),
            ..Default::default()
        };
        assert_eq!(u.total_output_tokens(), 160);
    }

    #[test]
    fn server_tool_invocations_merge() {
        let mut a = ServerToolInvocations {
            web_search: Some(2),
            ..Default::default()
        };
        let b = ServerToolInvocations {
            web_search: Some(3),
            file_search: Some(1),
            ..Default::default()
        };
        a.add(&b);
        assert_eq!(a.web_search, Some(5));
        assert_eq!(a.file_search, Some(1));
    }

    #[test]
    fn raw_field_skipped_when_none() {
        let u = Usage {
            input_tokens: 1,
            output_tokens: 1,
            ..Default::default()
        };
        let j = serde_json::to_string(&u).unwrap();
        assert!(!j.contains("raw"));
    }
}
