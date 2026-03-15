//! Session-related types for persistence and tracking.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::state::{MessageId, SessionId};

/// Environment context for coding-mode sessions.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EnvironmentContext {
    pub cwd: Option<PathBuf>,
    pub git_branch: Option<String>,
    pub git_commit: Option<String>,
    pub platform: Option<String>,
    pub sdk_version: Option<String>,
}

impl EnvironmentContext {
    pub fn capture(working_dir: Option<&Path>) -> Self {
        let (git_branch, git_commit) = working_dir.map(Self::git_info).unwrap_or_default();

        Self {
            cwd: working_dir.map(|p| p.to_path_buf()),
            git_branch,
            git_commit,
            platform: Some(current_platform().to_string()),
            sdk_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }

    fn git_info(dir: &Path) -> (Option<String>, Option<String>) {
        let branch = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(dir)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let commit = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(dir)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        (branch, commit)
    }

    pub fn is_empty(&self) -> bool {
        self.cwd.is_none() && self.git_branch.is_none()
    }
}

fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

/// Tool execution record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolExecution {
    pub id: Uuid,
    pub session_id: SessionId,
    pub message_id: Option<String>,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub tool_output: String,
    pub is_error: bool,
    pub error_message: Option<String>,
    pub duration_ms: u64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub plan_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl ToolExecution {
    pub fn new(
        session_id: SessionId,
        tool_name: impl Into<String>,
        tool_input: serde_json::Value,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            message_id: None,
            tool_name: tool_name.into(),
            tool_input,
            tool_output: String::new(),
            is_error: false,
            error_message: None,
            duration_ms: 0,
            input_tokens: None,
            output_tokens: None,
            plan_id: None,
            created_at: Utc::now(),
        }
    }

    pub fn output(mut self, output: impl Into<String>, is_error: bool) -> Self {
        self.tool_output = output.into();
        self.is_error = is_error;
        self
    }

    pub fn error(mut self, message: impl Into<String>) -> Self {
        self.is_error = true;
        self.error_message = Some(message.into());
        self
    }

    pub fn duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = duration_ms;
        self
    }

    pub fn plan(mut self, plan_id: Uuid) -> Self {
        self.plan_id = Some(plan_id);
        self
    }

    pub fn message(mut self, message_id: impl Into<String>) -> Self {
        self.message_id = Some(message_id.into());
        self
    }

    pub fn tokens(mut self, input: u32, output: u32) -> Self {
        self.input_tokens = Some(input);
        self.output_tokens = Some(output);
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    #[default]
    Draft,
    Approved,
    Executing,
    Completed,
    Failed,
    Cancelled,
}

impl PlanStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn can_execute(&self) -> bool {
        matches!(self, Self::Approved)
    }

    /// Parse from string with legacy alias support.
    pub fn from_str_lenient(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "approved" => Self::Approved,
            "executing" | "inprogress" | "in_progress" => Self::Executing,
            "completed" => Self::Completed,
            "cancelled" | "canceled" => Self::Cancelled,
            "failed" => Self::Failed,
            _ => Self::Draft,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub id: Uuid,
    pub session_id: SessionId,
    pub name: Option<String>,
    pub content: String,
    pub status: PlanStatus,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl Plan {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            name: None,
            content: String::new(),
            status: PlanStatus::Draft,
            error: None,
            created_at: Utc::now(),
            approved_at: None,
            started_at: None,
            completed_at: None,
        }
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    pub fn approve(&mut self) {
        self.status = PlanStatus::Approved;
        self.approved_at = Some(Utc::now());
    }

    pub fn start_execution(&mut self) {
        self.status = PlanStatus::Executing;
        self.started_at = Some(Utc::now());
    }

    pub fn complete(&mut self) {
        self.status = PlanStatus::Completed;
        self.completed_at = Some(Utc::now());
    }

    pub fn fail(&mut self, error: impl Into<String>) {
        self.status = PlanStatus::Failed;
        self.completed_at = Some(Utc::now());
        self.error = Some(error.into());
    }

    pub fn cancel(&mut self) {
        self.status = PlanStatus::Cancelled;
        self.completed_at = Some(Utc::now());
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    /// Parse from string with legacy alias support.
    pub fn from_str_lenient(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "in_progress" | "inprogress" => Self::InProgress,
            "completed" | "done" => Self::Completed,
            _ => Self::Pending,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: Uuid,
    pub session_id: SessionId,
    pub content: String,
    pub active_form: String,
    pub status: TodoStatus,
    pub plan_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl TodoItem {
    pub fn new(
        session_id: SessionId,
        content: impl Into<String>,
        active_form: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            content: content.into(),
            active_form: active_form.into(),
            status: TodoStatus::Pending,
            plan_id: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }

    pub fn plan(mut self, plan_id: Uuid) -> Self {
        self.plan_id = Some(plan_id);
        self
    }

    pub fn start(&mut self) {
        self.status = TodoStatus::InProgress;
        self.started_at = Some(Utc::now());
    }

    pub fn complete(&mut self) {
        self.status = TodoStatus::Completed;
        self.completed_at = Some(Utc::now());
    }

    pub fn status_icon(&self) -> &'static str {
        match self.status {
            TodoStatus::Pending => "○",
            TodoStatus::InProgress => "◐",
            TodoStatus::Completed => "●",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactTrigger {
    #[default]
    Manual,
    Auto,
    Threshold,
}

impl CompactTrigger {
    /// Parse from string with legacy alias support.
    pub fn from_str_lenient(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "auto" | "automatic" => Self::Auto,
            "threshold" => Self::Threshold,
            _ => Self::Manual,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactRecord {
    pub id: Uuid,
    pub session_id: SessionId,
    pub trigger: CompactTrigger,
    pub pre_tokens: usize,
    pub post_tokens: usize,
    pub saved_tokens: usize,
    pub summary: String,
    pub original_count: usize,
    pub new_count: usize,
    pub logical_parent_id: Option<MessageId>,
    pub created_at: DateTime<Utc>,
}

impl CompactRecord {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            trigger: CompactTrigger::default(),
            pre_tokens: 0,
            post_tokens: 0,
            saved_tokens: 0,
            summary: String::new(),
            original_count: 0,
            new_count: 0,
            logical_parent_id: None,
            created_at: Utc::now(),
        }
    }

    pub fn trigger(mut self, trigger: CompactTrigger) -> Self {
        self.trigger = trigger;
        self
    }

    pub fn counts(mut self, original: usize, new: usize) -> Self {
        self.original_count = original;
        self.new_count = new;
        self
    }

    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    pub fn saved_tokens(mut self, saved: usize) -> Self {
        self.saved_tokens = saved;
        self
    }

    pub fn tokens(mut self, pre: usize, post: usize) -> Self {
        self.pre_tokens = pre;
        self.post_tokens = post;
        self.saved_tokens = pre.saturating_sub(post);
        self
    }

    pub fn logical_parent(mut self, parent_id: MessageId) -> Self {
        self.logical_parent_id = Some(parent_id);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueOperation {
    Enqueue,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueStatus {
    #[default]
    Pending,
    Processing,
    Completed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: Uuid,
    pub session_id: SessionId,
    pub operation: QueueOperation,
    pub content: String,
    pub priority: i32,
    pub status: QueueStatus,
    pub created_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
}

impl QueueItem {
    pub fn enqueue(session_id: SessionId, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            operation: QueueOperation::Enqueue,
            content: content.into(),
            priority: 0,
            status: QueueStatus::Pending,
            created_at: Utc::now(),
            processed_at: None,
        }
    }

    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn start_processing(&mut self) {
        self.status = QueueStatus::Processing;
    }

    pub fn complete(&mut self) {
        self.status = QueueStatus::Completed;
        self.processed_at = Some(Utc::now());
    }

    pub fn cancel(&mut self) {
        self.status = QueueStatus::Cancelled;
        self.processed_at = Some(Utc::now());
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionStats {
    pub total_messages: usize,
    pub total_tool_calls: usize,
    pub tool_success_count: usize,
    pub tool_error_count: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: Decimal,
    pub avg_tool_duration_ms: f64,
    pub plans_count: usize,
    pub todos_completed: usize,
    pub todos_total: usize,
    pub compacts_count: usize,
    pub subagent_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAccessScope {
    pub tenant_id: Option<String>,
    pub principal_id: Option<String>,
}

impl SessionAccessScope {
    pub fn tenant(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    pub fn principal(mut self, principal_id: impl Into<String>) -> Self {
        self.principal_id = Some(principal_id.into());
        self
    }

    pub fn allows(&self, tenant_id: Option<&str>, principal_id: Option<&str>) -> bool {
        let tenant_ok = match self.tenant_id.as_deref() {
            Some(expected) => tenant_id == Some(expected),
            None => true,
        };
        let principal_ok = match self.principal_id.as_deref() {
            Some(expected) => principal_id == Some(expected),
            None => true,
        };
        tenant_ok && principal_ok
    }
}

impl SessionStats {
    pub fn tool_success_rate(&self) -> f64 {
        if self.total_tool_calls == 0 {
            1.0
        } else {
            self.tool_success_count as f64 / self.total_tool_calls as f64
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionTree {
    pub session_id: SessionId,
    pub session_type: super::state::SessionType,
    pub stats: SessionStats,
    pub children: Vec<SessionTree>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_environment_context() {
        let ctx = EnvironmentContext::capture(None);
        assert!(ctx.cwd.is_none());
        assert!(ctx.platform.is_some());
        assert!(ctx.sdk_version.is_some());
    }

    #[test]
    fn test_tool_execution_builder() {
        let session_id = SessionId::new();
        let exec = ToolExecution::new(session_id, "Bash", serde_json::json!({"command": "ls"}))
            .output("file1\nfile2", false)
            .duration(150);

        assert_eq!(exec.tool_name, "Bash");
        assert_eq!(exec.duration_ms, 150);
        assert!(!exec.is_error);
    }

    #[test]
    fn test_plan_lifecycle() {
        let session_id = SessionId::new();
        let mut plan = Plan::new(session_id)
            .name("Implement auth")
            .content("1. Create user model\n2. Add endpoints");

        assert_eq!(plan.status, PlanStatus::Draft);

        plan.approve();
        assert_eq!(plan.status, PlanStatus::Approved);
        assert!(plan.approved_at.is_some());

        plan.start_execution();
        assert_eq!(plan.status, PlanStatus::Executing);

        plan.complete();
        assert_eq!(plan.status, PlanStatus::Completed);
        assert!(plan.status.is_terminal());
    }

    #[test]
    fn test_todo_item() {
        let session_id = SessionId::new();
        let mut todo = TodoItem::new(session_id, "Fix bug", "Fixing bug");

        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(todo.status_icon(), "○");

        todo.start();
        assert_eq!(todo.status, TodoStatus::InProgress);
        assert_eq!(todo.status_icon(), "◐");

        todo.complete();
        assert_eq!(todo.status, TodoStatus::Completed);
        assert_eq!(todo.status_icon(), "●");
    }

    #[test]
    fn test_compact_record() {
        let session_id = SessionId::new();
        let record = CompactRecord::new(session_id)
            .trigger(CompactTrigger::Threshold)
            .tokens(100_000, 20_000)
            .counts(50, 5)
            .summary("Summary of conversation");

        assert_eq!(record.pre_tokens, 100_000);
        assert_eq!(record.post_tokens, 20_000);
        assert_eq!(record.saved_tokens, 80_000);
        assert_eq!(record.original_count, 50);
        assert_eq!(record.new_count, 5);
    }

    #[test]
    fn test_queue_item() {
        let session_id = SessionId::new();
        let mut item = QueueItem::enqueue(session_id, "Process this").priority(10);

        assert_eq!(item.status, QueueStatus::Pending);
        assert_eq!(item.priority, 10);

        item.start_processing();
        assert_eq!(item.status, QueueStatus::Processing);

        item.complete();
        assert_eq!(item.status, QueueStatus::Completed);
        assert!(item.processed_at.is_some());
    }

    #[test]
    fn test_session_stats() {
        let stats = SessionStats {
            total_tool_calls: 10,
            tool_success_count: 8,
            tool_error_count: 2,
            total_input_tokens: 1000,
            total_output_tokens: 500,
            ..Default::default()
        };

        assert!((stats.tool_success_rate() - 0.8).abs() < 0.001);
        assert_eq!(stats.total_tokens(), 1500);
    }

    #[test]
    fn test_status_from_str_lenient() {
        // TodoStatus
        assert_eq!(
            TodoStatus::from_str_lenient("in_progress"),
            TodoStatus::InProgress
        );
        assert_eq!(
            TodoStatus::from_str_lenient("inprogress"),
            TodoStatus::InProgress
        );
        assert_eq!(
            TodoStatus::from_str_lenient("completed"),
            TodoStatus::Completed
        );
        assert_eq!(TodoStatus::from_str_lenient("unknown"), TodoStatus::Pending);

        // PlanStatus
        assert_eq!(
            PlanStatus::from_str_lenient("approved"),
            PlanStatus::Approved
        );
        assert_eq!(
            PlanStatus::from_str_lenient("executing"),
            PlanStatus::Executing
        );
        assert_eq!(
            PlanStatus::from_str_lenient("inprogress"),
            PlanStatus::Executing
        );
        assert_eq!(
            PlanStatus::from_str_lenient("cancelled"),
            PlanStatus::Cancelled
        );
        assert_eq!(PlanStatus::from_str_lenient("unknown"), PlanStatus::Draft);

        // CompactTrigger
        assert_eq!(
            CompactTrigger::from_str_lenient("auto"),
            CompactTrigger::Auto
        );
        assert_eq!(
            CompactTrigger::from_str_lenient("automatic"),
            CompactTrigger::Auto
        );
        assert_eq!(
            CompactTrigger::from_str_lenient("threshold"),
            CompactTrigger::Threshold
        );
        assert_eq!(
            CompactTrigger::from_str_lenient("unknown"),
            CompactTrigger::Manual
        );
    }
}
