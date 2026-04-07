//! Normalised finish reasons across all providers.

use serde::{Deserialize, Serialize};

/// Why a model stopped generating.
///
/// This is the union of all distinct stop semantics across Anthropic, OpenAI
/// (Chat + Responses), Gemini, and Bedrock Converse, collapsed where the
/// distinctions are not actionable for an agent runtime.
///
/// - [`FinishReason::ContentFilter`] unifies Anthropic `refusal`, OpenAI
///   `content_filter`, Gemini `SAFETY`/`RECITATION`/`BLOCKLIST`/`PROHIBITED_CONTENT`,
///   and Bedrock `guardrail_intervened`.
/// - [`FinishReason::PauseTurn`] is a distinct semantic ("call me back with
///   the same history and I'll continue") used by Anthropic `pause_turn` and
///   OpenAI Responses `incomplete { reason: tool_call_loop }`. Agent runtimes
///   should re-invoke the model rather than treating it as completion.
/// - [`FinishReason::Other`] preserves any unknown raw token verbatim so
///   debugging and round-trip equality remain possible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Natural completion — the model produced a complete response.
    Stop,
    /// The model emitted one or more tool calls and is awaiting results.
    ToolCalls,
    /// Generation was truncated by `max_output_tokens`.
    Length,
    /// A configured stop sequence was hit.
    StopSequence,
    /// Safety / recitation / guardrail / refusal — content was filtered.
    ContentFilter,
    /// The model paused mid-turn and expects the agent to re-invoke it with
    /// the same conversation state.
    PauseTurn,
    /// The model errored mid-generation. Distinct from a transport error.
    Error,
    /// A provider-specific finish reason that does not map to any of the
    /// above. The raw token is preserved verbatim.
    Other(String),
}

impl FinishReason {
    /// Return `true` for finish reasons that the agent runtime should treat
    /// as "the model has more to say".
    pub fn should_continue(&self) -> bool {
        matches!(self, FinishReason::ToolCalls | FinishReason::PauseTurn)
    }

    /// Return `true` for terminal finish reasons (the conversation cannot
    /// reasonably continue without user intervention).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            FinishReason::Stop
                | FinishReason::Length
                | FinishReason::StopSequence
                | FinishReason::ContentFilter
                | FinishReason::Error
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_continue_only_for_tool_calls_and_pause() {
        assert!(FinishReason::ToolCalls.should_continue());
        assert!(FinishReason::PauseTurn.should_continue());
        assert!(!FinishReason::Stop.should_continue());
        assert!(!FinishReason::Length.should_continue());
    }

    #[test]
    fn other_round_trips_through_serde() {
        let r = FinishReason::Other("recitation".into());
        let j = serde_json::to_string(&r).unwrap();
        let back: FinishReason = serde_json::from_str(&j).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn snake_case_serialization() {
        assert_eq!(
            serde_json::to_string(&FinishReason::ToolCalls).unwrap(),
            "\"tool_calls\""
        );
        assert_eq!(
            serde_json::to_string(&FinishReason::ContentFilter).unwrap(),
            "\"content_filter\""
        );
    }
}
