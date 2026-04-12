//! Provider identifier used by pricing/cost attribution.
//!
//! `UsageProvider` is the **billing-side** view of which vendor produced
//! a given usage record. The same model id can be priced differently
//! depending on whether it was reached through the vendor's direct API,
//! through Bedrock, through Vertex, etc., so the pricing layer needs an
//! explicit tag rather than guessing from the model string alone.

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

/// Provider that generated a given usage record.
///
/// Used for provider-aware cost attribution when calculating pricing
/// across different API providers (Anthropic, OpenAI, Gemini, etc.).
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UsageProvider {
    #[default]
    Anthropic,
    OpenAi,
    Gemini,
    Bedrock,
    Vertex,
    Foundry,
    Unknown(String),
}

impl std::fmt::Display for UsageProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anthropic => write!(f, "Anthropic"),
            Self::OpenAi => write!(f, "OpenAI"),
            Self::Gemini => write!(f, "Gemini"),
            Self::Bedrock => write!(f, "Bedrock"),
            Self::Vertex => write!(f, "Vertex"),
            Self::Foundry => write!(f, "Foundry"),
            Self::Unknown(name) => write!(f, "{}", name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_known_variants() {
        assert_eq!(format!("{}", UsageProvider::Anthropic), "Anthropic");
        assert_eq!(format!("{}", UsageProvider::OpenAi), "OpenAI");
        assert_eq!(format!("{}", UsageProvider::Gemini), "Gemini");
        assert_eq!(format!("{}", UsageProvider::Bedrock), "Bedrock");
        assert_eq!(format!("{}", UsageProvider::Vertex), "Vertex");
        assert_eq!(format!("{}", UsageProvider::Foundry), "Foundry");
        assert_eq!(
            format!("{}", UsageProvider::Unknown("Custom".into())),
            "Custom"
        );
    }

    #[test]
    fn default_is_anthropic() {
        assert_eq!(UsageProvider::default(), UsageProvider::Anthropic);
    }
}
