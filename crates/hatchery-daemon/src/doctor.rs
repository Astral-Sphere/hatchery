//! Diagnostics: what the environment says, and what a provider *actually* does.
//!
//! Two halves. The environment checks name what is missing before startup (the audit's
//! friendlier sibling: a report, not a refusal). The provider probe (llm.md §7) sends one
//! minimal real request and reports measured behaviour — reasoning deltas seen or not, finish
//! reason, usage — so the capability table is calibrated against reality, not hope.

use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use hatchery_kernel::{ChatOptions, LlmProvider, Message, StreamEvent};
use hatchery_llm::{ProviderConfig, ProviderRegistry};
use hatchery_protocol::Usage;

use crate::config::LayeredConfig;
use crate::discover::StateDir;

/// One probe round may not exceed this; a hung endpoint is a finding, not a lifestyle.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// A nameable, pass/fail environment fact.
#[derive(Clone, Debug)]
pub struct Check {
    /// What was checked, phrased as the fact ("provider `deepseek` env key").
    pub name: String,
    /// Whether it held.
    pub ok: bool,
    /// What was found; a refusal message when `ok` is false.
    pub detail: String,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mark = if self.ok { "ok" } else { "MISSING" };
        write!(f, "{mark:7} {} — {}", self.name, self.detail)
    }
}

/// The environment half of `doctor`: everything that must hold before a daemon can serve.
#[must_use]
pub fn environment_checks(config: &LayeredConfig, state: &StateDir, data_dir: &Path) -> Vec<Check> {
    let mut checks = Vec::new();
    let providers = config.providers();
    checks.push(Check {
        name: "providers configured".to_owned(),
        ok: !providers.is_empty(),
        detail: format!(
            "{} provider(s) in the effective configuration",
            providers.len()
        ),
    });
    for (id, provider) in &providers {
        if provider.env_key.is_empty() {
            checks.push(Check {
                name: format!("provider `{id}` env key"),
                ok: true,
                detail: "none required (env_key empty)".to_owned(),
            });
            continue;
        }
        let present = std::env::var(&provider.env_key).is_ok();
        checks.push(Check {
            name: format!("provider `{id}` env key"),
            ok: present,
            detail: if present {
                format!("`{}` is set", provider.env_key)
            } else {
                format!(
                    "`{}` is not set; the daemon will refuse to serve",
                    provider.env_key
                )
            },
        });
    }
    checks.push(dir_check(
        "state directory",
        &state
            .lock_path()
            .parent()
            .map_or_else(|| state.lock_path().clone(), Path::to_path_buf),
    ));
    checks.push(dir_check("data directory", data_dir));
    checks
}

fn dir_check(name: &str, path: &Path) -> Check {
    // A real write, not just a stat: a read-only directory passes `create_dir_all` + `metadata`
    // and would have been reported "writable" without ever being so.
    let ok = std::fs::create_dir_all(path).is_ok() && {
        let probe = path.join(".hatchery-doctor-probe");
        let written = std::fs::write(&probe, b"probe").is_ok();
        let _ = std::fs::remove_file(&probe);
        written
    };
    Check {
        name: name.to_owned(),
        ok,
        detail: format!(
            "{} at {}",
            if ok { "writable" } else { "unavailable" },
            path.display()
        ),
    }
}

/// What one real request round revealed.
#[derive(Clone, Debug)]
pub struct RoundReport {
    /// Which round this was: `default` (no effort asked) or `off` (the switch flipped).
    pub label: String,
    /// Whether the round completed without an error event.
    pub ok: bool,
    /// The first failure, when there was one.
    pub error: Option<String>,
    /// The assistant text received.
    pub content: String,
    /// Characters of reasoning received (byte-exact discipline makes chars the honest unit).
    pub reasoning_chars: usize,
    /// What the provider said stopped the round.
    pub finish_reason: Option<String>,
    /// Token accounting, when the provider reported any.
    pub usage: Usage,
    /// How many `RateLimited` events the adapter emitted (retries it made).
    pub rate_limited_rounds: u32,
    /// Wall-clock time of the round.
    pub latency_ms: u64,
}

impl fmt::Display for RoundReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ok {
            writeln!(f, "  outcome:  completed in {} ms", self.latency_ms)?;
        } else {
            writeln!(
                f,
                "  outcome:  FAILED in {} ms — {}",
                self.latency_ms,
                self.error.as_deref().unwrap_or("unspecified")
            )?;
        }
        if self.rate_limited_rounds > 0 {
            writeln!(
                f,
                "  retries:  {} rate-limited round(s) survived",
                self.rate_limited_rounds
            )?;
        }
        writeln!(
            f,
            "  finish:   {}",
            self.finish_reason.as_deref().unwrap_or("(none)")
        )?;
        writeln!(
            f,
            "  reasoning: {} character(s) received",
            self.reasoning_chars
        )?;
        if self.usage.is_empty() {
            writeln!(f, "  usage:    not reported")?;
        } else {
            writeln!(
                f,
                "  usage:    prompt={:?} completion={:?} reasoning={:?}",
                self.usage.prompt_tokens, self.usage.completion_tokens, self.usage.reasoning_tokens
            )?;
        }
        write!(f, "  content:  {:?}", self.content)
    }
}

/// What the probe measured, one round per reasoning mode.
#[derive(Clone, Debug)]
pub struct ProbeReport {
    /// The provider id probed.
    pub provider: String,
    /// The model asked for.
    pub model: String,
    /// The rounds in the order they ran: `default`, then `off`.
    pub rounds: Vec<RoundReport>,
}

impl ProbeReport {
    /// True when every round succeeded.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.rounds.iter().all(|round| round.ok)
    }
}

impl fmt::Display for ProbeReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "provider `{}` model `{}`", self.provider, self.model)?;
        for round in &self.rounds {
            writeln!(f, "  round {}:", round.label)?;
            write!(f, "{round}")?;
            if round.reasoning_chars > 0 {
                writeln!(f)?;
                writeln!(f, "            ^ reasoning arrived by default")?;
            } else if round.label == "off" && round.ok {
                writeln!(f)?;
                writeln!(f, "            ^ reasoning suppressed as asked")?;
            }
        }
        Ok(())
    }
}

/// Sends one minimal request per reasoning mode and measures what comes back.
///
/// Two rounds, because the current generation of models are hybrid (reasoning on by
/// default, a request switch to turn it off): the `default` round shows what a plain turn
/// gets, the `off` round shows whether the capability table's off switch actually
/// suppresses it. Both are real [`LlmProvider`] rounds — retries, capability table and
/// translation behave exactly as a daemon turn would; that is the point. Offline tests pin
/// the probe to a mock wire; it is never run against real endpoints by tests.
pub async fn probe_provider(
    provider_id: &str,
    config: &ProviderConfig,
    model: &str,
) -> ProbeReport {
    hatchery_llm::install_tls_provider();
    let registry = ProviderRegistry::new();
    let _registration = registry.register(provider_id.to_owned(), config.clone());
    let Some(provider): Option<Arc<hatchery_llm::ChatCompletionsProvider>> =
        registry.resolve_model(model)
    else {
        return ProbeReport {
            provider: provider_id.to_owned(),
            model: model.to_owned(),
            rounds: vec![RoundReport {
                label: "default".to_owned(),
                ok: false,
                error: Some(format!(
                    "provider `{provider_id}` is not registered for model `{model}`"
                )),
                content: String::new(),
                reasoning_chars: 0,
                finish_reason: None,
                usage: Usage::default(),
                rate_limited_rounds: 0,
                latency_ms: 0,
            }],
        };
    };

    let mut rounds = Vec::new();
    for (label, effort) in [
        ("default", None),
        ("off", Some(hatchery_protocol::ReasoningEffort::Off)),
    ] {
        rounds.push(probe_round(&provider, model, label, effort).await);
    }
    ProbeReport {
        provider: provider_id.to_owned(),
        model: model.to_owned(),
        rounds,
    }
}

async fn probe_round(
    provider: &hatchery_llm::ChatCompletionsProvider,
    model: &str,
    label: &str,
    effort: Option<hatchery_protocol::ReasoningEffort>,
) -> RoundReport {
    let started = std::time::Instant::now();
    let options = ChatOptions {
        reasoning_effort: effort,
        max_output_tokens: Some(64),
        ..ChatOptions::new(model.to_owned())
    };
    let messages = vec![Message::user("Reply with exactly: ok")];
    let token = CancellationToken::new();
    let mut report = RoundReport {
        label: label.to_owned(),
        ok: true,
        error: None,
        content: String::new(),
        reasoning_chars: 0,
        finish_reason: None,
        usage: Usage::default(),
        rate_limited_rounds: 0,
        latency_ms: 0,
    };

    let stream = provider.chat_stream(&options, &messages, token).await;
    let mut stream = match stream {
        Ok(stream) => stream,
        Err(error) => {
            report.ok = false;
            report.error = Some(error.to_string());
            report.latency_ms = started.elapsed().as_millis() as u64;
            return report;
        }
    };

    loop {
        let event =
            tokio::time::timeout(PROBE_TIMEOUT, futures::stream::StreamExt::next(&mut stream))
                .await;
        let timed_out = event.is_err();
        let Some(event) = event.unwrap_or(None) else {
            // `Err` is the timeout; `Ok(None)` is the stream ending before any terminal event —
            // both are findings, and the honest report says which.
            if report.ok {
                report.ok = false;
                report.error = Some(if timed_out {
                    format!("the probe gave up after {PROBE_TIMEOUT:?} without a terminal event")
                } else {
                    "the stream ended without a terminal event".to_owned()
                });
            }
            break;
        };
        match event {
            StreamEvent::TextDelta { text } => report.content.push_str(&text),
            StreamEvent::ReasoningDelta { text } => report.reasoning_chars += text.chars().count(),
            StreamEvent::ReasoningDone { .. } => {}
            StreamEvent::ToolCall { .. } => {}
            StreamEvent::Usage { usage } => report.usage.merge(&usage),
            StreamEvent::RateLimited { .. } => report.rate_limited_rounds += 1,
            StreamEvent::Done { finish_reason } => {
                report.finish_reason = Some(format!("{finish_reason:?}"));
                break;
            }
            StreamEvent::Error { error } => {
                report.ok = false;
                report.error = Some(error.to_string());
                break;
            }
        }
    }
    report.latency_ms = started.elapsed().as_millis() as u64;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_llm::{ModelCapabilities, ReasoningWire};

    /// A reasoning-free stream (what an off round must produce).
    const SSE_OK: &str = concat!(
        "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}],\"created\":1718345013,\"model\":\"mock-model\",\"object\":\"chat.completion.chunk\",\"usage\":null}\n\n",
        "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"created\":1718345013,\"model\":\"mock-model\",\"object\":\"chat.completion.chunk\"}\n\n",
        "data: {\"choices\":[],\"created\":1718345013,\"id\":\"1\",\"model\":\"mock-model\",\"object\":\"chat.completion.chunk\",\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}}\n\n",
        "data: [DONE]\n\n",
    );

    /// The same shape with a reasoning delta first — what a hybrid model's default round
    /// produces when thinking is on.
    const SSE_REASONING_OK: &str = concat!(
        "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"pondering...\"},\"finish_reason\":null}],\"created\":1718345013,\"model\":\"mock-model\",\"object\":\"chat.completion.chunk\",\"usage\":null}\n\n",
        "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}],\"created\":1718345013,\"model\":\"mock-model\",\"object\":\"chat.completion.chunk\",\"usage\":null}\n\n",
        "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"created\":1718345013,\"model\":\"mock-model\",\"object\":\"chat.completion.chunk\"}\n\n",
        "data: {\"choices\":[],\"created\":1718345013,\"id\":\"1\",\"model\":\"mock-model\",\"object\":\"chat.completion.chunk\",\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,\"total_tokens\":10}}\n\n",
        "data: [DONE]\n\n",
    );

    #[tokio::test]
    async fn the_probe_runs_a_default_round_and_an_off_round() {
        // The switch is visible in the request body, so the mock routes on it: rounds asking
        // for `disabled` get a reasoning-free stream, the rest get reasoning plus content.
        let wire = hatchery_testkit::wire::MockWire::sse_switched(
            "\"type\":\"disabled\"",
            SSE_OK.to_owned(),
            SSE_REASONING_OK.to_owned(),
        )
        .await;
        // `PATH` exists in every environment; the mock does not validate the key's value.
        // The model under test is pinned to the thinking-switch row, so the off round speaks
        // DeepSeek's `thinking` object rather than the generic effort ladder.
        let mut config = ProviderConfig::new(wire.url(), "PATH");
        config.capabilities.push((
            "mock-model".to_owned(),
            ModelCapabilities {
                reasoning: ReasoningWire::ThinkingSwitch,
                ..ModelCapabilities::default()
            },
        ));
        let report = probe_provider("mock", &config, "mock-model").await;
        assert!(report.ok(), "{report}");
        assert_eq!(report.rounds.len(), 2, "{report}");
        assert_eq!(report.rounds[0].label, "default");
        assert_eq!(report.rounds[1].label, "off");

        let default = &report.rounds[0];
        assert_eq!(default.finish_reason.as_deref(), Some("Stop"), "{report}");
        assert_eq!(default.usage.prompt_tokens, Some(7), "{report}");
        assert_eq!(default.rate_limited_rounds, 0);

        let off = &report.rounds[1];
        assert_eq!(off.finish_reason.as_deref(), Some("Stop"), "{report}");

        // Both rounds really went out, and the second carried the off switch.
        let requests = wire.requests().await;
        assert_eq!(requests.len(), 2, "one request per round");
        assert!(
            !requests[0].body.contains("disabled"),
            "the default round does not flip the switch: {:?}",
            requests[0].body
        );
        assert!(
            requests[1].body.contains("\"type\":\"disabled\""),
            "the off round asks for non-thinking: {:?}",
            requests[1].body
        );
    }

    #[tokio::test]
    async fn the_probe_reports_auth_failures_honestly() {
        let body =
            "{\"error\":{\"message\":\"Invalid API key\",\"type\":\"invalid_request_error\"}}";
        let wire = hatchery_testkit::wire::MockWire::refuse_always(401, body).await;
        // `PATH` exists, so the request goes out and the 401 comes back; a missing variable
        // would be refused before the wire (that path is the adapter's own test).
        let config = ProviderConfig::new(wire.url(), "PATH");
        let report = probe_provider("mock", &config, "mock-model").await;
        assert!(!report.ok(), "{report}");
        for round in &report.rounds {
            assert!(
                round.error.as_deref().is_some_and(
                    |error| error.contains("401") || error.to_lowercase().contains("auth")
                ),
                "the error should name the failure: {report}"
            );
        }
    }

    #[tokio::test]
    async fn environment_checks_name_unset_env_keys() {
        let config = LayeredConfig::from_layers(vec![(
            hatchery_protocol::method::ConfigOrigin::User,
            toml::from_str(
                "[providers.ghost]\nbase_url = \"http://127.0.0.1:9\"\nenv_key = \"HATCHERY_TEST_DEFINITELY_MISSING_KEY\"\nmodels = [\"m\"]\n",
            )
            .expect("toml"),
        )]);
        let dir = tempfile::tempdir().expect("tempdir");
        let state = StateDir::at(dir.path().join("state"));
        let checks = environment_checks(&config, &state, &dir.path().join("data"));
        let ghost = checks
            .iter()
            .find(|check| check.name.contains("ghost"))
            .expect("the ghost provider is reported");
        assert!(!ghost.ok, "{ghost}");
        assert!(
            ghost
                .detail
                .contains("HATCHERY_TEST_DEFINITELY_MISSING_KEY")
        );
        assert!(
            checks
                .iter()
                .any(|check| check.name.contains("providers configured") && check.ok)
        );
    }
}
