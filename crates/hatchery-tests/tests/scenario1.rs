//! e2e 场景 1（docs/design/testing.md §4）：prompt → 流式 → 落库 → resume。
//!
//! One scripted provider conversation drives the full stack — real socket, real store, real
//! adapter — and the second turn's request body is held against the store byte for byte
//! (invariant 2: what the model sees is exactly what was recorded).

use hatchery_protocol::method as m;
use hatchery_protocol::{ItemKind, ServerEvent};
use hatchery_testkit::daemon::ClientProbe;
use hatchery_testkit::wire::MockWire;
use hatchery_tests::support;

#[tokio::test(flavor = "multi_thread")]
async fn invariant_minimal_chat_replays_reasoning_byte_exact() {
    let wire = MockWire::replay_sse(support::SSE_REASONING_OK).await;
    let daemon = support::daemon_at(&wire).await;
    let probe = ClientProbe::attach(&daemon).await;
    let created: m::SessionNewResult = probe.call(m::SESSION_NEW, &support::chat_params()).await;
    let session = created.session.id;
    let mut events = probe.events(&daemon, session).await;

    // ---- turn 1: the documented kernel sequence, projected on the wire --------------------
    let _turn: m::SessionPromptResult = probe
        .call(
            m::SESSION_PROMPT,
            &support::prompt_params(session, "hi", created.session.generation),
        )
        .await;
    let turn = support::collect_until_terminal(&mut events).await;

    assert_eq!(
        support::item_story(&turn),
        vec![
            "item_finished", // the user message is committed whole
            "item_started",  // the reasoning item opens
            "reasoning_delta",
            "reasoning_delta",
            "reasoning_delta",
            "item_finished", // the reasoning item closes at the text boundary
            "item_started",  // the answer opens
            "text_delta",
            "item_finished",
            "turn_finished",
        ],
        "the whole wire story of one reasoning turn: {turn:?}"
    );
    let streamed: String = turn
        .iter()
        .filter_map(|event| match &event.event {
            ServerEvent::ReasoningDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(streamed, support::REASONING, "deltas concatenate exactly");

    // Every event carries the assembly's generation, and the first one announced it.
    let generation = created.session.generation;
    assert!(
        turn.iter().all(|event| event.generation > generation),
        "the runtime's events are all from the bumped generation"
    );

    // ---- resume: the store holds the branch, newest first is not a thing here — root first ---
    let loaded: m::SessionLoadResult = probe
        .call(
            m::SESSION_LOAD,
            &m::SessionLoadParams {
                session_id: session,
                replay_from: None,
                generation: Some(
                    turn.iter()
                        .map(|event| event.generation)
                        .max()
                        .expect("nonempty"),
                ),
            },
        )
        .await;
    let kinds: Vec<String> = loaded
        .items
        .iter()
        .map(|item| match &item.kind {
            ItemKind::UserMessage(_) => "user".to_owned(),
            ItemKind::Reasoning(_) => "reasoning".to_owned(),
            ItemKind::AssistantMessage(_) => "assistant".to_owned(),
            other => panic!("the chat turn records only these three kinds: {other:?}"),
        })
        .collect();
    assert_eq!(kinds, ["user", "reasoning", "assistant"]);
    let stored_reasoning = match &loaded.items[1].kind {
        ItemKind::Reasoning(block) => block.text.clone(),
        other => panic!("expected the reasoning item: {other:?}"),
    };
    assert_eq!(stored_reasoning, support::REASONING);

    // ---- turn 2: the request must carry turn 1 back, byte for byte (invariant 2) ------------
    let _again: m::SessionPromptResult = probe
        .call(
            m::SESSION_PROMPT,
            &support::prompt_params(session, "and again", loaded.session.generation),
        )
        .await;
    let _second = support::collect_until_terminal(&mut events).await;

    let requests = wire.requests().await;
    assert_eq!(requests.len(), 2, "one POST per turn");
    let first: serde_json::Value =
        serde_json::from_str(&requests[0].body).expect("a JSON request body");
    let first_messages = first["messages"].as_array().expect("messages");
    assert_eq!(
        first_messages[0]["role"], "system",
        "the assembled prompt leads the request"
    );
    let system = first_messages[0]["content"].as_str().expect("system text");
    assert!(!system.is_empty(), "an empty prompt is not a prompt");
    assert_eq!(
        serde_json::to_value(&first_messages[1..]).expect("serialisable"),
        serde_json::json!([{ "role": "user", "content": "hi" }]),
        "the first turn carries the bare prompt behind it"
    );

    let second: serde_json::Value =
        serde_json::from_str(&requests[1].body).expect("a JSON request body");
    assert_eq!(second["model"], "m");
    let second_messages = second["messages"].as_array().expect("messages");
    assert_eq!(
        second_messages[0]["content"].as_str(),
        Some(system),
        "rendered once per assembly and frozen for the runtime's life: re-rendering per turn \
         would move the date and the cwd under the provider's prefix cache"
    );
    // The full context, rebuilt from the store by hand — this is invariant 2's two sides held
    // next to each other: serde string equality is byte equality, whitespace included.
    assert_eq!(
        serde_json::to_value(&second_messages[1..]).expect("serialisable"),
        serde_json::json!([
            { "role": "user", "content": "hi" },
            {
                "role": "assistant",
                "content": support::ANSWER,
                "reasoning_content": support::REASONING,
            },
            { "role": "user", "content": "and again" },
        ]),
        "the second request is the recorded history plus the new prompt, byte for byte"
    );

    // Invariant 2's boundary: the branch history is pinned byte for byte above, and the system
    // prompt — a derived artifact that is deliberately not an item — is pinned here, by the
    // method whose whole purpose is answering "why did the model see this?".
    let rendered: m::PromptRenderResult = probe
        .call(
            m::PROMPT_RENDER,
            &m::PromptRenderParams {
                session_id: Some(session),
                mode: None,
            },
        )
        .await;
    assert_eq!(
        rendered.text, system,
        "`prompt/render` reports the text the model was actually sent"
    );
    assert!(
        rendered
            .sections
            .iter()
            .all(|section| !section.text.is_empty()),
        "every section says something: {:?}",
        rendered.sections
    );

    daemon.stop().await;
}

/// The protocol's `replay_from` is the reconnecting frontend's gap-fill: items strictly after
/// the cursor, refused when the cursor is not on the active branch. `session/load` with `None`
/// is the full-branch path a fresh frontend takes.
#[tokio::test(flavor = "multi_thread")]
async fn a_reconnecting_frontend_gap_fills_from_the_store() {
    let wire = MockWire::replay_sse(support::SSE_REASONING_OK).await;
    let daemon = support::daemon_at(&wire).await;
    let probe = ClientProbe::attach(&daemon).await;
    let created: m::SessionNewResult = probe.call(m::SESSION_NEW, &support::chat_params()).await;
    let session = created.session.id;
    let mut events = probe.events(&daemon, session).await;

    let _turn: m::SessionPromptResult = probe
        .call(
            m::SESSION_PROMPT,
            &support::prompt_params(session, "hi", created.session.generation),
        )
        .await;
    let turn = support::collect_until_terminal(&mut events).await;

    let finished: Vec<hatchery_protocol::ItemId> = turn
        .iter()
        .filter_map(|event| match &event.event {
            ServerEvent::ItemFinished { item, .. } => Some(item.id),
            _ => None,
        })
        .collect();
    assert_eq!(finished.len(), 3, "user, reasoning, answer");

    let generation = turn
        .iter()
        .map(|event| event.generation)
        .max()
        .expect("nonempty");
    let gap: m::SessionLoadResult = probe
        .call(
            m::SESSION_LOAD,
            &m::SessionLoadParams {
                session_id: session,
                replay_from: Some(finished[0]),
                generation: Some(generation),
            },
        )
        .await;
    assert_eq!(
        gap.items.len(),
        2,
        "everything after the first item, and nothing before it"
    );

    let whole: m::SessionLoadResult = probe
        .call(
            m::SESSION_LOAD,
            &m::SessionLoadParams {
                session_id: session,
                replay_from: None,
                generation: Some(generation),
            },
        )
        .await;
    assert_eq!(whole.items.len(), 3, "a fresh frontend gets the branch");

    let off_branch: Result<m::SessionLoadResult, hatchery_protocol::ClientError> = probe
        .try_call(
            m::SESSION_LOAD,
            &m::SessionLoadParams {
                session_id: session,
                replay_from: Some(hatchery_protocol::ItemId::new()),
                generation: Some(generation),
            },
        )
        .await;
    let error = off_branch.expect_err("a cursor off the branch is refused");
    assert!(
        error.to_string().contains("active branch"),
        "the refusal names the problem: {error}"
    );

    daemon.stop().await;
}

/// The full envelope order, housekeeping included: `generation_bumped` opens a runtime's stream
/// and `session_updated` frames the item story. `item_story` strips exactly these two, so a
/// test that reads the whole stream is what catches an ordering regression between them — and
/// the prompt reply's turn id must be the one the terminal event carries.
#[tokio::test(flavor = "multi_thread")]
async fn the_wire_orders_housekeeping_around_item_events_and_names_the_turn() {
    let wire = MockWire::replay_sse(support::SSE_REASONING_OK).await;
    let daemon = support::daemon_at(&wire).await;
    let probe = ClientProbe::attach(&daemon).await;
    let created: m::SessionNewResult = probe.call(m::SESSION_NEW, &support::chat_params()).await;
    let session = created.session.id;
    let mut events = probe.events(&daemon, session).await;

    let result: m::SessionPromptResult = probe
        .call(
            m::SESSION_PROMPT,
            &support::prompt_params(session, "hi", created.session.generation),
        )
        .await;
    let turn = support::collect_until_terminal(&mut events).await;
    let kinds: Vec<String> = turn.iter().map(support::event_type).collect();

    assert_eq!(
        kinds.first().map(String::as_str),
        Some("generation_bumped"),
        "a runtime announces itself before anything else: {kinds:?}"
    );
    assert!(
        kinds
            .iter()
            .position(|kind| kind == "session_updated")
            .is_some_and(|update| {
                kinds
                    .iter()
                    .position(|kind| kind.ends_with("_delta"))
                    .is_none_or(|delta| update < delta)
            }),
        "the assembling housekeeping precedes every delta: {kinds:?}"
    );

    // The daemon minted the turn id, the kernel honoured it: the reply and the terminal event
    // name the same turn.
    match &turn.last().expect("nonempty").event {
        ServerEvent::TurnFinished { turn, .. } => {
            assert_eq!(*turn, result.turn, "the reply's turn is the finished turn");
        }
        other => panic!("expected a turn_finished last: {other:?}"),
    }

    daemon.stop().await;
}
