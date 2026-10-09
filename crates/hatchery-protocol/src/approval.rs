//! Approval requests: what the agent asks for, and what the user answers.

use serde::{Deserialize, Serialize};

/// How dangerous an action is, which decides the wording and the defaults a UI shows.
///
/// Deliberately coarse: the level drives whether a hard gate applies (invariant 5) and which
/// options the prompt offers, not fine-grained policy — that is per-tool configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Reads inside the workspace or elsewhere without side effects.
    ReadOnly,
    /// Writes inside the workspace.
    WritesWorkspace,
    /// Writes outside the workspace — a hard gate (invariant 5).
    WritesOutside,
    /// Runs a command.
    Executes,
    /// Talks to the network.
    Network,
}

impl RiskLevel {
    /// True when the action can change something outside the agent's own state.
    #[must_use]
    pub const fn is_side_effecting(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    /// True for the levels that always require explicit approval, even if a project-level rule
    /// tries to allow them (invariant 5).
    #[must_use]
    pub const fn is_hard_gate(self) -> bool {
        matches!(self, Self::WritesOutside)
    }
}

/// What the user may answer.
///
/// One enum for both the offered options and the answer: an answer is by construction one of the
/// options that were offered, so two mirrored enums would only create a way for them to drift.
/// (`docs/design/capabilities.md` §1 sketches an `ApprovalOutcome` for the answer; this is the
/// same set of choices.)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOption {
    /// Run it this time.
    AllowOnce,
    /// Run it, and persist a rule so this tool/argument shape is not asked about again.
    AllowAlways,
    /// Refuse this time.
    Deny,
    /// Refuse, and persist a rule so it is not asked about again.
    DenyAlways,
}

impl ApprovalOption {
    /// Every option, in the order a UI should render them.
    pub const ALL: &'static [Self] = &[
        Self::AllowOnce,
        Self::AllowAlways,
        Self::Deny,
        Self::DenyAlways,
    ];

    /// True when the answer lets the call run.
    #[must_use]
    pub const fn allows(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }

    /// True when the answer should be written to `approval_rules`.
    #[must_use]
    pub const fn is_remembered(self) -> bool {
        matches!(self, Self::AllowAlways | Self::DenyAlways)
    }
}

/// A tool call waiting for a decision.
///
/// Built by the tool host (`ToolHost::approval_for`), forwarded by the kernel as an
/// `ApprovalNeeded` event, answered by the daemon through whatever `ApprovalGate` the session
/// bound — a local prompt or an ACP host's `session/request_permission` (ADR-0004).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// Tool name as registered.
    pub tool: String,
    /// Human-readable digest of the arguments, e.g. `edit src/main.rs (+12 -3)`. Not the raw
    /// JSON: the user must be able to decide in seconds, and raw arguments can be megabytes.
    pub args_digest: String,
    /// How dangerous the call is.
    pub risk: RiskLevel,
    /// The options to offer. A hard gate omits the "always" choices (invariant 5).
    pub options: Vec<ApprovalOption>,
}

impl ApprovalRequest {
    /// A request offering every option.
    #[must_use]
    pub fn new(tool: impl Into<String>, args_digest: impl Into<String>, risk: RiskLevel) -> Self {
        Self {
            tool: tool.into(),
            args_digest: args_digest.into(),
            risk,
            options: ApprovalOption::ALL.to_vec(),
        }
    }

    /// A request that may only be allowed once (or denied): used by the hard gates.
    #[must_use]
    pub fn once_only(mut self) -> Self {
        self.options = vec![ApprovalOption::AllowOnce, ApprovalOption::Deny];
        self
    }

    /// True when the offered options include `option`.
    #[must_use]
    pub fn offers(&self, option: ApprovalOption) -> bool {
        self.options.contains(&option)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_request_offers_every_option() {
        let request =
            ApprovalRequest::new("write_file", "write src/lib.rs", RiskLevel::WritesWorkspace);
        assert_eq!(request.options.len(), 4);
        assert!(request.offers(ApprovalOption::AllowAlways));
        assert!(request.risk.is_side_effecting());
        assert!(!request.risk.is_hard_gate());
    }

    #[test]
    fn a_hard_gate_offers_no_remembered_options() {
        let request = ApprovalRequest::new(
            "write_file",
            "write ~/.ssh/config",
            RiskLevel::WritesOutside,
        )
        .once_only();
        assert!(request.risk.is_hard_gate());
        for option in ApprovalOption::ALL {
            assert_eq!(
                request.offers(*option),
                !option.is_remembered(),
                "{option:?} must not be offered by a hard gate (invariant 5)"
            );
        }
    }

    #[test]
    fn options_map_onto_allow_and_remember() {
        assert!(ApprovalOption::AllowAlways.allows());
        assert!(!ApprovalOption::DenyAlways.allows());
        assert!(ApprovalOption::DenyAlways.is_remembered());
        assert!(!ApprovalOption::AllowOnce.is_remembered());
    }

    #[test]
    fn requests_roundtrip() {
        let request = ApprovalRequest::new("shell", "rm -rf build/", RiskLevel::Executes);
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"executes\""), "{json}");
        assert_eq!(
            serde_json::from_str::<ApprovalRequest>(&json).expect("deserialize"),
            request
        );
    }
}
