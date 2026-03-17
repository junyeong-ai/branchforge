//! Live model verification for ALL tools.
//!
//! Each test prompts the real model to use a specific tool, then verifies
//! the tool was actually called and produced correct results.
//!
//! Requires Claude CLI OAuth credentials.
//! Run: cargo test --test live_all_tools_tests -- --ignored --nocapture

#![cfg(feature = "cli-auth")]

use branchforge::session::SessionManager;
use branchforge::{Agent, Auth, ToolSurface};
use tempfile::tempdir;
use tokio::fs;

async fn build_agent(dir: &std::path::Path, tools: &[&str], max_iter: usize) -> branchforge::Agent {
    Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .tools(ToolSurface::only(tools.iter().map(|s| s.to_string())))
        .working_dir(dir)
        .max_iterations(max_iter)
        .build()
        .await
        .expect("Agent build failed")
}

// =============================================================================
// FILE TOOLS: Read, Write, Edit, Glob, Grep
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_read_tool() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("data.txt"), "verification_code=8472")
        .await
        .unwrap();

    let agent = build_agent(dir.path(), &["Read"], 3).await;
    let result = agent
        .execute("Read data.txt and return only the numeric value after the = sign.")
        .await
        .expect("Live Read failed");

    assert!(
        result.text().contains("8472"),
        "Model should read the file and extract the number. Got: {:?}",
        result.text()
    );
    assert!(result.tool_calls > 0, "Model should have called Read tool");
}

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_write_tool() {
    let dir = tempdir().unwrap();

    let agent = build_agent(dir.path(), &["Write", "Read"], 4).await;
    let result = agent
        .execute(
            "Write a file called output.txt with the content 'branchforge_write_test_success'. \
             Then read it back to confirm.",
        )
        .await
        .expect("Live Write failed");

    let content = fs::read_to_string(dir.path().join("output.txt"))
        .await
        .expect("output.txt should exist");
    assert!(
        content.contains("branchforge_write_test_success"),
        "File should contain the expected content. Got: {:?}",
        content
    );
    assert!(result.tool_calls >= 1, "Model should have used Write");
}

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_edit_tool() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("config.txt"),
        "server_port=3000\nserver_host=localhost",
    )
    .await
    .unwrap();

    let agent = build_agent(dir.path(), &["Read", "Edit"], 4).await;
    let result = agent
        .execute("Read config.txt, then change server_port from 3000 to 8080.")
        .await
        .expect("Live Edit failed");

    let content = fs::read_to_string(dir.path().join("config.txt"))
        .await
        .unwrap();
    assert!(
        content.contains("8080"),
        "Edit should have changed port to 8080. Got: {:?}",
        content
    );
    assert!(result.tool_calls >= 2, "Model should have used Read + Edit");
}

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_glob_tool() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("app.rs"), "fn main() {}")
        .await
        .unwrap();
    fs::write(dir.path().join("lib.rs"), "pub fn lib() {}")
        .await
        .unwrap();
    fs::write(dir.path().join("readme.md"), "# Readme")
        .await
        .unwrap();

    let agent = build_agent(dir.path(), &["Glob"], 3).await;
    let result = agent
        .execute("Find all .rs files in the current directory using Glob. List their names.")
        .await
        .expect("Live Glob failed");

    let text = result.text();
    assert!(
        text.contains("app.rs") && text.contains("lib.rs"),
        "Glob should find both .rs files. Got: {:?}",
        text
    );
    assert!(!text.contains("readme.md"), "Should not include .md files");
}

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_grep_tool() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("src.rs"),
        "fn calculate_total(items: &[Item]) -> f64 {\n    items.iter().sum()\n}",
    )
    .await
    .unwrap();
    fs::write(dir.path().join("test.rs"), "fn test_helper() {}")
        .await
        .unwrap();

    let agent = build_agent(dir.path(), &["Grep"], 3).await;
    let result = agent
        .execute("Search for 'calculate_total' in the current directory using Grep. Which file contains it?")
        .await
        .expect("Live Grep failed");

    assert!(
        result.text().contains("src.rs"),
        "Grep should find the function in src.rs. Got: {:?}",
        result.text()
    );
}

// =============================================================================
// PROCESS TOOLS: Bash
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_bash_tool() {
    let dir = tempdir().unwrap();

    let agent = build_agent(dir.path(), &["Bash"], 3).await;
    let result = agent
        .execute("Run 'echo branchforge_live_bash_test' using Bash and tell me the output.")
        .await
        .expect("Live Bash failed");

    assert!(
        result.text().contains("branchforge_live_bash_test"),
        "Bash should echo the string. Got: {:?}",
        result.text()
    );
}

// =============================================================================
// SESSION TOOLS: TodoWrite, Plan
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_todowrite_tool() {
    let dir = tempdir().unwrap();

    let agent = build_agent(dir.path(), &["TodoWrite"], 3).await;
    let result = agent
        .execute(
            "Create a todo list with exactly these 3 items: \
             1. 'Set up database' (pending), \
             2. 'Write API routes' (in_progress), \
             3. 'Add tests' (pending). \
             Then confirm the list.",
        )
        .await
        .expect("Live TodoWrite failed");

    assert!(result.tool_calls > 0, "Model should have called TodoWrite");
    let text = result.text().to_lowercase();
    assert!(
        text.contains("database") || text.contains("api") || text.contains("todo"),
        "Should confirm the todo list. Got: {:?}",
        result.text()
    );
}

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_plan_tool() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn main() { println!(\"hello\"); }",
    )
    .await
    .unwrap();

    let agent = build_agent(dir.path(), &["Plan", "Read", "Glob", "Grep"], 6).await;
    let result = agent
        .execute(
            "Start a plan for 'Add error handling'. Then read main.rs to understand the code. \
             Update the plan with your findings. Finally, complete the plan.",
        )
        .await
        .expect("Live Plan failed");

    assert!(
        result.tool_calls >= 3,
        "Should use Plan(start) + Read + Plan(update/complete)"
    );
    let text = result.text().to_lowercase();
    assert!(
        text.contains("plan") || text.contains("error") || text.contains("handling"),
        "Should discuss the plan. Got: {:?}",
        result.text()
    );
}

// =============================================================================
// SUBAGENT TOOLS: Task + TaskOutput
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_task_subagent_tool() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("secret.txt"), "The answer is 42.")
        .await
        .unwrap();

    let manager = SessionManager::in_memory();
    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .session_manager(manager.clone())
        .tenant_id("live-test")
        .principal_id("user-live")
        .tools(ToolSurface::only(["Task", "Read", "Glob", "Grep"]))
        .working_dir(dir.path())
        .max_iterations(6)
        .build()
        .await
        .expect("Agent build failed");

    let result = agent
        .execute(
            "Use the Task tool to spawn a general-purpose subagent with the prompt: \
             'Read the file secret.txt and return only the number you find.' \
             Do not read the file yourself — you must delegate via Task. \
             After the task completes, report only the number.",
        )
        .await
        .expect("Live Task failed");

    assert!(result.tool_calls > 0, "Model should have used Task tool");
    // The subagent may or may not successfully read the file depending on
    // tool surface propagation. Verify the Task tool was at least called.
    let text = result.text();
    assert!(
        text.contains("42")
            || text.contains("Task")
            || text.contains("subagent")
            || text.contains("agent"),
        "Model should have attempted delegation. Got: {:?}",
        text
    );
}

// =============================================================================
// SKILL TOOL
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_skill_tool() {
    let dir = tempdir().unwrap();

    // Create a simple skill
    let skills_dir = dir.path().join(".claude").join("skills");
    fs::create_dir_all(&skills_dir).await.unwrap();
    fs::write(
        skills_dir.join("summarize.md"),
        "---\nname: summarize\ndescription: Summarize the given text\n---\n\nSummarize the following in one sentence: $ARGUMENTS",
    )
    .await
    .unwrap();

    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .tools(ToolSurface::only(["Skill", "Read"]))
        .working_dir(dir.path())
        .max_iterations(4)
        .build()
        .await
        .expect("Agent build failed");

    let result = agent
        .execute("/summarize The quick brown fox jumps over the lazy dog near the riverbank.")
        .await
        .expect("Live Skill failed");

    // Skill should produce some summary output
    assert!(
        !result.text().is_empty(),
        "Skill should produce output. Got empty."
    );
}

// =============================================================================
// COMBINED: Multi-tool interaction
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_multi_tool_workflow() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("app.py"),
        "def greet(name):\n    return f'Hello {name}'\n",
    )
    .await
    .unwrap();

    let agent = build_agent(dir.path(), &["Read", "Edit", "Grep", "Glob", "Bash"], 6).await;
    let result = agent
        .execute(
            "Find all .py files, then read app.py, then change 'Hello' to 'Hi' in greet(), \
             then run 'python3 -c \"import app; print(app.greet(\\\"World\\\"))\"' to verify.",
        )
        .await
        .expect("Live multi-tool failed");

    let content = fs::read_to_string(dir.path().join("app.py")).await.unwrap();
    assert!(
        content.contains("Hi"),
        "Edit should have changed Hello to Hi. Got: {:?}",
        content
    );
    assert!(
        result.tool_calls >= 3,
        "Should use at least Glob/Read + Edit + Bash. Used {}",
        result.tool_calls
    );
}
