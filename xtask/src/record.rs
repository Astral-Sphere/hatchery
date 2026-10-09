//! `record-fixtures`: probe real providers once, keep the bytes (docs/design/llm.md §7).
//!
//! The adapter's offline tests replay what this records, so the recorder's own rules are strict:
//!
//! * **Bytes, not reconstructions.** What lands in `<name>.sse` is the response body exactly as
//!   it arrived — framing, chunk order, whitespace — because a re-serialised "equivalent" would
//!   hide the provider quirks the fixtures exist to pin. This is why the recorder speaks HTTP
//!   itself: the product wire layer parses SSE on arrival and never sees raw bytes.
//! * **No secrets on disk, ever.** Every byte is scanned for the key that made the request and
//!   for key-shaped substrings; a hit aborts the write. The scan is a gate, not a scrubber —
//!   redacting would betray the byte-exactness rule above, so a leak means re-recording.
//! * **Attribution beside the bytes.** Each fixture gets a `.meta.json` sidecar naming the
//!   provider, model, request shape and time, so a future reader can tell a recording from a
//!   hand-made synthetic stream.
//!
//! Never runs in CI: it costs money and needs `DEEPSEEK_API_KEY` / `DASHSCOPE_API_KEY`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use serde_json::{Value, json};

/// The provider families M1 probes, with their endpoints and key variables.
struct Endpoint {
    id: &'static str,
    base_url: &'static str,
    env_key: &'static str,
}

const ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        id: "deepseek",
        base_url: "https://api.deepseek.com",
        env_key: "DEEPSEEK_API_KEY",
    },
    Endpoint {
        id: "qwen",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        env_key: "DASHSCOPE_API_KEY",
    },
];

/// One planned recording.
struct Probe {
    /// Fixture name, e.g. `deepseek-reasoning`.
    name: String,
    /// The model the probe addresses.
    model: &'static str,
    /// What the probe exercises, recorded in the sidecar.
    shape: &'static str,
    /// Extra request-body fields beyond the common streaming skeleton.
    extras: Value,
    /// Whether the probe is expected to stream (`true`) or to fail with an error body.
    expect_error: bool,
    /// A substring a successful recording must contain — the recorder fails loudly rather than
    /// keep a fixture that does not exercise what its name promises (the first tool-call probe
    /// came back as plain text: the model is free to decline, so the check is on the bytes).
    must_contain: Option<&'static str>,
}

/// Parses `record-fixtures [--provider deepseek|qwen|all] [--out DIR] [--force]` and records.
///
/// # Errors
///
/// Fails when a key variable is missing, a probe does not produce a usable stream, the
/// sanitizer finds anything key-shaped, or a fixture file already exists (pass `--force`).
pub fn run(args: &[String]) -> Result<String> {
    let mut provider = "all".to_owned();
    let mut out_dir = default_out_dir();
    let mut force = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--provider" => {
                provider = rest
                    .next()
                    .ok_or_else(|| anyhow!("--provider needs an argument"))?
                    .clone();
            }
            "--out" => {
                out_dir = PathBuf::from(
                    rest.next()
                        .ok_or_else(|| anyhow!("--out needs an argument"))?,
                );
            }
            "--force" => force = true,
            other => bail!("unknown record-fixtures argument {other:?}"),
        }
    }

    let endpoints: Vec<&Endpoint> = match provider.as_str() {
        "all" => ENDPOINTS.iter().collect(),
        id => ENDPOINTS
            .iter()
            .filter(|endpoint| endpoint.id == id)
            .collect(),
    };
    let Some(_) = endpoints.first() else {
        bail!("unknown provider {provider:?}; expected one of: all, deepseek, qwen");
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut reports = Vec::new();
        for endpoint in &endpoints {
            let api_key = std::env::var(endpoint.env_key)
                .ok()
                .filter(|key| !key.trim().is_empty())
                .ok_or_else(|| {
                    anyhow!(
                        "{} is not set; recording needs a real key",
                        endpoint.env_key
                    )
                })?;
            let probes = probes_for(endpoint.id);
            reports.push(record_all(endpoint, &api_key, &probes, &out_dir, force).await?);
        }
        Ok::<_, anyhow::Error>(reports.join("\n"))
    })
}

fn default_out_dir() -> PathBuf {
    // The recorder lives one level below the workspace root, as layering::check also assumes.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(Path::new("."))
        .join("crates/hatchery-llm/tests/fixtures")
}

/// The probe set for one provider: plain text, reasoning, a tool call, and an auth failure.
///
/// Both current models are hybrid — thinking on by default, a request switch to turn it off —
/// so every probe states its mode explicitly: DeepSeek via `thinking: {type: …}`, Qwen via
/// `enable_thinking` (2026-09-30 recalibration, worklog/llm.md).
fn probes_for(provider: &str) -> Vec<Probe> {
    let (text_model, reasoning_model, off_switch, on_switch) = match provider {
        "deepseek" => (
            "deepseek-flash",
            "deepseek-flash",
            json!({"thinking": {"type": "disabled"}}),
            json!({"thinking": {"type": "enabled"}}),
        ),
        _ => (
            "qwen3.8-flash",
            "qwen3.8-flash",
            json!({"enable_thinking": false}),
            json!({"enable_thinking": true}),
        ),
    };
    let probes = vec![
        Probe {
            name: format!("{provider}-text"),
            model: text_model,
            shape: "plain streaming text with a trailing usage-only chunk",
            extras: off_switch.clone(),
            expect_error: false,
            must_contain: None,
        },
        Probe {
            name: format!("{provider}-reasoning"),
            model: reasoning_model,
            shape: "reasoning deltas followed by the answer",
            extras: on_switch.clone(),
            expect_error: false,
            must_contain: Some("\"reasoning_content\":\""),
        },
        Probe {
            name: format!("{provider}-toolcall"),
            model: text_model,
            shape: "a function call streamed in fragments, finish_reason tool_calls",
            extras: {
                // Start from the off switch so the tool call is reasoning-free, then add the
                // tools and the forcing choice.
                let mut extras = off_switch.clone();
                let map = extras.as_object_mut().expect("the switch is an object");
                map.insert(
                    "tools".to_owned(),
                    json!([{
                        "type": "function",
                        "function": {
                            "name": "record_ping",
                            "description": "Echoes the value back; used only to record a tool-call stream.",
                            "parameters": {
                                "type": "object",
                                "properties": {"value": {"type": "string"}},
                                "required": ["value"]
                            }
                        }
                    }]),
                );
                // The first probe run (2026-09-30) came back as plain text: the model is free
                // to decline, and both did. A fixture probe is no place for persuasion.
                map.insert(
                    "tool_choice".to_owned(),
                    json!({"type": "function", "function": {"name": "record_ping"}}),
                );
                extras
            },
            expect_error: false,
            must_contain: Some("\"tool_calls\""),
        },
        Probe {
            name: format!("{provider}-401"),
            model: text_model,
            shape: "authentication error body",
            extras: off_switch.clone(),
            expect_error: true,
            must_contain: None,
        },
    ];
    probes
}

/// The request body every streaming probe shares.
fn stream_body(probe: &Probe) -> Value {
    let mut body = json!({
        "model": probe.model,
        "stream": true,
        "stream_options": {"include_usage": true},
        "messages": [
            {"role": "system", "content": "You are a fixture recording target. Follow the user's instruction exactly and keep answers under twenty words."},
            {"role": "user", "content": user_prompt(probe)},
        ],
    });
    if let (Some(target), Some(extras)) = (body.as_object_mut(), probe.extras.as_object()) {
        for (key, value) in extras {
            if !value.is_null() {
                target.insert(key.clone(), value.clone());
            }
        }
    }
    body
}

fn user_prompt(probe: &Probe) -> &'static str {
    if probe.shape.contains("tool call") {
        "Call the record_ping tool with value set to ok. Do not answer in words."
    } else if probe.shape.starts_with("reasoning") {
        "What is 2+2? Answer with just the number."
    } else {
        "Reply with exactly the word: hello"
    }
}

async fn record_all(
    endpoint: &Endpoint,
    api_key: &str,
    probes: &[Probe],
    out_dir: &Path,
    force: bool,
) -> Result<String> {
    ferritls_rustls::default_provider().install_default().ok();
    let client = reqwest::Client::new();

    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut written = Vec::new();

    for probe in probes {
        // The auth probe sends a deliberately wrong key; everything else the real one.
        let key = if probe.expect_error {
            "sk-deliberately-wrong-key"
        } else {
            api_key
        };
        let body = stream_body(probe);
        let (status, bytes) = capture(&client, endpoint, key, &body).await?;

        if probe.expect_error {
            anyhow::ensure!(
                status.as_u16() == 401 || status.as_u16() == 403,
                "{}: expected an auth failure, got HTTP {}",
                probe.name,
                status
            );
        } else {
            anyhow::ensure!(
                status.is_success(),
                "{}: HTTP {} — {}",
                probe.name,
                status,
                String::from_utf8_lossy(&bytes)
            );
            let text = String::from_utf8_lossy(&bytes);
            anyhow::ensure!(
                text.contains("data: ") && text.contains("[DONE]"),
                "{}: the response is not a complete SSE stream",
                probe.name
            );
            if let Some(required) = probe.must_contain {
                anyhow::ensure!(
                    text.contains(required),
                    "{}: the stream lacks {required:?} — the provider declined the shape this \
                     probe must record; tighten the request, not the check",
                    probe.name
                );
            }
        }

        scan_for_secrets(&bytes, api_key).with_context(|| probe.name.clone())?;
        let extension = if probe.expect_error { "json" } else { "sse" };
        let fixture = out_dir.join(format!("{}.{}", probe.name, extension));
        let sidecar = out_dir.join(format!("{}.meta.json", probe.name));
        if !force {
            anyhow::ensure!(
                !fixture.exists() && !sidecar.exists(),
                "{} exists; pass --force to re-record",
                fixture.display()
            );
        }
        std::fs::write(&fixture, &bytes)
            .with_context(|| format!("writing {}", fixture.display()))?;
        std::fs::write(&sidecar, provenance(endpoint, probe, status.as_u16()))
            .with_context(|| format!("writing {}", sidecar.display()))?;
        written.push(fixture);
    }

    Ok(format!(
        "recorded {} fixture(s) into {} (sanitizer passed)",
        written.len(),
        out_dir.display()
    ))
}

/// POSTs one probe and collects the whole response body byte for byte.
async fn capture(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    api_key: &str,
    body: &Value,
) -> Result<(reqwest::StatusCode, Vec<u8>)> {
    let response = client
        .post(format!("{}/chat/completions", endpoint.base_url))
        .bearer_auth(api_key)
        .header("accept", "text/event-stream")
        .json(body)
        .send()
        .await
        .with_context(|| format!("POSTing {}", endpoint.base_url))?;
    let status = response.status();
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk.with_context(|| "reading the stream")?);
    }
    Ok((status, bytes))
}

/// Refuses anything that looks like a live credential.
///
/// # Errors
///
/// Returns the offending snippet's description when the bytes contain the recording key itself
/// or any `sk-…` run long enough to be a real key.
fn scan_for_secrets(bytes: &[u8], api_key: &str) -> Result<()> {
    let text = String::from_utf8_lossy(bytes);
    if !api_key.is_empty() && text.contains(api_key) {
        bail!("the response contains the API key itself; refusing to write");
    }
    if let Some(found) = find_keyish(&text) {
        bail!(
            "the response contains a key-shaped string (`{}`…); refusing to write",
            &found[..found.len().min(12)]
        );
    }
    Ok(())
}

/// Finds a run of `sk-` followed by at least sixteen word characters.
fn find_keyish(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.';
    let mut i = 0;
    while let Some(at) = text[i..].find("sk-") {
        let start = i + at;
        let mut end = start + 3;
        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }
        if end - start >= 19 {
            return Some(&text[start..end]);
        }
        i = start + 3;
    }
    None
}

/// The sidecar that travels beside every recorded fixture.
fn provenance(endpoint: &Endpoint, probe: &Probe, status: u16) -> String {
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let sidecar = json!({
        "provenance": "recorded",
        "provider": endpoint.id,
        "model": probe.model,
        "shape": probe.shape,
        "wire": "chat-completions",
        "http_status": status,
        "recorded_at_unix": unix,
        "recorded_at_utc": utc_iso8601(unix),
        "sanitizer": "exact-key and sk-pattern scan passed",
        "recorder": "cargo xtask record-fixtures",
    });
    serde_json::to_string_pretty(&sidecar).expect("the sidecar is plain data")
}

/// Unix seconds to `YYYY-MM-DDTHH:MM:SSZ`, the civil-from-days algorithm (Howard Hinnant's).
fn utc_iso8601(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sanitizer_refuses_the_exact_key_and_key_shapes() {
        assert!(scan_for_secrets(b"hello world", "sk-live-abc").is_ok());

        let leaked = scan_for_secrets(b"Bearer sk-live-abc123", "sk-live-abc");
        assert!(
            leaked.is_err(),
            "the recording key itself must abort the write"
        );

        let shaped = scan_for_secrets(b"\"key\": \"sk-abcdefgh12345678\"", "sk-other");
        assert!(shaped.is_err(), "any key-shaped run must abort the write");

        // Short `sk-` runs are everywhere in prose; sixteen characters is the threshold.
        assert!(scan_for_secrets(b"task-123 sk-small", "sk-other").is_ok());
    }

    #[test]
    fn key_hunting_survives_adjacent_runs() {
        assert_eq!(
            find_keyish("xx sk-abcdefgh12345678 yy"),
            Some("sk-abcdefgh12345678")
        );
        assert_eq!(find_keyish("no keys here"), None);
        assert_eq!(
            find_keyish("sk-abc sk-abcdefgh12345678"),
            Some("sk-abcdefgh12345678")
        );
    }

    #[test]
    fn the_sidecar_is_attributed_and_dated() {
        let endpoint = &ENDPOINTS[0];
        let probe = Probe {
            name: "deepseek-text".to_owned(),
            model: "deepseek-chat",
            shape: "plain",
            extras: Value::Null,
            expect_error: false,
            must_contain: None,
        };
        let sidecar: Value =
            serde_json::from_str(&provenance(endpoint, &probe, 200)).expect("JSON");
        assert_eq!(sidecar["provenance"], "recorded");
        assert_eq!(sidecar["provider"], "deepseek");
        assert_eq!(sidecar["http_status"], 200);
        assert!(
            sidecar["recorded_at_utc"]
                .as_str()
                .is_some_and(|d| d.ends_with('Z'))
        );
    }

    #[test]
    fn unix_seconds_render_as_utc() {
        assert_eq!(utc_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_iso8601(1_759_276_800), "2025-10-01T00:00:00Z");
    }

    #[test]
    fn deepseek_probes_carry_no_thinking_field_and_qwen_do() {
        let deepseek = probes_for("deepseek");
        assert!(
            deepseek.iter().all(|probe| {
                let body = stream_body(probe);
                body.get("enable_thinking").is_none()
            }),
            "deepseek switches thinking by model; a field would be rejected"
        );

        let qwen = probes_for("qwen");
        let reasoning = qwen
            .iter()
            .find(|p| p.shape.starts_with("reasoning"))
            .unwrap();
        assert_eq!(stream_body(reasoning)["enable_thinking"], true);
    }

    #[test]
    fn every_stream_probe_asks_for_usage_and_streaming() {
        let probe = Probe {
            name: "x".to_owned(),
            model: "m",
            shape: "plain streaming text",
            extras: json!({"enable_thinking": false}),
            expect_error: false,
            must_contain: None,
        };
        let body = stream_body(&probe);
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }
}
