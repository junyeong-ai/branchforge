//! Micro-compaction — selective content block truncation.
//!
//! Reduces token usage by truncating large tool result content blocks
//! in the projection, without modifying the append-only graph.
//!
//! Results are stored in [`ContentOverrides`](crate::session::state::ContentOverrides)
//! and applied during `to_api_messages()`. They are intentionally transient:
//! lost on session reload, restoring full content from the graph.

use async_trait::async_trait;

use super::strategy::{CompactionContext, CompactionPlan, CompactionStrategy, ContentOverrideEntry};
use crate::session::state::Session;
use crate::session::SessionResult;
use crate::types::content::{ToolResultBlock, ToolResultContent, ToolResultContentBlock};
use crate::types::{CompactResult, ContentBlock};

/// Default threshold (fraction of max tokens) to trigger micro-compaction.
const DEFAULT_MICRO_THRESHOLD: f64 = 0.6;

/// Default max size (chars) for a single tool result before truncation.
const DEFAULT_MAX_RESULT_CHARS: usize = 8_000;

/// How many chars to keep when truncating.
const DEFAULT_TRUNCATE_TO_CHARS: usize = 2_000;

/// Micro-compaction strategy.
///
/// Scans tool result content blocks and truncates those exceeding
/// `max_result_chars`. Cheaper than full summarization (no LLM call),
/// but less effective at reducing tokens.
pub struct MicroCompaction {
    /// Token usage ratio threshold to trigger (0.0-1.0).
    pub threshold: f64,
    /// Max chars for a single tool result before truncation.
    pub max_result_chars: usize,
    /// Chars to keep when truncating.
    pub truncate_to_chars: usize,
}

impl Default for MicroCompaction {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_MICRO_THRESHOLD,
            max_result_chars: DEFAULT_MAX_RESULT_CHARS,
            truncate_to_chars: DEFAULT_TRUNCATE_TO_CHARS,
        }
    }
}

impl MicroCompaction {
    pub fn new(threshold: f64, max_result_chars: usize, truncate_to_chars: usize) -> Self {
        Self {
            threshold,
            max_result_chars,
            truncate_to_chars,
        }
    }

    pub fn threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold.clamp(0.1, 0.95);
        self
    }

    pub fn max_result_chars(mut self, chars: usize) -> Self {
        self.max_result_chars = chars;
        self
    }

    /// Find truncatable content blocks in the session's current projection.
    fn find_truncation_targets(&self, session: &Session) -> Vec<ContentOverrideEntry> {
        let branch_nodes = session.current_branch_graph_nodes();
        let mut entries = Vec::new();

        for node in &branch_nodes {
            // Only look at User and Assistant nodes (the ones that become messages)
            if !matches!(
                node.kind,
                crate::graph::NodeKind::User | crate::graph::NodeKind::Assistant
            ) {
                continue;
            }

            // Skip if already overridden
            if session.content_overrides.get(&node.id).is_some() {
                continue;
            }

            let Some(content_value) = node.payload.get("content") else {
                continue;
            };
            let Ok(blocks) = serde_json::from_value::<Vec<ContentBlock>>(content_value.clone())
            else {
                continue;
            };

            let mut has_large_block = false;
            let mut total_saved_tokens = 0u64;
            let mut replacement_blocks = Vec::with_capacity(blocks.len());

            for block in &blocks {
                let size = estimate_block_chars(block);
                if size > self.max_result_chars && is_truncatable(block) {
                    has_large_block = true;
                    let truncated = truncate_block(block, self.truncate_to_chars);
                    let new_size = estimate_block_chars(&truncated);
                    total_saved_tokens += ((size - new_size) / 4) as u64;
                    replacement_blocks.push(truncated);
                } else {
                    replacement_blocks.push(block.clone());
                }
            }

            if has_large_block {
                entries.push(ContentOverrideEntry {
                    node_id: node.id,
                    replacement_content: replacement_blocks,
                    original_tokens: total_saved_tokens,
                });
            }
        }

        entries
    }
}

#[async_trait]
impl CompactionStrategy for MicroCompaction {
    fn name(&self) -> &str {
        "micro"
    }

    fn requires_llm(&self) -> bool {
        false
    }

    fn is_durable(&self) -> bool {
        false
    }

    fn needs_compact(&self, ctx: &CompactionContext) -> bool {
        ctx.usage_ratio() >= self.threshold
    }

    fn plan(&self, session: &Session) -> SessionResult<CompactionPlan> {
        let targets = self.find_truncation_targets(session);
        if targets.is_empty() {
            return Ok(CompactionPlan::NotNeeded);
        }

        let estimated_savings: u64 = targets.iter().map(|e| e.original_tokens).sum();
        Ok(CompactionPlan::Override {
            overrides: targets,
            estimated_token_savings: estimated_savings,
        })
    }

    async fn execute(
        &self,
        plan: CompactionPlan,
        session: &mut Session,
        _client: Option<&crate::Client>,
    ) -> crate::Result<CompactResult> {
        let CompactionPlan::Override {
            overrides,
            estimated_token_savings,
        } = plan
        else {
            return Ok(CompactResult::NotNeeded);
        };

        let count = overrides.len();
        for entry in overrides {
            session
                .content_overrides
                .set(entry.node_id, entry.replacement_content);
        }

        Ok(CompactResult::Truncated {
            truncation_count: count,
            estimated_token_savings,
        })
    }
}

/// Estimate the character count of a content block.
fn estimate_block_chars(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text, .. } => text.len(),
        ContentBlock::ToolResult(result) => estimate_tool_result_chars(result),
        ContentBlock::ToolUse(tool_use) => {
            tool_use.input.to_string().len() + tool_use.name.len()
        }
        _ => 0,
    }
}

/// Estimate chars in a ToolResultBlock.
fn estimate_tool_result_chars(result: &ToolResultBlock) -> usize {
    match &result.content {
        Some(ToolResultContent::Text(text)) => text.len(),
        Some(ToolResultContent::Blocks(blocks)) => blocks
            .iter()
            .map(|b| match b {
                ToolResultContentBlock::Text { text } => text.len(),
                _ => 100, // approximate for images/search results
            })
            .sum(),
        None => 0,
    }
}

/// Check if a content block is suitable for truncation.
fn is_truncatable(block: &ContentBlock) -> bool {
    match block {
        ContentBlock::ToolResult(_) => true,
        ContentBlock::Text { text, .. } => text.len() > 4000,
        _ => false,
    }
}

/// Create a truncated version of a content block.
fn truncate_block(block: &ContentBlock, max_chars: usize) -> ContentBlock {
    match block {
        ContentBlock::Text { text, .. } => ContentBlock::text(format!(
            "{}... [truncated, {} chars total]",
            safe_truncate(text, max_chars),
            text.len()
        )),
        ContentBlock::ToolResult(result) => {
            ContentBlock::ToolResult(truncate_tool_result(result, max_chars))
        }
        other => other.clone(),
    }
}

/// Truncate a ToolResultBlock's content.
fn truncate_tool_result(result: &ToolResultBlock, max_chars: usize) -> ToolResultBlock {
    let truncated_content = match &result.content {
        Some(ToolResultContent::Text(text)) if text.len() > max_chars => {
            Some(ToolResultContent::Text(format!(
                "{}... [truncated, {} chars total]",
                safe_truncate(text, max_chars),
                text.len()
            )))
        }
        Some(ToolResultContent::Blocks(blocks)) => {
            let truncated: Vec<ToolResultContentBlock> = blocks
                .iter()
                .map(|b| match b {
                    ToolResultContentBlock::Text { text } if text.len() > max_chars => {
                        ToolResultContentBlock::Text {
                            text: format!(
                                "{}... [truncated, {} chars total]",
                                safe_truncate(text, max_chars),
                                text.len()
                            ),
                        }
                    }
                    other => other.clone(),
                })
                .collect();
            Some(ToolResultContent::Blocks(truncated))
        }
        other => other.clone(),
    };

    ToolResultBlock {
        tool_use_id: result.tool_use_id.clone(),
        content: truncated_content,
        is_error: result.is_error,
    }
}

/// Truncate a string at a char boundary.
fn safe_truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn micro_compaction_metadata() {
        let mc = MicroCompaction::default();
        assert_eq!(mc.name(), "micro");
        assert!(!mc.requires_llm());
        assert!(!mc.is_durable());
    }

    #[test]
    fn micro_compaction_needs_compact_threshold() {
        let mc = MicroCompaction::default();

        let below = CompactionContext {
            current_tokens: 50_000,
            max_tokens: 100_000,
            message_count: 10,
            idle_duration: None,
            last_compact_at: None,
            consecutive_failures: 0,
        };
        assert!(!mc.needs_compact(&below));

        let above = CompactionContext {
            current_tokens: 65_000,
            max_tokens: 100_000,
            message_count: 10,
            idle_duration: None,
            last_compact_at: None,
            consecutive_failures: 0,
        };
        assert!(mc.needs_compact(&above));
    }

    #[test]
    fn safe_truncate_at_boundary() {
        let s = "Hello, 세계!";
        let t = safe_truncate(s, 8);
        assert!(t.len() <= 8);
    }

    #[test]
    fn safe_truncate_no_truncation_needed() {
        let s = "short";
        assert_eq!(safe_truncate(s, 100), "short");
    }

    #[test]
    fn truncate_tool_result_text() {
        let result = ToolResultBlock::success("id1", "a".repeat(10_000));
        let truncated = truncate_tool_result(&result, 100);
        match &truncated.content {
            Some(ToolResultContent::Text(text)) => {
                assert!(text.len() < 200);
                assert!(text.contains("truncated"));
            }
            _ => panic!("Expected truncated text"),
        }
    }

    #[test]
    fn estimate_tool_result_text_size() {
        let result = ToolResultBlock::success("id1", "hello world");
        assert_eq!(estimate_tool_result_chars(&result), 11);
    }
}
