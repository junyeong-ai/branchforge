//! Advanced Features Integration Test
//!
//! Verifies advanced SDK features:
//! - Permission Modes (BypassPermissions, AcceptEdits, allow_tool, Default)
//! - Hook System (HookManager, HookEvent, HookOutput)
//! - Session Manager (create, update, fork, lifecycle, tenant)
//! - Subagent System (SubagentIndex, builtin_subagents)
//!
//! Run: cargo run --example advanced_test

use async_trait::async_trait;
use claude_agent::{
    Agent, Auth, Hook, ToolAccess,
    common::ContentSource,
    hooks::{HookContext, HookEvent, HookInput, HookManager, HookOutput},
    permissions::{PermissionMode, PermissionPolicy},
    session::{SessionConfig, SessionManager, SessionState},
    subagents::{SubagentIndex, builtin_subagents},
    types::ContentBlock,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

static PASSED: AtomicUsize = AtomicUsize::new(0);
static FAILED: AtomicUsize = AtomicUsize::new(0);

macro_rules! test {
    ($name:expr, $body:expr) => {{
        let start = Instant::now();
        match $body {
            Ok(()) => {
                println!("  [PASS] {} ({:.2?})", $name, start.elapsed());
                PASSED.fetch_add(1, Ordering::SeqCst);
            }
            Err(e) => {
                println!("  [FAIL] {} - {}", $name, e);
                FAILED.fetch_add(1, Ordering::SeqCst);
            }
        }
    }};
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("warn").init();

    println!("\n========================================================================");
    println!("                  Advanced Features Integration Test                    ");
    println!("========================================================================\n");

    let working_dir = std::env::current_dir().expect("Failed to get cwd");

    println!("Section 1: Permission Modes");
    println!("------------------------------------------------------------------------");
    test!(
        "BypassPermissions mode",
        test_bypass_permissions(&working_dir).await
    );
    test!("AcceptEdits mode", test_accept_edits(&working_dir).await);
    test!(
        "allow_tool rules",
        test_allow_tool_rules(&working_dir).await
    );
    test!("Default mode denies", test_default_mode_denies());
    test!("PermissionPolicy API", test_permission_policy_api());

    println!("\nSection 2: Hook System");
    println!("------------------------------------------------------------------------");
    test!("HookEvent types", test_hook_events());
    test!("HookManager registration", test_hook_manager());
    test!("Hook priority ordering", test_hook_priority());

    println!("\nSection 3: Session Manager");
    println!("------------------------------------------------------------------------");
    test!("Session create", test_session_create().await);
    test!("Session update", test_session_update().await);
    test!("Session messages", test_session_messages().await);
    test!("Session fork", test_session_fork().await);
    test!("Session lifecycle", test_session_lifecycle().await);
    test!("Session tenant", test_session_tenant().await);

    println!("\nSection 4: Subagent System");
    println!("------------------------------------------------------------------------");
    test!("SubagentIndex", test_subagent_definition());
    test!("Builtin subagents", test_builtin_subagents());
    test!("Subagent tool restrictions", test_subagent_tools());
    test!("Subagent model resolution", test_subagent_model());

    let (passed, failed) = (PASSED.load(Ordering::SeqCst), FAILED.load(Ordering::SeqCst));
    println!("\n========================================================================");
    println!("  RESULTS: {} passed, {} failed", passed, failed);
    println!("========================================================================\n");

    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

// =============================================================================
// Section 1: Permission Modes
// =============================================================================

async fn test_bypass_permissions(working_dir: &PathBuf) -> Result<(), String> {
    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .map_err(|e| format!("Auth: {}", e))?
        .tools(ToolAccess::only(["Bash"]))
        .permission_mode(PermissionMode::BypassPermissions)
        .working_dir(working_dir)
        .build()
        .await
        .map_err(|e| format!("Build: {}", e))?;

    let result = agent
        .execute("Use Bash to run 'echo BYPASS_TEST'. Confirm.")
        .await
        .map_err(|e| format!("Execute: {}", e))?;

    if result.tool_calls == 0 {
        return Err("Bash not called".into());
    }
    if !result.text.contains("BYPASS_TEST") {
        return Err("Output not found".into());
    }
    Ok(())
}

async fn test_accept_edits(working_dir: &PathBuf) -> Result<(), String> {
    let file_path = working_dir.join("_test_accept_edits.txt");

    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .map_err(|e| format!("Auth: {}", e))?
        .tools(ToolAccess::only(["Write", "Read"]))
        .permission_mode(PermissionMode::AcceptEdits)
        .working_dir(working_dir)
        .build()
        .await
        .map_err(|e| format!("Build: {}", e))?;

    let prompt = format!(
        "Use Write to create '{}' with 'ACCEPT_EDITS_TEST', then Read it. Tell me content.",
        file_path.display()
    );
    let result = agent
        .execute(&prompt)
        .await
        .map_err(|e| format!("Execute: {}", e))?;

    let _ = std::fs::remove_file(&file_path);

    if result.tool_calls < 2 {
        return Err(format!("Expected 2+ calls, got {}", result.tool_calls));
    }
    if !result.text.contains("ACCEPT_EDITS_TEST") {
        return Err("Content not found".into());
    }
    Ok(())
}

async fn test_allow_tool_rules(working_dir: &PathBuf) -> Result<(), String> {
    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .map_err(|e| format!("Auth: {}", e))?
        .tools(ToolAccess::only(["Glob"]))
        .permission_mode(PermissionMode::Default)
        .allow_tool("Glob")
        .working_dir(working_dir)
        .build()
        .await
        .map_err(|e| format!("Build: {}", e))?;

    let result = agent
        .execute("Use Glob to find '*.toml'. Confirm.")
        .await
        .map_err(|e| format!("Execute: {}", e))?;

    if result.tool_calls > 0 {
        Ok(())
    } else {
        Err("Glob not called with allow_tool".into())
    }
}

fn test_default_mode_denies() -> Result<(), String> {
    let policy = PermissionPolicy::default();
    let result = policy.check("Read", &serde_json::json!({"file_path": "/etc/passwd"}));

    if result.is_allowed() {
        return Err("Should deny without allow rule".into());
    }
    if !result.reason.contains("Default mode") {
        return Err(format!("Wrong reason: {}", result.reason));
    }
    Ok(())
}

fn test_permission_policy_api() -> Result<(), String> {
    let accept = PermissionPolicy::accept_edits();
    if !accept.check("Read", &serde_json::json!({})).is_allowed() {
        return Err("AcceptEdits should allow Read".into());
    }
    if accept.check("Bash", &serde_json::json!({})).is_allowed() {
        return Err("AcceptEdits should deny Bash".into());
    }

    let bypass = PermissionPolicy::permissive();
    if !bypass.check("Bash", &serde_json::json!({})).is_allowed() {
        return Err("Permissive should allow all".into());
    }

    Ok(())
}

// =============================================================================
// Section 2: Hook System
// =============================================================================

struct TestHook {
    name: String,
    events: Vec<HookEvent>,
    priority: i32,
}

impl TestHook {
    fn new(name: impl Into<String>, events: Vec<HookEvent>, priority: i32) -> Self {
        Self {
            name: name.into(),
            events,
            priority,
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
        _ctx: &HookContext,
    ) -> Result<HookOutput, claude_agent::Error> {
        Ok(HookOutput::allow())
    }
}

fn test_hook_events() -> Result<(), String> {
    let all_events = HookEvent::all();
    if all_events.len() < 10 {
        return Err(format!("Expected 10+ events, got {}", all_events.len()));
    }

    if !HookEvent::PreToolUse.can_block() {
        return Err("PreToolUse should be blockable".into());
    }
    if !HookEvent::UserPromptSubmit.can_block() {
        return Err("UserPromptSubmit should be blockable".into());
    }
    if HookEvent::PostToolUse.can_block() {
        return Err("PostToolUse should not be blockable".into());
    }

    Ok(())
}

fn test_hook_manager() -> Result<(), String> {
    let mut manager = HookManager::new();

    manager.register(TestHook::new("hook-1", vec![HookEvent::PreToolUse], 0));
    manager.register(TestHook::new("hook-2", vec![HookEvent::PostToolUse], 0));

    if !manager.has_hook("hook-1") {
        return Err("hook-1 not found".into());
    }
    if !manager.has_hook("hook-2") {
        return Err("hook-2 not found".into());
    }
    if manager.hook_names().len() != 2 {
        return Err("Expected 2 hooks".into());
    }

    let pre_hooks = manager.hooks_for_event(HookEvent::PreToolUse);
    if pre_hooks.len() != 1 {
        return Err("Should have 1 PreToolUse hook".into());
    }

    let post_hooks = manager.hooks_for_event(HookEvent::PostToolUse);
    if post_hooks.len() != 1 {
        return Err("Should have 1 PostToolUse hook".into());
    }

    Ok(())
}

fn test_hook_priority() -> Result<(), String> {
    let mut manager = HookManager::new();

    manager.register(TestHook::new("low", vec![HookEvent::PreToolUse], 1));
    manager.register(TestHook::new("high", vec![HookEvent::PreToolUse], 100));
    manager.register(TestHook::new("medium", vec![HookEvent::PreToolUse], 50));

    let hooks = manager.hooks_for_event(HookEvent::PreToolUse);
    if hooks.len() != 3 {
        return Err("Should have 3 hooks".into());
    }

    if hooks[0].priority() != 100 {
        return Err("First should be priority 100".into());
    }
    if hooks[1].priority() != 50 {
        return Err("Second should be priority 50".into());
    }
    if hooks[2].priority() != 1 {
        return Err("Third should be priority 1".into());
    }

    Ok(())
}

// =============================================================================
// Section 3: Session Manager
// =============================================================================

async fn test_session_create() -> Result<(), String> {
    let manager = SessionManager::in_memory();
    let session = manager
        .create(SessionConfig::default())
        .await
        .map_err(|e| e.to_string())?;

    if session.state != SessionState::Created {
        return Err("State should be Created".into());
    }
    if !session.messages.is_empty() {
        return Err("Should have no messages".into());
    }

    Ok(())
}

async fn test_session_update() -> Result<(), String> {
    let manager = SessionManager::in_memory();
    let mut session = manager
        .create(SessionConfig::default())
        .await
        .map_err(|e| e.to_string())?;
    let id = session.id;

    session.summary = Some("Updated summary".into());
    manager.update(&session).await.map_err(|e| e.to_string())?;

    let restored = manager.get(&id).await.map_err(|e| e.to_string())?;
    if restored.summary != Some("Updated summary".into()) {
        return Err("Summary not updated".into());
    }

    Ok(())
}

async fn test_session_messages() -> Result<(), String> {
    use claude_agent::session::SessionMessage;

    let manager = SessionManager::in_memory();
    let session = manager
        .create(SessionConfig::default())
        .await
        .map_err(|e| e.to_string())?;
    let id = session.id;

    manager
        .add_message(&id, SessionMessage::user(vec![ContentBlock::text("Hello")]))
        .await
        .map_err(|e| e.to_string())?;
    manager
        .add_message(
            &id,
            SessionMessage::assistant(vec![ContentBlock::text("Hi!")]),
        )
        .await
        .map_err(|e| e.to_string())?;

    let restored = manager.get(&id).await.map_err(|e| e.to_string())?;
    if restored.messages.len() != 2 {
        return Err(format!(
            "Expected 2 messages, got {}",
            restored.messages.len()
        ));
    }

    Ok(())
}

async fn test_session_fork() -> Result<(), String> {
    use claude_agent::session::SessionMessage;

    let manager = SessionManager::in_memory();
    let session = manager
        .create(SessionConfig::default())
        .await
        .map_err(|e| e.to_string())?;
    let id = session.id;

    manager
        .add_message(&id, SessionMessage::user(vec![ContentBlock::text("Hello")]))
        .await
        .map_err(|e| e.to_string())?;
    manager
        .add_message(
            &id,
            SessionMessage::assistant(vec![ContentBlock::text("Hi!")]),
        )
        .await
        .map_err(|e| e.to_string())?;

    let forked = manager.fork(&id).await.map_err(|e| e.to_string())?;

    if forked.id == id {
        return Err("Forked should have different ID".into());
    }
    if forked.messages.len() != 2 {
        return Err("Forked should have 2 messages".into());
    }
    if !forked.messages.iter().all(|m| m.is_sidechain) {
        return Err("Messages should be sidechain".into());
    }

    Ok(())
}

async fn test_session_lifecycle() -> Result<(), String> {
    let manager = SessionManager::in_memory();
    let session = manager
        .create(SessionConfig::default())
        .await
        .map_err(|e| e.to_string())?;
    let id = session.id;

    if session.state != SessionState::Created {
        return Err("Initial state wrong".into());
    }

    manager.complete(&id).await.map_err(|e| e.to_string())?;
    let completed = manager.get(&id).await.map_err(|e| e.to_string())?;
    if completed.state != SessionState::Completed {
        return Err("Should be Completed".into());
    }

    let session2 = manager
        .create(SessionConfig::default())
        .await
        .map_err(|e| e.to_string())?;
    let id2 = session2.id;

    manager.set_error(&id2).await.map_err(|e| e.to_string())?;
    let errored = manager.get(&id2).await.map_err(|e| e.to_string())?;
    if errored.state != SessionState::Failed {
        return Err("Should be Failed".into());
    }

    Ok(())
}

async fn test_session_tenant() -> Result<(), String> {
    let manager = SessionManager::in_memory();

    manager
        .create_with_identity(SessionConfig::default(), "tenant-a", "user-1")
        .await
        .map_err(|e| e.to_string())?;
    manager
        .create_with_identity(SessionConfig::default(), "tenant-a", "user-2")
        .await
        .map_err(|e| e.to_string())?;
    manager
        .create_with_identity(SessionConfig::default(), "tenant-b", "user-3")
        .await
        .map_err(|e| e.to_string())?;

    let all = manager.list().await.map_err(|e| e.to_string())?;
    if all.len() != 3 {
        return Err(format!("Expected 3, got {}", all.len()));
    }

    let tenant_a = manager
        .list_for_tenant("tenant-a")
        .await
        .map_err(|e| e.to_string())?;
    if tenant_a.len() != 2 {
        return Err(format!("Tenant A: expected 2, got {}", tenant_a.len()));
    }

    let tenant_b = manager
        .list_for_tenant("tenant-b")
        .await
        .map_err(|e| e.to_string())?;
    if tenant_b.len() != 1 {
        return Err(format!("Tenant B: expected 1, got {}", tenant_b.len()));
    }

    Ok(())
}

// =============================================================================
// Section 4: Subagent System
// =============================================================================

fn test_subagent_definition() -> Result<(), String> {
    let subagent = SubagentIndex::new("reviewer", "Code reviewer")
        .source(ContentSource::in_memory("Review the code"))
        .tools(["Read", "Grep", "Glob"])
        .model("claude-haiku-4-5-20251001");

    if subagent.name != "reviewer" {
        return Err("Name mismatch".into());
    }
    if subagent.description != "Code reviewer" {
        return Err("Description mismatch".into());
    }
    if subagent.allowed_tools.len() != 3 {
        return Err("Should have 3 tools".into());
    }

    Ok(())
}

fn test_builtin_subagents() -> Result<(), String> {
    let builtins = builtin_subagents();

    if builtins.is_empty() {
        return Err("Should have builtin subagents".into());
    }

    let names: Vec<_> = builtins.iter().map(|s| s.name.as_str()).collect();
    if !names.contains(&"explore") {
        return Err("Missing explore".into());
    }
    if !names.contains(&"plan") {
        return Err("Missing plan".into());
    }
    if !names.contains(&"general") {
        return Err("Missing general".into());
    }

    Ok(())
}

fn test_subagent_tools() -> Result<(), String> {
    use claude_agent::common::ToolRestricted;

    let restricted = SubagentIndex::new("limited", "Limited agent")
        .source(ContentSource::in_memory("Do limited things"))
        .tools(["Read", "Grep"]);

    if !restricted.has_tool_restrictions() {
        return Err("Should have restrictions".into());
    }
    if !restricted.is_tool_allowed("Read") {
        return Err("Read should be allowed".into());
    }
    if !restricted.is_tool_allowed("Grep") {
        return Err("Grep should be allowed".into());
    }
    if restricted.is_tool_allowed("Bash") {
        return Err("Bash should not be allowed".into());
    }

    let unrestricted =
        SubagentIndex::new("general", "General").source(ContentSource::in_memory("Do anything"));
    if unrestricted.has_tool_restrictions() {
        return Err("Should not have restrictions".into());
    }
    if !unrestricted.is_tool_allowed("Anything") {
        return Err("Should allow anything".into());
    }

    Ok(())
}

fn test_subagent_model() -> Result<(), String> {
    use claude_agent::client::{ModelConfig, ModelType};

    let config = ModelConfig::default();

    let direct = SubagentIndex::new("direct", "Direct")
        .source(ContentSource::in_memory("Use direct"))
        .model("custom-model");
    if direct.resolve_model(&config) != "custom-model" {
        return Err("Direct model mismatch".into());
    }

    let haiku = SubagentIndex::new("fast", "Fast")
        .source(ContentSource::in_memory("Be fast"))
        .model("haiku");
    if !haiku.resolve_model(&config).contains("haiku") {
        return Err("Haiku alias failed".into());
    }

    let typed = SubagentIndex::new("typed", "Typed")
        .source(ContentSource::in_memory("Use type"))
        .model_type(ModelType::Small);
    if !typed.resolve_model(&config).contains("haiku") {
        return Err("ModelType fallback failed".into());
    }

    Ok(())
}
