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

#![allow(missing_docs)]

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
#[non_exhaustive]
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
#[non_exhaustive]
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

/// Typed parse / lowering errors returned by
/// [`parse_permission_rule`] and [`parse_to_tool_rule`].
///
/// Each variant carries enough context to pinpoint the failure in
/// the original rule string. `col` is the **byte offset** (not
/// character index) into the raw input at which the parser first
/// detected the problem; callers that want to render a caret
/// underline can map that directly to a column in single-line
/// rule strings, which is the only shape the DSL supports.
///
/// Phase D E-3 hardening: new variants close long-standing silent
/// failure modes that previously either panicked or produced a
/// rule that matched nothing at runtime.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PermissionDslError {
    /// Whitespace-only or empty input. Nothing to parse.
    #[error("rule string is empty")]
    Empty,

    /// Opening `(` had no matching `)` by end of string.
    #[error("rule has unbalanced parentheses at col {col}: `{rule}`")]
    UnbalancedParens { rule: String, col: usize },

    /// Closing `)` was followed by more text. The DSL does not
    /// accept any trailing input after the subject.
    #[error(
        "unexpected trailing input after `)` at col {col}: `{trailing}` — only one rule per string"
    )]
    TrailingInput { trailing: String, col: usize },

    /// Tool identifier was empty or contained DSL-reserved
    /// characters (parentheses, whitespace, quotes).
    #[error("tool name is missing or invalid at col {col}: `{tool}`")]
    InvalidTool { tool: String, col: usize },

    /// `Tool()` — the subject parentheses were present but empty.
    /// Reject because it's almost always a typo for `Tool` (no
    /// subject) or `Tool(*)` (match-anything).
    #[error("subject for `{tool}` is empty at col {col}; drop the parentheses or use `{tool}(*)`")]
    EmptySubject { tool: String, col: usize },

    /// First token looked like a keyword but was not one of
    /// `allow` / `deny` / `ask`. Reject so users notice typos
    /// (`permit Bash(ls)` would otherwise be silently treated as
    /// the tool name `permit` with a trailing `Bash(ls)`).
    #[error(
        "unknown decision keyword `{keyword}` at col {col}; expected one of allow / deny / ask"
    )]
    UnknownKeyword { keyword: String, col: usize },

    /// Tool pattern failed to compile as a regex. The DSL lets
    /// users write `Read|Write` or `.*` as tool patterns; previously
    /// a malformed pattern such as `Re[ad` was silently stored,
    /// matched nothing at runtime, and left the user mystified
    /// about why their allow rule never fired.
    #[error("tool pattern `{pattern}` is not a valid regex: {details}")]
    InvalidToolPattern { pattern: String, details: String },
}

/// Parse a single rule string into a [`PermissionRuleSyntax`].
///
/// Byte offsets in returned [`PermissionDslError`]s are relative to
/// the **original** `input`, not the internally trimmed view, so
/// callers that render a caret underline can index straight into
/// the user-facing string.
pub fn parse_permission_rule(input: &str) -> Result<PermissionRuleSyntax, PermissionDslError> {
    // Leading whitespace width — used so every col offset we hand
    // back points into the original string, not the trimmed copy.
    let lead = input.len() - input.trim_start().len();
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(PermissionDslError::Empty);
    }

    // Optional decision keyword: allow / deny / ask, separated from
    // the tool by whitespace. Anything else means there's no
    // keyword and the first token is the tool.
    let (decision, rest_offset, rest) = if let Some(rest) = strip_keyword(trimmed, "allow") {
        (RuleDecisionKeyword::Allow, lead + "allow".len(), rest)
    } else if let Some(rest) = strip_keyword(trimmed, "deny") {
        (RuleDecisionKeyword::Deny, lead + "deny".len(), rest)
    } else if let Some(rest) = strip_keyword(trimmed, "ask") {
        (RuleDecisionKeyword::Ask, lead + "ask".len(), rest)
    } else if let Some(idx) = trimmed.find(char::is_whitespace) {
        // First token isn't a known keyword but the string has
        // whitespace — reject so users notice typos. We only fire
        // on purely-lowercase-alnum tokens so patterns like
        // `Read Bash` (two tools, impossible) still surface as
        // InvalidTool via the main path.
        let first = &trimmed[..idx];
        if first
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return Err(PermissionDslError::UnknownKeyword {
                keyword: first.into(),
                col: lead,
            });
        }
        (RuleDecisionKeyword::Default, lead, trimmed)
    } else {
        (RuleDecisionKeyword::Default, lead, trimmed)
    };

    // Skip whitespace between the decision keyword and the tool.
    let after_kw_trim = rest.len() - rest.trim_start().len();
    let tool_start = rest_offset + after_kw_trim;
    let rest = rest.trim();

    // Split tool from optional `(subject)`.
    let (tool, subject_info) = match rest.find('(') {
        Some(open) => {
            let close = match rest.rfind(')') {
                Some(i) if i > open => i,
                _ => {
                    return Err(PermissionDslError::UnbalancedParens {
                        rule: rest.into(),
                        col: tool_start + open,
                    });
                }
            };
            // Nothing is allowed after the closing paren.
            if close + 1 != rest.len() {
                let trailing = &rest[close + 1..];
                return Err(PermissionDslError::TrailingInput {
                    trailing: trailing.into(),
                    col: tool_start + close + 1,
                });
            }
            let tool = rest[..open].trim_end();
            let subject = &rest[open + 1..close];
            (tool, Some((subject, tool_start + open + 1)))
        }
        None => (rest, None),
    };

    if tool.is_empty() || !is_valid_tool_name(tool) {
        return Err(PermissionDslError::InvalidTool {
            tool: tool.into(),
            col: tool_start,
        });
    }

    let subject = match subject_info {
        Some((raw, col)) => {
            if raw.trim().is_empty() {
                return Err(PermissionDslError::EmptySubject {
                    tool: tool.into(),
                    col,
                });
            }
            Some(classify_subject(raw))
        }
        None => None,
    };

    Ok(PermissionRuleSyntax {
        decision,
        tool: tool.to_string(),
        subject,
    })
}

/// Convenience: parse a rule string and lower it into a [`ToolRule`]
/// using the supplied default decision when no keyword was given.
///
/// Phase D E-3: this also validates that the tool name compiles as a
/// regex before constructing the rule. Previously, a malformed
/// pattern such as `Re[ad` was silently stored — `ToolRule::matches`
/// fell back to exact-string comparison and the user's rule never
/// fired. Now the caller gets a typed `InvalidToolPattern` error
/// they can surface as a fatal config-load failure.
pub fn parse_to_tool_rule(
    input: &str,
    default_decision: ToolRuleDecision,
) -> Result<ToolRule, PermissionDslError> {
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
    ToolRule::try_new_from_dsl(parsed.tool, input_pattern, decision)
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
            PermissionDslError::Empty
        ));
    }

    #[test]
    fn errors_on_unbalanced_parens() {
        assert!(matches!(
            parse_permission_rule("Read(/etc/").unwrap_err(),
            PermissionDslError::UnbalancedParens { .. }
        ));
    }

    #[test]
    fn errors_on_unknown_keyword() {
        assert!(matches!(
            parse_permission_rule("permit Bash(ls)").unwrap_err(),
            PermissionDslError::UnknownKeyword { .. }
        ));
    }

    #[test]
    fn errors_on_tool_name_with_disallowed_grammar_char() {
        // Whitespace inside the tool name breaks the grammar — the
        // validator rejects it before the parser interprets it as
        // a decision keyword.
        assert!(matches!(
            parse_permission_rule("\"Bash\"(ls)").unwrap_err(),
            PermissionDslError::InvalidTool { .. }
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

    // ── Phase D E-3: grammar coverage matrix ───────────────────────

    /// `Tool()` — empty subject parens are almost always a typo.
    /// Reject with a dedicated variant so the error message can
    /// suggest either dropping the parens or using `Tool(*)`.
    #[test]
    fn e3_rejects_empty_subject_parens() {
        match parse_permission_rule("Bash()").unwrap_err() {
            PermissionDslError::EmptySubject { tool, col } => {
                assert_eq!(tool, "Bash");
                assert_eq!(col, 5); // byte after the `(`
            }
            other => panic!("expected EmptySubject, got {other:?}"),
        }
    }

    /// `Tool(arg)trailing` — anything after the closing `)` is
    /// rejected. The DSL is one-rule-per-string.
    #[test]
    fn e3_rejects_trailing_input_after_closing_paren() {
        match parse_permission_rule("Bash(git:*)extra").unwrap_err() {
            PermissionDslError::TrailingInput { trailing, col } => {
                assert_eq!(trailing, "extra");
                assert_eq!(col, 11);
            }
            other => panic!("expected TrailingInput, got {other:?}"),
        }
    }

    /// Column offsets for `UnbalancedParens` point at the opening
    /// `(`, not the end of the string, so error renderers can draw
    /// a caret at the actual problem site.
    #[test]
    fn e3_unbalanced_parens_points_at_opening() {
        match parse_permission_rule("Read(/etc/passwd").unwrap_err() {
            PermissionDslError::UnbalancedParens { col, .. } => {
                assert_eq!(col, 4);
            }
            other => panic!("expected UnbalancedParens, got {other:?}"),
        }
    }

    /// `UnknownKeyword` preserves the offending token and points at
    /// the start of the input (after any leading whitespace).
    #[test]
    fn e3_unknown_keyword_preserves_offset_past_leading_whitespace() {
        match parse_permission_rule("  permit Bash(ls)").unwrap_err() {
            PermissionDslError::UnknownKeyword { keyword, col } => {
                assert_eq!(keyword, "permit");
                assert_eq!(col, 2);
            }
            other => panic!("expected UnknownKeyword, got {other:?}"),
        }
    }

    /// Malformed tool regex patterns like `Re[ad` previously
    /// silently stored a rule that matched nothing at runtime.
    /// Phase D E-3 catches them during lowering via
    /// [`ToolRule::try_new_from_dsl`] and surfaces them as
    /// `InvalidToolPattern` so config loaders can reject the file.
    #[test]
    fn e3_rejects_invalid_tool_regex_at_lowering() {
        match parse_to_tool_rule("Re[ad", ToolRuleDecision::Allow).unwrap_err() {
            PermissionDslError::InvalidToolPattern { pattern, details } => {
                assert_eq!(pattern, "Re[ad");
                assert!(
                    !details.is_empty(),
                    "details should carry regex diagnostics"
                );
            }
            other => panic!("expected InvalidToolPattern, got {other:?}"),
        }
    }

    /// The full happy-path matrix: one row per accepted shape. Any
    /// regression in the parser that drops one of these rows shows
    /// up as a failure with a clear row label.
    #[test]
    fn e3_grammar_matrix_accepts_every_canonical_shape() {
        let cases: &[(&str, RuleDecisionKeyword, &str, Option<SubjectPattern>)] = &[
            ("Read", RuleDecisionKeyword::Default, "Read", None),
            ("allow Read", RuleDecisionKeyword::Allow, "Read", None),
            (
                "deny Bash(rm:*)",
                RuleDecisionKeyword::Deny,
                "Bash",
                Some(SubjectPattern::PrefixWild("rm".into())),
            ),
            (
                "ask WebFetch(domain:github.com)",
                RuleDecisionKeyword::Ask,
                "WebFetch",
                Some(SubjectPattern::Domain("github.com".into())),
            ),
            (
                "Read(/etc/*)",
                RuleDecisionKeyword::Default,
                "Read",
                Some(SubjectPattern::Glob("/etc/*".into())),
            ),
            (
                "Read(/etc/passwd)",
                RuleDecisionKeyword::Default,
                "Read",
                Some(SubjectPattern::Bare("/etc/passwd".into())),
            ),
            (
                "Read|Write",
                RuleDecisionKeyword::Default,
                "Read|Write",
                None,
            ),
            (".*", RuleDecisionKeyword::Default, ".*", None),
        ];
        for (input, decision, tool, subject) in cases {
            let parsed = parse_permission_rule(input)
                .unwrap_or_else(|e| panic!("row `{input}` failed to parse: {e}"));
            assert_eq!(&parsed.decision, decision, "row `{input}` wrong decision");
            assert_eq!(&parsed.tool, tool, "row `{input}` wrong tool");
            assert_eq!(&parsed.subject, subject, "row `{input}` wrong subject");
        }
    }

    /// The rejection matrix: every row must fail with the expected
    /// variant.
    #[test]
    fn e3_grammar_matrix_rejects_every_malformed_shape() {
        use PermissionDslError as E;
        let empty_err = |e: &E| matches!(e, E::Empty);
        let unbalanced = |e: &E| matches!(e, E::UnbalancedParens { .. });
        let trailing = |e: &E| matches!(e, E::TrailingInput { .. });
        let bad_tool = |e: &E| matches!(e, E::InvalidTool { .. });
        let empty_subject = |e: &E| matches!(e, E::EmptySubject { .. });
        let unknown_kw = |e: &E| matches!(e, E::UnknownKeyword { .. });

        let cases: &[(&str, fn(&E) -> bool, &str)] = &[
            ("", empty_err, "Empty"),
            ("   ", empty_err, "Empty"),
            ("Read(", unbalanced, "UnbalancedParens"),
            ("Read(/etc/", unbalanced, "UnbalancedParens"),
            ("Read()extra", trailing, "TrailingInput"),
            ("Read(/etc/)tail", trailing, "TrailingInput"),
            ("\"Bash\"(ls)", bad_tool, "InvalidTool"),
            ("Bash ls", bad_tool, "InvalidTool"), // Bash space ls — not a keyword either
            ("Bash()", empty_subject, "EmptySubject"),
            ("Bash(   )", empty_subject, "EmptySubject"),
            ("permit Bash(ls)", unknown_kw, "UnknownKeyword"),
            ("grant Read", unknown_kw, "UnknownKeyword"),
        ];
        for (input, predicate, label) in cases {
            let err = parse_permission_rule(input)
                .err()
                .unwrap_or_else(|| panic!("row `{input}` should have failed with {label}"));
            assert!(
                predicate(&err),
                "row `{input}` expected {label}, got {err:?}"
            );
        }
    }
}
