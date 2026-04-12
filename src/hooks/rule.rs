//! Shared hook rule and action types for hooks configuration.
//!
//! Used by plugin loader, skill frontmatter, and subagent frontmatter
//! to define lifecycle hooks in a uniform format.

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

use crate::config::HookConfig;

/// A hook rule entry mapping an optional matcher to a list of actions.
///
/// Format: `{"matcher": "Write|Edit", "hooks": [{"type": "command", "command": "..."}]}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookRule {
    #[serde(default)]
    pub matcher: Option<String>,
    pub hooks: Vec<HookAction>,
}

/// A single hook action within a rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookAction {
    #[serde(rename = "type")]
    pub hook_type: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

impl HookAction {
    /// Create a `HookConfig` from this action, using the rule-level matcher.
    ///
    /// Returns `None` if `hook_type` is not `"command"` or `command` is missing.
    pub fn to_hook_config(&self, rule_matcher: Option<&str>) -> Option<HookConfig> {
        if self.hook_type != "command" {
            return None;
        }
        let command = self.command.as_ref()?.clone();
        let matcher = rule_matcher.map(String::from);
        let timeout_secs = self.timeout;

        if matcher.is_some() || timeout_secs.is_some() {
            Some(HookConfig::Full {
                command,
                timeout_secs,
                matcher,
            })
        } else {
            Some(HookConfig::Command(command))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_rule_serde_roundtrip() {
        let json = r#"{"matcher":"Write|Edit","hooks":[{"type":"command","command":"fmt.sh","timeout":10}]}"#;
        let rule: HookRule = serde_json::from_str(json).unwrap();
        assert_eq!(rule.matcher.as_deref(), Some("Write|Edit"));
        assert_eq!(rule.hooks.len(), 1);
        assert_eq!(rule.hooks[0].hook_type, "command");
        assert_eq!(rule.hooks[0].command.as_deref(), Some("fmt.sh"));
        assert_eq!(rule.hooks[0].timeout, Some(10));

        let serialized = serde_json::to_string(&rule).unwrap();
        let deserialized: HookRule = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.matcher, rule.matcher);
        assert_eq!(deserialized.hooks.len(), 1);
    }

    #[test]
    fn test_hook_rule_no_matcher() {
        let json = r#"{"hooks":[{"type":"command","command":"check.sh"}]}"#;
        let rule: HookRule = serde_json::from_str(json).unwrap();
        assert!(rule.matcher.is_none());
        assert_eq!(rule.hooks.len(), 1);
    }

    #[test]
    fn test_hook_action_to_hook_config_command_only() {
        let action = HookAction {
            hook_type: "command".into(),
            command: Some("echo hello".into()),
            timeout: None,
        };
        let config = action.to_hook_config(None).unwrap();
        assert!(matches!(config, HookConfig::Command(cmd) if cmd == "echo hello"));
    }

    #[test]
    fn test_hook_action_to_hook_config_with_matcher() {
        let action = HookAction {
            hook_type: "command".into(),
            command: Some("fmt.sh".into()),
            timeout: None,
        };
        let config = action.to_hook_config(Some("Write|Edit")).unwrap();
        match config {
            HookConfig::Full {
                command, matcher, ..
            } => {
                assert_eq!(command, "fmt.sh");
                assert_eq!(matcher.as_deref(), Some("Write|Edit"));
            }
            _ => panic!("Expected Full config"),
        }
    }

    #[test]
    fn test_hook_action_to_hook_config_with_timeout() {
        let action = HookAction {
            hook_type: "command".into(),
            command: Some("slow.sh".into()),
            timeout: Some(30),
        };
        let config = action.to_hook_config(None).unwrap();
        match config {
            HookConfig::Full { timeout_secs, .. } => {
                assert_eq!(timeout_secs, Some(30));
            }
            _ => panic!("Expected Full config"),
        }
    }

    #[test]
    fn test_hook_action_to_hook_config_non_command_type() {
        let action = HookAction {
            hook_type: "prompt".into(),
            command: Some("ignored".into()),
            timeout: None,
        };
        assert!(action.to_hook_config(None).is_none());
    }

    #[test]
    fn test_hook_action_to_hook_config_missing_command() {
        let action = HookAction {
            hook_type: "command".into(),
            command: None,
            timeout: None,
        };
        assert!(action.to_hook_config(None).is_none());
    }

    #[test]
    fn test_hook_action_to_hook_config_full_combination() {
        let action = HookAction {
            hook_type: "command".into(),
            command: Some("lint.sh".into()),
            timeout: Some(15),
        };
        let config = action.to_hook_config(Some("Bash")).unwrap();
        match config {
            HookConfig::Full {
                command,
                timeout_secs,
                matcher,
            } => {
                assert_eq!(command, "lint.sh");
                assert_eq!(timeout_secs, Some(15));
                assert_eq!(matcher.as_deref(), Some("Bash"));
            }
            _ => panic!("Expected Full config"),
        }
    }
}
