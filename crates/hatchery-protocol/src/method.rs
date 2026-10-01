//! Method names and their parameter/result types.
//!
//! The constants are the single source of truth for the wire spelling: the daemon's router, the
//! CLI client and the ACP bridge all name `method::SESSION_PROMPT` instead of a string literal, so
//! a rename cannot leave one side behind. `the_method_table_matches_the_design` locks the list
//! against `docs/design/protocol.md` §3.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::approval::{ApprovalOption, ApprovalRequest};
use crate::content::Content;
use crate::id::{ApprovalId, ItemId, SessionId, TurnId};
use crate::item::{Item, ItemKindTag};
use crate::session::{
    ModelRef, ReasoningEffort, RewindScope, Session, SessionListFilter, SessionModeId,
};
use crate::time::Timestamp;

// ------------------------------------------------------------- client → daemon

/// Handshake: exchange versions and capabilities.
pub const DAEMON_HELLO: &str = "daemon/hello";
/// Create a session.
pub const SESSION_NEW: &str = "session/new";
/// Load a session and replay its active branch.
pub const SESSION_LOAD: &str = "session/load";
/// List sessions, newest first.
pub const SESSION_LIST: &str = "session/list";
/// Submit a user turn.
pub const SESSION_PROMPT: &str = "session/prompt";
/// Interrupt the running turn.
pub const SESSION_CANCEL: &str = "session/cancel";
/// Change the session mode (effective at the next turn boundary).
pub const SESSION_SET_MODE: &str = "session/set_mode";
/// Change per-session configuration.
pub const SESSION_SET_CONFIG: &str = "session/set_config";
/// Edit a history item, which forks the branch (ADR-0003).
pub const SESSION_EDIT_ITEM: &str = "session/edit_item";
/// List the branches of a session's item tree.
pub const SESSION_BRANCH_LIST: &str = "session/branch/list";
/// Move the active branch pointer.
pub const SESSION_BRANCH_SWITCH: &str = "session/branch/switch";
/// Delete a branch and everything below it.
pub const SESSION_BRANCH_DELETE: &str = "session/branch/delete";
/// Roll history and/or the working tree back.
pub const SESSION_REWIND: &str = "session/rewind";
/// Delete a session.
pub const SESSION_DELETE: &str = "session/delete";
/// Retitle a session.
pub const SESSION_RENAME: &str = "session/rename";
/// Answer a pending approval request.
pub const APPROVAL_RESPOND: &str = "approval/respond";
/// Read configuration with per-key origins.
pub const CONFIG_GET: &str = "config/get";
/// Write runtime configuration overrides.
pub const CONFIG_SET: &str = "config/set";
/// Render the prompt that would be sent, for transparency.
pub const PROMPT_RENDER: &str = "prompt/render";
/// Export a session as JSONL (ADR-0002's escape hatch).
pub const STORE_EXPORT_JSONL: &str = "store/export_jsonl";

/// Every method, in the order `docs/design/protocol.md` §3 lists them.
pub const ALL: &[&str] = &[
    DAEMON_HELLO,
    SESSION_NEW,
    SESSION_LOAD,
    SESSION_LIST,
    SESSION_PROMPT,
    SESSION_CANCEL,
    SESSION_SET_MODE,
    SESSION_SET_CONFIG,
    SESSION_EDIT_ITEM,
    SESSION_BRANCH_LIST,
    SESSION_BRANCH_SWITCH,
    SESSION_BRANCH_DELETE,
    SESSION_REWIND,
    SESSION_DELETE,
    SESSION_RENAME,
    APPROVAL_RESPOND,
    CONFIG_GET,
    CONFIG_SET,
    PROMPT_RENDER,
    STORE_EXPORT_JSONL,
];

// ---------------------------------------------------------------- daemon/hello

/// Who is calling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Client name, e.g. `hatchery-cli`.
    pub name: String,
    /// Client version.
    pub version: String,
}

/// Parameters of [`DAEMON_HELLO`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    /// The version the client speaks.
    pub protocol_version: String,
    /// Identifying the caller, for logs and for frontend-specific behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientInfo>,
    /// The daemon's boot token, from `daemon.json` (0600): proof the caller could read the
    /// user's own state directory. Additive field; a peer that does not send one simply sends
    /// none, and the daemon decides whether that is acceptable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_token: Option<String>,
}

/// What a daemon can do, for a frontend that wants to degrade gracefully.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonCapabilities {
    /// Method names the daemon serves, e.g. `session/branch/delete`.
    pub methods: Vec<String>,
    /// Session modes this daemon can offer, built-in and configured (ADR-0005).
    pub modes: Vec<SessionModeId>,
}

/// Result of [`DAEMON_HELLO`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    /// The version the daemon speaks.
    pub protocol_version: String,
    /// The daemon's own version, for bug reports.
    pub daemon_version: String,
    /// Versions the daemon accepts.
    pub supported_versions: Vec<String>,
    /// What it can do.
    pub capabilities: DaemonCapabilities,
}

// ----------------------------------------------------------------- session/new

/// Parameters of [`SESSION_NEW`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionNewParams {
    /// Chat, code, or a configured mode.
    pub mode: SessionModeId,
    /// Workspace root for Code sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// Model to use; the daemon picks the configured default when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// Initial title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Initial session-level configuration overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_patch: Option<Value>,
}

/// Result of [`SESSION_NEW`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionNewResult {
    /// The new session.
    pub session: Session,
}

// ---------------------------------------------------------------- session/load

/// Parameters of [`SESSION_LOAD`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLoadParams {
    /// Which session to load.
    pub session_id: SessionId,
    /// Replay only items after this one. Absent means the whole active branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_from: Option<ItemId>,
    /// The generation the caller last saw; a mismatch is refused (invariant 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
}

/// Result of [`SESSION_LOAD`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionLoadResult {
    /// The session as of now.
    pub session: Session,
    /// Items of the active branch, oldest first.
    pub items: Vec<Item>,
    /// Set when more items remain: call again with `replay_from` set to it. `None` means the
    /// replay is complete and the live stream takes over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<ItemId>,
}

// ---------------------------------------------------------------- session/list

/// Parameters of [`SESSION_LIST`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListParams {
    /// Opaque cursor from a previous page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Page size; the daemon clamps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Restrict the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<SessionListFilter>,
}

/// Result of [`SESSION_LIST`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionListResult {
    /// Sessions, most recently updated first.
    pub sessions: Vec<Session>,
    /// Cursor for the next page; `None` when the list is exhausted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// -------------------------------------------------------------- session/prompt

/// Parameters of [`SESSION_PROMPT`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionPromptParams {
    /// Which session to prompt.
    pub session_id: SessionId,
    /// What the user said.
    pub content: Content,
    /// The generation the caller believes is current; a mismatch is refused rather than applied
    /// to a runtime the caller does not own (invariant 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
}

/// Result of [`SESSION_PROMPT`]: the turn was accepted, not finished.
///
/// Everything else about the turn arrives as events on the session's stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPromptResult {
    /// The turn now running.
    pub turn: TurnId,
}

// -------------------------------------------------------------- session/cancel

/// Parameters of [`SESSION_CANCEL`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCancelParams {
    /// Which session to interrupt.
    pub session_id: SessionId,
}

/// Result of [`SESSION_CANCEL`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCancelResult {
    /// True when a turn was running and got cancelled; false when there was nothing to cancel.
    pub cancelled: bool,
}

// ------------------------------------------------------------- session/set_mode

/// Parameters of [`SESSION_SET_MODE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetModeParams {
    /// Which session.
    pub session_id: SessionId,
    /// The new mode.
    pub mode: SessionModeId,
}

/// Result of [`SESSION_SET_MODE`]. The change applies at the next turn boundary, so the
/// returned session already carries the new mode (ADR-0005).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetModeResult {
    /// The session as it is now.
    pub session: Session,
}

// ----------------------------------------------------------- session/set_config

/// A per-session configuration change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigPatch {
    /// Switch provider/model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// Change the reasoning effort (ADR-0007).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Free-form overrides merged over the layered configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Value>,
}

/// Parameters of [`SESSION_SET_CONFIG`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetConfigParams {
    /// Which session.
    pub session_id: SessionId,
    /// What to change.
    pub patch: ConfigPatch,
}

/// Result of [`SESSION_SET_CONFIG`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetConfigResult {
    /// The session as it is now.
    pub session: Session,
}

// ------------------------------------------------------------ session/edit_item

/// Parameters of [`SESSION_EDIT_ITEM`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditItemParams {
    /// The item to edit.
    pub item_id: ItemId,
    /// The replacement content.
    pub new_content: Content,
    /// Also roll the working tree back to the matching checkpoint (ADR-0006).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewind_scope: Option<RewindScope>,
}

/// Result of [`SESSION_EDIT_ITEM`].
///
/// Editing forks: the new item becomes the branch head and the old branch is kept, so a mistaken
/// edit is undone by switching back (ADR-0003).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditItemResult {
    /// Head of the new branch.
    pub branch_head: ItemId,
    /// The session as it is now.
    pub session: Session,
}

// --------------------------------------------------------- session/branch/*

/// Parameters of [`SESSION_BRANCH_LIST`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchListParams {
    /// Which session.
    pub session_id: SessionId,
}

/// One node of a session's item tree, for a branch view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchNode {
    /// Item id.
    pub id: ItemId,
    /// Parent in the tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ItemId>,
    /// Owning turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnId>,
    /// Kind of the item.
    pub kind: ItemKindTag,
    /// When it was appended.
    pub created_at: Timestamp,
    /// True for the items on the active branch (the chain the model would see).
    pub active: bool,
}

/// Result of [`SESSION_BRANCH_LIST`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchListResult {
    /// Every item of the session, so the frontend can draw the tree. Ordered by creation.
    pub nodes: Vec<BranchNode>,
}

/// Parameters of [`SESSION_BRANCH_SWITCH`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchSwitchParams {
    /// Which session.
    pub session_id: SessionId,
    /// The item to make active.
    pub head: ItemId,
}

/// Result of [`SESSION_BRANCH_SWITCH`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BranchSwitchResult {
    /// The session as it is now.
    pub session: Session,
}

/// Parameters of [`SESSION_BRANCH_DELETE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchDeleteParams {
    /// Which session.
    pub session_id: SessionId,
    /// Root of the subtree to delete.
    pub head: ItemId,
    /// Second confirmation. Deletion is not recoverable, so the daemon refuses without it.
    pub confirm: bool,
}

/// Result of [`SESSION_BRANCH_DELETE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchDeleteResult {
    /// How many items were removed, the subtree included.
    pub deleted_items: u64,
}

// -------------------------------------------------------------- session/rewind

/// Parameters of [`SESSION_REWIND`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindParams {
    /// Which session.
    pub session_id: SessionId,
    /// The item to roll back to.
    pub target_item: ItemId,
    /// Whether history, code, or both move (ADR-0003).
    pub scope: RewindScope,
    /// Also delete files that appeared after the checkpoint and were never tracked. Needs the
    /// user's explicit consent: the default `false` never touches their untracked files
    /// (`docs/design/capabilities.md` §2).
    #[serde(default)]
    pub purge_untracked: bool,
}

/// What a rewind actually changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindReport {
    /// Workspace files restored from the checkpoint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rolled_back: Vec<PathBuf>,
    /// Never-tracked files removed, only with `purge_untracked`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub purged: Vec<PathBuf>,
}

/// Result of [`SESSION_REWIND`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RewindResult {
    /// The session as it is now.
    pub session: Session,
    /// What changed on disk.
    pub report: RewindReport,
}

// ------------------------------------------------------- session/delete,rename

/// Parameters of [`SESSION_DELETE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDeleteParams {
    /// Which session to delete, items included.
    pub session_id: SessionId,
}

/// Result of [`SESSION_DELETE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDeleteResult {
    /// How many items were removed with it.
    pub deleted_items: u64,
}

/// Parameters of [`SESSION_RENAME`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRenameParams {
    /// Which session.
    pub session_id: SessionId,
    /// The new title; `None` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Result of [`SESSION_RENAME`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionRenameResult {
    /// The session as it is now.
    pub session: Session,
}

// ----------------------------------------------------------- approval/respond

/// Parameters of [`APPROVAL_RESPOND`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRespondParams {
    /// The `request_id` from the `ApprovalRequested` event.
    pub request_id: ApprovalId,
    /// Which offered option the user chose.
    pub option: ApprovalOption,
}

/// Result of [`APPROVAL_RESPOND`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRespondResult {
    /// False when no such request is pending — already answered, or timed out. The answer is
    /// accepted idempotently rather than erroring, because two frontends may race.
    pub handled: bool,
}

/// The full request behind an `ApprovalRequested` event, for a frontend that needs to re-render
/// it (e.g. after a reconnect).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    /// The pending request.
    pub request_id: ApprovalId,
    /// Which session it belongs to.
    pub session_id: SessionId,
    /// What is being asked.
    pub request: ApprovalRequest,
}

// -------------------------------------------------------------- config/get,set

/// Where a configuration value came from (`docs/design/platform.md` §1.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigOrigin {
    /// Compiled-in default.
    Builtin,
    /// `/etc/hatchery/config.toml`.
    System,
    /// `~/.config/hatchery/config.toml`.
    User,
    /// `<workspace>/.hatchery/config.toml`.
    Project,
    /// A runtime override: CLI flag, `config/set`, or the session's config patch.
    Runtime,
}

/// One resolved configuration value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigEntry {
    /// Dotted key path, e.g. `providers.deepseek.base_url`.
    pub key: String,
    /// The effective value.
    pub value: Value,
    /// Which layer supplied it.
    pub origin: ConfigOrigin,
}

/// Parameters of [`CONFIG_GET`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigGetParams {
    /// A single key, or a prefix. Absent returns everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
}

/// Result of [`CONFIG_GET`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigGetResult {
    /// Matching entries with their origins.
    pub entries: Vec<ConfigEntry>,
}

/// Parameters of [`CONFIG_SET`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigSetParams {
    /// Dotted key path.
    pub key_path: String,
    /// The new value.
    pub value: Value,
}

/// Result of [`CONFIG_SET`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigSetResult {
    /// The value as it is now, with its origin.
    pub entry: ConfigEntry,
}

// --------------------------------------------------------------- prompt/render

/// One section of the assembled prompt, so the user can see where every word came from
/// (`docs/design/platform.md` §2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSection {
    /// Section id, e.g. `identity`.
    pub id: String,
    /// Where the text came from: `builtin`, a file path, or `project:AGENTS.md`.
    pub source: String,
    /// The section's text.
    pub text: String,
}

/// Parameters of [`PROMPT_RENDER`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRenderParams {
    /// Render as this session would.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Or render for a bare mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SessionModeId>,
}

/// Result of [`PROMPT_RENDER`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRenderResult {
    /// The prompt as it would be sent.
    pub text: String,
    /// The same text, section by section.
    pub sections: Vec<PromptSection>,
}

// -------------------------------------------------------- store/export_jsonl

/// Parameters of [`STORE_EXPORT_JSONL`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportJsonlParams {
    /// Which session to export.
    pub session_id: SessionId,
    /// Where to write. The daemon refuses to overwrite an existing file.
    pub path: PathBuf,
    /// Export every branch instead of just the active one.
    #[serde(default)]
    pub all_branches: bool,
}

/// Result of [`STORE_EXPORT_JSONL`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportJsonlResult {
    /// The file that was written.
    pub path: PathBuf,
    /// How many items it contains.
    pub lines: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_method_table_matches_the_design() {
        // Transcribed from docs/design/protocol.md §3. A rename must update the design document
        // and this list together — that is the point of the test.
        let expected = [
            "daemon/hello",
            "session/new",
            "session/load",
            "session/list",
            "session/prompt",
            "session/cancel",
            "session/set_mode",
            "session/set_config",
            "session/edit_item",
            "session/branch/list",
            "session/branch/switch",
            "session/branch/delete",
            "session/rewind",
            "session/delete",
            "session/rename",
            "approval/respond",
            "config/get",
            "config/set",
            "prompt/render",
            "store/export_jsonl",
        ];
        assert_eq!(ALL, expected);
    }

    #[test]
    fn method_names_are_unique_and_namespaced() {
        let mut sorted = ALL.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ALL.len(), "duplicate method name");

        for name in ALL {
            let mut segments = name.split('/');
            let namespace = segments.next().expect("split always yields one segment");
            assert!(!namespace.is_empty(), "{name} has an empty namespace");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c == '/'),
                "{name} is not lowercase snake_case"
            );
            assert!(
                !name.contains("//") && !name.ends_with('/'),
                "{name} has an empty segment"
            );
            assert!(name.matches('/').count() >= 1, "{name} is not namespaced");
        }
    }

    #[test]
    fn params_and_results_roundtrip_through_json() {
        let params = SessionPromptParams {
            session_id: SessionId::new(),
            content: Content::text("hi"),
            generation: Some(2),
        };
        let json = serde_json::to_string(&params).expect("serialize");
        assert_eq!(
            serde_json::from_str::<SessionPromptParams>(&json).expect("deserialize"),
            params
        );

        // Optional fields must be absent, not null: a null would have to be distinguished from
        // "not supplied" on the daemon side.
        let minimal = SessionListParams::default();
        assert_eq!(
            serde_json::to_string(&minimal).expect("serialize"),
            "{}",
            "absent optional fields must not be written as null"
        );
    }

    #[test]
    fn a_missing_optional_field_deserializes_to_none() {
        let params: SessionListParams = serde_json::from_str("{}").expect("deserialize");
        assert_eq!(params, SessionListParams::default());
        // And an unknown field from a newer client is ignored rather than fatal: methods and
        // fields may only be added (docs/design/protocol.md §6).
        let forward: SessionListParams =
            serde_json::from_str(r#"{"limit":5,"invented_later":true}"#).expect("deserialize");
        assert_eq!(forward.limit, Some(5));
    }

    #[test]
    fn a_hello_roundtrips_with_and_without_a_client_block() {
        let params = HelloParams {
            protocol_version: crate::PROTOCOL_VERSION.to_owned(),
            client: Some(ClientInfo {
                name: "hatchery-cli".to_owned(),
                version: "0.1.0".to_owned(),
            }),
            boot_token: Some("boot-1234".to_owned()),
        };
        let json = serde_json::to_string(&params).expect("serialize");
        assert_eq!(
            serde_json::from_str::<HelloParams>(&json).expect("deserialize"),
            params
        );

        let anonymous = HelloParams {
            protocol_version: crate::PROTOCOL_VERSION.to_owned(),
            client: None,
            boot_token: None,
        };
        assert!(
            !serde_json::to_string(&anonymous)
                .expect("serialize")
                .contains("client")
        );
    }

    #[test]
    fn config_origins_name_their_layer() {
        assert_eq!(
            serde_json::to_string(&ConfigOrigin::Project).expect("serialize"),
            "\"project\""
        );
        for (origin, name) in [
            (ConfigOrigin::Builtin, "builtin"),
            (ConfigOrigin::System, "system"),
            (ConfigOrigin::User, "user"),
            (ConfigOrigin::Project, "project"),
            (ConfigOrigin::Runtime, "runtime"),
        ] {
            assert_eq!(
                serde_json::to_string(&origin).expect("serialize"),
                format!("\"{name}\"")
            );
        }
    }

    #[test]
    fn a_rewind_report_omits_empty_lists() {
        let result = RewindResult {
            session: Session {
                id: SessionId::new(),
                title: None,
                mode: SessionModeId::code(),
                workspace: None,
                model: ModelRef::new("deepseek", "deepseek-chat"),
                config_patch: None,
                created_at: Timestamp::UNIX_EPOCH,
                updated_at: Timestamp::UNIX_EPOCH,
                active_branch_head: None,
                generation: 0,
                status: crate::SessionStatus::Idle,
            },
            report: RewindReport::default(),
        };
        let json = serde_json::to_string(&result).expect("serialize");
        assert!(!json.contains("rolled_back"), "{json}");
        assert!(!json.contains("purged"), "{json}");
    }
}
