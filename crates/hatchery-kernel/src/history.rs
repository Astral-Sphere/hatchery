//! The history seam: where the conversation the model will see comes from.

use async_trait::async_trait;

use hatchery_protocol::ItemId;

use crate::error::KernelError;
use crate::message::Message;

/// The conversation as of one instant, plus where it ends.
///
/// One struct rather than two calls: asking for the head and then the messages would let the
/// branch move in between, and the kernel must chain its new items onto the head it actually
/// assembled from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistoryView {
    /// The item the active branch currently ends at. `None` for a session with no history yet.
    pub head: Option<ItemId>,
    /// The conversation, oldest first.
    pub messages: Vec<Message>,
}

/// Supplies the conversation for a turn.
///
/// A daemon-side implementation rebuilds this from the store's active branch (that is where
/// compaction, reasoning filtering and token budgeting happen — see `docs/design/kernel.md` §6);
/// tests supply a fixed list.
///
/// One source per session, like [`crate::EventSink`]: the daemon binds both when it assembles the
/// session's runtime, so neither needs a session parameter.
#[async_trait]
pub trait HistorySource: Send + Sync {
    /// The conversation and the branch head, as of now.
    ///
    /// # Errors
    ///
    /// Fails when the history cannot be read; a turn that cannot see its own history must not run
    /// (invariant 2 would be unenforceable).
    async fn view(&self) -> Result<HistoryView, KernelError>;
}
