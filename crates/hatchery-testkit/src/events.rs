//! Reading the facts a test asserts on out of a recorded turn.
//!
//! [`RecordingSink`](crate::RecordingSink) hands back the events; these functions answer the
//! questions a kernel-level test asks of them — how the turn ended, what it committed, which
//! states it moved through. They live here rather than in one crate's test file because the
//! daemon's tests (M1) assert the same things about the same events, and a second copy of
//! [`completion`] would be a second place to forget the check it makes.

use hatchery_kernel::{KernelError, KernelEvent, TurnCompletion};
use hatchery_protocol::{Item, ItemKind, ItemKindTag, StopReason};

/// How the turn ended.
///
/// # Panics
///
/// When the events do not hold exactly one `TurnEnded`. One terminal event per turn is part of
/// the kernel's contract, so it is checked here rather than trusted: picking the first match
/// would quietly accept a duplicate and let a turn that ended twice pass for one that ended once.
#[must_use]
pub fn completion(events: &[KernelEvent]) -> TurnCompletion {
    let ended: Vec<&TurnCompletion> = events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::TurnEnded { completion, .. } => Some(completion),
            _ => None,
        })
        .collect();
    assert_eq!(
        ended.len(),
        1,
        "a turn must end with exactly one TurnEnded event, got {}",
        ended.len()
    );
    ended.into_iter().next().expect("one event").clone()
}

/// Why the turn stopped, or `None` when it failed instead.
///
/// # Panics
///
/// See [`completion`].
#[must_use]
pub fn reason(events: &[KernelEvent]) -> Option<StopReason> {
    completion(events).stop_reason()
}

/// The error the turn failed with, or `None` when it completed.
///
/// # Panics
///
/// See [`completion`].
#[must_use]
pub fn error(events: &[KernelEvent]) -> Option<KernelError> {
    match completion(events) {
        TurnCompletion::Failed { error, .. } => Some(error),
        TurnCompletion::Completed { .. } => None,
    }
}

/// Every item the turn committed, in the order it finished them.
#[must_use]
pub fn finished_items(events: &[KernelEvent]) -> Vec<Item> {
    events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::ItemFinished { item } => Some(item.clone()),
            _ => None,
        })
        .collect()
}

/// The kinds of the committed items, in order — the shape of the turn's transcript.
#[must_use]
pub fn kinds(events: &[KernelEvent]) -> Vec<ItemKindTag> {
    finished_items(events).iter().map(Item::kind_tag).collect()
}

/// The states the machine moved through, in order.
///
/// Only the destination of each transition: a `StateChanged` carries where it came from too, and
/// a test that asserted both would be restating the machine's own definition.
#[must_use]
pub fn states(events: &[KernelEvent]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::StateChanged { to, .. } => Some(to.name()),
            _ => None,
        })
        .collect()
}

/// The text of every tool result the turn recorded, in order.
#[must_use]
pub fn tool_result_texts(events: &[KernelEvent]) -> Vec<String> {
    finished_items(events)
        .into_iter()
        .filter_map(|item| match item.kind {
            ItemKind::ToolResult(result) => Some(result.output.text),
            _ => None,
        })
        .collect()
}
