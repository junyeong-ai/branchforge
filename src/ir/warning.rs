//! Observable graceful-degradation warnings.
//!
//! Every codec populates [`ModelWarning`] entries on every lossy encode or
//! decode. This is the killer DX feature borrowed from Vercel AI SDK V2: when
//! a setting is silently dropped or clamped, the user finds out at runtime
//! through `ModelResponse::warnings`, not through inexplicable behaviour.

use serde::{Deserialize, Serialize};

/// A non-fatal degradation that occurred while encoding a request or
/// decoding a response.
///
/// Warnings are surfaced through `ModelResponse::warnings` and via
/// [`ModelStreamChunk::Warning`](super::stream::ModelStreamChunk::Warning) on
/// streaming responses. They are **never** errors — the call still completed.
/// They tell the caller that some part of their input or some piece of the
/// provider's output could not be honoured exactly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelWarning {
    /// A [`ModelSettings`](super::settings::ModelSettings) field was set but
    /// the codec does not implement it. The setting was dropped.
    UnsupportedSetting { setting: String, codec: String },
    /// A setting was clamped to the codec's allowed range (e.g.
    /// `temperature: 2.0` clamped to `1.0`).
    SettingClamped {
        setting: String,
        from: String,
        to: String,
    },
    /// A field round-tripped lossily through this codec because the wire
    /// format cannot represent it (e.g. Anthropic `cache_control` on a system
    /// block sent through OpenAI Chat).
    LossyEncode { field: String, reason: String },
    /// A `provider_options.<provider>` typed extension was set for a
    /// different provider than the active codec, so the entire option group
    /// was dropped.
    DroppedProviderOption { provider: String, option: String },
    /// A capability that the codec advertises as `Emulated` was used.
    /// Behaviour is best-effort and may not match a `Native` provider.
    CapabilityEmulated { capability: String },
    /// Free-form warning for one-off situations.
    Other(String),
}

impl ModelWarning {
    /// Convenience constructor. Accepts `&'static str` from codecs as well
    /// as runtime `String`s.
    pub fn unsupported(setting: impl Into<String>, codec: impl Into<String>) -> Self {
        ModelWarning::UnsupportedSetting {
            setting: setting.into(),
            codec: codec.into(),
        }
    }

    /// Convenience constructor.
    pub fn clamped(setting: impl Into<String>, from: impl ToString, to: impl ToString) -> Self {
        ModelWarning::SettingClamped {
            setting: setting.into(),
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    /// Convenience constructor.
    pub fn lossy(field: impl Into<String>, reason: impl Into<String>) -> Self {
        ModelWarning::LossyEncode {
            field: field.into(),
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamped_round_trips() {
        let w = ModelWarning::clamped("temperature", 2.0, 1.0);
        let j = serde_json::to_string(&w).unwrap();
        assert!(j.contains("setting_clamped"));
        let back: ModelWarning = serde_json::from_str(&j).unwrap();
        assert_eq!(w, back);
    }

    #[test]
    fn unsupported_constructor() {
        let w = ModelWarning::unsupported("seed", "anthropic-messages");
        match w {
            ModelWarning::UnsupportedSetting { setting, codec } => {
                assert_eq!(setting, "seed");
                assert_eq!(codec, "anthropic-messages");
            }
            _ => panic!("wrong variant"),
        }
    }
}
