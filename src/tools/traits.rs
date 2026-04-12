//! Tool trait definitions.
//!
//! The `Tool` trait is the single source of truth for everything a tool
//! knows about itself — not just *what it does* (`execute`) but also
//! *what kind of tool it is* (read-only? concurrency-safe? destructive?
//! how big can its output get? what permission subjects does it touch?).
//!
//! Every capability query has a default implementation so existing
//! tools keep compiling, but the defaults are deliberately
//! **conservative** (fail-closed: assume unsafe, mutating, serial,
//! small output) so tools that do not explicitly declare otherwise
//! cannot accidentally be parallelised or bypass a permission check.
//!
//! The two-trait pattern (`Tool` for erased dispatch,
//! `SchemaTool` for typed implementations) is preserved: concrete tools
//! pick `SchemaTool` to get JSON-Schema auto-derivation and typed
//! metadata methods; the blanket impl converts them into `Tool`
//! automatically.

use std::any::Any;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use super::context::ExecutionContext;
use crate::types::{ToolResult, ToolSpec};

/// How the runtime should react when a user interrupts a running
/// Rich preflight validation error returned from
/// [`Tool::validate_input`] / [`SchemaTool::validate_input_typed`].
///
/// Carries a human-readable `message` plus an optional machine
/// `code` so callers can branch on known failure modes without
/// parsing strings. Implementations should prefer specific codes
/// over free-form text.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("tool input validation failed: {message}")]
pub struct ValidationError {
    pub message: String,
    pub code: Option<u32>,
}

impl ValidationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }

    pub fn with_code(mut self, code: u32) -> Self {
        self.code = Some(code);
        self
    }
}

/// Result of [`Tool::validate_input`]. `Ok(())` means the input is
/// structurally and semantically valid and the tool is ready to
/// execute; `Err` means the input is rejected before `execute` is
/// called. Side-effect-free by contract.
pub type ValidationResult = std::result::Result<(), ValidationError>;

/// Core tool trait for all tool implementations.
///
/// # Self-description methods
///
/// Beyond `execute`, every tool is expected to declare what kind of
/// operation it performs so the runtime can schedule, permission-
/// check, and context-manage it correctly. All metadata methods
/// have conservative defaults — tools that fit the defaults (an
/// unknown, mutating, serial tool) can ignore them; tools that are
/// read-only, concurrency-safe, or have tight permission subjects
/// override the relevant methods.
///
/// # Capability query contract
///
/// Input-aware capability queries (`is_read_only(&input)`, etc.)
/// take `&serde_json::Value` so the same tool can report different
/// capabilities depending on its arguments. The canonical example
/// is `Bash`: `Bash("ls")` is read-only and concurrency-safe,
/// `Bash("rm -rf /")` is neither. Tools that are capability-static
/// (Read, Write, Glob) ignore the input parameter.
#[async_trait]
pub trait Tool: Send + Sync {
    // ── Core required methods ────────────────────────────────────
    fn as_any(&self) -> &dyn Any;
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn execute(&self, input: serde_json::Value, context: &ExecutionContext) -> ToolResult;

    fn definition(&self) -> ToolSpec {
        ToolSpec::new(self.name(), self.description(), self.input_schema())
    }

    // ── Static metadata (default impls) ──────────────────────────

    /// Alternative names this tool responds to. Used by
    /// `ToolRegistry::get` for backward-compatible renames: if the
    /// primary name is `Read` and an alias is `File.Read`, both
    /// resolve to the same tool. Never include the primary name in
    /// the alias list — that would be a self-loop.
    fn aliases(&self) -> &[&'static str] {
        &[]
    }

    /// Short phrase (3-10 words) describing what this tool is used
    /// for, consumed by [`ToolSearchManager`](crate::tools::ToolSearchManager)
    /// to improve fuzzy selection. Defaults to the tool
    /// description's first line if unset.
    fn search_hint(&self) -> Option<&str> {
        None
    }

    /// Maximum size of tool output (in bytes) carried inline in
    /// the session graph. Larger outputs are spilled to the
    /// configured [`crate::tools::OverflowStore`] and the LLM
    /// sees a short preview plus an [`crate::tools::OverflowRef`].
    /// Wire a store via `ToolRegistry::builder().overflow_store(...)`.
    ///
    /// Default: 200 KB. Tools that routinely emit tiny results
    /// (a boolean, a status line) can lower this to tighten
    /// context; tools that must emit entire files (`Read`) can
    /// raise it.
    ///
    /// **Unit is bytes**, measured against the UTF-8 encoding of
    /// the result text (matching `String::len`). Multi-byte
    /// characters count for more than one unit of budget.
    fn max_result_size_bytes(&self) -> usize {
        200_000
    }

    // ── Input-aware capability queries (default: fail-closed) ────

    /// `true` if the tool, given this input, only reads state
    /// without mutating it. Read-only tools can run in parallel
    /// with other read-only tools. The default is `false`
    /// (assume mutation) to keep schedulers conservative.
    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }

    /// `true` if this tool with this input is safe to run
    /// concurrently with another invocation of the same tool (or
    /// with other tools). Default: `false`. A tool that is
    /// read-only is not automatically concurrency-safe — the
    /// concurrency-safety property includes "no shared mutable
    /// resource" (like cache files, temp directories) that might
    /// race even across read-only callers.
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }

    /// `true` if the tool, given this input, performs an
    /// irreversible action (delete file, drop table, `kill -9`,
    /// git `reset --hard`). Used by HITL permission flows to
    /// escalate approval.
    fn is_destructive(&self, _input: &serde_json::Value) -> bool {
        false
    }

    /// `true` if the tool cannot run in parallel because it
    /// requires exclusive user attention (TTY-based prompts,
    /// interactive editors). Blocks the scheduler from batching
    /// the call with anything else.
    fn requires_user_interaction(&self, _input: &serde_json::Value) -> bool {
        false
    }

    // ── Validation & permission ──────────────────────────────────

    /// Preflight validation that runs **before** `execute` and
    /// **must be side-effect-free**. A scheduler can call
    /// `validate_input` on a batch of pending tool calls in
    /// parallel, reject the invalid ones, and then serially
    /// execute the valid ones.
    ///
    /// The default implementation deserialises the input into
    /// `SchemaTool::Input` (for `SchemaTool` implementers) and
    /// reports deserialisation failures. Tools with semantic
    /// rules ("path must be inside workspace", "command must not
    /// contain `sudo`") override this.
    async fn validate_input(
        &self,
        _input: &serde_json::Value,
        _context: &ExecutionContext,
    ) -> ValidationResult {
        Ok(())
    }

    /// Subjects this tool invocation would operate on, in the
    /// form expected by the permission DSL (`Bash(rm:*)` →
    /// `rm`, `Read(/etc/passwd)` → `/etc/passwd`). Returns an
    /// empty vec for tools that have no rule-matchable subject.
    ///
    /// The permission engine uses this for rule matching: given
    /// `deny Bash(rm:*)`, the engine asks `Bash.permission_subjects(&input)`
    /// and matches each returned subject against the rule's
    /// `SubjectPattern`.
    ///
    /// A single invocation may produce multiple subjects (e.g.
    /// `MultiEdit` on several files). Order matters only when a
    /// matching rule is `ask` — the first matching subject wins.
    fn permission_subjects(&self, _input: &serde_json::Value) -> Vec<String> {
        Vec::new()
    }
}

/// Schema-based tool trait with automatic JSON schema generation
/// and typed metadata hooks.
///
/// Provides a higher-level abstraction over `Tool` with typed
/// inputs, automatic schema derivation via `schemars`, and typed
/// versions of every metadata method from `Tool`. The blanket
/// impl below converts any `SchemaTool` into a `Tool` by
/// deserialising the JSON input once per query and forwarding to
/// the `*_typed` variant; deserialisation failures fall back to
/// the constant defaults.
#[async_trait]
pub trait SchemaTool: Send + Sync {
    type Input: JsonSchema + DeserializeOwned + Send + Sync;

    const NAME: &'static str;
    const DESCRIPTION: &'static str;

    /// When `true`, the emitted schema's `additionalProperties`
    /// defaults to `false` (strict mode). Providers that validate
    /// tool input strictly (OpenAI strict, Anthropic strict tool
    /// use) respect this.
    const STRICT: bool = false;

    /// Static fallback for `Tool::is_read_only` when the typed
    /// method is not overridden. Prefer overriding
    /// `is_read_only_typed` for input-aware logic; use this
    /// constant only when the read-only status is invariant.
    const READ_ONLY: bool = false;

    /// Static alias list. Equivalent to implementing `aliases()`
    /// but as a const so the tool's declaration site is the
    /// single source of truth.
    const ALIASES: &'static [&'static str] = &[];

    /// Static search hint, consumed by `ToolSearchManager`.
    const SEARCH_HINT: Option<&'static str> = None;

    /// Static max result size in **bytes**. See
    /// [`Tool::max_result_size_bytes`].
    const MAX_RESULT_SIZE_BYTES: usize = 200_000;

    // ── Required entry point ─────────────────────────────────────
    async fn handle(&self, input: Self::Input, context: &ExecutionContext) -> ToolResult;

    /// Override to provide a dynamic description instead of the
    /// static `DESCRIPTION` constant. When this returns `Some(desc)`,
    /// the blanket `Tool` impl uses it in `definition()`.
    fn custom_description(&self) -> Option<String> {
        None
    }

    fn input_schema() -> serde_json::Value {
        let schema = schemars::schema_for!(Self::Input);
        let mut value =
            serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({"type": "object"}));

        if let Some(obj) = value.as_object_mut() {
            if !obj.contains_key("properties") {
                obj.insert(
                    "properties".to_string(),
                    serde_json::Value::Object(serde_json::Map::new()),
                );
            }
            if !obj.contains_key("additionalProperties") {
                obj.insert(
                    "additionalProperties".to_string(),
                    serde_json::Value::Bool(!Self::STRICT),
                );
            }
        }

        value
    }

    // ── Typed metadata methods (default to the const or conservative fail-closed) ──

    fn is_read_only_typed(&self, _input: &Self::Input) -> bool {
        Self::READ_ONLY
    }

    fn is_concurrency_safe_typed(&self, _input: &Self::Input) -> bool {
        false
    }

    fn is_destructive_typed(&self, _input: &Self::Input) -> bool {
        false
    }

    fn requires_user_interaction_typed(&self, _input: &Self::Input) -> bool {
        false
    }

    async fn validate_input_typed(
        &self,
        _input: &Self::Input,
        _context: &ExecutionContext,
    ) -> ValidationResult {
        Ok(())
    }

    fn permission_subjects_typed(&self, _input: &Self::Input) -> Vec<String> {
        Vec::new()
    }
}

#[async_trait]
impl<T: SchemaTool + 'static> Tool for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        T::NAME
    }

    fn description(&self) -> &str {
        T::DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        T::input_schema()
    }

    fn definition(&self) -> ToolSpec {
        let desc = self
            .custom_description()
            .unwrap_or_else(|| T::DESCRIPTION.to_string());
        let mut definition = ToolSpec::new(T::NAME, &desc, T::input_schema());
        if T::STRICT {
            definition = definition.strict(true);
        }
        definition
    }

    // Static metadata comes from the consts on the concrete type.
    fn aliases(&self) -> &[&'static str] {
        T::ALIASES
    }

    fn search_hint(&self) -> Option<&str> {
        T::SEARCH_HINT
    }

    fn max_result_size_bytes(&self) -> usize {
        T::MAX_RESULT_SIZE_BYTES
    }

    // Input-aware queries deserialize once per call and forward to
    // the typed variant; on deserialise failure they fall back to
    // the fail-closed static (READ_ONLY for is_read_only, `false`
    // for everything else).
    fn is_read_only(&self, input: &serde_json::Value) -> bool {
        match serde_json::from_value::<T::Input>(input.clone()) {
            Ok(typed) => T::is_read_only_typed(self, &typed),
            Err(_) => T::READ_ONLY,
        }
    }

    fn is_concurrency_safe(&self, input: &serde_json::Value) -> bool {
        match serde_json::from_value::<T::Input>(input.clone()) {
            Ok(typed) => T::is_concurrency_safe_typed(self, &typed),
            Err(_) => false,
        }
    }

    fn is_destructive(&self, input: &serde_json::Value) -> bool {
        match serde_json::from_value::<T::Input>(input.clone()) {
            Ok(typed) => T::is_destructive_typed(self, &typed),
            Err(_) => false,
        }
    }

    fn requires_user_interaction(&self, input: &serde_json::Value) -> bool {
        match serde_json::from_value::<T::Input>(input.clone()) {
            Ok(typed) => T::requires_user_interaction_typed(self, &typed),
            Err(_) => false,
        }
    }

    async fn validate_input(
        &self,
        input: &serde_json::Value,
        context: &ExecutionContext,
    ) -> ValidationResult {
        let typed = serde_json::from_value::<T::Input>(input.clone()).map_err(|e| {
            ValidationError::new(format!("invalid input for tool `{}`: {e}", T::NAME)).with_code(1)
        })?;
        T::validate_input_typed(self, &typed, context).await
    }

    fn permission_subjects(&self, input: &serde_json::Value) -> Vec<String> {
        match serde_json::from_value::<T::Input>(input.clone()) {
            Ok(typed) => T::permission_subjects_typed(self, &typed),
            Err(_) => Vec::new(),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: &ExecutionContext) -> ToolResult {
        match serde_json::from_value::<T::Input>(input.clone()) {
            Ok(typed) => SchemaTool::handle(self, typed, context).await,
            Err(e) => {
                let provided: Vec<&str> = input
                    .as_object()
                    .map(|obj| obj.keys().map(|k| k.as_str()).collect())
                    .unwrap_or_default();
                let hint = if provided.is_empty() {
                    String::new()
                } else {
                    format!("\n  Provided fields: [{}]", provided.join(", "))
                };
                ToolResult::error(format!("Invalid input for {}: {}{}", T::NAME, e, hint))
            }
        }
    }
}
