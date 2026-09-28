//! A history source backed by a list the test controls.

use std::sync::Mutex;

use async_trait::async_trait;

use hatchery_kernel::{HistorySource, HistoryView, KernelError, Message};
use hatchery_protocol::ItemId;

/// Serves a fixed conversation, and lets the test move the branch head.
///
/// Stands in for the daemon's store-backed source: the kernel only asks for "the conversation and
/// where it ends", and nothing about that needs a database to exercise.
pub struct MemoryHistory {
    view: Mutex<HistoryView>,
}

impl MemoryHistory {
    /// A history with no messages and no head — a brand new session.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            view: Mutex::new(HistoryView::default()),
        }
    }

    /// A history that already contains a conversation.
    #[must_use]
    pub fn with_messages(messages: Vec<Message>) -> Self {
        Self {
            view: Mutex::new(HistoryView {
                head: None,
                messages,
            }),
        }
    }

    /// Sets the branch head the next turn will chain onto.
    #[must_use]
    pub fn with_head(self, head: ItemId) -> Self {
        self.view.lock().expect("the mutex is never poisoned").head = Some(head);
        self
    }

    /// Appends a message, as a previous turn would have.
    pub fn push(&self, message: Message) {
        self.view
            .lock()
            .expect("the mutex is never poisoned")
            .messages
            .push(message);
    }

    /// Appends a message and advances the head, as a committed item would.
    pub fn push_item(&self, message: Message, head: ItemId) {
        let mut view = self.view.lock().expect("the mutex is never poisoned");
        view.messages.push(message);
        view.head = Some(head);
    }

    /// The current view.
    #[must_use]
    pub fn view(&self) -> HistoryView {
        self.view
            .lock()
            .expect("the mutex is never poisoned")
            .clone()
    }
}

impl Default for MemoryHistory {
    fn default() -> Self {
        Self::empty()
    }
}

#[async_trait]
impl HistorySource for MemoryHistory {
    async fn view(&self) -> Result<HistoryView, KernelError> {
        Ok(self.view())
    }
}
