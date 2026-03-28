//! Hook manager for registering and executing hooks.

use super::{Hook, HookContext, HookEvent, HookInput, HookOutput};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{Duration, timeout};

#[derive(Clone)]
pub struct HookManager {
    hooks: Vec<Arc<dyn Hook>>,
    cache: HashMap<HookEvent, Vec<usize>>,
    default_timeout_secs: u64,
}

impl Default for HookManager {
    fn default() -> Self {
        Self::new()
    }
}

impl HookManager {
    pub fn new() -> Self {
        Self {
            hooks: Vec::new(),
            cache: HashMap::new(),
            default_timeout_secs: 60,
        }
    }

    pub fn timeout(timeout_secs: u64) -> Self {
        Self {
            hooks: Vec::new(),
            cache: HashMap::new(),
            default_timeout_secs: timeout_secs,
        }
    }

    fn rebuild_cache(&mut self) {
        self.cache.clear();
        for event in HookEvent::all() {
            let mut indices: Vec<usize> = self
                .hooks
                .iter()
                .enumerate()
                .filter(|(_, h)| h.events().contains(event))
                .map(|(i, _)| i)
                .collect();
            indices.sort_by_key(|&i| std::cmp::Reverse(self.hooks[i].priority()));
            self.cache.insert(*event, indices);
        }
    }

    pub fn register<H: Hook + 'static>(&mut self, hook: H) {
        self.hooks.push(Arc::new(hook));
        self.rebuild_cache();
    }

    pub fn register_arc(&mut self, hook: Arc<dyn Hook>) {
        self.hooks.push(hook);
        self.rebuild_cache();
    }

    pub fn register_many(&mut self, hooks: impl IntoIterator<Item = Arc<dyn Hook>>) {
        for hook in hooks {
            self.hooks.push(hook);
        }
        self.rebuild_cache();
    }

    pub fn unregister(&mut self, name: &str) {
        self.hooks.retain(|h| h.name() != name);
        self.rebuild_cache();
    }

    pub fn hook_names(&self) -> Vec<&str> {
        self.hooks.iter().map(|h| h.name()).collect()
    }

    pub fn has_hook(&self, name: &str) -> bool {
        self.hooks.iter().any(|h| h.name() == name)
    }

    #[inline]
    pub fn hooks_for_event(&self, event: HookEvent) -> Vec<&Arc<dyn Hook>> {
        self.cache
            .get(&event)
            .map(|indices| indices.iter().map(|&i| &self.hooks[i]).collect())
            .unwrap_or_default()
    }

    pub async fn execute(
        &self,
        event: HookEvent,
        input: HookInput,
        hook_context: &HookContext,
    ) -> Result<HookOutput, crate::Error> {
        self.execute_hooks::<fn(&str, &HookOutput)>(event, input, hook_context, None)
            .await
    }

    pub async fn execute_with_handler<F>(
        &self,
        event: HookEvent,
        input: HookInput,
        hook_context: &HookContext,
        handler: F,
    ) -> Result<HookOutput, crate::Error>
    where
        F: FnMut(&str, &HookOutput),
    {
        self.execute_hooks(event, input, hook_context, Some(handler))
            .await
    }

    async fn execute_hooks<F>(
        &self,
        event: HookEvent,
        input: HookInput,
        hook_context: &HookContext,
        mut handler: Option<F>,
    ) -> Result<HookOutput, crate::Error>
    where
        F: FnMut(&str, &HookOutput),
    {
        let hooks = self.hooks_for_event(event);

        if hooks.is_empty() {
            return Ok(HookOutput::allow());
        }

        let mut merged_output = HookOutput::allow();

        for hook in hooks {
            if let (Some(matcher), Some(tool_name)) = (hook.tool_matcher(), input.tool_name())
                && !matcher.is_match(tool_name)
            {
                continue;
            }

            let hook_timeout = hook.timeout_secs().min(self.default_timeout_secs);
            let result = timeout(
                Duration::from_secs(hook_timeout),
                hook.execute(input.clone(), hook_context),
            )
            .await;

            let output = match result {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => {
                    if event.can_block() {
                        // Blockable hooks use fail-closed: errors propagate
                        return Err(crate::Error::HookFailed {
                            hook: hook.name().to_string(),
                            reason: e.to_string(),
                        });
                    }
                    // Non-blockable hooks use fail-open: log and continue
                    tracing::warn!(hook = hook.name(), error = %e, "Hook execution failed");
                    continue;
                }
                Err(_) => {
                    if event.can_block() {
                        // Blockable hooks use fail-closed: timeouts propagate
                        return Err(crate::Error::HookTimeout {
                            hook: hook.name().to_string(),
                            duration_secs: hook_timeout,
                        });
                    }
                    // Non-blockable hooks use fail-open: log and continue
                    tracing::warn!(
                        hook = hook.name(),
                        timeout_secs = hook_timeout,
                        "Hook timed out"
                    );
                    continue;
                }
            };

            if let Some(ref mut h) = handler {
                h(hook.name(), &output);
            }
            merged_output = Self::merge_outputs(merged_output, output);

            if !merged_output.continue_execution {
                break;
            }
        }

        Ok(merged_output)
    }

    fn merge_outputs(base: HookOutput, new: HookOutput) -> HookOutput {
        HookOutput {
            continue_execution: base.continue_execution && new.continue_execution,
            stop_reason: new.stop_reason.or(base.stop_reason),
            suppress_logging: base.suppress_logging || new.suppress_logging,
            system_message: new.system_message.or(base.system_message),
            updated_input: new.updated_input.or(base.updated_input),
            additional_context: match (base.additional_context, new.additional_context) {
                (Some(a), Some(b)) => Some(format!("{}\n{}", a, b)),
                (a, b) => a.or(b),
            },
        }
    }
}

impl std::fmt::Debug for HookManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookManager")
            .field("hook_count", &self.hooks.len())
            .field("hook_names", &self.hook_names())
            .field("default_timeout_secs", &self.default_timeout_secs)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct TestHook {
        name: String,
        events: Vec<HookEvent>,
        priority: i32,
        block: bool,
    }

    impl TestHook {
        fn new(name: impl Into<String>, events: Vec<HookEvent>, priority: i32) -> Self {
            Self {
                name: name.into(),
                events,
                priority,
                block: false,
            }
        }

        fn blocking(name: impl Into<String>, events: Vec<HookEvent>, priority: i32) -> Self {
            Self {
                name: name.into(),
                events,
                priority,
                block: true,
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

        fn priority(&self) -> i32 {
            self.priority
        }

        async fn execute(
            &self,
            _input: HookInput,
            _hook_context: &HookContext,
        ) -> Result<HookOutput, crate::Error> {
            if self.block {
                Ok(HookOutput::block(format!("Blocked by {}", self.name)))
            } else {
                Ok(HookOutput::allow())
            }
        }
    }

    #[tokio::test]
    async fn test_hook_registration() {
        let mut manager = HookManager::new();
        manager.register(TestHook::new("hook1", vec![HookEvent::PreToolUse], 0));
        manager.register(TestHook::new("hook2", vec![HookEvent::PostToolUse], 0));

        assert!(manager.has_hook("hook1"));
        assert!(manager.has_hook("hook2"));
        assert!(!manager.has_hook("hook3"));
        assert_eq!(manager.hook_names().len(), 2);
    }

    #[tokio::test]
    async fn test_hook_unregistration() {
        let mut manager = HookManager::new();
        manager.register(TestHook::new("hook1", vec![HookEvent::PreToolUse], 0));
        manager.register(TestHook::new("hook2", vec![HookEvent::PreToolUse], 0));

        manager.unregister("hook1");

        assert!(!manager.has_hook("hook1"));
        assert!(manager.has_hook("hook2"));
    }

    #[tokio::test]
    async fn test_hooks_for_event() {
        let mut manager = HookManager::new();
        manager.register(TestHook::new("hook1", vec![HookEvent::PreToolUse], 10));
        manager.register(TestHook::new(
            "hook2",
            vec![HookEvent::PreToolUse, HookEvent::PostToolUse],
            5,
        ));
        manager.register(TestHook::new("hook3", vec![HookEvent::SessionStart], 0));

        let pre_hooks = manager.hooks_for_event(HookEvent::PreToolUse);
        assert_eq!(pre_hooks.len(), 2);
        // Check priority order (hook1 has higher priority)
        assert_eq!(pre_hooks[0].name(), "hook1");
        assert_eq!(pre_hooks[1].name(), "hook2");

        let session_hooks = manager.hooks_for_event(HookEvent::SessionStart);
        assert_eq!(session_hooks.len(), 1);
        assert_eq!(session_hooks[0].name(), "hook3");
    }

    #[tokio::test]
    async fn test_execute_allows() {
        let mut manager = HookManager::new();
        manager.register(TestHook::new("hook1", vec![HookEvent::PreToolUse], 0));
        manager.register(TestHook::new("hook2", vec![HookEvent::PreToolUse], 0));

        let input = HookInput::pre_tool_use("session-1", "Read", serde_json::json!({}));
        let hook_context = HookContext::new("session-1");
        let output = manager
            .execute(HookEvent::PreToolUse, input, &hook_context)
            .await
            .unwrap();

        assert!(output.continue_execution);
    }

    #[tokio::test]
    async fn test_execute_blocks() {
        let mut manager = HookManager::new();
        manager.register(TestHook::new("hook1", vec![HookEvent::PreToolUse], 0));
        manager.register(TestHook::blocking(
            "hook2",
            vec![HookEvent::PreToolUse],
            10, // Higher priority, runs first
        ));

        let input = HookInput::pre_tool_use("session-1", "Read", serde_json::json!({}));
        let hook_context = HookContext::new("session-1");
        let output = manager
            .execute(HookEvent::PreToolUse, input, &hook_context)
            .await
            .unwrap();

        assert!(!output.continue_execution);
        assert_eq!(output.stop_reason, Some("Blocked by hook2".to_string()));
    }

    #[tokio::test]
    async fn test_no_hooks_allows() {
        let manager = HookManager::new();

        let input = HookInput::pre_tool_use("session-1", "Read", serde_json::json!({}));
        let hook_context = HookContext::new("session-1");
        let output = manager
            .execute(HookEvent::PreToolUse, input, &hook_context)
            .await
            .unwrap();

        assert!(output.continue_execution);
    }

    // Hook that always fails
    struct FailingHook {
        name: String,
        events: Vec<HookEvent>,
    }

    impl FailingHook {
        fn new(name: impl Into<String>, events: Vec<HookEvent>) -> Self {
            Self {
                name: name.into(),
                events,
            }
        }
    }

    #[async_trait]
    impl Hook for FailingHook {
        fn name(&self) -> &str {
            &self.name
        }

        fn events(&self) -> &[HookEvent] {
            &self.events
        }

        async fn execute(
            &self,
            _input: HookInput,
            _hook_context: &HookContext,
        ) -> Result<HookOutput, crate::Error> {
            Err(crate::Error::Config("Hook failed intentionally".into()))
        }
    }

    // Hook that times out
    struct SlowHook {
        name: String,
        events: Vec<HookEvent>,
    }

    impl SlowHook {
        fn new(name: impl Into<String>, events: Vec<HookEvent>) -> Self {
            Self {
                name: name.into(),
                events,
            }
        }
    }

    #[async_trait]
    impl Hook for SlowHook {
        fn name(&self) -> &str {
            &self.name
        }

        fn events(&self) -> &[HookEvent] {
            &self.events
        }

        fn timeout_secs(&self) -> u64 {
            1 // Short timeout for testing
        }

        async fn execute(
            &self,
            _input: HookInput,
            _hook_context: &HookContext,
        ) -> Result<HookOutput, crate::Error> {
            // Sleep longer than timeout
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(HookOutput::allow())
        }
    }

    #[tokio::test]
    async fn test_blockable_hook_failure_returns_error() {
        let mut manager = HookManager::new();
        manager.register(FailingHook::new("failing", vec![HookEvent::PreToolUse]));

        let input = HookInput::pre_tool_use("session-1", "Read", serde_json::json!({}));
        let hook_context = HookContext::new("session-1");
        let result = manager
            .execute(HookEvent::PreToolUse, input, &hook_context)
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, crate::Error::HookFailed { .. }));
    }

    #[tokio::test]
    async fn test_blockable_hook_timeout_returns_error() {
        let mut manager = HookManager::timeout(1);
        manager.register(SlowHook::new("slow", vec![HookEvent::UserPromptSubmit]));

        let input = HookInput::user_prompt_submit("session-1", "test prompt");
        let hook_context = HookContext::new("session-1");
        let result = manager
            .execute(HookEvent::UserPromptSubmit, input, &hook_context)
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, crate::Error::HookTimeout { .. }));
    }

    #[tokio::test]
    async fn test_non_blockable_hook_failure_continues() {
        let mut manager = HookManager::new();
        // SessionEnd is non-blockable
        manager.register(FailingHook::new("failing", vec![HookEvent::SessionEnd]));
        manager.register(TestHook::new("success", vec![HookEvent::SessionEnd], 0));

        let input = HookInput::session_end("session-1");
        let hook_context = HookContext::new("session-1");
        let result = manager
            .execute(HookEvent::SessionEnd, input, &hook_context)
            .await;

        // Should succeed despite the failing hook
        assert!(result.is_ok());
        assert!(result.unwrap().continue_execution);
    }

    #[tokio::test]
    async fn test_non_blockable_hook_timeout_continues() {
        let mut manager = HookManager::timeout(1);
        // PostToolUse is non-blockable
        manager.register(SlowHook::new("slow", vec![HookEvent::PostToolUse]));

        let input = HookInput::post_tool_use(
            "session-1",
            "Read",
            crate::types::ToolOutput::success("result"),
        );
        let hook_context = HookContext::new("session-1");
        let result = manager
            .execute(HookEvent::PostToolUse, input, &hook_context)
            .await;

        // Should succeed despite the timeout
        assert!(result.is_ok());
        assert!(result.unwrap().continue_execution);
    }
}
