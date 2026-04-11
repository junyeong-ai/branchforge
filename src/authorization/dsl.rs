//! Permission Rule DSL parser.
//!
//! Parses the canonical `tool_name(subject)` form into a structured
//! [`PermissionRuleSyntax`] that the existing
//! [`super::ToolRule`] / [`super::ToolPolicy`] machinery can consume.
//!
//! # Grammar
//!
//! ```text
//! rule       := decision? tool ( "(" subject ")" )?
//! decision   := "allow" | "deny" | "ask"
//! tool       := identifier         // PascalCase tool name, e.g. Read, Bash, WebFetch
//! subject    := bare | glob | prefix_wild | domain
//! bare       := non-paren chars        // exact match (or path prefix)
//! glob       := contains "*"           // simple glob, e.g. /etc/*, *.txt
//! prefix_wild:= identifier ":*"        // e.g. git:*, rm:*
//! domain     := "domain:" host         // e.g. domain:github.com
//! ```
//!
//! # Examples
//!
//! | Input | Tool | Subject | Decision |
//! | :--- | :--- | :--- | :--- |
//! | `"Read"` | `Read` | none | default (Allow) |
//! | `"Read(/etc/*)"` | `Read` | `Glob("/etc/*")` | default |
//! | `"allow Bash(git:*)"` | `Bash` | `PrefixWild("git")` | Allow |
//! | `"deny Bash(rm:*)"` | `Bash` | `PrefixWild("rm")` | Deny |
//! | `"ask WebFetch(domain:github.com)"` | `WebFetch` | `Domain("github.com")` | Ask |
//!
//! Whitespace between the decision keyword and the tool name is
//! tolerated. The grammar is intentionally restrictive — it does not
//! attempt to parse arbitrary regex or shell expressions.

use std::fmt;

use super::rules::{ToolRule, ToolRuleDecision};

/// Parsed shape of a permission rule, before it is lowered into a
/// concrete [`ToolRule`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRuleSyntax {
    pub decision: RuleDecisionKeyword,
    pub tool: String,
    pub subject: Option<SubjectPattern>,
}

/// The optional `allow` / `deny` / `ask` keyword. `Default` means
/// "no keyword present" — callers decide how to interpret this
/// (typically allow).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RuleDecisionKeyword {
    #[default]
    Default,
    Allow,
    Deny,
    Ask,
}

/// Structured subject pattern carved out of the parenthesised
/// argument. Each variant corresponds to a distinct matcher
/// strategy in the existing rule engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubjectPattern {
    /// Exact string or path prefix.
    Bare(String),
    /// Glob with at least one `*` wildcard.
    Glob(String),
    /// `<prefix>:*` form for command prefixes (e.g. `git:*`).
    PrefixWild(String),
    /// `domain:<host>` form for URL host matching.
    Domain(String),
}

impl SubjectPattern {
    /// Lower the parsed subject pattern into the legacy
    /// `input_pattern: String` representation accepted by
    /// [`ToolRule`].
    pub fn into_input_pattern(self) -> String {
        match self {
            Self::Bare(s) => s,
            Self::Glob(s) => s,
            Self::PrefixWild(prefix) => format!("{prefix}:*"),
            Self::Domain(host) => format!("domain:{host}"),
        }
    }
}

/// Errors returned by [`parse_permission_rule`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PermissionRuleParseError {
    #[error("rule string is empty")]
    Empty,
    #[error("rule has unbalanced parentheses: {0}")]
    UnbalancedParens(String),
    #[error("tool name is missing or invalid: {0}")]
    InvalidTool(String),
    #[error("unknown decision keyword `{0}`; expected allow / deny / ask")]
    UnknownKeyword(String),
}

/// Parse a single rule string into a [`PermissionRuleSyntax`].
pub fn parse_permission_rule(
    input: &str,
) -> Result<PermissionRuleSyntax, PermissionRuleParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(PermissionRuleParseError::Empty);
    }

    // Optional decision keyword: allow / deny / ask, separated from
    // the tool by whitespace. Anything else means there's no
    // keyword and the first token is the tool.
    let (decision, rest) = if let Some(rest) = strip_keyword(trimmed, "allow") {
        (RuleDecisionKeyword::Allow, rest)
    } else if let Some(rest) = strip_keyword(trimmed, "deny") {
        (RuleDecisionKeyword::Deny, rest)
    } else if let Some(rest) = strip_keyword(trimmed, "ask") {
        (RuleDecisionKeyword::Ask, rest)
    } else if let Some(idx) = trimmed.find(char::is_whitespace) {
        // First token isn't a known keyword but the string has
        // whitespace — reject so users notice typos.
        let first = &trimmed[..idx];
        if first
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return Err(PermissionRuleParseError::UnknownKeyword(first.into()));
        }
        (RuleDecisionKeyword::Default, trimmed)
    } else {
        (RuleDecisionKeyword::Default, trimmed)
    };

    let rest = rest.trim();

    // Split tool from optional `(subject)`.
    let (tool, subject_text) = match rest.find('(') {
        Some(open) => {
            if !rest.ends_with(')') {
                return Err(PermissionRuleParseError::UnbalancedParens(rest.into()));
            }
            let tool = rest[..open].trim();
            let subject = &rest[open + 1..rest.len() - 1];
            (tool, Some(subject))
        }
        None => (rest, None),
    };

    if tool.is_empty() || !is_valid_tool_name(tool) {
        return Err(PermissionRuleParseError::InvalidTool(tool.into()));
    }

    let subject = subject_text.map(classify_subject);

    Ok(PermissionRuleSyntax {
        decision,
        tool: tool.to_string(),
        subject,
    })
}

/// Convenience: parse a rule string and lower it into a [`ToolRule`]
/// using the supplied default decision when no keyword was given.
pub fn parse_to_tool_rule(
    input: &str,
    default_decision: ToolRuleDecision,
) -> Result<ToolRule, PermissionRuleParseError> {
    let parsed = parse_permission_rule(input)?;
    let decision = match parsed.decision {
        RuleDecisionKeyword::Allow => ToolRuleDecision::Allow,
        RuleDecisionKeyword::Deny => ToolRuleDecision::Deny,
        // Ask is not directly representable on `ToolRuleDecision` —
        // it lives on `PermissionDecision` instead. Treat it as Allow
        // here so the rule still matches; callers that need true
        // Ask semantics should compose this DSL with their own
        // approval flow.
        RuleDecisionKeyword::Ask | RuleDecisionKeyword::Default => default_decision,
    };

    let input_pattern = parsed.subject.map(SubjectPattern::into_input_pattern);
    Ok(ToolRule::new_internal(parsed.tool, input_pattern, decision))
}

fn strip_keyword<'a>(s: &'a str, keyword: &str) -> Option<&'a str> {
    let lower = s
        .get(..keyword.len())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if lower == keyword {
        let rest = &s[keyword.len()..];
        if rest.starts_with(char::is_whitespace) {
            Some(rest)
        } else {
            None
        }
    } else {
        None
    }
}

fn is_valid_tool_name(s: &str) -> bool {
    // Tool names are matched by regex against the live tool name at
    // dispatch time. We accept the conservative regex meta-character
    // set so users can write `.*` (allow-all), `Read|Write` (allow
    // a set), `Bash`, etc. We reject only characters that would
    // break the DSL grammar — parentheses (subject delimiters),
    // whitespace, and quotes.
    !s.is_empty()
        && s.chars()
            .all(|c| !c.is_whitespace() && c != '(' && c != ')' && c != '"' && c != '\'')
}

fn classify_subject(s: &str) -> SubjectPattern {
    let s = s.trim();
    if let Some(host) = s.strip_prefix("domain:") {
        return SubjectPattern::Domain(host.to_string());
    }
    if let Some(prefix) = s.strip_suffix(":*")
        && !prefix.contains(':')
        && !prefix.is_empty()
    {
        return SubjectPattern::PrefixWild(prefix.to_string());
    }
    if s.contains('*') {
        return SubjectPattern::Glob(s.to_string());
    }
    SubjectPattern::Bare(s.to_string())
}

impl fmt::Display for PermissionRuleSyntax {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.decision {
            RuleDecisionKeyword::Default => {}
            RuleDecisionKeyword::Allow => write!(f, "allow ")?,
            RuleDecisionKeyword::Deny => write!(f, "deny ")?,
            RuleDecisionKeyword::Ask => write!(f, "ask ")?,
        }
        write!(f, "{}", self.tool)?;
        if let Some(sub) = &self.subject {
            let body = match sub {
                SubjectPattern::Bare(s) => s.clone(),
                SubjectPattern::Glob(s) => s.clone(),
                SubjectPattern::PrefixWild(p) => format!("{p}:*"),
                SubjectPattern::Domain(h) => format!("domain:{h}"),
            };
            write!(f, "({body})")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_tool() {
        let r = parse_permission_rule("Read").unwrap();
        assert_eq!(r.tool, "Read");
        assert_eq!(r.decision, RuleDecisionKeyword::Default);
        assert_eq!(r.subject, None);
    }

    #[test]
    fn parses_glob_path() {
        let r = parse_permission_rule("Read(/etc/*)").unwrap();
        assert_eq!(r.tool, "Read");
        assert_eq!(r.subject, Some(SubjectPattern::Glob("/etc/*".to_string())));
    }

    #[test]
    fn parses_prefix_wild_for_bash() {
        let r = parse_permission_rule("Bash(git:*)").unwrap();
        assert_eq!(
            r.subject,
            Some(SubjectPattern::PrefixWild("git".to_string()))
        );
    }

    #[test]
    fn parses_domain_for_webfetch() {
        let r = parse_permission_rule("WebFetch(domain:github.com)").unwrap();
        assert_eq!(
            r.subject,
            Some(SubjectPattern::Domain("github.com".to_string()))
        );
    }

    #[test]
    fn parses_allow_keyword() {
        let r = parse_permission_rule("allow Bash(git:*)").unwrap();
        assert_eq!(r.decision, RuleDecisionKeyword::Allow);
        assert_eq!(r.tool, "Bash");
    }

    #[test]
    fn parses_deny_keyword_case_insensitive() {
        let r = parse_permission_rule("DENY Bash(rm:*)").unwrap();
        assert_eq!(r.decision, RuleDecisionKeyword::Deny);
    }

    #[test]
    fn parses_ask_keyword() {
        let r = parse_permission_rule("ask WebFetch(domain:github.com)").unwrap();
        assert_eq!(r.decision, RuleDecisionKeyword::Ask);
    }

    #[test]
    fn parses_bare_subject_no_wildcards() {
        let r = parse_permission_rule("Read(/etc/passwd)").unwrap();
        assert_eq!(
            r.subject,
            Some(SubjectPattern::Bare("/etc/passwd".to_string()))
        );
    }

    #[test]
    fn errors_on_empty_input() {
        assert!(matches!(
            parse_permission_rule("   ").unwrap_err(),
            PermissionRuleParseError::Empty
        ));
    }

    #[test]
    fn errors_on_unbalanced_parens() {
        assert!(matches!(
            parse_permission_rule("Read(/etc/").unwrap_err(),
            PermissionRuleParseError::UnbalancedParens(_)
        ));
    }

    #[test]
    fn errors_on_unknown_keyword() {
        assert!(matches!(
            parse_permission_rule("permit Bash(ls)").unwrap_err(),
            PermissionRuleParseError::UnknownKeyword(_)
        ));
    }

    #[test]
    fn errors_on_tool_name_with_disallowed_grammar_char() {
        // Whitespace inside the tool name breaks the grammar — the
        // validator rejects it before the parser interprets it as
        // a decision keyword.
        assert!(matches!(
            parse_permission_rule("\"Bash\"(ls)").unwrap_err(),
            PermissionRuleParseError::InvalidTool(_)
        ));
    }

    #[test]
    fn allows_regex_meta_chars_in_tool_name() {
        // `.*` is a valid regex matching any tool name and a common
        // shorthand for "allow everything".
        let r = parse_permission_rule(".*").unwrap();
        assert_eq!(r.tool, ".*");

        // Pipe alternation is the canonical form for tool sets.
        let r = parse_permission_rule("Read|Write|Edit").unwrap();
        assert_eq!(r.tool, "Read|Write|Edit");
    }

    #[test]
    fn lowers_to_tool_rule_with_input_pattern() {
        let rule = parse_to_tool_rule("deny Bash(rm:*)", ToolRuleDecision::Allow).unwrap();
        assert_eq!(rule.decision, ToolRuleDecision::Deny);
        // The pattern lowering preserves the original surface form.
        assert_eq!(rule.input_pattern.as_deref(), Some("rm:*"));
    }

    #[test]
    fn display_round_trips_simple_form() {
        let r = parse_permission_rule("allow Bash(git:*)").unwrap();
        assert_eq!(r.to_string(), "allow Bash(git:*)");
    }

    #[test]
    fn lowers_default_decision_to_supplied_default() {
        let rule = parse_to_tool_rule("Read", ToolRuleDecision::Allow).unwrap();
        assert_eq!(rule.decision, ToolRuleDecision::Allow);
        let rule = parse_to_tool_rule("Read", ToolRuleDecision::Deny).unwrap();
        assert_eq!(rule.decision, ToolRuleDecision::Deny);
    }
}
