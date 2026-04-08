//! Context Compaction (Claude Code CLI compatible)
//!
//! Summarizes the entire conversation when context exceeds threshold.
//! Ported from Claude Code CLI's compact implementation for full compatibility.

use serde::{Deserialize, Serialize};

use crate::agent::DEFAULT_FAST_MODEL;
use crate::ir::{ContentPart, Message, Role};
use crate::session::state::{Session, SessionMessage};
use crate::session::types::CompactRecord;
use crate::session::{SessionError, SessionResult};
use super::CompactResult;

/// Context usage threshold for triggering compaction (80%).
pub const DEFAULT_COMPACT_THRESHOLD: f32 = 0.8;

/// Configuration for context compaction.
///
/// Controls when and how conversation history is summarized to fit within
/// context limits. The `detailed_summary` flag determines whether
/// detailed information (file names, code snippets, error details) is
/// preserved in summaries or replaced with a minimal overview.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactConfig {
    pub enabled: bool,
    pub threshold_percent: f32,
    pub summary_model: String,
    pub max_summary_tokens: u32,
    /// When true, summaries preserve detailed task information:
    /// - Specific file names and changes
    /// - Technical details and error descriptions
    /// - Problem-solving steps and decisions
    ///
    /// When false, summaries are minimal:
    /// - Primary request and intent
    /// - Key decisions made
    /// - Current status and next steps
    ///
    /// Derived from `OutputStyle::domain_instructions` when using `from_output_style()`.
    #[serde(default = "default_detailed_summary")]
    pub detailed_summary: bool,
    /// Optional custom instructions to append to the compact prompt.
    /// These are user-provided instructions for customizing the summary.
    #[serde(default)]
    pub custom_instructions: Option<String>,
}

fn default_detailed_summary() -> bool {
    true
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_percent: DEFAULT_COMPACT_THRESHOLD,
            summary_model: DEFAULT_FAST_MODEL.to_string(),
            max_summary_tokens: 4000,
            detailed_summary: true,
            custom_instructions: None,
        }
    }
}

impl CompactConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }

    pub fn threshold(mut self, percent: f32) -> Self {
        self.threshold_percent = percent.clamp(0.5, 0.95);
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.summary_model = model.into();
        self
    }

    /// Set whether to keep detailed task information in summaries.
    ///
    /// This mirrors the `detailed_summary` flag in `OutputStyle`.
    pub fn detailed_summary(mut self, keep: bool) -> Self {
        self.detailed_summary = keep;
        self
    }

    /// Set custom instructions for the compact prompt.
    pub fn custom_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.custom_instructions = Some(instructions.into());
        self
    }

    /// Create a CompactConfig derived from an OutputStyle's domain instruction presence.
    pub fn from_output_style(style: &crate::output_style::OutputStyle) -> Self {
        Self {
            detailed_summary: style.has_domain_instructions(),
            ..Default::default()
        }
    }
}

pub struct Compactor {
    config: CompactConfig,
}

impl Compactor {
    pub fn new(config: CompactConfig) -> Self {
        Self { config }
    }

    pub fn needs_compact(&self, current_tokens: u64, max_tokens: u64) -> bool {
        if !self.config.enabled {
            return false;
        }
        let threshold = (max_tokens as f32 * self.config.threshold_percent) as u64;
        current_tokens >= threshold
    }

    pub fn prepare_compact(&self, session: &Session) -> SessionResult<PreparedCompact> {
        if !self.config.enabled {
            return Err(SessionError::Compact {
                message: "Compact is disabled".to_string(),
            });
        }

        let messages = session.current_branch_messages();
        if messages.is_empty() {
            return Ok(PreparedCompact::NotNeeded);
        }

        let refs: Vec<_> = messages.iter().collect();
        let summary_prompt = self.format_for_summary(&refs);

        Ok(PreparedCompact::Ready {
            summary_prompt,
            message_count: messages.len(),
        })
    }

    pub async fn execute(
        &self,
        session: &mut Session,
        llm: &dyn crate::client::LlmCall,
    ) -> crate::Result<CompactResult> {
        let prepared = self.prepare_compact(session)?;
        let PreparedCompact::Ready { summary_prompt, .. } = prepared else {
            return Ok(CompactResult::NotNeeded);
        };

        let ir_request = crate::ir::ModelRequest::new(
            &self.config.summary_model,
            vec![Message::user(&summary_prompt)],
        )
        .with_max_tokens(self.config.max_summary_tokens);
        let ir_response = llm.send(&ir_request).await?;
        let result = self.apply_compact(session, ir_response.text())?;
        self.record_compact(session, &result);
        Ok(result)
    }

    pub fn apply_compact(
        &self,
        session: &mut Session,
        summary: String,
    ) -> SessionResult<CompactResult> {
        let projected_messages = session.current_branch_messages();
        let original_count = projected_messages.len();

        let removed_chars: usize = projected_messages
            .iter()
            .map(|m| {
                m.content
                    .iter()
                    .filter_map(|c| c.as_text())
                    .map(|t| t.len())
                    .sum::<usize>()
            })
            .sum();
        let saved_tokens = (removed_chars / 4) as u64;

        let branch_id = session.graph.primary_branch;
        session.graph.append_node(
            branch_id,
            crate::graph::NodeKind::Summary,
            serde_json::json!({
                "content": [ContentPart::text(format!("[Previous conversation summary]\n\n{}", summary))],
                "summary": summary,
            }),
        ).map_err(|e| SessionError::Storage {
            message: format!("compaction failed to append summary to primary branch: {}", e),
        })?;
        session.checkpoint_current_head(
            "compaction",
            Some("Context compaction summary appended".to_string()),
            vec!["compaction".to_string()],
        )?;

        // Summary is the only cached projection that needs to be refreshed;
        // current_branch_messages() now derives from graph on demand and
        // automatically starts from the new compaction Summary node.
        session.refresh_summary_cache();
        session.updated_at = chrono::Utc::now();

        Ok(CompactResult::Compacted {
            original_count,
            new_count: 1,
            saved_tokens: saved_tokens as usize,
            summary,
        })
    }

    pub fn record_compact(&self, session: &mut Session, result: &CompactResult) {
        if let CompactResult::Compacted {
            original_count,
            new_count,
            saved_tokens,
            summary,
        } = result
        {
            let record = CompactRecord::new(session.id)
                .counts(*original_count, *new_count)
                .summary(summary.clone())
                .saved_tokens(*saved_tokens);
            session.record_compact(record);
        }
    }

    fn format_for_summary(&self, messages: &[&SessionMessage]) -> String {
        let mut formatted = String::new();

        // Select prompt based on detailed_summary flag
        let prompt = if self.config.detailed_summary {
            COMPACTION_PROMPT_FULL
        } else {
            COMPACTION_PROMPT_MINIMAL
        };

        formatted.push_str(prompt);

        if let Some(ref instructions) = self.config.custom_instructions {
            formatted.push_str("\n\n");
            formatted.push_str("# Custom Summary Instructions\n\n");
            formatted.push_str(instructions);
        }

        formatted.push_str("\n\n---\n\n");
        formatted.push_str("# Conversation to summarize:\n\n");

        for msg in messages {
            let role = match msg.role {
                Role::User | Role::Tool => "Human",
                Role::Assistant => "Assistant",
            };

            formatted.push_str(&format!("**{}**:\n", role));

            for block in &msg.content {
                if let Some(text) = block.as_text() {
                    // Truncate very long messages but keep more context than before
                    let display_text = if text.len() > 8000 {
                        let mut end = 8000;
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        format!(
                            "{}... [truncated, {} chars total]",
                            &text[..end],
                            text.len()
                        )
                    } else {
                        text.to_string()
                    };
                    formatted.push_str(&display_text);
                    formatted.push('\n');
                }
            }
            formatted.push('\n');
        }

        formatted
    }

    pub fn config(&self) -> &CompactConfig {
        &self.config
    }
}

/// Full compaction prompt that preserves detailed task information.
const COMPACTION_PROMPT_FULL: &str = r#"Your task is to create a detailed summary of the conversation so far, paying close attention to the user's explicit requests and your previous actions.

This summary should be thorough in capturing technical details, code patterns, and architectural decisions that would be essential for continuing development work without losing context.

Before providing your final summary, wrap your analysis in <analysis> tags to organize your thoughts and ensure you've covered all necessary points. In your analysis process:

1. Chronologically analyze each message and section of the conversation. For each section thoroughly identify:
   - The user's explicit requests and intents
   - Your approach to addressing the user's requests
   - Key decisions, technical concepts and code patterns
   - Specific details like:
     - file names
     - full code snippets
     - function signatures
     - file edits
   - Errors that you ran into and how you fixed them
   - Pay special attention to specific user feedback that you received, especially if the user told you to do something differently.

2. Double-check for technical accuracy and completeness, addressing each required element thoroughly.

Your summary should include the following sections:

1. Primary Request and Intent: Capture all of the user's explicit requests and intents in detail

2. Key Technical Concepts: List all important technical concepts, technologies, and frameworks discussed.

3. Files and Code Sections: Enumerate specific files and code sections examined, modified, or created. Pay special attention to the most recent messages and include full code snippets where applicable and include a summary of why this file read or edit is important.

4. Errors and fixes: List all errors that you ran into, and how you fixed them. Pay special attention to specific user feedback that you received, especially if the user told you to do something differently.

5. Problem Solving: Document problems solved and any ongoing troubleshooting efforts.

6. All user messages: List ALL user messages that are not tool results. These are critical for understanding the users' feedback and changing intent.

7. Pending Tasks: Outline any pending tasks that you have explicitly been asked to work on.

8. Current Work: Describe in detail precisely what was being worked on immediately before this summary request, paying special attention to the most recent messages from both user and assistant. Include file names and code snippets where applicable.

9. Optional Next Step: List the next step that you will take that is related to the most recent work you were doing. IMPORTANT: ensure that this step is DIRECTLY in line with the user's most recent explicit requests, and the task you were working on immediately before this summary request. If your last task was concluded, then only list next steps if they are explicitly in line with the users request. Do not start on tangential requests or really old requests that were already completed without confirming with the user first.
   If there is a next step, include direct quotes from the most recent conversation showing exactly what task you were working on and where you left off. This should be verbatim to ensure there's no drift in task interpretation.

Here's an example of how your output should be structured:

<example>
<analysis>
[Your thought process, ensuring all points are covered thoroughly and accurately]
</analysis>

<summary>
1. Primary Request and Intent:
   [Detailed description]

2. Key Technical Concepts:
   - [Concept 1]
   - [Concept 2]
   - [...]

3. Files and Code Sections:
   - [File Name 1]
      - [Summary of why this file is important]
      - [Summary of the changes made to this file, if any]
      - [Important Code Snippet]
   - [File Name 2]
      - [Important Code Snippet]
   - [...]

4. Errors and fixes:
    - [Detailed description of error 1]:
      - [How you fixed the error]
      - [User feedback on the error if any]
    - [...]

5. Problem Solving:
   [Description of solved problems and ongoing troubleshooting]

6. All user messages:
    - [Detailed non tool use user message]
    - [...]

7. Pending Tasks:
   - [Task 1]
   - [Task 2]
   - [...]

8. Current Work:
   [Precise description of current work]

9. Optional Next Step:
   [Optional Next step to take]
</summary>
</example>

Please provide your summary based on the conversation so far, following this structure and ensuring precision and thoroughness in your response."#;

/// Minimal compaction prompt for concise summaries.
/// Used when `detailed_summary` is false.
const COMPACTION_PROMPT_MINIMAL: &str = r#"Your task is to create a concise summary of the conversation so far, focusing on the essential context needed to continue the interaction.

Before providing your final summary, briefly analyze the conversation in <analysis> tags.

Your summary should include the following sections:

1. Primary Request and Intent: What the user is trying to accomplish

2. Key Decisions Made: Important choices and approaches decided upon

3. Current Status: What has been completed and what remains

4. Next Steps: If applicable, what should be done next

Here's an example of how your output should be structured:

<example>
<analysis>
[Brief thought process]
</analysis>

<summary>
1. Primary Request and Intent:
   [Concise description]

2. Key Decisions Made:
   - [Decision 1]
   - [Decision 2]

3. Current Status:
   [What's done and what remains]

4. Next Steps:
   [What to do next, if applicable]
</summary>
</example>

Please provide a focused summary based on the conversation so far."#;

#[derive(Debug)]
pub enum PreparedCompact {
    NotNeeded,
    Ready {
        summary_prompt: String,
        message_count: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::state::SessionConfig;

    fn create_test_session(message_count: usize) -> Session {
        let mut session = Session::new(SessionConfig::default());

        for i in 0..message_count {
            let content = if i % 2 == 0 {
                format!("User message {}", i)
            } else {
                format!("Assistant response {}", i)
            };

            let msg = if i % 2 == 0 {
                SessionMessage::user(vec![ContentPart::text(content)])
            } else {
                SessionMessage::assistant(vec![ContentPart::text(content)])
            };

            session.add_message(msg).unwrap();
        }

        session
    }

    #[test]
    fn test_compact_strategy_default() {
        let strategy = CompactConfig::default();
        assert!(strategy.enabled);
        assert_eq!(strategy.threshold_percent, 0.8);
        assert!(strategy.detailed_summary);
        assert!(strategy.custom_instructions.is_none());
    }

    #[test]
    fn test_compact_strategy_disabled() {
        let strategy = CompactConfig::disabled();
        assert!(!strategy.enabled);
    }

    #[test]
    fn test_compact_strategy_with_detailed_summary() {
        let strategy = CompactConfig::default().detailed_summary(false);
        assert!(!strategy.detailed_summary);

        let strategy = CompactConfig::default().detailed_summary(true);
        assert!(strategy.detailed_summary);
    }

    #[test]
    fn test_compact_strategy_with_custom_instructions() {
        let strategy =
            CompactConfig::default().custom_instructions("Focus on test output and code changes.");

        assert_eq!(
            strategy.custom_instructions,
            Some("Focus on test output and code changes.".to_string())
        );
    }

    #[test]
    fn test_needs_compact() {
        let executor = Compactor::new(CompactConfig::default().threshold(0.8));

        assert!(!executor.needs_compact(70_000, 100_000));
        assert!(executor.needs_compact(80_000, 100_000));
        assert!(executor.needs_compact(90_000, 100_000));
    }

    #[test]
    fn test_prepare_compact_empty() {
        let session = Session::new(SessionConfig::default());
        let executor = Compactor::new(CompactConfig::default());

        let result = executor.prepare_compact(&session).unwrap();
        assert!(matches!(result, PreparedCompact::NotNeeded));
    }

    #[test]
    fn test_prepare_compact_ready_full_prompt() {
        let session = create_test_session(10);
        let executor = Compactor::new(CompactConfig::default().detailed_summary(true));

        let result = executor.prepare_compact(&session).unwrap();

        match result {
            PreparedCompact::Ready {
                summary_prompt,
                message_count,
            } => {
                // Full prompt should contain detailed sections
                assert!(summary_prompt.contains("Primary Request and Intent"));
                assert!(summary_prompt.contains("Key Technical Concepts"));
                assert!(summary_prompt.contains("Files and Code Sections"));
                assert!(summary_prompt.contains("Errors and fixes"));
                assert!(summary_prompt.contains("All user messages"));
                assert!(summary_prompt.contains("Optional Next Step"));
                assert_eq!(message_count, 10);
            }
            _ => panic!("Expected Ready"),
        }
    }

    #[test]
    fn test_prepare_compact_ready_minimal_prompt() {
        let session = create_test_session(10);
        let executor = Compactor::new(CompactConfig::default().detailed_summary(false));

        let result = executor.prepare_compact(&session).unwrap();

        match result {
            PreparedCompact::Ready {
                summary_prompt,
                message_count,
            } => {
                // Minimal prompt should be concise
                assert!(summary_prompt.contains("Primary Request and Intent"));
                assert!(summary_prompt.contains("Key Decisions Made"));
                assert!(summary_prompt.contains("Current Status"));
                assert!(summary_prompt.contains("Next Steps"));
                // Should NOT contain detailed coding sections
                assert!(!summary_prompt.contains("Files and Code Sections"));
                assert!(!summary_prompt.contains("All user messages"));
                assert_eq!(message_count, 10);
            }
            _ => panic!("Expected Ready"),
        }
    }

    #[test]
    fn test_prepare_compact_with_custom_instructions() {
        let session = create_test_session(5);
        let executor = Compactor::new(
            CompactConfig::default()
                .custom_instructions("Focus on Rust code changes and test results."),
        );

        let result = executor.prepare_compact(&session).unwrap();

        match result {
            PreparedCompact::Ready { summary_prompt, .. } => {
                assert!(summary_prompt.contains("# Custom Summary Instructions"));
                assert!(summary_prompt.contains("Focus on Rust code changes and test results."));
            }
            _ => panic!("Expected Ready"),
        }
    }

    #[test]
    fn test_apply_compact() {
        let mut session = create_test_session(10);
        let executor = Compactor::new(CompactConfig::default());

        let result = executor
            .apply_compact(&mut session, "Test summary".to_string())
            .unwrap();

        match result {
            CompactResult::Compacted {
                original_count,
                new_count,
                ..
            } => {
                assert_eq!(original_count, 10);
                assert_eq!(new_count, 1);
            }
            _ => panic!("Expected Compacted"),
        }

        assert!(session.summary.is_some());
        let messages = session.current_branch_messages();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].is_compact_summary);
    }

    #[test]
    fn test_prompt_contains_analysis_tags() {
        // Both prompts should instruct to use <analysis> tags
        assert!(COMPACTION_PROMPT_FULL.contains("<analysis>"));
        assert!(COMPACTION_PROMPT_MINIMAL.contains("<analysis>"));
    }

    #[test]
    fn test_prompt_contains_summary_tags() {
        // Both prompts should show <summary> in examples
        assert!(COMPACTION_PROMPT_FULL.contains("<summary>"));
        assert!(COMPACTION_PROMPT_MINIMAL.contains("<summary>"));
    }
}
