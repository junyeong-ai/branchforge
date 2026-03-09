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
}

impl ProviderProfile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            streaming: CapabilitySupport::Unsupported,
            tool_calling: CapabilitySupport::Unsupported,
            structured_outputs: CapabilitySupport::Unsupported,
            context_management: CapabilitySupport::Unsupported,
        }
    }
}
