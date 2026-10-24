//! Approval requests: what the agent asks for, and what the user answers.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::diff::Diff;

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

/// What the user is shown so they can decide without reading megabytes of JSON.
///
/// `args_digest` is one line and deliberately lossy; this is the payload a frontend renders
/// beside the options (D14). The variant names are wire spelling — adding one is a protocol
/// change, which is why `Diff` travels as structured hunks instead of a text blob that every
/// frontend would have to parse (`docs/design/protocol.md` §2.1 ②).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum ApprovalPreview {
    /// The change a write would make, as the same structured diff checkpoints use.
    Diff(Diff),
    /// A command about to run, exactly as it will be spawned.
    Command {
        /// argv[0] is the program; the rest are its arguments.
        argv: Vec<String>,
        /// The directory it runs in.
        cwd: PathBuf,
    },
    /// Text that fits neither shape (a URL, a snippet); already truncated by the tool.
    Excerpt(String),
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
    /// The payload to render beside the options. Optional because a tool may have nothing more
    /// to say than its digest, and because old peers must keep parsing these requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<ApprovalPreview>,
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
            preview: None,
        }
    }

    /// Attaches the payload a frontend renders (D14).
    #[must_use]
    pub fn with_preview(mut self, preview: ApprovalPreview) -> Self {
        self.preview = Some(preview);
        self
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

/// Where an approval rule applies.
///
/// The table has no workspace column, so the scope string is the only carrier: `global` for a
/// rule every workspace shares, and the canonical workspace path for one that belongs to a
/// single checkout (D8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub enum ApprovalScope {
    /// Applies to every session, whatever workspace it is bound to.
    Global,
    /// Applies only to sessions whose canonical workspace is this path.
    Workspace(PathBuf),
}

impl ApprovalScope {
    /// The spelling stored in `approval_rules.scope`.
    #[must_use]
    pub fn as_storage(&self) -> String {
        match self {
            Self::Global => "global".to_owned(),
            Self::Workspace(path) => format!("workspace:{}", path.to_string_lossy()),
        }
    }

    /// Parses the stored spelling. `None` for anything this build does not understand: an
    /// unknown scope must make its rule inert, never apply it somewhere else.
    #[must_use]
    pub fn from_storage(raw: &str) -> Option<Self> {
        if raw == "global" {
            return Some(Self::Global);
        }
        raw.strip_prefix("workspace:")
            .filter(|path| !path.is_empty())
            .map(|path| Self::Workspace(PathBuf::from(path)))
    }
}

/// One persisted "always allow" / "always deny" decision (D8).
///
/// Only [`ApprovalOption::AllowAlways`] and [`ApprovalOption::DenyAlways`] are meaningful here;
/// a once-decision is not a rule. `matcher` is a tool name in v1 — the UI's "always" options
/// offer a tool-wide scope, so persisting anything finer would record a decision the user was
/// never shown (see `docs/design/capabilities.md` §5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRule {
    /// Primary key.
    pub id: crate::id::ApprovalRuleId,
    /// Where it applies.
    pub scope: ApprovalScope,
    /// Tool name this rule matches.
    pub matcher: String,
    /// Allow or deny; only the remembered variants are valid.
    pub decision: ApprovalOption,
    /// When it was recorded.
    pub created_at: crate::time::Timestamp,
}

impl ApprovalRule {
    /// True for the two variants a rule may carry.
    #[must_use]
    pub fn is_valid_decision(decision: ApprovalOption) -> bool {
        matches!(
            decision,
            ApprovalOption::AllowAlways | ApprovalOption::DenyAlways
        )
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

    #[test]
    fn a_request_without_a_preview_omits_the_field() {
        let request = ApprovalRequest::new("read_file", "read a.txt", RiskLevel::ReadOnly);
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(!json.contains("preview"), "{json}");
    }

    #[test]
    fn every_preview_variant_roundtrips_and_keeps_its_tag() {
        let previews = [
            (
                ApprovalPreview::Excerpt("https://example.test".to_owned()),
                "excerpt",
            ),
            (
                ApprovalPreview::Command {
                    argv: vec!["ls".to_owned(), "-la".to_owned()],
                    cwd: PathBuf::from("/tmp"),
                },
                "command",
            ),
            (
                ApprovalPreview::Diff(crate::diff::Diff { files: Vec::new() }),
                "diff",
            ),
        ];
        for (preview, tag) in previews {
            let json = serde_json::to_string(&preview).expect("serialize");
            assert!(json.contains(&format!("\"kind\":\"{tag}\"")), "{json}");
            assert_eq!(
                serde_json::from_str::<ApprovalPreview>(&json).expect("deserialize"),
                preview
            );
        }
    }

    #[test]
    fn a_rule_scope_has_one_storage_spelling() {
        assert_eq!(ApprovalScope::Global.as_storage(), "global");
        let workspace = ApprovalScope::Workspace(PathBuf::from("/work/项目"));
        assert_eq!(workspace.as_storage(), "workspace:/work/项目");
        assert_eq!(
            ApprovalScope::from_storage("global"),
            Some(ApprovalScope::Global)
        );
        assert_eq!(
            ApprovalScope::from_storage(&workspace.as_storage()),
            Some(workspace)
        );
        assert_eq!(ApprovalScope::from_storage("workspace:"), None);
        assert_eq!(ApprovalScope::from_storage("session:abc"), None);
    }

    #[test]
    fn only_remembered_decisions_are_valid_rules() {
        assert!(ApprovalRule::is_valid_decision(ApprovalOption::AllowAlways));
        assert!(ApprovalRule::is_valid_decision(ApprovalOption::DenyAlways));
        assert!(!ApprovalRule::is_valid_decision(ApprovalOption::AllowOnce));
        assert!(!ApprovalRule::is_valid_decision(ApprovalOption::Deny));
    }
}
