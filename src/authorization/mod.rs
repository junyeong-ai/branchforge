//! Authorization system for controlling tool execution.
//!
//! # Design
//!
//! This module owns **policy evaluation** (rule DSL, permission decisions,
//! execution modes, approval channels). It does NOT own **subject
//! extraction** — that is the responsibility of each tool via
//! [`crate::tools::Tool::permission_subjects`]. Keeping extraction next
//! to the tool definition makes the "what string does `Bash(rm:*)`
//! match against?" question answerable by reading the tool's own
//! file, and prevents the parallel-registry anti-pattern flagged in
//! `.claude/rules/naming.md`.
//!
//! The previous `InputExtractor`/`FieldExtractor`/`default_extractors`
//! module was deleted in Phase D Workstream A-1 as a dual-system
//! violation.

pub mod approval;
mod denied;
pub mod dsl;
pub mod human;
mod modes;
mod rules;

pub use approval::DEFAULT_APPROVAL_TIMEOUT_SECS;
pub use denied::AuthorizationDenied;
pub use dsl::{
    PermissionDslError, PermissionRuleSyntax, RuleDecisionKeyword, SubjectPattern,
    parse_permission_rule, parse_to_tool_rule,
};
pub use human::{
    ElicitationRequest, ElicitationResponse, HumanInteractionError, HumanInteractionExtension,
    HumanInteractionHandler, HumanInteractionResult, Question, QuestionRequest, QuestionResponse,
    ToolApprovalRequest, ToolApprovalResponse,
};
pub use modes::ExecutionMode;
pub use rules::{
    PermissionDecision, PermissionDeniedReason, ToolLimits, ToolPolicy, ToolPolicyBuilder,
    ToolRule, ToolRuleDecision,
};
