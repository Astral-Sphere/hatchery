//! Sessions: the metadata around one conversation and its item tree.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::id::{ItemId, SessionId};
use crate::time::Timestamp;

/// Which mode a session runs in.
///
/// A `String` newtype, not an enum: ADR-0005 lets users declare custom modes in `config.toml`,
/// so the built-in pair is a convention rather than a closed set. Compare with
/// [`SessionModeId::as_str`] against [`SessionModeId::CHAT`] / [`SessionModeId::CODE`], or with
/// [`SessionModeId::is_builtin`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionModeId(String);

impl SessionModeId {
    /// Name of the built-in Chat mode: read-only tools, no checkpoints.
    pub const CHAT: &'static str = "chat";
    /// Name of the built-in Code mode: write and shell tools, approvals, checkpoints.
    pub const CODE: &'static str = "code";

    /// The built-in Chat mode.
    #[must_use]
    pub fn chat() -> Self {
        Self(Self::CHAT.to_owned())
    }

    /// The built-in Code mode.
    #[must_use]
    pub fn code() -> Self {
        Self(Self::CODE.to_owned())
    }

    /// A mode by name — built-in or declared by the user (ADR-0005).
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The name, as used in configuration and on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True for `chat` and `code`.
    #[must_use]
    pub fn is_builtin(&self) -> bool {
        matches!(self.0.as_str(), Self::CHAT | Self::CODE)
    }
}

impl fmt::Display for SessionModeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SessionModeId {
    type Err = std::convert::Infallible;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(Self(name.to_owned()))
    }
}

impl From<String> for SessionModeId {
    fn from(name: String) -> Self {
        Self(name)
    }
}

impl From<&str> for SessionModeId {
    fn from(name: &str) -> Self {
        Self(name.to_owned())
    }
}

/// Which provider and model a session talks to.
///
/// `reasoning_effort` is deliberately *not* here: it is a provider capability (ADR-0007), it can
/// be changed per session without changing the model, and it travels in the session config patch.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    /// Provider id from `config.toml`'s `[providers.*]` table, e.g. `deepseek`.
    pub provider: String,
    /// Model id as the provider names it, e.g. `deepseek-reasoner`.
    pub model: String,
}

impl ModelRef {
    /// A provider/model pair.
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

/// The canonical reasoning-effort ladder (ADR-0007).
///
/// Provider-agnostic on purpose: each provider family maps these onto its own wire field (a
/// different model, `enable_thinking`, a thinking budget, …). The mapping table lives in
/// `hatchery-llm`, not here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// Turn reasoning off (DeepSeek: switch to the non-reasoning model).
    Off,
    /// Light reasoning.
    Low,
    /// Provider default.
    #[default]
    Medium,
    /// Deep reasoning.
    High,
    /// The most the provider offers.
    Max,
}

/// What a session is doing right now.
///
/// Projected from the runtime rather than owned by the frontend: closing a terminal must not
/// change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// No turn in flight.
    Idle,
    /// A turn is running.
    Running,
    /// A turn is blocked on user approval.
    WaitingApproval,
    /// The last turn failed; the session is still usable.
    Error,
}

/// One session: the metadata a frontend needs, without its items.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Session id (UUIDv7).
    pub id: SessionId,
    /// Human-readable title. `None` until the daemon derives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Chat, code, or a user-declared mode (ADR-0005).
    pub mode: SessionModeId,
    /// The workspace root, for Code sessions. `None` in Chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// Provider and model.
    pub model: ModelRef,
    /// The session-level override document, merged over the layered configuration
    /// (`docs/design/platform.md` §1). Stored verbatim: it is the user's own shape, and the daemon
    /// is what gives it meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_patch: Option<Value>,
    /// When the session was created.
    pub created_at: Timestamp,
    /// When the session last changed — `session/list` is ordered by this.
    pub updated_at: Timestamp,
    /// Head of the active branch, i.e. the newest item the model will see. `None` until the
    /// first item exists: with foreign keys enforced, a session row cannot point at an item
    /// that does not exist yet, so "no items" must be representable (ADR-0010).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_branch_head: Option<ItemId>,
    /// Runtime generation (invariant 1): bumped whenever a new runtime takes the session over,
    /// so clients can drop a stale runtime's late events.
    pub generation: u64,
    /// Current status.
    pub status: SessionStatus,
}

impl Session {
    /// True when the session has no items yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active_branch_head.is_none()
    }
}

/// Reads a clearable text field back the way [`SessionPatch`] writes it.
///
/// Serde maps JSON `null` onto the *outer* option, so the default deserialization of
/// `Option<Option<String>>` would read `{"title":null}` as "leave the title alone" — exactly the
/// distinction the double option exists to carry. Here: absent stays `None`, `null` becomes
/// `Some(None)`, a string becomes `Some(Some(_))`, anything else is an error.
fn clearable_text<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ClearableText;

    impl<'de> serde::de::Visitor<'de> for ClearableText {
        type Value = Option<Option<String>>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a string, or null to clear")
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Some(None))
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Some(None))
        }

        fn visit_some<D: Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            String::deserialize(deserializer).map(|text| Some(Some(text)))
        }
    }

    deserializer.deserialize_option(ClearableText)
}

/// A partial update to a session's metadata.
///
/// Double options where "clear it" is a real operation: `title: None` means "leave alone" and
/// `title: Some(None)` means "remove the title". Session identity (`id`, `created_at`) and
/// runtime-owned fields (`generation`, `active_branch_head`) are not patchable.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionPatch {
    /// New title, or `Some(None)` to clear it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "clearable_text"
    )]
    pub title: Option<Option<String>>,
    /// New mode. Takes effect at the next turn boundary (ADR-0005).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SessionModeId>,
    /// New provider/model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// New status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SessionStatus>,
    /// Replacement for the session-level override document. Stored verbatim; the daemon merges
    /// it over the layered configuration when reading (`docs/design/platform.md` §1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_patch: Option<Value>,
}

impl SessionPatch {
    /// True when the patch would change nothing — the store skips the write.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.mode.is_none()
            && self.model.is_none()
            && self.status.is_none()
            && self.config_patch.is_none()
    }
}

/// Which checkpoints the shadow Git store keeps, matching `checkpoints.kind` in the schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    /// Taken before a write tool touches the workspace.
    PreWrite,
    /// Taken before a shell command runs.
    PreShell,
    /// Asked for by the user.
    Manual,
}

/// How much a rewind rolls back (ADR-0003, ADR-0006).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewindScope {
    /// Move the branch pointer; leave the working tree alone.
    Conversation,
    /// Restore files from a shadow-Git checkpoint; leave history alone.
    Code,
    /// Both.
    Both,
}

/// Filter for `session/list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListFilter {
    /// Only sessions in this mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SessionModeId>,
    /// Only sessions for this workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// Case-insensitive substring match on the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_contains: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session {
            id: SessionId::new(),
            title: None,
            mode: SessionModeId::code(),
            workspace: Some(PathBuf::from("/ws")),
            model: ModelRef::new("deepseek", "deepseek-reasoner"),
            config_patch: None,
            created_at: Timestamp::from_unix_millis(10),
            updated_at: Timestamp::from_unix_millis(20),
            active_branch_head: None,
            generation: 0,
            status: SessionStatus::Idle,
        }
    }

    #[test]
    fn a_fresh_session_has_no_head_and_no_title() {
        let s = session();
        assert!(s.is_empty());
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(!json.contains("title"), "{json}");
        assert!(!json.contains("active_branch_head"), "{json}");
        assert_eq!(
            serde_json::from_str::<Session>(&json).expect("deserialize"),
            s
        );
    }

    #[test]
    fn mode_roundtrips_as_a_bare_string() {
        assert_eq!(
            serde_json::to_string(&SessionModeId::code()).expect("serialize"),
            "\"code\""
        );
        let custom = SessionModeId::new("review-only");
        assert!(!custom.is_builtin());
        assert!(SessionModeId::chat().is_builtin());
        assert_eq!(custom.as_str(), "review-only");
    }

    #[test]
    fn a_patch_distinguishes_leave_alone_from_clear() {
        let untouched = SessionPatch::default();
        assert!(untouched.is_empty());

        let cleared = SessionPatch {
            title: Some(None),
            ..SessionPatch::default()
        };
        assert!(!cleared.is_empty());
        assert_eq!(
            serde_json::to_string(&cleared).expect("serialize"),
            r#"{"title":null}"#,
            "clearing the title must be expressible on the wire"
        );
        assert_eq!(
            serde_json::to_string(&untouched).expect("serialize"),
            "{}",
            "an absent field means 'leave alone'"
        );

        // Reading is the other half of the contract: serde would map `null` onto the outer
        // option, silently turning "clear it" back into "leave it alone".
        let read = |json: &str| {
            serde_json::from_str::<SessionPatch>(json)
                .unwrap_or_else(|error| panic!("{json} did not deserialize: {error}"))
                .title
        };
        assert_eq!(
            read(r#"{"title":null}"#),
            Some(None),
            "a null title must read back as 'clear it'"
        );
        assert_eq!(
            read("{}"),
            None,
            "an absent title must read back as 'leave it alone'"
        );
        assert_eq!(
            read(r#"{"title":"renamed"}"#),
            Some(Some("renamed".to_owned()))
        );
        assert_eq!(
            serde_json::from_str::<SessionPatch>(
                &serde_json::to_string(&cleared).expect("serialize")
            )
            .expect("deserialize"),
            cleared,
            "a clearing patch must survive a round trip"
        );
        assert!(
            serde_json::from_str::<SessionPatch>(r#"{"title":5}"#).is_err(),
            "a title that is neither a string nor null must not be guessed at"
        );
    }

    #[test]
    fn status_and_checkpoint_kinds_use_the_schema_spelling() {
        assert_eq!(
            serde_json::to_string(&SessionStatus::WaitingApproval).expect("serialize"),
            "\"waiting_approval\""
        );
        assert_eq!(
            serde_json::to_string(&CheckpointKind::PreShell).expect("serialize"),
            "\"pre_shell\"",
            "must match the checkpoints.kind column comment in docs/design/storage.md §2"
        );
    }

    #[test]
    fn list_filter_omits_absent_terms() {
        let filter = SessionListFilter {
            title_contains: Some("refactor".to_owned()),
            ..SessionListFilter::default()
        };
        assert_eq!(
            serde_json::to_string(&filter).expect("serialize"),
            r#"{"title_contains":"refactor"}"#
        );
    }
}
