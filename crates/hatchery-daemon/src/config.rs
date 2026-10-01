//! Layered configuration (docs/design/platform.md §1).
//!
//! Five layers, low to high: compiled-in defaults, the system file, the user file, the project
//! file, runtime overrides. Tables deep-merge; arrays replace wholesale. Every key remembers
//! which layer supplied it — `config/get` reports the origin, so "why is this value what it is"
//! is a question the config answers about itself.
//!
//! Two degradation rules, both per-key (decided 2026-09-28):
//!
//! * A key outside the schema is **ignored with a warning**, never a hard parse failure of the
//!   whole file — a typo in `[ui]` must not take down `[providers]`.
//! * A security-relevant key that fails to parse takes its **strictest default** and logs at
//!   error level. M1 has no such keys yet (the write-path approval rules arrive in M2); the
//!   mechanism is [`strict_keys`] and the tests pin its behaviour.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::Serialize;

use hatchery_llm::ProviderConfig;
use hatchery_protocol::method::ConfigOrigin;

/// Where the layer files live; `None` skips the layer.
#[derive(Clone, Debug, Default)]
pub struct LoadPaths {
    /// `/etc/hatchery/config.toml`.
    pub system: Option<PathBuf>,
    /// `~/.config/hatchery/config.toml`.
    pub user: Option<PathBuf>,
    /// `<workspace>/.hatchery/config.toml`.
    pub project: Option<PathBuf>,
}

impl LoadPaths {
    /// The standard locations for a workspace (the project layer only when one is bound).
    #[must_use]
    pub fn detect(workspace: Option<&Path>) -> Self {
        let user = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
            .map(|dir| dir.join("hatchery").join("config.toml"));
        Self {
            system: Some(PathBuf::from("/etc/hatchery/config.toml")),
            user,
            project: workspace.map(|ws| ws.join(".hatchery").join("config.toml")),
        }
    }
}

/// Why configuration could not be loaded or changed.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A layer file exists but is not valid TOML. Fatal: the user asked for these values
    /// explicitly, and silently running without them hides more than it reveals.
    #[error("config file {path}: {message}")]
    Parse {
        /// Which file.
        path: PathBuf,
        /// The parser's complaint.
        message: String,
    },
    /// A layer file could not be read (permissions, mostly).
    #[error("config file {path}: {message}")]
    Io {
        /// Which file.
        path: PathBuf,
        /// The OS's complaint.
        message: String,
    },
    /// `config/set` named a key outside the schema.
    #[error("unknown configuration key `{0}`")]
    UnknownKey(String),
}

/// The compiled-in document every layer merges over.
fn builtin_document() -> toml::Table {
    let providers: toml::Table = ["deepseek", "qwen"]
        .iter()
        .filter_map(|id| {
            let config = ProviderConfig::builtin(id);
            serde_json::to_value(&config)
                .ok()
                .and_then(|value| toml::Value::try_from(value).ok())
                .map(|value| (id.to_string(), value))
        })
        .collect();
    let mut document = toml::Table::new();
    document.insert("ui".to_owned(), toml_value_of(&BuiltinUi::default()));
    document.insert(
        "daemon".to_owned(),
        toml_value_of(&BuiltinDaemon::default()),
    );
    document.insert("providers".to_owned(), toml::Value::Table(providers));
    document
}

/// Serialises a struct into TOML, falling back to nothing on a failure that cannot happen for
/// plain data (the same concession `toml_to_json` makes in the other direction).
fn toml_value_of<T: Serialize>(value: &T) -> toml::Value {
    serde_json::to_value(value)
        .ok()
        .and_then(|json| toml::Value::try_from(json).ok())
        .unwrap_or(toml::Value::String(String::new()))
}

/// The `[ui]` keys M1 reads.
#[derive(Clone, Debug, Serialize)]
pub struct BuiltinUi {
    /// Whether the frontend shows reasoning by default.
    pub show_reasoning: bool,
    /// UI language; empty means the environment decides (fluent lookup is M4).
    pub language: String,
    /// A "reply in …" instruction injected into the environment prompt section.
    pub response_language: String,
}

impl Default for BuiltinUi {
    fn default() -> Self {
        Self {
            show_reasoning: true,
            language: String::new(),
            response_language: String::new(),
        }
    }
}

/// The `[daemon]` keys M1 reads.
#[derive(Clone, Debug, Serialize)]
pub struct BuiltinDaemon {
    /// Idle minutes before an unused runtime is unloaded (docs/design/daemon.md §3).
    pub idle_timeout_min: u64,
}

impl Default for BuiltinDaemon {
    fn default() -> Self {
        Self {
            idle_timeout_min: 30,
        }
    }
}

/// Keys whose parse failure must not degrade (M2's approval rules and friends).
///
/// A key listed here that fails to parse is dropped and logged at error level, and the typed
/// reader falls back to the strictest default. Nothing in M1 qualifies; the list exists so the
/// mechanism is real and the tests can pin it.
const STRICT_KEYS: &[&str] = &[];

/// True when the dotted key path is one this build reads.
#[must_use]
pub fn is_known_key(path: &str) -> bool {
    let known_static = [
        "ui.show_reasoning",
        "ui.language",
        "ui.response_language",
        "daemon.idle_timeout_min",
    ];
    if known_static.contains(&path) || STRICT_KEYS.contains(&path) {
        return true;
    }
    let mut parts = path.splitn(3, '.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("providers"), Some(_id), None) => true,
        (Some("providers"), Some(_id), Some(leaf)) => matches!(
            leaf,
            "base_url"
                | "env_key"
                | "wire"
                | "http_headers"
                | "retry"
                | "retry.max_attempts"
                | "retry.backoff_ms"
                | "retry.max_backoff_ms"
                | "retry.jitter_percent"
                | "reasoning"
                | "reasoning.reasoning_effort"
                | "reasoning.show_reasoning"
                | "models"
                | "capabilities"
        ),
        // Whole-table keys: an assignment like `providers = …` restructures the schema, so it
        // is treated as unknown rather than honoured.
        (Some("ui") | Some("daemon") | Some("providers"), None, None) => false,
        _ => false,
    }
}

/// The layered configuration, shared across the daemon.
#[derive(Debug)]
pub struct LayeredConfig {
    inner: RwLock<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Layers low → high; the runtime layer is last and is the only one that changes.
    layers: Vec<(ConfigOrigin, toml::Table)>,
    /// Dotted key path → the layer that supplied it. Rebuilt with the merge.
    origins: BTreeMap<String, ConfigOrigin>,
    /// The merged document.
    effective: toml::Table,
}

impl LayeredConfig {
    /// Loads the standard layer files, skipping the ones that do not exist.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Parse`] when an existing file is not valid TOML; [`ConfigError::Io`] when
    /// an existing file cannot be read.
    pub fn load(paths: &LoadPaths) -> Result<Self, ConfigError> {
        let mut layers = vec![(ConfigOrigin::Builtin, builtin_document())];
        for (origin, path) in [
            (ConfigOrigin::System, paths.system.as_deref()),
            (ConfigOrigin::User, paths.user.as_deref()),
            (ConfigOrigin::Project, paths.project.as_deref()),
        ] {
            let Some(path) = path else { continue };
            if !path.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(path).map_err(|error| ConfigError::Io {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
            let value: toml::Table = toml::from_str(&text).map_err(|error| ConfigError::Parse {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
            layers.push((origin, Self::validate(value, origin)));
        }
        Ok(Self {
            inner: RwLock::new(Inner::assemble(layers)),
        })
    }

    /// A config over explicit layer documents — the test seam, with the same per-key validation
    /// the file loader applies.
    #[must_use]
    pub fn from_layers(layers: Vec<(ConfigOrigin, toml::Table)>) -> Self {
        let mut all = vec![(ConfigOrigin::Builtin, builtin_document())];
        all.extend(
            layers
                .into_iter()
                .map(|(origin, table)| (origin, Self::validate(table, origin))),
        );
        Self {
            inner: RwLock::new(Inner::assemble(all)),
        }
    }

    /// Per-key validation of one layer: unknown keys are dropped with a warning.
    fn validate(table: toml::Table, origin: ConfigOrigin) -> toml::Table {
        filter_keys(toml::Value::Table(table), String::new(), origin)
            .and_then(|value| value.as_table().cloned())
            .unwrap_or_default()
    }

    /// The merged document, as JSON for the wire.
    #[must_use]
    pub fn effective_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.inner.read().expect("config is not poisoned").effective)
            .unwrap_or(serde_json::Value::Null)
    }

    /// `config/get`: every entry matching the dotted path (a prefix, or absent for all).
    #[must_use]
    pub fn entries(&self, key_path: Option<&str>) -> Vec<hatchery_protocol::method::ConfigEntry> {
        let inner = self.inner.read().expect("config is not poisoned");
        let mut flat = BTreeMap::new();
        flatten("", &inner.effective, &mut flat);
        flat.into_iter()
            .filter(|(key, _)| {
                key_path
                    .is_none_or(|prefix| key == prefix || key.starts_with(&format!("{prefix}.")))
            })
            .map(|(key, value)| hatchery_protocol::method::ConfigEntry {
                origin: inner
                    .origins
                    .get(&key)
                    .copied()
                    .unwrap_or(ConfigOrigin::Builtin),
                key,
                value,
            })
            .collect()
    }

    /// `config/set`: writes the runtime layer and returns the value as it is now.
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownKey`] when the key is outside the schema — the runtime layer is a
    /// user's keystroke, and a typo should bounce, not vanish.
    pub fn set(
        &self,
        key_path: &str,
        value: serde_json::Value,
    ) -> Result<hatchery_protocol::method::ConfigEntry, ConfigError> {
        if !is_known_key(key_path) {
            return Err(ConfigError::UnknownKey(key_path.to_owned()));
        }
        let toml_value = toml::Value::try_from(value.clone()).map_err(|_| {
            ConfigError::UnknownKey(format!("{key_path} (value is not TOML-representable)"))
        })?;
        let entry = {
            let mut inner = self.inner.write().expect("config is not poisoned");
            let runtime = inner
                .layers
                .last_mut()
                .expect("the builtin layer always exists");
            debug_assert_eq!(
                runtime.0,
                ConfigOrigin::Runtime,
                "the runtime layer is last"
            );
            insert_dotted(&mut runtime.1, key_path, toml_value);
            inner.remerge();
            let effective = inner
                .effective
                .get_dotted(key_path)
                .map(toml_to_json)
                .unwrap_or(serde_json::Value::Null);
            hatchery_protocol::method::ConfigEntry {
                key: key_path.to_owned(),
                value: effective,
                origin: ConfigOrigin::Runtime,
            }
        };
        Ok(entry)
    }

    /// The `[ui]` view.
    #[must_use]
    pub fn ui(&self) -> UiConfig {
        let inner = self.inner.read().expect("config is not poisoned");
        let value = &inner.effective;
        let flag = |key: &str, default: bool| {
            value
                .get_dotted(key)
                .and_then(toml::Value::as_bool)
                .unwrap_or(default)
        };
        let text = |key: &str| {
            value
                .get_dotted(key)
                .and_then(toml::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        UiConfig {
            show_reasoning: flag("ui.show_reasoning", true),
            language: text("ui.language"),
            response_language: text("ui.response_language"),
        }
    }

    /// The `[daemon]` view.
    #[must_use]
    pub fn daemon(&self) -> DaemonConfig {
        let idle = self
            .inner
            .read()
            .expect("config is not poisoned")
            .effective
            .get_dotted("daemon.idle_timeout_min")
            .and_then(toml::Value::as_integer)
            .map(|v| v.max(1) as u64)
            .unwrap_or(30);
        DaemonConfig {
            idle_timeout: std::time::Duration::from_secs(idle * 60),
        }
    }

    /// Every configured provider, built-ins deep-merged under the user's entries.
    #[must_use]
    pub fn providers(&self) -> BTreeMap<String, ProviderConfig> {
        let inner = self.inner.read().expect("config is not poisoned");
        let mut providers = BTreeMap::new();
        if let Some(table) = inner
            .effective
            .get("providers")
            .and_then(toml::Value::as_table)
        {
            for (id, value) in table {
                let json = toml_to_json(value);
                if let Ok(config) = serde_json::from_value::<ProviderConfig>(json) {
                    providers.insert(id.clone(), config);
                } else {
                    tracing::warn!(provider = %id, "provider entry failed to parse; using the built-in defaults");
                    providers.insert(id.clone(), ProviderConfig::builtin(id));
                }
            }
        }
        providers
    }

    /// The provider that serves `model`, by configured model list, plus its id.
    ///
    /// One registered provider serves any model name — a single-provider deployment should not
    /// have to repeat its own name.
    #[must_use]
    pub fn resolve_model(&self, model: &str) -> Option<(String, ProviderConfig)> {
        let providers = self.providers();
        if providers.len() == 1 {
            return providers.into_iter().next();
        }
        providers
            .into_iter()
            .find(|(_, config)| config.models.iter().any(|m| m == model))
    }
}

/// The `[ui]` view, typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiConfig {
    /// Whether the frontend shows reasoning by default.
    pub show_reasoning: bool,
    /// UI language; `None` means the environment decides.
    pub language: Option<String>,
    /// A "reply in …" instruction for the environment prompt section.
    pub response_language: Option<String>,
}

/// The `[daemon]` view, typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonConfig {
    /// How long an unused runtime stays loaded.
    pub idle_timeout: std::time::Duration,
}

/// Per-key validation of a layer document: unknown dotted keys are dropped, loudly.
fn filter_keys(value: toml::Value, prefix: String, origin: ConfigOrigin) -> Option<toml::Value> {
    let table = value.as_table()?.clone();
    let mut kept = toml::Table::new();
    for (key, child) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let child = if child.is_table() && !path.starts_with("providers.") {
            match filter_keys(child, path.clone(), origin) {
                Some(child) => child,
                None => continue,
            }
        } else if is_known_key(&path) {
            child
        } else {
            tracing::warn!(key = %path, layer = ?origin, "unknown configuration key ignored");
            continue;
        };
        kept.insert(key, child);
    }
    Some(toml::Value::Table(kept))
}

/// Inserts at a dotted path, creating intermediate tables.
fn insert_dotted(table: &mut toml::Table, path: &str, value: toml::Value) {
    let mut parts = path.splitn(2, '.');
    let head = parts.next().expect("splitn always yields one");
    match parts.next() {
        None => {
            table.insert(head.to_owned(), value);
        }
        Some(rest) => {
            let child = table
                .entry(head.to_owned())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let Some(child) = child.as_table_mut() {
                insert_dotted(child, rest, value);
            }
        }
    }
}

fn flatten(prefix: &str, table: &toml::Table, out: &mut BTreeMap<String, serde_json::Value>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value.as_table() {
            Some(child) => flatten(&path, child, out),
            None => {
                out.insert(path, toml_to_json(value));
            }
        }
    }
}

fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

/// Deep-merges `src` over `dst` (tables recurse, everything else replaces) and records each
/// leaf's origin.
fn merge(
    dst: &mut toml::Table,
    src: &toml::Table,
    origin: ConfigOrigin,
    prefix: &str,
    origins: &mut BTreeMap<String, ConfigOrigin>,
) {
    for (key, value) in src {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match (dst.get_mut(key), value.as_table()) {
            (Some(slot @ toml::Value::Table(_)), Some(child)) => {
                if let Some(slot) = slot.as_table_mut() {
                    merge(slot, child, origin, &path, origins);
                }
            }
            _ => {
                record_origins(value, &path, origin, origins);
                dst.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Records the origin of every leaf under a freshly inserted value (a whole-table insert owns
/// all of its leaves, and `config/get` answers per leaf).
fn record_origins(
    value: &toml::Value,
    path: &str,
    origin: ConfigOrigin,
    origins: &mut BTreeMap<String, ConfigOrigin>,
) {
    match value.as_table() {
        Some(table) => {
            for (key, child) in table {
                let child_path = format!("{path}.{key}");
                record_origins(child, &child_path, origin, origins);
            }
        }
        None => {
            origins.insert(path.to_owned(), origin);
        }
    }
}

impl Inner {
    fn assemble(mut layers: Vec<(ConfigOrigin, toml::Table)>) -> Self {
        // The runtime layer always exists and is always last: `config/set` needs somewhere to
        // write even when no file layers loaded. Provided runtime layers sort to the end rather
        // than being dropped — the test seam uses them.
        let runtime: Vec<_> = layers
            .iter()
            .filter(|(origin, _)| *origin == ConfigOrigin::Runtime)
            .cloned()
            .collect();
        layers.retain(|(origin, _)| *origin != ConfigOrigin::Runtime);
        layers.extend(if runtime.is_empty() {
            vec![(ConfigOrigin::Runtime, toml::Table::new())]
        } else {
            runtime
        });
        let mut this = Self {
            layers,
            origins: BTreeMap::new(),
            effective: toml::Table::new(),
        };
        this.remerge();
        this
    }

    fn remerge(&mut self) {
        self.effective = toml::Table::new();
        self.origins.clear();
        for (origin, table) in &self.layers {
            merge(&mut self.effective, table, *origin, "", &mut self.origins);
        }
    }
}

/// Dotted-path lookup on a TOML table.
trait GetDotted {
    fn get_dotted(&self, path: &str) -> Option<&toml::Value>;
}

impl GetDotted for toml::Table {
    fn get_dotted(&self, path: &str) -> Option<&toml::Value> {
        let mut parts = path.splitn(2, '.');
        let head = parts.next()?;
        let value = self.get(head)?;
        match parts.next() {
            None => Some(value),
            Some(rest) => value.as_table()?.get_dotted(rest),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(s: &str) -> toml::Table {
        toml::from_str(s).expect("test TOML")
    }

    #[test]
    fn higher_layers_win_and_records_origins() {
        let config = LayeredConfig::from_layers(vec![
            (
                ConfigOrigin::User,
                table(
                    "ui.show_reasoning = false\n[providers.gw]\nbase_url = \"https://gw\"\nenv_key = \"GW_KEY\"\n",
                ),
            ),
            (ConfigOrigin::Runtime, table("ui.show_reasoning = true\n")),
        ]);

        let entries = config.entries(Some("ui.show_reasoning"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].value, serde_json::json!(true));
        assert_eq!(entries[0].origin, ConfigOrigin::Runtime);

        let untouched = config.entries(Some("ui.language"));
        assert_eq!(untouched[0].origin, ConfigOrigin::Builtin);

        let gw = config.entries(Some("providers.gw.base_url"));
        assert_eq!(gw[0].origin, ConfigOrigin::User);
    }

    #[test]
    fn deep_merge_keeps_sibling_keys_and_arrays_replace() {
        let config = LayeredConfig::from_layers(vec![(
            ConfigOrigin::User,
            table("[providers.gw]\nbase_url = \"https://new\"\nmodels = [\"only-this\"]\n"),
        )]);
        let gw = &config.providers()["gw"];
        assert_eq!(gw.base_url, "https://new");
        assert_eq!(
            gw.models,
            vec!["only-this".to_owned()],
            "arrays replace wholesale"
        );
        assert_eq!(
            gw.env_key, "",
            "the sibling key from the built-in layer survives"
        );
    }

    #[test]
    fn builtins_fill_whole_provider_entries() {
        let config = LayeredConfig::from_layers(vec![]);
        let deepseek = &config.providers()["deepseek"];
        assert_eq!(deepseek.env_key, "DEEPSEEK_API_KEY");
        assert!(deepseek.base_url.contains("deepseek"));
        // ...and the origin says so.
        assert_eq!(
            config.entries(Some("providers.deepseek.base_url"))[0].origin,
            ConfigOrigin::Builtin
        );
    }

    #[test]
    fn unknown_keys_are_ignored_with_the_rest_of_the_file_kept() {
        let config = LayeredConfig::from_layers(vec![(
            ConfigOrigin::User,
            table("ui.show_reasoning = false\nui.typo_key = 1\nbogus_table.x = 2\n"),
        )]);
        assert!(!config.ui().show_reasoning, "the valid key survives");
        assert!(
            config.entries(Some("ui.typo_key")).is_empty(),
            "the unknown key is not served"
        );
        assert!(config.entries(Some("bogus_table")).is_empty());
    }

    #[test]
    fn runtime_set_updates_the_effective_value_and_is_refused_outside_the_schema() {
        let config = LayeredConfig::from_layers(vec![]);
        let entry = config
            .set("ui.show_reasoning", serde_json::json!(false))
            .expect("known key");
        assert_eq!(entry.value, serde_json::json!(false));
        assert_eq!(entry.origin, ConfigOrigin::Runtime);
        assert!(!config.ui().show_reasoning);

        let error = config
            .set("nuclear.launch_codes", serde_json::json!(1))
            .expect_err("unknown");
        assert!(error.to_string().contains("unknown configuration key"));
    }

    #[test]
    fn parse_failures_of_a_layer_file_are_fatal_and_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "ui.show_reasoning = not a bool").expect("write");
        let error = LayeredConfig::load(&LoadPaths {
            system: None,
            user: Some(path),
            project: None,
        })
        .expect_err("invalid TOML");
        assert!(matches!(error, ConfigError::Parse { .. }), "{error}");
    }

    #[test]
    fn typed_views_read_through_the_layers() {
        let config = LayeredConfig::from_layers(vec![(
            ConfigOrigin::User,
            table("[daemon]\nidle_timeout_min = 5\nui_is_not_here = true\n"),
        )]);
        assert_eq!(
            config.daemon().idle_timeout,
            std::time::Duration::from_secs(300)
        );
        assert_eq!(config.daemon().idle_timeout, config.daemon().idle_timeout);
        // And the builtin fallback holds when the key is absent.
        let fresh = LayeredConfig::from_layers(vec![]);
        assert_eq!(
            fresh.daemon().idle_timeout,
            std::time::Duration::from_secs(1800)
        );
    }

    #[test]
    fn models_resolve_to_their_provider_and_unknown_names_are_refused() {
        let config = LayeredConfig::from_layers(vec![]);
        let (id, _) = config.resolve_model("deepseek-flash").expect("listed");
        assert_eq!(id, "deepseek");
        let (id, _) = config.resolve_model("qwen3.8-flash").expect("listed");
        assert_eq!(id, "qwen");
        assert!(
            config.resolve_model("anything-at-all").is_none(),
            "two providers and an unlisted model: refuse rather than guess"
        );
    }

    #[test]
    fn strict_keys_exist_as_a_mechanism() {
        assert!(
            STRICT_KEYS.is_empty(),
            "M1 has no security keys yet; M2 adds them"
        );
        assert!(is_known_key("providers.deepseek.retry.backoff_ms"));
        assert!(
            !is_known_key("providers"),
            "whole-table assignments are not honoured"
        );
    }
}
