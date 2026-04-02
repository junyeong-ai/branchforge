//! State formatting utilities for agent compaction.

use crate::ToolRegistry;
use crate::session::types::{Plan, PlanStatus, TodoItem};

/// Format todo list for system-reminder after compaction.
pub fn format_todo_summary(todos: &[TodoItem]) -> String {
    todos
        .iter()
        .enumerate()
        .map(|(i, todo)| format!("{}. {} {}", i + 1, todo.status_icon(), todo.content))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format plan state for system-reminder after compaction.
pub fn format_plan_summary(plan: &Plan) -> String {
    let status = match plan.status {
        PlanStatus::Draft => "Draft",
        PlanStatus::Approved => "Approved",
        PlanStatus::Executing => "Executing",
        PlanStatus::Completed => "Completed",
        PlanStatus::Failed => "Failed",
        PlanStatus::Cancelled => "Cancelled",
    };

    let mut summary = format!("Status: {}", status);
    if let Some(ref name) = plan.name {
        summary.push_str(&format!("\nName: {}", name));
    }
    if !plan.content.is_empty() {
        summary.push_str(&format!("\nContent:\n{}", plan.content));
    }
    summary
}

/// Collect all active state for system-reminder after compaction.
pub async fn collect_compaction_state(tools: &ToolRegistry) -> Vec<String> {
    let mut sections = Vec::new();

    if let Some(tool_state) = tools.tool_state() {
        let todos = tool_state.todos().await;
        if !todos.is_empty() {
            sections.push(format!("## Current Tasks\n{}", format_todo_summary(&todos)));
        }

        if let Some(plan) = tool_state.current_plan().await
            && !plan.status.is_terminal()
        {
            sections.push(format!("## Active Plan\n{}", format_plan_summary(&plan)));
        }
    }

    let running_tasks = tools.task_registry().list_running().await;
    if !running_tasks.is_empty() {
        let tasks_summary = running_tasks
            .iter()
            .map(|(id, desc, elapsed)| {
                format!(
                    "- {}: \"{}\" (running for {:.1}s)",
                    id,
                    desc,
                    elapsed.as_secs_f64()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("## Running Background Agents\n{}", tasks_summary));
    }

    #[cfg(feature = "coding-tools")]
    if let Some(pm) = tools.process_manager() {
        let processes = pm.list().await;
        if !processes.is_empty() {
            let procs_summary = processes
                .iter()
                .map(|p| {
                    let cmd_display = if p.command.len() > 50 {
                        format!("{}...", &p.command[..50])
                    } else {
                        p.command.clone()
                    };
                    format!(
                        "- {}: \"{}\" (running for {:.1}s)",
                        p.id,
                        cmd_display,
                        p.started_at.elapsed().as_secs_f64()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("## Running Background Shells\n{}", procs_summary));
        }
    }

    sections
}
