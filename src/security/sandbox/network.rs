//! Network sandbox for domain filtering and access control.

use std::collections::HashSet;

use crate::config::NetworkSandboxConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainCheck {
    Allowed,
    Blocked,
}

#[derive(Debug, Clone)]
pub struct NetworkSandbox {
    allowed_domains: HashSet<String>,
    blocked_domains: HashSet<String>,
    permissive: bool,
}

impl NetworkSandbox {
    pub fn new() -> Self {
        Self {
            allowed_domains: default_allowed_domains(),
            blocked_domains: HashSet::new(),
            permissive: false,
        }
    }

    pub fn from_settings(settings: &NetworkSandboxConfig) -> Self {
        let mut allowed = default_allowed_domains();
        allowed.extend(settings.allowed_domains.iter().cloned());

        Self {
            allowed_domains: allowed,
            blocked_domains: settings.blocked_domains.clone(),
            permissive: false,
        }
    }

    pub fn permissive() -> Self {
        Self {
            allowed_domains: HashSet::new(),
            blocked_domains: HashSet::new(),
            permissive: true,
        }
    }

    pub fn with_allowed_domains(mut self, domains: impl IntoIterator<Item = String>) -> Self {
        self.allowed_domains.extend(domains);
        self
    }

    pub fn with_blocked_domains(mut self, domains: impl IntoIterator<Item = String>) -> Self {
        self.blocked_domains.extend(domains);
        self
    }

    pub fn check(&self, domain: &str) -> DomainCheck {
        if self.permissive {
            return DomainCheck::Allowed;
        }

        let normalized = normalize_domain(domain);

        if self.is_blocked(&normalized) {
            return DomainCheck::Blocked;
        }

        if self.is_allowed(&normalized) {
            return DomainCheck::Allowed;
        }

        // SDK: domains not explicitly allowed are blocked
        DomainCheck::Blocked
    }

    fn is_blocked(&self, domain: &str) -> bool {
        self.blocked_domains.contains(domain)
            || self
                .blocked_domains
                .iter()
                .any(|pattern| matches_domain_pattern(pattern, domain))
    }

    fn is_allowed(&self, domain: &str) -> bool {
        if self.allowed_domains.is_empty() {
            return true;
        }

        self.allowed_domains.contains(domain)
            || self
                .allowed_domains
                .iter()
                .any(|pattern| matches_domain_pattern(pattern, domain))
    }

    pub fn allowed_domains(&self) -> &HashSet<String> {
        &self.allowed_domains
    }

    pub fn blocked_domains(&self) -> &HashSet<String> {
        &self.blocked_domains
    }
}

impl Default for NetworkSandbox {
    fn default() -> Self {
        Self::new()
    }
}

fn default_allowed_domains() -> HashSet<String> {
    [
        "api.anthropic.com",
        "claude.ai",
        "statsig.anthropic.com",
        "sentry.io",
        "localhost",
        "127.0.0.1",
        "::1",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn normalize_domain(domain: &str) -> String {
    domain
        .trim()
        .to_lowercase()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or(domain)
        .split(':')
        .next()
        .unwrap_or(domain)
        .to_string()
}

fn matches_domain_pattern(pattern: &str, domain: &str) -> bool {
    if pattern.starts_with("*.") {
        let suffix = &pattern[1..];
        domain.ends_with(suffix) || domain == &pattern[2..]
    } else if pattern.starts_with('.') {
        domain.ends_with(pattern) || domain == &pattern[1..]
    } else {
        pattern == domain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_allowed() {
        let sandbox = NetworkSandbox::new();
        assert_eq!(sandbox.check("api.anthropic.com"), DomainCheck::Allowed);
        assert_eq!(sandbox.check("claude.ai"), DomainCheck::Allowed);
        assert_eq!(sandbox.check("localhost"), DomainCheck::Allowed);
    }

    #[test]
    fn test_unknown_domain_blocked() {
        let sandbox = NetworkSandbox::new();
        assert_eq!(sandbox.check("unknown.com"), DomainCheck::Blocked);
    }

    #[test]
    fn test_blocked_domain() {
        let sandbox = NetworkSandbox::new().with_blocked_domains(vec!["evil.com".into()]);
        assert_eq!(sandbox.check("evil.com"), DomainCheck::Blocked);
    }

    #[test]
    fn test_wildcard_allowed() {
        let sandbox = NetworkSandbox::new().with_allowed_domains(vec!["*.example.com".into()]);
        assert_eq!(sandbox.check("sub.example.com"), DomainCheck::Allowed);
        assert_eq!(sandbox.check("example.com"), DomainCheck::Allowed);
    }

    #[test]
    fn test_wildcard_blocked() {
        let sandbox = NetworkSandbox::new().with_blocked_domains(vec!["*.malware.com".into()]);
        assert_eq!(sandbox.check("sub.malware.com"), DomainCheck::Blocked);
    }

    #[test]
    fn test_normalize_domain() {
        assert_eq!(normalize_domain("https://example.com/path"), "example.com");
        assert_eq!(normalize_domain("example.com:8080"), "example.com");
        assert_eq!(normalize_domain("EXAMPLE.COM"), "example.com");
    }

    #[test]
    fn test_permissive() {
        let sandbox = NetworkSandbox::permissive();
        assert_eq!(sandbox.check("anything.com"), DomainCheck::Allowed);
    }

    #[test]
    fn test_blocked_takes_precedence() {
        let sandbox = NetworkSandbox::new()
            .with_allowed_domains(vec!["example.com".into()])
            .with_blocked_domains(vec!["example.com".into()]);
        assert_eq!(sandbox.check("example.com"), DomainCheck::Blocked);
    }
}
