//! The transport: newline-delimited JSON-RPC over UDS (and stdio), one task per connection.
//!
//! The connection loop is deliberately thin: read a frame, hand it to [`DaemonCore::dispatch`],
//! write the reply. Everything with policy in it lives behind the core, which is why the tests
//! can drive the whole protocol without a socket.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::UnixListener;

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
/// The subscription rides this same connection: after `session/new` or `session/load`, a
/// `subscribe_events` notification turns it into an event stream (docs/design/protocol.md §4).
/// Generic over the transport: a UDS stream and the stdio pipes behave identically here.
async fn serve_connection<R, W>(
    core: Arc<DaemonCore>,
    manager: Arc<SessionManager>,
    hub: Arc<LiveHub>,
    reader: R,
    writer: W,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(reader);
    // Replies and forwarded events share one writer under a mutex: two tasks, one socket, and
    // interleaved frames are fine because each frame is written in one locked pass.
    let writer: std::sync::Arc<tokio::sync::Mutex<BufWriter<W>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(BufWriter::new(writer)));
    let mut decoder = FrameDecoder::default();
    let mut line = String::new();
    // The session this connection subscribed to, if any — for subscriber counting on disconnect.
    let mut subscribed: Option<hatchery_protocol::SessionId> = None;

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // the client hung up
            Ok(_) => {}
            Err(error) => {
                tracing::warn!("a connection failed mid-read: {error}");
                break;
            }
        }

        let frames = match decoder.push(line.as_bytes()) {
            Ok(frames) => frames,
            Err(error) => {
                // A broken frame gets a parse-error reply when the id is knowable; the decoder
                // reports what it could not do, and we answer what we can.
                let reply = Response::err(
                    hatchery_protocol::Id::Number(0),
                    hatchery_protocol::ErrorObject::new(
                        hatchery_protocol::ErrorCode::ParseError,
                        error.to_string(),
                    ),
                );
                if write_reply(&writer, &reply).await.is_err() {
                    break;
                }
                continue;
            }
        };
        for frame in frames {
            match hatchery_protocol::decode_frame(&frame) {
                Ok(Incoming::Request(request)) => {
                    let response = core.dispatch(&request).await;
                    // `session/new` and `session/load` implicitly subscribe this connection to
                    // the session's events (docs/design/protocol.md §4) — no second call needed.
                    if response.is_ok()
                        && matches!(request.method.as_str(), m::SESSION_NEW | m::SESSION_LOAD)
                        && let Some(session) = reply_session(&response)
                    {
                        if let Some(previous) = subscribed.take() {
                            crate::core::detach_session(&manager, &hub, previous);
                        }
                        let receiver = crate::core::subscribe_session(&manager, &hub, session);
                        subscribed = Some(session);
                        let manager = Arc::clone(&manager);
                        let hub = Arc::clone(&hub);
                        tokio::spawn(forward_events(
                            manager,
                            hub,
                            session,
                            receiver,
                            Arc::clone(&writer),
                        ));
                    }
                    if write_reply(&writer, &response).await.is_err() {
                        return;
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
                        hatchery_protocol::Id::Number(0),
                        hatchery_protocol::ErrorObject::new(
                            hatchery_protocol::ErrorCode::ParseError,
                            error.to_string(),
                        ),
                    );
                    if write_reply(&writer, &reply).await.is_err() {
                        return;
                    }
                }
            }
        }
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

/// Fans hub events out over one connection until it dies or the session unloads.
///
/// A slow reader lags: `RecvError::Lagged` means events were lost in the buffer, and the honest
/// move is to stop streaming rather than serve a torn view — the frontend rebuilds through
/// `session/load` (docs/design/daemon.md §4).
async fn forward_events<W>(
    manager: Arc<SessionManager>,
    hub: Arc<LiveHub>,
    session: hatchery_protocol::SessionId,
    mut receiver: tokio::sync::broadcast::Receiver<SessionEvent>,
    writer: std::sync::Arc<tokio::sync::Mutex<BufWriter<W>>>,
) where
    W: AsyncWrite + Unpin + Send + 'static,
{
    loop {
        match receiver.recv().await {
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
                    || writer.write_all(b"\n").await.is_err()
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
    let line = hatchery_protocol::encode_frame(reply)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut writer = writer.lock().await;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
}
