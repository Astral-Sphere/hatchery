//! `web_fetch`: one URL, converted to markdown, bounded.
//!
//! D17 chose `reqwest` (already in the lock through the LLM adapter) plus `htmd` for the HTML to
//! markdown step. Two properties of this tool are deliberate:
//!
//! * it asks on **every** call. The approval rule table's matcher is a tool name (D8), so an
//!   `AllowAlways` here would silently grant every future domain; the design's "persistable
//!   domain rule" needs a finer matcher than the table has. A one-shot approval is the honest
//!   version until that exists.
//! * the scheme is checked before anything is sent. `file:`, `data:` and friends are not part of
//!   what a web fetcher does, and refusing them here means the approval prompt never offers one.
//!
//! It is **not** a browser: no JavaScript, no cookies persisted, no charset conversion beyond
//! UTF-8. Pages that need those come back as their raw text.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use hatchery_capabilities::{Tool, ToolCtx, ToolError};
use hatchery_kernel::ToolDef;
use hatchery_protocol::{ApprovalPreview, ApprovalRequest, RiskLevel, ToolCallSummary, ToolOutput};

/// How much of a response body is downloaded before the rest is dropped.
pub const MAX_DOWNLOAD_BYTES: usize = 5 * 1024 * 1024;
/// How long one fetch may take when the model does not say.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// The ceiling a model may ask for.
pub const MAX_TIMEOUT_SECS: u64 = 120;
const USER_AGENT: &str = concat!("hatchery/", env!("CARGO_PKG_VERSION"));

/// The fetch-one-page tool.
pub struct WebFetch {
    client: reqwest::Client,
}

impl WebFetch {
    /// The tool with its HTTP client.
    #[must_use]
    pub fn new() -> Self {
        // reqwest is built with `rustls-no-provider`, so constructing a client asserts that a
        // process-wide provider exists — even for an `http://` URL. The daemon installs the
        // project's ferritls backend at startup; a tools-only test has not, so install the same
        // provider here. First install wins, so this is a no-op inside the daemon.
        let _ = ferritls_rustls::default_provider().install_default();
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for WebFetch {
    fn default() -> Self {
        Self::new()
    }
}

struct Call {
    url: reqwest::Url,
    timeout: Duration,
}

fn parse(args: &Value) -> Result<Call, ToolError> {
    let raw = args["url"]
        .as_str()
        .ok_or_else(|| ToolError::InvalidArgs("web_fetch needs a string `url`".to_owned()))?;
    let url = reqwest::Url::parse(raw)
        .map_err(|error| ToolError::InvalidArgs(format!("`url` is not a URL: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ToolError::InvalidArgs(format!(
            "web_fetch only fetches http and https URLs, not {}",
            url.scheme()
        )));
    }
    let timeout = match args.get("timeout_secs") {
        None | Some(Value::Null) => Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        Some(value) => {
            let seconds = value.as_u64().ok_or_else(|| {
                ToolError::InvalidArgs("`timeout_secs` must be a positive integer".to_owned())
            })?;
            if seconds == 0 || seconds > MAX_TIMEOUT_SECS {
                return Err(ToolError::InvalidArgs(format!(
                    "`timeout_secs` must be between 1 and {MAX_TIMEOUT_SECS}"
                )));
            }
            Duration::from_secs(seconds)
        }
    };
    Ok(Call { url, timeout })
}

fn mime_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn is_html(mime: &str) -> bool {
    mime == "text/html" || mime == "application/xhtml+xml"
}

fn is_text(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/xhtml+xml"
                | "application/x-yaml"
                | "application/yaml"
                | "application/toml"
        )
}

#[async_trait]
impl Tool for WebFetch {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "web_fetch".to_owned(),
            description: "Fetches one http(s) URL and returns its text, converting HTML to \
                          markdown. The body is capped and oversized results are spilled to a \
                          file. The user approves each fetch. No JavaScript is executed."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "An http or https URL."},
                    "timeout_secs": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS,
                                     "description": "Give up after this many seconds."}
                },
                "required": ["url"]
            }),
        }
    }

    async fn needs_approval(&self, _ctx: &ToolCtx<'_>, args: &Value) -> Option<ApprovalRequest> {
        let raw = args["url"].as_str()?;
        let url = reqwest::Url::parse(raw).ok()?;
        if !matches!(url.scheme(), "http" | "https") {
            // `execute` refuses this with a precise message; asking the user about a call that
            // cannot run would only spend their attention.
            return None;
        }
        Some(
            ApprovalRequest::new("web_fetch", format!("fetch {url}"), RiskLevel::Network)
                .with_preview(ApprovalPreview::Excerpt(url.to_string()))
                // See the module docs: the rule table cannot express a domain, so "always" would be
                // a whole-tool grant the user was not shown.
                .once_only(),
        )
    }

    fn summarize(&self, args: &Value) -> ToolCallSummary {
        let raw = args["url"].as_str().unwrap_or("?");
        let short = reqwest::Url::parse(raw)
            .ok()
            .map(|url| {
                format!(
                    "{}://{}{}",
                    url.scheme(),
                    url.host_str().unwrap_or(""),
                    url.path()
                )
            })
            .unwrap_or_else(|| raw.to_owned());
        ToolCallSummary::new(format!("web_fetch {short}")).with_detail(raw.to_owned())
    }

    async fn execute(&self, ctx: ToolCtx<'_>, args: Value) -> Result<ToolOutput, ToolError> {
        let call = parse(&args)?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let mut response = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
            response = self
                .client
                .get(call.url.clone())
                .header(reqwest::header::USER_AGENT, USER_AGENT)
                .timeout(call.timeout)
                .send() => response.map_err(|error| {
                    ToolError::Backend(format!("fetch {}: {error}", call.url))
                })?,
        };
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let mime = mime_type(&content_type);

        // Sniff a missing content type: plenty of plain-text servers omit it, and an HTML page
        // served as `application/octet-stream` is still more useful converted than refused.
        let looks_like_html = mime.is_empty()
            && response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .is_none();
        if !mime.is_empty() && !is_html(&mime) && !is_text(&mime) {
            return Err(ToolError::Backend(format!(
                "{} returned {mime}, which web_fetch cannot read as text",
                call.url
            )));
        }

        let mut body = Vec::new();
        let mut truncated = false;
        loop {
            let chunk = tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|error| {
                    ToolError::Backend(format!("reading {}: {error}", call.url))
                })?,
            };
            let Some(chunk) = chunk else {
                break;
            };
            if body.len() + chunk.len() > MAX_DOWNLOAD_BYTES {
                let room = MAX_DOWNLOAD_BYTES - body.len();
                body.extend_from_slice(&chunk[..room]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }

        let text = String::from_utf8_lossy(&body).into_owned();
        let html = is_html(&mime) || (looks_like_html && text.trim_start().starts_with('<'));
        let content = if html {
            htmd::convert(&text)
                .map_err(|error| ToolError::Backend(format!("html to markdown: {error}")))?
        } else {
            text
        };

        let mut output = format!(
            "fetched {}\nstatus: {status}\ncontent-type: {}\n",
            call.url,
            if mime.is_empty() {
                "(none)"
            } else {
                mime.as_str()
            }
        );
        if truncated {
            output.push_str(&format!("[body truncated at {MAX_DOWNLOAD_BYTES} bytes]\n"));
        }
        output.push_str("---\n");
        if content.trim().is_empty() {
            output.push_str("(empty body)\n");
        } else {
            output.push_str(&content);
            if !output.ends_with('\n') {
                output.push('\n');
            }
        }
        Ok(ToolOutput::text(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_testkit::MemoryFs;
    use tokio_util::sync::CancellationToken;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ctx<'a>(
        fs: &'a MemoryFs,
        terminal: &'a hatchery_capabilities::NoTerminal,
        cancel: &'a CancellationToken,
    ) -> ToolCtx<'a> {
        ToolCtx {
            fs,
            terminal,
            cancel: cancel.clone(),
            emit: &|_| {},
        }
    }

    async fn fetch(url: &str) -> Result<ToolOutput, ToolError> {
        let fs = MemoryFs::new();
        let terminal = hatchery_capabilities::NoTerminal;
        let cancel = CancellationToken::new();
        WebFetch::new()
            .execute(
                ctx(&fs, &terminal, &cancel),
                serde_json::json!({"url": url}),
            )
            .await
    }

    #[tokio::test]
    async fn an_html_page_becomes_markdown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<html><body><h1>Title</h1><p>Hello <b>world</b></p></body></html>",
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        let output = fetch(&format!("{}/page", server.uri()))
            .await
            .expect("fetches");
        assert!(output.text.contains("status: 200"), "{}", output.text);
        assert!(output.text.contains("# Title"), "{}", output.text);
        assert!(output.text.contains("**world**"), "{}", output.text);
    }

    #[tokio::test]
    async fn plain_text_is_returned_without_conversion() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/notes.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("hello\nworld\n", "text/plain"))
            .mount(&server)
            .await;
        let output = fetch(&format!("{}/notes.txt", server.uri()))
            .await
            .expect("fetches");
        assert!(output.text.contains("hello\nworld"), "{}", output.text);
        assert!(!output.text.contains("<p>"), "{}", output.text);
    }

    #[tokio::test]
    async fn an_error_status_is_reported_rather_than_hidden() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing"))
            .respond_with(ResponseTemplate::new(404).set_body_raw("not here", "text/plain"))
            .mount(&server)
            .await;
        let output = fetch(&format!("{}/missing", server.uri()))
            .await
            .expect("the fetch ran");
        assert!(output.text.contains("status: 404"), "{}", output.text);
        assert!(output.text.contains("not here"), "{}", output.text);
    }

    #[tokio::test]
    async fn a_binary_content_type_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/image.png"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0_u8, 1, 2], "image/png"))
            .mount(&server)
            .await;
        let error = fetch(&format!("{}/image.png", server.uri()))
            .await
            .expect_err("refused");
        assert!(error.to_string().contains("image/png"), "{error}");
    }

    #[tokio::test]
    async fn a_scheme_other_than_http_is_refused_before_any_request() {
        let error = fetch("file:///etc/passwd").await.expect_err("refused");
        assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
        assert!(error.to_string().contains("http"), "{error}");
    }

    #[tokio::test]
    async fn a_slow_server_hits_the_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slow"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_raw("late", "text/plain"),
            )
            .mount(&server)
            .await;
        let fs = MemoryFs::new();
        let terminal = hatchery_capabilities::NoTerminal;
        let cancel = CancellationToken::new();
        let error = WebFetch::new()
            .execute(
                ctx(&fs, &terminal, &cancel),
                serde_json::json!({"url": format!("{}/slow", server.uri()), "timeout_secs": 1}),
            )
            .await
            .expect_err("timed out");
        assert!(error.to_string().contains("fetch"), "{error}");
    }

    #[tokio::test]
    async fn the_approval_is_one_shot_and_names_the_network() {
        let fs = MemoryFs::new();
        let terminal = hatchery_capabilities::NoTerminal;
        let cancel = CancellationToken::new();
        let request = WebFetch::new()
            .needs_approval(
                &ctx(&fs, &terminal, &cancel),
                &serde_json::json!({"url": "https://example.com/a"}),
            )
            .await
            .expect("an approval");
        assert_eq!(request.risk, RiskLevel::Network);
        assert_eq!(
            request.options,
            [
                hatchery_protocol::ApprovalOption::AllowOnce,
                hatchery_protocol::ApprovalOption::Deny
            ],
            "a domain rule does not exist yet; a whole-tool always would be too broad"
        );
        assert!(request.args_digest.contains("https://example.com/a"));
    }

    #[tokio::test]
    async fn a_non_http_url_gets_no_approval_prompt() {
        let fs = MemoryFs::new();
        let terminal = hatchery_capabilities::NoTerminal;
        let cancel = CancellationToken::new();
        let request = WebFetch::new();
        let args = serde_json::json!({"url": "file:///etc/passwd"});
        let ctx = ToolCtx {
            fs: &fs,
            terminal: &terminal,
            cancel: cancel.clone(),
            emit: &|_| {},
        };
        assert!(request.needs_approval(&ctx, &args).await.is_none());
    }
}
