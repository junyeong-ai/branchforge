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
//! // Or register a custom vendor at runtime. The `transport_builder`
//! // receives a `ProfileBuildContext` carrying the pre-resolved
//! // credential and an injected `EnvLookup` seam; read base URLs
//! // and other overrides through `ctx.env` so tests can inject
//! // fakes without touching the process environment.
//! let mut registry = ProfileRegistry::with_builtins();
//! registry.register(ProviderProfile {
//!     id: "my-internal-llm".into(),
//!     codec: || Arc::new(OpenAiChatCodec::new()),
//!     transport_builder: Box::new(|ctx| {
//!         let base = ctx.env.get_or("MY_LLM_URL", "https://llm.internal");
//!         /* build your own transport using `base` and `ctx.credential` */
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

#![allow(missing_docs)]

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
use crate::common::env::{EnvLookup, SystemEnv};
use crate::{Error, Result};

/// Where the credential for a profile comes from.
///
/// Profiles describe credential sources declaratively so the
/// registry can produce actionable error messages when a credential
/// is missing without baking vendor-specific env-var names into the
/// transport layer.
#[non_exhaustive]
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

    /// Resolve the hint against an injected [`EnvLookup`] and return
    /// a configuration error explaining what the user must set.
    ///
    /// Phase G-3: the `env` seam lets unit tests exercise every
    /// variant (present / absent / first-of-many / custom closure)
    /// without mutating process-wide state. Production code paths
    /// pass `&SystemEnv` and behave identically to the pre-G-3
    /// `std::env::var` implementation.
    fn resolve_with(&self, profile_id: &str, env: &dyn EnvLookup) -> Result<Option<SecretString>> {
        match self {
            Self::EnvVar { name, hint } => {
                let val = env.get(name).ok_or_else(|| {
                    Error::Config(format!("{name} not set for `{profile_id}` profile. {hint}"))
                })?;
                Ok(Some(SecretString::from(val)))
            }
            Self::EnvVarOneOf { names, hint } => {
                for n in *names {
                    if let Some(val) = env.get(n) {
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

/// Phase H-1: build-time context passed to every
/// [`ProviderProfile::transport_builder`] closure. Bundles all inputs
/// a profile builder may need to consult so the builder signature
/// stays stable as new concerns are added.
///
/// Before Phase H-1, `transport_builder` took the credential as a
/// flat parameter and called `std::env::var` directly for base-URL
/// overrides. That broke hermetic testing (no way to inject fake
/// env values) and made future extensions — tenant id, region,
/// tracing context — impossible without breaking the signature of
/// every existing profile. This struct is the fix: new fields slot
/// in as additions, existing profiles ignore them, and the
/// `#[non_exhaustive]` marker documents the evolution contract.
#[non_exhaustive]
#[derive(Debug)]
pub struct ProfileBuildContext<'a> {
    /// Profile id the context belongs to. Forwarded into error
    /// messages so a failing build names the offending profile
    /// rather than reporting an anonymous "credential missing".
    pub profile_id: &'a str,

    /// Credential pre-resolved by the registry before the closure
    /// runs. `None` when the profile declares [`CredentialHint::None`]
    /// (e.g. local Ollama, custom gateways without auth).
    pub credential: Option<SecretString>,

    /// Injected environment lookup seam. Profile builders read
    /// non-credential environment overrides — base URLs, regions,
    /// feature flags — through this trait instead of calling
    /// `std::env::var` directly, which keeps tests hermetic and
    /// parallel-safe.
    pub env: &'a dyn EnvLookup,
}

/// Type alias for the `transport_builder` closure. Uses a
/// higher-ranked lifetime bound (`for<'a>`) so a single boxed
/// closure can accept a fresh [`ProfileBuildContext`] with whatever
/// lifetime the caller has in scope at build time. Callers build
/// the context per `build_with` invocation; the HRTB lets one
/// closure handle any call-site lifetime.
pub type TransportBuilder =
    Box<dyn for<'a> Fn(&ProfileBuildContext<'a>) -> Result<Arc<dyn ModelTransport>> + Send + Sync>;

/// A complete description of how to build one provider client.
pub struct ProviderProfile {
    /// Stable id used as the registry key. Convention: lowercase
    /// kebab-case (`anthropic`, `vertex-gemini`, `my-internal`).
    pub id: Cow<'static, str>,
    /// Codec factory. Pure (no I/O), called once at build time.
    pub codec: fn() -> Arc<dyn ModelCodec>,
    /// Transport builder. Receives a [`ProfileBuildContext`] at
    /// build time carrying the pre-resolved credential, the
    /// injected [`EnvLookup`] seam, and the owning profile id.
    /// Base URLs and other non-credential overrides MUST be read
    /// through `ctx.env` — not `std::env::var` — so tests can
    /// inject fakes via [`ProfileRegistry::build_with`].
    pub transport_builder: TransportBuilder,
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

    /// Resolve credentials against the process environment, build
    /// the codec + transport, and return a fully-wired
    /// [`ProviderClient`]. Convenience wrapper around
    /// [`Self::build_with`] for production callers.
    pub fn build(&self, id: &str) -> Result<ProviderClient> {
        self.build_with(id, &SystemEnv)
    }

    /// Phase G-3 / H-1: build the named profile against an injected
    /// [`EnvLookup`]. Test harnesses pass an in-memory fake so the
    /// whole credential resolution path AND the profile's base-URL
    /// / endpoint overrides can be exercised without touching
    /// process-wide environment state.
    ///
    /// The [`ProfileBuildContext`] constructed here carries the
    /// resolved credential, the injected env seam, and the owning
    /// profile id; the `transport_builder` closure receives it as
    /// a single argument.
    pub fn build_with(&self, id: &str, env: &dyn EnvLookup) -> Result<ProviderClient> {
        let profile = self
            .profiles
            .get(id)
            .ok_or_else(|| Error::Config(format!("unknown provider profile: `{id}`")))?;
        let credential = profile.credential.resolve_with(&profile.id, env)?;
        let codec = (profile.codec)();
        let ctx = ProfileBuildContext {
            profile_id: &profile.id,
            credential,
            env,
        };
        let transport = (profile.transport_builder)(&ctx)?;
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
/// production entry point for examples and minimal applications.
/// Delegates to [`from_env_with`] with [`SystemEnv`].
pub async fn from_env() -> Result<ProviderClient> {
    from_env_with(&SystemEnv).await
}

/// Phase H-1: [`from_env`] against an injected [`EnvLookup`]. Used
/// by tests and embedding scenarios (serverless, sandboxed) where
/// the `BRANCHFORGE_PROVIDER` selector and every downstream base-URL
/// override must come from a fake or vault-backed source instead of
/// the process environment.
pub async fn from_env_with(env: &dyn EnvLookup) -> Result<ProviderClient> {
    let registry = ProfileRegistry::with_builtins();
    let id = env.get("BRANCHFORGE_PROVIDER").ok_or_else(|| {
        let known = registry.ids().collect::<Vec<_>>().join(", ");
        Error::Config(format!(
            "BRANCHFORGE_PROVIDER not set; choose one of: {known}"
        ))
    })?;
    registry.build_with(&id, env)
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

/// Phase H-1 helper: produce a typed `Error::Config` when a profile
/// requires a credential but resolution yielded `None`. The error
/// message names the profile id so multi-profile build sites don't
/// surface an anonymous "credential missing".
fn require_credential(ctx: &ProfileBuildContext<'_>) -> Result<SecretString> {
    ctx.credential.clone().ok_or_else(|| {
        Error::Config(format!(
            "profile `{}` requires a credential but none resolved",
            ctx.profile_id
        ))
    })
}

fn profile_anthropic() -> ProviderProfile {
    ProviderProfile {
        id: "anthropic".into(),
        codec: || Arc::new(AnthropicMessagesCodec::new()),
        transport_builder: Box::new(|ctx| {
            let base = ctx
                .env
                .get_or("ANTHROPIC_BASE_URL", "https://api.anthropic.com");
            let cred = require_credential(ctx)?;
            Ok(direct_with_codec(
                base,
                DirectAuth::XApiKey(cred),
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
        transport_builder: Box::new(|ctx| {
            let base = ctx.env.get_or("OPENAI_BASE_URL", "https://api.openai.com");
            let cred = require_credential(ctx)?;
            Ok(direct_with_codec(
                base,
                DirectAuth::Bearer(cred),
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
        transport_builder: Box::new(|ctx| {
            let base = ctx.env.get_or("OPENAI_BASE_URL", "https://api.openai.com");
            let cred = require_credential(ctx)?;
            Ok(direct_with_codec(
                base,
                DirectAuth::Bearer(cred),
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
        transport_builder: Box::new(|ctx| {
            let base = ctx.env.get_or(
                "GEMINI_BASE_URL",
                "https://generativelanguage.googleapis.com",
            );
            let cred = require_credential(ctx)?;
            Ok(direct_with_codec(
                base,
                DirectAuth::QueryParam {
                    param: "key",
                    value: cred,
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
        transport_builder: Box::new(|_ctx| {
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
        transport_builder: Box::new(|_ctx| {
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
        transport_builder: Box::new(|_ctx| {
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
        transport_builder: Box::new(|_ctx| {
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
        transport_builder: Box::new(move |ctx| {
            let base = ctx.env.get_or(base_env, default_base);
            let auth = match ctx.credential.clone() {
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
            transport_builder: Box::new(|_ctx| {
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
            transport_builder: Box::new(|_ctx| {
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

    // ── Phase G-3: credential resolution via injected EnvLookup ─────

    /// In-memory [`EnvLookup`] fake used by the Phase G-3 test suite.
    /// Deterministic and parallel-safe — tests never touch the
    /// process-wide environment.
    #[derive(Debug, Default)]
    struct FakeEnv(std::collections::HashMap<String, String>);

    impl FakeEnv {
        fn with(mut self, key: &str, val: &str) -> Self {
            self.0.insert(key.into(), val.into());
            self
        }
    }

    impl EnvLookup for FakeEnv {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    fn unwrap_secret(hint: &CredentialHint, env: &dyn EnvLookup) -> String {
        use secrecy::ExposeSecret;
        hint.resolve_with("test", env)
            .unwrap()
            .unwrap()
            .expose_secret()
            .to_string()
    }

    #[test]
    fn env_var_single_returns_value_when_present() {
        let hint = CredentialHint::EnvVar {
            name: "MY_KEY",
            hint: "see docs",
        };
        let env = FakeEnv::default().with("MY_KEY", "top-secret");
        assert_eq!(unwrap_secret(&hint, &env), "top-secret");
    }

    #[test]
    fn env_var_single_missing_errors_with_name_and_hint() {
        let hint = CredentialHint::EnvVar {
            name: "MY_KEY",
            hint: "https://example.com/keys",
        };
        let env = FakeEnv::default();
        let err = hint.resolve_with("test", &env).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("MY_KEY") && msg.contains("https://example.com/keys"),
            "error must surface both the var name and the user-facing hint: `{msg}`"
        );
    }

    #[test]
    fn env_var_one_of_first_match_wins() {
        let hint = CredentialHint::EnvVarOneOf {
            names: &["FIRST", "SECOND"],
            hint: "set one",
        };
        let env = FakeEnv::default()
            .with("FIRST", "first-value")
            .with("SECOND", "second-value");
        assert_eq!(unwrap_secret(&hint, &env), "first-value");
    }

    #[test]
    fn env_var_one_of_second_match_when_first_missing() {
        let hint = CredentialHint::EnvVarOneOf {
            names: &["FIRST", "SECOND"],
            hint: "set one",
        };
        let env = FakeEnv::default().with("SECOND", "second-value");
        assert_eq!(unwrap_secret(&hint, &env), "second-value");
    }

    #[test]
    fn env_var_one_of_all_missing_errors_lists_all_names() {
        let hint = CredentialHint::EnvVarOneOf {
            names: &["FIRST", "SECOND"],
            hint: "set FIRST or SECOND",
        };
        let env = FakeEnv::default();
        let err = hint.resolve_with("test", &env).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("FIRST") && msg.contains("SECOND"),
            "error must name every candidate var: `{msg}`"
        );
    }

    #[test]
    fn none_hint_resolves_to_no_credential() {
        let hint = CredentialHint::None;
        let env = FakeEnv::default();
        let result = hint.resolve_with("test", &env).unwrap();
        assert!(
            result.is_none(),
            "None hint must produce no credential regardless of env"
        );
    }

    #[test]
    fn custom_closure_resolves_with_injected_env_ignored() {
        use secrecy::ExposeSecret;
        let hint = CredentialHint::Custom(Box::new(|| {
            Ok(SecretString::from("closure-value".to_string()))
        }));
        let env = FakeEnv::default();
        let resolved = hint
            .resolve_with("test", &env)
            .unwrap()
            .expect("custom closure yields a credential");
        assert_eq!(resolved.expose_secret(), "closure-value");
    }

    /// `ProfileRegistry::build_with` exercises the full preset
    /// pipeline against a fake env. A profile whose credential is
    /// present must build; a profile whose credential is missing
    /// must surface a typed Config error pointing at the var name.
    #[test]
    fn profile_registry_build_with_fake_env_round_trips() {
        let registry = ProfileRegistry::with_builtins();

        // `anthropic` requires ANTHROPIC_API_KEY.
        let ok_env = FakeEnv::default().with("ANTHROPIC_API_KEY", "sk-fake");
        assert!(
            registry.build_with("anthropic", &ok_env).is_ok(),
            "build must succeed when the required key is present"
        );

        let missing_env = FakeEnv::default();
        let err = registry.build_with("anthropic", &missing_env).unwrap_err();
        assert!(
            err.to_string().contains("ANTHROPIC_API_KEY"),
            "missing credential error must name the var: `{err}`"
        );
    }

    #[test]
    fn profile_registry_build_with_unknown_id_errors() {
        let registry = ProfileRegistry::with_builtins();
        let env = FakeEnv::default();
        let err = registry
            .build_with("nonexistent-provider", &env)
            .unwrap_err();
        assert!(
            err.to_string().contains("nonexistent-provider"),
            "unknown-profile error must name the id: `{err}`"
        );
    }

    // ── Phase H-1: ProfileBuildContext seam ─────────────────────────

    /// Phase H-1 regression: a profile builder that reads a base URL
    /// override from `ctx.env` MUST see the injected `FakeEnv` value,
    /// not the process environment. The pre-H-1 implementation called
    /// `std::env::var` directly inside the closure and this test
    /// would have failed because the injected fake was ignored.
    ///
    /// We exercise this through the `custom` profile path rather than
    /// `anthropic` because we do not want the test to depend on a
    /// real `DirectTransport::new` being built against a reachable
    /// URL — instead we register a profile that simply echoes the
    /// resolved base URL into a recorded cell.
    #[test]
    fn phase_h1_profile_builder_reads_base_url_from_injected_env() {
        use std::sync::Mutex;

        static RECORDED: Mutex<Option<String>> = Mutex::new(None);

        let mut registry = ProfileRegistry::empty();
        registry.register(ProviderProfile {
            id: "h1-echo".into(),
            codec: || Arc::new(OpenAiChatCodec::new()),
            transport_builder: Box::new(|ctx| {
                let base = ctx
                    .env
                    .get_or("H1_ECHO_BASE_URL", "https://default.invalid");
                *RECORDED.lock().unwrap() = Some(base.clone());
                Ok(direct_with_codec(base, DirectAuth::None, &["openai-chat"]))
            }),
            credential: CredentialHint::None,
            default_model: Some("echo-model".into()),
        });

        let env = FakeEnv::default().with("H1_ECHO_BASE_URL", "http://mock.local:9999");
        registry
            .build_with("h1-echo", &env)
            .expect("custom h1-echo profile must build against fake env");

        let recorded = RECORDED.lock().unwrap().clone();
        assert_eq!(
            recorded.as_deref(),
            Some("http://mock.local:9999"),
            "transport_builder must read base URL through ctx.env, not std::env::var"
        );
    }

    /// Phase H-1 regression: when a profile requires a credential but
    /// none resolves, the error MUST name the offending profile id
    /// (pre-H-1 builders called `cred.expect("... profile requires
    /// credential")` with a hardcoded string, which made multi-profile
    /// build sites hard to diagnose). The context-carried profile id
    /// closes that gap.
    #[test]
    fn phase_h1_missing_credential_error_names_the_profile() {
        let registry = ProfileRegistry::with_builtins();
        let env = FakeEnv::default(); // no ANTHROPIC_API_KEY
        let err = registry.build_with("anthropic", &env).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("anthropic") && msg.contains("ANTHROPIC_API_KEY"),
            "credential-missing error must name both profile and var: `{msg}`"
        );
    }

    /// Phase H-1 regression: `from_env_with` routes the selector and
    /// every profile's base-URL override through a single injected
    /// env, so a test can fully drive the "one-call entry point"
    /// without touching process state.
    #[tokio::test]
    async fn phase_h1_from_env_with_respects_injected_selector() {
        let env = FakeEnv::default()
            .with("BRANCHFORGE_PROVIDER", "ollama")
            .with("OLLAMA_BASE_URL", "http://mock-ollama:11434/v1");
        // `ollama` profile has CredentialHint::None so no credential
        // resolution is needed. We only verify that the selector and
        // build path both honor the fake env.
        from_env_with(&env)
            .await
            .expect("from_env_with must resolve and build through fake env");
    }

    /// Phase H-1 regression: `from_env_with` error path when the
    /// selector is absent from the injected env. Lists the known
    /// profile ids so the user can pick one.
    #[tokio::test]
    async fn phase_h1_from_env_with_missing_selector_lists_known() {
        let env = FakeEnv::default(); // no BRANCHFORGE_PROVIDER
        let err = from_env_with(&env).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("BRANCHFORGE_PROVIDER") && msg.contains("anthropic"),
            "missing-selector error must list known profiles: `{msg}`"
        );
    }
}
