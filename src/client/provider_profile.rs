use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Full,
    Degradable,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub name: String,
    pub streaming: CapabilitySupport,
    pub tool_calling: CapabilitySupport,
    pub structured_outputs: CapabilitySupport,
    pub context_management: CapabilitySupport,
    pub vision: CapabilitySupport,
    pub prompt_caching: CapabilitySupport,
    pub extended_thinking: CapabilitySupport,
}

impl ProviderProfile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            streaming: CapabilitySupport::Unsupported,
            tool_calling: CapabilitySupport::Unsupported,
            structured_outputs: CapabilitySupport::Unsupported,
            context_management: CapabilitySupport::Unsupported,
            vision: CapabilitySupport::Unsupported,
            prompt_caching: CapabilitySupport::Unsupported,
            extended_thinking: CapabilitySupport::Unsupported,
        }
    }

    pub fn full(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            streaming: CapabilitySupport::Full,
            tool_calling: CapabilitySupport::Full,
            structured_outputs: CapabilitySupport::Full,
            context_management: CapabilitySupport::Full,
            vision: CapabilitySupport::Full,
            prompt_caching: CapabilitySupport::Full,
            extended_thinking: CapabilitySupport::Full,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_constructor() {
        let profile = ProviderProfile::full("anthropic");
        assert_eq!(profile.name, "anthropic");
        assert_eq!(profile.streaming, CapabilitySupport::Full);
        assert_eq!(profile.tool_calling, CapabilitySupport::Full);
        assert_eq!(profile.structured_outputs, CapabilitySupport::Full);
        assert_eq!(profile.context_management, CapabilitySupport::Full);
        assert_eq!(profile.vision, CapabilitySupport::Full);
        assert_eq!(profile.prompt_caching, CapabilitySupport::Full);
        assert_eq!(profile.extended_thinking, CapabilitySupport::Full);
    }

    #[test]
    fn test_new_constructor_all_unsupported() {
        let profile = ProviderProfile::new("test");
        assert_eq!(profile.name, "test");
        assert_eq!(profile.streaming, CapabilitySupport::Unsupported);
        assert_eq!(profile.tool_calling, CapabilitySupport::Unsupported);
        assert_eq!(profile.structured_outputs, CapabilitySupport::Unsupported);
        assert_eq!(profile.context_management, CapabilitySupport::Unsupported);
        assert_eq!(profile.vision, CapabilitySupport::Unsupported);
        assert_eq!(profile.prompt_caching, CapabilitySupport::Unsupported);
        assert_eq!(profile.extended_thinking, CapabilitySupport::Unsupported);
    }

    #[test]
    fn test_default_profile_returns_full() {
        // Simulate what the default ProviderAdapter::profile() method returns
        let profile = ProviderProfile::full("test-adapter");
        assert_eq!(profile.streaming, CapabilitySupport::Full);
        assert_eq!(profile.vision, CapabilitySupport::Full);
        assert_eq!(profile.prompt_caching, CapabilitySupport::Full);
        assert_eq!(profile.extended_thinking, CapabilitySupport::Full);
    }
}
