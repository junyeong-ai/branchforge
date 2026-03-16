//! Tool policy rules and evaluation.

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// Decision for a tool policy check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolDecision {
    Allow,
    Deny { reason: String },
}

impl ToolDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    pub fn is_denied(&self) -> bool {
        !self.is_allowed()
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Deny { reason } => reason,
            _ => "",
        }
    }

    pub fn allowed(reason: impl Into<String>) -> Self {
        let _ = reason.into(); // kept for compat but Allow carries no reason
        Self::Allow
    }

    pub fn denied(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
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
    pub fn allow(pattern: impl Into<String>) -> Self {
        Self::new(pattern, ToolRuleDecision::Allow)
    }

    pub fn deny(pattern: impl Into<String>) -> Self {
        Self::new(pattern, ToolRuleDecision::Deny)
    }

    fn new(pattern: impl Into<String>, decision: ToolRuleDecision) -> Self {
        let pattern = pattern.into();
        let anchored = anchor_pattern(&pattern);
        let compiled = Regex::new(&anchored).ok();
        Self {
            pattern,
            input_pattern: None,
            decision,
            reason: None,
            compiled,
        }
    }

    pub fn from_scoped(scoped: &str, decision: ToolRuleDecision) -> Self {
        if let Some((tool, scope)) = Self::parse_scope(scoped) {
            let anchored = anchor_pattern(&tool);
            let compiled = Regex::new(&anchored).ok();
            Self {
                pattern: tool,
                input_pattern: Some(scope),
                decision,
                reason: None,
                compiled,
            }
        } else {
            Self::new(scoped, decision)
        }
    }

    pub fn allow_scoped(scoped: &str) -> Self {
        Self::from_scoped(scoped, ToolRuleDecision::Allow)
    }

    pub fn deny_scoped(scoped: &str) -> Self {
        Self::from_scoped(scoped, ToolRuleDecision::Deny)
    }

    /// Create a rule from a pattern string, auto-detecting scoped patterns like `Bash(git:*)`.
    pub fn allow_pattern(pattern: impl Into<String>) -> Self {
        let p = pattern.into();
        if p.contains('(') {
            Self::allow_scoped(&p)
        } else {
            Self::allow(p)
        }
    }

    /// Create a deny rule from a pattern string, auto-detecting scoped patterns.
    pub fn deny_pattern(pattern: impl Into<String>) -> Self {
        let p = pattern.into();
        if p.contains('(') {
            Self::deny_scoped(&p)
        } else {
            Self::deny(p)
        }
    }

    fn parse_scope(s: &str) -> Option<(String, String)> {
        let start = s.find('(')?;
        let end = s.rfind(')')?;
        if start < end {
            Some((s[..start].to_string(), s[start + 1..end].to_string()))
        } else {
            None
        }
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

    pub fn matches_with_input(&self, tool_name: &str, input: &Value) -> bool {
        if !self.matches(tool_name) {
            return false;
        }

        match &self.input_pattern {
            Some(pattern) => self.match_input_pattern(pattern, tool_name, input),
            None => true,
        }
    }

    fn match_input_pattern(&self, pattern: &str, tool_name: &str, input: &Value) -> bool {
        let input_str = match tool_name {
            "Bash" => input.get("command").and_then(|v| v.as_str()),
            "Skill" => input.get("skill").and_then(|v| v.as_str()),
            "Read" | "Write" | "Edit" => input.get("file_path").and_then(|v| v.as_str()),
            "Glob" | "Grep" => input.get("path").and_then(|v| v.as_str()),
            "WebFetch" => {
                if let Some(domain) = pattern.strip_prefix("domain:") {
                    return input
                        .get("url")
                        .and_then(|v| v.as_str())
                        .map(|url| Self::matches_domain(url, domain))
                        .unwrap_or(false);
                }
                input.get("url").and_then(|v| v.as_str())
            }
            _ => None,
        };

        let Some(input_str) = input_str else {
            return false;
        };

        self.match_pattern(pattern, input_str)
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
            input == pattern || input.starts_with(&format!("{}/", pattern))
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
    fn matches_domain(url: &str, domain: &str) -> bool {
        // Extract host from URL
        let host = url
            // Remove protocol
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(url)
            // Take only the host part (before path, query, or port)
            .split('/')
            .next()
            .unwrap_or("")
            .split('?')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("");

        // Check exact match or subdomain match
        host == domain || host.ends_with(&format!(".{}", domain))
    }
}

#[derive(Clone, Debug, Default)]
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

    /// Check a tool against rules only.
    ///
    /// - First check deny rules: if any match, return Deny.
    /// - Then check allow rules: if any match, return Allow.
    /// - Default: Deny("no matching rule").
    pub fn check(&self, tool_name: &str, input: &Value) -> ToolDecision {
        // Deny rules first (highest priority)
        for rule in self
            .rules
            .iter()
            .filter(|r| r.decision == ToolRuleDecision::Deny)
        {
            if rule.matches_with_input(tool_name, input) {
                return ToolDecision::denied(
                    rule.reason
                        .clone()
                        .unwrap_or_else(|| format!("Denied by rule: {}", rule.pattern)),
                );
            }
        }

        // Allow rules
        for rule in self
            .rules
            .iter()
            .filter(|r| r.decision == ToolRuleDecision::Allow)
        {
            if rule.matches_with_input(tool_name, input) {
                return ToolDecision::Allow;
            }
        }

        // Default: deny
        ToolDecision::denied("No matching rule: tool not explicitly allowed")
    }

    /// Check permission for an explicit user-requested skill invocation such as `/review-pr`.
    ///
    /// This is intentionally distinct from model-driven `Skill` tool use:
    /// - deny rules still take precedence
    /// - allow rules are honored
    /// - if no rule matches, the explicit wrapper invocation is allowed and
    ///   nested tool usage remains governed by the delegated runtime policy
    pub fn check_explicit_skill(&self, input: &Value) -> ToolDecision {
        for rule in self
            .rules
            .iter()
            .filter(|r| r.decision == ToolRuleDecision::Deny)
        {
            if rule.matches_with_input("Skill", input) {
                return ToolDecision::denied(
                    rule.reason
                        .clone()
                        .unwrap_or_else(|| format!("Denied by rule: {}", rule.pattern)),
                );
            }
        }

        for rule in self
            .rules
            .iter()
            .filter(|r| r.decision == ToolRuleDecision::Allow)
        {
            if rule.matches_with_input("Skill", input) {
                return ToolDecision::Allow;
            }
        }

        ToolDecision::Allow
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

    pub fn allow(mut self, pattern: impl Into<String>) -> Self {
        self.policy.rules.push(ToolRule::allow_pattern(pattern));
        self
    }

    pub fn deny(mut self, pattern: impl Into<String>) -> Self {
        self.policy.rules.push(ToolRule::deny_pattern(pattern));
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

    #[test]
    fn test_tool_decision() {
        let allowed = ToolDecision::Allow;
        assert!(allowed.is_allowed());
        assert!(!allowed.is_denied());

        let denied = ToolDecision::denied("test");
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
        let rule = ToolRule::allow_scoped("Bash(git:*)");
        assert_eq!(rule.pattern, "Bash");
        assert_eq!(rule.input_pattern, Some("git:*".to_string()));
    }

    #[test]
    fn test_skill_scoped_rule_matches_skill_name() {
        let rule = ToolRule::deny_scoped("Skill(internal)");
        assert!(rule.matches_with_input("Skill", &serde_json::json!({"skill": "internal"})));
        assert!(!rule.matches_with_input("Skill", &serde_json::json!({"skill": "commit"})));
    }

    #[test]
    fn test_policy_permissive() {
        let policy = ToolPolicy::permissive();
        let result = policy.check("AnyTool", &Value::Null);
        assert!(result.is_allowed());
    }

    #[test]
    fn test_policy_deny_takes_precedence() {
        let policy = ToolPolicy::builder().allow(".*").deny("Write").build();

        assert!(policy.check("Read", &Value::Null).is_allowed());
        assert!(policy.check("Write", &Value::Null).is_denied());
    }

    #[test]
    fn test_policy_allow_rules() {
        let policy = ToolPolicy::builder().allow("Bash").allow("Read").build();

        assert!(policy.check("Bash", &Value::Null).is_allowed());
        assert!(policy.check("Read", &Value::Null).is_allowed());
        assert!(policy.check("Write", &Value::Null).is_denied());
    }

    #[test]
    fn test_scoped_allow() {
        let policy = ToolPolicy::builder().allow("Bash(git:*)").build();

        let git_input = serde_json::json!({"command": "git status"});
        let rm_input = serde_json::json!({"command": "rm -rf /"});

        assert!(policy.check("Bash", &git_input).is_allowed());
        assert!(policy.check("Bash", &rm_input).is_denied());
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

        assert!(policy.check("WebFetch", &github_input).is_allowed());
        assert!(policy.check("WebFetch", &other_input).is_denied());
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
        assert!(policy.check("WebFetch", &exact).is_allowed());
        assert!(policy.check("WebFetch", &subdomain).is_allowed());
        assert!(policy.check("WebFetch", &with_port).is_allowed());

        // Should deny: bypass attempts
        let fake_subdomain = serde_json::json!({"url": "https://github.com.attacker.com/path"});
        let query_bypass = serde_json::json!({"url": "https://attacker.com?url=github.com"});
        let path_bypass = serde_json::json!({"url": "https://attacker.com/github.com"});
        let partial_match = serde_json::json!({"url": "https://notgithub.com/page"});
        assert!(policy.check("WebFetch", &fake_subdomain).is_denied());
        assert!(policy.check("WebFetch", &query_bypass).is_denied());
        assert!(policy.check("WebFetch", &path_bypass).is_denied());
        assert!(policy.check("WebFetch", &partial_match).is_denied());
    }

    #[test]
    fn test_explicit_skill_invocation_allowed_in_default_mode() {
        let policy = ToolPolicy::default();
        let result = policy.check_explicit_skill(&serde_json::json!({"skill": "review-pr"}));
        assert!(result.is_allowed());
    }

    #[test]
    fn test_explicit_skill_invocation_respects_deny_rule() {
        let policy = ToolPolicy::builder().deny("Skill(internal)").build();

        assert!(
            policy
                .check_explicit_skill(&serde_json::json!({"skill": "review-pr"}))
                .is_allowed()
        );
        assert!(
            policy
                .check_explicit_skill(&serde_json::json!({"skill": "internal"}))
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
