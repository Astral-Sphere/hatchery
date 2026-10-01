//! The core-invariant suite (docs/design/testing.md §5), invariant 1's remaining legs.
//!
//! Invariant 2 lives in `scenario1.rs` (byte-exact replay), 3/4/5/6 in the crates that own
//! them. Every test here carries the `invariant_` prefix the nextest profile filters on, and
//! the default group runs them too — invariants never skip.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use hatchery_daemon::discover::StateDir;
use hatchery_protocol::method as m;
use hatchery_protocol::{ClientError, EventStream, SessionEvent};
use hatchery_testkit::daemon::ClientProbe;
use hatchery_testkit::wire::MockWire;
use hatchery_tests::support;

/// Invariant 1, client half: an event whose generation is below the highest already seen is an
/// older runtime's straggler, and the frontend must never render it.
///
/// The server here is a hand-written socket: it answers the subscribing call and then pushes
/// generations 5, 3, 7 in order — exactly what a superseded runtime's tail would look like.
#[tokio::test(flavor = "multi_thread")]
async fn invariant_stale_runtime_events_are_dropped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("events.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind");

    let session = hatchery_protocol::SessionId::new().to_string();
    let push_session = session.clone();
    tokio::spawn(async move {
        let (connection, _) = listener.accept().await.expect("a client");
        let (read, mut write) = connection.into_split();
        let mut lines = BufReader::new(read).lines();

        // The subscribing call's reply — the client resolves `subscribe` on it.
        let request = lines
            .next_line()
            .await
            .expect("the connection lived")
            .expect("a subscribing request arrived");
        assert!(request.contains(m::SESSION_LOAD), "{request}");
        let reply = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":10000,\"result\":{{\"session\":{{\"id\":\"{push_session}\"}}}}}}\n"
        );
        write.write_all(reply.as_bytes()).await.expect("write");

        for generation in [5_u64, 3, 7] {
            // `SessionEvent` flattens the server event, so `type` is a top-level key here.
            let frame = format!(
                "{{\"jsonrpc\":\"2.0\",\"method\":\"session/event\",\"params\":{{\"session\":\"{push_session}\",\"generation\":{generation},\"type\":\"generation_bumped\"}}}}\n"
            );
            write
                .write_all(frame.as_bytes())
                .await
                .expect("event write");
            write.flush().await.expect("flush");
        }
        // Held open until the client goes away; dropping ends the test's stream.
        let _ = lines.next_line().await;
    });

    let mut stream = EventStream::connect(&socket).await.expect("connect");
    stream
        .subscribe(m::SESSION_LOAD, serde_json::json!({"session_id": session}))
        .await
        .expect("subscribed");

    let seen = async {
        let mut generations = Vec::new();
        while generations.len() < 2 {
            let event: SessionEvent = tokio::time::timeout(Duration::from_secs(5), stream.next())
                .await
                .expect("an event in time")
                .expect("stream alive");
            generations.push(event.generation);
        }
        generations
    };
    assert_eq!(
        seen.await,
        vec![5, 7],
        "the stale generation-3 event never reaches the frontend"
    );
}

/// Invariant 1, server half: the runtime slot is the session's lease. A second prompt while a
/// turn is in flight is refused with the protocol's own error — not queued, not dropped
/// silently.
#[tokio::test(flavor = "multi_thread")]
async fn invariant_session_lease_blocks_second_runtime() {
    // The provider takes five seconds to answer, so the turn is genuinely in flight while the
    // test acts — deterministically, with no sleeping on the test's side.
    let wire = MockWire::replay_sse_after(support::SSE_REASONING_OK, Duration::from_secs(5)).await;
    let daemon = support::daemon_at(&wire).await;
    let probe = ClientProbe::attach(&daemon).await;
    let created: m::SessionNewResult = probe.call(m::SESSION_NEW, &support::chat_params()).await;
    let session = created.session.id;
    let mut events = probe.events(&daemon, session).await;

    let _first: m::SessionPromptResult = probe
        .call(
            m::SESSION_PROMPT,
            &support::prompt_params(session, "first", created.session.generation),
        )
        .await;

    // Wait until the turn has actually opened (its first commit is on the wire), then try to
    // slip a second prompt past the lease.
    let first_commit = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.next().await.expect("stream alive");
            if matches!(
                &event.event,
                hatchery_protocol::ServerEvent::ItemFinished { .. }
            ) {
                return event;
            }
        }
    })
    .await
    .expect("the turn opened in time");

    let second: Result<m::SessionPromptResult, ClientError> = probe
        .try_call(
            m::SESSION_PROMPT,
            &support::prompt_params(session, "second", first_commit.generation),
        )
        .await;
    let error = second.expect_err("the lease refuses the second prompt");
    assert!(
        error.to_string().contains("already running"),
        "the refusal is the TurnInProgress error: {error}"
    );

    // And the first turn is undisturbed: cancel it and see it end as its own kind of done —
    // an interrupted turn is a finished turn with the interrupted stop reason, not a failure.
    let _cancelled: m::SessionCancelResult = probe
        .call(
            m::SESSION_CANCEL,
            &m::SessionCancelParams {
                session_id: session,
            },
        )
        .await;
    let ended = support::collect_until_terminal(&mut events).await;
    let interrupted = ended.iter().find_map(|event| match &event.event {
        hatchery_protocol::ServerEvent::TurnFinished { completion, .. } => Some(completion),
        _ => None,
    });
    assert_eq!(
        interrupted.map(|completion| completion.reason),
        Some(hatchery_protocol::StopReason::Interrupted),
        "the cancelled turn ends, marked interrupted: {ended:?}"
    );

    daemon.stop().await;
}

/// Invariant 1, process half: one state directory, one daemon. Twenty racing starters, one
/// winner — the file lock decides, not luck.
#[test]
fn invariant_single_instance_race_admits_exactly_one_winner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = std::sync::Arc::new(StateDir::at(dir.path().join("state")));
    state.ensure().expect("state dir");

    let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..20 {
        let state = std::sync::Arc::clone(&state);
        let winners = std::sync::Arc::clone(&winners);
        handles.push(std::thread::spawn(move || {
            if let Ok(_guard) = state.acquire_instance() {
                // The guard is held for the thread's whole life: releasing early would let a
                // later racer win twice, which would prove nothing about the race.
                winners.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(50));
            }
        }));
    }
    for handle in handles {
        handle.join().expect("no racer panicked");
    }
    assert_eq!(
        winners.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "exactly one starter may hold the instance"
    );

    // And after every contender is gone, the lock is free again.
    drop(state.acquire_instance().expect("released on drop"));
}
