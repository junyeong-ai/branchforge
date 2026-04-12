//! Tool policy rules and evaluation.

#![allow(missing_docs)]

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

fn anchor_pattern(pattern: &str) -> String {
    let has_start = pattern.starts_with('^');
    let has_end = pattern.ends_with('$');
    match (has_start, has_end) {
        (true, true) => pattern.to_string(),
        (true, false) => format!("{}$", pattern),
        (false, true) => format!("^{}", pattern),
        (false, false) => format!("^{}$", pattern),
    }
}

/// Reason a permission check denied or deferred an operation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionDeniedReason {
    PolicyDeny(String),
    PlanModeBlocked,
    SupervisedReview,
    NotRegistered,
    HookBlocked(String),
    BudgetExceeded,
    NoMatchingRule,
    Custom(String),
}

impl std::fmt::Display for PermissionDeniedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PolicyDeny(msg) => write!(f, "{}", msg),
            Self::PlanModeBlocked => write!(f, "blocked in plan mode"),
            Self::SupervisedReview => write!(f, "requires supervised review"),
            Self::NotRegistered => write!(f, "tool not registered"),
            Self::HookBlocked(msg) => write!(f, "blocked by hook: {}", msg),
            Self::BudgetExceeded => write!(f, "budget exceeded"),
            Self::NoMatchingRule => write!(f, "no matching rule: tool not explicitly allowed"),
            Self::Custom(msg) => write!(f, "{}", msg),
        }
    }
}

impl crate::decision::DecisionReason for PermissionDeniedReason {
    fn category(&self) -> &'static str {
        match self {
            Self::PolicyDeny(_) => "policy_deny",
            Self::PlanModeBlocked => "plan_mode",
            Self::SupervisedReview => "supervised_review",
            Self::NotRegistered => "not_registered",
            Self::HookBlocked(_) => "hook_blocked",
            Self::BudgetExceeded => "budget_exceeded",
            Self::NoMatchingRule => "no_matching_rule",
            Self::Custom(_) => "custom",
        }
    }

    fn summary(&self) -> String {
        self.to_string()
    }
}

/// Decision for a tool policy check.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny { reason: PermissionDeniedReason },
    Ask { reason: PermissionDeniedReason },
}

impl PermissionDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    pub fn is_denied(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Deny { reason } | Self::Ask { reason } => match reason {
                PermissionDeniedReason::PolicyDeny(msg)
                | PermissionDeniedReason::HookBlocked(msg)
                | PermissionDeniedReason::Custom(msg) => msg,
                PermissionDeniedReason::PlanModeBlocked => "blocked in plan mode",
                PermissionDeniedReason::SupervisedReview => "requires supervised review",
                PermissionDeniedReason::NotRegistered => "tool not registered",
                PermissionDeniedReason::BudgetExceeded => "budget exceeded",
                PermissionDeniedReason::NoMatchingRule => {
                    "no matching rule: tool not explicitly allowed"
                }
            },
            _ => "",
        }
    }

    pub fn allowed() -> Self {
        Self::Allow
    }

    pub fn denied(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: PermissionDeniedReason::Custom(reason.into()),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_size: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_paths: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_paths: Option<Vec<String>>,
}

impl ToolLimits {
    pub fn timeout(timeout_ms: u64) -> Self {
        Self {
            timeout_ms: Some(timeout_ms),
            ..Default::default()
        }
    }

    pub fn max_output(max_bytes: usize) -> Self {
        Self {
            max_output_size: Some(max_bytes),
            ..Default::default()
        }
    }

    pub fn allowed_paths(mut self, paths: Vec<String>) -> Self {
        self.allowed_paths = Some(paths);
        self
    }

    pub fn denied_paths(mut self, paths: Vec<String>) -> Self {
        self.denied_paths = Some(paths);
        self
    }
}

/// Whether a rule allows or denies.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolRuleDecision {
    Allow,
    #[default]
    Deny,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolRule {
    pub pattern: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_pattern: Option<String>,

    pub decision: ToolRuleDecision,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,

    #[serde(skip)]
    compiled: Option<Regex>,
}

impl ToolRule {
    /// Build a rule from a DSL string with the supplied default
    /// decision (used when the string does not include an explicit
    /// `allow` / `deny` / `ask` keyword).
    ///
    /// Accepts the canonical rule grammar:
    ///
    /// ```text
    /// rule       := decision? tool ( "(" subject ")" )?
    /// decision   := "allow" | "deny" | "ask"
    /// subject    := bare | glob | prefix_wild | domain
    /// ```
    ///
    /// See [`super::dsl`] for the full grammar and subject classifier.
    ///
    /// # Phase D E-3: fallible surface
    ///
    /// Returns a typed [`super::dsl::PermissionDslError`] on parse
    /// or regex-compile failure. Config loaders should surface this
    /// as an [`crate::Error::Config`]; in-process callers with
    /// hardcoded, known-valid strings can use [`Self::allow`] /
    /// [`Self::deny`], which panic on malformed input (trust for
    /// compile-time constants, not for user-supplied config).
    pub fn from_dsl(
        rule_str: &str,
        default_decision: ToolRuleDecision,
    ) -> Result<Self, super::dsl::PermissionDslError> {
        super::dsl::parse_to_tool_rule(rule_str, default_decision)
    }

    /// Convenience for hardcoded, statically known rule strings
    /// (e.g. `ToolRule::allow(".*")` inside `ToolSurface`). Panics
    /// on a malformed rule — use [`Self::from_dsl`] for user
    /// input. The panic message is stable and points at the
    /// offending pattern.
    pub fn allow(rule_str: &str) -> Self {
        Self::from_dsl(rule_str, ToolRuleDecision::Allow)
            .unwrap_or_else(|e| panic!("invalid permission rule `{rule_str}`: {e}"))
    }

    /// Convenience for hardcoded, statically known deny rules.
    /// See [`Self::allow`] for the panic contract.
    pub fn deny(rule_str: &str) -> Self {
        Self::from_dsl(rule_str, ToolRuleDecision::Deny)
            .unwrap_or_else(|e| panic!("invalid permission rule `{rule_str}`: {e}"))
    }

    /// Crate-internal raw constructor used by the DSL parser after
    /// the grammar has accepted `pattern`. Skips the DSL grammar
    /// step but still compiles the anchored regex, which is where
    /// the `InvalidToolPattern` failure mode is caught.
    pub(crate) fn try_new_from_dsl(
        pattern: impl Into<String>,
        input_pattern: Option<String>,
        decision: ToolRuleDecision,
    ) -> Result<Self, super::dsl::PermissionDslError> {
        let pattern = pattern.into();
        let anchored = anchor_pattern(&pattern);
        let compiled = Regex::new(&anchored).map_err(|err| {
            super::dsl::PermissionDslError::InvalidToolPattern {
                pattern: pattern.clone(),
                details: err.to_string(),
            }
        })?;
        Ok(Self {
            pattern,
            input_pattern,
            decision,
            reason: None,
            compiled: Some(compiled),
        })
    }

    pub fn input_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.input_pattern = Some(pattern.into());
        self
    }

    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    pub fn compile(&mut self) -> Result<(), regex::Error> {
        self.compiled = Some(Regex::new(&anchor_pattern(&self.pattern))?);
        Ok(())
    }

    pub fn matches(&self, tool_name: &str) -> bool {
        if let Some(ref regex) = self.compiled {
            regex.is_match(tool_name)
        } else if let Ok(regex) = Regex::new(&anchor_pattern(&self.pattern)) {
            regex.is_match(tool_name)
        } else {
            self.pattern == tool_name
        }
    }

    /// Test whether this rule applies to a tool call given the subjects
    /// that tool extracted from its own input.
    ///
    /// `subjects` is produced by [`crate::tools::Tool::permission_subjects`]
    /// — the tool is the single source of truth for "what strings should
    /// `Tool(pattern)` match against?". A rule matches if any subject
    /// satisfies the pattern. An empty subjects slice with a present
    /// `input_pattern` means "no match" (fail-closed).
    pub fn matches_with_subjects(&self, tool_name: &str, subjects: &[String]) -> bool {
        if !self.matches(tool_name) {
            return false;
        }

        let Some(pattern) = &self.input_pattern else {
            return true;
        };

        // Domain patterns (e.g. `WebFetch(domain:github.com)`) require
        // URL parsing rather than raw prefix matching. The tool's
        // `permission_subjects` already returns the full URL as a
        // subject; the rule walks the subjects and applies the
        // domain-aware matcher.
        if let Some(domain) = pattern.strip_prefix("domain:") {
            return subjects.iter().any(|s| Self::matches_domain(s, domain));
        }

        subjects.iter().any(|s| self.match_pattern(pattern, s))
    }

    fn match_pattern(&self, pattern: &str, input: &str) -> bool {
        if pattern.ends_with(":*") || pattern.ends_with("**") {
            let prefix = &pattern[..pattern.len() - 2];
            input.starts_with(prefix)
        } else if pattern.contains('*') {
            let parts: Vec<&str> = pattern.split('*').collect();
            if parts.len() == 2 {
                input.starts_with(parts[0]) && input.ends_with(parts[1])
            } else {
                input == pattern
            }
        } else {
            input == pattern || std::path::Path::new(input).starts_with(pattern)
        }
    }

    /// Securely match a URL against an allowed domain.
    ///
    /// Extracts the actual host from the URL and checks for:
    /// - Exact domain match (e.g., "github.com")
    /// - Subdomain match (e.g., "api.github.com" matches "github.com")
    ///
    /// This prevents bypass attacks like:
    /// - `evil.github.com.attacker.com` (subdomain of attacker.com, not github.com)
    /// - `https://attacker.com?redirect=github.com` (domain in query string)
    fn matches_domain(url_str: &str, domain: &str) -> bool {
        let Ok(parsed) = url::Url::parse(url_str) else {
            return false;
        };
        let Some(host) = parsed.host_str() else {
            return false;
        };
        host == domain || host.ends_with(&format!(".{}", domain))
    }
}

#[derive(Clone, Default, Debug)]
pub struct ToolPolicy {
    pub rules: Vec<ToolRule>,
    pub tool_limits: HashMap<String, ToolLimits>,
}

impl ToolPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn builder() -> ToolPolicyBuilder {
        ToolPolicyBuilder::new()
    }

    /// A permissive policy that allows all tools.
    pub fn permissive() -> Self {
        Self::builder().allow(".*").build()
    }

    /// Check a tool call against the policy's rules.
    ///
    /// `subjects` is the tool's self-declared list of subjects for this
    /// input, obtained by the caller via
    /// [`crate::tools::Tool::permission_subjects`]. The policy is the
    /// rule engine; the tool is the extractor. See the module docs for
    /// the rationale.
    ///
    /// Evaluation order:
    /// 1. Deny rules — if any match, return `Deny`.
    /// 2. Allow rules — if any match, return `Allow`.
    /// 3. Default — `Deny { reason: NoMatchingRule }`.
    pub fn check(&self, tool_name: &str, subjects: &[String]) -> PermissionDecision {
        for rule in self
            .rules
            .iter()
            .filter(|r| r.decision == ToolRuleDecision::Deny)
        {
            if rule.matches_with_subjects(tool_name, subjects) {
                return PermissionDecision::Deny {
                    reason: PermissionDeniedReason::PolicyDeny(
                        rule.reason
                            .clone()
                            .unwrap_or_else(|| format!("Denied by rule: {}", rule.pattern)),
                    ),
                };
            }
        }

        for rule in self
            .rules
            .iter()
            .filter(|r| r.decision == ToolRuleDecision::Allow)
        {
            if rule.matches_with_subjects(tool_name, subjects) {
                return PermissionDecision::Allow;
            }
        }

        PermissionDecision::Deny {
            reason: PermissionDeniedReason::NoMatchingRule,
        }
    }

    /// Check permission for an explicit user-requested skill invocation
    /// such as `/review-pr`.
    ///
    /// Intentionally distinct from model-driven `Skill` tool use:
    /// - deny rules still take precedence
    /// - allow rules are honoured
    /// - if no rule matches, the explicit wrapper invocation is allowed
    ///   and nested tool usage remains governed by the delegated runtime
    ///   policy.
    pub fn check_explicit_skill(&self, subjects: &[String]) -> PermissionDecision {
        for rule in self
            .rules
            .iter()
            .filter(|r| r.decision == ToolRuleDecision::Deny)
        {
            if rule.matches_with_subjects("Skill", subjects) {
                return PermissionDecision::Deny {
                    reason: PermissionDeniedReason::PolicyDeny(
                        rule.reason
                            .clone()
                            .unwrap_or_else(|| format!("Denied by rule: {}", rule.pattern)),
                    ),
                };
            }
        }

        for rule in self
            .rules
            .iter()
            .filter(|r| r.decision == ToolRuleDecision::Allow)
        {
            if rule.matches_with_subjects("Skill", subjects) {
                return PermissionDecision::Allow;
            }
        }

        PermissionDecision::Allow
    }

    pub fn limits(&self, tool_name: &str) -> Option<&ToolLimits> {
        self.tool_limits.get(tool_name)
    }

    pub fn set_limits(&mut self, tool_name: impl Into<String>, limits: ToolLimits) {
        self.tool_limits.insert(tool_name.into(), limits);
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToolPolicyBuilder {
    policy: ToolPolicy,
}

impl ToolPolicyBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow(mut self, pattern: &str) -> Self {
        self.policy.rules.push(ToolRule::allow(pattern));
        self
    }

    pub fn deny(mut self, pattern: &str) -> Self {
        self.policy.rules.push(ToolRule::deny(pattern));
        self
    }

    pub fn rule(mut self, rule: ToolRule) -> Self {
        self.policy.rules.push(rule);
        self
    }

    pub fn tool_limits(mut self, tool_name: impl Into<String>, limits: ToolLimits) -> Self {
        self.policy.tool_limits.insert(tool_name.into(), limits);
        self
    }

    pub fn build(mut self) -> ToolPolicy {
        for rule in &mut self.policy.rules {
            let _ = rule.compile();
        }
        self.policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Test helper mirroring what each tool's `permission_subjects`
    /// returns. Used to drive `ToolPolicy::check` from tests without
    /// depending on the whole Tool trait — keeps the rules module
    /// testable in isolation.
    fn subjects(tool: &str, input: &Value) -> Vec<String> {
        let field = |key: &str| {
            input
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .into_iter()
                .collect::<Vec<_>>()
        };
        match tool {
            "Bash" => field("command"),
            "Skill" => field("skill"),
            "Read" | "Write" | "Edit" => field("file_path"),
            "Glob" | "Grep" => field("path"),
            "WebFetch" => field("url"),
            _ => Vec::new(),
        }
    }

    fn check(policy: &ToolPolicy, tool: &str, input: &Value) -> PermissionDecision {
        policy.check(tool, &subjects(tool, input))
    }

    #[test]
    fn test_tool_decision() {
        let allowed = PermissionDecision::Allow;
        assert!(allowed.is_allowed());
        assert!(!allowed.is_denied());

        let denied = PermissionDecision::denied("test");
        assert!(!denied.is_allowed());
        assert!(denied.is_denied());
        assert_eq!(denied.reason(), "test");
    }

    #[test]
    fn test_tool_rule_exact_match() {
        let rule = ToolRule::allow("Read");
        assert!(rule.matches("Read"));
        assert!(!rule.matches("Write"));
    }

    #[test]
    fn test_tool_rule_regex() {
        let mut rule = ToolRule::allow("Read|Write|Edit");
        rule.compile().unwrap();
        assert!(rule.matches("Read"));
        assert!(rule.matches("Write"));
        assert!(rule.matches("Edit"));
        assert!(!rule.matches("Bash"));
    }

    #[test]
    fn test_scoped_rule() {
        let rule = ToolRule::allow("Bash(git:*)");
        assert_eq!(rule.pattern, "Bash");
        assert_eq!(rule.input_pattern, Some("git:*".to_string()));
    }

    #[test]
    fn test_skill_scoped_rule_matches_skill_name() {
        let policy = ToolPolicy::builder()
            .allow(".*")
            .deny("Skill(internal)")
            .build();
        assert!(check(&policy, "Skill", &serde_json::json!({"skill": "internal"})).is_denied());
        assert!(check(&policy, "Skill", &serde_json::json!({"skill": "commit"})).is_allowed());
    }

    #[test]
    fn test_policy_permissive() {
        let policy = ToolPolicy::permissive();
        let result = check(&policy, "AnyTool", &Value::Null);
        assert!(result.is_allowed());
    }

    #[test]
    fn test_policy_deny_takes_precedence() {
        let policy = ToolPolicy::builder().allow(".*").deny("Write").build();

        assert!(check(&policy, "Read", &Value::Null).is_allowed());
        assert!(check(&policy, "Write", &Value::Null).is_denied());
    }

    #[test]
    fn test_policy_allow_rules() {
        let policy = ToolPolicy::builder().allow("Bash").allow("Read").build();

        assert!(check(&policy, "Bash", &Value::Null).is_allowed());
        assert!(check(&policy, "Read", &Value::Null).is_allowed());
        assert!(check(&policy, "Write", &Value::Null).is_denied());
    }

    #[test]
    fn test_scoped_allow() {
        let policy = ToolPolicy::builder().allow("Bash(git:*)").build();

        let git_input = serde_json::json!({"command": "git status"});
        let rm_input = serde_json::json!({"command": "rm -rf /"});

        assert!(check(&policy, "Bash", &git_input).is_allowed());
        assert!(check(&policy, "Bash", &rm_input).is_denied());
    }

    #[test]
    fn test_tool_limits() {
        let policy = ToolPolicy::builder()
            .tool_limits("Bash", ToolLimits::timeout(30000))
            .build();

        let limits = policy.limits("Bash").unwrap();
        assert_eq!(limits.timeout_ms, Some(30000));
        assert!(policy.limits("Read").is_none());
    }

    #[test]
    fn test_domain_filter() {
        let policy = ToolPolicy::builder()
            .allow("WebFetch(domain:github.com)")
            .build();

        let github_input = serde_json::json!({"url": "https://github.com/user/repo"});
        let other_input = serde_json::json!({"url": "https://example.com/page"});

        assert!(check(&policy, "WebFetch", &github_input).is_allowed());
        assert!(check(&policy, "WebFetch", &other_input).is_denied());
    }

    #[test]
    fn test_domain_filter_security() {
        let policy = ToolPolicy::builder()
            .allow("WebFetch(domain:github.com)")
            .build();

        // Should allow: exact domain and subdomains
        let exact = serde_json::json!({"url": "https://github.com/user/repo"});
        let subdomain = serde_json::json!({"url": "https://api.github.com/repos"});
        let with_port = serde_json::json!({"url": "https://github.com:443/path"});
        assert!(check(&policy, "WebFetch", &exact).is_allowed());
        assert!(check(&policy, "WebFetch", &subdomain).is_allowed());
        assert!(check(&policy, "WebFetch", &with_port).is_allowed());

        // Should deny: bypass attempts
        let fake_subdomain = serde_json::json!({"url": "https://github.com.attacker.com/path"});
        let query_bypass = serde_json::json!({"url": "https://attacker.com?url=github.com"});
        let path_bypass = serde_json::json!({"url": "https://attacker.com/github.com"});
        let partial_match = serde_json::json!({"url": "https://notgithub.com/page"});
        assert!(check(&policy, "WebFetch", &fake_subdomain).is_denied());
        assert!(check(&policy, "WebFetch", &query_bypass).is_denied());
        assert!(check(&policy, "WebFetch", &path_bypass).is_denied());
        assert!(check(&policy, "WebFetch", &partial_match).is_denied());
    }

    #[test]
    fn test_explicit_skill_invocation_allowed_in_default_mode() {
        let policy = ToolPolicy::default();
        let result = policy.check_explicit_skill(&subjects(
            "Skill",
            &serde_json::json!({"skill": "review-pr"}),
        ));
        assert!(result.is_allowed());
    }

    #[test]
    fn test_explicit_skill_invocation_respects_deny_rule() {
        let policy = ToolPolicy::builder().deny("Skill(internal)").build();

        assert!(
            policy
                .check_explicit_skill(&subjects(
                    "Skill",
                    &serde_json::json!({"skill": "review-pr"})
                ))
                .is_allowed()
        );
        assert!(
            policy
                .check_explicit_skill(&subjects(
                    "Skill",
                    &serde_json::json!({"skill": "internal"})
                ))
                .is_denied()
        );
    }

    #[test]
    fn test_matches_domain_helper() {
        // Exact match
        assert!(ToolRule::matches_domain(
            "https://github.com/path",
            "github.com"
        ));
        assert!(ToolRule::matches_domain("http://github.com", "github.com"));
        assert!(ToolRule::matches_domain(
            "https://github.com:443/path",
            "github.com"
        ));

        // Subdomain match
        assert!(ToolRule::matches_domain(
            "https://api.github.com/repos",
            "github.com"
        ));
        assert!(ToolRule::matches_domain(
            "https://raw.githubusercontent.com/f",
            "githubusercontent.com"
        ));

        // Security: should NOT match
        assert!(!ToolRule::matches_domain(
            "https://github.com.evil.com/x",
            "github.com"
        ));
        assert!(!ToolRule::matches_domain(
            "https://evil.com?r=github.com",
            "github.com"
        ));
        assert!(!ToolRule::matches_domain(
            "https://evil.com/github.com",
            "github.com"
        ));
        assert!(!ToolRule::matches_domain(
            "https://notgithub.com",
            "github.com"
        ));
        assert!(!ToolRule::matches_domain(
            "https://fakegithub.com",
            "github.com"
        ));
    }
}
