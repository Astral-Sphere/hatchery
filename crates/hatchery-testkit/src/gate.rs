//! A permit gate that scripted fakes hold until a test opens it.

use std::sync::Arc;

use tokio::sync::Semaphore;

/// A counting semaphore with a friendly name.
///
/// Timing is the reason this exists: a scripted stream that finishes instantly leaves no window
/// for an interrupt test to act in, and a tool that returns immediately cannot be cancelled. A
/// gated fake blocks until the test releases it.
#[derive(Clone, Debug)]
pub struct Gate {
    permits: Arc<Semaphore>,
}

impl Gate {
    /// A closed gate: nothing proceeds until it is released.
    #[must_use]
    pub fn new() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(0)),
        }
    }

    /// Lets `count` more steps proceed.
    pub fn release(&self, count: usize) {
        self.permits.add_permits(count);
    }

    /// Waits for a permit. Returns `false` when the gate's counterpart is gone.
    ///
    /// The permit is **consumed**, not borrowed: a counted semaphore hands a permit back when the
    /// guard drops, so `release(1)` would otherwise let every later step through instead of one.
    pub async fn acquire(&self) -> bool {
        match self.permits.acquire().await {
            Ok(permit) => {
                permit.forget();
                true
            }
            Err(_) => false,
        }
    }
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}
