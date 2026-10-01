//! A real daemon on a real (temp) socket, plus the probe that talks to it.
//!
//! The point of spawning the full stack in tests: the e2e assertions then cover UDS, framing,
//! routing and event fan-out — the layers a pure `DaemonCore` test cannot see. This also
//! answers testing.md's open question 4 for M1: in-process-but-through-sockets is the default
//! e2e shape, because it costs one temp dir and covers every byte the wire sees.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use hatchery_daemon::config::LayeredConfig;
use hatchery_daemon::core::DaemonCore;
use hatchery_daemon::discover::{DaemonInfo, StateDir};
use hatchery_daemon::hub::LiveHub;
use hatchery_daemon::manager::SessionManager;
use hatchery_protocol::{ClientError, DaemonClient, EventStream, SessionId};

/// A running daemon over a temp socket, with its guard and store.
pub struct TestDaemon {
    /// The state directory this daemon published itself under.
    pub state: StateDir,
    /// The socket path clients connect to.
    pub socket: std::path::PathBuf,
    /// The boot token from `daemon.json`.
    pub boot_token: String,
    /// The live hub, for tests that bisect publish from transport.
    pub hub: Arc<LiveHub>,
    shutdown: CancellationToken,
    /// Keeps the tempdir alive for the daemon's lifetime.
    _dir: tempfile::TempDir,
}

impl TestDaemon {
    /// Spawns the daemon: store on a temp db, layered config over the given documents, one UDS.
    ///
    /// # Panics
    ///
    /// Panics when any part of the stack fails to start — a test environment fault, not a case
    /// to branch on.
    pub async fn start(
        layers: Vec<(hatchery_protocol::method::ConfigOrigin, toml::Table)>,
    ) -> Self {
        hatchery_llm::install_tls_provider();
        let dir = tempfile::tempdir().expect("tempdir");
        let state = StateDir::at(dir.path().join("state"));
        state.ensure().expect("state dir");

        let store: Arc<dyn hatchery_store::SessionStore> = Arc::new(
            hatchery_store::TursoStore::open(dir.path().join("sessions.db"))
                .await
                .expect("store"),
        );
        let config = Arc::new(LayeredConfig::from_layers(layers));
        let hub = Arc::new(LiveHub::new());
        let manager = Arc::new(SessionManager::new(
            Arc::clone(&store),
            Arc::clone(&config),
            Arc::clone(&hub),
            dir.path().to_path_buf(),
        ));
        manager.recover_crashed_sessions().await.expect("recover");

        let boot_token = format!("test-boot-{}", hatchery_protocol::SessionId::new());
        let core = Arc::new(DaemonCore::new(
            Arc::clone(&manager),
            Arc::clone(&config),
            Arc::clone(&store),
            boot_token.clone(),
        ));

        let socket = state.socket_path();
        let shutdown = CancellationToken::new();
        {
            let core = Arc::clone(&core);
            let manager = Arc::clone(&manager);
            let hub = Arc::clone(&hub);
            let socket = socket.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                let server = hatchery_daemon::server::serve_uds(core, manager, hub, socket);
                tokio::select! {
                    _ = server => {},
                    _ = shutdown.cancelled() => {},
                }
            });
        }
        // Wait for the socket to appear, so a test never races the bind.
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(socket.exists(), "the daemon socket never appeared");

        let info = DaemonInfo {
            pid: std::process::id(),
            uds_path: socket.display().to_string(),
            protocol_version: hatchery_protocol::PROTOCOL_VERSION.to_owned(),
            boot_token: boot_token.clone(),
            started_at: hatchery_daemon::discover::unix_now(),
        };
        state.publish(&info).expect("publish");

        Self {
            state,
            socket,
            boot_token,
            hub,
            shutdown,
            _dir: dir,
        }
    }

    /// Stops the daemon and waits for the socket to disappear.
    pub async fn stop(self) {
        self.shutdown.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = self.state.clear();
    }
}

/// A frontend-shaped client: hello first, then calls and events like the real CLI.
pub struct ClientProbe {
    client: DaemonClient,
}

impl ClientProbe {
    /// Connects and shakes hands with the boot token.
    ///
    /// # Panics
    ///
    /// Panics on any refusal: a probe against a just-started test daemon has no excuses.
    pub async fn attach(daemon: &TestDaemon) -> Self {
        let client = DaemonClient::connect(&daemon.socket)
            .await
            .expect("connect");
        client
            .hello(Some(daemon.boot_token.clone()))
            .await
            .expect("hello");
        Self { client }
    }

    /// One typed call.
    ///
    /// # Panics
    ///
    /// Panics on failure; a probe asserts the happy path and lets the negative cases speak for
    /// themselves through [`try_call`](Self::try_call).
    pub async fn call<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> R {
        match self.try_call(method, params).await {
            Ok(result) => result,
            Err(error) => panic!("{method} failed: {error}"),
        }
    }

    /// One typed call, keeping the failure for assertions about refusals.
    pub async fn try_call<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R, ClientError> {
        let value = self
            .client
            .call_raw(method, serde_json::to_value(params).expect("params"))
            .await?;
        serde_json::from_value(value)
            .map_err(|error| ClientError::Connection(format!("unusable reply: {error}")))
    }

    /// Opens an events connection and subscribes it to a session.
    pub async fn events(&self, daemon: &TestDaemon, session: SessionId) -> EventStream {
        let mut stream = EventStream::connect(&daemon.socket)
            .await
            .expect("event socket");
        let reply = stream
            .subscribe(
                hatchery_protocol::method::SESSION_LOAD,
                serde_json::json!({"session_id": session}),
            )
            .await
            .expect("subscribe");
        assert!(
            reply.get("session").is_some(),
            "the subscribing call's reply arrives on the events connection: {reply}"
        );
        stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_protocol::ModelRef;
    use hatchery_protocol::method::{
        ConfigOrigin, SessionListParams, SessionListResult, SessionNewParams, SessionNewResult,
    };

    fn chat_params() -> SessionNewParams {
        SessionNewParams {
            mode: hatchery_protocol::SessionModeId::chat(),
            workspace: None,
            model: Some(ModelRef::new("deepseek", "deepseek-flash")),
            title: None,
            config_patch: None,
        }
    }

    #[tokio::test]
    async fn the_full_stack_answers_over_a_real_socket() {
        let daemon = TestDaemon::start(vec![(
            ConfigOrigin::User,
            toml::from_str("[providers.deepseek]\nbase_url = \"https://api.deepseek.com\"\nenv_key = \"DEEPSEEK_API_KEY\"\nmodels = [\"deepseek-flash\"]\n").expect("toml"),
        )])
        .await;

        let probe = ClientProbe::attach(&daemon).await;
        let created: SessionNewResult = probe
            .call(hatchery_protocol::method::SESSION_NEW, &chat_params())
            .await;
        let listed: SessionListResult = probe
            .call(
                hatchery_protocol::method::SESSION_LIST,
                &SessionListParams::default(),
            )
            .await;
        assert_eq!(listed.sessions.len(), 1);
        assert_eq!(listed.sessions[0].id, created.session.id);

        daemon.stop().await;
    }

    #[tokio::test]
    async fn a_wrong_boot_token_is_refused_at_hello() {
        let daemon = TestDaemon::start(vec![]).await;
        let client = DaemonClient::connect(&daemon.socket)
            .await
            .expect("connect");
        let error = client
            .hello(Some("wrong-token".to_owned()))
            .await
            .expect_err("refused");
        assert!(error.to_string().contains("boot token"), "{error}");
        daemon.stop().await;
    }

    #[tokio::test]
    async fn two_probes_see_each_others_session_and_events_flow() {
        let daemon = TestDaemon::start(vec![(
            ConfigOrigin::User,
            toml::from_str("[providers.deepseek]\nbase_url = \"https://api.deepseek.com\"\nenv_key = \"DEEPSEEK_API_KEY\"\nmodels = [\"deepseek-flash\"]\n").expect("toml"),
        )])
        .await;

        let first = ClientProbe::attach(&daemon).await;
        let created: SessionNewResult = first
            .call(hatchery_protocol::method::SESSION_NEW, &chat_params())
            .await;

        // The second frontend loads the same session on its own events connection.
        let mut second_events = {
            let second = ClientProbe::attach(&daemon).await;
            second.events(&daemon, created.session.id).await
        };

        // The hub itself has a subscriber: proves the publish path fires even if the socket
        // half is the problem.
        let mut hub_watch = daemon.hub.subscribe(created.session.id);

        // Config change through one probe arrives as an event on the other's stream.
        let _touched: SessionNewResult = first
            .call(
                hatchery_protocol::method::SESSION_SET_CONFIG,
                &hatchery_protocol::method::SetConfigParams {
                    session_id: created.session.id,
                    patch: hatchery_protocol::method::ConfigPatch {
                        model: None,
                        reasoning_effort: Some(hatchery_protocol::ReasoningEffort::Low),
                        overrides: None,
                    },
                },
            )
            .await;

        let hub_event = tokio::time::timeout(Duration::from_secs(2), hub_watch.recv()).await;
        assert!(hub_event.is_ok(), "the hub never received the publish");

        let event = tokio::time::timeout(Duration::from_secs(5), second_events.next())
            .await
            .expect("an event arrived in time");
        let event = event.expect("stream alive");
        assert_eq!(event.session, created.session.id);
        assert!(
            matches!(
                event.event,
                hatchery_protocol::ServerEvent::SessionUpdated { .. }
            ),
            "the fan-out carries the config change: {:?}",
            event.event
        );

        daemon.stop().await;
    }
}
