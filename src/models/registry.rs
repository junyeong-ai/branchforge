#![allow(missing_docs)]

use std::collections::HashMap;
use std::sync::OnceLock;

use super::builtin;
use super::family::{ModelFamily, ModelRole};
use super::provider::ProviderKind;
use super::spec::{ModelId, ModelSpec};

static REGISTRY: OnceLock<ModelRegistry> = OnceLock::new();

pub fn registry() -> &'static ModelRegistry {
    REGISTRY.get_or_init(ModelRegistry::builtins)
}

#[derive(Debug, Default)]
pub struct ModelRegistry {
    models: HashMap<ModelId, ModelSpec>,
    aliases: HashMap<String, ModelId>,
    by_family: HashMap<ModelFamily, Vec<ModelId>>,
    defaults: HashMap<ModelRole, ModelId>,
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn builtins() -> Self {
        let mut registry = Self::new();
        builtin::register_all(&mut registry);
        registry
    }

    pub fn register(&mut self, spec: ModelSpec) {
        let id = spec.id.clone();
        let family = spec.family;

        self.models.insert(id.clone(), spec);

        self.by_family.entry(family).or_default().push(id.clone());

        for alias in family.aliases() {
            self.aliases.insert(alias.to_string(), id.clone());
        }
    }

    pub fn set_default(&mut self, role: ModelRole, id: ModelId) {
        self.defaults.insert(role, id);
    }

    pub fn add_alias(&mut self, alias: impl Into<String>, id: ModelId) {
        self.aliases.insert(alias.into(), id);
    }

    pub fn get(&self, id: &str) -> Option<&ModelSpec> {
        self.models.get(id)
    }

    pub fn resolve(&self, alias_or_id: &str) -> Option<&ModelSpec> {
        // Direct ID lookup
        if let Some(spec) = self.models.get(alias_or_id) {
            return Some(spec);
        }

        // Alias lookup
        if let Some(canonical) = self.aliases.get(alias_or_id) {
            return self.models.get(canonical);
        }

        // Fallback: substring matching for model family
        let lower = alias_or_id.to_lowercase();
        let fallback = if lower.contains("opus") {
            self.latest(ModelFamily::Opus)
        } else if lower.contains("sonnet") {
            self.latest(ModelFamily::Sonnet)
        } else if lower.contains("haiku") {
            self.latest(ModelFamily::Haiku)
        } else {
            None
        };

        if let Some(spec) = &fallback {
            tracing::debug!(
                input = alias_or_id,
                resolved = %spec.id,
                "model resolved via substring fallback"
            );
        }

        fallback
    }

    pub fn default_for_role(&self, role: ModelRole) -> Option<&ModelSpec> {
        let id = self.defaults.get(&role)?;
        self.models.get(id)
    }

    pub fn latest(&self, family: ModelFamily) -> Option<&ModelSpec> {
        let ids = self.by_family.get(&family)?;
        let id = ids.first()?;
        self.models.get(id)
    }

    pub fn for_provider(&self, provider: ProviderKind, provider_id: &str) -> Option<&ModelSpec> {
        self.models
            .values()
            .find(|spec| spec.provider_ids.for_provider(provider) == Some(provider_id))
    }

    pub fn family_models(&self, family: ModelFamily) -> Vec<&ModelSpec> {
        self.by_family
            .get(&family)
            .map(|ids| ids.iter().filter_map(|id| self.models.get(id)).collect())
            .unwrap_or_default()
    }

    pub fn all(&self) -> impl Iterator<Item = &ModelSpec> {
        self.models.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_resolve() {
        let registry = ModelRegistry::builtins();

        assert!(registry.resolve("sonnet").is_some());
        assert!(registry.resolve("haiku").is_some());
        assert!(registry.resolve("opus").is_some());
    }

    #[test]
    fn test_registry_default_roles() {
        let registry = ModelRegistry::builtins();

        assert!(registry.default_for_role(ModelRole::Primary).is_some());
        assert!(registry.default_for_role(ModelRole::Small).is_some());
        assert!(registry.default_for_role(ModelRole::Reasoning).is_some());
    }

    #[test]
    fn test_registry_global() {
        assert!(registry().resolve("sonnet").is_some());
    }
}
