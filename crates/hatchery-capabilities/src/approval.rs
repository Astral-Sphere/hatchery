//! The approval seam: how "may I?" reaches a human.
//!
//! The trait lives here, but M1 never constructs a call to it: Chat mode has no approvals
//! (ADR-0005 — read-only tools need none), and the `DaemonApproval` implementation that routes
//! through the protocol lands in M2 with the write tools. The kernel's side of the round trip
//! (`ToolHost::approval_for` → `ApprovalNeeded` → `AgentCommand::ApprovalDecision`) is already
//! built and tested; this trait is the other socket.

use async_trait::async_trait;

use hatchery_protocol::{ApprovalOption, ApprovalRequest};

/// Answers approval requests.
///
/// Implementations own the timeout policy (fail-closed = deny) and the persistence of
/// "always allow" rules; the kernel knows neither.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Asks, and blocks until a human, a rule or the timeout answers.
    async fn request(&self, request: ApprovalRequest) -> ApprovalOption;
}
