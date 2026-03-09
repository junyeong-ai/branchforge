//! Built-in Tools Unit Test
//!
//! Direct verification of all 14 built-in tools without API calls:
//! - File: Read, Write, Edit, Glob, Grep
//! - Process: Bash, KillShell
//! - Session: TodoWrite, Plan
//! - Subagent: Task, TaskOutput (via TaskRegistry)
//! - Skills: SkillRegistry, SkillRuntime
//!
//! Run: cargo run --example all_tools_test

use claude_agent::ToolOutput;
use claude_agent::agent::{AgentMetrics, AgentState, TaskOutputTool, TaskRegistry};
use claude_agent::common::{ContentSource, IndexRegistry};
use claude_agent::security::SecurityContext;
use claude_agent::session::{MemoryPersistence, SessionId, SessionState, ToolState};
use claude_agent::skills::{SkillIndex, SkillRuntime};
use claude_agent::tools::{
    BashTool, EditTool, ExecutionContext, GlobTool, GrepTool, KillShellTool, PlanTool,
    ProcessManager, ReadTool, TodoWriteTool, Tool, WriteTool,
};
use claude_agent::types::{StopReason, Usage};
use std::sync::Arc;

struct TestRunner {
    passed: usize,
    failed: usize,
}

impl TestRunner {
    fn new() -> Self {
        Self {
            passed: 0,
            failed: 0,
        }
    }

    fn check(&mut self, name: &str, result: Result<(), String>) {
        match result {
            Ok(()) => {
                println!("  [PASS] {}", name);
                self.passed += 1;
            }
            Err(e) => {
                println!("  [FAIL] {} - {}", name, e);
                self.failed += 1;
            }
        }
    }

    fn summary(&self) -> bool {
        println!("\n------------------------------------------------------------------------");
        println!("  Result: {} passed, {} failed", self.passed, self.failed);
        println!("------------------------------------------------------------------------");
        self.failed == 0
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("========================================================================");
    println!("                  Built-in Tools Unit Test                              ");
    println!("========================================================================\n");

    let temp_dir = tempfile::tempdir()?;
    let working_dir = temp_dir.path().to_path_buf();

    let security = SecurityContext::builder()
        .root(&working_dir)
        .build()
        .unwrap_or_else(|_| SecurityContext::permissive());
    let ctx = ExecutionContext::new(security);

    let session_id = SessionId::new();
    let session_ctx = ToolState::new(session_id);
    let process_manager = Arc::new(ProcessManager::new());

    let mut runner = TestRunner::new();

    // =========================================================================
    // Section 1: File Tools
    // =========================================================================
    println!("Section 1: File Tools");
    println!("------------------------------------------------------------------------");

    let write = WriteTool;
    let test_file = working_dir.join("test.txt");
    let result = write
        .execute(
            serde_json::json!({
                "file_path": test_file.to_str().unwrap(),
                "content": "Hello World\nLine 2\nLine 3"
            }),
            &ctx,
        )
        .await;
    runner.check(
        "Write",
        match &result.output {
            ToolOutput::Success(_) | ToolOutput::Empty => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    let read = ReadTool;
    let result = read
        .execute(
            serde_json::json!({"file_path": test_file.to_str().unwrap()}),
            &ctx,
        )
        .await;
    runner.check(
        "Read",
        match &result.output {
            ToolOutput::Success(s) if s.contains("Hello World") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    let result = read
        .execute(
            serde_json::json!({
                "file_path": test_file.to_str().unwrap(),
                "offset": 1,
                "limit": 2
            }),
            &ctx,
        )
        .await;
    runner.check(
        "Read (offset/limit)",
        match &result.output {
            ToolOutput::Success(s) if s.contains("Line 2") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    let edit = EditTool;
    let result = edit
        .execute(
            serde_json::json!({
                "file_path": test_file.to_str().unwrap(),
                "old_string": "World",
                "new_string": "Rust"
            }),
            &ctx,
        )
        .await;
    let edited = tokio::fs::read_to_string(&test_file).await?;
    runner.check(
        "Edit",
        match &result.output {
            ToolOutput::Success(_) | ToolOutput::Empty if edited.contains("Hello Rust") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    tokio::fs::write(
        working_dir.join("file1.rs"),
        "fn main() { println!(\"hello\"); }",
    )
    .await?;
    tokio::fs::write(working_dir.join("file2.rs"), "fn test() { assert!(true); }").await?;
    tokio::fs::write(working_dir.join("readme.md"), "# README").await?;

    let glob = GlobTool;
    let result = glob
        .execute(
            serde_json::json!({"pattern": "*.rs", "path": working_dir.to_str().unwrap()}),
            &ctx,
        )
        .await;
    runner.check(
        "Glob",
        match &result.output {
            ToolOutput::Success(s) if s.contains("file1.rs") && s.contains("file2.rs") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    let grep = GrepTool;
    let result = grep
        .execute(
            serde_json::json!({"pattern": "fn main", "path": working_dir.to_str().unwrap()}),
            &ctx,
        )
        .await;
    runner.check(
        "Grep",
        match &result.output {
            ToolOutput::Success(s) if s.contains("file1.rs") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    // =========================================================================
    // Section 2: Process Tools
    // =========================================================================
    println!("\nSection 2: Process Tools");
    println!("------------------------------------------------------------------------");

    let bash = BashTool::process_manager(process_manager.clone());
    let result = bash
        .execute(
            serde_json::json!({"command": "echo 'Hello from Bash'"}),
            &ctx,
        )
        .await;
    runner.check(
        "Bash",
        match &result.output {
            ToolOutput::Success(s) if s.contains("Hello from Bash") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    let result = bash
        .execute(
            serde_json::json!({"command": "sleep 0.01 && echo done", "timeout": 5000}),
            &ctx,
        )
        .await;
    runner.check(
        "Bash (timeout)",
        match &result.output {
            ToolOutput::Success(s) if s.contains("done") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    let result = bash
        .execute(
            serde_json::json!({
                "command": "sleep 0.1 && echo 'background done'",
                "run_in_background": true
            }),
            &ctx,
        )
        .await;
    runner.check(
        "Bash (background)",
        match &result.output {
            ToolOutput::Success(s) if s.contains("Background") || s.contains("process") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    let kill = KillShellTool::process_manager(process_manager.clone());
    let result = kill
        .execute(serde_json::json!({"shell_id": "nonexistent_12345"}), &ctx)
        .await;
    runner.check(
        "KillShell (error)",
        match &result.output {
            ToolOutput::Error(e)
                if e.to_string().contains("not found") || e.to_string().contains("No process") =>
            {
                Ok(())
            }
            _ => Err(format!("Expected error: {:?}", result)),
        },
    );

    // =========================================================================
    // Section 3: Session Tools
    // =========================================================================
    println!("\nSection 3: Session Tools");
    println!("------------------------------------------------------------------------");

    let todo = TodoWriteTool::new(session_ctx.clone(), session_id);
    let result = todo
        .execute(
            serde_json::json!({
                "todos": [
                    {"content": "Task A", "status": "pending", "activeForm": "Working on A"},
                    {"content": "Task B", "status": "in_progress", "activeForm": "Working on B"},
                    {"content": "Task C", "status": "completed", "activeForm": "Done C"}
                ]
            }),
            &ctx,
        )
        .await;
    runner.check(
        "TodoWrite",
        match &result.output {
            ToolOutput::Success(s) if s.contains("Task") || s.contains("todo") => Ok(()),
            ToolOutput::Empty => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    let plan = PlanTool::new(session_ctx.clone());

    let result = plan
        .execute(
            serde_json::json!({"action": "start", "name": "Test Plan"}),
            &ctx,
        )
        .await;
    runner.check(
        "Plan (start)",
        match &result.output {
            ToolOutput::Success(s) if s.contains("Plan mode started") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    let result = plan
        .execute(
            serde_json::json!({"action": "update", "content": "Step 1: Analyze\nStep 2: Implement"}),
            &ctx,
        )
        .await;
    runner.check(
        "Plan (update)",
        match &result.output {
            ToolOutput::Success(s) if s.contains("Plan content updated") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    let result = plan
        .execute(serde_json::json!({"action": "status"}), &ctx)
        .await;
    runner.check(
        "Plan (status)",
        match &result.output {
            ToolOutput::Success(s) if s.contains("Step 1") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    let result = plan
        .execute(serde_json::json!({"action": "complete"}), &ctx)
        .await;
    runner.check(
        "Plan (complete)",
        match &result.output {
            ToolOutput::Success(s) if s.contains("Plan completed") => Ok(()),
            _ => Err(format!("{:?}", result)),
        },
    );

    // =========================================================================
    // Section 4: Skills
    // =========================================================================
    println!("\nSection 4: Skills");
    println!("------------------------------------------------------------------------");

    let mut skill_registry = IndexRegistry::<SkillIndex>::new();
    skill_registry.register(
        SkillIndex::new("calculator", "Math calculator")
            .source(ContentSource::in_memory("Calculate: $ARGUMENTS"))
            .triggers(["calculate", "math"]),
    );
    skill_registry.register(
        SkillIndex::new("greeter", "Greeting generator")
            .source(ContentSource::in_memory(
                "Generate greeting for: $ARGUMENTS",
            ))
            .triggers(["greet"]),
    );

    let executor = SkillRuntime::new(skill_registry);

    runner.check("SkillRegistry", {
        if executor.has_skill("calculator") && executor.has_skill("greeter") {
            Ok(())
        } else {
            Err("Skills not registered".into())
        }
    });

    let calc_result = executor.execute("calculator", Some("15 * 4")).await;
    runner.check("SkillRuntime", {
        if calc_result.success {
            Ok(())
        } else {
            Err("Skill execution failed".into())
        }
    });

    let triggered = executor
        .execute_by_trigger("please calculate 100 / 5")
        .await;
    runner.check("SkillRuntime (trigger)", {
        if triggered.is_some() {
            Ok(())
        } else {
            Err("Trigger activation failed".into())
        }
    });

    // =========================================================================
    // Section 5: Task Registry
    // =========================================================================
    println!("\nSection 5: Task Registry");
    println!("------------------------------------------------------------------------");

    let persistence = Arc::new(MemoryPersistence::new());
    let task_registry = TaskRegistry::new(persistence.clone());

    let task_id = uuid::Uuid::new_v4().to_string();
    let _cancel_rx = task_registry
        .register(
            task_id.clone(),
            "explore".to_string(),
            "Test task".to_string(),
        )
        .await;
    runner.check("TaskRegistry (register)", {
        let status = task_registry.get_status(&task_id).await;
        if status == Some(SessionState::Active) {
            Ok(())
        } else {
            Err(format!("Expected Active, got {:?}", status))
        }
    });

    let complete_id = uuid::Uuid::new_v4().to_string();
    drop(
        task_registry
            .register(
                complete_id.clone(),
                "general".to_string(),
                "Complete test".to_string(),
            )
            .await,
    );
    let result = claude_agent::AgentResult {
        text: "Task completed".to_string(),
        messages: vec![],
        tool_calls: 0,
        iterations: 1,
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        metrics: AgentMetrics::default(),
        state: AgentState::Completed,
        session_id: complete_id.clone(),
        structured_output: None,
        uuid: uuid::Uuid::new_v4().to_string(),
    };
    task_registry.complete(&complete_id, result).await;
    runner.check("TaskRegistry (complete)", {
        let status = task_registry.get_status(&complete_id).await;
        if status == Some(SessionState::Completed) {
            Ok(())
        } else {
            Err(format!("Expected Completed, got {:?}", status))
        }
    });

    let fail_id = uuid::Uuid::new_v4().to_string();
    drop(
        task_registry
            .register(fail_id.clone(), "plan".to_string(), "Fail test".to_string())
            .await,
    );
    task_registry
        .fail(&fail_id, "Simulated error".to_string())
        .await;
    runner.check("TaskRegistry (fail)", {
        let status = task_registry.get_status(&fail_id).await;
        if status == Some(SessionState::Failed) {
            Ok(())
        } else {
            Err(format!("Expected Failed, got {:?}", status))
        }
    });

    let cancel_id = uuid::Uuid::new_v4().to_string();
    drop(
        task_registry
            .register(
                cancel_id.clone(),
                "explore".to_string(),
                "Cancel test".to_string(),
            )
            .await,
    );
    let cancelled = task_registry.cancel(&cancel_id).await;
    runner.check("TaskRegistry (cancel)", {
        if cancelled {
            let status = task_registry.get_status(&cancel_id).await;
            if status == Some(SessionState::Cancelled) {
                Ok(())
            } else {
                Err(format!("Expected Cancelled, got {:?}", status))
            }
        } else {
            Err("Cancel returned false".into())
        }
    });

    runner.check("TaskOutputTool (schema)", {
        let output_tool = TaskOutputTool::new(task_registry.clone());
        let schema = output_tool.input_schema().to_string();
        if schema.contains("task_id") && schema.contains("block") && schema.contains("timeout") {
            Ok(())
        } else {
            Err("Missing required fields".into())
        }
    });

    // =========================================================================
    // Summary
    // =========================================================================
    let success = runner.summary();

    if success {
        println!("========================================================================");
        println!("                    All tests passed!                                   ");
        println!("========================================================================");
    } else {
        println!("========================================================================");
        println!("                    Some tests failed!                                  ");
        println!("========================================================================");
        std::process::exit(1);
    }

    Ok(())
}
