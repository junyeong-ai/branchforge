//! Server-side tool usage tracking (web search, web fetch, …).
//!
//! These types live in the agent domain — they describe **how the
//! application observes and aggregates** server-tool invocations across
//! requests, not how a single provider response wire-encodes them. The
//! per-response wire shape lives on `ir::Usage::server_tool_invocations`.

use serde::{Deserialize, Serialize};

/// Local accumulator of server-side tool usage across one or more API
/// calls. Tracks usage of tools that the provider executes server-side
/// (e.g. Anthropic web_search, Anthropic web_fetch) rather than locally.
///
/// **Important distinction:**
/// - `ServerToolUse` (this type): aggregate of provider-executed tool calls
/// - `ToolStats[\"WebSearch\"]`: local WebSearch tool calls (via DuckDuckGo etc.)
/// - `ModelUsage::*_requests`: per-model local web request counts
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerToolUse {
    /// Number of server-side web search requests.
    pub web_search_requests: u64,
    /// Number of server-side web fetch requests.
    pub web_fetch_requests: u64,
}

impl ServerToolUse {
    /// Record a web search request.
    pub fn record_web_search(&mut self) {
        self.web_search_requests += 1;
    }

    /// Record a web fetch request.
    pub fn record_web_fetch(&mut self) {
        self.web_fetch_requests += 1;
    }

    /// Check if any server tools were used.
    pub fn has_usage(&self) -> bool {
        self.web_search_requests > 0 || self.web_fetch_requests > 0
    }

    /// Add counts from a per-response IR `ServerToolInvocations`.
    pub fn add_from_ir(&mut self, invocations: &crate::ir::ServerToolInvocations) {
        if let Some(ws) = invocations.web_search {
            self.web_search_requests += ws;
        }
        if let Some(wf) = invocations.web_fetch {
            self.web_fetch_requests += wf;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_tool_use_record() {
        let mut stu = ServerToolUse::default();
        assert!(!stu.has_usage());
        stu.record_web_search();
        stu.record_web_fetch();
        assert!(stu.has_usage());
        assert_eq!(stu.web_search_requests, 1);
        assert_eq!(stu.web_fetch_requests, 1);
    }

    #[test]
    fn test_add_from_ir() {
        let mut stu = ServerToolUse::default();
        let inv = crate::ir::ServerToolInvocations {
            web_search: Some(3),
            web_fetch: Some(2),
            ..Default::default()
        };
        stu.add_from_ir(&inv);
        assert_eq!(stu.web_search_requests, 3);
        assert_eq!(stu.web_fetch_requests, 2);
        stu.add_from_ir(&inv);
        assert_eq!(stu.web_search_requests, 6);
        assert_eq!(stu.web_fetch_requests, 4);
    }
}
