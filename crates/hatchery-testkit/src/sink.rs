//! An event sink that remembers everything and lets tests wait for what they expect.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};

use hatchery_kernel::{EventSink, KernelEvent};

/// How long a test waits for an event before failing.
///
/// A bounded wait, not an unbounded one: a kernel that stops emitting would otherwise hang the
/// test run, and a timeout with the events seen so far is a far better failure than a hang.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Records every event the kernel emits.
///
/// The recording is the assertion surface: [`RecordingSink::names`] is compared against the
/// exact event sequence a path must emit (presence, order and count in one assertion), and
/// [`RecordingSink::wait_for`] lets a test react at a precise moment mid-turn.
pub struct RecordingSink {
    events: Mutex<Vec<KernelEvent>>,
    /// Bumped on every event. A `watch` channel rather than a `Notify`: a notification that fires
    /// between a test's check and its wait would be lost, while a changed version is not.
    revision: watch::Sender<u64>,
    subscribers: Mutex<Vec<mpsc::UnboundedSender<KernelEvent>>>,
}

impl RecordingSink {
    /// A sink that has seen nothing yet.
    #[must_use]
    pub fn new() -> Self {
        let (revision, _receiver) = watch::channel(0);
        Self {
            events: Mutex::new(Vec::new()),
            revision,
            subscribers: Mutex::new(Vec::new()),
        }
    }

    /// Everything emitted so far, in order.
    #[must_use]
    pub fn events(&self) -> Vec<KernelEvent> {
        self.events
            .lock()
            .expect("the mutex is never poisoned")
            .clone()
    }

    /// The event names, in order — the shortest way to assert a sequence.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.events
            .lock()
            .expect("the mutex is never poisoned")
            .iter()
            .map(KernelEvent::name)
            .collect()
    }

    /// A live stream of events, for tests that must react while the turn runs (approvals).
    #[must_use]
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<KernelEvent> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.subscribers
            .lock()
            .expect("the mutex is never poisoned")
            .push(sender);
        receiver
    }

    /// Waits until an event matching `predicate` has been emitted.
    ///
    /// # Panics
    ///
    /// When nothing matches within [`WAIT_TIMEOUT`], listing what did arrive.
    pub async fn wait_for(
        &self,
        what: &str,
        predicate: impl Fn(&KernelEvent) -> bool,
    ) -> Vec<KernelEvent> {
        self.wait_for_from(what, 0, predicate).await
    }

    /// Waits until an event matching `predicate` has been emitted at or after `from`.
    ///
    /// The cursor is what makes a multi-turn test possible: [`RecordingSink::wait_for`] scans
    /// from the beginning, so waiting for "the turn to end" a second time would match the first
    /// turn's `TurnEnded` again and return a stale snapshot before the second turn ran.
    ///
    /// # Panics
    ///
    /// When nothing matches within [`WAIT_TIMEOUT`], listing what did arrive.
    pub async fn wait_for_from(
        &self,
        what: &str,
        from: usize,
        predicate: impl Fn(&KernelEvent) -> bool,
    ) -> Vec<KernelEvent> {
        let mut watcher = self.revision.subscribe();
        let satisfied = async {
            loop {
                {
                    let events = self.events.lock().expect("the mutex is never poisoned");
                    if events.iter().skip(from).any(&predicate) {
                        return events.clone();
                    }
                }
                if watcher.changed().await.is_err() {
                    // The sink is gone, which cannot happen while the test holds it.
                    std::future::pending::<()>().await;
                }
            }
        };

        match tokio::time::timeout(WAIT_TIMEOUT, satisfied).await {
            Ok(events) => events,
            Err(_) => panic!(
                "timed out waiting for {what}; events seen: {:?}",
                self.names()
            ),
        }
    }

    /// Waits for the turn to end, however it ends.
    ///
    /// # Panics
    ///
    /// See [`RecordingSink::wait_for`].
    pub async fn wait_for_end(&self) -> Vec<KernelEvent> {
        self.wait_for("the turn to end", KernelEvent::ends_turn)
            .await
    }

    /// Waits for the turn that ends at or after `from` — the second turn's end in a multi-turn
    /// test, where [`RecordingSink::wait_for_end`] would match the first turn's again.
    ///
    /// # Panics
    ///
    /// See [`RecordingSink::wait_for`].
    pub async fn wait_for_end_from(&self, from: usize) -> Vec<KernelEvent> {
        self.wait_for_from("the turn to end", from, KernelEvent::ends_turn)
            .await
    }
}

impl Default for RecordingSink {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn emit(&self, event: KernelEvent) {
        self.events
            .lock()
            .expect("the mutex is never poisoned")
            .push(event.clone());
        self.subscribers
            .lock()
            .expect("the mutex is never poisoned")
            .retain(|subscriber| subscriber.send(event.clone()).is_ok());
        self.revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}
