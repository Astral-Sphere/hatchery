//! The provider registry: registration handles, atomic replacement (ADR-0009 discipline 3).
//!
//! A handle is the only way to reach a provider after registration, and dropping or replacing
//! through it is the only way to retire one. A session assembled earlier keeps the `Arc` it was
//! given — "runtime provider swap" means whole-table atomic replacement *between* turns, never a
//! mid-turn surprise.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::config::ProviderConfig;
use crate::provider::ChatCompletionsProvider;

/// Every provider this process knows, by id (`"deepseek"`, `"my-gateway"`, ...).
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    slots: Arc<RwLock<HashMap<String, Arc<ChatCompletionsProvider>>>>,
}

/// Controls one registration: dispose to retire, replace to swap under the same id.
pub struct Registration {
    registry: ProviderRegistry,
    id: String,
}

impl ProviderRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a provider under `id`, replacing any earlier one.
    ///
    /// Every config states its own facts; the *daemon's* config layer seeds its deepseek/qwen
    /// rows from [`ProviderConfig::builtin`] before a user layer merges over them.
    pub fn register(&self, id: impl Into<String>, config: ProviderConfig) -> Registration {
        let id = id.into();
        let mut slots = self
            .slots
            .write()
            .expect("provider registry is not poisoned");
        slots.insert(id.clone(), Arc::new(ChatCompletionsProvider::new(config)));
        Registration {
            registry: self.clone(),
            id,
        }
    }

    /// The provider registered under `id`.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Arc<ChatCompletionsProvider>> {
        self.slots
            .read()
            .expect("provider registry is not poisoned")
            .get(id)
            .cloned()
    }

    /// The provider that serves `model`, by exact membership of its configured model list.
    ///
    /// One registered provider serves any model name — a single provider deployment should not
    /// have to repeat its own name in the model list. When several providers list the same
    /// model, the answer is `None`: a `HashMap` iterates in a randomized order, so picking a
    /// winner would send the same config to different providers on different runs.
    #[must_use]
    pub fn resolve_model(&self, model: &str) -> Option<Arc<ChatCompletionsProvider>> {
        let slots = self
            .slots
            .read()
            .expect("provider registry is not poisoned");
        if slots.len() == 1 {
            return slots.values().next().cloned();
        }
        let mut serving = slots
            .values()
            .filter(|provider| provider.config().models.iter().any(|m| m == model));
        let first = serving.next()?;
        serving.next().is_none().then(|| first.clone())
    }

    /// Every registered id, sorted for display.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .slots
            .read()
            .expect("provider registry is not poisoned")
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }
}

impl Registration {
    /// Retires the registration. Sessions already holding the provider keep working.
    pub fn dispose(self) {
        let mut slots = self
            .registry
            .slots
            .write()
            .expect("provider registry is not poisoned");
        slots.remove(&self.id);
    }

    /// Swaps the provider under the same id, atomically for new sessions.
    pub fn replace(&self, config: ProviderConfig) {
        let mut slots = self
            .registry
            .slots
            .write()
            .expect("provider registry is not poisoned");
        slots.insert(
            self.id.clone(),
            Arc::new(ChatCompletionsProvider::new(config)),
        );
    }

    /// The id this registration owns.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry builds real HTTP clients, which needs a TLS backend installed exactly once
    /// per process; a second install is simply declined, so parallel tests may all call this.
    fn tls_installed() {
        crate::install_tls_provider();
    }

    fn config(env_key: &str) -> ProviderConfig {
        tls_installed();
        ProviderConfig::new("https://x", env_key)
    }

    #[test]
    fn register_get_and_dispose() {
        let registry = ProviderRegistry::new();
        let registration = registry.register("deepseek", config("K1"));
        assert_eq!(registration.id(), "deepseek");
        assert!(registry.get("deepseek").is_some());

        registration.dispose();
        assert!(registry.get("deepseek").is_none());
    }

    #[test]
    fn a_held_arc_survives_its_disposal() {
        let registry = ProviderRegistry::new();
        let registration = registry.register("deepseek", config("K1"));
        let held = registry.get("deepseek").expect("registered");
        registration.dispose();
        assert_eq!(
            held.config().env_key,
            "K1",
            "the assembled session keeps its provider"
        );
    }

    #[test]
    fn replace_swaps_under_the_same_id() {
        let registry = ProviderRegistry::new();
        let registration = registry.register("gw", config("OLD"));
        assert_eq!(registry.get("gw").expect("present").config().env_key, "OLD");

        registration.replace(config("NEW"));
        assert_eq!(registry.get("gw").expect("present").config().env_key, "NEW");
    }

    #[test]
    fn a_single_provider_serves_any_model() {
        let registry = ProviderRegistry::new();
        registry.register("gw", config("K").with_models(["m1"]));
        assert!(registry.resolve_model("m1").is_some());
        assert!(
            registry.resolve_model("anything-else").is_some(),
            "one provider, no list to consult"
        );
    }

    #[test]
    fn many_providers_resolve_by_model_list_only() {
        let registry = ProviderRegistry::new();
        registry.register("a", config("K").with_models(["m1"]));
        registry.register("b", config("K").with_models(["m2"]));
        assert!(registry.resolve_model("m2").is_some_and(
            |p| p.config().env_key == *"K" && p.config().models == vec!["m2".to_owned()]
        ));
        assert!(
            registry.resolve_model("m3").is_none(),
            "unlisted model, ambiguous registry: refuse rather than guess"
        );
    }

    #[test]
    fn overlapping_model_lists_refuse_rather_than_guess() {
        // A HashMap iterates in a randomized order: picking a winner on a tie would send the
        // same model to different providers on different runs. Refusal is the deterministic
        // answer.
        tls_installed();
        let registry = ProviderRegistry::new();
        registry.register("a", config("K").with_models(["shared"]));
        registry.register("b", config("K").with_models(["shared"]));
        assert!(registry.resolve_model("shared").is_none());
    }

    #[test]
    fn ids_are_sorted_for_display() {
        let registry = ProviderRegistry::new();
        registry.register("qwen", config("K"));
        registry.register("deepseek", config("K"));
        assert_eq!(
            registry.ids(),
            vec!["deepseek".to_owned(), "qwen".to_owned()]
        );
    }
}
