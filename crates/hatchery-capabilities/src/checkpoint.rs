//! The shadow-Git checkpoint store: one repository per workspace, its git dir under our own data
//! directory and its work tree the user's workspace (ADR-0006, ADR-0012).
//!
//! Everything here is **mechanism**. The budget that decides whether a snapshot may be taken at all
//! needs the `checkpoints` table, which lives in a sibling layer this crate must not depend on, so
//! the policy sits in the daemon and calls into this (D9). What this crate guarantees instead is
//! invariant 6: a checkpoint never touches the user's own repository — not its HEAD, not its index,
//! not its refs, and not by planting a `.git` gitlink in their workspace.
//!
//! The recipe below is transcribed from `tests/spike_shadow_git.rs`, which measured each of its
//! load-bearing choices against libgit2 and stays in the tree as a permanent gate:
//!
//! * `init_opts` with `no_dotgit_dir` + `bare` + `external_template(false)`, then **hand-written**
//!   `core.worktree` / `core.bare=false`, then `set_workdir(.., false)`. libgit2 only persists the
//!   work tree when `update_gitlink` is true, and that same flag writes a `.git` file into the
//!   user's workspace.
//! * ignore rules are replayed on **every** open, because `add_ignore_rule` binds to the handle.
//! * the developer's machine is locked out: no external template, a `core.excludesFile` that does
//!   not exist, `core.autocrlf=false`, `core.fsmonitor=false`, fixed identity.
//!
//! # What garbage collection can and cannot do here
//!
//! libgit2 has no object-level GC — `Repository` exposes `odb()` (read, write, foreach; no delete)
//! and `cleanup_state()` (stale state files, not objects), and nothing else. Dropping a commit from
//! the middle or the front of the chain would therefore mean rewriting the surviving ones into a
//! fresh object store, and **a rewritten commit has a different id** — which the append-only
//! `items` table already carries inside `ItemKind::Checkpoint { commit_id }` and can never update
//! (`items_no_update` trigger). So this store offers exactly one reclamation primitive,
//! [`CheckpointStore::destroy`], which removes a whole shadow repository, and it is only ever sound
//! for one whose checkpoints nothing can reference any more: an orphan, i.e. a workspace with no
//! rows left in `checkpoints` (storage.md open question 3). Over-budget workspaces are handled by
//! not growing, not by shrinking (D9).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use git2::build::CheckoutBuilder;
use git2::{
    Delta, DiffFlags, DiffLineType, IndexAddOption, ObjectType, Oid, Patch, Repository,
    RepositoryInitOptions, ResetType, Signature, StatusOptions, Tree,
};
use hatchery_protocol::{
    Checkpoint, CheckpointKind, Diff, DiffFile, DiffHunk, DiffLineKind, DiffStatus,
};
use uuid::Uuid;

/// The namespace every shadow-repository directory name is derived from.
///
/// Minted once as a random v4 UUID and **frozen**: the directory name of a workspace's shadow
/// repository is `Uuid::new_v5(NAMESPACE, workspace)`, so changing this constant would relocate
/// every checkpoint on every machine that ever ran the build and silently orphan them all.
const NAMESPACE: Uuid = Uuid::from_bytes([
    0x83, 0xad, 0x3c, 0x8c, 0x44, 0x71, 0x4a, 0xba, 0x92, 0x47, 0x2a, 0x3c, 0x56, 0xbf, 0x31, 0x43,
]);

/// `.git/` keeps the user's own repository out of every snapshot.
///
/// Load-bearing for the purge path too: because ignored paths are excluded from `statuses()`, a
/// workspace that *is* a git repository never has its `.git` contents listed as purgeable.
const BASE_IGNORE_RULES: &str = ".git/\n";

/// ADR-0006's large-file threshold: above it a file is left out of the snapshot and reported.
///
/// Exclusion, not deletion — the file stays exactly where it is (measured in the spike).
pub const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Why a checkpoint operation failed.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    /// The shadow git dir and the workspace are not disjoint: one contains the other, or the
    /// shadow dir sits where the user's own `.git` lives. Refused at open, before anything is
    /// written, because the alternative is checkpointing a repository into itself.
    #[error(
        "the shadow repository at {shadow} is not disjoint from the workspace {workspace}: {reason}"
    )]
    Collision {
        /// Where we wanted to put the shadow git dir.
        shadow: PathBuf,
        /// The workspace it was for.
        workspace: PathBuf,
        /// Which of the disjointness rules was broken.
        reason: String,
    },
    /// A commit id that this shadow repository does not have. Distinct from [`Self::Git`] because
    /// it is the expected outcome of rewinding to a checkpoint that garbage collection has already
    /// reclaimed, and the caller owes the user that specific sentence.
    #[error("no checkpoint {commit_id} in the shadow repository at {shadow}")]
    UnknownCommit {
        /// The id that was asked for.
        commit_id: String,
        /// Where it was looked for.
        shadow: PathBuf,
    },
    /// libgit2 refused.
    #[error("shadow repository: {0}")]
    Git(#[from] git2::Error),
    /// The disk refused.
    #[error("shadow repository: {0}")]
    Io(String),
    /// The blocking worker was lost, which only happens if the snapshot task panicked.
    #[error("the checkpoint task did not finish: {0}")]
    Task(String),
}

/// How a workspace's snapshots are taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointOptions {
    /// Ignore rules replayed on every open, on top of `.git/`. Gitignore syntax, one rule per
    /// line. Build artefacts belong here: they are the difference between a 3 kB shadow repository
    /// and a copy of `target/`.
    pub ignore_rules: Vec<String>,
    /// Files larger than this are left out of snapshots and reported in
    /// [`SnapshotReport::oversized`].
    pub max_file_bytes: u64,
}

impl Default for CheckpointOptions {
    fn default() -> Self {
        Self {
            ignore_rules: Vec::new(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        }
    }
}

/// What one snapshot produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotReport {
    /// The checkpoint itself: the commit id and why it was taken.
    pub checkpoint: Checkpoint,
    /// Workspace-relative paths left out because they exceeded
    /// [`CheckpointOptions::max_file_bytes`]. Never empty-by-accident: a caller that ignores this
    /// is telling the user "you can rewind this" when one file will not rewind.
    pub oversized: Vec<String>,
}

/// What a restore is allowed to do beyond rolling back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RestoreOptions {
    /// Also delete files that appeared after the target and that no snapshot ever tracked.
    ///
    /// Default false: a rewind never deletes the user's own untracked files. Turning it on needs
    /// approval plus a listed manifest first ([`CheckpointStore::untracked`] is that manifest).
    pub purge_untracked: bool,
}

/// What a restore did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreReport {
    /// The snapshot taken **before** anything was rolled back, so the restore is itself undoable.
    ///
    /// Recorded with `item_id = NULL` and no item: it is undo-of-undo, not conversation history —
    /// which is exactly why that column is nullable (storage.md §2).
    pub safety: SnapshotReport,
    /// Tracked files whose content the restore changed.
    pub rolled_back: Vec<PathBuf>,
    /// Files the purge removed. Empty unless [`RestoreOptions::purge_untracked`].
    pub purged: Vec<PathBuf>,
}

/// One workspace's shadow repository.
///
/// Cheap to hold and safe to share: every operation opens a fresh libgit2 handle (the ignore rules
/// are per-handle, so a cached handle would silently lose them) and takes the internal lock first,
/// which is what serialises two sessions bound to the same workspace (ADR-0006).
#[derive(Debug)]
pub struct CheckpointStore {
    shadow: Arc<Shadow>,
    /// One shadow repository, one writer: `index.add_all` + `commit` + `reset` are each a
    /// read-modify-write of the same git dir, and two of them interleaving would lose a snapshot.
    lock: tokio::sync::Mutex<()>,
}

/// The owned, `'static` half of a store: what a blocking worker needs and nothing more.
#[derive(Debug)]
struct Shadow {
    workspace: PathBuf,
    git_dir: PathBuf,
    ignore_rules: String,
    max_file_bytes: u64,
}

impl CheckpointStore {
    /// The workspace a shadow git dir was created for, read from the marker its own `create` wrote.
    ///
    /// The orphan sweep needs this and has nothing else to go on: the directory name is a derived
    /// uuid, so "whose repository is this?" can only be answered from inside it.
    ///
    /// `None` means *cannot tell* — not a repository, not one of ours, or unreadable — and a caller
    /// that is deciding whether to delete the directory must treat that as "leave it alone".
    #[must_use]
    pub fn recorded_workspace(git_dir: &Path) -> Option<PathBuf> {
        let repo = Repository::open(git_dir).ok()?;
        let recorded = repo.config().ok()?.get_string("hatchery.workspace").ok()?;
        (!recorded.is_empty()).then(|| PathBuf::from(recorded))
    }

    /// Binds a workspace to its shadow git dir.
    ///
    /// Nothing is created here — a shadow repository comes into existence with its first snapshot —
    /// but anything that *already* exists is verified now, at the point where refusing is cheap. A
    /// git dir that is not disjoint from the workspace, or that records a different workspace than
    /// the one being bound to it, is a [`CheckpointError::Collision`]: the alternative is
    /// checkpointing somebody else's tree, or a repository into itself, and finding that out during
    /// a user's first write is the worst moment for it.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Collision`] when the two paths overlap or the git dir belongs to another
    /// workspace, [`CheckpointError::Io`] when the workspace does not exist or cannot be
    /// canonicalised, and [`CheckpointError::Io`] for a workspace path that is not valid UTF-8 —
    /// `core.worktree` is a git *config string*, so there is no way to record one.
    pub fn open(
        workspace: &Path,
        git_dir: &Path,
        options: &CheckpointOptions,
    ) -> Result<Arc<Self>, CheckpointError> {
        let workspace = canonical(workspace).map_err(|error| {
            CheckpointError::Io(format!("workspace {}: {error}", workspace.display()))
        })?;
        assert_disjoint(git_dir, &workspace)?;

        let mut ignore_rules = String::from(BASE_IGNORE_RULES);
        for rule in &options.ignore_rules {
            ignore_rules.push_str(rule);
            if !rule.ends_with('\n') {
                ignore_rules.push('\n');
            }
        }

        let shadow = Arc::new(Shadow {
            max_file_bytes: options.max_file_bytes,
            git_dir: git_dir.to_path_buf(),
            ignore_rules,
            workspace,
        });
        // Checked here rather than at snapshot time: a workspace whose path cannot be written into
        // git config can never be checkpointed, and finding that out during a user's first write
        // is the worst moment for it.
        shadow.workspace_config_value()?;
        shadow.verify_existing()?;
        Ok(Arc::new(Self {
            lock: tokio::sync::Mutex::new(()),
            shadow,
        }))
    }

    /// The canonical workspace this store snapshots.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.shadow.workspace
    }

    /// Where the shadow git dir lives. Never inside the workspace, never the user's `.git`.
    #[must_use]
    pub fn git_dir(&self) -> &Path {
        &self.shadow.git_dir
    }

    /// Snapshots the whole workspace and returns the commit it produced.
    ///
    /// A snapshot whose tree is identical to the current head does **not** create a commit: it
    /// returns that head's id instead. Writing the same bytes twice is not a new state, and a
    /// write loop would otherwise pile up empty commits that every budget count then has to
    /// explain.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Git`] / [`CheckpointError::Io`] when libgit2 or the disk refuses,
    /// [`CheckpointError::Task`] if the blocking worker panicked.
    pub async fn snapshot(&self, kind: CheckpointKind) -> Result<SnapshotReport, CheckpointError> {
        let shadow = Arc::clone(&self.shadow);
        let _guard = self.lock.lock().await;
        spawn(move || {
            let repo = shadow.open_handle()?;
            shadow.commit_snapshot(&repo, kind, true)
        })
        .await
    }

    /// What changed between two checkpoints, as structured hunks.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::UnknownCommit`] when either id is not in this shadow repository — the
    /// expected answer for a checkpoint that garbage collection has reclaimed — or
    /// [`CheckpointError::Git`] when the trees will not diff.
    pub async fn diff(&self, from: &str, to: &str) -> Result<Diff, CheckpointError> {
        let shadow = Arc::clone(&self.shadow);
        let from = from.to_owned();
        let to = to.to_owned();
        let _guard = self.lock.lock().await;
        spawn(move || {
            let repo = shadow.open_handle()?;
            let old = shadow.tree_of(&repo, &from)?;
            let new = shadow.tree_of(&repo, &to)?;
            shadow.diff_trees(&repo, Some(&old), Some(&new))
        })
        .await
    }

    /// Rolls the workspace back to a checkpoint, snapshotting the current state first.
    ///
    /// The safety snapshot is what makes a rewind itself rewindable. Note that it is left
    /// unreachable from the shadow repository's head — the reset moves head to the target — and
    /// stays findable only because nothing here prunes objects and the `checkpoints` row keeps its
    /// id. That is a deliberate trade, not an oversight: see the module docs on GC.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::UnknownCommit`] when `to` is not in this shadow repository, or
    /// [`CheckpointError::Git`] / [`CheckpointError::Io`] when the rollback itself fails.
    pub async fn restore(
        &self,
        to: &str,
        options: RestoreOptions,
    ) -> Result<RestoreReport, CheckpointError> {
        let shadow = Arc::clone(&self.shadow);
        let to = to.to_owned();
        let _guard = self.lock.lock().await;
        spawn(move || {
            let repo = shadow.open_handle()?;
            shadow.restore_from(&repo, &to, options)
        })
        .await
    }

    /// The files a purge would delete: untracked, and not ignored.
    ///
    /// This is the manifest the approval prompt owes the user before
    /// [`RestoreOptions::purge_untracked`] is allowed to be true. Ignored paths are absent by
    /// construction, which is what keeps a purge from ever listing the user's `.git`, their build
    /// output or their `.env`.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Git`] when libgit2 will not produce a status list.
    pub async fn untracked(&self) -> Result<Vec<PathBuf>, CheckpointError> {
        let shadow = Arc::clone(&self.shadow);
        let _guard = self.lock.lock().await;
        spawn(move || {
            let repo = shadow.open_handle()?;
            shadow.untracked_in(&repo)
        })
        .await
    }

    /// Bytes the shadow git dir occupies — what a budget measures.
    ///
    /// Zero when the repository has never been created, so a workspace that has not checkpointed
    /// yet costs nothing rather than failing the query.
    pub async fn bytes(&self) -> u64 {
        let shadow = Arc::clone(&self.shadow);
        spawn(move || Ok(dir_bytes(&shadow.git_dir)))
            .await
            .unwrap_or(0)
    }

    /// How many snapshots are reachable from the shadow repository's head.
    ///
    /// Reachable, not recorded: after a restore, the checkpoints that followed the target are still
    /// in the object store and still named by items, but they are no longer on this chain. Budget
    /// accounting therefore uses [`Self::bytes`], and this count is for humans and tests.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Git`] when the walk fails.
    pub async fn count(&self) -> Result<u64, CheckpointError> {
        let shadow = Arc::clone(&self.shadow);
        let _guard = self.lock.lock().await;
        spawn(move || {
            let repo = shadow.open_handle()?;
            let mut walk = repo.revwalk()?;
            match repo.head() {
                Ok(_) => walk.push_head()?,
                // No commit yet: nothing to count, and pushing an unborn head is an error.
                Err(_) => return Ok(0),
            }
            Ok(walk.count() as u64)
        })
        .await
    }

    /// Deletes the shadow repository outright.
    ///
    /// The only reclamation this crate offers, and sound only for an **orphan**: a workspace whose
    /// `checkpoints` rows are all gone, so no item anywhere still carries one of its commit ids.
    /// Deleting a live one would strand every rewind target that points into it.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Io`] when the directory will not go away.
    pub async fn destroy(&self) -> Result<(), CheckpointError> {
        let shadow = Arc::clone(&self.shadow);
        let _guard = self.lock.lock().await;
        spawn(move || match std::fs::remove_dir_all(&shadow.git_dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(CheckpointError::Io(format!(
                "removing {}: {error}",
                shadow.git_dir.display()
            ))),
        })
        .await
    }
}

impl Shadow {
    /// The workspace path as it must appear in git config.
    fn workspace_config_value(&self) -> Result<&str, CheckpointError> {
        self.workspace.to_str().ok_or_else(|| {
            CheckpointError::Io(format!(
                "workspace {} is not valid UTF-8, so it cannot be recorded as core.worktree",
                self.workspace.display()
            ))
        })
    }

    /// Verifies a shadow git dir that already exists, without creating one.
    ///
    /// Called from `open`, so a git dir that belongs to another workspace — a name collision, a
    /// copied data directory — is refused at assembly time instead of at the first write.
    fn verify_existing(&self) -> Result<(), CheckpointError> {
        if !self.git_dir.join("HEAD").exists() {
            return Ok(());
        }
        let repo = Repository::open(&self.git_dir)?;
        self.verify(&repo)
    }

    /// Opens the shadow repository, creating it on first use.
    fn open_handle(&self) -> Result<Repository, CheckpointError> {
        let repo = if self.git_dir.join("HEAD").exists() {
            Repository::open(&self.git_dir)?
        } else {
            self.create()?
        };
        self.harden(&repo)?;
        // Per-handle in libgit2 (measured): a handle opened without replaying the rules snapshots
        // build output and the user's `.git`.
        repo.add_ignore_rule(&self.ignore_rules)?;
        self.verify(&repo)?;
        Ok(repo)
    }

    fn create(&self) -> Result<Repository, CheckpointError> {
        if let Some(parent) = self.git_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                CheckpointError::Io(format!("creating {}: {error}", parent.display()))
            })?;
        }
        let mut options = RepositoryInitOptions::new();
        // The git dir is exactly `git_dir` (no appended `/.git`), and no templates from whatever
        // machine this happens to be.
        options
            .no_dotgit_dir(true)
            .bare(true)
            .external_template(false);
        let repo = Repository::init_opts(&self.git_dir, &options)?;

        // Hand-written config, because `set_workdir(.., update_gitlink=true)` — the only variant
        // that persists `core.worktree` — is also the one that writes a `.git` file into the user's
        // workspace (libgit2 `repository.c`, and measured in the spike).
        let workspace = self.workspace_config_value()?;
        {
            let mut config = repo.config()?;
            config.set_str("core.worktree", workspace)?;
            config.set_bool("core.bare", false)?;
            // Our own identity marker: `open_handle` refuses a git dir that claims a different
            // workspace, which is how a name collision or a copied data directory stays loud
            // instead of quietly checkpointing the wrong tree.
            config.set_str("hatchery.workspace", workspace)?;
        }
        repo.set_workdir(&self.workspace, false)?;
        Ok(repo)
    }

    /// Locks out the developer's machine, on every open.
    fn harden(&self, repo: &Repository) -> Result<(), CheckpointError> {
        let mut config = repo.config()?;
        config.set_str("user.name", "hatchery")?;
        config.set_str("user.email", "checkpoint@hatchery.invalid")?;
        config.set_str("core.autocrlf", "false")?;
        // A global excludesfile would silently shrink every snapshot, so it points somewhere that
        // does not exist. Inside the git dir rather than `/nonexistent`: the same code has to work
        // on Windows.
        let excludes = self.git_dir.join("no-global-excludes");
        config.set_str("core.excludesFile", excludes.to_string_lossy().as_ref())?;
        config.set_bool("core.fsmonitor", false)?;
        Ok(())
    }

    /// Proves the handle is the one we think it is, on every open.
    fn verify(&self, repo: &Repository) -> Result<(), CheckpointError> {
        let workdir = repo.workdir().ok_or_else(|| {
            CheckpointError::Io(format!(
                "the shadow repository at {} has no work tree",
                self.git_dir.display()
            ))
        })?;
        let workdir = canonical(workdir).map_err(|error| {
            CheckpointError::Io(format!("shadow work tree {}: {error}", workdir.display()))
        })?;
        if workdir != self.workspace {
            return Err(CheckpointError::Collision {
                reason: format!(
                    "its work tree is {} rather than the workspace we asked for",
                    workdir.display()
                ),
                shadow: self.git_dir.clone(),
                workspace: self.workspace.clone(),
            });
        }
        // A git dir that records somebody else's workspace is not ours to write into.
        let recorded = repo
            .config()?
            .get_string("hatchery.workspace")
            .unwrap_or_default();
        let expected = self.workspace_config_value()?;
        if !recorded.is_empty() && recorded != expected {
            return Err(CheckpointError::Collision {
                reason: format!("it records the workspace {recorded}"),
                shadow: self.git_dir.clone(),
                workspace: self.workspace.clone(),
            });
        }
        Ok(())
    }

    fn signature(&self) -> Result<Signature<'static>, CheckpointError> {
        Ok(Signature::now("hatchery", "checkpoint@hatchery.invalid")?)
    }

    /// Snapshots the work tree into a commit.
    ///
    /// `update_head` is false only for the pre-restore safety snapshot, and the reason is
    /// load-bearing: `reset(Hard)` deletes the files that **HEAD tracks** and the target does not,
    /// so making the safety snapshot HEAD first would turn every rewind into a purge and delete the
    /// user's own never-tracked files — the exact thing `RestoreOptions::purge_untracked` defaults
    /// to false to prevent. Left unreferenced, the commit is still findable by id (it is in the
    /// object store, and nothing here prunes objects), which is all a restore needs.
    fn commit_snapshot(
        &self,
        repo: &Repository,
        kind: CheckpointKind,
        update_head: bool,
    ) -> Result<SnapshotReport, CheckpointError> {
        let mut index = repo.index()?;
        let mut oversized: Vec<String> = Vec::new();
        let limit = self.max_file_bytes;
        let workspace = self.workspace.clone();
        {
            let mut record = oversized_collector(&workspace, limit, &mut oversized);
            index.add_all(["*"], IndexAddOption::DEFAULT, Some(&mut record))?;
        }
        index.write()?;
        let tree = repo.find_tree(index.write_tree()?)?;

        let head = repo.head().ok().and_then(|head| head.target());
        // Nothing changed since the last snapshot: reuse its commit rather than piling up empty
        // ones that every budget count would then have to explain.
        if let Some(head) = head
            && let Ok(commit) = repo.find_commit(head)
            && commit.tree_id() == tree.id()
        {
            return Ok(SnapshotReport {
                checkpoint: Checkpoint {
                    commit_id: head.to_string(),
                    kind,
                },
                oversized,
            });
        }

        let signature = self.signature()?;
        let parent = head.and_then(|oid| repo.find_commit(oid).ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        // The commit message cannot carry the item id that ADR-0006 asks for: the kernel mints the
        // item *after* the tool returns, so at this moment there is nothing to name. The
        // association lives in the `checkpoints` row (`item_id`) and in the `Checkpoint` item's own
        // payload (`commit_id`), both of which are stronger than a message string. Recorded as a
        // deviation in worklog/capabilities.md rather than by amending the ADR.
        let oid = repo.commit(
            update_head.then_some("HEAD"),
            &signature,
            &signature,
            &format!("hatchery checkpoint ({})", kind_label(kind)),
            &tree,
            &parents,
        )?;
        Ok(SnapshotReport {
            checkpoint: Checkpoint {
                commit_id: oid.to_string(),
                kind,
            },
            oversized,
        })
    }

    fn tree_of<'repo>(
        &self,
        repo: &'repo Repository,
        commit_id: &str,
    ) -> Result<Tree<'repo>, CheckpointError> {
        let oid = Oid::from_str(commit_id).map_err(|_| self.unknown_commit(commit_id))?;
        let commit = repo
            .find_commit(oid)
            .map_err(|_| self.unknown_commit(commit_id))?;
        Ok(commit.tree()?)
    }

    fn unknown_commit(&self, commit_id: &str) -> CheckpointError {
        CheckpointError::UnknownCommit {
            commit_id: commit_id.to_owned(),
            shadow: self.git_dir.clone(),
        }
    }

    fn diff_trees(
        &self,
        repo: &Repository,
        old: Option<&Tree<'_>>,
        new: Option<&Tree<'_>>,
    ) -> Result<Diff, CheckpointError> {
        let mut diff = repo.diff_tree_to_tree(old, new, None)?;
        // Best effort: rename detection only changes how a delete+add pair is reported, and a
        // failure to detect similarity must not fail the diff.
        let _ = diff.find_similar(None);

        let mut out = Diff::new();
        for (index, delta) in diff.deltas().enumerate() {
            let Some(status) = status_of(delta.status()) else {
                continue;
            };
            let new_path = delta.new_file().path().unwrap_or(Path::new(""));
            let path = slash_separated(new_path);
            let old_path = delta
                .old_file()
                .path()
                .filter(|_| matches!(status, DiffStatus::Renamed | DiffStatus::Copied))
                .map(slash_separated);

            // Built before the flags are read, and the order matters: libgit2 only decides a file
            // is binary once something loads its content, and building the patch is what does that.
            // Measured — `delta.flags()` is empty beforehand and `BINARY` afterwards, so a diff that
            // checked first would report every binary file as a text file with no hunks.
            let patch = Patch::from_diff(&diff, index)?;
            if delta.flags().contains(DiffFlags::BINARY) {
                let mut file = DiffFile::binary(path, status);
                if let Some(old_path) = old_path {
                    file = file.with_old_path(old_path);
                }
                out.push_file(file);
                continue;
            }

            let mut hunks = Vec::new();
            if let Some(patch) = patch {
                for h in 0..patch.num_hunks() {
                    let (header, _) = patch.hunk(h)?;
                    let mut hunk = DiffHunk::new(
                        header.old_start(),
                        header.old_lines(),
                        header.new_start(),
                        header.new_lines(),
                    );
                    for l in 0..patch.num_lines_in_hunk(h)? {
                        let line = patch.line_in_hunk(h, l)?;
                        let Some(kind) = line_kind(line.origin_value()) else {
                            continue;
                        };
                        // The content carries the newline; a hunk line is stored without one, so a
                        // renderer owns the joining (and a wrapped line does not inherit the wrap).
                        let text = String::from_utf8_lossy(line.content());
                        hunk.lines.push(hatchery_protocol::DiffLine {
                            kind,
                            text: text.trim_end_matches(['\n', '\r']).to_owned(),
                        });
                    }
                    hunks.push(hunk);
                }
            }

            let mut file = match status {
                DiffStatus::Added => DiffFile::added(path, hunks),
                DiffStatus::Deleted => DiffFile::deleted(path, hunks),
                _ => DiffFile::modified(path, hunks),
            };
            if let Some(old_path) = old_path {
                file = file.with_old_path(old_path);
            }
            out.push_file(file);
        }
        Ok(out)
    }

    fn restore_from(
        &self,
        repo: &Repository,
        to: &str,
        options: RestoreOptions,
    ) -> Result<RestoreReport, CheckpointError> {
        let target_oid = Oid::from_str(to).map_err(|_| self.unknown_commit(to))?;
        let target = repo
            .find_object(target_oid, Some(ObjectType::Commit))
            .map_err(|_| self.unknown_commit(to))?;

        let head_before = repo.head().ok().and_then(|head| head.target());

        // Undo-of-undo, taken before anything is destroyed — and deliberately *not* made HEAD, see
        // `commit_snapshot`.
        let safety = self.commit_snapshot(repo, CheckpointKind::Manual, false)?;
        // Staging the work tree is how a snapshot is built, but the index is also what `reset(Hard)`
        // consults to decide what to delete. Left staged, a file no checkpoint ever tracked would
        // look tracked and be removed, turning every rewind into a purge — the one thing
        // `purge_untracked: false` exists to prevent. Put the index back the way HEAD had it.
        self.unstage_to(repo, head_before)?;

        let rolled_back = {
            let head_tree = head_before
                .and_then(|oid| repo.find_commit(oid).ok())
                .and_then(|commit| commit.tree().ok());
            let target_tree = repo.find_commit(target_oid)?.tree()?;
            self.diff_trees(repo, head_tree.as_ref(), Some(&target_tree))?
                .files
                .into_iter()
                .map(|file| PathBuf::from(file.path))
                .collect()
        };

        // The purge manifest is computed before the purge: `checkout_index` does not report what it
        // removed, and the user has to see the list before approving it anyway.
        let purged = if options.purge_untracked {
            self.untracked_in(repo)?
        } else {
            Vec::new()
        };

        let mut checkout = CheckoutBuilder::new();
        checkout.force().recreate_missing(true);
        repo.reset(&target, ResetType::Hard, Some(&mut checkout))?;

        if options.purge_untracked {
            // A hard reset only checks out the paths that differ from the target, so it never
            // removes files that were untracked all along — that needs a second pass over the
            // index (measured). `remove_ignored` is deliberately *not* set: a purge removes
            // untracked files, not build output and not the user's `.env`.
            let mut purge = CheckoutBuilder::new();
            purge.force().remove_untracked(true);
            repo.checkout_index(None, Some(&mut purge))?;
        }

        Ok(RestoreReport {
            purged,
            rolled_back,
            safety,
        })
    }

    /// Puts the shadow index back to a commit's tree, undoing the staging a snapshot did.
    ///
    /// Only `restore` needs this, and only because `reset(Hard)` reads the index to decide what to
    /// delete: a staged-but-never-committed file looks tracked, and would be removed.
    fn unstage_to(&self, repo: &Repository, head: Option<Oid>) -> Result<(), CheckpointError> {
        let mut index = repo.index()?;
        match head.and_then(|oid| repo.find_commit(oid).ok()) {
            Some(commit) => index.read_tree(&commit.tree()?)?,
            // No commit to go back to: an empty index is the honest equivalent.
            None => index.clear()?,
        }
        index.write()?;
        Ok(())
    }

    fn untracked_in(&self, repo: &Repository) -> Result<Vec<PathBuf>, CheckpointError> {
        let mut options = StatusOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .exclude_submodules(true);
        // `include_ignored` stays false, which is the whole safety story here: ignored paths never
        // reach the list, so `.git/`, build output and anything the ignore rules cover cannot be
        // purged by a rewind.
        let statuses = repo.statuses(Some(&mut options))?;
        let mut paths: Vec<PathBuf> = statuses
            .iter()
            .filter(|entry| entry.status() == git2::Status::WT_NEW)
            .filter_map(|entry| entry.path().ok().map(PathBuf::from))
            .collect();
        paths.sort();
        Ok(paths)
    }
}

/// The per-workspace shadow repositories, shared by every session bound to the same workspace.
///
/// Sharing is the point (ADR-0006): two sessions in one workspace must not hold two repositories
/// over the same files, or each one's snapshots would be blind to the other's writes. The cache is
/// also where the mutual exclusion lives — one [`CheckpointStore`] per workspace means one lock per
/// workspace, and the daemon holds a single pool.
pub struct CheckpointPool {
    root: PathBuf,
    options: CheckpointOptions,
    stores: tokio::sync::Mutex<HashMap<PathBuf, Arc<CheckpointStore>>>,
}

impl CheckpointPool {
    /// A pool whose shadow repositories live under `root` (`<data>/checkpoints`).
    #[must_use]
    pub fn new(root: PathBuf, options: CheckpointOptions) -> Self {
        Self {
            options,
            root,
            stores: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Where `root` is: every shadow git dir is a directory below it.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The shadow git dir a workspace maps to — a pure function, so a caller can answer "is this
    /// directory still owned?" without opening anything.
    #[must_use]
    pub fn git_dir_for(&self, workspace: &Path) -> PathBuf {
        self.root.join(
            Uuid::new_v5(
                &NAMESPACE,
                workspace.as_os_str().to_string_lossy().as_bytes(),
            )
            .simple()
            .to_string(),
        )
    }

    /// The store for a workspace, opening (and caching) it on first ask.
    ///
    /// # Errors
    ///
    /// Whatever [`CheckpointStore::open`] reports, plus [`CheckpointError::Io`] when the workspace
    /// itself cannot be canonicalised.
    pub async fn for_workspace(
        &self,
        workspace: &Path,
    ) -> Result<Arc<CheckpointStore>, CheckpointError> {
        let canonical = canonical(workspace).map_err(|error| {
            CheckpointError::Io(format!("workspace {}: {error}", workspace.display()))
        })?;
        let mut stores = self.stores.lock().await;
        if let Some(store) = stores.get(&canonical) {
            return Ok(Arc::clone(store));
        }
        let git_dir = self.git_dir_for(&canonical);
        let store = CheckpointStore::open(&canonical, &git_dir, &self.options)?;
        stores.insert(canonical, Arc::clone(&store));
        Ok(store)
    }

    /// Every shadow git dir on disk, whether or not anything is cached for it.
    ///
    /// The orphan sweep needs the ones nobody opened: a workspace whose sessions were all deleted
    /// has a directory here and no rows in `checkpoints`, and that is decidable only by looking at
    /// both sides.
    pub async fn shadow_dirs(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect();
        dirs.sort();
        dirs
    }

    /// Removes one shadow git dir from disk and from the cache.
    ///
    /// Takes a directory rather than a workspace because the orphan sweep works from
    /// [`Self::shadow_dirs`], where the workspace is only knowable by opening the repository.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Io`] when the directory will not go away. A directory that is already
    /// gone is success.
    pub async fn discard_dir(&self, git_dir: &Path) -> Result<(), CheckpointError> {
        {
            let mut stores = self.stores.lock().await;
            stores.retain(|_, store| store.git_dir() != git_dir);
        }
        match std::fs::remove_dir_all(git_dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(CheckpointError::Io(format!(
                "removing {}: {error}",
                git_dir.display()
            ))),
        }
    }
}

/// Builds the `add_all` filter that leaves oversized files out of a snapshot.
///
/// The contract is libgit2's, not ours: 0 keeps the path, a positive value skips it, a negative one
/// aborts the whole scan (which we never ask for — an unstat-able file is skipped, not fatal).
fn oversized_collector<'a>(
    workspace: &'a Path,
    limit: u64,
    oversized: &'a mut Vec<String>,
) -> impl FnMut(&Path, &[u8]) -> i32 + 'a {
    move |path: &Path, _matched: &[u8]| {
        let Ok(meta) = std::fs::metadata(workspace.join(path)) else {
            return 0;
        };
        if meta.is_file() && meta.len() > limit {
            oversized.push(slash_separated(path));
            return 1;
        }
        0
    }
}

/// Maps libgit2's delta onto the wire vocabulary, or `None` for a delta that describes no
/// difference — `Unmodified`, and the three working-tree-only kinds (`Ignored`, `Untracked`,
/// `Conflicted`) that a tree-to-tree diff cannot produce.
fn status_of(delta: Delta) -> Option<DiffStatus> {
    Some(match delta {
        Delta::Added => DiffStatus::Added,
        Delta::Deleted => DiffStatus::Deleted,
        Delta::Modified => DiffStatus::Modified,
        Delta::Renamed => DiffStatus::Renamed,
        Delta::Copied => DiffStatus::Copied,
        Delta::Typechange => DiffStatus::TypeChanged,
        // A tree-to-tree diff has no working tree to be untracked, ignored, conflicted or
        // unreadable in, and an unmodified entry is not a difference. Skipping is honest: the
        // alternative would be inventing a wire status for something that cannot happen.
        Delta::Unmodified
        | Delta::Ignored
        | Delta::Untracked
        | Delta::Unreadable
        | Delta::Conflicted => return None,
    })
}

/// Maps a diff line onto the wire vocabulary, or `None` for the lines a hunk must not carry.
///
/// The `*EOFNL` variants are dropped on purpose: they mark "no newline at end of file" and carry no
/// text of their own, and inventing a line for them would show a phantom entry in every renderer.
/// The header and binary kinds only ever reach a `git_diff_print` callback, never `line_in_hunk`.
fn line_kind(origin: DiffLineType) -> Option<DiffLineKind> {
    Some(match origin {
        DiffLineType::Context => DiffLineKind::Context,
        DiffLineType::Addition => DiffLineKind::Added,
        DiffLineType::Deletion => DiffLineKind::Removed,
        DiffLineType::ContextEOFNL
        | DiffLineType::AddEOFNL
        | DiffLineType::DeleteEOFNL
        | DiffLineType::FileHeader
        | DiffLineType::HunkHeader
        | DiffLineType::Binary => return None,
    })
}

/// The human-readable spelling of a checkpoint kind, for the commit message.
fn kind_label(kind: CheckpointKind) -> &'static str {
    match kind {
        CheckpointKind::PreWrite => "pre-write",
        CheckpointKind::PreShell => "pre-shell",
        CheckpointKind::Manual => "manual",
    }
}

/// A path as the wire vocabulary spells it: `/`-separated on every platform, so a diff recorded on
/// Windows and rendered on Linux is the same bytes.
fn slash_separated(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

/// Total size of a directory tree, without a dependency: the shadow git dir is ours, shallow, and
/// contains no symlinks worth following.
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

/// The startup assertion: the shadow git dir must have nothing to do with the user's repository.
///
/// Four overlaps are refused, each of which would put our writes somewhere the user did not offer
/// them: the shadow dir *is* their `.git`; it lives inside their `.git`; it lives inside the
/// workspace at all (then it would snapshot itself, and grow every time it did); or the workspace
/// lives inside it (a work tree inside its own git dir).
fn assert_disjoint(git_dir: &Path, workspace: &Path) -> Result<(), CheckpointError> {
    let collision = |reason: String| CheckpointError::Collision {
        reason,
        shadow: git_dir.to_path_buf(),
        workspace: workspace.to_path_buf(),
    };
    let user_git = workspace.join(".git");

    if git_dir == user_git {
        return Err(collision("it is the user's own .git".to_owned()));
    }
    if git_dir.starts_with(&user_git) {
        return Err(collision("it lives inside the user's .git".to_owned()));
    }
    if git_dir.starts_with(workspace) {
        return Err(collision(
            "it lives inside the workspace, so it would snapshot itself".to_owned(),
        ));
    }
    if workspace.starts_with(git_dir) {
        return Err(collision(
            "the workspace lives inside it; a work tree cannot be inside its own git dir"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Runs blocking git2 work off the async worker.
///
/// libgit2 is synchronous, and a cold snapshot of a 500-file workspace measured 48.9 ms — long
/// enough that parking a runtime worker on it would be visible to every other session. The same
/// reasoning is why `LocalFs` reads through `tokio::fs`.
async fn spawn<T>(
    work: impl FnOnce() -> Result<T, CheckpointError> + Send + 'static,
) -> Result<T, CheckpointError>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| CheckpointError::Task(error.to_string()))?
}
