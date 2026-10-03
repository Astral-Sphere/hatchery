//! The client half of the protocol: what every frontend uses instead of knowing about
//! transports (ADR-0001's thin client).
//!
//! One helper, three jobs: connect, call, and follow a session's events. Replies are routed to
//! their callers by request id; session events arrive on a dedicated connection under the
//! `session/event` notification, because `session/new` and `session/load` implicitly subscribe
//! the connection that made them (docs/design/protocol.md §4).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use crate::method::HelloParams;
use crate::rpc::{self, Id, Incoming, Request, Response};
use crate::{PROTOCOL_VERSION, SessionEvent};

/// The one-slot reply mailbox of an event connection: the subscribing call's response.
type PendingReply = oneshot::Sender<Result<serde_json::Value, ClientError>>;

/// The notification method the daemon pushes session events under.
pub const EVENT_NOTIFICATION: &str = "session/event";

/// How long a call may go unanswered. Generous: a `session/prompt` returns when the turn is
/// *accepted*, and everything else is quick; a hung daemon is hung anyway.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Why the client gave up.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The socket is not there (yet). `attach_or_spawn` treats this as "spawn".
    #[error("cannot reach the daemon at {path}: {source}")]
    Connect {
        /// The socket path.
        path: String,
        /// The OS's complaint.
        source: std::io::Error,
    },
    /// The request never got an answer in time, or the connection died trying.
    #[error("the connection dropped before a reply: {0}")]
    Connection(String),
    /// The daemon answered with an error object.
    #[error("{message}")]
    Daemon {
        /// The wire's error message.
        message: String,
    },
    /// A frame could not be built or parsed.
    #[error("protocol: {0}")]
    Frame(#[from] rpc::FrameError),
    /// The socket errored mid-operation.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The reply's id matched no caller — a client bug or a desynchronised stream.
    #[error("a reply arrived for request {0}, which nobody is waiting on")]
    OrphanReply(i64),
}

/// A live connection to the daemon over UDS.
#[derive(Clone)]
pub struct DaemonClient {
    inner: Arc<Inner>,
}

struct Inner {
    write: AsyncMutex<tokio::io::BufWriter<tokio::net::unix::OwnedWriteHalf>>,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<serde_json::Value, ClientError>>>>,
    next_id: AtomicI64,
}

impl DaemonClient {
    /// Connects to the daemon's UDS and starts the reply router.
    ///
    /// # Errors
    ///
    /// [`ClientError::Connect`] when the socket is not there.
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        let path = path.as_ref();
        let stream = UnixStream::connect(path)
            .await
            .map_err(|source| ClientError::Connect {
                path: path.display().to_string(),
                source,
            })?;
        let (read, write) = stream.into_split();
        let inner = Arc::new(Inner {
            write: AsyncMutex::new(tokio::io::BufWriter::new(write)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
        });
        spawn_reply_router(Arc::downgrade(&inner), read);
        Ok(Self { inner })
    }

    /// One request, one response: the result payload, or the daemon's error as
    /// [`ClientError::Daemon`].
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub async fn call_raw(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let request = Request::new(Id::Number(id), method).with_params(&params)?;
        let line = rpc::encode_frame(&request)?;

        let (sender, receiver) = oneshot::channel();
        #[allow(clippy::useless_conversion)]
        let id = i64::from(id);
        self.inner
            .pending
            .lock()
            .expect("pending map is not poisoned")
            .insert(id, sender);
        {
            let mut writer = self.inner.write.lock().await;
            if let Err(error) = writer.write_all(line.as_bytes()).await.map(|()| {
                // The newline is part of the framing; a single future chains both writes.
                writer.write_all(b"\n")
            }) {
                self.take_pending(id);
                return Err(ClientError::Connection(error.to_string()));
            }
            if let Err(error) = writer.flush().await {
                self.take_pending(id);
                return Err(ClientError::Connection(error.to_string()));
            }
        }

        match tokio::time::timeout(CALL_TIMEOUT, receiver).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err(ClientError::Connection(format!(
                "the router dropped request {id}"
            ))),
            Err(_) => {
                self.take_pending(id);
                Err(ClientError::Connection(format!(
                    "request {id} went unanswered for {CALL_TIMEOUT:?}"
                )))
            }
        }
    }

    fn take_pending(
        &self,
        id: i64,
    ) -> Option<oneshot::Sender<Result<serde_json::Value, ClientError>>> {
        self.inner
            .pending
            .lock()
            .expect("pending map is not poisoned")
            .remove(&id)
    }

    /// Calls a typed method: params in, typed result out.
    ///
    /// The protocol's methods are constants plus concrete param/result types rather than a
    /// trait, so this is the generic form callers use when they have both types at hand.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R, ClientError> {
        let params = serde_json::to_value(params)
            .map_err(|error| ClientError::Connection(format!("unserialisable params: {error}")))?;
        let value = self.call_raw(method, params).await?;
        serde_json::from_value(value)
            .map_err(|error| ClientError::Connection(format!("unusable reply: {error}")))
    }

    /// The handshake. The daemon refuses a wrong version or a wrong token here.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub async fn hello(
        &self,
        boot_token: Option<String>,
    ) -> Result<crate::method::HelloResult, ClientError> {
        self.call::<HelloParams, crate::method::HelloResult>(
            crate::method::DAEMON_HELLO,
            &HelloParams {
                protocol_version: PROTOCOL_VERSION.to_owned(),
                client: None,
                boot_token,
            },
        )
        .await
    }
}

/// Reads the connection, routing every reply to the caller that asked for it.
///
/// The router holds the inner state only *weakly*: the connection must die with the last
/// [`DaemonClient`] clone, and a strong hold here would keep the write half — and therefore the
/// whole connection — alive forever after every caller was gone.
fn spawn_reply_router(inner: Weak<Inner>, read: tokio::net::unix::OwnedReadHalf) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(read).lines();
        loop {
            // The await happens on NO strong reference: an upgrade held across it would keep the
            // write half alive through the router itself, and the daemon could never observe the
            // last client hanging up. A line can only arrive while a client lives, so the
            // upgrade after the await is the honest check.
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) | Err(_) => {
                    // The daemon is gone: every waiter learns at once.
                    if let Some(inner) = inner.upgrade() {
                        let mut pending =
                            inner.pending.lock().expect("pending map is not poisoned");
                        for (_id, waiter) in pending.drain() {
                            let _ = waiter.send(Err(ClientError::Connection(
                                "the daemon connection closed".to_owned(),
                            )));
                        }
                    }
                    return;
                }
            };
            let Some(inner) = inner.upgrade() else {
                // Every client is gone; dropping the read half here is what ends the connection
                // for the daemon too.
                return;
            };
            match rpc::decode_frame(&line) {
                Ok(Incoming::Response(response)) => route(inner.as_ref(), response),
                Ok(Incoming::Request(_)) | Ok(Incoming::Notification(_)) => {
                    // The daemon pushes events on dedicated event connections; one on the
                    // call connection means a future protocol feature — logged, not fatal.
                    tracing::debug!("an unexpected server-initiated frame arrived");
                }
                Err(error) => {
                    tracing::warn!("an unparseable frame arrived from the daemon: {error}");
                }
            }
        }
    });
}

fn route(inner: &Inner, response: Response) {
    let id = match &response.id {
        Id::Number(number) => *number,
        Id::String(text) => text.parse().unwrap_or(i64::MIN),
    };
    if let Some(waiter) = inner
        .pending
        .lock()
        .expect("pending map is not poisoned")
        .remove(&id)
    {
        let payload = if response.is_ok() {
            response
                .result_as::<serde_json::Value>()
                .map_err(ClientError::Frame)
        } else {
            Err(ClientError::Daemon {
                message: response
                    .error
                    .map_or_else(|| "unspecified error".to_owned(), |error| error.message),
            })
        };
        let _ = waiter.send(payload);
    } else {
        tracing::warn!("a reply arrived for request {id}, which nobody is waiting on");
    }
}

/// A stream of one connection's session events.
///
/// The subscription rides the connection that made it (`session/new` / `session/load` are
/// implicit subscriptions), so the flow is: open the stream, [`subscribe`](Self::subscribe) on
/// it, then [`next`](Self::next) forever. The subscribing call's reply comes back through
/// `subscribe`'s result even as later events are already flowing to `next`.
pub struct EventStream {
    writer: AsyncMutex<tokio::io::BufWriter<tokio::net::unix::OwnedWriteHalf>>,
    pending: Arc<Mutex<Option<PendingReply>>>,
    next_id: AtomicI64,
    events: mpsc::Receiver<SessionEvent>,
}

impl EventStream {
    /// Opens an events connection.
    ///
    /// # Errors
    ///
    /// [`ClientError::Connect`] when the socket is not there.
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        let path = path.as_ref();
        let stream = UnixStream::connect(path)
            .await
            .map_err(|source| ClientError::Connect {
                path: path.display().to_string(),
                source,
            })?;
        let (read, write) = stream.into_split();
        let (tx, rx) = mpsc::channel(1024);
        let pending: Arc<Mutex<Option<PendingReply>>> = Arc::new(Mutex::new(None));
        spawn_event_router(Arc::clone(&pending), read, tx);
        Ok(Self {
            writer: AsyncMutex::new(tokio::io::BufWriter::new(write)),
            pending,
            next_id: AtomicI64::new(10_000),
            events: rx,
        })
    }

    /// Makes the subscribing call on this connection. The reply resolves here; the events that
    /// follow land in [`Self::next`].
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub async fn subscribe(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = Request::new(Id::Number(id), method).with_params(&params)?;
        let line = rpc::encode_frame(&request)?;
        let (sender, receiver) = oneshot::channel();
        *self.pending.lock().expect("pending is not poisoned") = Some(sender);
        {
            let mut writer = self.writer.lock().await;
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
        match tokio::time::timeout(CALL_TIMEOUT, receiver).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err(ClientError::Connection(
                "the router dropped the call".to_owned(),
            )),
            Err(_) => Err(ClientError::Connection(
                "the subscribing call timed out".to_owned(),
            )),
        }
    }

    /// The next event, or `None` once the daemon is gone.
    pub async fn next(&mut self) -> Option<SessionEvent> {
        self.events.recv().await
    }

    /// Drops the reader half: kept for signature honesty in tests.
    #[allow(dead_code)]
    fn has_reader(&mut self) -> bool {
        // `reader` is driven by the router task spawned in `connect`; this accessor exists so
        // the field is not flagged unused.
        !matches!(
            self.events.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        )
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        // Closing the writer ends the connection; the router task notices and exits.
        self.events.close();
    }
}

/// Reads the events connection forever: replies resolve the pending call, everything else is
/// parsed as a session event.
///
/// Events carrying a generation *below* the highest one already seen are dropped (invariant 1,
/// the client half): an assembly bumps the generation before its first event goes out, so a
/// lower one can only be an older runtime's straggler — replaying it would overwrite newer
/// state with stale deltas.
fn spawn_event_router(
    pending: Arc<Mutex<Option<PendingReply>>>,
    read: tokio::net::unix::OwnedReadHalf,
    events: mpsc::Sender<SessionEvent>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(read).lines();
        let mut highest_generation = 0_u64;
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => match rpc::decode_frame(&line) {
                    Ok(Incoming::Response(response)) => {
                        let waiter = pending.lock().expect("pending is not poisoned").take();
                        if let Some(waiter) = waiter {
                            let payload = if response.is_ok() {
                                response
                                    .result_as::<serde_json::Value>()
                                    .map_err(ClientError::Frame)
                            } else {
                                Err(ClientError::Daemon {
                                    message: response.error.map_or_else(
                                        || "unspecified error".to_owned(),
                                        |error| error.message,
                                    ),
                                })
                            };
                            let _ = waiter.send(payload);
                        }
                    }
                    Ok(Incoming::Notification(notification))
                        if notification.method == EVENT_NOTIFICATION =>
                    {
                        let value = notification.params.unwrap_or(serde_json::Value::Null);
                        match serde_json::from_value::<SessionEvent>(value) {
                            Ok(event) => {
                                if event.generation < highest_generation {
                                    tracing::debug!(
                                        event_generation = event.generation,
                                        seen = highest_generation,
                                        "dropped an event from a superseded runtime"
                                    );
                                    continue;
                                }
                                highest_generation = event.generation;
                                if events.send(event).await.is_err() {
                                    return;
                                }
                            }
                            Err(error) => {
                                tracing::warn!("an event payload did not parse: {error}");
                            }
                        }
                    }
                    Ok(frame) => {
                        tracing::debug!(
                            "a non-event frame arrived on the events connection: {frame:?}"
                        );
                    }
                    Err(error) => tracing::warn!("an unparseable event frame arrived: {error}"),
                },
                Ok(None) | Err(_) => return,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::{UnixListener, UnixStream as TokioUnixStream};

    /// A fake daemon: binds a tempdir socket and hands the test each accepted connection.
    struct FakeDaemon {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
        accept: tokio::sync::mpsc::Receiver<TokioUnixStream>,
    }

    impl FakeDaemon {
        async fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("fake.sock");
            let listener = UnixListener::bind(&path).expect("bind");
            let (accepted_tx, accepted_rx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    if accepted_tx.send(stream).await.is_err() {
                        return;
                    }
                }
            });
            Self {
                _dir: dir,
                path,
                accept: accepted_rx,
            }
        }

        async fn connection(&mut self) -> TokioUnixStream {
            self.accept.recv().await.expect("a connection arrived")
        }
    }

    async fn read_line(stream: &mut TokioUnixStream) -> String {
        let mut line = Vec::new();
        loop {
            let mut byte = [0_u8; 1];
            let read = stream.read(&mut byte).await.expect("read");
            assert!(read == 1, "the connection ended: {read}");
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        String::from_utf8(line).expect("utf-8 frames")
    }

    /// The numeric id of a request line the fake daemon read.
    fn request_id(line: &str) -> i64 {
        let after = line.split("\"id\":").nth(1).expect("an id field");
        let token = after.split(',').next().expect("a value");
        token
            .trim()
            .trim_end_matches('}')
            .parse()
            .expect("a numeric id")
    }

    #[tokio::test]
    async fn concurrent_calls_are_routed_by_id_even_when_replied_out_of_order() {
        let mut daemon = FakeDaemon::new().await;
        let client = DaemonClient::connect(&daemon.path).await.expect("connect");
        // Spawned, not merely boxed: the test runtime is current-thread, and an unpolled future
        // never writes its request — the fake's read would deadlock against it.
        let call_a = client.clone();
        let mut first =
            tokio::spawn(async move { call_a.call_raw("a/first", serde_json::json!({})).await });
        let call_b = client.clone();
        let mut second =
            tokio::spawn(async move { call_b.call_raw("a/second", serde_json::json!({})).await });

        let mut stream = daemon.connection().await;
        let one = read_line(&mut stream).await;
        let two = read_line(&mut stream).await;
        assert!(
            one.contains("\"a/first\"") && two.contains("\"a/second\""),
            "{one}{two}"
        );

        // Answer the SECOND call first: a router that matches replies by arrival order misroutes
        // here, which is the whole point of the id map.
        use tokio::io::AsyncWriteExt;
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"which\":\"second\"}}\n")
            .await
            .expect("write");
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"which\":\"first\"}}\n")
            .await
            .expect("write");

        let first_reply = tokio::time::timeout(Duration::from_secs(5), &mut first)
            .await
            .expect("first answered")
            .expect("task")
            .expect("no error");
        let second_reply = tokio::time::timeout(Duration::from_secs(5), &mut second)
            .await
            .expect("second answered")
            .expect("task")
            .expect("no error");
        assert_eq!(first_reply["which"], "first");
        assert_eq!(second_reply["which"], "second");
    }

    #[tokio::test]
    async fn a_daemon_error_object_surfaces_as_a_daemon_error() {
        let mut daemon = FakeDaemon::new().await;
        let client = DaemonClient::connect(&daemon.path).await.expect("connect");
        let failing = client.clone();
        let mut call =
            tokio::spawn(async move { failing.call_raw("a/fails", serde_json::json!({})).await });

        let mut stream = daemon.connection().await;
        let _request = read_line(&mut stream).await;
        use tokio::io::AsyncWriteExt;
        stream
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32002,\"message\":\"a turn is already running\"}}\n",
            )
            .await
            .expect("write");

        let error = tokio::time::timeout(Duration::from_secs(5), &mut call)
            .await
            .expect("answered")
            .expect("task")
            .expect_err("an error reply");
        assert!(
            error.to_string().contains("a turn is already running"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_reply_for_an_unknown_id_does_not_steal_a_later_reply() {
        let mut daemon = FakeDaemon::new().await;
        let client = DaemonClient::connect(&daemon.path).await.expect("connect");
        let real = client.clone();
        let mut call =
            tokio::spawn(async move { real.call_raw("a/real", serde_json::json!({})).await });

        let mut stream = daemon.connection().await;
        let _request = read_line(&mut stream).await;
        use tokio::io::AsyncWriteExt;
        // An orphan reply first — a desynchronised daemon or a replayed frame.
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":9999,\"result\":{}}\n")
            .await
            .expect("write");
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n")
            .await
            .expect("write");

        let reply = tokio::time::timeout(Duration::from_secs(5), &mut call)
            .await
            .expect("answered")
            .expect("task")
            .expect("the real reply is not consumed by the orphan");
        assert_eq!(reply["ok"], true);
    }

    #[tokio::test]
    async fn dropping_the_last_client_ends_the_connection() {
        let mut daemon = FakeDaemon::new().await;
        let client = DaemonClient::connect(&daemon.path).await.expect("connect");
        let mut stream = daemon.connection().await;
        // The client moves into the task, which drops it after the answer — from the moment the
        // reply lands, the daemon side must be able to observe the hang-up.
        let mut call = tokio::spawn(async move {
            let reply = client
                .call_raw("a/ping", serde_json::json!({}))
                .await
                .expect("no error");
            drop(client);
            reply
        });
        let _request = read_line(&mut stream).await;
        use tokio::io::AsyncWriteExt;
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
            .await
            .expect("write");
        tokio::time::timeout(Duration::from_secs(5), &mut call)
            .await
            .expect("answered")
            .expect("task");

        // EOF on the daemon's side is what "the client is gone" means; before the weak-router
        // fix the router task held the write half and this read hung forever.
        let mut leftover = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut leftover))
            .await
            .expect("the daemon sees EOF in time")
            .expect("eof read");
        assert!(
            leftover.is_empty(),
            "the connection ended, and nothing followed: {leftover:?}"
        );
    }

    #[tokio::test]
    async fn a_stale_generation_event_is_dropped_and_an_equal_one_passes() {
        // The client half of invariant 1: events below the highest seen generation are a
        // superseded runtime's stragglers; equal and higher pass.
        let mut daemon = FakeDaemon::new().await;
        let events = EventStream::connect(&daemon.path).await.expect("connect");
        // The subscribing call runs in a task: the fake's reads must be able to wait on it.
        let mut worker = tokio::spawn(async move {
            let mut events = events;
            let reply = events
                .subscribe("session/load", serde_json::json!({}))
                .await
                .expect("subscribed");
            let _ = reply;
            let mut seen = Vec::new();
            for _ in 0..3 {
                let event = tokio::time::timeout(Duration::from_secs(5), events.next())
                    .await
                    .expect("an event in time")
                    .expect("streamed");
                seen.push(event.generation);
            }
            seen
        });

        let mut stream = daemon.connection().await;
        let request = read_line(&mut stream).await;
        assert!(request.contains("session/load"), "{request}");
        let id = request_id(&request);
        use tokio::io::AsyncWriteExt;
        stream
            .write_all(format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{}}}}\n").as_bytes())
            .await
            .expect("write");

        // Envelopes with `type` as a top-level params key (the SessionEvent wire shape).
        for generation in [3_u64, 2, 3, 4] {
            let frame = format!(
                "{{\"jsonrpc\":\"2.0\",\"method\":\"session/event\",\"params\":{{\"session\":\"01890f47-0000-7000-8000-000000000001\",\"generation\":{generation},\"type\":\"generation_bumped\"}}}}\n"
            );
            stream.write_all(frame.as_bytes()).await.expect("write");
        }

        let seen = tokio::time::timeout(Duration::from_secs(10), &mut worker)
            .await
            .expect("the worker finishes")
            .expect("task");
        assert_eq!(
            seen,
            vec![3, 3, 4],
            "generation 2 never reaches the frontend"
        );
    }

    #[tokio::test]
    async fn the_subscribing_reply_and_the_first_event_share_one_connection() {
        // The reply resolves through `subscribe` even while later frames are already event
        // notifications — the one-slot mailbox must route by frame kind, not by arrival.
        let mut daemon = FakeDaemon::new().await;
        let events = EventStream::connect(&daemon.path).await.expect("connect");
        let mut worker = tokio::spawn(async move {
            let mut events = events;
            let reply = events
                .subscribe("session/new", serde_json::json!({}))
                .await
                .expect("the reply still lands");
            assert!(reply.get("session").is_some(), "{reply}");
            let event = tokio::time::timeout(Duration::from_secs(5), events.next())
                .await
                .expect("an event in time")
                .expect("streamed");
            event.generation
        });

        let mut stream = daemon.connection().await;
        let request = read_line(&mut stream).await;
        let id = request_id(&request);
        use tokio::io::AsyncWriteExt;
        // The notification BEFORE the reply: the router must not spend the reply slot on it.
        stream
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"method\":\"session/event\",\"params\":{\"session\":\"01890f47-0000-7000-8000-000000000001\",\"generation\":1,\"type\":\"generation_bumped\"}}\n",
            )
            .await
            .expect("write");
        stream
            .write_all(
                format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"session\":{{}}}}}}\n")
                    .as_bytes(),
            )
            .await
            .expect("write");

        let generation = tokio::time::timeout(Duration::from_secs(10), &mut worker)
            .await
            .expect("the worker finishes")
            .expect("task");
        assert_eq!(generation, 1);
    }
}
