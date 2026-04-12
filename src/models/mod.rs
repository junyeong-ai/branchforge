#![allow(missing_docs)]

mod builtin;
mod family;
mod provider;
mod registry;
mod spec;

pub use family::{ModelFamily, ModelRole};
pub use provider::{ProviderIds, ProviderKind};
pub use registry::{ModelRegistry, registry};
pub use spec::{Capabilities, LONG_CONTEXT_THRESHOLD, ModelId, ModelSpec, ModelVersion};

pub mod context_window {
    use super::registry;

    pub const STANDARD: u64 = 200_000;
    pub const EXTENDED: u64 = 1_000_000;
    pub const DEFAULT: u64 = 128_000;

    pub fn for_model(model: &str) -> u64 {
        registry()
            .resolve(model)
            .map(|spec| spec.capabilities.context_window)
            .unwrap_or(DEFAULT)
    }

    pub fn for_model_extended(model: &str, extended_enabled: bool) -> u64 {
        registry()
            .resolve(model)
            .map(|spec| spec.capabilities.effective_context(extended_enabled))
            .unwrap_or(DEFAULT)
    }
}

pub mod output_tokens {
    use super::registry;

    pub const DEFAULT: u64 = 8_192;

    pub fn for_model(model: &str) -> u64 {
        registry()
            .resolve(model)
            .map(|spec| spec.capabilities.max_output_tokens)
            .unwrap_or(DEFAULT)
    }
}
