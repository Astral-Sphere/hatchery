//! Invariant 5, composed: nothing a project or a remembered rule can say turns a hard gate into
//! a silent run.
//!
//! The three legs live in their owners' suites — the config layer drops project assignments, the
//! write tools narrow sensitive paths to `once_only`, and the kernel refuses an `AllowAlways`
//! answer to a request that never offered it. This test composes the two that meet in the daemon,
//! because a regression in either alone would still satisfy its own suite while opening the gate.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use hatchery_daemon::approvals::{GatedTools, RuleSource};
use hatchery_daemon::config::{ApprovalConfig, LayeredConfig};
use hatchery_kernel::{
    ApprovalNeed, CheckpointCollector, KernelError, ToolDef, ToolHost, ToolInvocation,
};
use hatchery_protocol::method::ConfigOrigin;
use hatchery_protocol::{
    ApprovalOption, ApprovalRequest, ApprovalRule, ApprovalRuleId, ApprovalScope, RiskLevel,
    Timestamp, ToolCallSummary, ToolProgress,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

/// A host whose one call is a hard gate: it offers `AllowOnce`/`Deny`, like `.env` writes do.
struct HardGate;

#[async_trait]
impl ToolHost for HardGate {
    fn snapshot(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    fn summarize(&self, name: &str, _args: &Value) -> ToolCallSummary {
        ToolCallSummary::new(name)
    }

    async fn approval_for(
        &self,
        _name: &str,
        _args: &Value,
        _cancel: CancellationToken,
    ) -> ApprovalNeed {
        ApprovalNeed::Ask(
            ApprovalRequest::new("write_file", "write .env", RiskLevel::WritesWorkspace)
                .once_only(),
        )
    }

    async fn invoke(
        &self,
        name: &str,
        _args: Value,
        _cancel: CancellationToken,
        _progress: UnboundedSender<ToolProgress>,
        _checkpoints: CheckpointCollector,
    ) -> Result<ToolInvocation, KernelError> {
        Err(KernelError::tool(name, "the hard gate must stop this call"))
    }
}

/// A rule set that says "always allow write_file" twice: persisted and configured.
struct AlreadyAllowed;

#[async_trait]
impl RuleSource for AlreadyAllowed {
    async fn rules(&self) -> Result<Vec<ApprovalRule>, hatchery_store::StoreError> {
        Ok(vec![ApprovalRule {
            id: ApprovalRuleId::new(),
            scope: ApprovalScope::Global,
            matcher: "write_file".to_owned(),
            decision: ApprovalOption::AllowAlways,
            created_at: Timestamp::now(),
        }])
    }
}

fn config(allow: &[&str]) -> ApprovalConfig {
    ApprovalConfig {
        timeout: Duration::from_secs(1),
        allow: allow.iter().map(|name| (*name).to_owned()).collect(),
        deny: Vec::new(),
    }
}

#[tokio::test]
async fn invariant_project_config_cannot_disable_hard_gates() {
    // Leg 1: a project file that tries to pre-authorize writes is dropped with an error log, so
    // it never reaches the gate as a rule.
    let project = LayeredConfig::from_layers(vec![(
        ConfigOrigin::Project,
        "[approval]\nallow = [\"write_file\"]\n"
            .parse::<toml::Table>()
            .expect("valid toml"),
    )]);
    assert!(
        project.approval().allow.is_empty(),
        "a repository must not pre-authorize its own writes"
    );

    // Leg 2: even a user-level allow plus a persisted AllowAlways cannot waive a request that
    // offers no remembered options. This is the composition the invariant is about.
    let gated = GatedTools::new(
        Arc::new(HardGate),
        Arc::new(AlreadyAllowed),
        config(&["write_file"]),
    );
    let need = gated
        .approval_for(
            "write_file",
            &serde_json::json!({"path": ".env", "content": "SECRET=1"}),
            CancellationToken::new(),
        )
        .await;
    let ApprovalNeed::Ask(request) = need else {
        panic!("an allow rule must not turn a hard gate into a silent run: {need:?}");
    };
    assert_eq!(
        request.options,
        [ApprovalOption::AllowOnce, ApprovalOption::Deny],
        "the hard gate still offers only the once-decision"
    );
}
