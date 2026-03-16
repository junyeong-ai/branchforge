//! Security policy configuration.

use crate::authorization::ToolPolicy;

#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    pub tool_policy: ToolPolicy,
    pub allow_sandbox_bypass: bool,
    pub max_symlink_depth: u8,
}

impl SecurityPolicy {
    pub fn new(tool_policy: ToolPolicy) -> Self {
        Self {
            tool_policy,
            allow_sandbox_bypass: false,
            max_symlink_depth: 10,
        }
    }

    pub fn permissive() -> Self {
        Self {
            tool_policy: ToolPolicy::permissive(),
            allow_sandbox_bypass: true,
            max_symlink_depth: 255,
        }
    }

    pub fn strict() -> Self {
        Self {
            tool_policy: ToolPolicy::new(),
            allow_sandbox_bypass: false,
            max_symlink_depth: 5,
        }
    }

    pub fn tool_policy(mut self, policy: ToolPolicy) -> Self {
        self.tool_policy = policy;
        self
    }

    pub fn sandbox_bypass(mut self, allow: bool) -> Self {
        self.allow_sandbox_bypass = allow;
        self
    }

    pub fn symlink_depth(mut self, depth: u8) -> Self {
        self.max_symlink_depth = depth;
        self
    }

    pub fn can_bypass_sandbox(&self) -> bool {
        self.allow_sandbox_bypass
    }
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self::new(ToolPolicy::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = SecurityPolicy::default();
        assert!(!policy.allow_sandbox_bypass);
        assert_eq!(policy.max_symlink_depth, 10);
    }

    #[test]
    fn test_permissive_policy() {
        let policy = SecurityPolicy::permissive();
        assert!(policy.allow_sandbox_bypass);
    }

    #[test]
    fn test_strict_policy() {
        let policy = SecurityPolicy::strict();
        assert!(!policy.allow_sandbox_bypass);
        assert_eq!(policy.max_symlink_depth, 5);
    }
}
