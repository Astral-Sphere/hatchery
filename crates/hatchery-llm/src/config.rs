//! How one provider endpoint is reached and tuned (docs/design/llm.md §2).
//!
//! The daemon loads these from `[providers.<id>]` in the layered config; this crate only defines
//! the shape and consumes it. API keys are never part of the config: a provider names the
//! environment variable that holds its key (`env_key`), and the key is read at request time so a
//! key that appears after startup still works and a config file can never leak one.

use hatchery_protocol::ReasoningEffort;
use serde::{Deserialize, Serialize};

use crate::capability::ModelCapabilities;

/// Which wire protocol an endpoint speaks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireApi {
    /// OpenAI Chat Completions — the M1 wire.
    #[default]
    ChatCompletions,
    /// OpenAI Responses — the second wire (docs/design/llm.md §5), later milestone.
    Responses,
}

/// How hard to retry a request that failed to start (docs/design/llm.md §6).
///
/// Applies only before the first event of a round reaches the kernel: a stream that dies midway
/// is reported, not silently restarted, because half a round the user saw has already been
/// committed and replaying it would duplicate the items.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetryPolicy {
    /// Total attempts per round, counting the first one. `1` means no retry.
    pub max_attempts: u32,
    /// Backoff of the first retry. Doubles per attempt up to `max_backoff_ms`.
    pub backoff_ms: u64,
    /// The ceiling the doubling never crosses.
    pub max_backoff_ms: u64,
    /// Jitter as a fraction of the backoff, in per-cent (0–100). 0 disables jitter.
    pub jitter_percent: u8,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            backoff_ms: 500,
            max_backoff_ms: 30_000,
            jitter_percent: 20,
        }
    }
}

impl RetryPolicy {
    /// The delay before attempt `attempt` (1-based, so `attempt = 2` is the first retry).
    ///
    /// Deterministic apart from the jitter, which is seeded per process from the wall clock —
    /// enough to keep a fleet of clients from stampeding in lockstep.
    #[must_use]
    pub fn backoff_for(&self, attempt: u32) -> std::time::Duration {
        let exp = attempt.saturating_sub(2);
        let base = self
            .backoff_ms
            .saturating_mul(1_u64 << exp.min(16))
            .min(self.max_backoff_ms);
        let jitter = jitter_nanos();
        let spread = (u128::from(base) * u128::from(self.jitter_percent) / 100) as u64;
        let offset = if spread == 0 {
            0
        } else {
            (jitter % spread * 2).saturating_sub(spread) as i64
        };
        let ms = (base as i64 + offset).max(0) as u64;
        std::time::Duration::from_millis(ms)
    }
}

/// A tiny per-process PRNG seed, advanced on every call: SplitMix64 over the wall clock.
fn jitter_nanos() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut seed = STATE.load(Ordering::Relaxed);
    if seed == 0 {
        seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64)
            | 1;
    }
    seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    STATE.store(seed, Ordering::Relaxed);
    let mut z = seed;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Reasoning defaults for a provider (docs/design/llm.md §2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReasoningConfig {
    /// The effort a session starts with when it does not pick one.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Whether the frontend shows reasoning by default. Affects display only, never passback.
    pub show_reasoning: bool,
}

/// One provider endpoint (docs/design/llm.md §2).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// Base URL of the OpenAI-compatible API, no `/chat/completions` suffix.
    pub base_url: String,
    /// The environment variable that holds the API key. The key is read from it at request time.
    pub env_key: String,
    /// Which wire this endpoint speaks.
    pub wire: WireApi,
    /// Extra headers sent verbatim, e.g. gateways' `X-Custom`.
    pub http_headers: std::collections::BTreeMap<String, String>,
    /// Retry behaviour for requests that fail to start.
    pub retry: RetryPolicy,
    /// Reasoning defaults.
    pub reasoning: ReasoningConfig,
    /// The models this provider serves, for `/model` and provider resolution by model name.
    pub models: Vec<String>,
    /// Per-model capability overrides; the built-in table covers everything not listed.
    pub capabilities: Vec<(String, ModelCapabilities)>,
}

impl ProviderConfig {
    /// A config with only the two facts every provider must state.
    #[must_use]
    pub fn new(base_url: impl Into<String>, env_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            env_key: env_key.into(),
            ..Self::default()
        }
    }

    /// The built-in defaults a config file may omit.
    #[must_use]
    pub fn builtin(id: &str) -> Self {
        match id {
            "deepseek" => Self::new("https://api.deepseek.com", "DEEPSEEK_API_KEY")
                .with_models(["deepseek-flash"])
                .with_effort(ReasoningEffort::High),
            "qwen" => Self::new(
                "https://dashscope.aliyuncs.com/compatible-mode/v1",
                "DASHSCOPE_API_KEY",
            )
            .with_models(["qwen3.8-flash"])
            .with_effort(ReasoningEffort::High),
            _ => Self::default(),
        }
    }

    /// States the models this provider serves.
    #[must_use]
    pub fn with_models(mut self, models: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.models = models.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the default effort.
    #[must_use]
    pub fn with_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning.reasoning_effort = Some(effort);
        self
    }

    /// Reads the API key from the configured environment variable.
    ///
    /// # Errors
    ///
    /// Fails when the variable is unset or empty — worth failing before the request, because the
    /// provider would only repeat the news as a 401.
    pub fn api_key(&self) -> Result<String, String> {
        resolve_key(std::env::var(&self.env_key).ok(), &self.env_key)
    }
}

/// The interesting half of [`ProviderConfig::api_key`], apart from the environment lookup so it
/// needs no process-global state to test.
fn resolve_key(found: Option<String>, env_key: &str) -> Result<String, String> {
    found
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| format!("environment variable `{env_key}` is not set"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let policy = RetryPolicy {
            max_attempts: 5,
            backoff_ms: 500,
            max_backoff_ms: 2_000,
            jitter_percent: 0,
        };
        assert_eq!(policy.backoff_for(2), std::time::Duration::from_millis(500));
        assert_eq!(
            policy.backoff_for(3),
            std::time::Duration::from_millis(1_000)
        );
        assert_eq!(
            policy.backoff_for(4),
            std::time::Duration::from_millis(2_000)
        );
        assert_eq!(
            policy.backoff_for(9),
            std::time::Duration::from_millis(2_000),
            "the ceiling holds however deep the exponent goes"
        );
        assert_eq!(
            policy.backoff_for(1),
            std::time::Duration::from_millis(500),
            "attempt 1 is not a retry, but its delay is still well-defined"
        );
    }

    #[test]
    fn jitter_stays_inside_the_band() {
        let policy = RetryPolicy {
            jitter_percent: 20,
            ..RetryPolicy::default()
        };
        for attempt in 2..6_u32 {
            let delay = policy.backoff_for(attempt).as_millis();
            let base = policy.backoff_ms.saturating_mul(1 << (attempt - 2).min(4));
            assert!(
                delay >= u128::from(base) * 8 / 10 && delay <= u128::from(base) * 12 / 10,
                "attempt {attempt}: {delay} ms is outside ±20% of {base} ms"
            );
        }
    }

    #[test]
    fn the_key_is_accepted_present_and_rejected_absent() {
        assert_eq!(
            resolve_key(Some(" sk-live-1 ".to_owned()), "K").expect("set"),
            " sk-live-1 "
        );

        let error = resolve_key(None, "DEEPSEEK_API_KEY").expect_err("unset");
        assert!(error.contains("DEEPSEEK_API_KEY"), "{error}");

        let blank = resolve_key(Some("   ".to_owned()), "K").expect_err("blank");
        assert!(blank.contains('K'), "{blank}");
    }

    #[test]
    fn builtin_providers_name_their_endpoints_and_models() {
        let deepseek = ProviderConfig::builtin("deepseek");
        assert_eq!(deepseek.env_key, "DEEPSEEK_API_KEY");
        assert_eq!(deepseek.models, vec!["deepseek-flash".to_owned()]);
        assert_eq!(
            deepseek.reasoning.reasoning_effort,
            Some(ReasoningEffort::High)
        );

        let qwen = ProviderConfig::builtin("qwen");
        assert_eq!(qwen.env_key, "DASHSCOPE_API_KEY");
        assert!(qwen.base_url.contains("compatible-mode"));
        assert_eq!(qwen.models, vec!["qwen3.8-flash".to_owned()]);
    }
}
