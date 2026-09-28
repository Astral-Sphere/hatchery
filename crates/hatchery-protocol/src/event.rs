//! Server events: what the daemon pushes to attached frontends.
//!
//! Two enums, not one: a session-scoped event rides inside a [`SessionEvent`] envelope that
//! carries the session id and the runtime generation, while daemon-wide news has neither. Mixing
//! them into one enum would force every variant to carry a generation field it cannot use, and
//! would make the client-side invariant-1 filter ("drop events below the generation I last saw")
//! read fourteen fields instead of one.

use serde::{Deserialize, Serialize};

use crate::approval::ApprovalRequest;
use crate::error::EventError;
use crate::id::{ApprovalId, ItemId, SessionId, TurnId};
use crate::item::{Item, ItemStub};
use crate::session::{Session, SessionModeId};
use crate::tool::ToolCallSummary;
use crate::usage::TurnCompletion;

/// A session-scoped event, plus the coordinates a frontend filters on.
///
/// The event's own fields are flattened next to `session` and `generation`, so a frame looks like
/// `{"session":"…","generation":3,"type":"text_delta","item":"…","text":"…"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    /// Which session the event belongs to. Every attached frontend gets its own copy (the live
    /// hub fans out; `docs/design/protocol.md` §4).
    pub session: SessionId,
    /// The runtime generation that produced the event (invariant 1). A client drops events whose
    /// generation is below the highest it has seen, so a replaced runtime's stragglers cannot
    /// corrupt the view.
    pub generation: u64,
    /// What happened.
    #[serde(flatten)]
    pub event: ServerEvent,
}

impl SessionEvent {
    /// Wraps an event for a session and generation.
    #[must_use]
    pub const fn new(session: SessionId, generation: u64, event: ServerEvent) -> Self {
        Self {
            session,
            generation,
            event,
        }
    }

    /// The event's wire type name, e.g. `text_delta`.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        self.event.type_name()
    }
}

/// Something that happened inside one session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// An item was created; its payload follows through the deltas and `ItemFinished`.
    ItemStarted {
        /// Position of the new item in the tree.
        item: ItemStub,
    },
    /// More assistant text arrived. High frequency; the daemon coalesces these before sending
    /// (M1).
    TextDelta {
        /// The item being appended to.
        item: ItemId,
        /// The increment, not the whole text.
        text: String,
    },
    /// More reasoning arrived (ADR-0007: displayed, and replayed verbatim to the provider).
    ReasoningDelta {
        /// The item being appended to.
        item: ItemId,
        /// The increment, verbatim.
        text: String,
    },
    /// An item is complete and committed to the store.
    ItemFinished {
        /// The finished item, payload included.
        item: Item,
    },
    /// A tool call began, with a summary a human can read.
    ToolCallStarted {
        /// The tool call item.
        item: ItemId,
        /// One or two lines describing the call.
        summary: ToolCallSummary,
    },
    /// A tool emitted progress.
    ToolCallProgress {
        /// The tool call item.
        item: ItemId,
        /// Incremental output.
        chunk: String,
    },
    /// The agent needs a decision before it can continue. Answer with the `approval/respond`
    /// method, echoing `request_id`.
    ApprovalRequested {
        /// Correlates this prompt with the answer.
        request_id: ApprovalId,
        /// What is being asked and which options to offer.
        request: ApprovalRequest,
    },
    /// A turn ended normally.
    TurnFinished {
        /// The turn that ended.
        turn: TurnId,
        /// Why it ended and what it cost.
        completion: TurnCompletion,
    },
    /// A turn ended with an error.
    TurnFailed {
        /// The turn that failed.
        turn: TurnId,
        /// What went wrong.
        error: EventError,
    },
    /// Session metadata changed: title, mode, status, head.
    SessionUpdated {
        /// The session as it is now; a client replaces its cached copy with this.
        ///
        /// Named `state`, not `session`: the envelope already owns `session` (the id), and a
        /// duplicate key would make the frame undecodable.
        state: Session,
    },
    /// The mode changed at a turn boundary (ADR-0005).
    ModeSwitched {
        /// Mode before.
        from: SessionModeId,
        /// Mode after. Effective from the next turn.
        to: SessionModeId,
    },
    /// The provider asked us to slow down. The daemon retries by itself; the frontend only shows
    /// the countdown (ADR-0007).
    RateLimited {
        /// How long to wait before the retry.
        retry_after_ms: u64,
    },
    /// A new runtime took the session over; events below this generation are stale.
    ///
    /// Carries no payload: the new generation *is* the envelope's `generation`, because this
    /// event is by definition the first one produced by the new runtime.
    GenerationBumped,
}

impl ServerEvent {
    /// The event's wire type name, matching the serde tag.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::ItemStarted { .. } => "item_started",
            Self::TextDelta { .. } => "text_delta",
            Self::ReasoningDelta { .. } => "reasoning_delta",
            Self::ItemFinished { .. } => "item_finished",
            Self::ToolCallStarted { .. } => "tool_call_started",
            Self::ToolCallProgress { .. } => "tool_call_progress",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::TurnFinished { .. } => "turn_finished",
            Self::TurnFailed { .. } => "turn_failed",
            Self::SessionUpdated { .. } => "session_updated",
            Self::ModeSwitched { .. } => "mode_switched",
            Self::RateLimited { .. } => "rate_limited",
            Self::GenerationBumped => "generation_bumped",
        }
    }

    /// True for the streaming deltas, which the daemon may coalesce before sending.
    ///
    /// Control events must never be coalesced or reordered (`docs/design/testing.md` §3.6), so
    /// the hub needs to be able to tell the two apart.
    #[must_use]
    pub const fn is_coalescable(&self) -> bool {
        matches!(self, Self::TextDelta { .. } | Self::ReasoningDelta { .. })
    }

    /// True when the event ends the turn.
    #[must_use]
    pub const fn ends_turn(&self) -> bool {
        matches!(self, Self::TurnFinished { .. } | Self::TurnFailed { .. })
    }
}

/// Daemon-wide news, not tied to a session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonEvent {
    /// The daemon is going away; frontends should detach rather than reconnect.
    DaemonShuttingDown {
        /// Human-readable reason, for the log and the status line.
        reason: String,
    },
}

impl DaemonEvent {
    /// The event's wire type name, matching the serde tag.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::DaemonShuttingDown { .. } => "daemon_shutting_down",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::RiskLevel;
    use crate::content::Content;
    use crate::error::ErrorCode;
    use crate::item::{BranchNote, ItemKind};
    use crate::session::{ModelRef, SessionStatus};
    use crate::time::Timestamp;
    use crate::usage::StopReason;

    fn session_id() -> SessionId {
        SessionId::from_uuid(uuid::Uuid::from_u128(42))
    }

    fn item_id() -> ItemId {
        ItemId::from_uuid(uuid::Uuid::from_u128(43))
    }

    /// One event per variant. Golden fixtures and the mapping tests both iterate this.
    pub(crate) fn samples() -> Vec<ServerEvent> {
        let session = SessionId::new();
        let item = Item::new(session, ItemKind::UserMessage(Content::text("hi")));
        vec![
            ServerEvent::ItemStarted { item: item.stub() },
            ServerEvent::TextDelta {
                item: item.id,
                text: "hello".to_owned(),
            },
            ServerEvent::ReasoningDelta {
                item: item.id,
                text: "thinking".to_owned(),
            },
            ServerEvent::ItemFinished { item: item.clone() },
            ServerEvent::ToolCallStarted {
                item: item.id,
                summary: ToolCallSummary::new("read src/main.rs"),
            },
            ServerEvent::ToolCallProgress {
                item: item.id,
                chunk: "line 1\n".to_owned(),
            },
            ServerEvent::ApprovalRequested {
                request_id: ApprovalId::new(),
                request: ApprovalRequest::new(
                    "write_file",
                    "write src/lib.rs",
                    RiskLevel::WritesWorkspace,
                ),
            },
            ServerEvent::TurnFinished {
                turn: TurnId::new(),
                completion: TurnCompletion::new(StopReason::ModelDone),
            },
            ServerEvent::TurnFailed {
                turn: TurnId::new(),
                error: EventError::llm("connection reset", true),
            },
            ServerEvent::SessionUpdated {
                state: Session {
                    id: session,
                    title: Some("a session".to_owned()),
                    mode: SessionModeId::code(),
                    workspace: None,
                    model: ModelRef::new("deepseek", "deepseek-reasoner"),
                    config_patch: None,
                    created_at: Timestamp::from_unix_millis(1),
                    updated_at: Timestamp::from_unix_millis(2),
                    active_branch_head: Some(item.id),
                    generation: 1,
                    status: SessionStatus::Running,
                },
            },
            ServerEvent::ModeSwitched {
                from: SessionModeId::chat(),
                to: SessionModeId::code(),
            },
            ServerEvent::RateLimited {
                retry_after_ms: 1500,
            },
            ServerEvent::GenerationBumped,
        ]
    }

    #[test]
    fn every_variant_names_itself_in_snake_case() {
        for event in samples() {
            let name = event.type_name();
            let json = serde_json::to_value(&event).expect("serialize");
            assert_eq!(
                json.get("type").and_then(serde_json::Value::as_str),
                Some(name),
                "type_name() and the serde tag drifted apart for {name}"
            );
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{name} is not snake_case"
            );
        }

        let names: Vec<&str> = samples().iter().map(ServerEvent::type_name).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(names.len(), unique.len(), "duplicate event type names");
        assert_eq!(
            names.len(),
            13,
            "docs/design/protocol.md §4 lists thirteen session-scoped events"
        );
    }

    #[test]
    fn the_envelope_keeps_session_and_generation_at_the_top_level() {
        let event = SessionEvent::new(
            session_id(),
            3,
            ServerEvent::TextDelta {
                item: item_id(),
                text: "hi".to_owned(),
            },
        );
        let json = serde_json::to_value(&event).expect("serialize");
        let object = json.as_object().expect("envelope is an object");
        assert_eq!(
            object.get("session").and_then(serde_json::Value::as_str),
            Some(session_id().to_string().as_str())
        );
        assert_eq!(
            object.get("generation").and_then(serde_json::Value::as_u64),
            Some(3)
        );
        assert_eq!(
            object.get("type").and_then(serde_json::Value::as_str),
            Some("text_delta"),
            "the event's tag must be flattened next to the envelope fields: {json}"
        );
        assert_eq!(
            serde_json::from_value::<SessionEvent>(json).expect("deserialize"),
            event
        );
    }

    #[test]
    fn every_enveloped_variant_roundtrips() {
        for event in samples() {
            let envelope = SessionEvent::new(session_id(), 7, event.clone());
            let json = serde_json::to_string(&envelope).expect("serialize");
            let back: SessionEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, envelope, "{} did not roundtrip", event.type_name());
        }
    }

    #[test]
    fn only_streaming_deltas_may_be_coalesced() {
        let coalescable: Vec<&str> = samples()
            .iter()
            .filter(|event| event.is_coalescable())
            .map(ServerEvent::type_name)
            .collect();
        assert_eq!(coalescable, vec!["text_delta", "reasoning_delta"]);
    }

    #[test]
    fn only_turn_finished_and_failed_end_a_turn() {
        let endings: Vec<&str> = samples()
            .iter()
            .filter(|event| event.ends_turn())
            .map(ServerEvent::type_name)
            .collect();
        assert_eq!(endings, vec!["turn_finished", "turn_failed"]);
    }

    #[test]
    fn no_event_field_collides_with_the_envelope() {
        // A duplicate JSON key (e.g. an event that also calls something `session`) silently
        // merges during serialization and fails during deserialization, so it must be caught
        // here rather than by whoever plugs in a frontend.
        for event in samples() {
            let json = serde_json::to_value(&event).expect("serialize");
            let object = json.as_object().expect("events are objects");
            for reserved in ["session", "generation"] {
                assert!(
                    !object.contains_key(reserved),
                    "{} has a field named {reserved}, which the envelope already owns",
                    event.type_name()
                );
            }
        }
    }

    #[test]
    fn daemon_events_roundtrip_without_a_session() {
        let event = DaemonEvent::DaemonShuttingDown {
            reason: "user asked".to_owned(),
        };
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(
            json.get("type").and_then(serde_json::Value::as_str),
            Some(event.type_name())
        );
        assert_eq!(
            serde_json::from_value::<DaemonEvent>(json).expect("deserialize"),
            event
        );
    }

    #[test]
    fn a_failed_turn_says_whether_retrying_could_help() {
        let event = SessionEvent::new(
            session_id(),
            1,
            ServerEvent::TurnFailed {
                turn: TurnId::new(),
                error: EventError::new(ErrorCode::StoreError, "disk on fire"),
            },
        );
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains(r#""code":"store_error""#), "{json}");
        assert!(
            !json.contains("retryable"),
            "a storage failure is not a retry question: {json}"
        );
    }

    #[test]
    fn item_started_carries_no_payload() {
        let stub = Item::new(
            session_id(),
            ItemKind::BranchNote(BranchNote {
                note: "n".to_owned(),
            }),
        )
        .stub();
        let json =
            serde_json::to_value(ServerEvent::ItemStarted { item: stub }).expect("serialize");
        let object = json.as_object().expect("object");
        assert_eq!(
            object
                .get("item")
                .and_then(|i| i.get("kind"))
                .and_then(serde_json::Value::as_str),
            Some("branch_note")
        );
        assert!(
            object.get("item").and_then(|i| i.get("payload")).is_none(),
            "the stub must not drag the payload along: {json}"
        );
    }
}
