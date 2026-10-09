//! Shared sample values for the protocol integration tests.
//!
//! Every sample here is **deterministic** — fixed ids, fixed timestamps — because each one is
//! also a golden fixture under `tests/fixtures/protocol-v1/`. A sample that called
//! `SessionId::new()` would rewrite its golden on every run and the fixture would stop being a
//! contract.

#![allow(dead_code)]

use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use hatchery_protocol::method::{
    ApprovalRespondParams, ApprovalRespondResult, BranchDeleteParams, BranchDeleteResult,
    BranchListParams, BranchListResult, BranchNode, BranchSwitchParams, BranchSwitchResult,
    ClientInfo, ConfigEntry, ConfigGetParams, ConfigGetResult, ConfigOrigin, ConfigPatch,
    ConfigSetParams, ConfigSetResult, DaemonCapabilities, EditItemParams, EditItemResult,
    ExportJsonlParams, ExportJsonlResult, HelloParams, HelloResult, PromptRenderParams,
    PromptRenderResult, PromptSection, RewindParams, RewindReport, RewindResult,
    SessionCancelParams, SessionCancelResult, SessionDeleteParams, SessionDeleteResult,
    SessionListParams, SessionListResult, SessionLoadParams, SessionLoadResult, SessionNewParams,
    SessionNewResult, SessionPromptParams, SessionPromptResult, SessionRenameParams,
    SessionRenameResult, SetConfigParams, SetConfigResult, SetModeParams, SetModeResult,
};
use hatchery_protocol::{
    ApprovalId, ApprovalOption, ApprovalRequest, BranchNote, Checkpoint, CheckpointKind,
    Compaction, Content, ContentPart, DaemonEvent, ErrorCode, ErrorObject, EventError, Item,
    ItemId, ItemIdRange, ItemKind, ItemKindTag, JsonRpcVersion, ModeSwitch, ModelRef, Notification,
    PROTOCOL_VERSION, ReasoningBlock, ReasoningEffort, Request, Response, RiskLevel,
    SUPPORTED_PROTOCOL_VERSIONS, ServerEvent, Session, SessionEvent, SessionModeId, SessionPatch,
    SessionStatus, SignatureBlock, SpilledOutput, StopReason, Timestamp, ToolArtifact, ToolCall,
    ToolCallSummary, ToolOutput, ToolProgress, ToolResult, ToolStatus, TurnCompletion, TurnId,
    Usage,
};

/// Session id used by every sample.
pub const SESSION_ID: &str = "01890f47-0000-7000-8000-000000000001";
/// First item id used by the samples.
pub const ITEM_ID: &str = "01890f47-0000-7000-8000-000000000002";
/// Second item id used by the samples.
pub const ITEM_ID_2: &str = "01890f47-0000-7000-8000-000000000003";
/// Turn id used by every sample.
pub const TURN_ID: &str = "01890f47-0000-7000-8000-000000000004";
/// Approval id used by every sample.
pub const APPROVAL_ID: &str = "01890f47-0000-7000-8000-000000000005";

/// A fixed timestamp, so goldens never move.
pub const TIMESTAMP: Timestamp = Timestamp::from_unix_millis(1_780_000_000_000);

/// Parses one of the fixed ids above.
///
/// Ids are built from strings rather than from `uuid::Uuid` values because `uuid` is a normal
/// dependency of the crate under test, not a dev-dependency, and integration tests only see the
/// public API — which is the point of testing through it.
pub fn session_id() -> hatchery_protocol::SessionId {
    SESSION_ID.parse().expect("SESSION_ID is a valid uuid")
}

/// The first fixed item id.
pub fn item_id() -> ItemId {
    ITEM_ID.parse().expect("ITEM_ID is a valid uuid")
}

/// The second fixed item id.
pub fn item_id_2() -> ItemId {
    ITEM_ID_2.parse().expect("ITEM_ID_2 is a valid uuid")
}

/// The fixed turn id.
pub fn turn_id() -> TurnId {
    TURN_ID.parse().expect("TURN_ID is a valid uuid")
}

/// The fixed approval id.
pub fn approval_id() -> ApprovalId {
    APPROVAL_ID.parse().expect("APPROVAL_ID is a valid uuid")
}

/// A fully populated session.
pub fn session() -> Session {
    Session {
        id: session_id(),
        title: Some("refactor the store".to_owned()),
        mode: SessionModeId::code(),
        workspace: Some(PathBuf::from("/home/dev/project")),
        model: ModelRef::new("deepseek", "deepseek-reasoner"),
        config_patch: Some(serde_json::json!({"ui": {"show_reasoning": true}})),
        created_at: TIMESTAMP,
        updated_at: TIMESTAMP,
        active_branch_head: Some(item_id_2()),
        generation: 3,
        status: SessionStatus::WaitingApproval,
    }
}

/// A session that has never run: no title, no workspace, no head.
///
/// The mirror image of [`session`], and the shape the store's foreign keys depend on:
/// `active_branch_head` must be *absent* rather than dangling (ADR-0010).
pub fn fresh_session() -> Session {
    Session {
        id: session_id(),
        title: None,
        mode: SessionModeId::chat(),
        workspace: None,
        model: ModelRef::new("deepseek", "deepseek-reasoner"),
        config_patch: None,
        created_at: TIMESTAMP,
        updated_at: TIMESTAMP,
        active_branch_head: None,
        generation: 0,
        status: SessionStatus::Idle,
    }
}

/// One item per [`ItemKind`], in tag order, with deterministic ids and timestamps.
///
/// The ids are positional (`ITEM_ID`, `ITEM_ID_2`) rather than freshly minted, so a golden diff
/// shows the field change under review instead of two new UUIDs.
pub fn items() -> Vec<(&'static str, Item)> {
    let session = session_id();
    let make = |stem: &'static str, kind: ItemKind, created_at: i64| {
        (
            stem,
            Item {
                id: item_id(),
                session,
                parent: Some(item_id_2()),
                turn: Some(turn_id()),
                kind,
                created_at: Timestamp::from_unix_millis(created_at),
            },
        )
    };

    vec![
        make(
            "item_user_message",
            ItemKind::UserMessage(Content {
                text: "why is the store slow?".to_owned(),
                parts: vec![ContentPart::Resource {
                    uri: "/tmp/notes.md".to_owned(),
                    mime_type: Some("text/markdown".to_owned()),
                    text: None,
                }],
            }),
            TIMESTAMP.as_unix_millis(),
        ),
        make(
            "item_assistant_message",
            ItemKind::AssistantMessage(Content::text("because every read walks the tree")),
            TIMESTAMP.as_unix_millis() + 1,
        ),
        make(
            "item_reasoning",
            ItemKind::Reasoning(ReasoningBlock {
                text: " leading space and\ntrailing newline \n".to_owned(),
                signature: Some(SignatureBlock::new(
                    SignatureBlock::OPENAI_ENCRYPTED_CONTENT,
                    "b3BhcXVl",
                )),
            }),
            TIMESTAMP.as_unix_millis() + 2,
        ),
        make(
            "item_tool_call",
            ItemKind::ToolCall(ToolCall {
                name: "read_file".to_owned(),
                args: serde_json::json!({"path": "crates/hatchery-store/src/tree.rs", "limit": 40}),
                status: ToolStatus::Completed,
                provider_call_id: Some("call_00_probe".to_owned()),
            }),
            TIMESTAMP.as_unix_millis() + 3,
        ),
        make(
            "item_tool_result",
            ItemKind::ToolResult(ToolResult {
                call: item_id_2(),
                output: ToolOutput {
                    text: "fn chain(…) { … }".to_owned(),
                    artifacts: vec![ToolArtifact {
                        path: PathBuf::from("/tmp/out.txt"),
                        mime_type: None,
                    }],
                    spilled: Some(SpilledOutput {
                        path: PathBuf::from("/tmp/state/tool-results/1.txt"),
                        bytes: Some(120_000),
                    }),
                },
                is_error: false,
            }),
            TIMESTAMP.as_unix_millis() + 4,
        ),
        make(
            "item_checkpoint",
            ItemKind::Checkpoint(Checkpoint {
                commit_id: "9f2c4a1b7e5d3f80".to_owned(),
                kind: CheckpointKind::PreWrite,
            }),
            TIMESTAMP.as_unix_millis() + 5,
        ),
        make(
            "item_compaction",
            ItemKind::Compaction(Compaction {
                summary: "the first three turns settled on the in-memory tree walk".to_owned(),
                covered: ItemIdRange::new(item_id(), item_id_2()),
            }),
            TIMESTAMP.as_unix_millis() + 6,
        ),
        make(
            "item_mode_switch",
            ItemKind::ModeSwitch(ModeSwitch {
                from: SessionModeId::chat(),
                to: SessionModeId::code(),
            }),
            TIMESTAMP.as_unix_millis() + 7,
        ),
        make(
            "item_branch_note",
            ItemKind::BranchNote(BranchNote {
                note: "the short route, abandoned".to_owned(),
            }),
            TIMESTAMP.as_unix_millis() + 8,
        ),
    ]
}

/// One [`ServerEvent`] per variant, with deterministic ids.
///
/// The stems must cover [`ServerEvent::type_name`] exactly; `serde_roundtrip.rs` asserts that, so
/// a new variant cannot be added without a fixture.
pub fn events() -> Vec<(&'static str, ServerEvent)> {
    let item = items().remove(0).1;
    vec![
        (
            "event_item_started",
            ServerEvent::ItemStarted { item: item.stub() },
        ),
        (
            "event_text_delta",
            ServerEvent::TextDelta {
                item: item_id(),
                text: "because ".to_owned(),
            },
        ),
        (
            "event_reasoning_delta",
            ServerEvent::ReasoningDelta {
                item: item_id(),
                text: "the user is asking about ".to_owned(),
            },
        ),
        ("event_item_finished", ServerEvent::ItemFinished { item }),
        (
            "event_tool_call_started",
            ServerEvent::ToolCallStarted {
                item: item_id(),
                summary: ToolCallSummary::new("edit crates/hatchery-store/src/tree.rs")
                    .with_detail("+12 -3"),
            },
        ),
        (
            "event_tool_call_progress",
            ServerEvent::ToolCallProgress {
                item: item_id(),
                chunk: "compiling hatchery-store\n".to_owned(),
            },
        ),
        (
            "event_approval_requested",
            ServerEvent::ApprovalRequested {
                request_id: approval_id(),
                request: ApprovalRequest::new(
                    "write_file",
                    "write ~/.ssh/config (outside the workspace)",
                    RiskLevel::WritesOutside,
                )
                .once_only(),
            },
        ),
        (
            "event_turn_finished",
            ServerEvent::TurnFinished {
                turn: turn_id(),
                completion: TurnCompletion::new(StopReason::ModelDone).with_usage(Usage {
                    prompt_tokens: Some(12_345),
                    completion_tokens: Some(678),
                    reasoning_tokens: Some(400),
                    requests: 3,
                }),
            },
        ),
        (
            "event_turn_failed",
            ServerEvent::TurnFailed {
                turn: turn_id(),
                error: EventError::llm("429 from the gateway", true),
            },
        ),
        (
            "event_session_updated",
            ServerEvent::SessionUpdated { state: session() },
        ),
        (
            "event_mode_switched",
            ServerEvent::ModeSwitched {
                from: SessionModeId::chat(),
                to: SessionModeId::code(),
            },
        ),
        (
            "event_rate_limited",
            ServerEvent::RateLimited {
                retry_after_ms: 12_000,
            },
        ),
        ("event_generation_bumped", ServerEvent::GenerationBumped),
    ]
}

/// One [`DaemonEvent`].
pub fn daemon_events() -> Vec<(&'static str, DaemonEvent)> {
    vec![(
        "daemon_event_shutting_down",
        DaemonEvent::DaemonShuttingDown {
            reason: "idle timeout".to_owned(),
        },
    )]
}

/// One sample of each JSON-RPC frame kind.
pub fn frames() -> Vec<(&'static str, Value)> {
    let request = Request::new(7_i64, hatchery_protocol::method::SESSION_LOAD)
        .with_params(&SessionLoadParams {
            session_id: session_id(),
            replay_from: Some(item_id()),
            generation: Some(3),
        })
        .expect("params are representable");
    let notification = Notification::new(hatchery_protocol::method::SESSION_CANCEL);
    let response_ok = Response::ok(7_i64, &SessionCancelResult { cancelled: true })
        .expect("result representable");
    let response_err = Response::err(
        8_i64,
        ErrorObject::new(ErrorCode::SessionNotFound, "no session with that id")
            .with_data(serde_json::json!({"session_id": SESSION_ID})),
    );

    vec![
        ("frame_request", to_value(&request)),
        ("frame_notification", to_value(&notification)),
        ("frame_response_ok", to_value(&response_ok)),
        ("frame_response_err", to_value(&response_err)),
    ]
}

/// Every method's parameter struct, so each one is pinned by a golden and stays deserializable.
///
/// The method name doubles as the fixture stem suffix and as the key used by
/// [`params_validator`].
pub fn method_params() -> Vec<(&'static str, Value)> {
    use hatchery_protocol::method as m;

    let samples: Vec<(&'static str, Value)> = vec![
        (
            m::DAEMON_HELLO,
            to_value(&HelloParams {
                protocol_version: PROTOCOL_VERSION.to_owned(),
                client: Some(ClientInfo {
                    name: "hatchery-cli".to_owned(),
                    version: "0.1.0".to_owned(),
                }),
                boot_token: Some("boot-1234".to_owned()),
            }),
        ),
        (
            m::SESSION_NEW,
            to_value(&SessionNewParams {
                mode: SessionModeId::code(),
                workspace: Some(PathBuf::from("/home/dev/project")),
                model: Some(ModelRef::new("deepseek", "deepseek-reasoner")),
                title: None,
                config_patch: None,
            }),
        ),
        (
            m::SESSION_LOAD,
            to_value(&SessionLoadParams {
                session_id: session_id(),
                replay_from: None,
                generation: Some(3),
            }),
        ),
        (
            m::SESSION_LIST,
            to_value(&SessionListParams {
                cursor: Some("cursor-1".to_owned()),
                limit: Some(50),
                filter: Some(hatchery_protocol::SessionListFilter {
                    mode: Some(SessionModeId::code()),
                    workspace: None,
                    title_contains: Some("store".to_owned()),
                }),
            }),
        ),
        (
            m::SESSION_PROMPT,
            to_value(&SessionPromptParams {
                session_id: session_id(),
                content: Content::text("why is the store slow?"),
                generation: Some(3),
            }),
        ),
        (
            m::SESSION_CANCEL,
            to_value(&SessionCancelParams {
                session_id: session_id(),
            }),
        ),
        (
            m::SESSION_SET_MODE,
            to_value(&SetModeParams {
                session_id: session_id(),
                mode: SessionModeId::chat(),
            }),
        ),
        (
            m::SESSION_SET_CONFIG,
            to_value(&SetConfigParams {
                session_id: session_id(),
                patch: ConfigPatch {
                    model: None,
                    reasoning_effort: Some(ReasoningEffort::High),
                    overrides: Some(serde_json::json!({"ui": {"show_reasoning": true}})),
                },
            }),
        ),
        (
            m::SESSION_EDIT_ITEM,
            to_value(&EditItemParams {
                item_id: item_id(),
                new_content: Content::text("actually, why is the read path slow?"),
                rewind_scope: Some(hatchery_protocol::RewindScope::Conversation),
            }),
        ),
        (
            m::SESSION_BRANCH_LIST,
            to_value(&BranchListParams {
                session_id: session_id(),
            }),
        ),
        (
            m::SESSION_BRANCH_SWITCH,
            to_value(&BranchSwitchParams {
                session_id: session_id(),
                head: item_id(),
            }),
        ),
        (
            m::SESSION_BRANCH_DELETE,
            to_value(&BranchDeleteParams {
                session_id: session_id(),
                head: item_id(),
                confirm: true,
            }),
        ),
        (
            m::SESSION_REWIND,
            to_value(&RewindParams {
                session_id: session_id(),
                target_item: item_id(),
                scope: hatchery_protocol::RewindScope::Both,
                purge_untracked: false,
            }),
        ),
        (
            m::SESSION_DELETE,
            to_value(&SessionDeleteParams {
                session_id: session_id(),
            }),
        ),
        (
            m::SESSION_RENAME,
            to_value(&SessionRenameParams {
                session_id: session_id(),
                title: Some("store performance".to_owned()),
            }),
        ),
        (
            m::APPROVAL_RESPOND,
            to_value(&ApprovalRespondParams {
                request_id: approval_id(),
                option: ApprovalOption::AllowOnce,
            }),
        ),
        (
            m::CONFIG_GET,
            to_value(&ConfigGetParams {
                key_path: Some("providers".to_owned()),
            }),
        ),
        (
            m::CONFIG_SET,
            to_value(&ConfigSetParams {
                key_path: "providers.deepseek.reasoning_effort".to_owned(),
                value: serde_json::json!("high"),
            }),
        ),
        (
            m::PROMPT_RENDER,
            to_value(&PromptRenderParams {
                session_id: Some(session_id()),
                mode: None,
            }),
        ),
        (
            m::STORE_EXPORT_JSONL,
            to_value(&ExportJsonlParams {
                session_id: session_id(),
                path: PathBuf::from("/tmp/export.jsonl"),
                all_branches: false,
            }),
        ),
    ];

    assert_eq!(
        samples.len(),
        m::ALL.len(),
        "every method needs a parameter sample; add one when adding a method"
    );
    samples
}

/// Every method's result struct, so each one is pinned by a golden and stays deserializable.
///
/// The method name is the key used by [`result_stem`] and [`results_validator`]. Results are
/// pinned with the same rigour as parameters: they are what a frontend has to render, and an
/// unpinned result type is a wire shape nobody reviewed.
pub fn method_results() -> Vec<(&'static str, Value)> {
    use hatchery_protocol::method as m;

    let session = session();
    // `items()` gives every sample the same id and the same parent, which is fine for one-item
    // fixtures but not for a replayed branch: this one has to be a real chain, root first.
    let mut replay: Vec<Item> = items().into_iter().take(2).map(|(_, item)| item).collect();
    replay[0].parent = None;
    replay[1].id = item_id_2();
    replay[1].parent = Some(item_id());

    let samples: Vec<(&'static str, Value)> = vec![
        (
            m::DAEMON_HELLO,
            to_value(&HelloResult {
                protocol_version: PROTOCOL_VERSION.to_owned(),
                daemon_version: "0.1.0".to_owned(),
                supported_versions: SUPPORTED_PROTOCOL_VERSIONS
                    .iter()
                    .map(|version| (*version).to_owned())
                    .collect(),
                capabilities: DaemonCapabilities {
                    methods: m::ALL.iter().map(|name| (*name).to_owned()).collect(),
                    modes: vec![SessionModeId::chat(), SessionModeId::code()],
                },
            }),
        ),
        (
            m::SESSION_NEW,
            to_value(&SessionNewResult {
                session: session.clone(),
            }),
        ),
        (
            m::SESSION_LOAD,
            to_value(&SessionLoadResult {
                session: session.clone(),
                items: replay,
                next_cursor: Some(item_id_2()),
            }),
        ),
        (
            m::SESSION_LIST,
            to_value(&SessionListResult {
                sessions: vec![session.clone()],
                next_cursor: Some("cursor-2".to_owned()),
            }),
        ),
        (
            m::SESSION_PROMPT,
            to_value(&SessionPromptResult { turn: turn_id() }),
        ),
        (
            m::SESSION_CANCEL,
            to_value(&SessionCancelResult { cancelled: true }),
        ),
        (
            m::SESSION_SET_MODE,
            to_value(&SetModeResult {
                session: Session {
                    mode: SessionModeId::chat(),
                    ..session.clone()
                },
            }),
        ),
        (
            m::SESSION_SET_CONFIG,
            to_value(&SetConfigResult {
                session: session.clone(),
            }),
        ),
        (
            m::SESSION_EDIT_ITEM,
            to_value(&EditItemResult {
                branch_head: item_id(),
                session: session.clone(),
            }),
        ),
        (
            m::SESSION_BRANCH_LIST,
            to_value(&BranchListResult {
                nodes: vec![
                    BranchNode {
                        id: item_id(),
                        parent: None,
                        turn: Some(turn_id()),
                        kind: ItemKindTag::UserMessage,
                        created_at: TIMESTAMP,
                        active: true,
                    },
                    BranchNode {
                        id: item_id_2(),
                        parent: Some(item_id()),
                        turn: Some(turn_id()),
                        kind: ItemKindTag::AssistantMessage,
                        created_at: TIMESTAMP,
                        active: false,
                    },
                ],
            }),
        ),
        (
            m::SESSION_BRANCH_SWITCH,
            to_value(&BranchSwitchResult {
                session: session.clone(),
            }),
        ),
        (
            m::SESSION_BRANCH_DELETE,
            to_value(&BranchDeleteResult { deleted_items: 4 }),
        ),
        (
            m::SESSION_REWIND,
            to_value(&RewindResult {
                session: session.clone(),
                report: RewindReport {
                    rolled_back: vec![PathBuf::from("src/main.rs")],
                    purged: Vec::new(),
                },
            }),
        ),
        (
            m::SESSION_DELETE,
            to_value(&SessionDeleteResult { deleted_items: 12 }),
        ),
        (
            m::SESSION_RENAME,
            to_value(&SessionRenameResult {
                session: Session {
                    title: Some("store performance".to_owned()),
                    ..session.clone()
                },
            }),
        ),
        (
            m::APPROVAL_RESPOND,
            to_value(&ApprovalRespondResult { handled: true }),
        ),
        (
            m::CONFIG_GET,
            to_value(&ConfigGetResult {
                entries: vec![ConfigEntry {
                    key: "providers.deepseek.reasoning_effort".to_owned(),
                    value: serde_json::json!("high"),
                    origin: ConfigOrigin::User,
                }],
            }),
        ),
        (
            m::CONFIG_SET,
            to_value(&ConfigSetResult {
                entry: ConfigEntry {
                    key: "providers.deepseek.reasoning_effort".to_owned(),
                    value: serde_json::json!("high"),
                    origin: ConfigOrigin::Runtime,
                },
            }),
        ),
        (
            m::PROMPT_RENDER,
            to_value(&PromptRenderResult {
                text: "identity\nmode\n".to_owned(),
                sections: vec![PromptSection {
                    id: "identity".to_owned(),
                    source: "builtin".to_owned(),
                    text: "You are hatchery.".to_owned(),
                }],
            }),
        ),
        (
            m::STORE_EXPORT_JSONL,
            to_value(&ExportJsonlResult {
                path: PathBuf::from("/tmp/export.jsonl"),
                lines: 42,
            }),
        ),
    ];

    assert_eq!(
        samples.len(),
        m::ALL.len(),
        "every method needs a result sample; add one when adding a method"
    );
    samples
}

/// Shapes whose *absent* fields are the contract.
///
/// Every other golden pins what is on the wire; these two pin what must not be there. A root item
/// and a session that never ran carry their optional links as absent keys, never as `null`, so
/// that "no parent" and "a parent we do not know" stay distinguishable in the store (ADR-0010).
pub fn absent_field_shapes() -> Vec<(&'static str, Value)> {
    vec![
        (
            "item_root",
            to_value(
                &Item::with_id(
                    item_id(),
                    session_id(),
                    ItemKind::UserMessage(Content::text("the first thing said")),
                )
                .with_created_at(TIMESTAMP),
            ),
        ),
        ("session_fresh", to_value(&fresh_session())),
    ]
}

/// A session patch in both of its interesting shapes: leave alone, and clear the title.
pub fn session_patches() -> Vec<(&'static str, Value)> {
    vec![
        (
            "session_patch_clear_title",
            to_value(&SessionPatch {
                title: Some(None),
                mode: Some(SessionModeId::chat()),
                ..SessionPatch::default()
            }),
        ),
        ("session_patch_empty", to_value(&SessionPatch::default())),
    ]
}

/// Values that are not a particular struct but still part of the wire contract.
pub fn tables() -> Vec<(&'static str, Value)> {
    vec![
        (
            "table_methods",
            to_value(&hatchery_protocol::method::ALL.to_vec()),
        ),
        (
            "table_error_codes",
            to_value(
                &ErrorCode::ALL
                    .iter()
                    .map(|code| serde_json::json!({"name": code.as_str(), "code": code.as_i64()}))
                    .collect::<Vec<_>>(),
            ),
        ),
        (
            "table_item_kinds",
            to_value(
                &ItemKindTag::ALL
                    .iter()
                    .map(|tag| tag.as_str())
                    .collect::<Vec<_>>(),
            ),
        ),
        ("value_protocol_version", to_value(&PROTOCOL_VERSION)),
        (
            "value_tool_output_spilled",
            to_value(&ToolOutput::spilled(
                "first 200 lines…",
                "/tmp/state/tool-results/1.txt",
            )),
        ),
        (
            "value_tool_progress",
            to_value(&ToolProgress::from("compiling…".to_owned())),
        ),
        ("value_jsonrpc_version", to_value(&JsonRpcVersion::V2)),
    ]
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("every sample is representable as JSON")
}

/// A validator: deserializes a fixture into its type and reports the parsed value.
pub type Check = Box<dyn Fn(&Value) -> Result<String, String>>;

/// Builds a validator for a concrete type.
///
/// Used as `as_type::<Item>()`: the fixture table is heterogeneous, so each entry carries the
/// deserializer for exactly the type it pins. Deserializing into `Value` would prove nothing.
pub fn as_type<T>() -> Check
where
    T: DeserializeOwned + std::fmt::Debug + 'static,
{
    Box::new(|value: &Value| {
        serde_json::from_value::<T>(value.clone())
            .map(|parsed| format!("{parsed:?}"))
            .map_err(|error| error.to_string())
    })
}

/// A validator for types that only need to be a non-empty JSON array.
pub fn as_json_list() -> Check {
    Box::new(|value: &Value| match value {
        Value::Array(items) if !items.is_empty() => Ok(format!("{} entries", items.len())),
        other => Err(format!("expected a non-empty array, got {other}")),
    })
}

/// A validator for a bare JSON string.
pub fn as_json_string() -> Check {
    Box::new(|value: &Value| match value {
        Value::String(text) if !text.is_empty() => Ok(text.clone()),
        other => Err(format!("expected a non-empty string, got {other}")),
    })
}

/// A validator for the patch that clears the title.
///
/// Deserializing into [`SessionPatch`] is not enough on its own: serde maps JSON `null` onto the
/// *outer* option, so a patch that lost its clear marker still deserializes — as "leave the title
/// alone". The value is the thing this fixture exists to pin.
pub fn as_title_clearing_patch() -> Check {
    Box::new(|value: &Value| {
        let patch = serde_json::from_value::<SessionPatch>(value.clone())
            .map_err(|error| error.to_string())?;
        if patch.title != Some(None) {
            return Err(format!(
                "title is {:?}, expected Some(None): a null title must read back as 'clear it'",
                patch.title
            ));
        }
        Ok(format!("{patch:?}"))
    })
}

/// The validator for one method's parameters, keyed by method name.
pub fn params_validator(method: &str) -> Check {
    use hatchery_protocol::method as m;

    match method {
        m::DAEMON_HELLO => as_type::<HelloParams>(),
        m::SESSION_NEW => as_type::<SessionNewParams>(),
        m::SESSION_LOAD => as_type::<SessionLoadParams>(),
        m::SESSION_LIST => as_type::<SessionListParams>(),
        m::SESSION_PROMPT => as_type::<SessionPromptParams>(),
        m::SESSION_CANCEL => as_type::<SessionCancelParams>(),
        m::SESSION_SET_MODE => as_type::<SetModeParams>(),
        m::SESSION_SET_CONFIG => as_type::<SetConfigParams>(),
        m::SESSION_EDIT_ITEM => as_type::<EditItemParams>(),
        m::SESSION_BRANCH_LIST => as_type::<BranchListParams>(),
        m::SESSION_BRANCH_SWITCH => as_type::<BranchSwitchParams>(),
        m::SESSION_BRANCH_DELETE => as_type::<BranchDeleteParams>(),
        m::SESSION_REWIND => as_type::<RewindParams>(),
        m::SESSION_DELETE => as_type::<SessionDeleteParams>(),
        m::SESSION_RENAME => as_type::<SessionRenameParams>(),
        m::APPROVAL_RESPOND => as_type::<ApprovalRespondParams>(),
        m::CONFIG_GET => as_type::<ConfigGetParams>(),
        m::CONFIG_SET => as_type::<ConfigSetParams>(),
        m::PROMPT_RENDER => as_type::<PromptRenderParams>(),
        m::STORE_EXPORT_JSONL => as_type::<ExportJsonlParams>(),
        other => panic!("no validator for method {other:?}; add one"),
    }
}

/// The validator for one method's result, keyed by method name.
pub fn results_validator(method: &str) -> Check {
    use hatchery_protocol::method as m;

    match method {
        m::DAEMON_HELLO => as_type::<HelloResult>(),
        m::SESSION_NEW => as_type::<SessionNewResult>(),
        m::SESSION_LOAD => as_type::<SessionLoadResult>(),
        m::SESSION_LIST => as_type::<SessionListResult>(),
        m::SESSION_PROMPT => as_type::<SessionPromptResult>(),
        m::SESSION_CANCEL => as_type::<SessionCancelResult>(),
        m::SESSION_SET_MODE => as_type::<SetModeResult>(),
        m::SESSION_SET_CONFIG => as_type::<SetConfigResult>(),
        m::SESSION_EDIT_ITEM => as_type::<EditItemResult>(),
        m::SESSION_BRANCH_LIST => as_type::<BranchListResult>(),
        m::SESSION_BRANCH_SWITCH => as_type::<BranchSwitchResult>(),
        m::SESSION_BRANCH_DELETE => as_type::<BranchDeleteResult>(),
        m::SESSION_REWIND => as_type::<RewindResult>(),
        m::SESSION_DELETE => as_type::<SessionDeleteResult>(),
        m::SESSION_RENAME => as_type::<SessionRenameResult>(),
        m::APPROVAL_RESPOND => as_type::<ApprovalRespondResult>(),
        m::CONFIG_GET => as_type::<ConfigGetResult>(),
        m::CONFIG_SET => as_type::<ConfigSetResult>(),
        m::PROMPT_RENDER => as_type::<PromptRenderResult>(),
        m::STORE_EXPORT_JSONL => as_type::<ExportJsonlResult>(),
        other => panic!("no validator for the result of method {other:?}; add one"),
    }
}

/// True when the developer asked for fixture regeneration *and* the gate is not running.
///
/// Shared by `golden_fixtures.rs`, which writes, and `version_compat.rs`, which reads: nextest
/// runs test binaries concurrently, so a reader that ignored an update in progress would report a
/// half-written golden as a compatibility break.
pub fn updating() -> bool {
    let asked = std::env::var("UPDATE_FIXTURES").is_ok_and(|value| value == "1");
    let gated = std::env::var("INSTA_UPDATE").is_ok_and(|value| value == "no");
    asked && !gated
}

/// Where the goldens live for the current major version.
pub fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("protocol-v{}", hatchery_protocol::PROTOCOL_MAJOR))
}

/// Every fixture: name, the value to pin, and how to prove it still deserializes.
pub struct Fixture {
    /// File stem inside [`fixture_dir`].
    pub stem: &'static str,
    /// A short description used in failure messages.
    pub shape: &'static str,
    /// The value to pin.
    pub value: Value,
    /// How to deserialize it again.
    pub check: Check,
}

/// One registered fixture by its stem, for tests that pin a specific value.
///
/// # Panics
///
/// When no fixture carries `stem`, which is a typo in the test rather than a protocol change.
pub fn fixture_by_stem(stem: &str) -> Fixture {
    all()
        .into_iter()
        .find(|entry| entry.stem == stem)
        .unwrap_or_else(|| panic!("no fixture named {stem}; see tests/support/mod.rs"))
}

/// Assembles the whole fixture set.
pub fn all() -> Vec<Fixture> {
    let mut fixtures = Vec::new();

    for (stem, item) in items() {
        fixtures.push(Fixture {
            stem,
            shape: "Item",
            value: to_value(&item),
            check: as_type::<Item>(),
        });
    }

    fixtures.push(Fixture {
        stem: "session",
        shape: "Session",
        value: to_value(&session()),
        check: as_type::<Session>(),
    });

    for (stem, event) in events() {
        fixtures.push(Fixture {
            stem,
            shape: "ServerEvent",
            value: to_value(&SessionEvent::new(session_id(), 3, event)),
            check: as_type::<SessionEvent>(),
        });
    }

    for (stem, event) in daemon_events() {
        fixtures.push(Fixture {
            stem,
            shape: "DaemonEvent",
            value: to_value(&event),
            check: as_type::<DaemonEvent>(),
        });
    }

    for (stem, frame) in frames() {
        let check = match stem {
            "frame_request" => as_type::<Request>(),
            "frame_notification" => as_type::<Notification>(),
            "frame_response_ok" | "frame_response_err" => as_type::<Response>(),
            other => panic!("no validator for frame {other:?}"),
        };
        fixtures.push(Fixture {
            stem,
            shape: "frame",
            value: frame,
            check,
        });
    }

    for (method, value) in method_params() {
        fixtures.push(Fixture {
            stem: params_stem(method),
            shape: "params",
            value,
            check: params_validator(method),
        });
    }

    for (method, value) in method_results() {
        fixtures.push(Fixture {
            stem: result_stem(method),
            shape: "result",
            value,
            check: results_validator(method),
        });
    }

    for (stem, value) in session_patches() {
        let check = match stem {
            "session_patch_clear_title" => as_title_clearing_patch(),
            "session_patch_empty" => as_type::<SessionPatch>(),
            other => panic!("no validator for session patch {other:?}"),
        };
        fixtures.push(Fixture {
            stem,
            shape: "SessionPatch",
            value,
            check,
        });
    }

    for (stem, value) in absent_field_shapes() {
        let check = match stem {
            "item_root" => as_type::<Item>(),
            "session_fresh" => as_type::<Session>(),
            other => panic!("no validator for absent-field shape {other:?}"),
        };
        fixtures.push(Fixture {
            stem,
            shape: "absent-field shape",
            value,
            check,
        });
    }

    for (stem, value) in tables() {
        let check = match stem {
            "table_methods" | "table_item_kinds" => as_json_list(),
            "table_error_codes" => as_json_list(),
            "value_protocol_version" => as_json_string(),
            "value_jsonrpc_version" => as_type::<JsonRpcVersion>(),
            "value_tool_output_spilled" => as_type::<ToolOutput>(),
            "value_tool_progress" => as_type::<ToolProgress>(),
            other => panic!("no validator for table {other:?}"),
        };
        fixtures.push(Fixture {
            stem,
            shape: "table",
            value,
            check,
        });
    }

    fixtures
}

/// The fixture stem for one method's parameters.
///
/// Stems read like file names, not like RPC paths, so `session/branch/list` becomes
/// `params_session_branch_list`. A method missing here panics rather than silently losing its
/// golden.
pub fn params_stem(method: &str) -> &'static str {
    use hatchery_protocol::method as m;

    match method {
        m::DAEMON_HELLO => "params_daemon_hello",
        m::SESSION_NEW => "params_session_new",
        m::SESSION_LOAD => "params_session_load",
        m::SESSION_LIST => "params_session_list",
        m::SESSION_PROMPT => "params_session_prompt",
        m::SESSION_CANCEL => "params_session_cancel",
        m::SESSION_SET_MODE => "params_session_set_mode",
        m::SESSION_SET_CONFIG => "params_session_set_config",
        m::SESSION_EDIT_ITEM => "params_session_edit_item",
        m::SESSION_BRANCH_LIST => "params_session_branch_list",
        m::SESSION_BRANCH_SWITCH => "params_session_branch_switch",
        m::SESSION_BRANCH_DELETE => "params_session_branch_delete",
        m::SESSION_REWIND => "params_session_rewind",
        m::SESSION_DELETE => "params_session_delete",
        m::SESSION_RENAME => "params_session_rename",
        m::APPROVAL_RESPOND => "params_approval_respond",
        m::CONFIG_GET => "params_config_get",
        m::CONFIG_SET => "params_config_set",
        m::PROMPT_RENDER => "params_prompt_render",
        m::STORE_EXPORT_JSONL => "params_store_export_jsonl",
        other => panic!("no parameter fixture stem for method {other:?}; add one"),
    }
}

/// The fixture stem for one method's result. See [`params_stem`].
pub fn result_stem(method: &str) -> &'static str {
    use hatchery_protocol::method as m;

    match method {
        m::DAEMON_HELLO => "result_daemon_hello",
        m::SESSION_NEW => "result_session_new",
        m::SESSION_LOAD => "result_session_load",
        m::SESSION_LIST => "result_session_list",
        m::SESSION_PROMPT => "result_session_prompt",
        m::SESSION_CANCEL => "result_session_cancel",
        m::SESSION_SET_MODE => "result_session_set_mode",
        m::SESSION_SET_CONFIG => "result_session_set_config",
        m::SESSION_EDIT_ITEM => "result_session_edit_item",
        m::SESSION_BRANCH_LIST => "result_session_branch_list",
        m::SESSION_BRANCH_SWITCH => "result_session_branch_switch",
        m::SESSION_BRANCH_DELETE => "result_session_branch_delete",
        m::SESSION_REWIND => "result_session_rewind",
        m::SESSION_DELETE => "result_session_delete",
        m::SESSION_RENAME => "result_session_rename",
        m::APPROVAL_RESPOND => "result_approval_respond",
        m::CONFIG_GET => "result_config_get",
        m::CONFIG_SET => "result_config_set",
        m::PROMPT_RENDER => "result_prompt_render",
        m::STORE_EXPORT_JSONL => "result_store_export_jsonl",
        other => panic!("no result fixture stem for method {other:?}; add one"),
    }
}
