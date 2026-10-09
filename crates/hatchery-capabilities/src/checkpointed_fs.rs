//! The write path's undo point: a decorator over any [`FsBackend`], and the sink that carries what
//! it collected out of the tool call (D13).
//!
//! The checkpoint is a **decorator** rather than a field on [`crate::LocalFs`], and that is a
//! deliberate deviation from the design sketch ("`LocalFs` 写前打检查点"):
//!
//! * the local backend stays a filesystem — it does not have to know that a git repository exists,
//!   which is the same reason it does not know what a tool is;
//! * the collector can be per **tool call** instead of per session, so two calls can never mix
//!   their checkpoints even if the kernel ever runs them concurrently;
//! * any backend can be checkpointed, including the testkit's in-memory one, which is what makes
//!   "the write sequence and the checkpoint records line up" testable without a disk.
//!
//! From outside the seam the behaviour is exactly what the sketch asked for: a write through
//! `LocalFs` is preceded by a shadow-git snapshot.

use async_trait::async_trait;
use hatchery_kernel::CheckpointCollector;
use hatchery_protocol::Checkpoint;

use crate::checkpoint::CheckpointError;
use crate::fs::{FsBackend, FsEntry, FsError, FsMetadata};

/// What the pre-write snapshot produced.
///
/// Two successes, because "no checkpoint" is sometimes a decision rather than a failure — see
/// [`PreWrite::Skipped`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreWrite {
    /// The undo point that now exists. Carried out of the call in a [`CheckpointSink`].
    Taken(Checkpoint),
    /// No undo point, on purpose, and the write should still happen.
    ///
    /// This is D9's fallback: the budget was still exceeded after garbage collection, so the
    /// checkpoint is dropped rather than the agent's work. Refusing the write would turn our own
    /// policy number into an outage, and the large-file rule already set the precedent of excluding
    /// what is too expensive and *reporting* it. The reason exists to be logged and, eventually,
    /// surfaced — a silent un-undoable write is the one outcome this must not be.
    Skipped {
        /// Why, in one sentence a log can carry.
        reason: String,
    },
}

/// Takes the undo point that must exist before a write.
///
/// A trait rather than [`crate::CheckpointStore`] itself because deciding *whether* to snapshot is
/// policy that needs the `checkpoints` table, and that table lives in a sibling layer the
/// capabilities crate must not depend on. The daemon implements this: budget first, then the
/// shadow repository.
#[async_trait]
pub trait Checkpointer: Send + Sync {
    /// Records the state the workspace is about to leave behind.
    ///
    /// # Errors
    ///
    /// [`CheckpointError`] only when the undo point could not be taken *and* that is not a decision
    /// somebody made — a broken shadow repository, a full disk. The caller refuses the write in that
    /// case: an agent write that cannot be undone is worse than no write.
    async fn pre_write(&self) -> Result<PreWrite, CheckpointError>;
}

/// A filesystem that snapshots before it writes.
///
/// Reads pass straight through: a checkpoint records the state a write is about to destroy, and a
/// read destroys nothing.
pub struct CheckpointedFs<'a> {
    inner: &'a dyn FsBackend,
    checkpointer: &'a dyn Checkpointer,
    collector: &'a CheckpointCollector,
}

impl<'a> CheckpointedFs<'a> {
    /// Wraps `inner` so that every write through it is preceded by a checkpoint recorded in
    /// `collector`.
    ///
    /// The collector is the kernel's, lent for the duration of one call — see
    /// [`CheckpointCollector`] for why it is shared rather than returned.
    #[must_use]
    pub const fn new(
        inner: &'a dyn FsBackend,
        checkpointer: &'a dyn Checkpointer,
        collector: &'a CheckpointCollector,
    ) -> Self {
        Self {
            checkpointer,
            collector,
            inner,
        }
    }
}

#[async_trait]
impl FsBackend for CheckpointedFs<'_> {
    async fn read_text_file(&self, path: &str) -> Result<String, FsError> {
        self.inner.read_text_file(path).await
    }

    async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, FsError> {
        self.inner.read_dir(path).await
    }

    async fn metadata(&self, path: &str) -> Result<FsMetadata, FsError> {
        self.inner.metadata(path).await
    }

    async fn write_text_file(&self, path: &str, contents: &str) -> Result<(), FsError> {
        match self.checkpointer.pre_write().await {
            Ok(PreWrite::Taken(checkpoint)) => self.collector.push(checkpoint),
            Ok(PreWrite::Skipped { reason }) => {
                tracing::warn!(
                    path,
                    reason = %reason,
                    "writing without a checkpoint; this write cannot be rewound"
                );
            }
            // The one failure that stops the write. It is not reported as `Io`: nothing is wrong
            // with the path or the disk, the file is untouched, and the model deserves to hear that
            // the reason is a missing undo point rather than a permission or a typo.
            Err(error) => return Err(FsError::Checkpoint(error.to_string())),
        }
        // Written even when the write then fails: `create_dir_all` may already have run, so the
        // checkpoint is the accurate record of what the workspace looked like beforehand, and an
        // undo point that turns out to be unnecessary costs one commit.
        self.inner.write_text_file(path, contents).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::CheckpointError;
    use hatchery_protocol::CheckpointKind;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, PoisonError};

    /// Counts pre-write calls and hands back a predictable commit id.
    struct Recording {
        calls: AtomicUsize,
        outcome: Outcome,
    }

    #[derive(Clone)]
    enum Outcome {
        Taken,
        Skipped,
        Broken,
    }

    impl Recording {
        fn new(outcome: Outcome) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                outcome,
            }
        }
    }

    #[async_trait]
    impl Checkpointer for Recording {
        async fn pre_write(&self) -> Result<PreWrite, CheckpointError> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            match self.outcome {
                Outcome::Taken => Ok(PreWrite::Taken(Checkpoint {
                    commit_id: format!("commit-{call}"),
                    kind: CheckpointKind::PreWrite,
                })),
                Outcome::Skipped => Ok(PreWrite::Skipped {
                    reason: "over budget".to_owned(),
                }),
                Outcome::Broken => Err(CheckpointError::Io("the disk said no".to_owned())),
            }
        }
    }

    /// Records what was written, and nothing else.
    #[derive(Default)]
    struct Writing {
        written: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl FsBackend for Writing {
        async fn read_text_file(&self, path: &str) -> Result<String, FsError> {
            Err(FsError::NotFound(path.to_owned()))
        }
        async fn read_dir(&self, _path: &str) -> Result<Vec<FsEntry>, FsError> {
            Ok(Vec::new())
        }
        async fn metadata(&self, path: &str) -> Result<FsMetadata, FsError> {
            Err(FsError::NotFound(path.to_owned()))
        }
        async fn write_text_file(&self, path: &str, _contents: &str) -> Result<(), FsError> {
            self.written
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(path.to_owned());
            Ok(())
        }
    }

    fn written(fs: &Writing) -> Vec<String> {
        fs.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The ordering is the whole point of a *pre*-write checkpoint: one checkpoint per write, taken
    /// before it, in the order the writes happened.
    #[tokio::test]
    async fn every_write_is_preceded_by_exactly_one_checkpoint() {
        let inner = Writing::default();
        let recorder = Recording::new(Outcome::Taken);
        let collector = CheckpointCollector::new();
        let fs = CheckpointedFs::new(&inner, &recorder, &collector);

        fs.write_text_file("a.txt", "one").await.expect("writes");
        fs.write_text_file("b.txt", "two").await.expect("writes");
        // A read is not a write and must not cost a checkpoint.
        let _ = fs.read_text_file("a.txt").await;

        assert_eq!(recorder.calls.load(Ordering::Relaxed), 2);
        assert_eq!(written(&inner), ["a.txt", "b.txt"]);
        let collected = collector.drain();
        assert_eq!(
            collected
                .iter()
                .map(|c| c.commit_id.as_str())
                .collect::<Vec<_>>(),
            ["commit-0", "commit-1"],
            "checkpoints keep the write order"
        );
        assert!(collected.iter().all(|c| c.kind == CheckpointKind::PreWrite));
        assert!(collector.is_empty(), "draining empties the collector");
    }

    /// D9: an over-budget snapshot is skipped, the write still happens, and nothing is recorded —
    /// so the un-undoable write is visible as a *gap* rather than as a fabricated checkpoint.
    #[tokio::test]
    async fn a_skipped_checkpoint_still_writes_and_records_nothing() {
        let inner = Writing::default();
        let recorder = Recording::new(Outcome::Skipped);
        let collector = CheckpointCollector::new();
        let fs = CheckpointedFs::new(&inner, &recorder, &collector);

        fs.write_text_file("a.txt", "one")
            .await
            .expect("writes anyway");

        assert_eq!(written(&inner), ["a.txt"]);
        assert_eq!(recorder.calls.load(Ordering::Relaxed), 1);
        assert!(
            collector.is_empty(),
            "a skipped snapshot records no checkpoint"
        );
    }

    /// The fail-closed half of D9: a broken undo point refuses the write, and says it is about the
    /// checkpoint rather than pretending to be an io error.
    #[tokio::test]
    async fn a_broken_checkpointer_refuses_the_write() {
        let inner = Writing::default();
        let recorder = Recording::new(Outcome::Broken);
        let collector = CheckpointCollector::new();
        let fs = CheckpointedFs::new(&inner, &recorder, &collector);

        let error = fs
            .write_text_file("a.txt", "one")
            .await
            .expect_err("no checkpoint, no write");
        assert!(
            matches!(&error, FsError::Checkpoint(message) if message.contains("the disk said no")),
            "{error}"
        );
        assert!(written(&inner).is_empty(), "the file must not be written");
        assert!(collector.is_empty());
    }

    /// The property this crate's whole arrangement rests on: the registry hands the tool host a
    /// *clone*, and the kernel drains the original. If a clone had its own buffer, every checkpoint
    /// a write took would be silently discarded.
    #[tokio::test]
    async fn a_clone_of_the_collector_reaches_the_original() {
        let inner = Writing::default();
        let recorder = Recording::new(Outcome::Taken);
        let collector = CheckpointCollector::new();
        let lent = collector.clone();
        let fs = CheckpointedFs::new(&inner, &recorder, &lent);

        fs.write_text_file("a.txt", "one").await.expect("writes");

        assert_eq!(
            collector
                .drain()
                .iter()
                .map(|c| c.commit_id.clone())
                .collect::<Vec<_>>(),
            ["commit-0"]
        );
    }
}
