//! Authorization modes for controlling tool execution behavior.

use serde::{Deserialize, Serialize};

/// Authorization mode that determines the default behavior for tool execution.
///
/// This is particularly important for Chat API environments where interactive
/// user approval is not possible.
///
/// # Modes
///
/// - **Rules**: Standard rule-based flow - tools must be explicitly allowed
///   or will be denied. Use allow/deny rules to control access.
///
/// - **AutoApproveFiles**: Auto-approve file operations (Read, Write, Edit, Glob, Grep).
///   Useful for development scenarios where file access is expected.
///
/// - **AllowAll**: Allow all tool executions without permission checks.
///   ⚠️ Use with extreme caution - only in trusted, sandboxed environments.
///
/// - **ReadOnly**: Read-only mode. Only allows read operations like Read, Glob, Grep,
///   WebSearch, and WebFetch. Blocks all write/execute operations.
///
/// # Example
///
/// ```rust
/// use branchforge::authorization::AuthorizationMode;
///
/// let mode = AuthorizationMode::AutoApproveFiles;
/// assert!(!mode.allows_all());
/// assert!(!mode.is_read_only());
///
/// let mode = AuthorizationMode::ReadOnly;
/// assert!(mode.is_read_only());
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthorizationMode {
    /// Standard rule-based flow - use allow/deny rules
    ///
    /// In this mode, tools are evaluated against allow/deny rules.
    /// If no rule matches, the tool is denied by default.
    #[default]
    Rules,

    /// Auto-approve file operations
    ///
    /// This mode automatically approves file-related tools:
    /// - Read, Write, Edit, Glob, Grep
    ///
    /// Other tools still require explicit allow rules.
    AutoApproveFiles,

    /// Allow all tool executions without permission checks
    ///
    /// ⚠️ **Warning**: This mode bypasses all permission checks.
    /// Only use in fully trusted, sandboxed environments.
    AllowAll,

    /// Read-only mode
    ///
    /// Only allows read-only tools:
    /// - Read, Glob, Grep, WebSearch, WebFetch
    ///
    /// All write and execute operations are blocked.
    ReadOnly,
}

impl AuthorizationMode {
    pub fn allows_all(&self) -> bool {
        matches!(self, AuthorizationMode::AllowAll)
    }

    pub fn is_read_only(&self) -> bool {
        matches!(self, AuthorizationMode::ReadOnly)
    }

    pub fn auto_approves_files(&self) -> bool {
        matches!(self, AuthorizationMode::AutoApproveFiles)
    }

    pub fn uses_rules(&self) -> bool {
        matches!(self, AuthorizationMode::Rules)
    }

    pub fn description(&self) -> &'static str {
        match self {
            AuthorizationMode::Rules => "Standard rule-based permission flow",
            AuthorizationMode::AutoApproveFiles => "Auto-approve file operations",
            AuthorizationMode::AllowAll => "Allow all operations (dangerous)",
            AuthorizationMode::ReadOnly => "Read-only mode",
        }
    }
}

impl std::fmt::Display for AuthorizationMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthorizationMode::Rules => write!(f, "rules"),
            AuthorizationMode::AutoApproveFiles => write!(f, "autoApproveFiles"),
            AuthorizationMode::AllowAll => write!(f, "allowAll"),
            AuthorizationMode::ReadOnly => write!(f, "readOnly"),
        }
    }
}

impl std::str::FromStr for AuthorizationMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "rules" => Ok(AuthorizationMode::Rules),
            "autoapprovefiles" | "auto-approve-files" | "auto_approve_files" => {
                Ok(AuthorizationMode::AutoApproveFiles)
            }
            "allowall" | "allow-all" | "allow_all" => Ok(AuthorizationMode::AllowAll),
            "readonly" | "read-only" | "read_only" => Ok(AuthorizationMode::ReadOnly),
            _ => Err(format!("Unknown authorization mode: {}", s)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_mode() {
        let mode = AuthorizationMode::default();
        assert!(mode.uses_rules());
        assert!(!mode.allows_all());
        assert!(!mode.is_read_only());
        assert!(!mode.auto_approves_files());
    }

    #[test]
    fn test_accept_edits_mode() {
        let mode = AuthorizationMode::AutoApproveFiles;
        assert!(!mode.uses_rules());
        assert!(!mode.allows_all());
        assert!(!mode.is_read_only());
        assert!(mode.auto_approves_files());
    }

    #[test]
    fn test_bypass_mode() {
        let mode = AuthorizationMode::AllowAll;
        assert!(!mode.uses_rules());
        assert!(mode.allows_all());
        assert!(!mode.is_read_only());
        assert!(!mode.auto_approves_files());
    }

    #[test]
    fn test_plan_mode() {
        let mode = AuthorizationMode::ReadOnly;
        assert!(!mode.uses_rules());
        assert!(!mode.allows_all());
        assert!(mode.is_read_only());
        assert!(!mode.auto_approves_files());
    }

    #[test]
    fn test_display() {
        assert_eq!(AuthorizationMode::Rules.to_string(), "rules");
        assert_eq!(
            AuthorizationMode::AutoApproveFiles.to_string(),
            "autoApproveFiles"
        );
        assert_eq!(AuthorizationMode::AllowAll.to_string(), "allowAll");
        assert_eq!(AuthorizationMode::ReadOnly.to_string(), "readOnly");
    }

    #[test]
    fn test_from_str() {
        assert_eq!(
            "rules".parse::<AuthorizationMode>().unwrap(),
            AuthorizationMode::Rules
        );
        assert_eq!(
            "autoApproveFiles".parse::<AuthorizationMode>().unwrap(),
            AuthorizationMode::AutoApproveFiles
        );
        assert_eq!(
            "auto-approve-files".parse::<AuthorizationMode>().unwrap(),
            AuthorizationMode::AutoApproveFiles
        );
        assert_eq!(
            "allowAll".parse::<AuthorizationMode>().unwrap(),
            AuthorizationMode::AllowAll
        );
        assert_eq!(
            "readonly".parse::<AuthorizationMode>().unwrap(),
            AuthorizationMode::ReadOnly
        );
    }

    #[test]
    fn test_serde() {
        let mode = AuthorizationMode::AutoApproveFiles;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"autoApproveFiles\"");

        let parsed: AuthorizationMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, mode);
    }
}
