//! e2e 场景 2（docs/design/testing.md §4）：双前端扇出一致，断线重连补差。
//!
//! Two probes on two real sockets must see the same envelope sequence — the hub's broadcast
//! gives every subscriber the same order — and a frontend that dropped its connection rebuilds
//! through `session/load` (the hub has no replay window; that is the documented M1 shape), then
//! keeps streaming live events from its new subscription.

use hatchery_protocol::EventStream;
use hatchery_protocol::ServerEvent;
use hatchery_protocol::method as m;
use hatchery_testkit::daemon::ClientProbe;
use hatchery_testkit::wire::MockWire;
use hatchery_tests::support;

#[tokio::test(flavor = "multi_thread")]
async fn two_frontends_see_identical_sequences_and_a_reconnect_gap_fills() {
    let wire = MockWire::replay_sse(support::SSE_REASONING_OK).await;
    let daemon = support::daemon_at(&wire).await;

    let first = ClientProbe::attach(&daemon).await;
    let created: m::SessionNewResult = first.call(m::SESSION_NEW, &support::chat_params()).await;
    let session = created.session.id;

    // Both frontends subscribe before anything happens, so both must see the whole turn.
    let mut left = first.events(&daemon, session).await;
    let second = ClientProbe::attach(&daemon).await;
    let mut right = second.events(&daemon, session).await;

    let _turn: m::SessionPromptResult = first
        .call(
            m::SESSION_PROMPT,
            &support::prompt_params(session, "hi", created.session.generation),
        )
        .await;
    let seen_by_left = support::collect_until_terminal(&mut left).await;
    let seen_by_right = support::collect_until_terminal(&mut right).await;

    assert_eq!(
        seen_by_left, seen_by_right,
        "every attached frontend sees the same events in the same order"
    );
    assert!(!seen_by_left.is_empty());
    assert!(
        seen_by_left.iter().all(|event| event.session == session),
        "every envelope names the session"
    );

    // ---- reconnect: the subscribing connection dies; the frontend rebuilds from the store -----
    let last_finished = seen_by_left
        .iter()
        .rev()
        .filter_map(|event| match &event.event {
            ServerEvent::ItemFinished { item, .. } => Some(item.id),
            _ => None,
        })
        .next()
        .expect("the turn committed items");
    let generation = seen_by_left
        .iter()
        .map(|event| event.generation)
        .max()
        .expect("nonempty");

    drop(left);

    // The hub has no replay window, so the reconnector asks the store for everything after the
    // last item it rendered — the gap-fill — and re-subscribes for what comes next.
    let reattached = ClientProbe::attach(&daemon).await;
    let gap: m::SessionLoadResult = reattached
        .call(
            m::SESSION_LOAD,
            &m::SessionLoadParams {
                session_id: session,
                replay_from: Some(last_finished),
                generation: Some(generation),
            },
        )
        .await;
    assert!(
        gap.items.is_empty(),
        "the frontend had already seen every item: the gap is empty"
    );

    let mut live = EventStream::connect(&daemon.socket)
        .await
        .expect("reconnect");
    live.subscribe(
        m::SESSION_LOAD,
        serde_json::json!({
            "session_id": session,
            "generation": generation,
        }),
    )
    .await
    .expect("resubscribed");

    let _next: m::SessionPromptResult = reattached
        .call(
            m::SESSION_PROMPT,
            &support::prompt_params(session, "after reconnect", generation),
        )
        .await;
    let next_turn = support::collect_until_terminal(&mut live).await;
    assert_eq!(
        support::item_story(&next_turn).first().expect("nonempty"),
        "item_finished",
        "the reconnected stream carries the new turn from its first commit"
    );

    daemon.stop().await;
}

/// Unused on this binary but shared by the sibling scenario files through `support`; naming it
/// here keeps the helper's own smoke path exercised on every run.
#[tokio::test(flavor = "multi_thread")]
async fn the_shared_fixture_wire_streams_the_documented_shape() {
    let wire = MockWire::replay_sse(support::SSE_REASONING_OK).await;
    let daemon = support::daemon_at(&wire).await;
    let probe = ClientProbe::attach(&daemon).await;
    let created: m::SessionNewResult = probe.call(m::SESSION_NEW, &support::chat_params()).await;
    let mut events = probe.events(&daemon, created.session.id).await;

    let _turn: m::SessionPromptResult = probe
        .call(
            m::SESSION_PROMPT,
            &support::prompt_params(created.session.id, "hi", created.session.generation),
        )
        .await;
    let turn = support::collect_until_terminal(&mut events).await;
    let types: Vec<String> = turn.iter().map(support::event_type).collect();
    assert!(types.contains(&"reasoning_delta".to_owned()), "{types:?}");
    assert!(types.contains(&"turn_finished".to_owned()), "{types:?}");

    daemon.stop().await;
}
