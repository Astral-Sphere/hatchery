//! The promoted shadow-Git checkpoint store: the recipe the spike measured, now behind a real API
//! (ADR-0006, ADR-0012).
//!
//! The three `invariant_` tests that used to run against the spike's private `Sandbox` live here
//! now, with the same assertions and the real code under test; the spike keeps the measurements that
//! are about **libgit2** rather than about us (per-handle ignore rules, `statuses()` not rewriting
//! the user's index, snapshot cost, budget queryability).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hatchery_capabilities::{
    CheckpointError, CheckpointOptions, CheckpointPool, CheckpointStore, Checkpointer, FsBackend,
    LocalFs, PreWrite, RestoreOptions,
};
use hatchery_protocol::{CheckpointKind, DiffStatus};
use hatchery_testkit::TempWorkspace;

/// A workspace, a shadow git dir outside it, and the small conveniences the tests repeat.
struct Sandbox {
    workspace: TempWorkspace,
    shadow_root: tempfile::TempDir,
}

impl Sandbox {
    /// A plain workspace: no user repository, which is the case ADR-0006 insists must work.
    fn new() -> Self {
        Self {
            shadow_root: tempfile::tempdir().expect("shadow tempdir"),
            workspace: TempWorkspace::new(),
        }
    }

    /// A workspace that is also a dirty user repository — the only interesting starting state for
    /// invariant 6, since a clean one would let "never touched the user's repository" pass by
    /// doing nothing.
    fn git() -> Self {
        Self {
            shadow_root: tempfile::tempdir().expect("shadow tempdir"),
            workspace: TempWorkspace::git(),
        }
    }

    /// The canonical workspace path, which is what `CheckpointStore` records.
    fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.workspace.root()).expect("canonical workspace")
    }

    fn git_dir(&self) -> PathBuf {
        self.shadow_root.path().join("repo.git")
    }

    fn store(&self) -> Arc<CheckpointStore> {
        self.store_with(CheckpointOptions::default())
    }

    fn store_with(&self, options: CheckpointOptions) -> Arc<CheckpointStore> {
        CheckpointStore::open(&self.root(), &self.git_dir(), &options).expect("opens")
    }

    fn write(&self, relative: &str, contents: &str) {
        self.workspace.write(relative, contents);
    }

    fn read(&self, relative: &str) -> Option<String> {
        std::fs::read_to_string(self.workspace.root().join(relative)).ok()
    }

    fn exists(&self, relative: &str) -> bool {
        self.workspace.root().join(relative).exists()
    }
}

/// The policy-free checkpointer: every write gets a snapshot, nothing in front of it. The daemon
/// wraps this shape with its budget ladder (D9); a test wants the mechanism alone.
struct AlwaysSnapshot(Arc<CheckpointStore>);

#[async_trait]
impl Checkpointer for AlwaysSnapshot {
    async fn pre_write(&self) -> Result<PreWrite, CheckpointError> {
        let report = self.0.snapshot(CheckpointKind::PreWrite).await?;
        Ok(PreWrite::Taken(report.checkpoint))
    }
}

// ---------------------------------------------------------------------------
// Invariant 6: the shadow repository must not touch the user's repository.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invariant_shadow_git_never_touches_user_repo() {
    let sandbox = Sandbox::git();
    let store = sandbox.store();

    let before = sandbox.workspace.repo_state();
    assert!(
        !before.status.is_empty(),
        "the user repo should start out dirty, otherwise the test proves nothing"
    );

    let first = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");
    sandbox.write("agent-writes.txt", "written by the agent\n");
    sandbox.write("agent-adds.txt", "added by the agent\n");
    store
        .snapshot(CheckpointKind::PreWrite)
        .await
        .expect("snapshot");
    store
        .restore(&first.checkpoint.commit_id, RestoreOptions::default())
        .await
        .expect("restore");

    let after = sandbox.workspace.repo_state();

    // The agent's own files legitimately show up as untracked in the user's status: invariant 6
    // forbids everything *else*.
    let without_agent_files = |state: &[String]| -> Vec<String> {
        state
            .iter()
            .filter(|line| !line.contains("agent-writes.txt") && !line.contains("agent-adds.txt"))
            .cloned()
            .collect()
    };
    assert_eq!(
        without_agent_files(&before.status),
        without_agent_files(&after.status),
        "the user's status changed beyond the agent's own file writes"
    );
    assert_eq!(before.head, after.head, "user HEAD moved");
    assert_eq!(before.branch, after.branch, "user branch changed");
    assert_eq!(before.refs, after.refs, "user refs changed");
    assert_eq!(
        before.index_mtime, after.index_mtime,
        "the user's index was rewritten"
    );
    assert_eq!(
        before.git_dir_entries, after.git_dir_entries,
        "the user's .git directory gained or lost entries"
    );

    // And the user's own work is still there, in the state they left it.
    assert_eq!(
        sandbox.read("committed.txt").as_deref(),
        Some("committed\n")
    );
    assert_eq!(sandbox.read("staged.txt").as_deref(), Some("staged\n"));
    assert_eq!(sandbox.read("unstaged.txt").as_deref(), Some("unstaged\n"));
}

#[tokio::test]
async fn invariant_no_gitlink_is_planted_in_the_user_workspace() {
    // A workspace with no repository at all: `RepositoryInitOptions::workdir_path` is documented to
    // create a gitlink, which is why the recipe hand-writes `core.worktree` and calls
    // `set_workdir(.., false)` instead (ADR-0012).
    let plain = Sandbox::new();
    let store = plain.store();
    assert!(!plain.exists(".git"), "the workspace starts without a .git");
    store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");
    assert!(
        !plain.exists(".git"),
        "a snapshot planted a .git in a workspace that had none"
    );

    // And a workspace that *is* a repository keeps `.git` as the directory it was.
    let sandbox = Sandbox::git();
    let store = sandbox.store();
    let before = sandbox.workspace.repo_state().git_dir_entries;
    let git_kind = std::fs::metadata(sandbox.workspace.root().join(".git"))
        .expect(".git")
        .file_type();
    assert!(git_kind.is_dir(), ".git should be a directory");

    store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    assert!(
        std::fs::metadata(sandbox.workspace.root().join(".git"))
            .expect(".git")
            .file_type()
            .is_dir(),
        ".git stopped being a directory"
    );
    assert_eq!(
        before,
        sandbox.workspace.repo_state().git_dir_entries,
        "the user's .git gained or lost entries"
    );
}

#[tokio::test]
async fn invariant_purge_restore_also_removes_never_tracked_files() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    sandbox.write("own.txt", "the user's own file\n");

    let target = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");
    sandbox.write("agent.txt", "written after the checkpoint\n");

    // Without purge: the agent's file survives, because a rewind must never delete something the
    // user may have written themselves in the meantime.
    store
        .restore(&target.checkpoint.commit_id, RestoreOptions::default())
        .await
        .expect("restore");
    assert!(sandbox.exists("agent.txt"), "a plain restore purged a file");
    assert!(sandbox.exists("own.txt"));

    // With purge: it goes, and the report says so.
    let report = store
        .restore(
            &target.checkpoint.commit_id,
            RestoreOptions {
                purge_untracked: true,
            },
        )
        .await
        .expect("purging restore");
    assert!(
        !sandbox.exists("agent.txt"),
        "purge left a never-tracked file behind"
    );
    assert!(sandbox.exists("own.txt"), "purge removed a tracked file");
    assert!(
        report
            .purged
            .iter()
            .any(|path| path.to_string_lossy().contains("agent.txt")),
        "the report must name what it deleted: {:?}",
        report.purged
    );
}

// ---------------------------------------------------------------------------
// Restore semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn restore_rolls_back_tracked_files_and_leaves_never_tracked_ones_alone() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    sandbox.write("a.txt", "one\n");
    sandbox.write("b.txt", "one\n");
    let target = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    sandbox.write("a.txt", "two\n");
    store
        .snapshot(CheckpointKind::PreWrite)
        .await
        .expect("snapshot");
    // Written after the last snapshot, so no checkpoint ever tracked it. That distinction is the
    // whole rule: `reset(Hard)` deletes what HEAD tracks and the target does not, so a file that
    // *was* snapshotted later is legitimately rolled away, while one that never was belongs to the
    // user and must survive.
    sandbox.write("c.txt", "created after every checkpoint\n");

    let report = store
        .restore(&target.checkpoint.commit_id, RestoreOptions::default())
        .await
        .expect("restore");

    assert_eq!(sandbox.read("a.txt").as_deref(), Some("one\n"));
    assert_eq!(sandbox.read("b.txt").as_deref(), Some("one\n"));
    assert!(
        sandbox.exists("c.txt"),
        "a file no checkpoint tracked must survive a restore"
    );
    assert!(
        report.rolled_back.iter().any(|p| p.ends_with("a.txt")),
        "the report must name what it rolled back: {:?}",
        report.rolled_back
    );

    // The restore took a safety snapshot first, so the state it destroyed is still reachable —
    // which is the entire reason `checkpoints.item_id` is nullable.
    assert_ne!(
        report.safety.checkpoint.commit_id, target.checkpoint.commit_id,
        "the safety snapshot must be its own commit"
    );
    assert_eq!(report.safety.checkpoint.kind, CheckpointKind::Manual);
    store
        .restore(
            &report.safety.checkpoint.commit_id,
            RestoreOptions::default(),
        )
        .await
        .expect("the safety snapshot is restorable");
    assert_eq!(
        sandbox.read("a.txt").as_deref(),
        Some("two\n"),
        "undo-of-undo must bring the state back"
    );
}

#[tokio::test]
async fn an_unchanged_workspace_reuses_the_previous_commit() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    sandbox.write("a.txt", "one\n");

    let first = store
        .snapshot(CheckpointKind::PreWrite)
        .await
        .expect("snapshot");
    let second = store
        .snapshot(CheckpointKind::PreWrite)
        .await
        .expect("snapshot");

    assert_eq!(
        first.checkpoint.commit_id, second.checkpoint.commit_id,
        "nothing changed, so nothing should be committed"
    );
    assert_eq!(store.count().await.expect("count"), 1);

    sandbox.write("a.txt", "two\n");
    let third = store
        .snapshot(CheckpointKind::PreWrite)
        .await
        .expect("snapshot");
    assert_ne!(first.checkpoint.commit_id, third.checkpoint.commit_id);
    assert_eq!(store.count().await.expect("count"), 2);
}

#[tokio::test]
async fn the_purge_manifest_never_lists_the_users_own_repository() {
    // `.git/` is in the ignore rules, and `untracked()` reads a status list that excludes ignored
    // paths. If either half broke, a `--purge` rewind would offer to delete the user's repository.
    let sandbox = Sandbox::git();
    let store = sandbox.store();
    store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");
    // After the snapshot, so nothing ever tracked it: that is what makes it purgeable.
    sandbox.write("agent.txt", "written by the agent\n");

    let manifest = store.untracked().await.expect("manifest");
    assert!(
        manifest.iter().any(|path| path.ends_with("agent.txt")),
        "the manifest should list what a purge would delete: {manifest:?}"
    );
    assert!(
        !manifest
            .iter()
            .any(|path| path.to_string_lossy().contains(".git")),
        "the manifest must never include the user's repository: {manifest:?}"
    );
    assert!(
        !manifest.iter().any(|path| path.ends_with("committed.txt")),
        "a tracked file is not purgeable: {manifest:?}"
    );
}

#[tokio::test]
async fn the_users_own_git_directory_is_never_snapshotted() {
    let sandbox = Sandbox::git();
    let store = sandbox.store();
    let before = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    // A file inside the user's .git, plus a real workspace change to prove the diff below is not
    // simply empty for some unrelated reason.
    sandbox.write(".git/HATCHERY-MARKER", "should never be tracked\n");
    sandbox.write("work.txt", "a real change\n");
    let after = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    let diff = store
        .diff(&before.checkpoint.commit_id, &after.checkpoint.commit_id)
        .await
        .expect("diff");
    let paths: Vec<&str> = diff.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["work.txt"], "the shadow repo saw .git: {paths:?}");

    // And the user's repository is still exactly what it was, marker and all: exclusion is not
    // deletion, so nothing we did removed anything from in there.
    assert!(sandbox.exists(".git/HATCHERY-MARKER"));
    assert!(sandbox.exists(".git/HEAD"));
    assert!(sandbox.read("committed.txt").is_some());
}

// ---------------------------------------------------------------------------
// Ignore rules and the size limit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ignore_rules_are_replayed_on_every_open() {
    // `add_ignore_rule` binds to the handle, so a store that opened once and cached the handle
    // would quietly start snapshotting whatever the rules used to exclude. The rules therefore have
    // to be replayed by *every* open — which is what the second store below proves, since it is a
    // fresh handle over an existing git dir.
    let sandbox = Sandbox::new();
    let options = CheckpointOptions {
        ignore_rules: vec!["build/".to_owned()],
        ..CheckpointOptions::default()
    };

    sandbox.write("src/main.rs", "fn main() {}\n");
    let first = sandbox.store_with(options.clone());
    let baseline = first
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");
    drop(first);

    // Written while no handle is open, so only a replayed rule can keep them out.
    sandbox.write("build/artifact.o", "binary junk\n");
    sandbox.write("src/lib.rs", "pub fn f() {}\n");
    let second = sandbox.store_with(options);
    let snapshot = second
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");
    let diff = second
        .diff(
            &baseline.checkpoint.commit_id,
            &snapshot.checkpoint.commit_id,
        )
        .await
        .expect("diff");
    let paths: Vec<&str> = diff.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["src/lib.rs"], "the ignored path was snapshotted");
    assert!(
        sandbox.exists("build/artifact.o"),
        "exclusion is not deletion: the file itself must survive"
    );
}

#[tokio::test]
async fn oversized_files_are_left_out_and_reported() {
    let sandbox = Sandbox::new();
    let store = sandbox.store_with(CheckpointOptions {
        max_file_bytes: 1024,
        ..CheckpointOptions::default()
    });
    let baseline = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    sandbox.write("small.txt", "keep me\n");
    sandbox.write("huge.bin", &"x".repeat(2048));
    let report = store
        .snapshot(CheckpointKind::PreWrite)
        .await
        .expect("snapshot");

    assert_eq!(
        report.oversized,
        ["huge.bin"],
        "the exclusion must be reported"
    );
    let diff = store
        .diff(&baseline.checkpoint.commit_id, &report.checkpoint.commit_id)
        .await
        .expect("diff");
    let paths: Vec<&str> = diff.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["small.txt"]);
    assert!(
        sandbox.exists("huge.bin"),
        "an oversized file is excluded from the snapshot, not deleted"
    );

    // A limit of zero means "no size filter", not "exclude everything that has bytes": the second
    // reading would silently turn every snapshot into an empty one.
    let unlimited = sandbox.store_with(CheckpointOptions {
        max_file_bytes: u64::MAX,
        ..CheckpointOptions::default()
    });
    let report = unlimited
        .snapshot(CheckpointKind::PreWrite)
        .await
        .expect("snapshot");
    assert!(report.oversized.is_empty(), "{:?}", report.oversized);
}

#[tokio::test]
async fn a_workspace_gitignore_is_honoured_by_the_shadow_repository() {
    // Measured, and worth pinning because it is not obvious: the shadow repository is bare with an
    // external work tree and `core.excludesFile` pointing at a path that does not exist, yet
    // libgit2 still reads the `.gitignore` files inside the work tree. That is what keeps a Rust
    // workspace's `target/` out of every snapshot without us inventing an exclusion list — and it
    // is also why a non-git workspace needs `CheckpointOptions::ignore_rules` to get the same.
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    let baseline = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    sandbox.write(".gitignore", "target/\n*.log\n");
    sandbox.write("target/debug/huge", "artefact\n");
    sandbox.write("run.log", "noise\n");
    sandbox.write("src/main.rs", "fn main() {}\n");
    let snapshot = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    let diff = store
        .diff(
            &baseline.checkpoint.commit_id,
            &snapshot.checkpoint.commit_id,
        )
        .await
        .expect("diff");
    let paths: Vec<&str> = diff.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, [".gitignore", "src/main.rs"], "{paths:?}");
    assert!(
        sandbox.exists("target/debug/huge") && sandbox.exists("run.log"),
        "being ignored is not being deleted"
    );

    // The same workspace without a `.gitignore` tracks everything, which is the case the configured
    // rules exist for.
    let plain = Sandbox::new();
    let plain_store = plain.store();
    let baseline = plain_store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");
    plain.write("target/debug/huge", "artefact\n");
    let snapshot = plain_store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");
    let diff = plain_store
        .diff(
            &baseline.checkpoint.commit_id,
            &snapshot.checkpoint.commit_id,
        )
        .await
        .expect("diff");
    assert_eq!(
        diff.files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["target/debug/huge"],
        "without a .gitignore nothing is excluded but .git/"
    );
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

#[tokio::test]
async fn diff_between_checkpoints_carries_structured_hunks() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    sandbox.write("keep.txt", "unchanged\n");
    sandbox.write("edit.txt", "one\ntwo\nthree\n");
    sandbox.write("gone.txt", "deleted later\n");
    let before = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    sandbox.write("edit.txt", "one\nTWO\nthree\nfour\n");
    sandbox.write("new.txt", "created\n");
    std::fs::remove_file(sandbox.workspace.root().join("gone.txt")).expect("remove");
    let after = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    let diff = store
        .diff(&before.checkpoint.commit_id, &after.checkpoint.commit_id)
        .await
        .expect("diff");
    let by_path: std::collections::BTreeMap<&str, &hatchery_protocol::DiffFile> = diff
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();

    assert_eq!(
        by_path.keys().copied().collect::<Vec<_>>(),
        ["edit.txt", "gone.txt", "new.txt"],
        "an unchanged file is not a difference"
    );
    assert_eq!(by_path["new.txt"].status, DiffStatus::Added);
    assert_eq!(by_path["gone.txt"].status, DiffStatus::Deleted);
    assert_eq!(by_path["edit.txt"].status, DiffStatus::Modified);

    let hunks = &by_path["edit.txt"].hunks;
    assert_eq!(hunks.len(), 1, "one contiguous change is one hunk");
    let hunk = &hunks[0];
    assert_eq!(hunk.old_start, 1, "{hunk:?}");
    // Old side = context + removed (one, two, three); new side = context + added (one, TWO, three,
    // four). The header counts the file's lines, not the hunk's rendered length.
    assert_eq!(hunk.old_lines, 3, "{hunk:?}");
    assert_eq!(hunk.new_start, 1);
    assert_eq!(hunk.new_lines, 4, "{hunk:?}");

    use hatchery_protocol::DiffLineKind::{Added, Context, Removed};
    let lines: Vec<_> = hunk
        .lines
        .iter()
        .map(|line| (line.kind, line.text.as_str()))
        .collect();
    assert!(lines.contains(&(Context, "one")), "{lines:?}");
    assert!(lines.contains(&(Removed, "two")), "{lines:?}");
    assert!(lines.contains(&(Added, "TWO")), "{lines:?}");
    assert!(lines.contains(&(Added, "four")), "{lines:?}");
    assert!(
        lines.iter().all(|(_, text)| !text.ends_with('\n')),
        "a hunk line carries no newline: {lines:?}"
    );
    assert_eq!(diff.line_counts(), (3, 2), "{:?}", diff.line_counts());
}

#[tokio::test]
async fn diff_of_a_binary_file_says_so_instead_of_showing_nothing() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    let before = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");
    sandbox.write("image.png", "\u{89}PNG\r\n\u{1a}\n\0\0binary");
    let after = store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    let diff = store
        .diff(&before.checkpoint.commit_id, &after.checkpoint.commit_id)
        .await
        .expect("diff");
    let file = diff
        .files
        .iter()
        .find(|file| file.path == "image.png")
        .expect("the binary file is a difference");
    assert!(file.binary, "{file:?}");
    assert!(
        file.hunks.is_empty(),
        "a binary file has no lines: {file:?}"
    );
    assert_eq!(file.status, DiffStatus::Added);
}

#[tokio::test]
async fn an_unknown_commit_is_named_rather_than_wrapped() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    let error = store
        .diff("not-a-commit", "also-not")
        .await
        .expect_err("no such commit");
    assert!(
        matches!(&error, CheckpointError::UnknownCommit { commit_id, .. } if commit_id == "not-a-commit"),
        "{error}"
    );
    let error = store
        .restore(
            "0000000000000000000000000000000000000000",
            RestoreOptions::default(),
        )
        .await
        .expect_err("no such commit");
    assert!(
        matches!(error, CheckpointError::UnknownCommit { .. }),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// The disjointness assertion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_shadow_dir_that_is_not_disjoint_from_the_workspace_is_refused() {
    let sandbox = Sandbox::git();
    let root = sandbox.root();
    let options = CheckpointOptions::default();

    // The user's own .git: writing checkpoints there would be writing into their repository.
    let error = CheckpointStore::open(&root, &root.join(".git"), &options)
        .expect_err("that is the user's repository");
    assert!(
        matches!(error, CheckpointError::Collision { .. }),
        "{error}"
    );

    // Inside the workspace at all: the shadow repository would snapshot itself and grow every
    // time it did.
    let error = CheckpointStore::open(&root, &root.join("shadow.git"), &options)
        .expect_err("it would snapshot itself");
    assert!(
        matches!(error, CheckpointError::Collision { .. }),
        "{error}"
    );

    // Inside the user's .git, which is inside the workspace.
    let error = CheckpointStore::open(&root, &root.join(".git").join("hatchery"), &options)
        .expect_err("inside the user's .git");
    assert!(
        matches!(error, CheckpointError::Collision { .. }),
        "{error}"
    );
}

#[tokio::test]
async fn a_shadow_dir_that_claims_another_workspace_is_refused() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    // The same git dir, offered a different workspace: either a name collision or a data directory
    // that was copied or moved. Either way, checkpointing the wrong tree is worse than refusing.
    let other = tempfile::tempdir().expect("another workspace");
    let error = CheckpointStore::open(
        &std::fs::canonicalize(other.path()).expect("canonical"),
        &sandbox.git_dir(),
        &CheckpointOptions::default(),
    )
    .expect_err("the git dir belongs to somebody else");
    let owner = store.workspace().to_string_lossy();
    assert!(
        matches!(&error, CheckpointError::Collision { reason, .. } if reason.contains(owner.as_ref())),
        "the refusal must name the workspace the git dir really belongs to: {error}"
    );
}

// ---------------------------------------------------------------------------
// The pool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_pool_shares_one_store_per_workspace() {
    let sandbox = Sandbox::new();
    let root = sandbox.root();
    let pool = CheckpointPool::new(
        sandbox.shadow_root.path().join("checkpoints"),
        CheckpointOptions::default(),
    );

    // Two sessions on one workspace must share one repository, or each one's snapshots would be
    // blind to the other's writes (ADR-0006).
    let first = pool.for_workspace(&root).await.expect("store");
    let again = pool
        .for_workspace(sandbox.workspace.root())
        .await
        .expect("the same store, spelled differently");
    assert_eq!(first.git_dir(), again.git_dir());

    first
        .snapshot(CheckpointKind::PreWrite)
        .await
        .expect("snapshot");
    sandbox.write("later.txt", "written between the two handles\n");
    let second = again
        .snapshot(CheckpointKind::PreWrite)
        .await
        .expect("snapshot");
    assert_ne!(
        first.git_dir(),
        sandbox.workspace.root(),
        "the shadow dir is never the workspace"
    );
    assert_eq!(again.count().await.expect("count"), 2);
    assert_eq!(first.count().await.expect("count"), 2);
    assert_ne!(second.checkpoint.commit_id, String::new());

    // The directory name is derived from the workspace path, so it is stable across pool instances.
    let dir_name = first
        .git_dir()
        .file_name()
        .expect("a name")
        .to_string_lossy()
        .into_owned();
    assert_eq!(dir_name.len(), 32, "a uuid in its simple form: {dir_name}");
    let other_pool = CheckpointPool::new(
        sandbox.shadow_root.path().join("checkpoints"),
        CheckpointOptions::default(),
    );
    assert_eq!(
        other_pool.git_dir_for(&root).file_name().expect("a name"),
        std::ffi::OsStr::new(&dir_name),
        "the same workspace must map to the same directory"
    );
}

#[tokio::test]
async fn an_orphan_shadow_repository_is_decidable_and_reclaimable() {
    let sandbox = Sandbox::new();
    let root = sandbox.root();
    let pool = CheckpointPool::new(
        sandbox.shadow_root.path().join("checkpoints"),
        CheckpointOptions::default(),
    );
    let store = pool.for_workspace(&root).await.expect("store");
    store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");
    let bytes_before = store.bytes().await;
    assert!(bytes_before > 0, "a snapshot occupies something");

    // The sweep's input: a directory whose recorded workspace can be looked up in the store.
    let dirs = pool.shadow_dirs().await;
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    assert_eq!(
        CheckpointStore::recorded_workspace(&dirs[0]).as_deref(),
        Some(root.as_path()),
        "the sweep must be able to ask whose repository this is"
    );

    pool.discard_dir(&dirs[0]).await.expect("reclaimed");
    assert!(!dirs[0].exists(), "the directory is gone");
    assert!(
        pool.shadow_dirs().await.is_empty(),
        "and nothing is left to sweep"
    );
    // Discarding twice is not an error: the sweep races a delete it may already have done.
    pool.discard_dir(&dirs[0]).await.expect("idempotent");
}

#[tokio::test]
async fn a_directory_that_is_not_ours_is_left_alone() {
    let sandbox = Sandbox::new();
    let root = PathBuf::from(sandbox.shadow_root.path()).join("checkpoints");
    std::fs::create_dir_all(root.join("not-a-repository")).expect("mkdir");
    let pool = CheckpointPool::new(root, CheckpointOptions::default());

    let dirs = pool.shadow_dirs().await;
    assert_eq!(dirs.len(), 1);
    assert_eq!(
        CheckpointStore::recorded_workspace(&dirs[0]),
        None,
        "a directory we did not create has no marker"
    );
    // "Cannot tell" must never mean "delete": the sweep in the daemon skips these, and so does
    // anything else that acts on the answer.
    assert!(dirs[0].exists());
}

// ---------------------------------------------------------------------------
// The write path: LocalFs behind the decorator
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_write_through_the_seam_leaves_an_undo_point_that_restores_it() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    sandbox.write("existing.txt", "before\n");
    store
        .snapshot(CheckpointKind::Manual)
        .await
        .expect("snapshot");

    let inner = LocalFs::new(sandbox.workspace.root()).expect("backend");
    let checkpointer = AlwaysSnapshot(Arc::clone(&store));
    let collector = hatchery_kernel::CheckpointCollector::new();
    let fs = hatchery_capabilities::CheckpointedFs::new(&inner, &checkpointer, &collector);

    // A brand-new path, so the write also has to create its parent directories.
    fs.write_text_file("src/new/file.rs", "fn main() {}\n")
        .await
        .expect("writes");
    fs.write_text_file("existing.txt", "after\n")
        .await
        .expect("writes");

    assert_eq!(
        sandbox.read("src/new/file.rs").as_deref(),
        Some("fn main() {}\n")
    );
    assert_eq!(sandbox.read("existing.txt").as_deref(), Some("after\n"));

    // The write sequence and the checkpoint records line up: one undo point per write, in order,
    // and each one describes the state *before* its own write.
    let collected = collector.drain();
    assert_eq!(collected.len(), 2, "{collected:?}");
    assert!(collected.iter().all(|c| c.kind == CheckpointKind::PreWrite));

    // And the first undo point really is the pre-write state of the first write.
    store
        .restore(&collected[0].commit_id, RestoreOptions::default())
        .await
        .expect("restore");
    assert!(
        !sandbox.exists("src/new/file.rs"),
        "restoring the first pre-write checkpoint must undo the first write"
    );
    assert_eq!(sandbox.read("existing.txt").as_deref(), Some("before\n"));
}

#[tokio::test]
async fn a_write_outside_the_workspace_is_refused_by_the_local_backend() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    let inner = LocalFs::new(sandbox.workspace.root()).expect("backend");
    let checkpointer = AlwaysSnapshot(Arc::clone(&store));
    let collector = hatchery_kernel::CheckpointCollector::new();
    let fs = hatchery_capabilities::CheckpointedFs::new(&inner, &checkpointer, &collector);

    for path in ["../escape.txt", "/etc/escape.txt", "a/../../escape.txt"] {
        let error = fs.write_text_file(path, "nope").await.expect_err("refused");
        assert!(
            matches!(error, hatchery_capabilities::FsError::OutsideWorkspace(_)),
            "{path}: {error}"
        );
    }
    assert!(
        !sandbox
            .workspace
            .root()
            .parent()
            .unwrap_or(Path::new("/"))
            .join("escape.txt")
            .exists(),
        "nothing was written outside"
    );
}

/// Whether the symlink a case needs is now on the disk. Windows mints a symbolic link only with
/// `SeCreateSymbolicLinkPrivilege` (or developer mode) and answers error 1314 without it — measured
/// on `x86_64-pc-windows-gnu`, where the account running the tests is not an administrator. The
/// escape these cases guard is a property of the resolver, not of the runner, so a machine that
/// cannot build the fixture says which case it skips instead of failing a case that never existed.
/// Recorded as skipped-on-platform in `docs/worklog/capabilities.md`.
fn symlink_was_minted(made: std::io::Result<()>) -> bool {
    match made {
        Ok(()) => true,
        #[cfg(windows)]
        Err(error) if error.raw_os_error() == Some(1314) => {
            eprintln!("skipped: this machine cannot create symlinks (os error 1314)");
            false
        }
        Err(error) => panic!("the symlink fixture failed: {error}"),
    }
}

#[tokio::test]
async fn a_symlinked_ancestor_cannot_carry_a_write_out_of_the_workspace() {
    // The escape the write path has to close that the lexical check cannot see: `link/` points
    // outside, so `create_dir_all` would happily build directories over there — before any check
    // had run, if resolution anchored on the file instead of on the deepest existing ancestor.
    let sandbox = Sandbox::new();
    let outside = tempfile::tempdir().expect("outside");
    #[cfg(unix)]
    let minted = std::os::unix::fs::symlink(outside.path(), sandbox.workspace.root().join("link"));
    #[cfg(windows)]
    let minted =
        std::os::windows::fs::symlink_dir(outside.path(), sandbox.workspace.root().join("link"));
    if !symlink_was_minted(minted) {
        return;
    }

    let store = sandbox.store();
    let inner = LocalFs::new(sandbox.workspace.root()).expect("backend");
    let checkpointer = AlwaysSnapshot(Arc::clone(&store));
    let collector = hatchery_kernel::CheckpointCollector::new();
    let fs = hatchery_capabilities::CheckpointedFs::new(&inner, &checkpointer, &collector);

    let error = fs
        .write_text_file("link/deeper/file.txt", "nope")
        .await
        .expect_err("refused");
    assert!(
        matches!(error, hatchery_capabilities::FsError::OutsideWorkspace(_)),
        "{error}"
    );
    assert!(
        !outside.path().join("deeper").exists(),
        "directories were created outside the workspace: {:?}",
        std::fs::read_dir(outside.path())
            .expect("read")
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );

    // A dangling symlink is refused for the same reason: writing "through" it would create the
    // file at its destination, outside every checkpoint.
    #[cfg(unix)]
    let minted = std::os::unix::fs::symlink(
        outside.path().join("absent"),
        sandbox.workspace.root().join("dangling"),
    );
    #[cfg(windows)]
    let minted = std::os::windows::fs::symlink_file(
        outside.path().join("absent"),
        sandbox.workspace.root().join("dangling"),
    );
    if !symlink_was_minted(minted) {
        return;
    }
    let error = fs
        .write_text_file("dangling", "nope")
        .await
        .expect_err("refused");
    assert!(
        matches!(error, hatchery_capabilities::FsError::Io(_)),
        "a broken symlink must not be written through: {error}"
    );
    assert!(!outside.path().join("absent").exists());
}

#[tokio::test]
async fn a_workspace_that_is_not_a_repository_still_checkpoints() {
    // ADR-0006: the shadow repository does not depend on the user having one.
    let sandbox = Sandbox::new();
    assert!(sandbox.workspace.repo().is_none(), "no user repository");
    let store = sandbox.store();
    sandbox.write("a.txt", "one\n");
    let first = store
        .snapshot(CheckpointKind::PreWrite)
        .await
        .expect("snapshot");
    sandbox.write("a.txt", "two\n");
    store
        .restore(&first.checkpoint.commit_id, RestoreOptions::default())
        .await
        .expect("restore");
    assert_eq!(sandbox.read("a.txt").as_deref(), Some("one\n"));
}
