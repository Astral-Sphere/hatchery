//! Wire protocol and data model of the hatchery daemon.
//!
//! Layer **L0** ([docs/architecture.md](https://github.com/Astral-Sphere/hatchery/blob/main/docs/architecture.md) §3):
//! the CLI, the GTK frontend, headless exec and the ACP bridge are all JSON-RPC 2.0 clients
//! speaking these types, and the daemon is the only runtime owner (ADR-0001).
//!
//! This crate is also the workspace's **shared vocabulary**: the kernel, the capability seam and
//! the store all use the same ids, content, tool-output and approval types rather than mirroring
//! them. That is why it sits below every other crate (see `docs/architecture.md` §3).
//!
//! Three things live here, and nothing else:
//!
//! 1. **the data model** — [`Session`], [`Item`] and [`ItemKind`]: an append-only tree where
//!    editing forks instead of overwriting (ADR-0003);
//! 2. **the methods** — client → daemon calls, typed in [`method`];
//! 3. **the events** — daemon → client notifications, [`SessionEvent`] and [`DaemonEvent`].
//!
//! Within a major the vocabulary only grows: a field added by a newer peer is ignored, but enum
//! values are frozen — an unknown one is refused rather than guessed at, so a peer from a later
//! major cannot be half-understood (`docs/design/protocol.md` §6).
//!
//! Design: `docs/design/protocol.md`.
//!
//! # Examples
//!
//! Ids are UUIDv7 and render as lowercase hyphenated UUIDs (decided 2026-09-28):
//!
//! ```
//! use hatchery_protocol::{ItemId, SessionId};
//!
//! let session = SessionId::new();
//! assert_eq!(session.to_string().len(), 36);
//! assert!(ItemId::new() != ItemId::new());
//! ```
//!
//! History is a tree of items, and an item's payload is flattened next to its kind — the same
//! two-column shape the `items` table stores:
//!
//! ```
//! use hatchery_protocol::{Content, Item, ItemKind, SessionId, Timestamp};
//!
//! let item = Item::new(SessionId::new(), ItemKind::UserMessage(Content::text("hi")))
//!     .with_created_at(Timestamp::from_unix_millis(1));
//!
//! let json = serde_json::to_value(&item).unwrap();
//! assert_eq!(json["kind"], "user_message");
//! assert_eq!(json["payload"]["text"], "hi");
//! assert_eq!(serde_json::from_value::<Item>(json).unwrap(), item);
//! ```
//!
//! Calls travel as newline-delimited JSON-RPC frames:
//!
//! ```
//! use hatchery_protocol::{Incoming, Request, decode_frame, encode_frame, method};
//!
//! let request = Request::new(1_i64, method::SESSION_LOAD);
//! let frame = encode_frame(&request).unwrap();
//! assert!(frame.ends_with('\n'), "frames are newline delimited");
//! assert!(matches!(decode_frame(&frame).unwrap(), Incoming::Request(_)));
//! ```
//!
//! Versions are negotiated at the handshake, and compatibility is decided per major:
//!
//! ```
//! use hatchery_protocol::{PROTOCOL_VERSION, is_compatible};
//!
//! assert!(is_compatible(PROTOCOL_VERSION));
//! assert!(is_compatible("1.9.0"));
//! assert!(!is_compatible("2.0.0"));
//! ```

mod approval;
mod content;
mod error;
mod event;
mod id;
mod item;
pub mod method;
mod rpc;
mod session;
mod time;
mod tool;
mod usage;
mod version;

pub use approval::{ApprovalOption, ApprovalRequest, RiskLevel};
pub use content::{Content, ContentPart, SignatureBlock};
pub use error::{ErrorCode, ErrorObject, EventError, UnknownErrorCode};
pub use event::{DaemonEvent, ServerEvent, SessionEvent};
pub use id::{ApprovalId, ItemId, ItemIdRange, SessionId, TurnId};
pub use item::{
    BranchNote, Checkpoint, Compaction, Item, ItemKind, ItemKindTag, ItemStub, ModeSwitch,
    ReasoningBlock, ToolCall, ToolResult, UnknownItemKind,
};
pub use rpc::{
    FrameDecoder, FrameError, Id, Incoming, JsonRpcVersion, Notification, Request, Response,
    classify, decode_frame, encode_frame,
};
pub use session::{
    CheckpointKind, ModelRef, ReasoningEffort, RewindScope, Session, SessionListFilter,
    SessionModeId, SessionPatch, SessionStatus,
};
pub use time::Timestamp;
pub use tool::{
    SpilledOutput, ToolArtifact, ToolCallSummary, ToolOutput, ToolProgress, ToolStatus,
};
pub use usage::{StopReason, TurnCompletion, Usage};
pub use version::{
    PROTOCOL_MAJOR, PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS, is_compatible, major_of,
};
