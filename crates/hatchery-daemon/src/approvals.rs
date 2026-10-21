//! The daemon half of the approval round trip (M2 Phase 2).
//!
//! The kernel already owns the state machine: it calls `ToolHost::approval_for`, emits
//! `ApprovalNeeded { request_id, request }`, and waits for `AgentCommand::ApprovalDecision`.
//! What was missing is everything between that event and that command:
//!
//! * [`PendingApprovals`] — `request_id → waiter`, so `approval/respond` has somewhere to deliver
//!   an answer. Two frontends may race; the first answer wins and the second gets `handled: false`.
//! * [`DaemonApproval`] — the local [`ApprovalGate`]. It registers, publishes the event, and waits
//!   with a **fail-closed** timeout. The kernel deliberately has no timeout: only the answering
//!   side knows how long a human needs.
//! * [`GatedTools`] — a `ToolHost` decorator that applies persisted and configured rules *before*
//!   the kernel asks. Deny is unconditional; an allow rule can never waive a hard gate (a request
//!   that does not offer `AllowAlways` is asked regardless).
//!
//! Rules are evaluated order-independently, deny first. That is forced by the schema — the
//! `approval_rules` table has no ordering column — and it is also the safer reading: adding an
//! allow rule must never silently override a deny. The same shape is what atomcode's
//! `permission_rules` gate landed on (`deny` checked first, allow never applying to sensitive
//! paths), and what the user ruled for D8.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use hatchery_capabilities::ApprovalGate;
use hatchery_kernel::{
    AgentCommand, AgentHandle, ApprovalNeed, KernelError, ToolDef, ToolHost, ToolInvocation,
};
use hatchery_protocol::{
    ApprovalId, ApprovalOption, ApprovalRequest, ApprovalRule, ApprovalScope, ServerEvent,
    SessionEvent, SessionId, ToolCallSummary, ToolProgress,
};
use hatchery_store::SessionStore;

use crate::config::ApprovalConfig;
use crate::hub::LiveHub;

/// One waiting request: who is parked on it, and how to answer.
#[derive(Debug)]
struct Pending {
    session: SessionId,
    request: ApprovalRequest,
    sender: oneshot::Sender<ApprovalOption>,
}

/// Approval requests waiting for an answer, keyed by the kernel's request id.
#[derive(Debug, Default)]
pub struct PendingApprovals {
    inner: Mutex<HashMap<ApprovalId, Pending>>,
}

impl PendingApprovals {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a waiter and returns its receiving end.
    ///
    /// A duplicate id would mean the kernel reused one; the old waiter is dropped, which resolves
    /// its receiver to `Err` and therefore to deny. That is the safe direction for a bug.
    fn register(
        &self,
        id: ApprovalId,
        session: SessionId,
        request: ApprovalRequest,
    ) -> oneshot::Receiver<ApprovalOption> {
        let (tx, rx) = oneshot::channel();
        self.slots().insert(
            id,
            Pending {
                session,
                request,
                sender: tx,
            },
        );
        rx
    }

    /// Answers a pending request. False when no such request is waiting — already answered,
    /// timed out, or turned down by a disconnected client.
    pub fn respond(&self, id: ApprovalId, option: ApprovalOption) -> bool {
        let sender = self.slots().remove(&id);
        match sender {
            Some(pending) => pending.sender.send(option).is_ok(),
            None => false,
        }
    }

    /// Drops a waiter without answering it (timeout, shutdown).
    fn cancel(&self, id: ApprovalId) {
        self.slots().remove(&id);
    }

    /// The requests one session is parked on, oldest request id first.
    ///
    /// `session/load` hands these to a reconnecting frontend so a prompt that arrived while it
    /// was away is rendered rather than timing out unseen. Sorted by id because a `HashMap` has
    /// no order and two identical requests must not answer differently on two loads.
    #[must_use]
    pub fn for_session(
        &self,
        session: SessionId,
    ) -> Vec<hatchery_protocol::method::PendingApproval> {
        let slots = self.slots();
        let mut waiting: Vec<_> = slots
            .iter()
            .filter(|(_, pending)| pending.session == session)
            .map(|(id, pending)| (id.to_string(), *id, pending))
            .collect();
        waiting.sort_by(|left, right| left.0.cmp(&right.0));
        waiting
            .into_iter()
            .map(
                |(_, request_id, pending)| hatchery_protocol::method::PendingApproval {
                    request_id,
                    session_id: pending.session,
                    request: pending.request.clone(),
                },
            )
            .collect()
    }

    /// How many requests are waiting, for tests and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots().len()
    }

    /// True when nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots().is_empty()
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<ApprovalId, Pending>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The local gate: publish the question, wait for `approval/respond`, deny on timeout.
pub struct DaemonApproval {
    session: SessionId,
    generation: u64,
    hub: Arc<LiveHub>,
    pending: Arc<PendingApprovals>,
    timeout: Duration,
}

impl DaemonApproval {
    /// A gate for one runtime generation.
    #[must_use]
    pub fn new(
        session: SessionId,
        generation: u64,
        hub: Arc<LiveHub>,
        pending: Arc<PendingApprovals>,
        timeout: Duration,
    ) -> Self {
        Self {
            session,
            generation,
            hub,
            pending,
            timeout,
        }
    }
}

#[async_trait]
impl ApprovalGate for DaemonApproval {
    async fn request(&self, request_id: ApprovalId, request: ApprovalRequest) -> ApprovalOption {
        // Register *before* publishing: an answer that arrives the instant the event is on the
        // wire must find the waiter. The reverse order is a real race — a scripted client answers
        // in microseconds — and its symptom would be a prompt that waits for the full timeout
        // after the user already clicked.
        let receiver = self
            .pending
            .register(request_id, self.session, request.clone());
        self.hub.publish(SessionEvent::new(
            self.session,
            self.generation,
            ServerEvent::ApprovalRequested {
                request_id,
                request,
            },
        ));
        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(option)) => option,
            Ok(Err(_)) => {
                // The sender was dropped: a cancelled turn or a restarted runtime. Fail closed.
                tracing::warn!(%request_id, "the approval waiter was dropped; denying");
                ApprovalOption::Deny
            }
            Err(_) => {
                self.pending.cancel(request_id);
                // The kernel records this as a denial; the model reads it as one. The timeout is
                // distinguishable in the log, which is where an operator looks for "why did it
                // stop?".
                tracing::warn!(
                    %request_id,
                    timeout_ms = self.timeout.as_millis(),
                    "no approval answer before the timeout; denying (fail closed)"
                );
                ApprovalOption::Deny
            }
        }
    }
}

/// Where [`GatedTools`] reads persisted rules from.
///
/// A trait rather than a store handle so the decorator can be tested without a database, and so a
/// future ACP-bound session can feed it from the host. A failure is surfaced, not swallowed: the
/// decorator must know it cannot trust either an allow or a deny that it cannot read.
#[async_trait]
pub trait RuleSource: Send + Sync {
    /// The rules that apply to this session's workspace (global ones included).
    async fn rules(&self) -> Result<Vec<ApprovalRule>, hatchery_store::StoreError>;
}

/// The persistent implementation: `approval_rules` rows, filtered to this workspace.
pub struct StoreRules {
    store: Arc<dyn SessionStore>,
    workspace: Option<PathBuf>,
}

impl StoreRules {
    /// Rules for one session's workspace.
    #[must_use]
    pub fn new(store: Arc<dyn SessionStore>, workspace: Option<PathBuf>) -> Self {
        Self { store, workspace }
    }
}

#[async_trait]
impl RuleSource for StoreRules {
    async fn rules(&self) -> Result<Vec<ApprovalRule>, hatchery_store::StoreError> {
        // Fetch all and filter here rather than asking the store for a workspace: a session
        // without a workspace must see global rules only, and "no workspace" is not "every
        // workspace". The table is small and this keeps the store API to one query shape.
        let all = self.store.approval_rules(None).await?;
        Ok(all
            .into_iter()
            .filter(|rule| match &rule.scope {
                ApprovalScope::Global => true,
                ApprovalScope::Workspace(path) => self.workspace.as_deref() == Some(path.as_path()),
            })
            .collect())
    }
}

/// A `ToolHost` that answers "already allowed / already denied" before the kernel asks.
pub struct GatedTools {
    inner: Arc<dyn ToolHost>,
    rules: Arc<dyn RuleSource>,
    config: ApprovalConfig,
}

impl GatedTools {
    /// Wraps a session's registry with its rule sources.
    #[must_use]
    pub fn new(
        inner: Arc<dyn ToolHost>,
        rules: Arc<dyn RuleSource>,
        config: ApprovalConfig,
    ) -> Self {
        Self {
            inner,
            rules,
            config,
        }
    }

    fn configured(&self, list: &[String], tool: &str) -> bool {
        list.iter().any(|matcher| matcher == tool)
    }
}

#[async_trait]
impl ToolHost for GatedTools {
    fn snapshot(&self) -> Vec<ToolDef> {
        self.inner.snapshot()
    }

    fn summarize(&self, name: &str, args: &serde_json::Value) -> ToolCallSummary {
        self.inner.summarize(name, args)
    }

    async fn approval_for(
        &self,
        name: &str,
        args: &serde_json::Value,
        cancel: CancellationToken,
    ) -> ApprovalNeed {
        let need = self.inner.approval_for(name, args, cancel).await;
        let ApprovalNeed::Ask(request) = need else {
            return need;
        };

        // A store failure must not turn into an allow (it cannot be trusted) nor into a deny (the
        // user can still decide). Asking is the only answer that is safe in both directions.
        let stored = match self.rules.rules().await {
            Ok(rules) => Some(rules),
            Err(error) => {
                tracing::warn!(%error, tool = %request.tool, "approval rules unreadable; asking");
                None
            }
        };

        // Deny is unconditional: a deny rule applies even to a hard gate, because it only ever
        // takes away a power. This mirrors atomcode's `deny`-first rule and keeps the evaluation
        // independent of insertion order (the table has no sort column).
        let denied_by_rule = stored.as_ref().is_some_and(|rules| {
            rules.iter().any(|rule| {
                rule.matcher == request.tool && rule.decision == ApprovalOption::DenyAlways
            })
        });
        if denied_by_rule || self.configured(&self.config.deny, &request.tool) {
            return ApprovalNeed::Refused(format!(
                "denied by an approval rule for `{}`; do not repeat it",
                request.tool
            ));
        }

        // Invariant 5: a request that does not offer `AllowAlways` is a hard gate. No allow rule —
        // persisted, configured, or project-supplied — may turn it into a silent run. The kernel
        // already refuses an `AllowAlways` answer to such a request; this is the other half, so
        // the request never skips the prompt in the first place.
        if !request.offers(ApprovalOption::AllowAlways) {
            return ApprovalNeed::Ask(request);
        }
        let allowed_by_rule = stored.as_ref().is_some_and(|rules| {
            rules.iter().any(|rule| {
                rule.matcher == request.tool && rule.decision == ApprovalOption::AllowAlways
            })
        });
        if allowed_by_rule || self.configured(&self.config.allow, &request.tool) {
            return ApprovalNeed::NotNeeded;
        }
        ApprovalNeed::Ask(request)
    }

    async fn invoke(
        &self,
        name: &str,
        args: serde_json::Value,
        cancel: CancellationToken,
        progress: tokio::sync::mpsc::UnboundedSender<ToolProgress>,
        checkpoints: hatchery_kernel::CheckpointCollector,
    ) -> Result<ToolInvocation, KernelError> {
        self.inner
            .invoke(name, args, cancel, progress, checkpoints)
            .await
    }
}

/// Records a remembered decision as a rule. Failures are logged, never retried: the decision
/// still stands for the call in flight, and the next call simply asks again.
pub async fn remember(
    store: &dyn SessionStore,
    workspace: Option<&Path>,
    tool: &str,
    decision: ApprovalOption,
) {
    if !decision.is_remembered() {
        return;
    }
    let scope = match workspace {
        Some(path) => ApprovalScope::Workspace(path.to_path_buf()),
        None => ApprovalScope::Global,
    };
    if let Err(error) = store.record_approval_rule(scope, tool, decision).await {
        tracing::warn!(%error, tool, "could not persist the approval rule");
    }
}

/// The per-runtime loop that turns `ApprovalNeeded` into a decision.
///
/// It is a task rather than work inside the sink because the sink must not block on a human: it
/// also carries text deltas and item boundaries for this runtime.
pub async fn pump_approvals(
    mut requests: UnboundedReceiver<(ApprovalId, ApprovalRequest)>,
    gate: Arc<dyn ApprovalGate>,
    handle: AgentHandle,
    store: Arc<dyn SessionStore>,
    workspace: Option<PathBuf>,
) {
    while let Some((request_id, request)) = requests.recv().await {
        let option = gate.request(request_id, request.clone()).await;
        remember(store.as_ref(), workspace.as_deref(), &request.tool, option).await;
        if handle
            .submit(AgentCommand::decide(request_id, option))
            .await
            .is_err()
        {
            tracing::debug!("the runtime is gone; the approval pump stops");
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use hatchery_protocol::{ApprovalRuleId, RiskLevel, Timestamp};

    struct Asking {
        request: ApprovalRequest,
    }

    #[async_trait]
    impl ToolHost for Asking {
        fn snapshot(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        fn summarize(&self, name: &str, _args: &serde_json::Value) -> ToolCallSummary {
            ToolCallSummary::new(name)
        }
        async fn approval_for(
            &self,
            _name: &str,
            _args: &serde_json::Value,
            _cancel: CancellationToken,
        ) -> ApprovalNeed {
            ApprovalNeed::Ask(self.request.clone())
        }
        async fn invoke(
            &self,
            name: &str,
            _args: serde_json::Value,
            _cancel: CancellationToken,
            _progress: tokio::sync::mpsc::UnboundedSender<ToolProgress>,
            _checkpoints: hatchery_kernel::CheckpointCollector,
        ) -> Result<ToolInvocation, KernelError> {
            Err(KernelError::tool(name, "not used"))
        }
    }

    struct Fixed(Vec<ApprovalRule>);
    #[async_trait]
    impl RuleSource for Fixed {
        async fn rules(&self) -> Result<Vec<ApprovalRule>, hatchery_store::StoreError> {
            Ok(self.0.clone())
        }
    }

    struct Broken;
    #[async_trait]
    impl RuleSource for Broken {
        async fn rules(&self) -> Result<Vec<ApprovalRule>, hatchery_store::StoreError> {
            Err(hatchery_store::StoreError::Database("down".to_owned()))
        }
    }

    fn rule(tool: &str, decision: ApprovalOption) -> ApprovalRule {
        ApprovalRule {
            id: ApprovalRuleId::new(),
            scope: ApprovalScope::Workspace(PathBuf::from("/work")),
            matcher: tool.to_owned(),
            decision,
            created_at: Timestamp::now(),
        }
    }

    fn request(tool: &str, options: Vec<ApprovalOption>) -> ApprovalRequest {
        ApprovalRequest {
            tool: tool.to_owned(),
            args_digest: format!("{tool} call"),
            risk: RiskLevel::WritesWorkspace,
            options,
            preview: None,
        }
    }

    fn gated(
        rules: Arc<dyn RuleSource>,
        config: ApprovalConfig,
        request: ApprovalRequest,
    ) -> GatedTools {
        GatedTools::new(Arc::new(Asking { request }), rules, config)
    }

    fn config(allow: &[&str], deny: &[&str]) -> ApprovalConfig {
        ApprovalConfig {
            timeout: Duration::from_secs(1),
            allow: allow.iter().map(|s| (*s).to_owned()).collect(),
            deny: deny.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    async fn ask(tools: &GatedTools) -> ApprovalNeed {
        tools
            .approval_for(
                "write_file",
                &serde_json::json!({}),
                CancellationToken::new(),
            )
            .await
    }

    #[tokio::test]
    async fn a_persisted_allow_rule_skips_the_prompt() {
        let tools = gated(
            Arc::new(Fixed(vec![rule("write_file", ApprovalOption::AllowAlways)])),
            config(&[], &[]),
            request("write_file", ApprovalOption::ALL.to_vec()),
        );
        assert_eq!(ask(&tools).await, ApprovalNeed::NotNeeded);
    }

    #[tokio::test]
    async fn a_configured_allow_rule_skips_the_prompt() {
        let tools = gated(
            Arc::new(Fixed(Vec::new())),
            config(&["write_file"], &[]),
            request("write_file", ApprovalOption::ALL.to_vec()),
        );
        assert_eq!(ask(&tools).await, ApprovalNeed::NotNeeded);
    }

    #[tokio::test]
    async fn a_deny_rule_refuses_without_asking() {
        let tools = gated(
            Arc::new(Fixed(vec![rule("write_file", ApprovalOption::DenyAlways)])),
            config(&[], &[]),
            request("write_file", ApprovalOption::ALL.to_vec()),
        );
        let ApprovalNeed::Refused(reason) = ask(&tools).await else {
            panic!("a deny rule must refuse");
        };
        assert!(reason.contains("denied by an approval rule"), "{reason}");
        assert!(reason.contains("do not repeat"), "{reason}");
    }

    #[tokio::test]
    async fn deny_beats_allow_whatever_the_source() {
        let tools = gated(
            Arc::new(Fixed(vec![rule("write_file", ApprovalOption::AllowAlways)])),
            config(&[], &["write_file"]),
            request("write_file", ApprovalOption::ALL.to_vec()),
        );
        assert!(matches!(ask(&tools).await, ApprovalNeed::Refused(_)));
    }

    #[tokio::test]
    async fn a_hard_gate_is_asked_even_with_an_allow_rule() {
        let tools = gated(
            Arc::new(Fixed(vec![rule("write_file", ApprovalOption::AllowAlways)])),
            config(&["write_file"], &[]),
            request(
                "write_file",
                vec![ApprovalOption::AllowOnce, ApprovalOption::Deny],
            ),
        );
        assert!(matches!(ask(&tools).await, ApprovalNeed::Ask(_)));
    }

    #[tokio::test]
    async fn a_hard_gate_can_still_be_refused_by_a_deny_rule() {
        let tools = gated(
            Arc::new(Fixed(vec![rule("write_file", ApprovalOption::DenyAlways)])),
            config(&[], &[]),
            request(
                "write_file",
                vec![ApprovalOption::AllowOnce, ApprovalOption::Deny],
            ),
        );
        assert!(matches!(ask(&tools).await, ApprovalNeed::Refused(_)));
    }

    #[tokio::test]
    async fn a_rule_for_another_tool_does_nothing() {
        let tools = gated(
            Arc::new(Fixed(vec![rule("edit", ApprovalOption::AllowAlways)])),
            config(&[], &[]),
            request("write_file", ApprovalOption::ALL.to_vec()),
        );
        assert!(matches!(ask(&tools).await, ApprovalNeed::Ask(_)));
    }

    #[tokio::test]
    async fn an_unreadable_rule_source_asks_rather_than_allowing_or_refusing() {
        let tools = gated(
            Arc::new(Broken),
            config(&[], &[]),
            request("write_file", ApprovalOption::ALL.to_vec()),
        );
        assert!(matches!(ask(&tools).await, ApprovalNeed::Ask(_)));
    }

    #[tokio::test]
    async fn pending_requests_are_listed_per_session_and_disappear_once_answered() {
        let pending = PendingApprovals::new();
        let one = SessionId::new();
        let two = SessionId::new();
        let first = ApprovalId::new();
        let second = ApprovalId::new();
        let _first = pending.register(
            first,
            one,
            request("write_file", ApprovalOption::ALL.to_vec()),
        );
        let _second = pending.register(second, two, request("edit", ApprovalOption::ALL.to_vec()));

        let listed = pending.for_session(one);
        assert_eq!(listed.len(), 1, "another session's prompt is not ours");
        assert_eq!(listed[0].request_id, first);
        assert_eq!(listed[0].session_id, one);
        assert_eq!(listed[0].request.tool, "write_file");

        assert!(pending.respond(first, ApprovalOption::AllowOnce));
        assert!(
            pending.for_session(one).is_empty(),
            "an answered request must not come back on reconnect"
        );
        assert_eq!(pending.for_session(two).len(), 1);
    }

    #[tokio::test]
    async fn an_answer_with_no_waiter_is_not_handled() {
        let pending = PendingApprovals::new();
        assert!(!pending.respond(ApprovalId::new(), ApprovalOption::AllowOnce));
    }

    #[tokio::test]
    async fn the_gate_publishes_before_it_waits_and_delivers_the_answer() {
        let hub = Arc::new(LiveHub::new());
        let session = SessionId::new();
        let mut events = hub.subscribe(session);
        let pending = Arc::new(PendingApprovals::new());
        let gate: Arc<dyn ApprovalGate> = Arc::new(DaemonApproval::new(
            session,
            7,
            Arc::clone(&hub),
            Arc::clone(&pending),
            Duration::from_secs(2),
        ));

        let request_id = ApprovalId::new();
        let asked = {
            let gate = Arc::clone(&gate);
            let request = request("write_file", ApprovalOption::ALL.to_vec());
            tokio::spawn(async move { gate.request(request_id, request).await })
        };

        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("the question was published")
            .expect("the hub is alive");
        assert!(matches!(
            event.event,
            ServerEvent::ApprovalRequested { request_id: id, .. } if id == request_id
        ));
        assert!(pending.respond(request_id, ApprovalOption::AllowAlways));
        assert_eq!(
            asked.await.expect("the gate task finished"),
            ApprovalOption::AllowAlways
        );
        assert!(pending.is_empty(), "the waiter is removed once answered");
    }

    #[tokio::test]
    async fn a_missing_answer_fails_closed_and_clears_the_waiter() {
        let hub = Arc::new(LiveHub::new());
        let session = SessionId::new();
        let pending = Arc::new(PendingApprovals::new());
        let gate = DaemonApproval::new(
            session,
            1,
            hub,
            Arc::clone(&pending),
            Duration::from_millis(20),
        );
        let request_id = ApprovalId::new();
        let option = gate
            .request(
                request_id,
                request("write_file", ApprovalOption::ALL.to_vec()),
            )
            .await;
        assert_eq!(option, ApprovalOption::Deny);
        assert!(pending.is_empty());
        assert!(!pending.respond(request_id, ApprovalOption::AllowOnce));
    }
}
