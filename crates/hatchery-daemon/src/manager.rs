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

use hatchery_kernel::{AgentCommand, ChatOptions};
use hatchery_llm::{ProviderConfig, ProviderRegistry};
use hatchery_protocol::method::{
    BranchDeleteParams, BranchDeleteResult, BranchListParams, BranchListResult, BranchNode,
    BranchSwitchParams, BranchSwitchResult, EditItemParams, EditItemResult, ExportJsonlParams,
    ExportJsonlResult, RewindParams, RewindReport, RewindResult, SessionDeleteParams,
    SessionDeleteResult, SessionListParams, SessionListResult, SessionLoadParams,
    SessionLoadResult, SessionNewParams, SessionNewResult, SessionPromptParams,
    SessionPromptResult, SessionRenameParams, SessionRenameResult, SetConfigParams,
    SetConfigResult, SetModeParams, SetModeResult,
};
use hatchery_protocol::{
    ErrorCode, EventError, Item, ItemId, ItemKind, ReasoningEffort, RewindScope, ServerEvent,
    Session, SessionEvent, SessionId, SessionModeId, SessionPatch, SessionStatus, TurnId,
};
use hatchery_store::SessionStore;

use crate::config::LayeredConfig;
use crate::hub::LiveHub;
use crate::prompt::AssembledSection;
use crate::runtime::{RuntimeParts, SessionRuntime};

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

/// What resolving a model name gives: the adapter to talk to, which configured provider it came
/// from, and whether that provider echoes reasoning back into requests.
struct Resolved {
    provider: hatchery_llm::ChatCompletionsProvider,
    provider_id: String,
    echo_reasoning: bool,
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
    /// Where the per-section prompt overrides are read from; `None` reads none.
    ///
    /// The production entry passes [`crate::prompt::default_prompts_dir`]. Tests pass `None`
    /// rather than inheriting whatever the machine running them happens to have configured —
    /// the same reason `LayeredConfig` is built from injected layers in a test.
    prompts_dir: Option<std::path::PathBuf>,
    /// Shadow repositories and the budget that guards them.
    ///
    /// Built from `data_dir` rather than injected: the location is not a choice a caller should be
    /// able to make per session, and a test isolates it the way it isolates everything else, by
    /// pointing `data_dir` at a temp dir.
    checkpoints: Arc<crate::checkpoints::Checkpoints>,
    /// Requests waiting for `approval/respond`.
    pending: Arc<crate::approvals::PendingApprovals>,
}

impl SessionManager {
    /// Builds a manager over a store and a layered config.
    #[must_use]
    pub fn new(
        store: Arc<dyn SessionStore>,
        config: Arc<LayeredConfig>,
        hub: Arc<LiveHub>,
        data_dir: std::path::PathBuf,
        prompts_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let checkpoints = Arc::new(crate::checkpoints::Checkpoints::new(
            &data_dir,
            &config.checkpoints(),
            Arc::clone(&store),
        ));
        Self {
            store,
            config,
            hub,
            data_dir,
            slots: Mutex::new(HashMap::new()),
            turn_gates: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            providers: Mutex::new(ProviderRegistry::new()),
            prompts_dir,
            checkpoints,
            pending: Arc::new(crate::approvals::PendingApprovals::new()),
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
            pending_approvals: self.pending.for_session(params.session_id),
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
        // Resolved before the gate and before any assembly: a model nothing serves is a
        // configuration error the caller should hear about, and it must not cost a live runtime.
        let options = self.turn_options(&session)?;
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
            .submit(AgentCommand::prompt_with_options(
                turn,
                params.content,
                options,
            ))
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

    /// `session/set_mode`: change the mode and rebuild the runtime around it.
    ///
    /// The three things a mode stands for — the tool catalogue, the approval policy and the
    /// prompt variant — all live in the runtime (ADR-0005), so writing the row without swapping
    /// the runtime would leave the model calling the old catalogue under the new label. The
    /// switch therefore takes the turn gate, refuses while a turn is in flight, and reassembles
    /// once the row is written. Frontends stay subscribed: the hub outlives the runtime, and
    /// `GenerationBumped` is how they learn to reset their view.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::TurnInProgress`] while a turn is running — ADR-0005's "in-flight tool calls
    /// finish or are cancelled first" cannot be honoured by swapping tools mid-call;
    /// [`ErrorCode::InvalidRequest`] for Code without a workspace.
    pub async fn set_mode(&self, params: SetModeParams) -> Result<SetModeResult, ManagerError> {
        let session = self
            .store
            .session(params.session_id)
            .await
            .map_err(store_error)?;
        if session.mode == params.mode {
            return Ok(SetModeResult {
                session: self.session_for_wire(session),
            });
        }
        if params.mode.as_str() == SessionModeId::CODE && session.workspace.is_none() {
            return Err(ManagerError::new(
                ErrorCode::InvalidRequest,
                "code mode requires a workspace: there is nowhere to write",
            ));
        }
        let _lease = self.gate_for(session.id).await;
        // Re-read under the gate: a prompt may have accepted a turn while this call waited, and
        // the check above is only as fresh as the read that produced it.
        let session = self
            .store
            .session(params.session_id)
            .await
            .map_err(store_error)?;
        self.ensure_idle(session.id)?;
        let updated = self
            .store
            .update_session(
                session.id,
                SessionPatch {
                    mode: Some(params.mode.clone()),
                    ..SessionPatch::default()
                },
            )
            .await
            .map_err(store_error)?;
        if let Some(runtime) = self.take_runtime(session.id) {
            let live = !runtime.handle.is_closed();
            runtime.shutdown();
            if live {
                self.assemble(&updated).await?;
            }
        }
        let current = self.session_for_wire(updated);
        self.hub.publish(SessionEvent::new(
            current.id,
            current.generation,
            ServerEvent::ModeSwitched {
                from: session.mode,
                to: params.mode,
            },
        ));
        self.publish_session_updated(&current);
        Ok(SetModeResult { session: current })
    }

    /// `session/edit_item`: replace a message, forking the branch.
    ///
    /// The fork is the store's: the replacement takes the original's parent, so the old branch
    /// stays reachable and a mistaken edit is undone by switching back (ADR-0003). When a rewind
    /// scope is given, the code leg runs **before** the fork — the same ordering rule as
    /// `session/rewind`, for the same reason: a failed restore must not leave the history moved.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::TurnInProgress`] while a turn is running; the store's own errors for an item
    /// that is not editable or belongs to another session.
    pub async fn edit_item(&self, params: EditItemParams) -> Result<EditItemResult, ManagerError> {
        let session_id = self
            .store
            .session_of_item(params.item_id)
            .await
            .map_err(store_error)?;
        let _lease = self.gate_for(session_id).await;
        self.ensure_idle(session_id)?;
        let session = self.store.session(session_id).await.map_err(store_error)?;
        if matches!(
            params.rewind_scope,
            Some(RewindScope::Code | RewindScope::Both)
        ) {
            self.rewind_code(&session, params.item_id, false).await?;
        }
        let forked = self
            .store
            .edit_fork(session_id, params.item_id, params.new_content)
            .await
            .map_err(store_error)?;
        let session = self.store.session(session_id).await.map_err(store_error)?;
        let session = self.session_for_wire(session);
        self.publish_session_updated(&session);
        Ok(EditItemResult {
            branch_head: forked.id,
            session,
        })
    }

    /// `session/branch/list`: the whole item tree, with the active chain marked.
    ///
    /// # Errors
    ///
    /// Propagates the store's failure.
    pub async fn branch_list(
        &self,
        params: BranchListParams,
    ) -> Result<BranchListResult, ManagerError> {
        let tree = self
            .store
            .branch_tree(params.session_id)
            .await
            .map_err(store_error)?;
        let nodes = tree
            .nodes
            .into_iter()
            .map(|node| BranchNode {
                id: node.row.id,
                parent: node.row.parent,
                turn: node.row.turn,
                kind: node.row.kind,
                created_at: node.created_at,
                active: node.active,
            })
            .collect();
        Ok(BranchListResult { nodes })
    }

    /// `session/branch/switch`: point the active branch at an existing item.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::TurnInProgress`] while a turn is running; the store refuses an item from
    /// another session.
    pub async fn branch_switch(
        &self,
        params: BranchSwitchParams,
    ) -> Result<BranchSwitchResult, ManagerError> {
        let _lease = self.gate_for(params.session_id).await;
        self.ensure_idle(params.session_id)?;
        let session = self
            .store
            .switch_branch(params.session_id, params.head)
            .await
            .map_err(store_error)?;
        let session = self.session_for_wire(session);
        self.publish_session_updated(&session);
        Ok(BranchSwitchResult { session })
    }

    /// `session/branch/delete`: remove a subtree.
    ///
    /// The store refuses a subtree that contains the active head, so an accidental delete cannot
    /// leave the session pointing at nothing. `confirm` is the protocol's second confirmation:
    /// this is the one session operation that cannot be undone, and a mistyped request should not
    /// be enough.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] without `confirm`; [`ErrorCode::TurnInProgress`] while a turn
    /// is running.
    pub async fn branch_delete(
        &self,
        params: BranchDeleteParams,
    ) -> Result<BranchDeleteResult, ManagerError> {
        if !params.confirm {
            return Err(ManagerError::new(
                ErrorCode::InvalidRequest,
                "deleting a branch is not recoverable; repeat the call with `confirm: true`",
            ));
        }
        let _lease = self.gate_for(params.session_id).await;
        self.ensure_idle(params.session_id)?;
        let deleted_items = self
            .store
            .delete_branch(params.session_id, params.head)
            .await
            .map_err(store_error)?;
        Ok(BranchDeleteResult { deleted_items })
    }

    /// `session/rewind`: move history, code, or both back to an item.
    ///
    /// The ordering is the load-bearing part: for [`RewindScope::Both`] the code is restored
    /// **first**, and the head moves only after that succeeded. The other order would leave a
    /// history that claims a state the working tree is not in, and the next turn would build on
    /// a lie (storage.md §5).
    ///
    /// The code leg locates its checkpoint by walking the item chain, not the `checkpoints`
    /// table: the first `Checkpoint` item *after* the target carries the state before the first
    /// write that happened after it — which is exactly the state the target describes. No such
    /// item means nothing was written after the target, which makes the code leg a no-op, not an
    /// error.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] when the target is not on the active branch, or when a code
    /// scope was asked for on a session without a workspace; [`ErrorCode::TurnInProgress`] while
    /// a turn is running; a restore failure, which leaves the history untouched.
    pub async fn rewind(&self, params: RewindParams) -> Result<RewindResult, ManagerError> {
        if params.purge_untracked {
            // capabilities.md §2 requires an approval plus the manifest `untracked()` lists
            // before a purge may delete anything, and the protocol has no way to fetch that
            // manifest yet. Deleting without the documented consent would be the one failure
            // mode rewind must never have, so the honest answer is a refusal.
            return Err(ManagerError::new(
                ErrorCode::InvalidRequest,
                "purging untracked files needs the approval and manifest flow; \
                 this build refuses rather than deleting them unlisted",
            ));
        }
        let _lease = self.gate_for(params.session_id).await;
        self.ensure_idle(params.session_id)?;
        let session = self
            .store
            .session(params.session_id)
            .await
            .map_err(store_error)?;
        let chain = self
            .store
            .rebuild_chain(session.id, None)
            .await
            .map_err(store_error)?;
        if !chain.iter().any(|item| item.id == params.target_item) {
            return Err(ManagerError::new(
                ErrorCode::InvalidRequest,
                "the rewind target is not on this session's active branch",
            ));
        }
        let mut report = RewindReport::default();
        if matches!(params.scope, RewindScope::Code | RewindScope::Both)
            && let Some(restored) = self
                .rewind_code(&session, params.target_item, params.purge_untracked)
                .await?
        {
            report.rolled_back = restored.rolled_back;
            report.purged = restored.purged;
        }
        if matches!(params.scope, RewindScope::Conversation | RewindScope::Both) {
            let moved = self
                .store
                .switch_branch(session.id, params.target_item)
                .await
                .map_err(store_error)?;
            let moved = self.session_for_wire(moved);
            self.publish_session_updated(&moved);
            return Ok(RewindResult {
                session: moved,
                report,
            });
        }
        Ok(RewindResult {
            session: self.session_for_wire(session),
            report,
        })
    }

    /// The code half of a rewind, shared with `session/edit_item`.
    ///
    /// `None` means there is nothing to restore: no checkpoint follows the target, so nothing was
    /// written after it and the working tree already is the state it describes. That is a no-op,
    /// not an error — a broken checkpoint would report itself from the restore instead.
    /// Callers hold the turn gate.
    async fn rewind_code(
        &self,
        session: &Session,
        target_item: ItemId,
        purge_untracked: bool,
    ) -> Result<Option<hatchery_capabilities::RestoreReport>, ManagerError> {
        let workspace = session.workspace.as_deref().ok_or_else(|| {
            ManagerError::new(
                ErrorCode::InvalidRequest,
                "a code rewind requires a workspace; this session has none",
            )
        })?;
        let chain = self
            .store
            .rebuild_chain(session.id, None)
            .await
            .map_err(store_error)?;
        let position = chain
            .iter()
            .position(|item| item.id == target_item)
            .ok_or_else(|| {
                ManagerError::new(
                    ErrorCode::InvalidRequest,
                    "the rewind target is not on this session's active branch",
                )
            })?;
        let commit = chain[position + 1..]
            .iter()
            .find_map(|item| match &item.kind {
                ItemKind::Checkpoint(checkpoint) => Some(checkpoint.commit_id.clone()),
                _ => None,
            });
        let Some(commit) = commit else {
            return Ok(None);
        };
        self.checkpoints
            .restore(
                session.id,
                workspace,
                &commit,
                hatchery_capabilities::RestoreOptions { purge_untracked },
            )
            .await
            .map(Some)
            .map_err(|error| ManagerError::new(ErrorCode::InternalError, error.to_string()))
    }

    /// `session/delete`: remove the session, its items and its live runtime.
    ///
    /// The runtime is torn down first and without the idle sweep's watcher veto: deleting a
    /// session somebody is watching is the point of the method, and leaving an agent running
    /// against rows that no longer exist would only produce store errors.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::TurnInProgress`] while a turn is running — a turn's items would be written
    /// against a session row that is being deleted.
    pub async fn delete_session(
        &self,
        params: SessionDeleteParams,
    ) -> Result<SessionDeleteResult, ManagerError> {
        let _lease = self.gate_for(params.session_id).await;
        self.ensure_idle(params.session_id)?;
        if let Some(runtime) = self.take_runtime(params.session_id) {
            runtime.shutdown();
        }
        let deleted_items = self
            .store
            .delete_session(params.session_id)
            .await
            .map_err(store_error)?;
        self.hub.remove(&params.session_id);
        self.watchers
            .lock()
            .expect("watchers is not poisoned")
            .remove(&params.session_id);
        Ok(SessionDeleteResult { deleted_items })
    }

    /// `session/rename`: set or clear the title.
    ///
    /// # Errors
    ///
    /// Propagates the store's failure.
    pub async fn rename(
        &self,
        params: SessionRenameParams,
    ) -> Result<SessionRenameResult, ManagerError> {
        let session = self
            .store
            .update_session(
                params.session_id,
                SessionPatch {
                    title: Some(params.title),
                    ..SessionPatch::default()
                },
            )
            .await
            .map_err(store_error)?;
        let session = self.session_for_wire(session);
        self.publish_session_updated(&session);
        Ok(SessionRenameResult { session })
    }

    /// `store/export_jsonl`: write a session out, refusing to overwrite.
    ///
    /// # Errors
    ///
    /// The store's failure for an existing file or an unwritable path.
    pub async fn export(
        &self,
        params: ExportJsonlParams,
    ) -> Result<ExportJsonlResult, ManagerError> {
        let lines = self
            .store
            .export_jsonl(params.session_id, params.path.clone(), params.all_branches)
            .await
            .map_err(store_error)?;
        Ok(ExportJsonlResult {
            path: params.path,
            lines,
        })
    }

    /// The per-session turn gate, cloned out of the map.
    async fn gate_for(&self, session_id: SessionId) -> tokio::sync::OwnedMutexGuard<()> {
        let gate = Arc::clone(
            self.turn_gates
                .lock()
                .expect("turn gates is not poisoned")
                .entry(session_id)
                .or_default(),
        );
        gate.lock_owned().await
    }

    /// Refuses a session operation while a turn is in flight.
    ///
    /// The answer is the protocol's `TurnInProgress` — the same refusal a second prompt gets —
    /// rather than a queue: queueing would apply the operation at an unspecified later moment,
    /// and the caller could not tell which state it landed on.
    fn ensure_idle(&self, session_id: SessionId) -> Result<(), ManagerError> {
        match self.runtime_for(session_id) {
            Some(runtime) if runtime.is_busy() => {
                Err(ManagerError(crate::core::turn_in_progress()))
            }
            _ => Ok(()),
        }
    }

    /// Removes a runtime from its slot, if there is one.
    fn take_runtime(&self, session_id: SessionId) -> Option<Arc<SessionRuntime>> {
        self.slots
            .lock()
            .expect("slots is not poisoned")
            .remove(&session_id)
            .map(|slot| slot.runtime)
    }

    /// The session with the generation the wire should carry.
    ///
    /// Assembly bumps the store's generation after a caller has already read the row, so a reply
    /// built from that read would advertise a generation the very next event contradicts.
    fn session_for_wire(&self, session: Session) -> Session {
        let generation = self
            .slots
            .lock()
            .expect("slots is not poisoned")
            .get(&session.id)
            .map_or(session.generation, |slot| slot.runtime.generation);
        Session {
            generation,
            ..session
        }
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
        let wanted = self.provider_id_for(&session.model.model);
        {
            let mut slots = self.slots.lock().expect("slots is not poisoned");
            if let Some(slot) = slots.get_mut(&session.id) {
                let live = !slot.runtime.handle.is_closed();
                // `/model` can cross providers, and the adapter, the capability table and the
                // reasoning-echo flag are all per provider, so a session that moved needs a new
                // assembly (D19). Not while a turn is running, though: tearing a runtime down to
                // honour a model switch would break the promise that a turn nobody is watching
                // still runs to completion (D2). Such a prompt is refused as `TurnInProgress`
                // instead, and the switch lands on the one after it.
                let moved = wanted.is_some_and(|wanted| wanted != slot.runtime.provider_id);
                if live && (!moved || slot.runtime.is_busy()) {
                    slot.last_activity = std::time::Instant::now();
                    return Ok(Arc::clone(&slot.runtime));
                }
                if live {
                    slot.runtime.shutdown();
                }
                // A dead runtime in the slot is as good as absent; fall through to reassembly.
            }
        }
        self.assemble(session).await
    }

    /// Which configured provider serves a model right now, if any.
    fn provider_id_for(&self, model: &str) -> Option<String> {
        self.config.resolve_model(model).map(|(id, _)| id)
    }

    /// The knobs for one turn, resolved at submit time (D19).
    ///
    /// Model and effort follow the turn rather than the assembly, because the alternative is not
    /// available: `unload` refuses a session somebody is watching, and reassembling bumps the
    /// generation under the frontend that just asked. The prompt is the deliberate exception and
    /// stays frozen for the runtime's life (D15) — it is a prefix-cache asset, and nothing a user
    /// changes mid-session is supposed to move it.
    ///
    /// # Errors
    ///
    /// `ConfigError` when no configured provider serves the session's model. Failing here rather
    /// than at assembly leaves a live runtime alone: a bad `/model` must not tear down the
    /// session somebody is watching.
    fn turn_options(&self, session: &Session) -> Result<ChatOptions, ManagerError> {
        let (_, provider) = self
            .config
            .resolve_model(&session.model.model)
            .ok_or_else(|| {
                ManagerError::new(
                    ErrorCode::ConfigError,
                    format!("no provider serves model `{}`", session.model.model),
                )
            })?;
        let mut options = ChatOptions::new(session.model.model.clone());
        options.reasoning_effort = Self::effort_for(session, &provider);
        Ok(options)
    }

    /// Effort for one turn: the session's own patch — what `/effort` writes — over the provider's
    /// configured default. A patch that does not parse is a warning and falls through to the
    /// default, rather than being silently honoured as "no preference": the value came from a
    /// caller and a typo in it is worth saying out loud.
    fn effort_for(session: &Session, provider: &ProviderConfig) -> Option<ReasoningEffort> {
        if let Some(value) = session
            .config_patch
            .as_ref()
            .and_then(|patch| patch.get("reasoning_effort"))
        {
            match serde_json::from_value::<ReasoningEffort>(value.clone()) {
                Ok(effort) => return Some(effort),
                Err(error) => tracing::warn!(
                    %error,
                    session = %session.id,
                    "the session's reasoning_effort does not parse; using the provider's default"
                ),
            }
        }
        provider.reasoning.reasoning_effort
    }

    /// Builds a fresh runtime: provider from config, tools from the Chat set, generation bumped
    /// in the store *before* the first event can carry it (invariant 1).
    async fn assemble(&self, session: &Session) -> Result<Arc<SessionRuntime>, ManagerError> {
        let Resolved {
            provider,
            provider_id,
            echo_reasoning,
        } = self.provider_for(&session.model.model)?;
        let tools = self.tools_for(session).await?;
        // Rendered here and frozen for the runtime's life (D15): re-rendering per turn would move
        // the `environment` section's date and cwd under the provider's prefix cache, which is the
        // stability the rest of the request is built to preserve. Freezing is also what the model
        // and the effort already do — nothing reassembles a live runtime on a config change (the
        // only unload is the idle sweep), so all three land at the next assembly. `prompt/render`
        // reports the frozen text rather than a fresh render, so "why did the model see this"
        // stays answerable; whether a config change *should* reassemble is daemon.md's open
        // question 5.
        let prompt = self.render_prompt(
            &session.mode,
            &Self::tool_names_for(&session.mode),
            session.workspace.as_deref(),
        );

        let updated = self
            .store
            .bump_generation(session.id)
            .await
            .map_err(store_error)?;
        let runtime = Arc::new(
            SessionRuntime::spawn(
                &updated,
                updated.generation,
                RuntimeParts {
                    store: Arc::clone(&self.store),
                    hub: Arc::clone(&self.hub),
                    provider: Arc::new(provider),
                    provider_id,
                    tools,
                    echo_reasoning,
                    prompt,
                    approvals: Arc::clone(&self.pending),
                    approval_timeout: self.config.approval().timeout,
                },
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
    fn provider_for(&self, model: &str) -> Result<Resolved, ManagerError> {
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
        Ok(Resolved {
            provider: (*provider).clone(),
            provider_id: id,
            echo_reasoning: echo,
        })
    }

    /// Which built-in tools a mode turns on (ADR-0005).
    ///
    /// An unknown mode falls back to Chat's read-only set rather than to everything: a mode the
    /// daemon has never heard of must not quietly gain write tools. Custom modes need the mode
    /// registry Phase 3 adds; until then this is the safe direction for a typo.
    fn policy_for(mode: &hatchery_protocol::SessionModeId) -> hatchery_capabilities::ToolPolicy {
        match mode.as_str() {
            hatchery_protocol::SessionModeId::CODE => hatchery_capabilities::ToolPolicy::all(),
            hatchery_protocol::SessionModeId::CHAT => {
                hatchery_capabilities::ToolPolicy::including(["read_file", "glob", "grep"])
            }
            other => {
                tracing::warn!(mode = other, "unknown mode; using the Chat tool set");
                hatchery_capabilities::ToolPolicy::including(["read_file", "glob", "grep"])
            }
        }
    }

    /// The tool names a mode would advertise, without building a registry.
    ///
    /// `prompt/render` needs these for a bare mode, and the tool-discipline section needs them at
    /// assembly; both must derive from the same policy as the registry, or the prompt would
    /// describe a catalogue the model cannot call.
    #[must_use]
    pub fn tool_names_for(mode: &hatchery_protocol::SessionModeId) -> Vec<String> {
        let policy = Self::policy_for(mode);
        hatchery_tools::code_tools()
            .into_iter()
            .map(|tool| tool.def().name)
            .filter(|name| policy.allows(name))
            .collect()
    }

    /// The tool host over the session's workspace (or the daemon's cwd).
    ///
    /// One registry per session, wrapped in [`crate::approvals::GatedTools`] so persisted and
    /// configured rules answer before the kernel asks. Code mode requires a workspace: the whole
    /// point of a write tool is a place to write, and a Code session without one would silently
    /// write into the daemon's cwd.
    ///
    /// # Errors
    ///
    /// Propagates a backend or checkpoint failure. Failing the assembly rather than degrading to
    /// "no checkpoints" is deliberate: a session whose writes cannot be undone must not look like
    /// one that can, and the alternative discovers this on the user's first `write_file`.
    async fn tools_for(
        &self,
        session: &Session,
    ) -> Result<Arc<dyn hatchery_kernel::ToolHost>, ManagerError> {
        if session.mode.as_str() == hatchery_protocol::SessionModeId::CODE
            && session.workspace.is_none()
        {
            return Err(ManagerError::new(
                ErrorCode::InvalidRequest,
                "code mode requires a workspace: there is nowhere to write",
            ));
        }
        let root = session
            .workspace
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| self.data_dir.clone()));
        let fs = hatchery_capabilities::LocalFs::new(root)
            .map_err(|error| ManagerError::new(ErrorCode::InternalError, error.to_string()))?;
        let checkpointer = match &session.workspace {
            Some(workspace) => Some(
                self.checkpoints
                    .checkpointer_for(workspace)
                    .await
                    .map_err(|error| {
                        ManagerError::new(ErrorCode::InternalError, error.to_string())
                    })?,
            ),
            None => None,
        };
        let backends = hatchery_capabilities::Backends {
            fs: Arc::new(fs),
            terminal: Arc::new(hatchery_capabilities::NoTerminal),
            checkpointer,
        };
        let policy = Self::policy_for(&session.mode);
        let mut registry = hatchery_capabilities::ToolRegistry::new(backends);
        for tool in hatchery_tools::code_tools() {
            if policy.allows(&tool.def().name) {
                registry.register(tool);
            }
        }
        let rules = Arc::new(crate::approvals::StoreRules::new(
            Arc::clone(&self.store),
            session.workspace.clone(),
        ));
        Ok(Arc::new(crate::approvals::GatedTools::new(
            Arc::new(registry),
            rules,
            self.config.approval(),
        )))
    }

    /// The checkpoint facilities: shadow repositories, budgets and the orphan sweep.
    #[must_use]
    pub fn checkpoints(&self) -> &Arc<crate::checkpoints::Checkpoints> {
        &self.checkpoints
    }

    /// Answers a pending approval request.
    ///
    /// `false` means no such request is waiting — already answered, timed out, or the turn was
    /// cancelled. Idempotent by design: two frontends may race to answer the same prompt.
    pub fn respond_approval(
        &self,
        request_id: hatchery_protocol::ApprovalId,
        option: hatchery_protocol::ApprovalOption,
    ) -> bool {
        self.pending.respond(request_id, option)
    }

    /// The prompt for a mode, over a workspace, with the tool catalogue it will advertise.
    ///
    /// The manager is the assembler: it owns the config layer that supplies `response_language`,
    /// the override directory and the workspace's project context. `prompt/render` calls this too,
    /// so the transparency answer and the text a request carries cannot drift.
    #[must_use]
    pub fn render_prompt(
        &self,
        mode: &hatchery_protocol::SessionModeId,
        tool_names: &[String],
        workspace: Option<&std::path::Path>,
    ) -> Vec<AssembledSection> {
        let overrides = self.config.ui();
        let env = crate::prompt::environment(workspace, overrides.response_language.as_deref());
        crate::prompt::render(mode, tool_names, &env, self.prompts_dir.as_ref())
    }

    /// The prompt a *live* runtime was assembled with, if this session has one.
    ///
    /// `prompt/render` prefers this over a fresh render: the question it answers is "why did the
    /// model see this", and a fresh render may legitimately differ from the frozen one — the date
    /// moved, or an override file changed since the assembly. A runtime whose agent has stopped
    /// counts as absent, because the next prompt reassembles and renders again.
    #[must_use]
    pub fn rendered_prompt(&self, session: SessionId) -> Option<Vec<AssembledSection>> {
        self.slots
            .lock()
            .expect("slots is not poisoned")
            .get(&session)
            .filter(|slot| !slot.runtime.handle.is_closed())
            .map(|slot| slot.runtime.prompt.clone())
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
            None,
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
            None,
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

    async fn session_in_workspace(
        manager: &SessionManager,
        workspace: std::path::PathBuf,
    ) -> Session {
        let mut params = new_params();
        params.workspace = Some(workspace);
        manager.new_session(params).await.expect("created").session
    }

    /// Appends a Checkpoint item carrying `commit_id`, which is how a rewind finds a restore
    /// target (storage.md §5). The commit itself need not exist for the walk to find it.
    async fn append_checkpoint(
        manager: &SessionManager,
        session: SessionId,
        parent: ItemId,
        commit_id: &str,
    ) -> Item {
        let mut item = Item::new(
            session,
            ItemKind::Checkpoint(hatchery_protocol::Checkpoint {
                commit_id: commit_id.to_owned(),
                kind: hatchery_protocol::CheckpointKind::PreWrite,
            }),
        );
        item.parent = Some(parent);
        manager
            .store
            .append_item(item.clone())
            .await
            .expect("append");
        item
    }

    fn rewind_params(
        session_id: SessionId,
        target_item: ItemId,
        scope: RewindScope,
    ) -> RewindParams {
        RewindParams {
            session_id,
            target_item,
            scope,
            purge_untracked: false,
        }
    }

    #[tokio::test]
    async fn set_mode_persists_the_row_and_broadcasts_the_switch() {
        let (dir, manager, hub) = manager().await;
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let session = session_in_workspace(&manager, workspace).await;
        let mut events = hub.subscribe(session.id);

        let result = manager
            .set_mode(SetModeParams {
                session_id: session.id,
                mode: SessionModeId::code(),
            })
            .await
            .expect("switched");

        assert_eq!(result.session.mode, SessionModeId::code());
        let stored = manager.store.session(session.id).await.expect("stored");
        assert_eq!(stored.mode, SessionModeId::code());
        let event = events.recv().await.expect("an event");
        assert!(matches!(
            event.event,
            ServerEvent::ModeSwitched { ref from, ref to }
                if *from == SessionModeId::chat() && *to == SessionModeId::code()
        ));
        assert_eq!(
            manager.live_runtimes(),
            0,
            "nothing was assembled for a session that had no runtime"
        );
    }

    #[tokio::test]
    async fn code_mode_without_a_workspace_is_refused_and_changes_nothing() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        let error = manager
            .set_mode(SetModeParams {
                session_id: created.session.id,
                mode: SessionModeId::code(),
            })
            .await
            .expect_err("no workspace");
        assert_eq!(error.into_event().code, ErrorCode::InvalidRequest);
        let stored = manager
            .store
            .session(created.session.id)
            .await
            .expect("stored");
        assert_eq!(stored.mode, SessionModeId::chat());
    }

    #[tokio::test]
    async fn setting_the_mode_it_already_has_is_a_no_op() {
        let (_dir, manager, hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        let mut events = hub.subscribe(created.session.id);

        manager
            .set_mode(SetModeParams {
                session_id: created.session.id,
                mode: SessionModeId::chat(),
            })
            .await
            .expect("no-op");

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), events.recv())
                .await
                .is_err(),
            "nothing changed, so nothing is broadcast"
        );
    }

    #[tokio::test]
    async fn branch_list_marks_the_active_chain_and_switch_moves_it() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        let items = chain_of_three(&manager, created.session.id).await;

        let listed = manager
            .branch_list(BranchListParams {
                session_id: created.session.id,
            })
            .await
            .expect("listed");
        assert_eq!(listed.nodes.len(), 3);
        assert!(listed.nodes.iter().all(|node| node.active));

        let switched = manager
            .branch_switch(BranchSwitchParams {
                session_id: created.session.id,
                head: items[1].id,
            })
            .await
            .expect("switched");
        assert_eq!(switched.session.active_branch_head, Some(items[1].id));

        let relisted = manager
            .branch_list(BranchListParams {
                session_id: created.session.id,
            })
            .await
            .expect("relisted");
        let active: Vec<ItemId> = relisted
            .nodes
            .iter()
            .filter(|node| node.active)
            .map(|node| node.id)
            .collect();
        assert_eq!(active, vec![items[0].id, items[1].id]);
    }

    #[tokio::test]
    async fn branch_delete_needs_confirmation_and_refuses_the_active_head() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        let items = chain_of_three(&manager, created.session.id).await;

        let unconfirmed = manager
            .branch_delete(BranchDeleteParams {
                session_id: created.session.id,
                head: items[2].id,
                confirm: false,
            })
            .await
            .expect_err("no confirmation");
        assert_eq!(unconfirmed.into_event().code, ErrorCode::InvalidRequest);

        let refused = manager
            .branch_delete(BranchDeleteParams {
                session_id: created.session.id,
                head: items[0].id,
                confirm: true,
            })
            .await
            .expect_err("the active head is inside");
        assert_eq!(refused.into_event().code, ErrorCode::InvalidRequest);

        // Move the head off the tail, then that tail is deletable.
        manager
            .branch_switch(BranchSwitchParams {
                session_id: created.session.id,
                head: items[1].id,
            })
            .await
            .expect("switched");
        let deleted = manager
            .branch_delete(BranchDeleteParams {
                session_id: created.session.id,
                head: items[2].id,
                confirm: true,
            })
            .await
            .expect("deleted");
        assert_eq!(deleted.deleted_items, 1);
    }

    #[tokio::test]
    async fn rename_sets_and_clears_the_title() {
        let (_dir, manager, hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        let mut events = hub.subscribe(created.session.id);

        let renamed = manager
            .rename(SessionRenameParams {
                session_id: created.session.id,
                title: Some("a better name".to_owned()),
            })
            .await
            .expect("renamed");
        assert_eq!(renamed.session.title.as_deref(), Some("a better name"));
        assert!(matches!(
            events.recv().await.expect("event").event,
            ServerEvent::SessionUpdated { .. }
        ));

        let cleared = manager
            .rename(SessionRenameParams {
                session_id: created.session.id,
                title: None,
            })
            .await
            .expect("cleared");
        assert_eq!(cleared.session.title, None);
    }

    #[tokio::test]
    async fn deleting_a_session_removes_its_items() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        chain_of_three(&manager, created.session.id).await;

        let deleted = manager
            .delete_session(SessionDeleteParams {
                session_id: created.session.id,
            })
            .await
            .expect("deleted");
        assert_eq!(deleted.deleted_items, 3);
        assert_eq!(
            manager
                .store
                .session(created.session.id)
                .await
                .expect_err("gone")
                .to_event_error()
                .code,
            ErrorCode::SessionNotFound
        );
    }

    #[tokio::test]
    async fn export_writes_the_branch_and_refuses_to_overwrite() {
        let (dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        chain_of_three(&manager, created.session.id).await;
        let path = dir.path().join("session.jsonl");

        let exported = manager
            .export(ExportJsonlParams {
                session_id: created.session.id,
                path: path.clone(),
                all_branches: false,
            })
            .await
            .expect("exported");
        assert_eq!(exported.lines, 3);
        assert!(path.is_file());

        let again = manager
            .export(ExportJsonlParams {
                session_id: created.session.id,
                path,
                all_branches: false,
            })
            .await
            .expect_err("refuses to overwrite");
        assert_eq!(again.into_event().code, ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn a_conversation_rewind_moves_the_head_and_leaves_the_disk_alone() {
        let (dir, manager, _hub) = manager().await;
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let session = session_in_workspace(&manager, workspace.clone()).await;
        let items = chain_of_three(&manager, session.id).await;
        let note = workspace.join("note.txt");
        std::fs::write(&note, "untouched\n").expect("write");

        let result = manager
            .rewind(rewind_params(
                session.id,
                items[0].id,
                RewindScope::Conversation,
            ))
            .await
            .expect("rewound");
        assert_eq!(result.session.active_branch_head, Some(items[0].id));
        assert_eq!(
            std::fs::read_to_string(&note).expect("still there"),
            "untouched\n"
        );
        assert!(result.report.rolled_back.is_empty());
    }

    #[tokio::test]
    async fn a_rewind_target_off_the_active_branch_is_refused() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        chain_of_three(&manager, created.session.id).await;

        let error = manager
            .rewind(rewind_params(
                created.session.id,
                ItemId::new(),
                RewindScope::Conversation,
            ))
            .await
            .expect_err("not on the branch");
        assert_eq!(error.into_event().code, ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn a_code_rewind_requires_a_workspace() {
        let (_dir, manager, _hub) = manager().await;
        let created = manager.new_session(new_params()).await.expect("created");
        let items = chain_of_three(&manager, created.session.id).await;

        let error = manager
            .rewind(rewind_params(
                created.session.id,
                items[0].id,
                RewindScope::Code,
            ))
            .await
            .expect_err("no workspace");
        assert_eq!(error.into_event().code, ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn a_purge_rewind_is_refused_until_the_manifest_flow_exists() {
        let (dir, manager, _hub) = manager().await;
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let session = session_in_workspace(&manager, workspace).await;
        let items = chain_of_three(&manager, session.id).await;

        let mut params = rewind_params(session.id, items[0].id, RewindScope::Both);
        params.purge_untracked = true;
        let error = manager.rewind(params).await.expect_err("refused");
        assert_eq!(
            error.into_event().code,
            ErrorCode::InvalidRequest,
            "a purge without the approval and its manifest must never delete a file"
        );
    }

    #[tokio::test]
    async fn a_both_rewind_leaves_the_history_alone_when_the_restore_fails() {
        let (dir, manager, _hub) = manager().await;
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let session = session_in_workspace(&manager, workspace).await;
        let items = chain_of_three(&manager, session.id).await;
        let checkpoint = append_checkpoint(
            &manager,
            session.id,
            items[2].id,
            "0000000000000000000000000000000000000000",
        )
        .await;

        let error = manager
            .rewind(rewind_params(session.id, items[0].id, RewindScope::Both))
            .await
            .expect_err("the commit does not exist");
        assert_eq!(error.into_event().code, ErrorCode::InternalError);
        let after = manager.store.session(session.id).await.expect("session");
        assert_eq!(
            after.active_branch_head,
            Some(checkpoint.id),
            "a failed restore must never move the history first"
        );
    }

    #[tokio::test]
    async fn a_code_rewind_with_nothing_written_after_the_target_is_a_no_op() {
        let (dir, manager, _hub) = manager().await;
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let session = session_in_workspace(&manager, workspace.clone()).await;
        let items = chain_of_three(&manager, session.id).await;
        let note = workspace.join("note.txt");
        std::fs::write(&note, "as it was\n").expect("write");

        let result = manager
            .rewind(rewind_params(session.id, items[2].id, RewindScope::Code))
            .await
            .expect("no-op");
        assert!(result.report.rolled_back.is_empty());
        assert!(result.report.purged.is_empty());
        assert_eq!(result.session.active_branch_head, Some(items[2].id));
        assert_eq!(
            std::fs::read_to_string(&note).expect("still there"),
            "as it was\n"
        );
    }

    #[tokio::test]
    async fn a_code_rewind_restores_the_checkpoint_and_records_the_safety_snapshot() {
        let (dir, manager, _hub) = manager().await;
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let note = workspace.join("note.txt");
        std::fs::write(&note, "v1\n").expect("write");
        let session = session_in_workspace(&manager, workspace.clone()).await;
        let items = chain_of_three(&manager, session.id).await;

        let shadow = manager
            .checkpoints()
            .store_for(&workspace)
            .await
            .expect("shadow");
        let snapshot = shadow
            .snapshot(hatchery_protocol::CheckpointKind::PreWrite)
            .await
            .expect("snapshot");
        let checkpoint = append_checkpoint(
            &manager,
            session.id,
            items[2].id,
            &snapshot.checkpoint.commit_id,
        )
        .await;
        std::fs::write(&note, "v2\n").expect("the write after the checkpoint");

        let result = manager
            .rewind(rewind_params(session.id, items[2].id, RewindScope::Code))
            .await
            .expect("rewound");
        assert_eq!(
            std::fs::read_to_string(&note).expect("restored"),
            "v1\n",
            "the checkpoint's content is back"
        );
        assert!(
            result
                .report
                .rolled_back
                .iter()
                .any(|path| path.ends_with("note.txt")),
            "{:?}",
            result.report.rolled_back
        );
        assert_eq!(
            result.session.active_branch_head,
            Some(checkpoint.id),
            "a code rewind does not move the head"
        );

        let rows = manager
            .store
            .checkpoints_for_workspace(&workspace)
            .await
            .expect("rows");
        assert!(
            rows.iter().any(|row| row.item.is_none()),
            "the undo-of-undo is recorded without an item: {rows:?}"
        );
    }
}
