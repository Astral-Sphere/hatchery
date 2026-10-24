//! The append-only history tree.
//!
//! `Item` is the smallest unit of history. Items never change in place (invariant 3); editing
//! produces a new item on a new branch (ADR-0003). The tree shape is `parent` links — a session
//! is a forest of branches, and `Session::active_branch_head` decides which one counts.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::content::{Content, SignatureBlock};
use crate::id::{ItemId, ItemIdRange, SessionId, TurnId};
use crate::session::{CheckpointKind, SessionModeId};
use crate::time::Timestamp;
use crate::tool::{ToolOutput, ToolStatus};

/// One item of history.
///
/// `kind` and its payload are flattened into the item object rather than nested under a `kind`
/// field, which gives `{"id":…,"kind":"user_message","payload":{…}}` — the same two-column shape
/// the `items` table stores (`kind` and `payload`, `docs/design/storage.md` §2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Item id (UUIDv7, so text order follows creation order).
    pub id: ItemId,
    /// Owning session. Redundant with the enclosing event or method call on purpose: the store
    /// actor receives bare items, and a mismatch is worth catching before it becomes a foreign
    /// key error.
    pub session: SessionId,
    /// Parent in the tree. `None` for a branch root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ItemId>,
    /// Which turn produced this item, when it belongs to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnId>,
    /// What the item is, plus its payload.
    #[serde(flatten)]
    pub kind: ItemKind,
    /// When the item was appended.
    pub created_at: Timestamp,
}

impl Item {
    /// A root item with a freshly minted id and the current time.
    #[must_use]
    pub fn new(session: SessionId, kind: ItemKind) -> Self {
        Self::with_id(ItemId::new(), session, kind)
    }

    /// An item with a specific id.
    ///
    /// For callers that had to announce the id before the payload existed: the kernel emits
    /// `ItemStarted` with a stub when a streamed item opens and completes it with `ItemFinished`
    /// once the text is in, and the id must not change in between.
    #[must_use]
    pub fn with_id(id: ItemId, session: SessionId, kind: ItemKind) -> Self {
        Self {
            id,
            session,
            parent: None,
            turn: None,
            kind,
            created_at: Timestamp::now(),
        }
    }

    /// Chains this item after `parent`.
    #[must_use]
    pub fn with_parent(mut self, parent: ItemId) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Attaches the item to a turn.
    #[must_use]
    pub fn with_turn(mut self, turn: TurnId) -> Self {
        self.turn = Some(turn);
        self
    }

    /// Overrides the timestamp (tests that must be reproducible).
    #[must_use]
    pub fn with_created_at(mut self, created_at: Timestamp) -> Self {
        self.created_at = created_at;
        self
    }

    /// The item's kind, without its payload.
    #[must_use]
    pub const fn kind_tag(&self) -> ItemKindTag {
        self.kind.tag()
    }

    /// The item's tree position, without its payload.
    ///
    /// Sent as `ItemStarted` so a timeline can create the node before the deltas arrive, and
    /// returned by replay cursors that only need the skeleton.
    #[must_use]
    pub fn stub(&self) -> ItemStub {
        ItemStub {
            id: self.id,
            parent: self.parent,
            turn: self.turn,
            kind: self.kind_tag(),
        }
    }
}

/// An item's identity and position, without its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemStub {
    /// Item id.
    pub id: ItemId,
    /// Parent in the tree, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ItemId>,
    /// Owning turn, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnId>,
    /// What kind of item the payload will be.
    pub kind: ItemKindTag,
}

/// What an item is, carrying its payload.
///
/// Nine variants, each a newtype over the payload struct that documents its fields: adding a
/// field to one kind therefore never touches the others, and the store can persist `kind` and
/// `payload` as two columns without a hand-written mapping.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum ItemKind {
    /// Something the user said.
    UserMessage(Content),
    /// Something the model said.
    AssistantMessage(Content),
    /// A reasoning trace, stored verbatim (ADR-0007: no trim, no normalise).
    Reasoning(ReasoningBlock),
    /// A tool call emitted by the model.
    ToolCall(ToolCall),
    /// The result of a tool call.
    ToolResult(ToolResult),
    /// A shadow-Git checkpoint taken before a write (ADR-0006).
    Checkpoint(Checkpoint),
    /// A summary that replaces a span of history (M5).
    Compaction(Compaction),
    /// The session switched mode at a turn boundary (ADR-0005).
    ModeSwitch(ModeSwitch),
    /// A user's note about what a branch is for (M4).
    BranchNote(BranchNote),
}

impl ItemKind {
    /// The kind tag, without the payload.
    #[must_use]
    pub const fn tag(&self) -> ItemKindTag {
        match self {
            Self::UserMessage(_) => ItemKindTag::UserMessage,
            Self::AssistantMessage(_) => ItemKindTag::AssistantMessage,
            Self::Reasoning(_) => ItemKindTag::Reasoning,
            Self::ToolCall(_) => ItemKindTag::ToolCall,
            Self::ToolResult(_) => ItemKindTag::ToolResult,
            Self::Checkpoint(_) => ItemKindTag::Checkpoint,
            Self::Compaction(_) => ItemKindTag::Compaction,
            Self::ModeSwitch(_) => ItemKindTag::ModeSwitch,
            Self::BranchNote(_) => ItemKindTag::BranchNote,
        }
    }

    /// True for the kinds that carry conversation content, i.e. what a provider request is built
    /// from. Reasoning is included: whether it is replayed is a provider-capability decision
    /// (ADR-0007), not a property of the item.
    #[must_use]
    pub const fn is_conversation(&self) -> bool {
        matches!(
            self,
            Self::UserMessage(_)
                | Self::AssistantMessage(_)
                | Self::Reasoning(_)
                | Self::ToolCall(_)
                | Self::ToolResult(_)
                | Self::Compaction(_)
        )
    }

    /// The payload half of the flattened representation — exactly the `items.payload` column.
    ///
    /// Derived from the serde representation rather than duplicated, so the wire form and the
    /// database can never disagree.
    ///
    /// # Panics
    ///
    /// Never in practice: an adjacently tagged enum always serialises to an object carrying both
    /// `kind` and `payload`. A unit test asserts that for every variant.
    #[must_use]
    pub fn to_payload(&self) -> Value {
        let Value::Object(mut map) = serde_json::to_value(self)
            .expect("ItemKind contains no value serde_json cannot represent")
        else {
            unreachable!("an adjacently tagged enum serialises to a JSON object");
        };
        map.remove("payload")
            .expect("an adjacently tagged enum carries a payload key")
    }

    /// Rebuilds an item kind from the two database columns.
    ///
    /// # Errors
    ///
    /// Returns the serde error when the stored payload does not match the stored kind — a
    /// corrupt row is reported, never silently dropped.
    pub fn from_parts(tag: ItemKindTag, payload: Value) -> Result<Self, serde_json::Error> {
        let mut object = serde_json::Map::new();
        object.insert("kind".to_owned(), Value::String(tag.as_str().to_owned()));
        object.insert("payload".to_owned(), payload);
        serde_json::from_value(Value::Object(object))
    }
}

/// The kind of an item, on its own.
///
/// A typed tag rather than a `&str`: the store writes it to a column, the wire carries it in
/// `ItemStarted`, and a mistyped kind string should not compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKindTag {
    /// See [`ItemKind::UserMessage`].
    UserMessage,
    /// See [`ItemKind::AssistantMessage`].
    AssistantMessage,
    /// See [`ItemKind::Reasoning`].
    Reasoning,
    /// See [`ItemKind::ToolCall`].
    ToolCall,
    /// See [`ItemKind::ToolResult`].
    ToolResult,
    /// See [`ItemKind::Checkpoint`].
    Checkpoint,
    /// See [`ItemKind::Compaction`].
    Compaction,
    /// See [`ItemKind::ModeSwitch`].
    ModeSwitch,
    /// See [`ItemKind::BranchNote`].
    BranchNote,
}

/// Generates [`ItemKindTag`]'s `ALL`, `as_str` and `from_name` from one variant list.
///
/// One list, three outputs, so none of them can drift: `as_str`'s match is exhaustive over the
/// enum, so a variant added to the enum without its spelling here fails to compile, and `ALL` —
/// which `from_name` searches and the fixtures are checked against — is built from the same list.
macro_rules! item_kind_tags {
    ($($variant:ident => $name:literal),* $(,)?) => {
        impl ItemKindTag {
            /// Every tag, in declaration order. Tests iterate this to keep the mapping honest.
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// The wire and database spelling (`snake_case`).
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            /// Parses the database or wire spelling.
            #[must_use]
            pub fn from_name(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|tag| tag.as_str() == name)
            }
        }
    };
}

item_kind_tags! {
    UserMessage => "user_message",
    AssistantMessage => "assistant_message",
    Reasoning => "reasoning",
    ToolCall => "tool_call",
    ToolResult => "tool_result",
    Checkpoint => "checkpoint",
    Compaction => "compaction",
    ModeSwitch => "mode_switch",
    BranchNote => "branch_note",
}

impl fmt::Display for ItemKindTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ItemKindTag {
    type Err = UnknownItemKind;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::from_name(name).ok_or_else(|| UnknownItemKind(name.to_owned()))
    }
}

/// Returned when a stored or incoming item kind is not one this build knows.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown item kind {0:?}")]
pub struct UnknownItemKind(String);

impl UnknownItemKind {
    /// The name that was not recognised, for a report or a log line.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }
}

/// Payload of [`ItemKind::Reasoning`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReasoningBlock {
    /// The reasoning text, byte-for-byte as the provider emitted it (ADR-0007).
    pub text: String,
    /// Opaque provider signature, when the provider issued one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SignatureBlock>,
}

impl ReasoningBlock {
    /// Reasoning without a signature.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            signature: None,
        }
    }
}

/// Payload of [`ItemKind::ToolCall`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Tool name as registered, e.g. `write_file`.
    pub name: String,
    /// Raw arguments from the model. Kept as JSON: the shape belongs to the tool's schema, and
    /// the UI needs the original to render a diff or an approval prompt.
    pub args: Value,
    /// Where the call is in its lifecycle.
    pub status: ToolStatus,
    /// The provider's correlation id for this call, verbatim (M1, additive).
    ///
    /// History rebuild needs it: the next request must pair each tool result with the id the
    /// provider originally issued, byte for byte, or the rebuilt conversation diverges from the
    /// one that produced these turns (invariant 2). Absent on items committed before the field
    /// existed; the assembler synthesizes a stable stand-in for those rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_call_id: Option<String>,
}

impl ToolCall {
    /// A call with no provider id recorded (legacy rows, tests).
    #[must_use]
    pub fn new(name: impl Into<String>, args: Value, status: ToolStatus) -> Self {
        Self {
            name: name.into(),
            args,
            status,
            provider_call_id: None,
        }
    }
}

/// Payload of [`ItemKind::ToolResult`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// The [`ItemKind::ToolCall`] item this answers.
    pub call: ItemId,
    /// What the tool produced.
    pub output: ToolOutput,
    /// True when the output is an error report rather than a result. Kept next to `output`
    /// rather than inside it: an error still has text worth reading.
    pub is_error: bool,
}

/// Payload of [`ItemKind::Checkpoint`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Shadow-Git commit id. Named `commit_id` because `commit` is a reserved word the SQL parser
    /// rejects as a column name (ADR-0010).
    pub commit_id: String,
    /// Why the checkpoint was taken.
    pub kind: CheckpointKind,
}

/// Payload of [`ItemKind::Compaction`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compaction {
    /// The summary that replaces the covered span.
    pub summary: String,
    /// The span this compaction stands in for.
    pub covered: ItemIdRange,
}

/// Payload of [`ItemKind::ModeSwitch`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeSwitch {
    /// Mode before the switch.
    pub from: SessionModeId,
    /// Mode after the switch. Applies from the next turn (ADR-0005).
    pub to: SessionModeId,
}

/// Payload of [`ItemKind::BranchNote`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchNote {
    /// The user's note about this branch.
    pub note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One sample per variant, used by the tests below and by the golden fixtures.
    pub(crate) fn samples() -> Vec<ItemKind> {
        vec![
            ItemKind::UserMessage(Content::text("hello")),
            ItemKind::AssistantMessage(Content::text("hi")),
            ItemKind::Reasoning(ReasoningBlock {
                text: " let me think \n".to_owned(),
                signature: Some(SignatureBlock::new(
                    SignatureBlock::OPENAI_ENCRYPTED_CONTENT,
                    "abc",
                )),
            }),
            ItemKind::ToolCall(ToolCall {
                name: "read_file".to_owned(),
                args: serde_json::json!({"path": "src/main.rs"}),
                status: ToolStatus::Completed,
                provider_call_id: Some("call_probe_0".to_owned()),
            }),
            ItemKind::ToolResult(ToolResult {
                call: ItemId::from_uuid(uuid::Uuid::nil()),
                output: ToolOutput::text("fn main() {}"),
                is_error: false,
            }),
            ItemKind::Checkpoint(Checkpoint {
                commit_id: "deadbeef".to_owned(),
                kind: CheckpointKind::PreWrite,
            }),
            ItemKind::Compaction(Compaction {
                summary: "summarised the first three turns".to_owned(),
                covered: ItemIdRange::new(
                    ItemId::from_uuid(uuid::Uuid::nil()),
                    ItemId::from_uuid(uuid::Uuid::from_u128(1)),
                ),
            }),
            ItemKind::ModeSwitch(ModeSwitch {
                from: SessionModeId::chat(),
                to: SessionModeId::code(),
            }),
            ItemKind::BranchNote(BranchNote {
                note: "tried the short route".to_owned(),
            }),
        ]
    }

    #[test]
    fn the_enum_and_the_tag_list_agree() {
        assert_eq!(
            samples().len(),
            ItemKindTag::ALL.len(),
            "every variant needs a sample and a tag; add both together"
        );
        for kind in samples() {
            let tag = kind.tag();
            let json = serde_json::to_value(&kind).expect("serialize");
            assert_eq!(
                json.get("kind").and_then(Value::as_str),
                Some(tag.as_str()),
                "the variant's serde tag and as_str() drifted apart"
            );
            assert!(
                ItemKindTag::ALL.contains(&tag),
                "{tag} is missing from ItemKindTag::ALL"
            );
            assert_eq!(ItemKindTag::from_name(tag.as_str()), Some(tag));
            assert_eq!(tag.to_string(), tag.as_str());
        }
    }

    #[test]
    fn every_tag_in_the_registry_roundtrips_through_its_spelling() {
        // `ALL` is generated from the same list as `as_str`, so this covers every variant the
        // enum has — including any added later, which the macro forces into that list.
        let mut seen = Vec::new();
        for tag in ItemKindTag::ALL {
            let name = tag.as_str();
            assert_eq!(
                serde_json::to_value(tag).expect("serialize"),
                Value::String(name.to_owned()),
                "{name}: as_str() and the serde spelling drifted apart"
            );
            assert_eq!(
                ItemKindTag::from_name(name),
                Some(*tag),
                "{name} did not parse back"
            );
            assert_eq!(tag.to_string(), name);
            seen.push(name);
        }

        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(seen.len(), unique.len(), "two tags share a spelling");
        assert_eq!(ItemKindTag::from_name("nonsense"), None);
    }

    #[test]
    fn payload_split_roundtrips_for_every_variant() {
        for kind in samples() {
            let payload = kind.to_payload();
            assert!(
                payload.is_object(),
                "{} payload must be an object: {payload}",
                kind.tag()
            );
            let rebuilt = ItemKind::from_parts(kind.tag(), payload)
                .unwrap_or_else(|e| panic!("{} did not rebuild: {e}", kind.tag()));
            assert_eq!(rebuilt, kind, "{} lost data in the split", kind.tag());
        }
    }

    #[test]
    fn the_kind_and_payload_keys_sit_at_the_top_level_of_an_item() {
        let item = Item::new(SessionId::new(), ItemKind::UserMessage(Content::text("hi")))
            .with_created_at(Timestamp::from_unix_millis(1));
        let json = serde_json::to_value(&item).expect("serialize");
        let object = json.as_object().expect("item is an object");
        assert_eq!(
            object.get("kind").and_then(Value::as_str),
            Some("user_message")
        );
        assert_eq!(
            object
                .get("payload")
                .and_then(|p| p.get("text"))
                .and_then(Value::as_str),
            Some("hi"),
            "the payload must be flattened, not nested: {json}"
        );
        assert_eq!(
            serde_json::from_value::<Item>(json).expect("deserialize"),
            item
        );
    }

    #[test]
    fn an_item_omits_the_optional_links_it_does_not_have() {
        let item = Item::new(
            SessionId::new(),
            ItemKind::BranchNote(BranchNote {
                note: "n".to_owned(),
            }),
        );
        let json = serde_json::to_string(&item).expect("serialize");
        assert!(!json.contains("parent"), "{json}");
        assert!(!json.contains("turn"), "{json}");
    }

    #[test]
    fn a_corrupt_row_is_reported_not_ignored() {
        let error = ItemKind::from_parts(ItemKindTag::UserMessage, serde_json::json!({"nope": 1}))
            .expect_err("a payload that does not match its kind must not deserialize");
        assert!(!error.to_string().is_empty());
        assert_eq!(
            "user_message".parse::<ItemKindTag>().expect("known tag"),
            ItemKindTag::UserMessage
        );
        assert!("nonsense".parse::<ItemKindTag>().is_err());
    }

    #[test]
    fn only_conversation_kinds_feed_a_provider_request() {
        let tags: Vec<ItemKindTag> = samples()
            .iter()
            .filter(|kind| kind.is_conversation())
            .map(ItemKind::tag)
            .collect();
        assert_eq!(
            tags,
            vec![
                ItemKindTag::UserMessage,
                ItemKindTag::AssistantMessage,
                ItemKindTag::Reasoning,
                ItemKindTag::ToolCall,
                ItemKindTag::ToolResult,
                ItemKindTag::Compaction,
            ]
        );
    }
}
