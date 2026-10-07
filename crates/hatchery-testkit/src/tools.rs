//! A tool host whose catalogue, approvals and results are scripted.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;

use hatchery_kernel::{
    AgentCommand, AgentHandle, KernelError, KernelEvent, ToolDef, ToolHost, ToolInvocation,
};
use hatchery_protocol::{
    ApprovalOption, ApprovalRequest, RiskLevel, ToolCallSummary, ToolOutput, ToolProgress,
};

use crate::gate::Gate;

/// One invocation the host was asked to perform.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordedCall {
    /// Which tool.
    pub name: String,
    /// The raw arguments as the kernel passed them.
    pub args: Value,
    /// True when the turn was interrupted while this call was waiting.
    pub cancelled: bool,
}

/// What a scripted invocation does.
enum ScriptedResult {
    /// Returns this output as a success.
    Output(ToolOutput),
    /// Returns this output as a failure the model can read.
    Failure(ToolOutput),
    /// Refuses to run at all.
    Error(String),
}

/// A tool host with no real tools behind it.
///
/// Everything the kernel can observe about tools goes through three methods, so the fake needs no
/// filesystem, process or approval backend — that is the seam paying off.
pub struct ScriptedToolHost {
    defs: Mutex<Vec<ToolDef>>,
    approvals: Mutex<HashMap<String, ApprovalRequest>>,
    results: Mutex<HashMap<String, VecDeque<ScriptedResult>>>,
    calls: Mutex<Vec<RecordedCall>>,
    gate: Option<Gate>,
}

impl ScriptedToolHost {
    /// A host with no tools.
    #[must_use]
    pub fn new() -> Self {
        Self {
            defs: Mutex::new(Vec::new()),
            approvals: Mutex::new(HashMap::new()),
            results: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            gate: None,
        }
    }

    /// Advertises these tools.
    #[must_use]
    pub fn advertising(self, names: &[&str]) -> Self {
        self.set_defs(names);
        self
    }

    /// Replaces the advertised catalogue, as a mode switch would.
    pub fn set_defs(&self, names: &[&str]) {
        let defs = names
            .iter()
            .map(|name| ToolDef {
                name: (*name).to_owned(),
                description: format!("scripted {name}"),
                parameters: serde_json::json!({"type": "object"}),
            })
            .collect();
        *self.defs.lock().expect("the mutex is never poisoned") = defs;
    }

    /// Makes this tool need approval before it runs.
    #[must_use]
    pub fn requiring_approval(self, name: &str, risk: RiskLevel) -> Self {
        self.requiring_approval_with(ApprovalRequest::new(
            name,
            format!("{name} (scripted)"),
            risk,
        ))
    }

    /// Makes this tool need approval, offering exactly what `request` offers.
    ///
    /// [`Self::requiring_approval`] builds its request with [`ApprovalRequest::new`], whose offer
    /// list is always the full four, so it cannot express a hard gate: `once_only()` is what
    /// narrows the list, and the kernel's refusal of an answer outside it — the only mechanism
    /// that makes "cannot be remembered" real — is unreachable through a request that offers
    /// everything.
    #[must_use]
    pub fn requiring_approval_with(self, request: ApprovalRequest) -> Self {
        self.approvals
            .lock()
            .expect("the mutex is never poisoned")
            .insert(request.tool.clone(), request);
        self
    }

    /// Makes this tool return `output` successfully. Queued: the nth call gets the nth answer.
    #[must_use]
    pub fn answering(self, name: &str, output: ToolOutput) -> Self {
        self.script(name, ScriptedResult::Output(output))
    }

    /// Makes this tool return `output` as a failure.
    #[must_use]
    pub fn failing(self, name: &str, output: ToolOutput) -> Self {
        self.script(name, ScriptedResult::Failure(output))
    }

    /// Makes invoking this tool fail the turn outright.
    #[must_use]
    pub fn erroring(self, name: &str, message: &str) -> Self {
        self.script(name, ScriptedResult::Error(message.to_owned()))
    }

    /// Makes every invocation wait for a permit, so a test can interrupt mid-tool.
    #[must_use]
    pub fn gated(self) -> (Self, Gate) {
        let gate = Gate::new();
        (
            Self {
                gate: Some(gate.clone()),
                ..self
            },
            gate,
        )
    }

    /// Every invocation the host was asked to perform, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls
            .lock()
            .expect("the mutex is never poisoned")
            .clone()
    }

    /// The names of the tools invoked, in order.
    #[must_use]
    pub fn call_names(&self) -> Vec<String> {
        self.calls().into_iter().map(|call| call.name).collect()
    }

    fn script(self, name: &str, result: ScriptedResult) -> Self {
        self.results
            .lock()
            .expect("the mutex is never poisoned")
            .entry(name.to_owned())
            .or_default()
            .push_back(result);
        self
    }
}

impl Default for ScriptedToolHost {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHost for ScriptedToolHost {
    fn snapshot(&self) -> Vec<ToolDef> {
        self.defs
            .lock()
            .expect("the mutex is never poisoned")
            .clone()
    }

    fn summarize(&self, name: &str, _args: &Value) -> ToolCallSummary {
        ToolCallSummary::new(format!("call {name}"))
    }

    fn approval_for(&self, name: &str, _args: &Value) -> Option<ApprovalRequest> {
        self.approvals
            .lock()
            .expect("the mutex is never poisoned")
            .get(name)
            .cloned()
    }

    async fn invoke(
        &self,
        name: &str,
        args: Value,
        cancel: CancellationToken,
        progress: UnboundedSender<ToolProgress>,
    ) -> Result<ToolInvocation, KernelError> {
        let scripted = {
            let mut results = self.results.lock().expect("the mutex is never poisoned");
            results.get_mut(name).and_then(VecDeque::pop_front)
        };

        // Progress before any waiting, so a test can observe the forwarding without having to
        // release the gate first.
        let _ = progress.send(ToolProgress::from(format!("{name} started")));

        // The call is recorded on ENTRY, and its cancellation verdict is written by a drop
        // guard. The kernel's tool select is cancel-first: an interrupted invocation is dropped
        // without ever being polled again, so a record written only after the gate could never
        // exist for the case that matters most. The guard is what makes "an interrupt cancels
        // the tool" observable from the host side at all.
        let index = {
            let mut calls = self.calls.lock().expect("the mutex is never poisoned");
            calls.push(RecordedCall {
                name: name.to_owned(),
                args,
                cancelled: false,
            });
            calls.len() - 1
        };
        let _guard = CancelGuard {
            calls: &self.calls,
            index,
            cancel: cancel.clone(),
        };

        if let Some(gate) = &self.gate {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return Ok(ToolInvocation::failed(ToolOutput::text(format!(
                        "{name} was cancelled"
                    ))));
                }
                _ = gate.acquire() => {}
            }
        } else if cancel.is_cancelled() {
            return Ok(ToolInvocation::failed(ToolOutput::text(format!(
                "{name} was cancelled"
            ))));
        }

        match scripted {
            Some(ScriptedResult::Output(output)) => Ok(ToolInvocation::ok(output)),
            Some(ScriptedResult::Failure(output)) => Ok(ToolInvocation::failed(output)),
            Some(ScriptedResult::Error(message)) => Err(KernelError::tool(name, message)),
            None => Ok(ToolInvocation::failed(ToolOutput::text(format!(
                "ScriptedToolHost has no scripted response for `{name}`"
            )))),
        }
    }
}

/// Writes the cancellation verdict into a recorded call when the invocation ends.
///
/// This includes the case the host cannot otherwise see: the kernel dropping the future while it
/// is parked, because the turn was interrupted. `cancelled` then records that the token was
/// cancelled by the time the future died — which is exactly the seam contract under test.
struct CancelGuard<'a> {
    calls: &'a Mutex<Vec<RecordedCall>>,
    index: usize,
    cancel: CancellationToken,
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        if self.cancel.is_cancelled() {
            let mut calls = self.calls.lock().expect("the mutex is never poisoned");
            if let Some(call) = calls.get_mut(self.index) {
                call.cancelled = true;
            }
        }
    }
}

/// How a test answers the approvals it is asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptedApproval {
    /// Allow every call once.
    AllowOnce,
    /// Allow every call and remember it.
    AllowAlways,
    /// Refuse every call.
    Deny,
    /// Refuse every call and remember it.
    DenyAlways,
    /// Answer with these choices in order; after that, deny.
    Script(Vec<ApprovalOption>),
}

impl ScriptedApproval {
    fn next(&mut self) -> ApprovalOption {
        match self {
            Self::AllowOnce => ApprovalOption::AllowOnce,
            Self::AllowAlways => ApprovalOption::AllowAlways,
            Self::Deny => ApprovalOption::Deny,
            Self::DenyAlways => ApprovalOption::DenyAlways,
            Self::Script(choices) => {
                if choices.is_empty() {
                    ApprovalOption::Deny
                } else {
                    choices.remove(0)
                }
            }
        }
    }
}

/// Answers approval requests as a frontend would, until the event stream closes.
///
/// Returns a task handle so a test can stop it; dropping it is usually enough, since the events
/// channel closes with the sink.
#[must_use]
pub fn answer_approvals(
    handle: AgentHandle,
    mut events: UnboundedReceiver<KernelEvent>,
    mut policy: ScriptedApproval,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let KernelEvent::ApprovalNeeded { request_id, .. } = event {
                let option = policy.next();
                if handle
                    .submit(AgentCommand::decide(request_id, option))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    })
}
