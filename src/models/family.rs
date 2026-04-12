#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelFamily {
    Opus,
    Sonnet,
    Haiku,
}

impl ModelFamily {
    pub fn aliases(&self) -> &'static [&'static str] {
        match self {
            Self::Opus => &["opus", "reasoning", "large"],
            Self::Sonnet => &["sonnet", "default", "primary", "balanced"],
            Self::Haiku => &["haiku", "small", "fast"],
        }
    }

    pub fn default_role(&self) -> ModelRole {
        match self {
            Self::Opus => ModelRole::Reasoning,
            Self::Sonnet => ModelRole::Primary,
            Self::Haiku => ModelRole::Small,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRole {
    Primary,
    Small,
    Reasoning,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_family_aliases() {
        assert!(ModelFamily::Sonnet.aliases().contains(&"sonnet"));
        assert!(ModelFamily::Haiku.aliases().contains(&"fast"));
        assert!(ModelFamily::Opus.aliases().contains(&"reasoning"));
    }

    #[test]
    fn test_default_roles() {
        assert_eq!(ModelFamily::Sonnet.default_role(), ModelRole::Primary);
        assert_eq!(ModelFamily::Haiku.default_role(), ModelRole::Small);
        assert_eq!(ModelFamily::Opus.default_role(), ModelRole::Reasoning);
    }
}
