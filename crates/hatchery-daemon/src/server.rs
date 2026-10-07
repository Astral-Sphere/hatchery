//! The transport: newline-delimited JSON-RPC over UDS (and stdio), one task per connection.
//!
//! The connection loop is deliberately thin: read a frame, hand it to [`DaemonCore::dispatch`],
//! write the reply. Everything with policy in it lives behind the core, which is why the tests
//! can drive the whole protocol without a socket.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufWriter};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

use hatchery_protocol::SessionEvent;
use hatchery_protocol::{FrameDecoder, Incoming, Response, method as m};

use crate::core::DaemonCore;
use crate::hub::LiveHub;
use crate::manager::SessionManager;

/// Serves one UDS path forever. Returns when the listener dies.
///
/// # Errors
///
/// Propagates the bind failure — an unbound listener is the startup audit's business, surfaced
/// before this is called; a listener that dies mid-flight is logged by the caller.
pub async fn serve_uds(
    core: Arc<DaemonCore>,
    manager: Arc<SessionManager>,
    hub: Arc<LiveHub>,
    path: std::path::PathBuf,
) -> std::io::Result<()> {
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    crate::discover::restrict_to_owner(&path);
    tracing::info!(socket = %path.display(), "the daemon is listening");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let core = Arc::clone(&core);
                let manager = Arc::clone(&manager);
                let hub = Arc::clone(&hub);
                tokio::spawn(async move {
                    let (read, write) = stream.into_split();
                    serve_connection(core, manager, hub, read, write).await;
                });
            }
            Err(error) => {
                tracing::warn!("a connection failed to accept: {error}");
            }
        }
    }
}

/// Serves stdin/stdout until stdin closes. The embedded-transport shape: an embedding host
/// (or a test) drives the daemon over pipes instead of a socket, same frames, same core.
pub async fn serve_stdio(core: Arc<DaemonCore>, manager: Arc<SessionManager>, hub: Arc<LiveHub>) {
    serve_connection(core, manager, hub, tokio::io::stdin(), tokio::io::stdout()).await;
}

/// One connection's life: request frames in, reply frames out, optionally an event subscription.
///
/// `session/new` and `session/load` implicitly subscribe the connection to that session's event
/// stream; a later subscription on the same connection replaces the earlier one. Generic over
/// the transport: a UDS stream and the stdio pipes behave identically here.
async fn serve_connection<R, W>(
    core: Arc<DaemonCore>,
    manager: Arc<SessionManager>,
    hub: Arc<LiveHub>,
    mut reader: R,
    writer: W,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    // Replies and forwarded events share one writer under a mutex: two tasks, one socket, and
    // interleaved frames are fine because each frame is written in one locked pass.
    let writer: std::sync::Arc<tokio::sync::Mutex<BufWriter<W>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(BufWriter::new(writer)));
    let mut decoder = FrameDecoder::default();
    // Raw chunks go straight into the decoder, which owns the frame cap: handing it whole
    // `read_line` results instead would buffer a runaway line in this task's memory before the
    // decoder ever saw it, defeating the limit the decoder exists to enforce.
    let mut chunk = vec![0_u8; 16 * 1024];
    // The session this connection subscribes to, and the task forwarding its events.
    let mut subscribed: Option<hatchery_protocol::SessionId> = None;
    let mut forwarding: Option<(CancellationToken, tokio::task::JoinHandle<()>)> = None;

    'read: loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) => break, // the client hung up
            Ok(read) => read,
            Err(error) => {
                tracing::warn!("a connection failed mid-read: {error}");
                break;
            }
        };

        // Frames the chunk completed before a decoder failure still get served; the failure
        // itself gets the parse-error reply a broken frame deserves. The reply's id is a lie by
        // construction, so it is drawn from a range the client never mints (its ids count up
        // from 10_000) rather than one a real request could carry. The decoder drops the
        // offending bytes, so the next push starts clean.
        let mut scans = vec![decoder.push(&chunk[..read])];
        while let Some(mut scan) = scans.pop() {
            if let Some(error) = scan.error.take() {
                let reply = Response::err(
                    hatchery_protocol::Id::Number(-1),
                    hatchery_protocol::ErrorObject::new(
                        hatchery_protocol::ErrorCode::ParseError,
                        error.to_string(),
                    ),
                );
                if write_reply(&writer, &reply).await.is_err() {
                    break 'read;
                }
                // A complete frame that shared the chunk with the bad line is still buffered, and
                // no further bytes may ever arrive to flush it: scan for it now.
                scans.push(decoder.push(&[]));
            }
            for frame in scan.frames {
                match hatchery_protocol::decode_frame(&frame) {
                    Ok(Incoming::Request(request)) => {
                        let response = core.dispatch(&request).await;
                        // `session/new` and `session/load` implicitly subscribe this connection to
                        // the session's events (docs/design/protocol.md §4) — no second call
                        // needed. A second subscription replaces the first: the old forwarding
                        // task is cancelled and its session detached, or the connection would
                        // receive two interleaved event streams while the manager counts one
                        // subscriber.
                        if response.is_ok()
                            && matches!(request.method.as_str(), m::SESSION_NEW | m::SESSION_LOAD)
                            && let Some(session) = reply_session(&response)
                        {
                            if let Some(previous) = subscribed.take() {
                                crate::core::detach_session(&manager, &hub, previous);
                            }
                            if let Some((cancel, task)) = forwarding.take() {
                                cancel.cancel();
                                task.abort();
                            }
                            let receiver = crate::core::subscribe_session(&manager, &hub, session);
                            subscribed = Some(session);
                            let cancel = CancellationToken::new();
                            let task = tokio::spawn(forward_events(
                                Arc::clone(&manager),
                                Arc::clone(&hub),
                                session,
                                receiver,
                                cancel.clone(),
                                Arc::clone(&writer),
                            ));
                            forwarding = Some((cancel, task));
                        }
                        if write_reply(&writer, &response).await.is_err() {
                            break 'read;
                        }
                    }
                    Ok(Incoming::Notification(notification)) => {
                        tracing::warn!(
                            "a `{}` notification arrived; M1 serves none",
                            notification.method
                        );
                    }
                    Ok(Incoming::Response(_)) => {
                        tracing::warn!("a response frame arrived from a client; ignored");
                    }
                    Err(error) => {
                        let reply = Response::err(
                            hatchery_protocol::Id::Number(-1),
                            hatchery_protocol::ErrorObject::new(
                                hatchery_protocol::ErrorCode::ParseError,
                                error.to_string(),
                            ),
                        );
                        if write_reply(&writer, &reply).await.is_err() {
                            break 'read;
                        }
                    }
                }
            }
        }
    }

    if let Some((cancel, task)) = forwarding.take() {
        cancel.cancel();
        task.abort();
    }
    if let Some(session) = subscribed {
        crate::core::detach_session(&manager, &hub, session);
    }
}

/// Pulls the session id out of a `session/new` / `session/load` reply.
fn reply_session(response: &Response) -> Option<hatchery_protocol::SessionId> {
    let value = response.result.as_ref()?;
    // `session` is a whole Session object; the id is a leaf.
    let id = value.get("session")?.get("id")?.clone();
    serde_json::from_value(id).ok()
}

/// The method names whose success subscribes the connection, for tests.
pub const SUBSCRIBING_METHODS: &[&str] = &[m::SESSION_NEW, m::SESSION_LOAD];

/// The notification method events travel under.
pub const EVENT_NOTIFICATION: &str = "session/event";

/// Fans hub events out over one connection until it dies, the session unloads, or the connection
/// subscribes to a different session (the token).
///
/// A slow reader lags: `RecvError::Lagged` means events were lost in the buffer, and the honest
/// move is to stop streaming rather than serve a torn view — the frontend rebuilds through
/// `session/load` (docs/design/daemon.md §4).
async fn forward_events<W>(
    manager: Arc<SessionManager>,
    hub: Arc<LiveHub>,
    session: hatchery_protocol::SessionId,
    mut receiver: tokio::sync::broadcast::Receiver<SessionEvent>,
    cancel: CancellationToken,
    writer: std::sync::Arc<tokio::sync::Mutex<BufWriter<W>>>,
) where
    W: AsyncWrite + Unpin + Send + 'static,
{
    loop {
        let event = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            received = receiver.recv() => received,
        };
        match event {
            Ok(event) => {
                // Events ride a JSON-RPC notification (`session/event`, params = the envelope),
                // so every peer classifier accepts the frame without protocol special cases.
                let notification = match hatchery_protocol::Notification::new(EVENT_NOTIFICATION)
                    .with_params(&event)
                {
                    Ok(notification) => notification,
                    Err(error) => {
                        tracing::error!("an event failed to encode: {error}");
                        continue;
                    }
                };
                let frame = match hatchery_protocol::encode_frame(&notification) {
                    Ok(frame) => frame,
                    Err(error) => {
                        tracing::error!("an event failed to encode: {error}");
                        continue;
                    }
                };
                let mut writer = writer.lock().await;
                if writer.write_all(frame.as_bytes()).await.is_err()
                    || writer.flush().await.is_err()
                {
                    drop(writer);
                    crate::core::detach_session(&manager, &hub, session);
                    return;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(lost)) => {
                tracing::warn!(
                    session = %session,
                    "a subscriber fell {lost} events behind; disconnecting it for a rebuild"
                );
                crate::core::detach_session(&manager, &hub, session);
                return;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn write_reply<W>(
    writer: &std::sync::Arc<tokio::sync::Mutex<BufWriter<W>>>,
    reply: &Response,
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin + Send,
{
    // The frame already ends in its newline (`encode_frame`); writing a second one would put a
    // blank line on the wire after every reply, which strict line readers must not have to skip.
    let line = hatchery_protocol::encode_frame(reply)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut writer = writer.lock().await;
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::AsyncBufReadExt;

    use hatchery_store::SessionStore;

    use crate::config::LayeredConfig;
    use crate::core::DaemonCore;
    use crate::hub::LiveHub;
    use crate::manager::SessionManager;

    /// A full core over duplex pipes — the stdio transport with a dial instead of a TTY.
    async fn core() -> (Arc<DaemonCore>, Arc<SessionManager>, Arc<LiveHub>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn SessionStore> = Arc::new(
            hatchery_store::TursoStore::open(dir.path().join("t.db"))
                .await
                .expect("open"),
        );
        let config = Arc::new(LayeredConfig::from_layers(vec![]));
        let hub = Arc::new(LiveHub::new());
        let manager = Arc::new(SessionManager::new(
            Arc::clone(&store),
            Arc::clone(&config),
            Arc::clone(&hub),
            dir.path().to_path_buf(),
            None,
        ));
        let core = Arc::new(DaemonCore::new(
            Arc::clone(&manager),
            Arc::clone(&config),
            Arc::clone(&store),
            "token".to_owned(),
        ));
        // The tempdir must outlive the returned stack pieces; leak it — a test-scoped leak.
        std::mem::forget(dir);
        (core, manager, hub)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_pipes_transport_serves_the_same_core() {
        let (core, manager, hub) = core().await;
        // Two duplex pairs: one carries requests in, one carries replies out — matching how
        // stdin and stdout are separate pipes in the real stdio transport.
        let (mut request_tx, request_rx) = tokio::io::duplex(64 * 1024);
        let (reply_rx, reply_tx) = tokio::io::duplex(64 * 1024);
        tokio::spawn(serve_connection(core, manager, hub, request_rx, reply_tx));

        let hello =
            hatchery_protocol::Request::new(hatchery_protocol::Id::Number(1), m::DAEMON_HELLO)
                .with_params(&serde_json::json!({
                    "protocol_version": hatchery_protocol::PROTOCOL_VERSION,
                    "boot_token": "token",
                }))
                .map(|request| hatchery_protocol::encode_frame(&request).expect("frame"))
                .expect("request");
        request_tx.write_all(hello.as_bytes()).await.expect("write");
        request_tx.write_all(b"\n").await.expect("newline");

        let mut line = String::new();
        let mut replies = tokio::io::BufReader::new(reply_rx);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            replies.read_line(&mut line),
        )
        .await
        .expect("a reply in time")
        .expect("read");
        let reply = serde_json::from_str::<serde_json::Value>(line.trim())
            .expect("the reply is one JSON line");
        assert_eq!(reply["id"], 1, "{reply}");
        assert_eq!(
            reply["result"]["protocol_version"],
            hatchery_protocol::PROTOCOL_VERSION
        );
    }

    #[test]
    fn subscription_rides_the_documented_methods() {
        assert_eq!(SUBSCRIBING_METHODS, &[m::SESSION_NEW, m::SESSION_LOAD]);
    }

    #[test]
    fn a_session_id_is_pulled_from_both_reply_shapes() {
        let id = hatchery_protocol::SessionId::new();
        let new_reply = Response::ok(
            hatchery_protocol::Id::Number(1),
            &serde_json::json!({"session": {"id": id}}),
        )
        .expect("reply");
        assert_eq!(reply_session(&new_reply), Some(id));

        let load_reply = Response::ok(
            hatchery_protocol::Id::Number(2),
            &serde_json::json!({"session": {"id": id}, "items": []}),
        )
        .expect("reply");
        assert_eq!(reply_session(&load_reply), Some(id));

        let unrelated = Response::ok(
            hatchery_protocol::Id::Number(3),
            &serde_json::json!({"entry": {}}),
        )
        .expect("reply");
        assert_eq!(reply_session(&unrelated), None);
    }

    /// Sends one request into the connection and returns the first reply line carrying its id.
    async fn call<R: tokio::io::AsyncRead + Unpin>(
        request_tx: &mut (impl tokio::io::AsyncWrite + Unpin),
        replies: &mut tokio::io::BufReader<R>,
        buffer: &mut String,
        id: i64,
        method: &str,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let request = hatchery_protocol::Request::new(hatchery_protocol::Id::Number(id), method)
            .with_params(params)
            .map(|request| hatchery_protocol::encode_frame(&request).expect("frame"))
            .expect("request");
        tokio::io::AsyncWriteExt::write_all(request_tx, request.as_bytes())
            .await
            .expect("write");
        tokio::io::AsyncWriteExt::write_all(request_tx, b"\n")
            .await
            .expect("newline");
        loop {
            buffer.clear();
            tokio::time::timeout(std::time::Duration::from_secs(5), replies.read_line(buffer))
                .await
                .expect("a reply in time")
                .expect("read");
            let reply =
                serde_json::from_str::<serde_json::Value>(buffer.trim()).expect("one JSON line");
            if reply["id"] == id {
                return reply;
            }
        }
    }

    fn hello_params() -> serde_json::Value {
        serde_json::json!({
            "protocol_version": hatchery_protocol::PROTOCOL_VERSION,
            "boot_token": "token",
        })
    }

    async fn current_state(
        manager: &SessionManager,
        session: hatchery_protocol::SessionId,
    ) -> hatchery_protocol::Session {
        manager
            .load(hatchery_protocol::method::SessionLoadParams {
                session_id: session,
                replay_from: None,
                generation: None,
            })
            .await
            .expect("load")
            .session
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_oversized_line_is_refused_and_named_as_such() {
        let (core, manager, hub) = core().await;
        let (mut request_tx, request_rx) = tokio::io::duplex(64 * 1024);
        let (reply_rx, reply_tx) = tokio::io::duplex(64 * 1024);
        tokio::spawn(serve_connection(core, manager, hub, request_rx, reply_tx));

        // A frame past the decoder's cap, streamed from a task: the small duplex backs pressure
        // long before the limit is reached, and the daemon must keep draining while refusing.
        let flood = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let chunk = vec![b'a'; 32 * 1024];
            for _ in 0..((hatchery_protocol::FrameDecoder::MAX_FRAME_BYTES / chunk.len()) + 8) {
                if request_tx.write_all(&chunk).await.is_err() {
                    return;
                }
            }
            let _ = request_tx.write_all(b"\n").await;
            // The connection must still be alive afterwards: a normal request is answered.
            let request =
                hatchery_protocol::Request::new(hatchery_protocol::Id::Number(7), m::DAEMON_HELLO)
                    .with_params(&hello_params())
                    .map(|request| hatchery_protocol::encode_frame(&request).expect("frame"))
                    .expect("request");
            let _ = request_tx.write_all(request.as_bytes()).await;
            let _ = request_tx.write_all(b"\n").await;
        });

        use tokio::io::AsyncBufReadExt;
        let mut replies = tokio::io::BufReader::new(reply_rx);
        let mut buffer = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            buffer.clear();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                replies.read_line(&mut buffer),
            )
            .await
            .expect("a reply in time")
            .expect("read");
            let reply =
                serde_json::from_str::<serde_json::Value>(buffer.trim()).expect("one JSON line");
            if reply["error"]["code"] == hatchery_protocol::ErrorCode::ParseError.as_i64() {
                assert!(
                    reply["error"]["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("limit")),
                    "the refusal names the cap: {reply}"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the flood never produced a refusal"
            );
        }
        // The id-7 hello comes back through the same connection.
        loop {
            buffer.clear();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                replies.read_line(&mut buffer),
            )
            .await
            .expect("a reply in time")
            .expect("read");
            let reply =
                serde_json::from_str::<serde_json::Value>(buffer.trim()).expect("one JSON line");
            if reply["id"] == 7 {
                assert!(
                    reply["result"].is_object(),
                    "alive after the flood: {reply}"
                );
                break;
            }
        }
        flood.await.expect("the flood writer finished");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_invalid_utf8_line_is_skipped_and_the_connection_survives() {
        let (core, manager, hub) = core().await;
        let (mut request_tx, request_rx) = tokio::io::duplex(64 * 1024);
        let (reply_rx, reply_tx) = tokio::io::duplex(64 * 1024);
        tokio::spawn(serve_connection(core, manager, hub, request_rx, reply_tx));

        use tokio::io::AsyncWriteExt;
        request_tx
            .write_all(b"\xff\xfe this line is not utf-8 at all\n")
            .await
            .expect("write the bad line");
        let mut replies = tokio::io::BufReader::new(reply_rx);
        let mut buffer = String::new();
        let hello = call(
            &mut request_tx,
            &mut replies,
            &mut buffer,
            2,
            m::DAEMON_HELLO,
            &hello_params(),
        )
        .await;
        assert!(
            hello["result"].is_object(),
            "one bad line drops one line, not the connection"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_subscription_stops_the_first_sessions_events() {
        let (core, manager, hub) = core().await;
        let (mut request_tx, request_rx) = tokio::io::duplex(64 * 1024);
        let (reply_rx, reply_tx) = tokio::io::duplex(64 * 1024);
        tokio::spawn(serve_connection(
            core,
            Arc::clone(&manager),
            Arc::clone(&hub),
            request_rx,
            reply_tx,
        ));

        let mut replies = tokio::io::BufReader::new(reply_rx);
        let mut buffer = String::new();
        // Session A is created over the connection (subscribing it); session B is then loaded on
        // the same connection, which must replace — not join — the subscription.
        let created = call(
            &mut request_tx,
            &mut replies,
            &mut buffer,
            10,
            m::SESSION_NEW,
            &serde_json::json!({"mode": "chat"}),
        )
        .await;
        let session_a: hatchery_protocol::SessionId =
            serde_json::from_value(created["result"]["session"]["id"].clone()).expect("session a");
        let other = manager
            .new_session(hatchery_protocol::method::SessionNewParams {
                mode: hatchery_protocol::SessionModeId::chat(),
                workspace: None,
                model: None,
                title: None,
                config_patch: None,
            })
            .await
            .expect("second session")
            .session
            .id;
        call(
            &mut request_tx,
            &mut replies,
            &mut buffer,
            12,
            m::SESSION_LOAD,
            &serde_json::json!({"session_id": other}),
        )
        .await;

        // Both sessions publish a fresh state; only the current subscription may arrive.
        hub.publish(hatchery_protocol::SessionEvent::new(
            session_a,
            1,
            hatchery_protocol::ServerEvent::SessionUpdated {
                state: current_state(&manager, session_a).await,
            },
        ));
        hub.publish(hatchery_protocol::SessionEvent::new(
            other,
            1,
            hatchery_protocol::ServerEvent::SessionUpdated {
                state: current_state(&manager, other).await,
            },
        ));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut saw_current = false;
        while std::time::Instant::now() < deadline {
            buffer.clear();
            let read = tokio::time::timeout(
                std::time::Duration::from_millis(300),
                replies.read_line(&mut buffer),
            )
            .await;
            let Ok(Ok(read)) = read else { break };
            if read == 0 {
                break;
            }
            let frame =
                serde_json::from_str::<serde_json::Value>(buffer.trim()).expect("one JSON line");
            if frame["method"] == EVENT_NOTIFICATION {
                let session: hatchery_protocol::SessionId =
                    serde_json::from_value(frame["params"]["session"].clone())
                        .expect("a session on every event");
                assert_eq!(
                    session, other,
                    "events from the replaced subscription must stop"
                );
                saw_current = true;
            }
        }
        assert!(saw_current, "the current subscription still streams");
    }
}
