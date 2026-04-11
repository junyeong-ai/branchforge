//! Open-set provider profile registry.
//!
//! A [`ProviderProfile`] is a named recipe for assembling one
//! [`crate::client::codec::ModelCodec`] + one
//! [`crate::client::transport::ModelTransport`] +
//! [`CredentialHint`] into a fully-wired
//! [`crate::client::provider_client::ProviderClient`]. The [`ProfileRegistry`]
//! ships with builtin profiles for every well-known vendor and
//! **also accepts user-registered profiles at runtime**, so adding
//! a new OpenAI-compatible vendor or a custom auth scheme does not
//! require recompiling BranchForge or forking the repository.
//!
//! This module replaces the older closed-set `Preset` enum, which
//! conflated four orthogonal decisions (codec, transport,
//! credential source, default URL) into a single match-dispatched
//! variant and made adding a new vendor require code changes in
//! 5+ locations.
//!
//! # Example
//!
//! ```ignore
//! use branchforge::client::preset::{ProfileRegistry, ProviderProfile, CredentialHint};
//! use branchforge::client::codec::OpenAiChatCodec;
//! use std::sync::Arc;
//!
//! // Use a builtin profile.
//! let registry = ProfileRegistry::with_builtins();
//! let client = registry.build("groq")?;
//!
//! // Or register a custom vendor at runtime.
//! let mut registry = ProfileRegistry::with_builtins();
//! registry.register(ProviderProfile {
//!     id: "my-internal-llm".into(),
//!     codec: || Arc::new(OpenAiChatCodec::new()),
//!     transport_builder: Box::new(|| {
//!         /* build your own transport */
//!         todo!()
//!     }),
//!     credential: CredentialHint::EnvVar {
//!         name: "MY_LLM_KEY",
//!         hint: "Internal LLM gateway key",
//!     },
//!     default_model: Some("internal-llama-3".into()),
//! });
//! let client = registry.build("my-internal-llm")?;
//! ```

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use secrecy::SecretString;

#[cfg(feature = "aws")]
use crate::client::codec::BedrockConverseCodec;
use crate::client::codec::{
    AnthropicMessagesCodec, GeminiGenerateCodec, ModelCodec, OpenAiChatCodec, OpenAiResponsesCodec,
};
use crate::client::provider_client::ProviderClient;
use crate::client::transport::{DirectAuth, DirectTransport, ModelTransport};
use crate::{Error, Result};

/// Where the credential for a profile comes from.
///
/// Profiles describe credential sources declaratively so the
/// registry can produce actionable error messages when a credential
/// is missing without baking vendor-specific env-var names into the
/// transport layer.
pub enum CredentialHint {
    /// Read a single environment variable.
    EnvVar {
        name: &'static str,
        /// Where the user can obtain a key (URL or instructions),
        /// surfaced in the error message when the env var is unset.
        hint: &'static str,
    },
    /// Try a list of environment variables in order; the first one
    /// set wins. Used by Gemini (`GEMINI_API_KEY` or
    /// `GOOGLE_API_KEY`).
    EnvVarOneOf {
        names: &'static [&'static str],
        hint: &'static str,
    },
    /// No credential needed (Ollama, llama.cpp, custom local
    /// gateways).
    None,
    /// Caller-supplied closure that resolves the credential at
    /// build time. Used for OAuth flows, ADC tokens, AWS SigV4,
    /// or any auth scheme that env-var hints cannot express.
    Custom(Box<dyn Fn() -> Result<SecretString> + Send + Sync>),
}

impl std::fmt::Debug for CredentialHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnvVar { name, .. } => f.debug_tuple("EnvVar").field(name).finish(),
            Self::EnvVarOneOf { names, .. } => f.debug_tuple("EnvVarOneOf").field(names).finish(),
            Self::None => f.write_str("None"),
            Self::Custom(_) => f.write_str("Custom(<closure>)"),
        }
    }
}

impl CredentialHint {
    /// Resolve the hint to a [`SecretString`], or return a
    /// configuration error explaining what the user must set.
    fn resolve(&self, profile_id: &str) -> Result<Option<SecretString>> {
        match self {
            Self::EnvVar { name, hint } => {
                let val = std::env::var(name).map_err(|_| {
                    Error::Config(format!("{name} not set for `{profile_id}` profile. {hint}"))
                })?;
                Ok(Some(SecretString::from(val)))
            }
            Self::EnvVarOneOf { names, hint } => {
                for n in *names {
                    if let Ok(val) = std::env::var(n) {
                        return Ok(Some(SecretString::from(val)));
                    }
                }
                Err(Error::Config(format!(
                    "none of {names:?} set for `{profile_id}` profile. {hint}"
                )))
            }
            Self::None => Ok(None),
            Self::Custom(f) => f().map(Some),
        }
    }
}

/// A complete description of how to build one provider client.
pub struct ProviderProfile {
    /// Stable id used as the registry key. Convention: lowercase
    /// kebab-case (`anthropic`, `vertex-gemini`, `my-internal`).
    pub id: Cow<'static, str>,
    /// Codec factory. Pure (no I/O), called once at build time.
    pub codec: fn() -> Arc<dyn ModelCodec>,
    /// Transport builder. The closure receives the resolved
    /// credential (if any) and returns a fully-configured
    /// transport. The default base URL is captured inside the
    /// closure; profiles that respect a `*_BASE_URL` env-var
    /// override read it inside the closure too.
    pub transport_builder:
        Box<dyn Fn(Option<SecretString>) -> Result<Arc<dyn ModelTransport>> + Send + Sync>,
    /// Where the credential comes from, declaratively.
    pub credential: CredentialHint,
    /// Optional default model id surfaced to consumers (examples,
    /// CLI). Application code is free to override.
    pub default_model: Option<String>,
}

impl std::fmt::Debug for ProviderProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderProfile")
            .field("id", &self.id)
            .field("credential", &self.credential)
            .field("default_model", &self.default_model)
            .finish()
    }
}

/// Open-set registry of [`ProviderProfile`]s.
///
/// Construct via [`Self::with_builtins`] for the canonical 16
/// builtin profiles, then call [`Self::register`] to add custom
/// profiles. Lookup via [`Self::build`] resolves the credential
/// and assembles the [`ProviderClient`].
pub struct ProfileRegistry {
    profiles: BTreeMap<String, ProviderProfile>,
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

impl ProfileRegistry {
    /// Empty registry. Use [`Self::with_builtins`] for the
    /// canonical preset set instead unless you specifically need
    /// to ship a curated subset.
    pub fn empty() -> Self {
        Self {
            profiles: BTreeMap::new(),
        }
    }

    /// Registry pre-populated with all canonical builtin profiles.
    pub fn with_builtins() -> Self {
        let mut reg = Self::empty();
        for builder in BUILTIN_BUILDERS {
            let profile = builder();
            reg.register(profile);
        }
        reg
    }

    /// Add or replace a profile by id.
    pub fn register(&mut self, profile: ProviderProfile) {
        self.profiles.insert(profile.id.to_string(), profile);
    }

    /// Returns `true` if a profile with this id exists.
    pub fn contains(&self, id: &str) -> bool {
        self.profiles.contains_key(id)
    }

    /// Iterate registered profile ids alphabetically.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.profiles.keys().map(String::as_str)
    }

    /// Look up a profile by id without building it.
    pub fn get(&self, id: &str) -> Option<&ProviderProfile> {
        self.profiles.get(id)
    }

    /// Resolve credentials, build the codec + transport, and
    /// return a fully-wired [`ProviderClient`].
    pub fn build(&self, id: &str) -> Result<ProviderClient> {
        let profile = self
            .profiles
            .get(id)
            .ok_or_else(|| Error::Config(format!("unknown provider profile: `{id}`")))?;
        let credential = profile.credential.resolve(&profile.id)?;
        let codec = (profile.codec)();
        let transport = (profile.transport_builder)(credential)?;
        ProviderClient::new(codec, transport)
    }
}

impl std::fmt::Debug for ProfileRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileRegistry")
            .field("profile_ids", &self.profiles.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Resolve a profile id from `BRANCHFORGE_PROVIDER` and build it
/// against the canonical builtin registry. This is the one-call
/// entry point for examples and minimal applications.
pub async fn from_env() -> Result<ProviderClient> {
    let registry = ProfileRegistry::with_builtins();
    let id = std::env::var("BRANCHFORGE_PROVIDER").map_err(|_| {
        let known = registry.ids().collect::<Vec<_>>().join(", ");
        Error::Config(format!(
            "BRANCHFORGE_PROVIDER not set; choose one of: {known}"
        ))
    })?;
    registry.build(&id)
}

// ==========================================================================
// Builtin profile factories
// ==========================================================================
//
// Each builder is a `fn() -> ProviderProfile` so the registry can
// rebuild a fresh instance per call (the transport closure is
// stateful with respect to the resolved credential).

type ProfileBuilder = fn() -> ProviderProfile;

const BUILTIN_BUILDERS: &[ProfileBuilder] = &[
    profile_anthropic,
    profile_openai,
    profile_openai_chat,
    profile_gemini,
    #[cfg(feature = "gcp")]
    profile_vertex_gemini,
    #[cfg(feature = "gcp")]
    profile_vertex_anthropic,
    #[cfg(feature = "aws")]
    profile_bedrock,
    #[cfg(feature = "azure")]
    profile_foundry_anthropic,
    profile_grok,
    profile_groq,
    profile_mistral,
    profile_together,
    profile_fireworks,
    profile_cerebras,
    profile_ollama,
];

fn direct_with_codec(
    base_url: String,
    auth: DirectAuth,
    allowed_codecs: &'static [&'static str],
) -> Arc<dyn ModelTransport> {
    Arc::new(DirectTransport::new(base_url, auth).with_allowed_codecs(allowed_codecs))
}

fn profile_anthropic() -> ProviderProfile {
    ProviderProfile {
        id: "anthropic".into(),
        codec: || Arc::new(AnthropicMessagesCodec::new()),
        transport_builder: Box::new(|cred| {
            let base = std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".into());
            Ok(direct_with_codec(
                base,
                DirectAuth::XApiKey(cred.expect("anthropic profile requires credential")),
                &["anthropic-messages"],
            ))
        }),
        credential: CredentialHint::EnvVar {
            name: "ANTHROPIC_API_KEY",
            hint: "Get your key at https://console.anthropic.com/settings/keys",
        },
        default_model: Some("claude-sonnet-4-5".into()),
    }
}

fn profile_openai() -> ProviderProfile {
    ProviderProfile {
        id: "openai".into(),
        codec: || Arc::new(OpenAiResponsesCodec::new()),
        transport_builder: Box::new(|cred| {
            let base = std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com".into());
            Ok(direct_with_codec(
                base,
                DirectAuth::Bearer(cred.expect("openai profile requires credential")),
                &["openai-responses"],
            ))
        }),
        credential: CredentialHint::EnvVar {
            name: "OPENAI_API_KEY",
            hint: "Get your key at https://platform.openai.com/api-keys",
        },
        default_model: Some("gpt-4o-mini".into()),
    }
}

fn profile_openai_chat() -> ProviderProfile {
    ProviderProfile {
        id: "openai-chat".into(),
        codec: || Arc::new(OpenAiChatCodec::new()),
        transport_builder: Box::new(|cred| {
            let base = std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com".into());
            Ok(direct_with_codec(
                base,
                DirectAuth::Bearer(cred.expect("openai-chat profile requires credential")),
                &["openai-chat"],
            ))
        }),
        credential: CredentialHint::EnvVar {
            name: "OPENAI_API_KEY",
            hint: "Get your key at https://platform.openai.com/api-keys \
                   (or set OPENAI_BASE_URL for an OpenAI-compatible server)",
        },
        default_model: Some("gpt-4o-mini".into()),
    }
}

fn profile_gemini() -> ProviderProfile {
    ProviderProfile {
        id: "gemini".into(),
        codec: || Arc::new(GeminiGenerateCodec::new()),
        transport_builder: Box::new(|cred| {
            let base = std::env::var("GEMINI_BASE_URL")
                .unwrap_or_else(|_| "https://generativelanguage.googleapis.com".into());
            Ok(direct_with_codec(
                base,
                DirectAuth::QueryParam {
                    param: "key",
                    value: cred.expect("gemini profile requires credential"),
                },
                &["gemini-generate"],
            ))
        }),
        credential: CredentialHint::EnvVarOneOf {
            names: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            hint: "Get your key at https://aistudio.google.com/apikey",
        },
        default_model: Some("gemini-2.5-flash".into()),
    }
}

#[cfg(feature = "gcp")]
fn profile_vertex_gemini() -> ProviderProfile {
    use crate::client::transport::VertexTransport;
    ProviderProfile {
        id: "vertex-gemini".into(),
        codec: || Arc::new(GeminiGenerateCodec::new()),
        transport_builder: Box::new(|_cred| {
            let transport = futures::executor::block_on(VertexTransport::from_env())?;
            Ok(Arc::new(transport) as Arc<dyn ModelTransport>)
        }),
        credential: CredentialHint::Custom(Box::new(|| {
            Ok(SecretString::from("vertex-adc-managed-internally"))
        })),
        default_model: Some("gemini-2.5-flash".into()),
    }
}

#[cfg(feature = "gcp")]
fn profile_vertex_anthropic() -> ProviderProfile {
    use crate::client::transport::VertexTransport;
    ProviderProfile {
        id: "vertex-anthropic".into(),
        codec: || Arc::new(AnthropicMessagesCodec::new()),
        transport_builder: Box::new(|_cred| {
            let transport = futures::executor::block_on(VertexTransport::from_env())?;
            Ok(Arc::new(transport) as Arc<dyn ModelTransport>)
        }),
        credential: CredentialHint::Custom(Box::new(|| {
            Ok(SecretString::from("vertex-adc-managed-internally"))
        })),
        default_model: Some("claude-sonnet-4-5@20250929".into()),
    }
}

#[cfg(feature = "aws")]
fn profile_bedrock() -> ProviderProfile {
    use crate::client::transport::BedrockTransport;
    ProviderProfile {
        id: "bedrock".into(),
        codec: || Arc::new(BedrockConverseCodec::new()),
        transport_builder: Box::new(|_cred| {
            let transport = futures::executor::block_on(BedrockTransport::from_env())?;
            Ok(Arc::new(transport) as Arc<dyn ModelTransport>)
        }),
        credential: CredentialHint::Custom(Box::new(|| {
            Ok(SecretString::from("bedrock-sigv4-managed-internally"))
        })),
        default_model: Some("anthropic.claude-sonnet-4-5-20250929-v1:0".into()),
    }
}

#[cfg(feature = "azure")]
fn profile_foundry_anthropic() -> ProviderProfile {
    use crate::client::transport::FoundryTransport;
    ProviderProfile {
        id: "foundry-anthropic".into(),
        codec: || Arc::new(AnthropicMessagesCodec::new()),
        transport_builder: Box::new(|_cred| {
            let transport = FoundryTransport::from_env()?;
            Ok(Arc::new(transport) as Arc<dyn ModelTransport>)
        }),
        credential: CredentialHint::Custom(Box::new(|| {
            Ok(SecretString::from("foundry-entra-managed-internally"))
        })),
        default_model: Some("claude-sonnet-4-5".into()),
    }
}

// ─── OpenAI-compatible third-party profiles ────────────────────

fn openai_compat_profile(
    id: &'static str,
    default_base: &'static str,
    base_env: &'static str,
    credential: CredentialHint,
    default_model: &'static str,
) -> ProviderProfile {
    ProviderProfile {
        id: id.into(),
        codec: || Arc::new(OpenAiChatCodec::new()),
        transport_builder: Box::new(move |cred| {
            let base = std::env::var(base_env).unwrap_or_else(|_| default_base.into());
            let auth = match cred {
                Some(secret) => DirectAuth::Bearer(secret),
                None => DirectAuth::None,
            };
            Ok(direct_with_codec(base, auth, &["openai-chat"]))
        }),
        credential,
        default_model: Some(default_model.into()),
    }
}

fn profile_grok() -> ProviderProfile {
    openai_compat_profile(
        "grok",
        "https://api.x.ai/v1",
        "XAI_BASE_URL",
        CredentialHint::EnvVar {
            name: "XAI_API_KEY",
            hint: "Get your key at https://console.x.ai",
        },
        "grok-2-latest",
    )
}

fn profile_groq() -> ProviderProfile {
    openai_compat_profile(
        "groq",
        "https://api.groq.com/openai/v1",
        "GROQ_BASE_URL",
        CredentialHint::EnvVar {
            name: "GROQ_API_KEY",
            hint: "Get your key at https://console.groq.com/keys",
        },
        "llama-3.3-70b-versatile",
    )
}

fn profile_mistral() -> ProviderProfile {
    openai_compat_profile(
        "mistral",
        "https://api.mistral.ai/v1",
        "MISTRAL_BASE_URL",
        CredentialHint::EnvVar {
            name: "MISTRAL_API_KEY",
            hint: "Get your key at https://console.mistral.ai/api-keys",
        },
        "mistral-large-latest",
    )
}

fn profile_together() -> ProviderProfile {
    openai_compat_profile(
        "together",
        "https://api.together.xyz/v1",
        "TOGETHER_BASE_URL",
        CredentialHint::EnvVar {
            name: "TOGETHER_API_KEY",
            hint: "Get your key at https://api.together.xyz/settings/api-keys",
        },
        "meta-llama/Llama-3.3-70B-Instruct-Turbo",
    )
}

fn profile_fireworks() -> ProviderProfile {
    openai_compat_profile(
        "fireworks",
        "https://api.fireworks.ai/inference/v1",
        "FIREWORKS_BASE_URL",
        CredentialHint::EnvVar {
            name: "FIREWORKS_API_KEY",
            hint: "Get your key at https://fireworks.ai/account/api-keys",
        },
        "accounts/fireworks/models/llama-v3p3-70b-instruct",
    )
}

fn profile_cerebras() -> ProviderProfile {
    openai_compat_profile(
        "cerebras",
        "https://api.cerebras.ai/v1",
        "CEREBRAS_BASE_URL",
        CredentialHint::EnvVar {
            name: "CEREBRAS_API_KEY",
            hint: "Get your key at https://cloud.cerebras.ai",
        },
        "llama3.3-70b",
    )
}

fn profile_ollama() -> ProviderProfile {
    openai_compat_profile(
        "ollama",
        "http://localhost:11434/v1",
        "OLLAMA_BASE_URL",
        CredentialHint::None,
        "llama3.2",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_contains_all_canonical_profiles() {
        let r = ProfileRegistry::with_builtins();
        for id in [
            "anthropic",
            "openai",
            "openai-chat",
            "gemini",
            "grok",
            "groq",
            "mistral",
            "together",
            "fireworks",
            "cerebras",
            "ollama",
        ] {
            assert!(r.contains(id), "missing builtin profile: {id}");
        }
    }

    #[test]
    fn ids_are_alphabetical() {
        let r = ProfileRegistry::with_builtins();
        let ids: Vec<_> = r.ids().collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn unknown_profile_returns_actionable_error() {
        let r = ProfileRegistry::with_builtins();
        let err = r.build("does-not-exist").unwrap_err();
        assert!(err.to_string().contains("does-not-exist"));
    }

    #[test]
    fn missing_credential_returns_actionable_error() {
        unsafe {
            std::env::remove_var("XAI_API_KEY");
        }
        let r = ProfileRegistry::with_builtins();
        let err = r.build("grok").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("XAI_API_KEY"));
        assert!(msg.contains("grok"));
    }

    #[test]
    fn ollama_works_without_credential() {
        unsafe {
            std::env::remove_var("OLLAMA_API_KEY");
        }
        let r = ProfileRegistry::with_builtins();
        // Ollama profile uses CredentialHint::None and DirectAuth::None.
        r.build("ollama").expect("ollama should not require a key");
    }

    #[test]
    fn user_can_register_custom_profile() {
        let mut r = ProfileRegistry::with_builtins();
        r.register(ProviderProfile {
            id: "my-custom".into(),
            codec: || Arc::new(OpenAiChatCodec::new()),
            transport_builder: Box::new(|_cred| {
                Ok(direct_with_codec(
                    "http://localhost:9999/v1".into(),
                    DirectAuth::None,
                    &["openai-chat"],
                ))
            }),
            credential: CredentialHint::None,
            default_model: Some("custom-model".into()),
        });
        assert!(r.contains("my-custom"));
        r.build("my-custom").expect("custom profile must build");
    }

    #[test]
    fn user_can_override_builtin_profile() {
        let mut r = ProfileRegistry::with_builtins();
        // Replacing the "ollama" id with a custom variant.
        r.register(ProviderProfile {
            id: "ollama".into(),
            codec: || Arc::new(OpenAiChatCodec::new()),
            transport_builder: Box::new(|_cred| {
                Ok(direct_with_codec(
                    "http://my-internal-ollama:11434/v1".into(),
                    DirectAuth::None,
                    &["openai-chat"],
                ))
            }),
            credential: CredentialHint::None,
            default_model: Some("custom-llama".into()),
        });
        let profile = r.get("ollama").unwrap();
        assert_eq!(profile.default_model.as_deref(), Some("custom-llama"));
    }

    #[test]
    fn default_model_is_exposed_per_profile() {
        let r = ProfileRegistry::with_builtins();
        assert_eq!(
            r.get("anthropic").unwrap().default_model.as_deref(),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(
            r.get("ollama").unwrap().default_model.as_deref(),
            Some("llama3.2")
        );
    }
}
