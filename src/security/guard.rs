//! SecurityGuard: Pre-execution input validation for tools.

use std::path::Path;

use glob::Pattern;
use serde_json::Value;

use super::bash::SecurityConcern;
use super::{SecurityContext, SecurityError};
use crate::authorization::ToolLimits;

pub struct SecurityGuard;

impl SecurityGuard {
    pub fn validate(
        security: &SecurityContext,
        tool_name: &str,
        input: &Value,
    ) -> Result<(), SecurityError> {
        let limits = security
            .policy
            .permission
            .limits(tool_name)
            .cloned()
            .unwrap_or_default();
        let schema = ToolPathSchema::for_tool(tool_name);

        for field in schema.path_fields {
            if let Some(path_str) = input.get(*field).and_then(|v| v.as_str()) {
                security.fs.resolve_with_limits(path_str, &limits)?;
            }
        }

        if schema.is_shell {
            Self::validate_bash_command(security, input, &limits)?;
        }

        Ok(())
    }

    fn validate_bash_command(
        security: &SecurityContext,
        input: &Value,
        limits: &ToolLimits,
    ) -> Result<(), SecurityError> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SecurityError::InvalidPath("missing command".into()))?;

        let bypass = input
            .get("dangerouslyDisableSandbox")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && security.policy.can_bypass_sandbox();

        let analysis = security
            .bash
            .validate(command)
            .map_err(SecurityError::BashBlocked)?;

        for concern in &analysis.concerns {
            if matches!(concern, SecurityConcern::DangerousCommand(_)) {
                return Err(SecurityError::BashBlocked(format!(
                    "Blocked dangerous pattern: {:?}",
                    concern
                )));
            }
        }

        if bypass {
            return Ok(());
        }

        for path_ref in &analysis.paths {
            let path = Path::new(&path_ref.path);

            if !security.fs.is_within(path) {
                return Err(SecurityError::PathEscape(path.to_path_buf()));
            }

            if let Some(ref allowed) = limits.allowed_paths
                && !allowed.is_empty()
                && !matches_patterns(path, allowed)
            {
                return Err(SecurityError::DeniedPath(path.to_path_buf()));
            }

            if let Some(ref denied) = limits.denied_paths
                && matches_patterns(path, denied)
            {
                return Err(SecurityError::DeniedPath(path.to_path_buf()));
            }
        }

        Ok(())
    }
}

fn matches_patterns(path: &Path, patterns: &[String]) -> bool {
    let path_str = path.to_string_lossy();
    patterns.iter().any(|pattern| {
        match Pattern::new(pattern) {
            Ok(g) => g.matches(&path_str),
            Err(e) => {
                tracing::error!(pattern = %pattern, error = %e, "Invalid glob pattern in security policy");
                true // Fail closed: treat invalid patterns as matching to prevent bypass
            }
        }
    })
}

struct ToolPathSchema {
    path_fields: &'static [&'static str],
    is_shell: bool,
}

impl ToolPathSchema {
    fn for_tool(name: &str) -> Self {
        match name {
            "Read" | "Write" | "Edit" => Self {
                path_fields: &["file_path"],
                is_shell: false,
            },
            "Glob" | "Grep" => Self {
                path_fields: &["path"],
                is_shell: false,
            },
            "Bash" => Self {
                path_fields: &[],
                is_shell: true,
            },
            _ => Self {
                path_fields: &[],
                is_shell: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_test_context(root: &Path) -> SecurityContext {
        SecurityContext::new(root).unwrap()
    }

    #[test]
    fn test_path_escape_blocked() {
        let dir = tempdir().unwrap();
        let security = create_test_context(dir.path());

        let input = serde_json::json!({
            "file_path": "/etc/passwd"
        });

        let result = SecurityGuard::validate(&security, "Read", &input);
        assert!(matches!(result, Err(SecurityError::PathEscape(_))));
    }

    #[test]
    fn test_traversal_blocked() {
        let dir = tempdir().unwrap();
        let security = create_test_context(dir.path());

        let input = serde_json::json!({
            "file_path": "../../../etc/passwd"
        });

        let result = SecurityGuard::validate(&security, "Read", &input);
        assert!(matches!(result, Err(SecurityError::PathEscape(_))));
    }

    #[test]
    fn test_valid_path_allowed() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("test.txt"), "content").unwrap();

        let security = create_test_context(&root);

        let input = serde_json::json!({
            "file_path": root.join("test.txt").to_str().unwrap()
        });

        let result = SecurityGuard::validate(&security, "Read", &input);
        assert!(result.is_ok());
    }

    #[test]
    fn test_bash_path_escape_blocked() {
        let dir = tempdir().unwrap();
        let security = create_test_context(dir.path());

        let input = serde_json::json!({
            "command": "cat /etc/passwd"
        });

        let result = SecurityGuard::validate(&security, "Bash", &input);
        assert!(matches!(result, Err(SecurityError::PathEscape(_))));
    }

    #[test]
    fn test_dangerous_command_blocked() {
        let dir = tempdir().unwrap();
        let security = create_test_context(dir.path());

        let input = serde_json::json!({
            "command": "rm -rf /"
        });

        let result = SecurityGuard::validate(&security, "Bash", &input);
        assert!(
            matches!(result, Err(SecurityError::BashBlocked(_))),
            "Expected BashBlocked error for dangerous command, got {:?}",
            result
        );
    }

    #[test]
    fn test_glob_path_optional() {
        let dir = tempdir().unwrap();
        let security = create_test_context(dir.path());

        let input = serde_json::json!({
            "pattern": "*.rs"
        });

        let result = SecurityGuard::validate(&security, "Glob", &input);
        assert!(result.is_ok());
    }

    #[test]
    fn test_non_path_tool_allowed() {
        let dir = tempdir().unwrap();
        let security = create_test_context(dir.path());

        let input = serde_json::json!({
            "query": "test search"
        });

        let result = SecurityGuard::validate(&security, "WebSearch", &input);
        assert!(result.is_ok());
    }
}
