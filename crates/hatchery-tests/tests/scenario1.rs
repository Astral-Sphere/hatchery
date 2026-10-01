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
    assert_eq!(
        first["messages"].as_array().expect("messages").len(),
        1,
        "the first turn starts from the bare prompt"
    );

    let second: serde_json::Value =
        serde_json::from_str(&requests[1].body).expect("a JSON request body");
    assert_eq!(second["model"], "m");
    // The full context, rebuilt from the store by hand — this is invariant 2's two sides held
    // next to each other: serde string equality is byte equality, whitespace included.
    assert_eq!(
        second["messages"],
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
