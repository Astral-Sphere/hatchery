//! An [`ApprovalGate`] that answers from a script, for contract tests and daemon tests.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;

use hatchery_capabilities::ApprovalGate;
use hatchery_protocol::{ApprovalId, ApprovalOption, ApprovalRequest};

/// Records every question and answers from a queue.
///
/// An exhausted queue answers **deny**, the same fail-closed answer a timeout gives: a test that
/// forgot to script an answer must not accidentally approve a write.
pub struct ScriptedGate {
    answers: Mutex<VecDeque<ApprovalOption>>,
    seen: Mutex<Vec<(ApprovalId, ApprovalRequest)>>,
}

impl ScriptedGate {
    /// A gate with these answers, in order.
    #[must_use]
    pub fn answering(answers: impl IntoIterator<Item = ApprovalOption>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Every question the gate was asked, in order.
    #[must_use]
    pub fn seen(&self) -> Vec<(ApprovalId, ApprovalRequest)> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl ApprovalGate for ScriptedGate {
    async fn request(&self, request_id: ApprovalId, request: ApprovalRequest) -> ApprovalOption {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((request_id, request));
        self.answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or(ApprovalOption::Deny)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_protocol::RiskLevel;

    fn request(tool: &str) -> ApprovalRequest {
        ApprovalRequest::new(tool, format!("{tool} call"), RiskLevel::WritesWorkspace)
    }

    #[tokio::test]
    async fn answers_arrive_in_order_and_are_recorded() {
        let gate = ScriptedGate::answering([ApprovalOption::AllowOnce, ApprovalOption::Deny]);
        let first = ApprovalId::new();
        let second = ApprovalId::new();
        assert_eq!(
            gate.request(first, request("write_file")).await,
            ApprovalOption::AllowOnce
        );
        assert_eq!(
            gate.request(second, request("edit")).await,
            ApprovalOption::Deny
        );
        let seen = gate.seen();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, first);
        assert_eq!(seen[1].1.tool, "edit");
    }

    #[tokio::test]
    async fn an_unscripted_question_fails_closed() {
        let gate = ScriptedGate::answering([]);
        assert_eq!(
            gate.request(ApprovalId::new(), request("write_file")).await,
            ApprovalOption::Deny
        );
    }
}
