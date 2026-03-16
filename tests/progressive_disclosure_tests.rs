//! Progressive Disclosure Tests
//!
//! Tests for skills, subagents, rules, output styles, and on-demand loading
//! patterns (SkillIndex, RuleIndex, triggers, execution modes).
//!
//! Run: cargo nextest run --test progressive_disclosure_tests --all-features

use branchforge::{
    ContentSource, Index, IndexRegistry, SourceType, ToolOutput, ToolRestricted, ToolSurface,
    common::PathMatched,
    context::RuleIndex,
    skills::{
        SkillExecutionKind, SkillIndex, SkillIndexLoader, SkillResult, SkillRuntime, SkillTool,
    },
    tools::{ExecutionContext, Tool, ToolRegistry},
};
use tempfile::tempdir;
use tokio::fs;

// =============================================================================
// Skill Index
// =============================================================================

mod skill_index_tests {
    use super::*;

    #[test]
    fn test_skill_index_creation() {
        let skill = SkillIndex::new("commit", "Create a git commit")
            .allowed_tools(["Bash"])
            .argument_hint("message");

        assert_eq!(skill.name, "commit");
        assert_eq!(skill.allowed_tools.len(), 1);
        assert_eq!(skill.description, "Create a git commit");
    }

    #[test]
    fn test_skill_index_matching() {
        let skill = SkillIndex::new("commit", "Create a git commit")
            .source(ContentSource::in_memory("Content"));

        assert!(skill.matches_command("/commit"));
        assert!(skill.matches_command("/commit -m 'message'"));
        assert!(!skill.matches_command("/other"));
        assert!(!skill.matches_command("commit"));
    }

    #[test]
    fn test_skill_index_trigger_matching() {
        let skill = SkillIndex::new("test", "Test skill")
            .source(ContentSource::in_memory("Content: $ARGUMENTS"))
            .triggers(["/test", "test keyword"]);

        assert_eq!(skill.name, "test");
        assert!(skill.matches_triggers("/test please"));
        assert!(skill.matches_triggers("I want to test keyword this"));
    }

    #[test]
    fn test_skill_index_command_matching() {
        let index = SkillIndex::new("git-commit", "Create commits").triggers(["commit", "git"]);

        assert_eq!(index.name, "git-commit");
        assert!(index.matches_command("/git-commit"));
        assert!(index.matches_triggers("I want to commit"));
    }

    #[test]
    fn test_skill_argument_substitution() {
        let content = "Fix issue: $ARGUMENTS in the codebase";
        let result = SkillIndex::substitute_args(content, Some("login bug"));
        assert_eq!(result, "Fix issue: login bug in the codebase");
    }

    #[test]
    fn test_skill_multiple_argument_substitution() {
        let content = "First: $ARGUMENTS, Second: $ARGUMENTS";
        let result = SkillIndex::substitute_args(content, Some("value"));
        assert_eq!(result, "First: value, Second: value");
    }

    #[test]
    fn test_skill_positional_argument_substitution() {
        let content = "File: $1, Action: $2, All: $ARGUMENTS";
        let result = SkillIndex::substitute_args(content, Some("main.rs build"));
        assert_eq!(result, "File: main.rs, Action: build, All: main.rs build");
    }

    #[test]
    fn test_skill_allowed_tools() {
        let skill = SkillIndex::new("read-only", "Read only")
            .source(ContentSource::in_memory("Content"))
            .allowed_tools(["Read", "Grep", "Glob"]);

        assert!(skill.is_tool_allowed("Read"));
        assert!(skill.is_tool_allowed("Grep"));
        assert!(!skill.is_tool_allowed("Bash"));
        assert!(!skill.is_tool_allowed("Write"));
    }

    #[test]
    fn test_skill_allowed_tools_bash_pattern() {
        let git_skill = SkillIndex::new("git-helper", "Git commands")
            .source(ContentSource::in_memory("Git: $ARGUMENTS"))
            .allowed_tools(["Bash(git:*)", "Read"]);

        assert!(git_skill.is_tool_allowed("Bash"));
        assert!(git_skill.is_tool_allowed("Read"));
        assert!(!git_skill.is_tool_allowed("Write"));
    }

    #[test]
    fn test_skill_model_override() {
        let skill = SkillIndex::new("fast", "Fast skill")
            .source(ContentSource::in_memory("Content"))
            .model("claude-haiku-4-5-20251001");

        assert_eq!(skill.model, Some("claude-haiku-4-5-20251001".to_string()));
    }

    #[test]
    fn test_skill_source_type() {
        let skill = SkillIndex::new("commit", "Create git commit")
            .source(ContentSource::in_memory("Analyze and commit changes"))
            .source_type(SourceType::Builtin)
            .triggers(["/commit"]);

        assert_eq!(skill.name, "commit");
        assert!(skill.matches_triggers("/commit please"));
        assert!(!skill.matches_triggers("just commit"));
    }

    #[test]
    fn test_skill_result() {
        let success = SkillResult::success("Task completed");
        assert!(success.success);
        assert!(success.error.is_none());

        let error = SkillResult::error("Task failed");
        assert!(!error.success);
        assert!(error.error.is_some());
    }
}

// =============================================================================
// Skill Registry
// =============================================================================

mod skill_registry_tests {
    use super::*;

    #[test]
    fn test_skill_registry() {
        let mut registry = IndexRegistry::<SkillIndex>::new();

        let skill1 =
            SkillIndex::new("commit", "Commit").source(ContentSource::in_memory("content1"));
        let skill2 =
            SkillIndex::new("review", "Review").source(ContentSource::in_memory("content2"));

        registry.register(skill1);
        registry.register(skill2);

        assert!(registry.get("commit").is_some());
        assert!(registry.get("review").is_some());
        assert!(registry.get("unknown").is_none());
    }
}

// =============================================================================
// Skill Executor
// =============================================================================

mod skill_executor_tests {
    use super::*;

    #[tokio::test]
    async fn test_skill_executor_direct() {
        let mut registry = IndexRegistry::<SkillIndex>::new();
        registry.register(
            SkillIndex::new("math", "Math skill")
                .source(ContentSource::in_memory("Calculate: $ARGUMENTS"))
                .triggers(["calculate"]),
        );

        let executor = SkillRuntime::new(registry);
        let result = executor.execute("math", Some("2+2")).await;
        assert!(result.success);
        assert!(result.output.contains("2+2"));
    }

    #[tokio::test]
    async fn test_skill_executor_trigger() {
        let mut registry = IndexRegistry::<SkillIndex>::new();
        registry.register(
            SkillIndex::new("math", "Math skill")
                .source(ContentSource::in_memory("Calculate: $ARGUMENTS"))
                .triggers(["calculate"]),
        );

        let executor = SkillRuntime::new(registry);
        let trigger_result = executor.execute_by_trigger("calculate 5*5").await;
        assert!(trigger_result.is_some());
    }

    #[tokio::test]
    async fn test_skill_on_demand_loading() {
        let mut registry = IndexRegistry::<SkillIndex>::new();
        registry.register(SkillIndex::new("commit", "Git commit helper").source(
            ContentSource::in_memory(
                "Create a commit with message: $ARGUMENTS\n\nUse git add and git commit.",
            ),
        ));
        registry.register(SkillIndex::new("deploy", "Deployment helper").source(
            ContentSource::in_memory(
                "Deploy to environment: $ARGUMENTS\n\nRun deployment scripts.",
            ),
        ));
        registry.register(SkillIndex::new("review-pr", "PR review helper").source(
            ContentSource::in_memory("Review PR: $ARGUMENTS\n\nCheck code quality."),
        ));

        let executor = SkillRuntime::new(registry);

        assert!(executor.has_skill("commit"));
        assert!(executor.has_skill("deploy"));
        assert!(executor.has_skill("review-pr"));
        assert!(!executor.has_skill("nonexistent"));

        let result = executor.execute("commit", Some("fix: typo")).await;
        assert!(result.success);
        assert!(result.output.contains("fix: typo"));
    }

    #[tokio::test]
    async fn test_trigger_based_skill_activation() {
        let mut registry = IndexRegistry::<SkillIndex>::new();
        registry.register(
            SkillIndex::new("jira-helper", "Jira")
                .source(ContentSource::in_memory("Handle Jira: $ARGUMENTS"))
                .triggers(["jira", "issue", "ticket"]),
        );
        registry.register(
            SkillIndex::new("docker-helper", "Docker")
                .source(ContentSource::in_memory("Handle Docker: $ARGUMENTS"))
                .triggers(["docker", "container"]),
        );

        let executor = SkillRuntime::new(registry);

        assert!(
            executor
                .execute_by_trigger("fix the jira ticket")
                .await
                .is_some()
        );
        assert!(
            executor
                .execute_by_trigger("restart docker container")
                .await
                .is_some()
        );
        assert!(
            executor
                .execute_by_trigger("random unrelated text")
                .await
                .is_none()
        );
    }
}

#[tokio::test]
async fn test_inline_skill_execution_kind() {
    let mut registry = IndexRegistry::<SkillIndex>::new();
    registry.register(
        SkillIndex::new("analyze", "Analyze code")
            .source(ContentSource::in_memory("Analyze: $ARGUMENTS")),
    );

    let executor = SkillRuntime::new(registry);
    let result = executor.execute("analyze", Some("main.rs")).await;

    assert!(result.success);
    assert_eq!(result.execution_kind, SkillExecutionKind::Inline);
    assert!(result.output.contains("Analyze: main.rs"));
}

// =============================================================================
// Skill Index Loader (from directory)
// =============================================================================

mod skill_loader_tests {
    use super::*;

    #[tokio::test]
    async fn test_skill_loader_from_directory() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");

        let deploy_dir = skills_dir.join("deploy");
        fs::create_dir_all(&deploy_dir).await.unwrap();
        fs::write(
            deploy_dir.join("SKILL.md"),
            r#"---
name: deploy
description: Deploy the application
allowed-tools:
  - Bash
argument-hint: environment
---
Deploy the application to the $ARGUMENTS environment.

Steps:
1. Run tests
2. Build the application
3. Deploy to $ARGUMENTS
"#,
        )
        .await
        .unwrap();

        let lambda_dir = skills_dir.join("aws-lambda");
        fs::create_dir_all(&lambda_dir).await.unwrap();
        fs::write(
            lambda_dir.join("SKILL.md"),
            r#"---
name: aws-lambda
description: Deploy AWS Lambda function
---
Deploy AWS Lambda function: $ARGUMENTS"#,
        )
        .await
        .unwrap();

        let ecs_dir = skills_dir.join("aws-ecs");
        fs::create_dir_all(&ecs_dir).await.unwrap();
        fs::write(
            ecs_dir.join("SKILL.md"),
            r#"---
name: aws-ecs
description: Deploy ECS
---
Deploy ECS: $ARGUMENTS"#,
        )
        .await
        .unwrap();

        let loader = SkillIndexLoader::new();
        let indices = loader.scan_directory(&skills_dir).await.unwrap();

        let mut registry = IndexRegistry::<SkillIndex>::new();
        registry.register_all(indices);

        assert!(registry.contains("deploy"));
        assert!(registry.contains("aws-lambda"));
        assert!(registry.contains("aws-ecs"));

        let skill = registry.get("deploy").unwrap();
        let content = skill.load_content().await.unwrap();
        let output = skill.execute("production", &content).await;
        assert!(output.contains("production"));

        assert!(skill.allowed_tools.contains(&"Bash".to_string()));
    }
}

// =============================================================================
// Skill Tool
// =============================================================================

mod skill_tool_tests {
    use super::*;

    #[tokio::test]
    async fn test_skill_tool_in_default_registry() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        assert!(registry.contains("Skill"));
    }

    #[tokio::test]
    async fn test_skill_tool_execute() {
        let mut skill_registry = IndexRegistry::<SkillIndex>::new();
        skill_registry.register(
            SkillIndex::new("test-skill", "Test skill")
                .source(ContentSource::in_memory("Execute: $ARGUMENTS"))
                .triggers(["test"]),
        );

        let executor = SkillRuntime::new(skill_registry);
        let tool = SkillTool::new(executor);
        let ctx = ExecutionContext::permissive();

        let result = tool
            .execute(
                serde_json::json!({
                    "skill": "test-skill",
                    "args": "hello world"
                }),
                &ctx,
            )
            .await;

        match &result.output {
            ToolOutput::Success(content) => assert!(content.contains("hello world")),
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_skill_tool_execute_inline_atlassian() {
        let mut skill_registry = IndexRegistry::<SkillIndex>::new();
        skill_registry.register(
            SkillIndex::new("atlassian-cli", "Execute Atlassian CLI commands")
                .source(ContentSource::in_memory(
                    r#"
You have access to the atlassian-cli tool. Use Bash to run commands like:
- `atlassian jira issue list --project $ARGUMENTS`

Execute the user's request: $ARGUMENTS
"#,
                ))
                .triggers(["jira", "confluence", "atlassian"]),
        );

        let executor = SkillRuntime::new(skill_registry);
        let skill_tool = SkillTool::new(executor);
        let ctx = ExecutionContext::permissive();

        let result = skill_tool
            .execute(
                serde_json::json!({
                    "skill": "atlassian-cli",
                    "args": "list issues in PROJECT-123"
                }),
                &ctx,
            )
            .await;

        assert!(!result.is_error());
        if let ToolOutput::Success(content) = &result.output {
            assert!(content.contains("atlassian"));
            assert!(content.contains("list issues in PROJECT-123"));
        }
    }
}

// =============================================================================
// Rule Index
// =============================================================================

mod rule_index_tests {
    use super::*;

    #[test]
    fn test_rule_index_path_matching() {
        let index = RuleIndex::new("rust")
            .paths(vec!["**/*.rs".into()])
            .priority(10);

        assert!(index.matches_path(std::path::Path::new("src/lib.rs")));
        assert!(!index.matches_path(std::path::Path::new("src/lib.ts")));
    }

    #[test]
    fn test_rule_index_global_rule() {
        let security_rule = RuleIndex::new("security").priority(20);
        assert!(security_rule.matches_path(std::path::Path::new("any/file.txt")));
    }

    #[tokio::test]
    async fn test_rule_index_lazy_load_content() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".claude").join("rules");
        fs::create_dir_all(&rules_dir).await.unwrap();

        fs::write(
            rules_dir.join("rust.md"),
            "# Rust Guidelines\n\n- Use snake_case\n- No unwrap() in production",
        )
        .await
        .unwrap();

        fs::write(
            rules_dir.join("security.md"),
            "# Security Rules\n\n- Never expose API keys",
        )
        .await
        .unwrap();

        let loader = branchforge::context::MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        assert_eq!(content.rule_indices.len(), 2);

        let rust_index = content.rule_indices.iter().find(|r| r.name == "rust");
        assert!(rust_index.is_some());
        let rust_content = rust_index.unwrap().load_content().await.unwrap();
        assert!(rust_content.contains("snake_case"));
    }
}

// =============================================================================
// CLI Tool Integration Patterns
// =============================================================================

mod cli_integration_tests {
    use super::*;

    #[tokio::test]
    async fn test_skill_with_docker_simulation() {
        let mut skill_registry = IndexRegistry::<SkillIndex>::new();
        skill_registry.register(
            SkillIndex::new("docker-compose", "Manage Docker Compose services").source(
                ContentSource::in_memory(
                    r#"
You have access to docker-compose. Use Bash to execute:

Available commands:
- docker-compose up -d: Start services
- docker-compose down: Stop services

User request: $ARGUMENTS

Execute the appropriate docker-compose command using Bash.
"#,
                ),
            ),
        );

        let executor = SkillRuntime::new(skill_registry);
        let result = executor
            .execute("docker-compose", Some("start the web service"))
            .await;

        assert!(result.success);
        assert!(result.output.contains("docker-compose"));
    }

    #[tokio::test]
    async fn test_context_aware_skill_activation() {
        let mut skill_registry = IndexRegistry::<SkillIndex>::new();
        skill_registry.register(
            SkillIndex::new("rust-analyzer", "Rust code analysis")
                .source(ContentSource::in_memory("Analyze Rust: $ARGUMENTS"))
                .triggers(["rust", "cargo"]),
        );
        skill_registry.register(
            SkillIndex::new("npm-scripts", "NPM script runner")
                .source(ContentSource::in_memory("Run npm: $ARGUMENTS"))
                .triggers(["npm", "node", "package.json"]),
        );

        let executor = SkillRuntime::new(skill_registry);

        assert!(
            executor
                .execute_by_trigger("cargo build failed")
                .await
                .is_some()
        );
        assert!(
            executor
                .execute_by_trigger("npm install error")
                .await
                .is_some()
        );
        assert!(executor.execute_by_trigger("random text").await.is_none());
    }
}

// =============================================================================
// Live Tests (require CLI credentials)
// =============================================================================

#[cfg(feature = "cli-integration")]
mod live_tests {
    use branchforge::{Agent, Auth};
    use tempfile::tempdir;
    use tokio::fs;

    use super::*;

    #[tokio::test]
    #[ignore = "Requires CLI credentials"]
    async fn test_live_agent_with_custom_skill() {
        let agent = Agent::builder()
            .auth(Auth::ClaudeCli)
            .await
            .expect("Failed to load CLI credentials")
            .skill(
                SkillIndex::new("math-helper", "Perform mathematical calculations").source(
                    ContentSource::in_memory(
                        "Calculate: $ARGUMENTS. Show your work and provide the answer.",
                    ),
                ),
            )
            .tools(ToolSurface::only(["Skill"]))
            .max_iterations(5)
            .build()
            .await
            .expect("Failed to create agent");

        let result = agent
            .execute("/math-helper 15 * 23 + 47")
            .await
            .expect("Agent failed");

        assert!(
            result.tool_calls > 0
                || result.text().contains("392")
                || result.text().contains("Calculate"),
            "Should contain result or skill output"
        );
    }

    #[tokio::test]
    #[ignore = "Requires CLI credentials"]
    async fn test_live_progressive_skill_discovery() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("test.txt"), "Hello World")
            .await
            .unwrap();

        let agent = Agent::builder()
            .auth(Auth::ClaudeCli)
            .await
            .expect("Failed to load CLI credentials")
            .skill(
                SkillIndex::new("file-analyzer", "Analyze file contents").source(
                    ContentSource::in_memory("Read and analyze the file: $ARGUMENTS"),
                ),
            )
            .skill(
                SkillIndex::new("code-reviewer", "Review code for issues")
                    .source(ContentSource::in_memory("Review code: $ARGUMENTS")),
            )
            .skill(
                SkillIndex::new("docker-helper", "Help with Docker commands")
                    .source(ContentSource::in_memory("Docker command: $ARGUMENTS")),
            )
            .tools(ToolSurface::only(["Skill", "Read"]))
            .working_dir(dir.path())
            .max_iterations(3)
            .build()
            .await
            .expect("Failed to create agent");

        let result = agent
            .execute("/file-analyzer test.txt")
            .await
            .expect("Agent failed");

        assert!(result.tool_calls > 0 || !result.text().is_empty());
    }
}
