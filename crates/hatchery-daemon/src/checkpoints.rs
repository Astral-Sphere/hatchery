//! Checkpoint policy: when a snapshot may be taken, and which shadow repositories have no owner.
//!
//! The mechanism lives in `hatchery-capabilities` ([`CheckpointStore`]) and the records live in
//! `hatchery-store`. Those two are siblings at L2 and neither may depend on the other, so the layer
//! that can see both is the one that has to hold the policy (D9, D13, storage.md open question 3).
//!
//! # What the circuit breaker can do, and why
//!
//! ADR-0006 asks for "GC the oldest checkpoints" when a budget is exceeded. Commit-level GC is not
//! available, for two reasons that are properties of the pieces rather than of this code:
//!
//! * libgit2 has no object-level GC — `Repository` exposes `odb()` (read, write, foreach) and
//!   `cleanup_state()` (stale state files), and nothing that deletes an object;
//! * dropping commits from a chain means re-committing the survivors, and **a re-committed commit
//!   has a different id** — which `ItemKind::Checkpoint { commit_id }` already carries inside an
//!   append-only table (`items_no_update` refuses the update that would fix it).
//!
//! So the granularity of reclamation here is a whole shadow repository, and the ladder is:
//!
//! 1. **sweep orphans** — a workspace with no rows left in `checkpoints` has nothing that can
//!    reference its commits, so deleting its repository is safe and reclaims real bytes;
//! 2. **start over** — a workspace over its own budget with more than one snapshot loses them all
//!    and begins again, which keeps *future* writes undoable instead of giving up on them;
//! 3. **skip** — when one snapshot alone exceeds the budget, or the global breaker is still tripped
//!    after sweeping, the write goes ahead un-undoable with a warning. Never refused: the budget is
//!    our own policy number, and turning it into an outage would be worse than the growth it prevents.
//!
//! A genuine git or disk failure is the only thing that refuses a write, and that decision is the
//! seam's ([`hatchery_capabilities::CheckpointedFs`]), not this module's.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hatchery_capabilities::{
    CheckpointError, CheckpointOptions, CheckpointPool, CheckpointStore, Checkpointer, PreWrite,
};
use hatchery_protocol::CheckpointKind;
use hatchery_store::SessionStore;

use crate::config::CheckpointsConfig;

/// The two budgets of ADR-0006, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// One workspace's shadow repository.
    pub workspace_bytes: u64,
    /// Every shadow repository together — the one that protects the user's disk rather than ours.
    pub global_bytes: u64,
}

/// The one spelling a workspace may have in a `checkpoints` row.
///
/// Canonical, because that is what a shadow repository records for itself in its own config and
/// therefore what the orphan sweep asks the store about. Two writers that disagree would make a live
/// workspace look abandoned and the sweep would delete its repository, so this is a function rather
/// than an expression repeated at each call site.
///
/// A path with no canonical form is one that does not exist; recording it as given beats dropping
/// the row, and the sweep's "cannot tell" branch then keeps the repository rather than reclaiming it.
#[must_use]
pub fn recorded_workspace(workspace: &Path) -> PathBuf {
    std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf())
}

/// The daemon's checkpoint facilities: one pool of shadow repositories over one store of records.
pub struct Checkpoints {
    pool: Arc<CheckpointPool>,
    store: Arc<dyn SessionStore>,
    budget: Budget,
}

impl Checkpoints {
    /// Binds the pool to `<data_dir>/checkpoints`.
    ///
    /// Nothing is created here: a shadow repository comes into existence with the first snapshot of
    /// the workspace it belongs to, so a daemon that only ever runs Chat sessions leaves no trace.
    #[must_use]
    pub fn new(data_dir: &Path, config: &CheckpointsConfig, store: Arc<dyn SessionStore>) -> Self {
        let options = CheckpointOptions {
            ignore_rules: config.ignore_rules.clone(),
            max_file_bytes: config.max_file_bytes,
        };
        Self {
            budget: Budget {
                global_bytes: config.global_budget,
                workspace_bytes: config.workspace_budget,
            },
            pool: Arc::new(CheckpointPool::new(data_dir.join("checkpoints"), options)),
            store,
        }
    }

    /// Where every shadow repository lives.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.pool.root()
    }

    /// The budgets in force.
    #[must_use]
    pub const fn budget(&self) -> Budget {
        self.budget
    }

    /// The workspace's shadow repository, opening it on first ask.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Collision`] when the shadow git dir would not be disjoint from the
    /// workspace, or [`CheckpointError::Io`] when the workspace cannot be canonicalised.
    pub async fn store_for(
        &self,
        workspace: &Path,
    ) -> Result<Arc<CheckpointStore>, CheckpointError> {
        self.pool.for_workspace(workspace).await
    }

    /// The checkpointer a session's write path is wrapped in.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::store_for`] reports — which is why assembly surfaces it rather than
    /// silently degrading to "no checkpoints": a Code session that cannot undo its writes is a
    /// startup finding, not a per-write surprise.
    pub async fn checkpointer_for(
        &self,
        workspace: &Path,
    ) -> Result<Arc<dyn Checkpointer>, CheckpointError> {
        let shadow = self.store_for(workspace).await?;
        Ok(Arc::new(WorkspaceCheckpoints {
            budget: self.budget,
            pool: Arc::clone(&self.pool),
            // The same spelling the sink uses for its rows; see `recorded_workspace`.
            workspace: recorded_workspace(workspace),
            shadow,
            store: Arc::clone(&self.store),
        }))
    }

    /// Deletes every shadow repository whose workspace has no records left.
    ///
    /// Safe by construction: a workspace with no rows has no items carrying its commit ids either
    /// (the rows cascade with the sessions), so nothing can ask to rewind into it. A directory whose
    /// workspace cannot be determined is **left alone** — "cannot tell" must never mean "delete".
    ///
    /// Returns how many directories were reclaimed.
    pub async fn sweep_orphans(&self) -> u64 {
        sweep_orphans(&self.pool, self.store.as_ref()).await
    }
}

/// The sweep, usable without a [`Checkpoints`] — the budget ladder calls it mid-write.
async fn sweep_orphans(pool: &CheckpointPool, store: &dyn SessionStore) -> u64 {
    let mut reclaimed = 0;
    for dir in pool.shadow_dirs().await {
        let Some(workspace) = CheckpointStore::recorded_workspace(&dir) else {
            tracing::warn!(
                directory = %dir.display(),
                "a shadow repository whose workspace cannot be read; leaving it alone"
            );
            continue;
        };
        let owned = match store.checkpoints_for_workspace(&workspace).await {
            Ok(rows) => !rows.is_empty(),
            Err(error) => {
                // Cannot tell whether it has an owner, so it keeps it. The sweep is opportunistic:
                // it runs on every budget check, and a store that stays broken is a bigger problem
                // than a directory that survives one pass.
                tracing::warn!(
                    workspace = %workspace.display(),
                    "cannot check for checkpoint records, keeping the shadow repository: {error}"
                );
                continue;
            }
        };
        if owned {
            continue;
        }
        match pool.discard_dir(&dir).await {
            Ok(()) => {
                tracing::info!(
                    directory = %dir.display(),
                    workspace = %workspace.display(),
                    "reclaimed an orphan shadow repository"
                );
                reclaimed += 1;
            }
            Err(error) => tracing::error!(
                directory = %dir.display(),
                "failed to reclaim an orphan shadow repository: {error}"
            ),
        }
    }
    reclaimed
}

/// One workspace's undo points, with the budget in front of them.
struct WorkspaceCheckpoints {
    budget: Budget,
    pool: Arc<CheckpointPool>,
    shadow: Arc<CheckpointStore>,
    store: Arc<dyn SessionStore>,
    workspace: PathBuf,
}

impl WorkspaceCheckpoints {
    /// Applies the ladder, returning the reason to skip or `None` when the snapshot may be taken.
    ///
    /// A store failure anywhere in here is logged and treated as "budget unknown, proceed": the
    /// budget protects our own storage, and silently losing the ability to undo because a query
    /// failed would be the worse trade.
    async fn enforce(&self) -> Option<String> {
        let global = self.global_bytes().await;
        if global > self.budget.global_bytes {
            let reclaimed = sweep_orphans(&self.pool, self.store.as_ref()).await;
            let after = self.global_bytes().await;
            tracing::warn!(
                before = global,
                after,
                reclaimed,
                limit = self.budget.global_bytes,
                "the global checkpoint budget is exceeded"
            );
            if after > self.budget.global_bytes {
                return Some(format!(
                    "the global checkpoint budget is exceeded ({} of {} bytes after reclaiming \
                     {reclaimed} orphaned repositories)",
                    after, self.budget.global_bytes
                ));
            }
        }

        let bytes = self.shadow.bytes().await;
        if bytes <= self.budget.workspace_bytes {
            return None;
        }
        let snapshots = self.shadow.count().await.unwrap_or(0);
        if snapshots <= 1 {
            // One snapshot already exceeds the budget, so the workspace itself is too big for it.
            // Starting over would only make room for the same snapshot again, and doing that on
            // every write would thrash: destroy, re-snapshot, destroy.
            return Some(format!(
                "one snapshot of {} is {bytes} bytes, over the {} byte budget; nothing to reclaim",
                self.workspace.display(),
                self.budget.workspace_bytes
            ));
        }

        // Reclaim by starting over. The rows go first: a record pointing into a repository that no
        // longer exists would offer a rewind that cannot be performed, and the items that also
        // carry those commit ids are append-only, so they will report "no such checkpoint" from
        // here on. That is the honest cost of a budget, and it is why the warning is loud.
        self.forget_records().await;
        if let Err(error) = self.shadow.destroy().await {
            tracing::error!(
                workspace = %self.workspace.display(),
                "failed to reclaim an over-budget shadow repository: {error}"
            );
            return Some(format!(
                "the shadow repository could not be reclaimed: {error}"
            ));
        }
        tracing::warn!(
            workspace = %self.workspace.display(),
            bytes,
            snapshots,
            limit = self.budget.workspace_bytes,
            "the workspace checkpoint budget was exceeded; discarded its checkpoints and started over"
        );
        None
    }

    /// Drops every record for this workspace, so the sweep and the budget see a clean slate.
    async fn forget_records(&self) {
        let rows = match self.store.checkpoints_for_workspace(&self.workspace).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(
                    workspace = %self.workspace.display(),
                    "cannot list the checkpoint records to discard: {error}"
                );
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        let ids: Vec<_> = rows.iter().map(|row| row.id).collect();
        if let Err(error) = self.store.delete_checkpoints(&ids).await {
            tracing::error!(
                workspace = %self.workspace.display(),
                "failed to discard {} checkpoint records: {error}",
                ids.len()
            );
        }
    }

    /// Bytes across every shadow repository on disk, including ones no session has opened.
    async fn global_bytes(&self) -> u64 {
        let mut total: u64 = 0;
        for dir in self.pool.shadow_dirs().await {
            total = total.saturating_add(dir_bytes(&dir));
        }
        total
    }
}

#[async_trait]
impl Checkpointer for WorkspaceCheckpoints {
    async fn pre_write(&self) -> Result<PreWrite, CheckpointError> {
        if let Some(reason) = self.enforce().await {
            return Ok(PreWrite::Skipped { reason });
        }
        let report = self.shadow.snapshot(CheckpointKind::PreWrite).await?;
        if !report.oversized.is_empty() {
            // ADR-0006's "skip and record": excluding a file is silent data loss for a rewind, so
            // it is logged with the paths rather than folded into a byte count.
            tracing::warn!(
                workspace = %self.workspace.display(),
                paths = ?report.oversized,
                "left out of the checkpoint: larger than the size limit"
            );
        }
        Ok(PreWrite::Taken(report.checkpoint))
    }
}

/// Total size of a directory tree, for the global budget.
///
/// Duplicated from `hatchery-capabilities` on purpose: that one measures a repository this module
/// may not have opened, and reaching into a private helper across a crate boundary to save twelve
/// lines would couple the policy layer to the mechanism's internals.
fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_capabilities::RestoreOptions;
    use hatchery_protocol::{ModelRef, Session, SessionModeId, SessionStatus, Timestamp};
    use hatchery_store::{CheckpointRecord, TursoStore};

    /// A data dir, one workspace inside it, and a store that knows the session owning it.
    struct Fixture {
        dir: tempfile::TempDir,
        workspace: PathBuf,
        store: Arc<dyn SessionStore>,
        session: Session,
    }

    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let workspace = dir.path().join("workspace");
            std::fs::create_dir_all(&workspace).expect("mkdir");
            let store: Arc<dyn SessionStore> = Arc::new(
                TursoStore::open(dir.path().join("test.db"))
                    .await
                    .expect("open"),
            );
            let session = Session {
                id: hatchery_protocol::SessionId::new(),
                title: None,
                mode: SessionModeId::code(),
                workspace: Some(workspace.clone()),
                model: ModelRef::new("deepseek", "deepseek-flash"),
                config_patch: None,
                created_at: Timestamp::now(),
                updated_at: Timestamp::now(),
                active_branch_head: None,
                generation: 0,
                status: SessionStatus::Idle,
            };
            store.create_session(session.clone()).await.expect("create");
            Self {
                dir,
                workspace,
                store,
                session,
            }
        }

        /// A second workspace in the same data dir, with a session that owns it.
        async fn sibling(&self, name: &str) -> PathBuf {
            let workspace = self.dir.path().join(name);
            std::fs::create_dir_all(&workspace).expect("mkdir");
            let session = Session {
                id: hatchery_protocol::SessionId::new(),
                workspace: Some(workspace.clone()),
                ..self.session.clone()
            };
            self.store
                .create_session(session.clone())
                .await
                .expect("create");
            workspace
        }

        fn checkpoints(&self, budget: Budget) -> Checkpoints {
            Checkpoints::new(
                self.dir.path(),
                &CheckpointsConfig {
                    global_budget: budget.global_bytes,
                    ignore_rules: Vec::new(),
                    max_file_bytes: u64::MAX,
                    workspace_budget: budget.workspace_bytes,
                },
                Arc::clone(&self.store),
            )
        }

        /// A checkpoint row, in the shape the pre-restore safety snapshot uses: no item.
        async fn record(&self, workspace: &Path, commit_id: &str) {
            self.store
                .record_checkpoint(CheckpointRecord {
                    id: hatchery_protocol::CheckpointId::new(),
                    session: self.session.id,
                    item: None,
                    workspace: workspace.to_path_buf(),
                    commit_id: commit_id.to_owned(),
                    kind: CheckpointKind::PreWrite,
                    created_at: Timestamp::now(),
                })
                .await
                .expect("record");
        }

        async fn rows(&self, workspace: &Path) -> Vec<CheckpointRecord> {
            self.store
                .checkpoints_for_workspace(workspace)
                .await
                .expect("rows")
        }

        fn write(&self, workspace: &Path, name: &str, contents: &str) {
            std::fs::write(workspace.join(name), contents).expect("write");
        }
    }

    /// D9's reclamation step. Commit-level GC is not available (see the module docs), so the ladder
    /// starts a workspace over: the old checkpoints are lost, and the ones that follow are real
    /// again. The alternative — skipping from here on — would leave the workspace permanently
    /// un-undoable because of a number in a config file.
    #[tokio::test]
    async fn an_over_budget_workspace_starts_over_rather_than_giving_up() {
        let fixture = Fixture::new().await;
        let checkpoints = fixture.checkpoints(Budget {
            global_bytes: u64::MAX,
            workspace_bytes: 1,
        });
        let checkpointer = checkpoints
            .checkpointer_for(&fixture.workspace)
            .await
            .expect("checkpointer");

        // Two snapshots with rows, taken while the budget was not in the way: the workspace has
        // history worth discarding, which is what distinguishes this from the skip case.
        let shadow = checkpoints
            .store_for(&fixture.workspace)
            .await
            .expect("store");
        fixture.write(&fixture.workspace, "a.txt", "one\n");
        let first = shadow
            .snapshot(CheckpointKind::PreWrite)
            .await
            .expect("one");
        fixture
            .record(shadow.workspace(), &first.checkpoint.commit_id)
            .await;
        fixture.write(&fixture.workspace, "b.txt", "two\n");
        let second = shadow
            .snapshot(CheckpointKind::PreWrite)
            .await
            .expect("two");
        fixture
            .record(shadow.workspace(), &second.checkpoint.commit_id)
            .await;
        assert_eq!(fixture.rows(shadow.workspace()).await.len(), 2);
        assert!(shadow.bytes().await > 1, "the fixture must be over budget");

        let outcome = checkpointer.pre_write().await.expect("pre_write");
        let taken = match outcome {
            PreWrite::Taken(checkpoint) => checkpoint,
            PreWrite::Skipped { reason } => {
                panic!("it should have reclaimed, not skipped: {reason}")
            }
        };

        assert!(
            fixture.rows(shadow.workspace()).await.is_empty(),
            "the discarded checkpoints must not stay in the index"
        );
        assert_eq!(
            shadow.count().await.expect("count"),
            1,
            "and the repository starts over from the new snapshot"
        );
        assert_ne!(taken.commit_id, first.checkpoint.commit_id);

        // The cost is honest and bounded: a rewind aimed at a discarded commit reports that it is
        // gone rather than silently restoring something else.
        let error = shadow
            .restore(&first.checkpoint.commit_id, RestoreOptions::default())
            .await
            .expect_err("that checkpoint was reclaimed");
        assert!(
            matches!(error, CheckpointError::UnknownCommit { .. }),
            "{error}"
        );
    }

    /// The other side of D9: when a *single* snapshot already exceeds the budget, reclaiming cannot
    /// help — destroying the repository would only make room for the same snapshot again, on every
    /// write. So it skips, and says why.
    #[tokio::test]
    async fn a_workspace_too_big_for_one_snapshot_skips_instead_of_thrashing() {
        let fixture = Fixture::new().await;
        let checkpoints = fixture.checkpoints(Budget {
            global_bytes: u64::MAX,
            workspace_bytes: 1,
        });
        let checkpointer = checkpoints
            .checkpointer_for(&fixture.workspace)
            .await
            .expect("checkpointer");

        fixture.write(&fixture.workspace, "a.txt", "one\n");
        let first = checkpointer.pre_write().await.expect("under budget");
        assert!(matches!(first, PreWrite::Taken(_)), "{first:?}");

        let skipped = checkpointer.pre_write().await.expect("over budget");
        let reason = match skipped {
            PreWrite::Skipped { reason } => reason,
            taken => panic!("one snapshot over budget must skip, got {taken:?}"),
        };
        assert!(reason.contains("budget"), "{reason}");
        // The one checkpoint that exists is kept: skipping is about not growing, not about
        // discarding what is already there.
        assert_eq!(
            checkpoints
                .store_for(&fixture.workspace)
                .await
                .expect("store")
                .count()
                .await
                .expect("count"),
            1
        );
    }

    /// The global breaker protects the user's disk rather than our index, so it does not destroy
    /// anything anybody still owns: it sweeps orphans and, if that is not enough, stops growing.
    #[tokio::test]
    async fn the_global_budget_sweeps_orphans_and_then_skips() {
        let fixture = Fixture::new().await;
        let checkpoints = fixture.checkpoints(Budget {
            global_bytes: 1,
            workspace_bytes: u64::MAX,
        });
        let checkpointer = checkpoints
            .checkpointer_for(&fixture.workspace)
            .await
            .expect("checkpointer");

        // An orphan: a shadow repository nobody has a row for, which is what a deleted session
        // leaves behind once its rows have cascaded away.
        let abandoned = fixture.sibling("abandoned").await;
        let orphan_store = checkpoints.store_for(&abandoned).await.expect("store");
        fixture.write(&abandoned, "x.txt", "nobody owns this\n");
        orphan_store
            .snapshot(CheckpointKind::Manual)
            .await
            .expect("snapshot");
        let orphan_dir = orphan_store.git_dir().to_path_buf();
        assert!(orphan_dir.exists(), "the fixture built an orphan");

        // And an owned repository, which the sweep must leave alone even under budget pressure —
        // so that after the sweep the global total is still over its 1 byte limit.
        let owned = checkpoints
            .store_for(&fixture.workspace)
            .await
            .expect("store");
        fixture.write(&fixture.workspace, "a.txt", "one\n");
        let snapshot = owned
            .snapshot(CheckpointKind::PreWrite)
            .await
            .expect("snapshot");
        fixture
            .record(owned.workspace(), &snapshot.checkpoint.commit_id)
            .await;

        let skipped = checkpointer.pre_write().await.expect("pre_write");
        match skipped {
            PreWrite::Skipped { reason } => assert!(reason.contains("global"), "{reason}"),
            taken => panic!("the global budget is 1 byte, so this must skip: {taken:?}"),
        }
        assert!(
            !orphan_dir.exists(),
            "the sweep must run before the breaker gives up"
        );
        assert!(
            owned.git_dir().exists(),
            "and it must not reclaim what somebody owns"
        );
    }

    /// The sweep is the only thing that ever notices an abandoned shadow repository, so both of its
    /// answers have to be right: reclaim what nobody owns, keep what somebody does.
    #[tokio::test]
    async fn the_sweep_reclaims_orphans_and_keeps_owned_repositories() {
        let fixture = Fixture::new().await;
        let checkpoints = fixture.checkpoints(Budget {
            global_bytes: u64::MAX,
            workspace_bytes: u64::MAX,
        });

        let owned = checkpoints
            .store_for(&fixture.workspace)
            .await
            .expect("store");
        fixture.write(&fixture.workspace, "a.txt", "one\n");
        let snapshot = owned
            .snapshot(CheckpointKind::PreWrite)
            .await
            .expect("snapshot");
        // Recorded under the canonical spelling, which is what the shadow repository records for
        // itself and therefore what the sweep asks about.
        fixture
            .record(owned.workspace(), &snapshot.checkpoint.commit_id)
            .await;

        let abandoned = fixture.sibling("abandoned").await;
        let orphan = checkpoints.store_for(&abandoned).await.expect("store");
        fixture.write(&abandoned, "x.txt", "nobody owns this\n");
        orphan
            .snapshot(CheckpointKind::Manual)
            .await
            .expect("snapshot");
        let orphan_dir = orphan.git_dir().to_path_buf();
        let owned_dir = owned.git_dir().to_path_buf();

        assert_eq!(checkpoints.sweep_orphans().await, 1, "one orphan");
        assert!(!orphan_dir.exists(), "the orphan was reclaimed");
        assert!(owned_dir.exists(), "an owned repository must survive");
        assert_eq!(fixture.rows(owned.workspace()).await.len(), 1);
        assert_eq!(checkpoints.sweep_orphans().await, 0, "nothing left to do");
    }

    /// Both writers of a checkpoint row spell the workspace the way the shadow repository records
    /// it, so the sweep can find them. If they ever diverge the sweep asks about a path nobody has a
    /// row for and reclaims a *live* repository — user data, gone, with nothing in the log to say
    /// why. This pins the agreement.
    #[tokio::test]
    async fn a_workspace_reached_through_a_different_path_is_still_owned() {
        let fixture = Fixture::new().await;
        let checkpoints = fixture.checkpoints(Budget {
            global_bytes: u64::MAX,
            workspace_bytes: u64::MAX,
        });

        // The same directory reached through a symlink: a genuinely different `PathBuf` that
        // canonicalises to the same workspace. (`join(".")` would not do — `Path` equality compares
        // components, and a `.` component is not one.)
        let spelled = fixture.dir.path().join("link");
        std::os::unix::fs::symlink(&fixture.workspace, &spelled).expect("symlink");
        assert_ne!(spelled, fixture.workspace);
        let store = checkpoints.store_for(&spelled).await.expect("store");
        let checkpointer = checkpoints
            .checkpointer_for(&spelled)
            .await
            .expect("checkpointer");
        assert_eq!(
            recorded_workspace(&spelled),
            recorded_workspace(&fixture.workspace),
            "both spellings must land on the same row key"
        );

        fixture.write(&fixture.workspace, "a.txt", "one\n");
        let snapshot = store
            .snapshot(CheckpointKind::PreWrite)
            .await
            .expect("snapshot");
        // Written the way the runtime's sink writes it: through `recorded_workspace`.
        fixture
            .record(
                &recorded_workspace(&spelled),
                &snapshot.checkpoint.commit_id,
            )
            .await;

        assert_eq!(
            checkpoints.sweep_orphans().await,
            0,
            "an owned repository is not an orphan, however its workspace was spelled"
        );
        assert!(store.git_dir().exists());
        assert!(
            matches!(
                checkpointer.pre_write().await.expect("pre_write"),
                PreWrite::Taken(_)
            ),
            "and the checkpointer bound to the other spelling still works"
        );
    }

    /// The one rule for how a workspace is spelled in a checkpoint row, exercised directly: the
    /// sweep reads the spelling back out of the shadow repository's own config, so anything else is
    /// a row the sweep cannot see.
    #[tokio::test]
    async fn recorded_workspace_is_the_spelling_the_shadow_repository_uses() {
        let fixture = Fixture::new().await;
        let checkpoints = fixture.checkpoints(Budget {
            global_bytes: u64::MAX,
            workspace_bytes: u64::MAX,
        });
        let store = checkpoints
            .store_for(&fixture.workspace)
            .await
            .expect("store");
        fixture.write(&fixture.workspace, "a.txt", "one\n");
        store
            .snapshot(CheckpointKind::PreWrite)
            .await
            .expect("snapshot");

        assert_eq!(
            CheckpointStore::recorded_workspace(store.git_dir()).as_deref(),
            Some(recorded_workspace(&fixture.workspace).as_path()),
            "the row spelling and the repository's own spelling must agree"
        );
        // A path that does not exist has no canonical form; falling back to it as given is better
        // than dropping the row, and the sweep's "cannot tell" branch keeps the repository.
        let absent = fixture.dir.path().join("no-such-workspace");
        assert_eq!(recorded_workspace(&absent), absent);
    }
}
