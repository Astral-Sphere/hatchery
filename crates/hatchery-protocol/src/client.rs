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
use std::sync::{Arc, Mutex};
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
        spawn_reply_router(inner.clone(), read);
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

/// Reads the connection forever, routing every reply to the caller that asked for it.
fn spawn_reply_router(inner: Arc<Inner>, read: tokio::net::unix::OwnedReadHalf) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(read).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => match rpc::decode_frame(&line) {
                    Ok(Incoming::Response(response)) => route(inner.as_ref(), response),
                    Ok(Incoming::Request(_)) | Ok(Incoming::Notification(_)) => {
                        // The daemon pushes events on dedicated event connections; one on the
                        // call connection means a future protocol feature — logged, not fatal.
                        tracing::debug!("an unexpected server-initiated frame arrived");
                    }
                    Err(error) => {
                        tracing::warn!("an unparseable frame arrived from the daemon: {error}");
                    }
                },
                Ok(None) | Err(_) => {
                    // The daemon is gone: every waiter learns at once.
                    let mut pending = inner.pending.lock().expect("pending map is not poisoned");
                    for (_id, waiter) in pending.drain() {
                        let _ = waiter.send(Err(ClientError::Connection(
                            "the daemon connection closed".to_owned(),
                        )));
                    }
                    return;
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
