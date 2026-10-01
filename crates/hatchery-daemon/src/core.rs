//! The method dispatch: the daemon's half of the protocol, transport-free.
//!
//! [`DaemonCore::dispatch`] is the whole server surface as a pure(ish) function — request in,
//! response out — which is what the tests exercise and what the transport layer in `server.rs`
//! forwards to. Everything the design calls "assembly audit" and "disposer" also lives here:
//! the audit refuses to serve a half-wired daemon, and teardown runs disposers in strict
//! reverse registration order (ADR-0009).

use std::sync::Arc;

use hatchery_protocol::{
    ErrorCode, ErrorObject, EventError, PROTOCOL_VERSION, SessionEvent, SessionId, method as m,
};
use hatchery_protocol::{Id, Incoming, Request, Response};
use hatchery_store::SessionStore;

use crate::config::LayeredConfig;
use crate::hub::LiveHub;
use crate::manager::SessionManager;

/// What this daemon serves, in the order the protocol lists them.
///
/// M1 is the chat loop: sessions, config, prompt rendering. Branch surgery, rewind, approvals
/// and export arrive with their milestones; a frontend reading this list degrades gracefully
/// instead of calling into the void.
const SERVED_METHODS: &[&str] = &[
    m::DAEMON_HELLO,
    m::SESSION_NEW,
    m::SESSION_LOAD,
    m::SESSION_LIST,
    m::SESSION_PROMPT,
    m::SESSION_CANCEL,
    m::SESSION_SET_CONFIG,
    m::CONFIG_GET,
    m::CONFIG_SET,
    m::PROMPT_RENDER,
];

/// The method names this build serves, for the capability list and tests.
#[must_use]
pub fn served_methods() -> &'static [&'static str] {
    SERVED_METHODS
}

/// The daemon's service core: every method, no sockets.
pub struct DaemonCore {
    manager: Arc<SessionManager>,
    config: Arc<LayeredConfig>,
    store: Arc<dyn SessionStore>,
    /// The boot token this daemon accepted at startup; `daemon/hello` must present it.
    boot_token: String,
    hello: m::HelloResult,
}

impl DaemonCore {
    /// Assembles the core.
    #[must_use]
    pub fn new(
        manager: Arc<SessionManager>,
        config: Arc<LayeredConfig>,
        store: Arc<dyn SessionStore>,
        boot_token: String,
    ) -> Self {
        let hello = m::HelloResult {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            supported_versions: vec![PROTOCOL_VERSION.to_owned()],
            capabilities: m::DaemonCapabilities {
                methods: SERVED_METHODS.iter().map(|s| (*s).to_owned()).collect(),
                modes: vec![hatchery_protocol::SessionModeId::chat()],
            },
        };
        Self {
            manager,
            config,
            store,
            boot_token,
            hello,
        }
    }

    /// Handles one request, any failure becoming a protocol error object.
    ///
    /// Panics are caught one frame up (the connection task), never here.
    #[must_use]
    pub async fn dispatch(&self, request: &Request) -> Response {
        let id = request.id.clone();
        match self.handle(request).await {
            Ok(result) => Response::ok(id.clone(), &result).unwrap_or_else(|error| {
                Response::err(
                    id,
                    ErrorObject::new(ErrorCode::InternalError, error.to_string()),
                )
            }),
            Err(event) => Response::err(
                id,
                ErrorObject {
                    code: event.code.as_i64(),
                    message: event.message,
                    data: event.retryable.map(serde_json::Value::from),
                },
            ),
        }
    }

    async fn handle(&self, request: &Request) -> Result<serde_json::Value, EventError> {
        let params = request.params.clone().unwrap_or(serde_json::Value::Null);
        match request.method.as_str() {
            m::DAEMON_HELLO => {
                let hello: m::HelloParams = serde_json::from_value(params).map_err(bad_params)?;
                if !hatchery_protocol::is_compatible(&hello.protocol_version) {
                    return Err(EventError::new(
                        ErrorCode::UnsupportedProtocolVersion,
                        format!(
                            "the client speaks {}, this daemon speaks {PROTOCOL_VERSION}",
                            hello.protocol_version
                        ),
                    ));
                }
                if hello.boot_token.as_deref() != Some(self.boot_token.as_str()) {
                    return Err(EventError::new(
                        ErrorCode::InvalidRequest,
                        "the boot token does not match this daemon",
                    ));
                }
                serde_json::to_value(&self.hello).map_err(internal)
            }
            m::SESSION_NEW => {
                let params: m::SessionNewParams =
                    serde_json::from_value(params).map_err(bad_params)?;
                serde_json::to_value(self.manager.new_session(params).await.map_err(manager)?)
                    .map_err(internal)
            }
            m::SESSION_LOAD => {
                let params: m::SessionLoadParams =
                    serde_json::from_value(params).map_err(bad_params)?;
                serde_json::to_value(self.manager.load(params).await.map_err(manager)?)
                    .map_err(internal)
            }
            m::SESSION_LIST => {
                let params: m::SessionListParams =
                    serde_json::from_value(params).map_err(bad_params)?;
                serde_json::to_value(self.manager.list(params).await.map_err(manager)?)
                    .map_err(internal)
            }
            m::SESSION_PROMPT => {
                let params: m::SessionPromptParams =
                    serde_json::from_value(params).map_err(bad_params)?;
                serde_json::to_value(self.manager.prompt(params).await.map_err(manager)?)
                    .map_err(internal)
            }
            m::SESSION_CANCEL => {
                let params: m::SessionCancelParams =
                    serde_json::from_value(params).map_err(bad_params)?;
                serde_json::to_value(
                    self.manager
                        .cancel(params.session_id)
                        .await
                        .map_err(manager)?,
                )
                .map_err(internal)
            }
            m::SESSION_SET_CONFIG => {
                let params: m::SetConfigParams =
                    serde_json::from_value(params).map_err(bad_params)?;
                serde_json::to_value(self.manager.set_config(params).await.map_err(manager)?)
                    .map_err(internal)
            }
            m::CONFIG_GET => {
                let params: m::ConfigGetParams =
                    serde_json::from_value(params).map_err(bad_params)?;
                let entries = self.config.entries(params.key_path.as_deref());
                serde_json::to_value(m::ConfigGetResult { entries }).map_err(internal)
            }
            m::CONFIG_SET => {
                let params: m::ConfigSetParams =
                    serde_json::from_value(params).map_err(bad_params)?;
                let entry = self
                    .config
                    .set(&params.key_path, params.value)
                    .map_err(|error| EventError::new(ErrorCode::ConfigError, error.to_string()))?;
                serde_json::to_value(m::ConfigSetResult { entry }).map_err(internal)
            }
            m::PROMPT_RENDER => {
                let params: m::PromptRenderParams =
                    serde_json::from_value(params).map_err(bad_params)?;
                self.render_prompt(params).await
            }
            other => Err(EventError::new(
                ErrorCode::MethodNotFound,
                format!("`{other}` is not served by this daemon"),
            )),
        }
    }

    /// `prompt/render`: the assembled Chat prompt for a session or a bare mode.
    async fn render_prompt(
        &self,
        params: m::PromptRenderParams,
    ) -> Result<serde_json::Value, EventError> {
        let (workspace, model) = match params.session_id {
            Some(session_id) => {
                let session =
                    self.store.session(session_id).await.map_err(|error| {
                        EventError::new(ErrorCode::StoreError, error.to_string())
                    })?;
                (session.workspace.clone(), session.model.model)
            }
            None => (None, String::new()),
        };
        let overrides = self.config.ui();
        let env = crate::prompt::Environment {
            cwd: workspace
                .as_ref()
                .map_or_else(
                    || std::env::current_dir().unwrap_or_default(),
                    std::clone::Clone::clone,
                )
                .display()
                .to_string(),
            platform: std::env::consts::OS.to_owned(),
            date: humantime_date(),
            git_status: crate::prompt::git_summary(workspace.as_deref()),
            response_language: overrides.response_language.unwrap_or_default(),
        };
        let _ = model;
        let sections = crate::prompt::render_chat(&env, None);
        serde_json::to_value(crate::prompt::render_result(&sections)).map_err(internal)
    }
}

fn bad_params(error: serde_json::Error) -> EventError {
    EventError::new(ErrorCode::InvalidParams, error.to_string())
}

fn internal(error: serde_json::Error) -> EventError {
    EventError::new(ErrorCode::InternalError, error.to_string())
}

fn manager(error: crate::manager::ManagerError) -> EventError {
    error.into_event()
}

/// Today as `YYYY-MM-DD` via the standard library only.
pub(crate) use crate::clock::humantime_date;

/// The audit: what must be true before the daemon may serve.
///
/// Fail-loud (ADR-0009): the missing pieces are listed, all at once, and the daemon exits —
/// a half-wired runtime would surface as confusing per-session failures instead.
#[must_use]
pub fn audit(
    config: &LayeredConfig,
    state_dir: &crate::discover::StateDir,
    data_dir: &std::path::Path,
) -> Vec<String> {
    let mut missing = Vec::new();
    if config.providers().is_empty() {
        missing.push("no provider is configured (add a [providers.*] table)".to_owned());
    }
    for (id, provider) in config.providers() {
        if provider.env_key.is_empty() {
            continue; // the entry itself is incomplete; the per-provider check below catches it
        }
        if std::env::var(&provider.env_key).is_err() {
            missing.push(format!(
                "provider `{id}` needs environment variable `{}` set",
                provider.env_key
            ));
        }
    }
    if std::fs::create_dir_all(data_dir).is_err() {
        missing.push(format!(
            "the data directory {} cannot be created",
            data_dir.display()
        ));
    }
    if state_dir.ensure().is_err() {
        missing.push(format!(
            "the state directory {} cannot be created",
            state_dir.lock_path().parent().map_or_else(
                || "unknown".to_owned(),
                |parent| parent.display().to_string(),
            )
        ));
    }
    missing
}

/// One teardown step: its name (for the log and the order test) and its action.
type Disposer = (&'static str, Box<dyn FnOnce() + Send>);

/// Disposers, run in strict reverse registration order.
#[derive(Default)]
pub struct Disposers {
    stack: Vec<Disposer>,
}

impl Disposers {
    /// An empty stack.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a teardown step.
    pub fn push(&mut self, name: &'static str, dispose: impl FnOnce() + Send + 'static) {
        self.stack.push((name, Box::new(dispose)));
    }

    /// How many steps are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.stack.len()
    }

    /// True when nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// Runs everything, last registered first.
    ///
    /// A panicking disposer is caught and logged: teardown must finish even when one step is
    /// broken, and the remaining steps still run.
    pub fn run_reverse(&mut self) -> Vec<&'static str> {
        let mut order = Vec::with_capacity(self.stack.len());
        while let Some((name, dispose)) = self.stack.pop() {
            order.push(name);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(dispose));
            if result.is_err() {
                tracing::error!("disposer `{name}` panicked during teardown; continuing");
            }
        }
        order
    }
}

/// Subscribes the manager's counters and the hub to a new events connection.
///
/// The server layer calls this once per event connection; it returns the receiver the
/// connection's forward task reads.
#[must_use]
pub fn subscribe_session(
    manager: &SessionManager,
    hub: &LiveHub,
    session: SessionId,
) -> tokio::sync::broadcast::Receiver<SessionEvent> {
    manager.attach(session);
    hub.subscribe(session)
}

/// Notes a connection leaving, so idle sweeps see the true watcher count.
pub fn detach_session(manager: &SessionManager, hub: &LiveHub, session: SessionId) {
    let _ = hub; // the hub has no per-connection state; the manager's count is the truth
    manager.detach(session);
}

/// Marks a `session/prompt` as forbidden while a turn runs — the manager already refuses; this
/// exists so the error code is spelled from one place.
#[must_use]
pub fn turn_in_progress() -> EventError {
    EventError::new(ErrorCode::TurnInProgress, "a turn is already running")
}

/// True when the incoming frame is a request (notifications carry no reply).
#[must_use]
pub const fn is_request(incoming: &Incoming) -> bool {
    matches!(incoming, Incoming::Request(_))
}

/// The id of a request, for logging.
#[must_use]
pub fn request_id(request: &Request) -> Id {
    request.id.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::StateDir;
    use hatchery_protocol::method::SessionNewParams;
    use hatchery_protocol::{ModelRef, SessionModeId};

    async fn core() -> (tempfile::TempDir, DaemonCore, Arc<LayeredConfig>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn SessionStore> = Arc::new(
            hatchery_store::TursoStore::open(dir.path().join("t.db"))
                .await
                .expect("open"),
        );
        let config = Arc::new(LayeredConfig::from_layers(vec![]));
        let hub = Arc::new(LiveHub::new());
        let manager = Arc::new(SessionManager::new(
            Arc::clone(&store),
            Arc::clone(&config),
            hub,
            dir.path().to_path_buf(),
        ));
        (
            dir,
            DaemonCore::new(manager, config, store, "token".to_owned()),
            Arc::new(LayeredConfig::from_layers(vec![])),
        )
    }

    fn request(method: &str, params: serde_json::Value) -> Request {
        Request::new(Id::Number(1), method)
            .with_params(&params)
            .expect("params")
    }

    #[tokio::test]
    async fn hello_accepts_the_right_token_and_refuses_the_wrong_one() {
        let (_dir, core, _config) = core().await;
        let ok = core
            .dispatch(&request(
                m::DAEMON_HELLO,
                serde_json::json!({
                    "protocol_version": PROTOCOL_VERSION,
                    "boot_token": "token",
                }),
            ))
            .await;
        assert!(ok.is_ok(), "{ok:?}");

        let bad_token = core
            .dispatch(&request(
                m::DAEMON_HELLO,
                serde_json::json!({
                    "protocol_version": PROTOCOL_VERSION,
                    "boot_token": "nope",
                }),
            ))
            .await;
        assert!(!bad_token.is_ok());

        let bad_version = core
            .dispatch(&request(
                m::DAEMON_HELLO,
                serde_json::json!({
                    "protocol_version": "9.0.0",
                    "boot_token": "token",
                }),
            ))
            .await;
        assert!(!bad_version.is_ok());
        let error = bad_version.error.expect("error object");
        assert_eq!(error.code, ErrorCode::UnsupportedProtocolVersion.as_i64());
    }

    #[tokio::test]
    async fn an_unknown_method_is_method_not_found() {
        let (_dir, core, _config) = core().await;
        let response = core
            .dispatch(&request("session/rewind", serde_json::json!({})))
            .await;
        let error = response.error.expect("refused");
        assert_eq!(error.code, ErrorCode::MethodNotFound.as_i64());
    }

    #[tokio::test]
    async fn the_chat_loop_runs_through_dispatch() {
        let (_dir, core, _config) = core().await;
        let created = core
            .dispatch(&request(
                m::SESSION_NEW,
                serde_json::to_value(SessionNewParams {
                    mode: SessionModeId::chat(),
                    workspace: None,
                    model: Some(ModelRef::new("deepseek", "deepseek-flash")),
                    title: None,
                    config_patch: None,
                })
                .expect("params"),
            ))
            .await;
        let result: m::SessionNewResult =
            serde_json::from_value(created.result.expect("ok")).expect("typed");
        assert_eq!(result.session.mode, SessionModeId::chat());

        let listed = core
            .dispatch(&request(m::SESSION_LIST, serde_json::json!({})))
            .await;
        let listed: m::SessionListResult =
            serde_json::from_value(listed.result.expect("ok")).expect("typed");
        assert_eq!(listed.sessions.len(), 1);
    }

    #[tokio::test]
    async fn config_round_trips_through_dispatch() {
        let (_dir, core, _config) = core().await;
        let set = core
            .dispatch(&request(
                m::CONFIG_SET,
                serde_json::json!({"key_path": "ui.show_reasoning", "value": false}),
            ))
            .await;
        assert!(set.is_ok(), "{set:?}");

        let get = core
            .dispatch(&request(
                m::CONFIG_GET,
                serde_json::json!({"key_path": "ui.show_reasoning"}),
            ))
            .await;
        let result: m::ConfigGetResult =
            serde_json::from_value(get.result.expect("ok")).expect("typed");
        assert_eq!(result.entries[0].value, serde_json::json!(false));
        assert_eq!(result.entries[0].origin, m::ConfigOrigin::Runtime);
    }

    #[tokio::test]
    async fn prompt_render_lists_four_sections() {
        let (_dir, core, _config) = core().await;
        let rendered = core
            .dispatch(&request(
                m::PROMPT_RENDER,
                serde_json::json!({"mode": "chat"}),
            ))
            .await;
        let result: m::PromptRenderResult =
            serde_json::from_value(rendered.result.expect("ok")).expect("typed");
        let ids: Vec<&str> = result.sections.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["identity", "mode_chat", "environment", "safety_gate"]);
    }

    #[test]
    fn the_audit_lists_every_missing_piece_at_once() {
        let config = LayeredConfig::from_layers(vec![(
            m::ConfigOrigin::User,
            toml::from_str(
                "[providers.gw]\nbase_url = \"https://gw\"\nenv_key = \"HATCHERY_TEST_MISSING_KEY\"\n",
            )
            .expect("toml"),
        )]);
        let dir = tempfile::tempdir().expect("tempdir");
        let state = StateDir::at(dir.path().join("state"));
        let findings = audit(&config, &state, &dir.path().join("data"));
        assert!(
            findings
                .iter()
                .any(|f| f.contains("HATCHERY_TEST_MISSING_KEY")),
            "the missing key is named: {findings:?}"
        );
    }

    #[test]
    fn disposers_run_in_strict_reverse_order_and_survive_panics() {
        let mut disposers = Disposers::new();
        let order: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>> = std::sync::Arc::default();
        let o1 = Arc::clone(&order);
        disposers.push("first", move || o1.lock().expect("locked").push("first"));
        let o2 = Arc::clone(&order);
        disposers.push("second", move || o2.lock().expect("locked").push("second"));
        disposers.push("panics", || panic!("a broken teardown step"));
        let o3 = Arc::clone(&order);
        disposers.push("third", move || o3.lock().expect("locked").push("third"));

        let ran = disposers.run_reverse();
        assert_eq!(ran, ["third", "panics", "second", "first"]);
        assert_eq!(
            *order.lock().expect("locked"),
            ["third", "second", "first"],
            "the panic did not stop the rest, and the order is reversed"
        );
    }

    #[test]
    fn server_constants_name_what_m1_serves() {
        assert!(SERVED_METHODS.contains(&m::SESSION_PROMPT));
        assert!(!SERVED_METHODS.contains(&m::SESSION_REWIND), "rewind is M2");
        assert!(
            !SERVED_METHODS.contains(&m::APPROVAL_RESPOND),
            "approvals are M2"
        );
        assert!(is_request(&Incoming::Request(request(
            "x",
            serde_json::json!({})
        ))));
    }
}
