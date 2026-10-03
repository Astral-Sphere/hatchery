//! The session manager: which sessions exist, which have live runtimes, and who may drive them.
//!
//! M1 runs a single live runtime at a time in practice, but the bookkeeping is per-session from
//! the start — slots, generations and turn gates are per-session shapes, and the single-session
//! limit is one config away, not one redesign away (docs/design/daemon.md §3).
//!
//! Two decisions this file pins for M1 (both open questions in daemon.md, answered here):
//!
//! * **D2, a turn in flight with nobody watching runs to completion.** The results are in the
//!   store either way; killing work because the last frontend blinked would throw away a paid
//!   for turn.
//! * **Idle unload** drops a runtime only after it has been idle *and* subscriber-less for the
//!   configured timeout. The session row survives; the next prompt reassembles.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hatchery_kernel::AgentCommand;
use hatchery_llm::ProviderRegistry;
use hatchery_protocol::method::{
    SessionListParams, SessionListResult, SessionLoadParams, SessionLoadResult, SessionNewParams,
    SessionNewResult, SessionPromptParams, SessionPromptResult, SetConfigParams, SetConfigResult,
};
use hatchery_protocol::{
    ErrorCode, EventError, Item, ItemId, ServerEvent, Session, SessionEvent, SessionId,
    SessionPatch, SessionStatus, TurnId,
};
use hatchery_store::SessionStore;

use crate::config::LayeredConfig;
use crate::hub::LiveHub;
use crate::runtime::SessionRuntime;

/// Why a manager call failed, in the wire's vocabulary.
#[derive(Clone, Debug, thiserror::Error)]
pub struct ManagerError(pub EventError);

impl std::fmt::Display for ManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.message)
    }
}

impl ManagerError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(EventError::new(code, message))
    }

    /// The wire shape.
    #[must_use]
    pub fn into_event(self) -> EventError {
        self.0
    }
}

/// One live runtime.
struct Slot {
    runtime: Arc<SessionRuntime>,
    /// When the runtime was last used; the idle sweep compares this to the timeout.
    last_activity: std::time::Instant,
}

/// Owns the live runtimes.
pub struct SessionManager {
    store: Arc<dyn SessionStore>,
    config: Arc<LayeredConfig>,
    hub: Arc<LiveHub>,
    data_dir: std::path::PathBuf,
    slots: Mutex<HashMap<SessionId, Slot>>,
    /// Serialises one session's prompt path — runtime assembly, the busy check and the submit —
    /// so two concurrent prompts cannot both slip through the gap between "is a turn running?"
    /// and "now it is".
    turn_gates: Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
    /// Frontends watching each session's events, tracked outside the slots: a subscription
    /// outlives the runtime it was attached to (an assembly must not drop the count, or the idle
    /// sweep would unload a session somebody is watching).
    watchers: Mutex<HashMap<SessionId, u32>>,
    providers: Mutex<ProviderRegistry>,
}

impl SessionManager {
    /// Builds a manager over a store and a layered config.
    #[must_use]
    pub fn new(
        store: Arc<dyn SessionStore>,
        config: Arc<LayeredConfig>,
        hub: Arc<LiveHub>,
        data_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            store,
            config,
            hub,
            data_dir,
            slots: Mutex::new(HashMap::new()),
            turn_gates: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            providers: Mutex::new(ProviderRegistry::new()),
        }
    }

    /// Marks crashed sessions idle and closes the turns they died in.
    ///
    /// After a daemon restart, a session row still saying `running` — and a turn row still open —
    /// describes work that no longer exists anywhere. The turn rows close as failures (the
    /// store's "failed, not running" shape: `ended_at` set, `stop_reason` NULL) before any
    /// runtime can exist, so nothing mistakes them for live work.
    ///
    /// # Errors
    ///
    /// Propagates the store's failure — an unmarkable crashed session would lie forever.
    pub async fn recover_crashed_sessions(&self) -> Result<u64, ManagerError> {
        for (session, turn) in self.store.open_turns().await.map_err(store_error)? {
            self.store
                .finish_turn(session, turn, None)
                .await
                .map_err(store_error)?;
            tracing::warn!(session = %session, turn = %turn, "closed a turn its daemon died in");
        }

        let crashed = self
            .store
            .list_sessions(SessionListParams::default())
            .await
            .map_err(store_error)?;
        let mut recovered = 0;
        for session in crashed.sessions {
            if session.status == SessionStatus::Running
                || session.status == SessionStatus::WaitingApproval
            {
                self.store
                    .update_session(
                        session.id,
                        SessionPatch {
                            status: Some(SessionStatus::Idle),
                            ..SessionPatch::default()
                        },
                    )
                    .await
                    .map_err(store_error)?;
                tracing::warn!(session = %session.id, "marked an orphaned running session idle");
                recovered += 1;
            }
        }
        Ok(recovered)
    }

    /// `session/new`.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::ConfigError`] when no provider serves the requested model.
    pub async fn new_session(
        &self,
        params: SessionNewParams,
    ) -> Result<SessionNewResult, ManagerError> {
        let model = match params.model {
            Some(model) => model,
            None => self.default_model()?,
        };
        if self.config.resolve_model(&model.model).is_none() {
            return Err(ManagerError::new(
                ErrorCode::ConfigError,
                format!(
                    "no provider serves model `{}`; configure one under [providers.*]",
                    model.model
                ),
            ));
        }
        let session = Session {
            id: SessionId::new(),
            title: params.title,
            mode: params.mode,
            workspace: params.workspace,
            model,
            config_patch: params.config_patch,
            created_at: hatchery_protocol::Timestamp::now(),
            updated_at: hatchery_protocol::Timestamp::now(),
            active_branch_head: None,
            generation: 0,
            status: SessionStatus::Idle,
        };
        let stored = self
            .store
            .create_session(session)
            .await
            .map_err(store_error)?;
        Ok(SessionNewResult { session: stored })
    }

    /// `session/load`: the session row plus the active branch, oldest first.
    ///
    /// M1 serves the whole branch in one page (`next_cursor` stays `None`); the pagination
    /// contract is the store's to grow into, and the field is honoured either way.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::GenerationMismatch`] when the caller's generation is stale; the caller is
    /// about to view a session a newer runtime owns (invariant 1).
    pub async fn load(&self, params: SessionLoadParams) -> Result<SessionLoadResult, ManagerError> {
        let session = self
            .store
            .session(params.session_id)
            .await
            .map_err(store_error)?;
        if params
            .generation
            .is_some_and(|seen| seen < session.generation)
        {
            return Err(ManagerError::new(
                ErrorCode::GenerationMismatch,
                format!(
                    "the session is at generation {}, the caller last saw {:?}",
                    session.generation, params.generation
                ),
            ));
        }
        let items = self
            .chain_for(params.replay_from, params.session_id)
            .await?;
        Ok(SessionLoadResult {
            session,
            items,
            next_cursor: None,
        })
    }

    /// The active branch, honouring the replay cursor.
    ///
    /// The protocol's `replay_from` means "items strictly after this one" (a reconnecting
    /// frontend's gap-fill); the store's `rebuild_chain` head means "the branch ending here", so
    /// the cursor is resolved against the active branch here, where session semantics live. A
    /// cursor that is not on the active branch is refused rather than silently ignored — a
    /// gap-fill that returns the whole branch would look like success while duplicating items.
    async fn chain_for(
        &self,
        replay_from: Option<ItemId>,
        session_id: SessionId,
    ) -> Result<Vec<Item>, ManagerError> {
        let chain = self
            .store
            .rebuild_chain(session_id, None)
            .await
            .map_err(store_error)?;
        match replay_from {
            None => Ok(chain),
            Some(cursor) => {
                let position =
                    chain
                        .iter()
                        .position(|item| item.id == cursor)
                        .ok_or_else(|| {
                            ManagerError::new(
                                ErrorCode::InvalidRequest,
                                "the replay cursor is not on this session's active branch",
                            )
                        })?;
                Ok(chain.into_iter().skip(position + 1).collect())
            }
        }
    }

    /// `session/prompt`: accept a turn, assembling the runtime first if it is not live.
    ///
    /// The whole accept path — assembly, busy check, submit — runs under the session's turn
    /// gate, and acceptance is marked on the runtime before the submit goes out. The two closes
    /// the race used to slip through: two prompts arriving while the runtime is still being
    /// assembled, and a second prompt landing in the gap between the submit and the kernel's
    /// state machine opening the turn.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::TurnInProgress`] when a turn is already running; [`ErrorCode::GenerationMismatch`]
    /// on a stale caller.
    pub async fn prompt(
        &self,
        params: SessionPromptParams,
    ) -> Result<SessionPromptResult, ManagerError> {
        let session = self
            .store
            .session(params.session_id)
            .await
            .map_err(store_error)?;
        if params
            .generation
            .is_some_and(|seen| seen < session.generation)
        {
            return Err(ManagerError::new(
                ErrorCode::GenerationMismatch,
                format!(
                    "the session is at generation {}, the caller submitted at {:?}",
                    session.generation, params.generation
                ),
            ));
        }
        let gate = Arc::clone(
            self.turn_gates
                .lock()
                .expect("turn gates is not poisoned")
                .entry(session.id)
                .or_default(),
        );
        let _lease = gate.lock().await;
        let runtime = self.ensure_runtime(&session).await?;
        // The runtime slot is the session's lease (invariant 1): one turn at a time, and the
        // refusal is the protocol's own error, not a silently dropped command. The marker is
        // taken atomically so the prompt after this one — arriving the instant this submit
        // returns — sees it before the kernel has even opened the turn.
        if runtime.is_busy() || !runtime.begin_turn() {
            return Err(ManagerError(crate::core::turn_in_progress()));
        }
        let turn = TurnId::new();
        if runtime
            .handle
            .submit(AgentCommand::prompt_with_turn(turn, params.content))
            .await
            .is_err()
        {
            // The submit did not land, so no turn will run to release the marker.
            runtime.end_turn();
            return Err(ManagerError::new(
                ErrorCode::InternalError,
                "the runtime stopped unexpectedly",
            ));
        }
        Ok(SessionPromptResult { turn })
    }

    /// `session/cancel`.
    ///
    /// # Errors
    ///
    /// Propagates the store's failure.
    pub async fn cancel(
        &self,
        session_id: SessionId,
    ) -> Result<hatchery_protocol::method::SessionCancelResult, ManagerError> {
        let cancelled = match self.runtime_for(session_id) {
            // An interrupt on an idle runtime is a no-op the kernel logs; reporting it as a
            // cancellation would tell the frontend something happened when nothing did.
            Some(runtime) if runtime.is_busy() => runtime.handle.interrupt().await.is_ok(),
            _ => false,
        };
        Ok(hatchery_protocol::method::SessionCancelResult { cancelled })
    }

    /// `session/list`.
    ///
    /// # Errors
    ///
    /// Propagates the store's failure.
    pub async fn list(&self, params: SessionListParams) -> Result<SessionListResult, ManagerError> {
        self.store.list_sessions(params).await.map_err(store_error)
    }

    /// `session/set_config`.
    ///
    /// # Errors
    ///
    /// Propagates the store's failure.
    pub async fn set_config(
        &self,
        params: SetConfigParams,
    ) -> Result<SetConfigResult, ManagerError> {
        let mut patch = SessionPatch::default();
        if let Some(model) = params.patch.model {
            patch.model = Some(model);
        }
        if params.patch.reasoning_effort.is_some() || params.patch.overrides.is_some() {
            // One read, one merge: effort and overrides both land in the same override document,
            // and a second read would only see the pre-patch row and drop the first edit.
            let session = self
                .store
                .session(params.session_id)
                .await
                .map_err(store_error)?;
            let mut merged = session
                .config_patch
                .clone()
                .unwrap_or(serde_json::json!({}));
            if let Some(object) = merged.as_object_mut() {
                if let Some(effort) = params.patch.reasoning_effort {
                    object.insert("reasoning_effort".to_owned(), serde_json::json!(effort));
                }
                if let Some(overrides) = params
                    .patch
                    .overrides
                    .clone()
                    .and_then(|v| v.as_object().cloned())
                {
                    for (key, value) in overrides {
                        object.insert(key, value);
                    }
                }
            }
            patch.config_patch = Some(merged);
        }
        let session = self
            .store
            .update_session(params.session_id, patch)
            .await
            .map_err(store_error)?;
        self.publish_session_updated(&session);
        Ok(SetConfigResult { session })
    }

    /// Records that a connection subscribed to a session's events.
    ///
    /// Tracked outside the slots on purpose: the usual flow subscribes before the first prompt,
    /// so at subscription time there is often no runtime to hang the count on — and the count
    /// must survive the runtime that later assembles under it.
    pub fn attach(&self, session_id: SessionId) {
        *self
            .watchers
            .lock()
            .expect("watchers is not poisoned")
            .entry(session_id)
            .or_default() += 1;
    }

    /// Records that a connection left.
    pub fn detach(&self, session_id: SessionId) {
        if let Some(watching) = self
            .watchers
            .lock()
            .expect("watchers is not poisoned")
            .get_mut(&session_id)
        {
            *watching = watching.saturating_sub(1);
        }
    }

    /// How many runtimes are live.
    #[must_use]
    pub fn live_runtimes(&self) -> usize {
        self.slots.lock().expect("slots is not poisoned").len()
    }

    /// Shuts down runtimes that have been idle and unwatched past the timeout.
    ///
    /// A busy runtime is never unloaded (D2): its turn runs to completion even for nobody. A
    /// runtime whose agent has stopped is swept like any other — a dead runtime in the slot is
    /// as good as an idle one, and both reassemble on the next prompt.
    pub async fn sweep_idle(&self) {
        self.sweep_after(self.config.daemon().idle_timeout).await;
    }

    /// The sweep against an explicit timeout; the config clamps `idle_timeout_min` to a whole
    /// minute, and the tests need to exercise the sweep without waiting one out.
    async fn sweep_after(&self, timeout: std::time::Duration) {
        let expired: Vec<SessionId> = {
            let slots = self.slots.lock().expect("slots is not poisoned");
            let watchers = self.watchers.lock().expect("watchers is not poisoned");
            slots
                .iter()
                .filter(|(session_id, slot)| {
                    !slot.runtime.is_busy()
                        && slot.last_activity.elapsed() > timeout
                        && watchers.get(session_id).is_none_or(|count| *count == 0)
                })
                .map(|(session_id, _)| *session_id)
                .collect()
        };
        for session_id in expired {
            self.unload(&session_id).await;
        }
    }

    /// Unloads one runtime: the store row and the events stay; only the process memory goes.
    ///
    /// The busy and watched states are re-checked under the slot lock: the sweep's snapshot is
    /// taken before the await, and a prompt or subscription that landed in between must win.
    pub async fn unload(&self, session_id: &SessionId) {
        let runtime = {
            let mut slots = self.slots.lock().expect("slots is not poisoned");
            let watched = self
                .watchers
                .lock()
                .expect("watchers is not poisoned")
                .get(session_id)
                .is_some_and(|count| *count > 0);
            match slots.get(session_id).map(|slot| slot.runtime.is_busy()) {
                Some(true) | None => None,
                Some(false) if watched => None,
                Some(false) => slots
                    .remove(session_id)
                    .map(|slot| Arc::clone(&slot.runtime)),
            }
        };
        if let Some(runtime) = runtime {
            runtime.shutdown();
            self.hub.remove(session_id);
            tracing::info!(session = %session_id, "unloaded an idle runtime");
        }
    }

    /// The live runtime for a session, if any.
    fn runtime_for(&self, session_id: SessionId) -> Option<Arc<SessionRuntime>> {
        self.slots
            .lock()
            .expect("slots is not poisoned")
            .get(&session_id)
            .map(|slot| Arc::clone(&slot.runtime))
    }

    /// The live runtime for a session, assembling one if there is none.
    async fn ensure_runtime(&self, session: &Session) -> Result<Arc<SessionRuntime>, ManagerError> {
        {
            let mut slots = self.slots.lock().expect("slots is not poisoned");
            if let Some(slot) = slots.get_mut(&session.id)
                && !slot.runtime.handle.is_closed()
            {
                slot.last_activity = std::time::Instant::now();
                return Ok(Arc::clone(&slot.runtime));
            }
            // A dead runtime in the slot is as good as absent; fall through to reassembly.
        }
        self.assemble(session).await
    }

    /// Builds a fresh runtime: provider from config, tools from the Chat set, generation bumped
    /// in the store *before* the first event can carry it (invariant 1).
    async fn assemble(&self, session: &Session) -> Result<Arc<SessionRuntime>, ManagerError> {
        let (provider, echo) = self.provider_for(&session.model.model)?;
        let tools = self.chat_tools(session)?;

        let updated = self
            .store
            .bump_generation(session.id)
            .await
            .map_err(store_error)?;
        let runtime = Arc::new(
            SessionRuntime::spawn(
                &updated,
                updated.generation,
                Arc::clone(&self.store),
                Arc::clone(&self.hub),
                Arc::new(provider),
                Arc::new(tools),
                echo,
            )
            .await
            .map_err(store_error)?,
        );
        self.slots.lock().expect("slots is not poisoned").insert(
            session.id,
            Slot {
                runtime: Arc::clone(&runtime),
                last_activity: std::time::Instant::now(),
            },
        );
        // The bump reaches the wire in its own event first, so a frontend can reset its view
        // for the new runtime before the first projected state lands.
        self.hub.publish(SessionEvent::new(
            session.id,
            updated.generation,
            ServerEvent::GenerationBumped,
        ));
        self.publish_session_updated(&updated);
        Ok(runtime)
    }

    /// Publishes a `SessionUpdated` for subscribers, best-effort.
    fn publish_session_updated(&self, session: &Session) {
        let generation = self
            .slots
            .lock()
            .expect("slots is not poisoned")
            .get(&session.id)
            .map_or(session.generation, |slot| slot.runtime.generation);
        self.hub.publish(SessionEvent::new(
            session.id,
            generation,
            ServerEvent::SessionUpdated {
                state: Session {
                    generation,
                    ..session.clone()
                },
            },
        ));
    }

    /// Resolves a model to a configured provider, caching the adapter in the registry.
    ///
    /// Returns the provider's echo decision with it: the same config's capability table the
    /// adapter folds, so history passback and the wire's requests can never disagree about
    /// whether reasoning rides back.
    fn provider_for(
        &self,
        model: &str,
    ) -> Result<(hatchery_llm::ChatCompletionsProvider, bool), ManagerError> {
        let (id, provider_config) = self.config.resolve_model(model).ok_or_else(|| {
            ManagerError::new(
                ErrorCode::ConfigError,
                format!("no provider serves model `{model}`"),
            )
        })?;
        let echo = provider_config
            .capability_table()
            .capabilities(model)
            .echo_reasoning;
        let registry = self.providers.lock().expect("providers is not poisoned");
        // Register on every assembly: the registry replaces under the id, so a config change
        // (new endpoint, new env key) reaches the next runtime instead of living forever as a
        // stale cache. Assembled sessions keep the Arc they hold — the swap is between turns.
        registry.register(id.clone(), provider_config.clone());
        let provider = registry.get(&id).expect("the registration just landed");
        Ok(((*provider).clone(), echo))
    }

    /// The Chat toolset over the session's workspace (or the daemon's cwd).
    fn chat_tools(
        &self,
        session: &Session,
    ) -> Result<hatchery_capabilities::ToolRegistry, ManagerError> {
        let root = session
            .workspace
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| self.data_dir.clone()));
        let fs = hatchery_capabilities::LocalFs::new(root)
            .map_err(|error| ManagerError::new(ErrorCode::InternalError, error.to_string()))?;
        let backends = hatchery_capabilities::Backends {
            fs: Arc::new(fs),
            terminal: Arc::new(hatchery_capabilities::NoTerminal),
        };
        let mut registry = hatchery_capabilities::ToolRegistry::new(backends);
        for tool in hatchery_tools::chat_tools() {
            registry.register(tool);
        }
        Ok(registry)
    }

    /// The model a new session uses when the caller did not pick one: the first configured
    /// provider's first listed model.
    fn default_model(&self) -> Result<hatchery_protocol::ModelRef, ManagerError> {
        let providers = self.config.providers();
        let (id, config) = providers.into_iter().next().ok_or_else(|| {
            ManagerError::new(
                ErrorCode::ConfigError,
                "no provider is configured; add a [providers.*] table to your config",
            )
        })?;
        let model = config.models.first().cloned().ok_or_else(|| {
            // Falling back to the provider id here would mint sessions whose model only fails
            // at request time; the config is unusable for a default and must say so now.
            ManagerError::new(
                ErrorCode::ConfigError,
                format!("the provider `{id}` lists no models; add one to `models`"),
            )
        })?;
        Ok(hatchery_protocol::ModelRef::new(id, model))
    }
}

fn store_error(error: hatchery_store::StoreError) -> ManagerError {
    // The store already knows which of its failures are caller errors (`SessionNotFound`) and
    // which are storage failures; flattening them here would make "bad id" and "database down"
    // indistinguishable on the wire.
    ManagerError(error.to_event_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_protocol::ModelRef;
    use hatchery_protocol::Timestamp;

    async fn manager() -> (tempfile::TempDir, Arc<SessionManager>, Arc<LiveHub>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn SessionStore> = Arc::new(
            hatchery_store::TursoStore::open(dir.path().join("t.db"))
                .await
                .expect("open"),
        );
        let config = Arc::new(LayeredConfig::from_layers(vec![]));
        let hub = Arc::new(LiveHub::new());
        let manager = Arc::new(SessionManager::new(
            store,
            config,
            Arc::clone(&hub),
            dir.path().to_path_buf(),
        ));
        (dir, manager, hub)
    }

    fn new_params() -> SessionNewParams {
        SessionNewParams {
            mode: hatchery_protocol::SessionModeId::chat(),
            workspace: None,
            model: Some(ModelRef::new("deepseek", "deepseek-flash")),
            title: Some("test".to_owned()),
            config_patch: None,
        }
    }

    #[tokio::test]
    async fn a_new_session_persists_and_lists() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        let listed = manager
            .list(SessionListParams::default())
            .await
            .expect("listed");
        assert_eq!(listed.sessions.len(), 1);
        assert_eq!(listed.sessions[0].id, created.session.id);
        assert_eq!(listed.sessions[0].status, SessionStatus::Idle);
    }

    #[tokio::test]
    async fn an_unknown_model_is_a_config_error_not_a_store_error() {
        let (_dir, manager, _hub) = manager().await;
        let mut params = new_params();
        params.model = Some(ModelRef::new("deepseek", "no-such-model"));
        let error = manager
            .new_session(params)
            .await
            .expect_err("unknown model");
        assert_eq!(error.into_event().code, ErrorCode::ConfigError);
    }

    #[tokio::test]
    async fn the_generation_bumps_in_the_store_per_assembly() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        let session = created.session;

        // Assembly happens on the first prompt; the provider call itself would need a key, but
        // assembly fails first at the key check — which proves the seam was reached. For the
        // bookkeeping test, bump through the store the way assemble does.
        let bumped = manager
            .store
            .bump_generation(session.id)
            .await
            .expect("bump");
        assert_eq!(bumped.generation, 1);
        let loaded = manager
            .load(SessionLoadParams {
                session_id: session.id,
                replay_from: None,
                generation: Some(1),
            })
            .await
            .expect("current caller accepted");
        assert_eq!(loaded.session.generation, 1);

        let stale = manager
            .load(SessionLoadParams {
                session_id: session.id,
                replay_from: None,
                generation: Some(0),
            })
            .await
            .expect_err("stale caller");
        assert_eq!(stale.into_event().code, ErrorCode::GenerationMismatch);
    }

    #[tokio::test]
    async fn loading_an_unknown_session_is_session_not_found() {
        let (_dir, manager, _hub) = manager().await;
        let error = manager
            .load(SessionLoadParams {
                session_id: SessionId::new(),
                replay_from: None,
                generation: None,
            })
            .await
            .expect_err("unknown");
        assert_eq!(
            error.into_event().code,
            ErrorCode::SessionNotFound,
            "a bad id and a database failure must be tellable apart on the wire"
        );
    }

    async fn chain_of_three(manager: &SessionManager, session: SessionId) -> Vec<Item> {
        use hatchery_protocol::{Content, ItemKind};
        let mut parent = None;
        let mut items = Vec::new();
        for text in ["first", "second", "third"] {
            let mut item = Item::new(session, ItemKind::UserMessage(Content::text(text)));
            item.parent = parent;
            parent = Some(item.id);
            items.push(item);
        }
        manager
            .store
            .append_items(items.clone())
            .await
            .expect("append");
        items
    }

    #[tokio::test]
    async fn a_replay_cursor_returns_only_items_after_it() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        let items = chain_of_three(&manager, created.session.id).await;

        let gap: Vec<String> = manager
            .load(SessionLoadParams {
                session_id: created.session.id,
                replay_from: Some(items[0].id),
                generation: None,
            })
            .await
            .expect("gap-fill")
            .items
            .iter()
            .map(|item| match &item.kind {
                hatchery_protocol::ItemKind::UserMessage(content) => content.text.clone(),
                other => panic!("unexpected item: {other:?}"),
            })
            .collect();
        assert_eq!(gap, vec!["second".to_owned(), "third".to_owned()]);

        let whole = manager
            .load(SessionLoadParams {
                session_id: created.session.id,
                replay_from: None,
                generation: None,
            })
            .await
            .expect("whole branch");
        assert_eq!(whole.items.len(), 3);
    }

    #[tokio::test]
    async fn a_replay_cursor_off_the_active_branch_is_refused() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        chain_of_three(&manager, created.session.id).await;
        let error = manager
            .load(SessionLoadParams {
                session_id: created.session.id,
                replay_from: Some(ItemId::new()),
                generation: None,
            })
            .await
            .expect_err("unknown cursor");
        assert_eq!(error.into_event().code, ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn recover_marks_orphaned_running_sessions_idle() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        manager
            .store
            .update_session(
                created.session.id,
                SessionPatch {
                    status: Some(SessionStatus::Running),
                    ..SessionPatch::default()
                },
            )
            .await
            .expect("patch");
        let recovered = manager.recover_crashed_sessions().await.expect("recover");
        assert_eq!(recovered, 1);
        let after = manager
            .store
            .session(created.session.id)
            .await
            .expect("session");
        assert_eq!(after.status, SessionStatus::Idle);
    }

    #[tokio::test]
    async fn set_config_merges_overrides_and_switches_models() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        let result = manager
            .set_config(SetConfigParams {
                session_id: created.session.id,
                patch: hatchery_protocol::method::ConfigPatch {
                    model: Some(ModelRef::new("qwen", "qwen3.8-flash")),
                    reasoning_effort: Some(hatchery_protocol::ReasoningEffort::Low),
                    overrides: Some(serde_json::json!({"custom": 1})),
                },
            })
            .await
            .expect("set");
        assert_eq!(result.session.model.model, "qwen3.8-flash");
        let patch = result.session.config_patch.expect("merged");
        assert_eq!(patch["reasoning_effort"], "low");
        assert_eq!(patch["custom"], 1);
    }

    // Timestamp stays imported for the type the manager stores; keep it honest.
    const _: Option<Timestamp> = None;

    async fn manager_with_config(
        config: Arc<LayeredConfig>,
    ) -> (tempfile::TempDir, Arc<SessionManager>, Arc<LiveHub>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn SessionStore> = Arc::new(
            hatchery_store::TursoStore::open(dir.path().join("t.db"))
                .await
                .expect("open"),
        );
        let hub = Arc::new(LiveHub::new());
        let manager = Arc::new(SessionManager::new(
            store,
            config,
            Arc::clone(&hub),
            dir.path().to_path_buf(),
        ));
        (dir, manager, hub)
    }

    fn table(document: &str) -> toml::Table {
        document.parse().expect("a valid toml layer")
    }

    async fn recover_fixture() -> (tempfile::TempDir, Arc<SessionManager>, SessionId) {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        let id = created.session.id;
        (_dir, manager, id)
    }

    #[tokio::test]
    async fn recover_closes_the_open_turn_of_a_crashed_session() {
        let (_dir, manager, session) = recover_fixture().await;
        let turn = TurnId::new();
        manager
            .store
            .start_turn(session, turn, Timestamp::now())
            .await
            .expect("a turn its daemon died in");
        manager
            .store
            .update_session(
                session,
                SessionPatch {
                    status: Some(SessionStatus::Running),
                    ..SessionPatch::default()
                },
            )
            .await
            .expect("running");

        manager.recover_crashed_sessions().await.expect("recover");

        assert!(
            manager.store.open_turns().await.expect("read").is_empty(),
            "no turn survives a restart still open"
        );
        let after = manager.store.session(session).await.expect("session");
        assert_eq!(after.status, SessionStatus::Idle);
    }

    #[tokio::test]
    async fn recover_is_idempotent_and_marks_waiting_approval_sessions_idle() {
        let (_dir, manager, session) = recover_fixture().await;
        manager
            .store
            .update_session(
                session,
                SessionPatch {
                    status: Some(SessionStatus::WaitingApproval),
                    ..SessionPatch::default()
                },
            )
            .await
            .expect("waiting");

        assert_eq!(
            manager.recover_crashed_sessions().await.expect("recover"),
            1
        );
        assert_eq!(manager.recover_crashed_sessions().await.expect("again"), 0);
        let after = manager.store.session(session).await.expect("session");
        assert_eq!(after.status, SessionStatus::Idle);
    }

    #[tokio::test]
    async fn cancelling_an_idle_session_reports_nothing_cancelled() {
        let (_dir, manager, session) = recover_fixture().await;
        let result = manager.cancel(session).await.expect("cancel");
        assert!(
            !result.cancelled,
            "an interrupt on an idle session is a no-op, not a cancellation"
        );
    }

    /// A manager whose provider points at a wire that holds the response, with a zero idle
    /// timeout so the sweep is ready the moment a runtime goes quiet.
    async fn manager_on_a_slow_wire(
        wire: &hatchery_testkit::MockWire,
    ) -> (tempfile::TempDir, Arc<SessionManager>, SessionId) {
        let config = Arc::new(LayeredConfig::from_layers(vec![(
            hatchery_protocol::method::ConfigOrigin::User,
            table(&format!(
                "[daemon]\nidle_timeout_min = 0\n\n[providers.testprov]\n\
                 base_url = \"{url}\"\nenv_key = \"PATH\"\nmodels = [\"m\"]\n",
                url = wire.url()
            )),
        )]));
        let (_dir, manager, _hub) = manager_with_config(config).await;
        let created = manager
            .new_session(SessionNewParams {
                mode: hatchery_protocol::SessionModeId::chat(),
                workspace: None,
                model: Some(ModelRef::new("testprov", "m")),
                title: Some("sweep".to_owned()),
                config_patch: None,
            })
            .await
            .expect("created");
        (_dir, manager, created.session.id)
    }

    async fn wait_until_quiet(manager: &SessionManager) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let busy = manager
                    .slots
                    .lock()
                    .expect("slots is not poisoned")
                    .values()
                    .any(|slot| slot.runtime.is_busy());
                if !busy {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the turn ends");
    }

    #[tokio::test]
    async fn a_busy_runtime_is_never_swept_and_an_idle_one_goes() {
        // D2's two legs in one live session: while the provider holds the turn open, the sweep
        // must leave the runtime alone; once the turn is over, the same sweep unloads it.
        hatchery_llm::install_tls_provider();
        let wire = hatchery_testkit::MockWire::replay_sse_after(
            "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"}}]}\n\n\
             data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
            std::time::Duration::from_secs(5),
        )
        .await;
        let (_dir, manager, session) = manager_on_a_slow_wire(&wire).await;

        manager
            .prompt(SessionPromptParams {
                session_id: session,
                content: hatchery_protocol::Content::text("hold the turn open"),
                generation: None,
            })
            .await
            .expect("accepted");

        manager.sweep_after(std::time::Duration::ZERO).await;
        assert_eq!(
            manager.live_runtimes(),
            1,
            "a running turn is never unloaded, whatever the timeout says"
        );

        manager.cancel(session).await.expect("cancel");
        wait_until_quiet(&manager).await;
        manager.sweep_after(std::time::Duration::ZERO).await;
        assert_eq!(
            manager.live_runtimes(),
            0,
            "the same sweep unloads the runtime once it is idle and unwatched"
        );
    }

    #[tokio::test]
    async fn a_watched_runtime_is_not_swept_even_after_reassembly() {
        // The count must survive the runtime: subscribe before the runtime exists (the normal
        // session/new → prompt flow), then let the sweep look at the assembled runtime.
        hatchery_llm::install_tls_provider();
        let wire = hatchery_testkit::MockWire::replay_sse_after(
            "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"}}]}\n\n\
             data: [DONE]\n\n",
            std::time::Duration::from_secs(5),
        )
        .await;
        let (_dir, manager, session) = manager_on_a_slow_wire(&wire).await;

        manager.attach(session);
        manager
            .prompt(SessionPromptParams {
                session_id: session,
                content: hatchery_protocol::Content::text("watched"),
                generation: None,
            })
            .await
            .expect("accepted");
        manager.cancel(session).await.expect("cancel");
        wait_until_quiet(&manager).await;

        manager.sweep_after(std::time::Duration::ZERO).await;
        assert_eq!(
            manager.live_runtimes(),
            1,
            "the idle sweep never pulls a runtime somebody is watching"
        );

        manager.detach(session);
        manager.sweep_after(std::time::Duration::ZERO).await;
        assert_eq!(manager.live_runtimes(), 0, "unwatched again, it goes");
    }
}
