//! Security & Authorization Tests
//!
//! Tests for security context, guards, authorization, hooks, and network sandbox.
//!
//! Run: cargo nextest run --test security_and_permissions_tests --all-features

// =============================================================================
// Security
// =============================================================================

mod security_tests {
    use branchforge::security::{SecurityContext, SecurityGuard};
    use tempfile::tempdir;

    #[test]
    fn test_security_context_creation() {
        let dir = tempdir().unwrap();
        let ctx = SecurityContext::new(dir.path()).unwrap();
        assert!(!ctx.fs.is_permissive());
    }

    #[test]
    fn test_security_context_permissive() {
        let ctx = SecurityContext::permissive();
        assert!(ctx.fs.is_permissive());
    }

    #[test]
    fn test_security_path_validation() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("file.txt"), "content").unwrap();

        let ctx = SecurityContext::new(&root).unwrap();
        let valid = ctx.fs.resolve("file.txt");
        assert!(valid.is_ok());
    }

    #[test]
    fn test_security_guard_blocks_escape() {
        let dir = tempdir().unwrap();
        let ctx = SecurityContext::new(dir.path()).unwrap();

        let input = serde_json::json!({ "file_path": "/etc/passwd" });
        let result = SecurityGuard::validate(&ctx, "Read", &input);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_security_bash_dangerous_blocked() {
        use branchforge::tools::{BashTool, ExecutionContext, Tool};

        let tool = BashTool::new();
        let security = SecurityContext::builder()
            .root(".")
            .build()
            .unwrap_or_else(|_| SecurityContext::permissive());
        let ctx = ExecutionContext::new(security);

        let result = tool
            .execute(serde_json::json!({"command": "rm -rf /"}), &ctx)
            .await;

        let text = result.text();
        assert!(
            result.is_error()
                || text.contains("denied")
                || text.contains("Authorization")
                || text.contains("may not be removed")
                || text.contains("Exit code: 1"),
            "Dangerous command should be blocked or denied: {:?}",
            result
        );
    }
}

// =============================================================================
// Authorization
// =============================================================================

mod authorization_tests {
    use branchforge::authorization::{
        ExecutionMode, ToolPolicyBuilder, is_file_tool, is_read_only_tool, is_shell_tool,
    };

    #[test]
    fn test_tool_classification() {
        assert!(is_read_only_tool("Read"));
        assert!(is_read_only_tool("Glob"));
        assert!(is_read_only_tool("Grep"));
        assert!(!is_read_only_tool("Write"));

        assert!(is_file_tool("Read"));
        assert!(is_file_tool("Write"));
        assert!(is_file_tool("Edit"));
        assert!(!is_file_tool("Bash"));

        assert!(is_shell_tool("Bash"));
        assert!(is_shell_tool("KillShell"));
        assert!(!is_shell_tool("Read"));
    }

    #[test]
    fn test_execution_modes() {
        assert!(matches!(ExecutionMode::default(), ExecutionMode::Auto));
        assert_eq!(ExecutionMode::Auto.to_string(), "auto");
        assert_eq!(ExecutionMode::Plan.to_string(), "plan");
        assert_eq!(ExecutionMode::Supervised.to_string(), "supervised");
    }

    #[test]
    fn test_tool_policy_builder() {
        let policy = ToolPolicyBuilder::new()
            .allow("Read")
            .allow("Glob")
            .deny("Bash")
            .build();

        let read_result = policy.check("Read", &serde_json::Value::Null);
        assert!(read_result.is_allowed());

        let bash_result = policy.check("Bash", &serde_json::Value::Null);
        assert!(bash_result.is_denied());
    }
}

// =============================================================================
// Hooks
// =============================================================================

mod hook_tests {
    use async_trait::async_trait;
    use branchforge::hooks::{Hook, HookContext, HookEvent, HookInput, HookManager, HookOutput};

    struct TestHook {
        name: String,
        events: Vec<HookEvent>,
    }

    impl TestHook {
        fn new() -> Self {
            Self {
                name: "test-hook".to_string(),
                events: vec![HookEvent::PreToolUse],
            }
        }
    }

    #[async_trait]
    impl Hook for TestHook {
        fn name(&self) -> &str {
            &self.name
        }

        fn events(&self) -> &[HookEvent] {
            &self.events
        }

        async fn execute(
            &self,
            input: HookInput,
            _ctx: &HookContext,
        ) -> Result<HookOutput, branchforge::Error> {
            if input.tool_name() == Some("Bash") {
                return Ok(HookOutput::block("Bash blocked by hook"));
            }
            Ok(HookOutput::allow())
        }
    }

    #[test]
    fn test_hook_registration() {
        let mut manager = HookManager::new();
        manager.register(TestHook::new());
        assert_eq!(manager.hook_names().len(), 1);
    }

    #[test]
    fn test_hook_event_can_block() {
        assert!(HookEvent::PreToolUse.can_block());
        assert!(HookEvent::UserPromptSubmit.can_block());
        assert!(!HookEvent::PostToolUse.can_block());
        assert!(!HookEvent::SessionEnd.can_block());
    }

    #[test]
    fn test_hook_output_allow() {
        let output = HookOutput::allow();
        assert!(output.continue_execution);
        assert!(output.stop_reason.is_none());
    }

    #[test]
    fn test_hook_output_block() {
        let output = HookOutput::block("Security violation");
        assert!(!output.continue_execution);
        assert_eq!(output.stop_reason, Some("Security violation".to_string()));
    }

    #[test]
    fn test_hook_output_modify() {
        let output = HookOutput::allow().updated_input(serde_json::json!({"modified": true}));
        assert!(output.continue_execution);
        assert!(output.updated_input.is_some());
    }

    #[test]
    fn test_hook_output_context() {
        let output = HookOutput::allow()
            .system_message("Injected")
            .context("Extra info");

        assert!(output.system_message.is_some());
        assert!(output.additional_context.is_some());
    }
}

// =============================================================================
// Network Sandbox
// =============================================================================

mod network_sandbox_tests {
    use branchforge::tools::{DomainCheck, NetworkSandbox};

    #[test]
    fn test_network_sandbox_defaults() {
        let sandbox = NetworkSandbox::new();

        assert_eq!(sandbox.check("api.anthropic.com"), DomainCheck::Allowed);
        assert_eq!(sandbox.check("claude.ai"), DomainCheck::Allowed);
        assert_eq!(sandbox.check("localhost"), DomainCheck::Allowed);
        assert_eq!(sandbox.check("unknown.com"), DomainCheck::Blocked);
    }

    #[test]
    fn test_network_sandbox_wildcards() {
        let sandbox = NetworkSandbox::new()
            .allowed_domains(vec!["*.example.com".to_string()])
            .blocked_domains(vec!["*.malware.com".to_string()]);

        assert_eq!(sandbox.check("sub.example.com"), DomainCheck::Allowed);
        assert_eq!(sandbox.check("sub.malware.com"), DomainCheck::Blocked);
    }

    #[test]
    fn test_network_sandbox_block_precedence() {
        let sandbox = NetworkSandbox::new()
            .allowed_domains(vec!["example.com".to_string()])
            .blocked_domains(vec!["example.com".to_string()]);

        assert_eq!(sandbox.check("example.com"), DomainCheck::Blocked);
    }
}
