//! The live hub: fan-out of session events to every attached frontend.
//!
//! Per session, one `tokio::sync::broadcast` channel. M1 ships without coalescing on purpose
//! (roadmap): deltas go out one by one, and the measurements this produces decide the M2
//! strategy — `ServerEvent::is_coalescable` already marks what would be merged.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::broadcast;

use hatchery_protocol::{ServerEvent, SessionEvent, SessionId};

/// How many events a slow subscriber may miss before it is told so.
///
/// Deep enough that a burst of deltas never spills in normal use; a subscriber that falls this
/// far behind gets `RecvError::Lagged` and rebuilds its view through `session/load` — the hub
/// never blocks other frontends on one slow reader (docs/design/daemon.md §4).
const CHANNEL_CAPACITY: usize = 4096;

/// Event fan-out for every live session.
#[derive(Default)]
pub struct LiveHub {
    channels: Mutex<HashMap<SessionId, broadcast::Sender<SessionEvent>>>,
    /// The newest generation each session has published, so an event from a superseded runtime
    /// (an in-flight publish racing an unload, say) cannot reach a frontend that has already
    /// moved on. The client filters too; this is the server-side half of the same invariant.
    generations: Mutex<HashMap<SessionId, u64>>,
}

impl LiveHub {
    /// An empty hub.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribes to a session's stream, creating it if this is the first subscriber.
    pub fn subscribe(&self, session: SessionId) -> broadcast::Receiver<SessionEvent> {
        let mut channels = self.channels.lock().expect("hub is not poisoned");
        channels
            .entry(session)
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .subscribe()
    }

    /// Publishes one event to the session's subscribers.
    ///
    /// No subscribers is normal (a session runs detached from frontends), not an error. Events
    /// from below the session's newest generation are dropped here: two runtimes for one session
    /// must never interleave on the wire (invariant 1), and an unload can race a publish that is
    /// already past its await point.
    pub fn publish(&self, event: SessionEvent) {
        let mut generations = self.generations.lock().expect("hub is not poisoned");
        let known = generations.get(&event.session).copied().unwrap_or(0);
        if event.generation < known {
            tracing::debug!(
                session = %event.session,
                stale = event.generation,
                newest = known,
                "dropped an event from a superseded runtime"
            );
            return;
        }
        generations.insert(event.session, event.generation);
        drop(generations);
        let channels = self.channels.lock().expect("hub is not poisoned");
        if let Some(sender) = channels.get(&event.session) {
            let _ = sender.send(event);
        }
    }

    /// Drops a session's channel once no runtime holds it.
    pub fn remove(&self, session: &SessionId) {
        self.channels
            .lock()
            .expect("hub is not poisoned")
            .remove(session);
        self.generations
            .lock()
            .expect("hub is not poisoned")
            .remove(session);
    }

    /// How many sessions have channels.
    #[must_use]
    pub fn len(&self) -> usize {
        self.channels.lock().expect("hub is not poisoned").len()
    }

    /// True when nothing is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The event, wrapped for its session and generation — what the sink builds.
    #[must_use]
    pub fn envelope(session: SessionId, generation: u64, event: ServerEvent) -> SessionEvent {
        SessionEvent::new(session, generation, event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_protocol::SessionId;

    fn tick(session: SessionId, generation: u64) -> SessionEvent {
        SessionEvent::new(session, generation, ServerEvent::GenerationBumped)
    }

    #[tokio::test]
    async fn every_subscriber_sees_every_event_in_order() {
        let hub = LiveHub::new();
        let session = SessionId::new();
        let mut first = hub.subscribe(session);
        let mut second = hub.subscribe(session);

        for generation in 1..=3_u64 {
            hub.publish(tick(session, generation));
        }
        for receiver in [&mut first, &mut second] {
            for expected in 1..=3_u64 {
                let event = receiver.try_recv().expect("buffered");
                assert_eq!(event.generation, expected);
            }
        }
    }

    #[tokio::test]
    async fn a_session_without_subscribers_publishes_into_the_void() {
        let hub = LiveHub::new();
        hub.publish(tick(SessionId::new(), 1));
        assert!(hub.is_empty(), "no channel is created for the event");
    }

    #[tokio::test]
    async fn a_session_with_no_subscribers_has_no_backlog_to_replay() {
        // tokio broadcast buffers only while receivers exist, and M1 ships without a replay
        // window (docs/worklog/daemon.md): a reconnecting frontend rebuilds through
        // `session/load`, so this is the documented shape, not a gap.
        let hub = LiveHub::new();
        let session = SessionId::new();
        hub.publish(tick(session, 1));
        let mut late = hub.subscribe(session);
        assert!(
            late.try_recv().is_err(),
            "nothing before the subscription is replayed"
        );
        hub.publish(tick(session, 2));
        assert_eq!(late.try_recv().expect("live").generation, 2);
    }

    #[tokio::test]
    async fn removing_the_session_tears_down_its_channel() {
        let hub = LiveHub::new();
        let session = SessionId::new();
        drop(hub.subscribe(session));
        hub.remove(&session);
        assert_eq!(hub.len(), 0);
    }

    #[tokio::test]
    async fn invariant_events_below_the_session_generation_are_dropped() {
        // Invariant 1's server half: after a reassembly bumps the generation, a publish that was
        // already in flight from the old runtime must not interleave into the new stream.
        let hub = LiveHub::new();
        let session = SessionId::new();
        let mut subscriber = hub.subscribe(session);

        hub.publish(tick(session, 3));
        hub.publish(tick(session, 2));
        hub.publish(tick(session, 3));
        hub.publish(tick(session, 4));

        let seen: Vec<u64> = (0..3)
            .filter_map(|_| subscriber.try_recv().ok())
            .map(|event| event.generation)
            .collect();
        assert_eq!(seen, vec![3, 3, 4], "generation 2 never arrives");
    }

    #[tokio::test]
    async fn a_newer_generation_reopens_the_stream_after_a_remove() {
        // Unload clears the generation memory with the channel, so the next assembly's events
        // are not measured against a runtime that no longer exists.
        let hub = LiveHub::new();
        let session = SessionId::new();
        hub.publish(tick(session, 5));
        hub.remove(&session);
        hub.publish(tick(session, 1));
        let mut subscriber = hub.subscribe(session);
        hub.publish(tick(session, 2));
        assert_eq!(subscriber.try_recv().expect("live").generation, 2);
    }
}
