//! SSE fixture loading and the wire double that replays them (docs/design/testing.md §2).
//!
//! A fixture is a pair of files under the *consumer crate's* `tests/fixtures/`:
//!
//! * `<name>.sse` — the bytes the provider sent, exactly as they arrived: the framing (`data:`
//!   lines, blank-line separators, the `[DONE]` sentinel), the chunk order, the whitespace.
//!   Byte-exactness is the point — a re-serialised "equivalent" stream would hide the very
//!   provider quirks the fixtures exist to pin.
//! * `<name>.meta.json` — provenance: which provider, which model, when it was recorded, and
//!   whether the bytes were recorded from the real endpoint or synthesised for a test shape.
//!   Loading a fixture without its sidecar is refused, because an unattributed recording is a
//!   liability.

use std::path::PathBuf;

use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A local wiremock server standing in for one provider endpoint.
///
/// Assembled here so every HTTP-level test replays bytes the same way; the mock records what it
/// was sent, which is how request-body goldens are asserted.
pub struct MockWire {
    /// The running server; its `uri()` is the provider's base URL for the test.
    pub server: MockServer,
}

impl MockWire {
    /// A wire that answers every POST with these SSE bytes, status 200.
    pub async fn replay_sse(body: impl Into<String>) -> Self {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body.into()),
            )
            .mount(&server)
            .await;
        Self { server }
    }

    /// [`Self::replay_sse`], but the answer takes `delay` to arrive.
    ///
    /// This is how a test holds a turn in flight deterministically: the request is real and the
    /// turn is genuinely waiting on the provider, so a second prompt must be refused while the
    /// clock runs — no sleeping on the test's side required.
    pub async fn replay_sse_after(body: impl Into<String>, delay: std::time::Duration) -> Self {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body.into())
                    .set_delay(delay),
            )
            .mount(&server)
            .await;
        Self { server }
    }

    /// A wire that answers the first `n` POSTs with `status` and `body`, then streams `then_sse`.
    ///
    /// This is the shape of a rate-limited start: refusals, then service.
    pub async fn refuse_then_sse(status: u16, body: &str, n: usize, then_sse: &str) -> Self {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body.to_owned()))
            .up_to_n_times(n.try_into().expect("a test-sized count fits u64"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(then_sse.to_owned()),
            )
            .mount(&server)
            .await;
        Self { server }
    }

    /// A wire that answers every POST with `status` and `body`, never streaming.
    pub async fn refuse_always(status: u16, body: &str) -> Self {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body.to_owned()))
            .mount(&server)
            .await;
        Self { server }
    }

    /// Routes by request body: bodies containing `on_marker` get `on_body`, the rest get
    /// `off_body`. This is the shape of a thinking-switch probe — one endpoint, two rounds,
    /// the request itself says which mode was asked for.
    pub async fn sse_switched(
        on_marker: &str,
        on_body: impl Into<String>,
        off_body: impl Into<String>,
    ) -> Self {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::body_string_contains(on_marker))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(on_body.into()),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(off_body.into()),
            )
            .mount(&server)
            .await;
        Self { server }
    }

    /// The base URL a provider config should point at.
    #[must_use]
    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// Every recorded request, oldest first.
    ///
    /// Headers are lowercase names with string values — exactly what a golden assertion wants
    /// to spell, and `authorization` rides along like any other header.
    pub async fn requests(&self) -> Vec<RecordedWireRequest> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|request| RecordedWireRequest {
                path: request.url.path().to_owned(),
                body: String::from_utf8_lossy(&request.body).into_owned(),
                headers: request
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_owned(),
                            String::from_utf8_lossy(value.as_bytes()).into_owned(),
                        )
                    })
                    .collect(),
            })
            .collect()
    }
}

/// One request the wire saw, flattened for assertions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedWireRequest {
    /// The path the POST went to.
    pub path: String,
    /// The JSON body, decoded lossily — it is UTF-8 by construction.
    pub body: String,
    /// Header names (lowercase) and values, in wire order.
    pub headers: Vec<(String, String)>,
}

impl RecordedWireRequest {
    /// One header's value, if the request carried it.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Reads `tests/fixtures/<name>.sse` from the crate whose test is running.
///
/// Integration tests are the consumers, and Cargo sets `CARGO_MANIFEST_DIR` to the crate under
/// test there, so the fixture always resolves relative to the test's own crate — which is also
/// where `cargo xtask record-fixtures` writes.
///
/// # Panics
///
/// Panics when the fixture or its provenance sidecar is missing: a test naming a fixture that
/// does not exist is a typo, and half a test is worse than a clear failure.
pub fn sse_fixture(name: &str) -> Vec<u8> {
    bytes_fixture(name, "sse")
}

/// Reads `tests/fixtures/<name>.json` — an error body or other non-stream fixture.
///
/// # Panics
///
/// Same contract as [`sse_fixture`]: the sidecar must exist too.
pub fn json_fixture(name: &str) -> Vec<u8> {
    bytes_fixture(name, "json")
}

fn bytes_fixture(name: &str, extension: &str) -> Vec<u8> {
    let dir = fixture_dir();
    let sidecar = dir.join(format!("{name}.meta.json"));
    assert!(
        sidecar.is_file(),
        "fixture `{name}` has no provenance sidecar at {}",
        sidecar.display()
    );
    std::fs::read(dir.join(format!("{name}.{extension}"))).unwrap_or_else(|error| {
        panic!(
            "fixture `{name}.{extension}` could not be read from {}: {error}",
            dir.display()
        )
    })
}

fn fixture_dir() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .expect("integration tests run with CARGO_MANIFEST_DIR set");
    PathBuf::from(manifest).join("tests").join("fixtures")
}
