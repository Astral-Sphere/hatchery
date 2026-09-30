//! Shadow-git spike: measured behaviour of the **git2 / vendored libgit2** backend for checkpoints,
//! kept as permanent regression tests (ADR-0006, ADR-0012, docs/design/testing.md §3.5).
//!
//! The shape is "a repository whose git dir lives under our own data directory while its work tree
//! is the user's workspace". The point of this file is invariant 6: checkpoints must never touch the
//! user's own repository — not its HEAD, not its index, not its refs, and not even by planting a
//! `.git` gitlink file in their workspace.
//!
//! Everything runs in a tempdir. libgit2 reads the developer's global/system git configuration and
//! template directory, which would make these tests depend on the machine running them, so the
//! helper below disables external templates and pins the config keys that change snapshot or
//! checkout behaviour (`core.autocrlf`, `core.excludesFile`, identity, fsmonitor).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use git2::build::CheckoutBuilder;
use git2::{
    IndexAddOption, ObjectType, Oid, Repository, RepositoryInitOptions, ResetType, Signature,
    StatusOptions,
};
use tempfile::TempDir;

/// `.git/` keeps a user's own repository out of our snapshots. `CheckpointStore` adds the
/// large-file and build-artifact exclusions from configuration on top of this
/// (docs/design/capabilities.md §2).
const SHADOW_IGNORE_RULES: &str = ".git/\n";

/// A workspace, optionally a user git repository inside it, plus the shadow repository's git dir.
struct Sandbox {
    // Held only to keep the directory alive; the underscore keeps dead_code quiet.
    _dir: TempDir,
    workspace: PathBuf,
    git_dir: PathBuf,
    // libgit2's ignore rules added with `add_ignore_rule` live on the repository handle, so they
    // have to be re-applied every time the shadow repo is opened. That is exactly what this field
    // models: CheckpointStore will derive its rules from configuration on each open.
    extra_ignore_rules: RefCell<String>,
}

impl Sandbox {
    fn new(user_repo: bool) -> Self {
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = dir.path().to_path_buf();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");

        let sandbox = Self {
            _dir: dir,
            extra_ignore_rules: RefCell::new(String::new()),
            git_dir: root.join("shadow").join("repo.git"),
            workspace,
        };
        if user_repo {
            sandbox.make_user_repo();
        }
        sandbox
    }

    fn signature() -> Signature<'static> {
        Signature::now("hatchery-spike", "spike@localhost").expect("signature")
    }

    /// Adds ignore rules that every subsequent `open_shadow` re-applies.
    fn add_extra_ignore(&self, rules: &str) {
        self.extra_ignore_rules.borrow_mut().push_str(rules);
    }

    /// Pins the config keys that would otherwise leak in from the developer's machine and change
    /// what we snapshot or how we check it out.
    fn harden(repo: &Repository) {
        let mut config = repo.config().expect("repo config");
        config
            .set_str("user.name", "hatchery-spike")
            .expect("set user.name");
        config
            .set_str("user.email", "spike@localhost")
            .expect("set user.email");
        config
            .set_str("core.autocrlf", "false")
            .expect("set core.autocrlf");
        // A developer's global excludesfile would silently shrink our snapshots.
        config
            .set_str("core.excludesFile", "/nonexistent/hatchery-excludes")
            .expect("set core.excludesFile");
        config
            .set_bool("core.fsmonitor", false)
            .expect("set core.fsmonitor");
    }

    /// Opens the shadow repository, creating it on first use, with the user's workspace as its work
    /// tree and **no** gitlink written into that workspace.
    fn open_shadow(&self) -> Repository {
        let repo = if self.git_dir.join("HEAD").exists() {
            Repository::open(&self.git_dir).expect("open shadow repo")
        } else {
            let mut options = RepositoryInitOptions::new();
            // The git dir is exactly `git_dir` (no appended /.git), and no templates from the
            // developer's machine.
            options
                .no_dotgit_dir(true)
                .bare(true)
                .external_template(false);
            let repo = Repository::init_opts(&self.git_dir, &options).expect("init shadow repo");
            // libgit2's `git_repository_set_workdir` only persists `core.worktree` and
            // `core.bare=false` when `update_gitlink` is true — and that same flag is what writes a
            // `.git` gitlink into the work tree (repository.c, git_repository_set_workdir). So the
            // config is written by hand here and the in-memory handle is pointed at the workspace
            // with `update_gitlink: false`, which leaves the user's directory alone.
            let mut config = repo.config().expect("shadow config");
            config
                .set_str(
                    "core.worktree",
                    self.workspace.to_str().expect("workspace path is utf-8"),
                )
                .expect("set core.worktree");
            config
                .set_bool("core.bare", false)
                .expect("clear core.bare");
            drop(config);
            repo.set_workdir(&self.workspace, false)
                .expect("attach the user workspace as the work tree");
            repo
        };
        Self::harden(&repo);
        repo.add_ignore_rule(SHADOW_IGNORE_RULES)
            .expect("add ignore rules");
        let extra = self.extra_ignore_rules.borrow();
        if !extra.is_empty() {
            repo.add_ignore_rule(&extra)
                .expect("add extra ignore rules");
        }
        drop(extra);

        let workdir =
            std::fs::canonicalize(repo.workdir().expect("the shadow repo has a work tree"))
                .expect("canonicalise the work tree");
        let expected = std::fs::canonicalize(&self.workspace).expect("canonicalise the workspace");
        assert_eq!(
            workdir, expected,
            "the shadow repository must use the user's workspace as its work tree"
        );
        repo
    }

    /// Opens the user's own repository.
    fn open_user_repo(&self) -> Repository {
        Repository::open(&self.workspace).expect("open the user's repository")
    }

    fn user_index_mtime(&self) -> Option<SystemTime> {
        mtime(&self.workspace.join(".git").join("index"))
    }

    /// Everything about the user's repository that shadow operations must not change.
    fn user_repo_state(&self) -> UserRepoState {
        let repo = self.open_user_repo();
        let mut options = StatusOptions::new();
        options.include_untracked(true).recurse_untracked_dirs(true);
        let status_list = repo.statuses(Some(&mut options)).expect("statuses");
        let mut status: Vec<String> = status_list
            .iter()
            .map(|entry| {
                format!(
                    "{:?} {}",
                    entry.status(),
                    entry.path().unwrap_or("<unparsable>")
                )
            })
            .collect();
        status.sort();

        let mut refs: Vec<String> = repo
            .references()
            .expect("references")
            .map(|reference| {
                let reference = reference.expect("reference");
                format!(
                    "{} {:?}",
                    reference.name().unwrap_or("<unparsable>"),
                    reference.target()
                )
            })
            .collect();
        refs.sort();

        UserRepoState {
            status,
            head: repo
                .head()
                .ok()
                .and_then(|head| head.target())
                .map(|oid| oid.to_string())
                .unwrap_or_else(|| "unborn".to_owned()),
            branch: repo
                .head()
                .ok()
                .and_then(|head| head.shorthand().ok().map(str::to_owned))
                .unwrap_or_else(|| "none".to_owned()),
            refs,
            index_mtime: self.user_index_mtime(),
            git_dir_entries: sorted_entries(&self.workspace.join(".git")),
        }
    }

    /// Creates a user repository with one commit, plus staged, unstaged and untracked changes.
    fn make_user_repo(&self) {
        let repo = Repository::init(&self.workspace).expect("init the user's repository");
        Self::harden(&repo);

        self.write("committed.txt", "committed\n");
        let mut index = repo.index().expect("user index");
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .expect("add all");
        index.write().expect("write index");
        let tree = repo
            .find_tree(index.write_tree().expect("write tree"))
            .expect("find tree");
        let signature = Self::signature();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .expect("initial commit");

        self.write("staged.txt", "staged\n");
        let mut index = repo.index().expect("user index");
        index
            .add_path(Path::new("staged.txt"))
            .expect("stage staged.txt");
        index.write().expect("write index");

        self.write("unstaged.txt", "unstaged\n");
        self.write("untracked.txt", "untracked\n");
    }

    fn write(&self, name: &str, contents: &str) {
        let path = self.workspace.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(path, contents).expect("write file");
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.workspace.join(name)).ok()
    }

    fn exists(&self, name: &str) -> bool {
        self.workspace.join(name).exists()
    }

    /// Takes a checkpoint of the whole workspace and returns its commit id.
    fn snapshot(&self, label: &str) -> Oid {
        let repo = self.open_shadow();
        let mut index = repo.index().expect("shadow index");
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .expect("stage the workspace");
        index.write().expect("write shadow index");
        let tree = repo
            .find_tree(index.write_tree().expect("write tree"))
            .expect("find tree");
        let signature = Self::signature();
        let parent = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .and_then(|oid| repo.find_commit(oid).ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &signature, &signature, label, &tree, &parents)
            .expect("checkpoint commit")
    }

    fn tracked_files(&self, commit: Oid) -> Vec<String> {
        let repo = self.open_shadow();
        let tree = repo
            .find_commit(commit)
            .expect("find commit")
            .tree()
            .expect("commit tree");
        let mut files = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
            if matches!(entry.kind(), Some(ObjectType::Blob)) {
                files.push(format!("{root}{}", entry.name().unwrap_or("<unparsable>")));
            }
            git2::TreeWalkResult::Ok
        })
        .expect("walk tree");
        files.sort();
        files
    }

    /// Restores the workspace to a checkpoint. `purge` also removes files that no checkpoint ever
    /// tracked — the `RestoreOptions.purge_untracked` of docs/design/capabilities.md §2.
    fn restore(&self, to: Oid, purge: bool) {
        let repo = self.open_shadow();
        let target = repo
            .find_object(to, Some(ObjectType::Commit))
            .expect("find target commit");
        let mut checkout = CheckoutBuilder::new();
        checkout.force().recreate_missing(true);
        repo.reset(&target, ResetType::Hard, Some(&mut checkout))
            .expect("hard reset to the checkpoint");

        if purge {
            // A hard reset only checks out the paths that differ from the target, so it never
            // deletes files that were untracked all along. That needs a second, explicit pass over
            // the index. Ignored files are deliberately kept: purge removes untracked files, not
            // build output or the user's `.env`.
            let mut purge_checkout = CheckoutBuilder::new();
            purge_checkout.force().remove_untracked(true);
            repo.checkout_index(None, Some(&mut purge_checkout))
                .expect("purge untracked files");
        }
    }

    fn changed_paths(&self, from: Oid, to: Oid) -> Vec<String> {
        let repo = self.open_shadow();
        let (old, new) = (
            repo.find_commit(from).expect("from").tree().expect("tree"),
            repo.find_commit(to).expect("to").tree().expect("tree"),
        );
        let diff = repo
            .diff_tree_to_tree(Some(&old), Some(&new), None)
            .expect("diff trees");
        let mut paths: Vec<String> = diff
            .deltas()
            .map(|delta| {
                delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "<unparsable>".to_owned())
            })
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }

    /// Total size of the shadow git dir, which is what the checkpoint budget measures.
    fn shadow_dir_bytes(&self) -> u64 {
        dir_bytes(&self.git_dir)
    }
}

#[derive(Debug)]
struct UserRepoState {
    status: Vec<String>,
    head: String,
    branch: String,
    refs: Vec<String>,
    index_mtime: Option<SystemTime>,
    git_dir_entries: Vec<String>,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

fn sorted_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

fn write_files(sandbox: &Sandbox, prefix: &str, count: usize) {
    for index in 0..count {
        sandbox.write(&format!("{prefix}{index:04}.rs"), &format!("// {index}\n"));
    }
}

// ---------------------------------------------------------------------------
// Invariant 6: the shadow repository must not touch the user's repository.
// ---------------------------------------------------------------------------

#[test]
fn invariant_shadow_git_never_touches_user_repo() {
    let sandbox = Sandbox::new(true);
    let before = sandbox.user_repo_state();
    assert!(
        !before.status.is_empty(),
        "the user repo should start out dirty, otherwise the test proves nothing"
    );

    let first = sandbox.snapshot("checkpoint 1");
    sandbox.write("agent-writes.txt", "second\n");
    sandbox.write("agent-adds.txt", "new\n");
    let _second = sandbox.snapshot("checkpoint 2");
    sandbox.restore(first, false);

    let after = sandbox.user_repo_state();

    // The agent's own writes legitimately appear in the user's status as untracked files; what
    // invariant 6 forbids is the shadow repository changing anything else.
    let agent_files = ["agent-writes.txt", "agent-adds.txt"];
    let without_agent_files = |status: &[String]| -> Vec<String> {
        status
            .iter()
            .filter(|line| !agent_files.iter().any(|name| line.contains(name)))
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

    assert_eq!(sandbox.read("staged.txt").as_deref(), Some("staged\n"));
    assert_eq!(sandbox.read("unstaged.txt").as_deref(), Some("unstaged\n"));
    assert_eq!(
        sandbox.read("committed.txt").as_deref(),
        Some("committed\n")
    );
}

/// `RepositoryInitOptions::workdir_path` documents that it creates a `.git` gitlink in the work
/// tree. Planting one in the user's workspace would corrupt a non-git workspace and collide with a
/// real `.git` directory, so this pins the `set_workdir(.., false)` route instead.
#[test]
fn no_gitlink_is_planted_in_the_user_workspace() {
    let plain = Sandbox::new(false);
    assert!(!plain.workspace.join(".git").exists());
    plain.snapshot("checkpoint");
    assert!(
        !plain.workspace.join(".git").exists(),
        "the shadow repository planted a .git gitlink in a workspace that had none"
    );

    let with_repo = Sandbox::new(true);
    let entries_before = sorted_entries(&with_repo.workspace.join(".git"));
    with_repo.snapshot("checkpoint");
    assert!(
        with_repo.workspace.join(".git").is_dir(),
        "the user's .git must stay a directory, not be replaced by a gitlink file"
    );
    assert_eq!(
        entries_before,
        sorted_entries(&with_repo.workspace.join(".git")),
        "the shadow repository added or removed entries inside the user's .git"
    );
}

#[test]
fn shadow_repo_does_not_track_the_users_git_directory() {
    let sandbox = Sandbox::new(true);
    sandbox.write("agent-writes.txt", "x\n");
    let commit = sandbox.snapshot("checkpoint");

    let tracked = sandbox.tracked_files(commit);
    assert!(
        tracked.iter().any(|file| file == "agent-writes.txt"),
        "the agent's file should be tracked: {tracked:?}"
    );
    assert!(
        !tracked.iter().any(|file| file.starts_with(".git/")),
        "the shadow repo must never track the user's .git directory: {tracked:?}"
    );
}

// ---------------------------------------------------------------------------
// Snapshot / restore semantics.
// ---------------------------------------------------------------------------

#[test]
fn restore_rolls_back_tracked_files_and_leaves_never_tracked_ones_alone() {
    let sandbox = Sandbox::new(false);
    sandbox.write("file.txt", "version 1\n");
    let first = sandbox.snapshot("checkpoint 1");

    sandbox.write("file.txt", "version 2\n");
    sandbox.write("later-tracked.txt", "added then snapshotted\n");
    let _second = sandbox.snapshot("checkpoint 2");
    sandbox.write("never-tracked.txt", "created after the last checkpoint\n");

    sandbox.restore(first, false);

    assert_eq!(
        sandbox.read("file.txt").as_deref(),
        Some("version 1\n"),
        "restore did not roll the file back"
    );
    assert_eq!(
        sandbox.read("later-tracked.txt"),
        None,
        "a file the later checkpoint tracked must be removed by the rollback"
    );
    assert_eq!(
        sandbox.read("never-tracked.txt").as_deref(),
        Some("created after the last checkpoint\n"),
        "a default restore must not delete files no checkpoint ever tracked"
    );
}

/// The `--purge` half of `RestoreOptions`: explicit, approval-gated, and it does remove files that
/// no checkpoint ever tracked.
#[test]
fn purge_restore_also_removes_never_tracked_files() {
    let sandbox = Sandbox::new(false);
    sandbox.write("file.txt", "version 1\n");
    let first = sandbox.snapshot("checkpoint 1");

    sandbox.write("file.txt", "version 2\n");
    sandbox.write("never-tracked.txt", "created after the last checkpoint\n");

    sandbox.restore(first, true);

    assert_eq!(sandbox.read("file.txt").as_deref(), Some("version 1\n"));
    assert_eq!(
        sandbox.read("never-tracked.txt"),
        None,
        "purge must remove files that no checkpoint ever tracked"
    );
}

#[test]
fn diff_between_checkpoints_lists_the_changed_files() {
    let sandbox = Sandbox::new(false);
    sandbox.write("a.txt", "1\n");
    sandbox.write("b.txt", "1\n");
    let first = sandbox.snapshot("checkpoint 1");

    sandbox.write("a.txt", "2\n");
    sandbox.write("c.txt", "new\n");
    let second = sandbox.snapshot("checkpoint 2");

    assert_eq!(sandbox.changed_paths(first, second), vec!["a.txt", "c.txt"]);

    let repo = sandbox.open_shadow();
    let old = repo.find_commit(first).expect("from").tree().expect("tree");
    let new = repo.find_commit(second).expect("to").tree().expect("tree");
    let diff = repo
        .diff_tree_to_tree(Some(&old), Some(&new), None)
        .expect("diff");
    let stats = diff.stats().expect("diff stats");
    assert_eq!(stats.files_changed(), 2);
    assert!(
        stats.insertions() >= 2,
        "insertions: {}",
        stats.insertions()
    );
}

#[test]
fn ignore_rules_keep_excluded_paths_out_of_the_shadow_repo() {
    let sandbox = Sandbox::new(false);
    // Stands in for the large-file and build-artifact exclusions CheckpointStore will configure.
    sandbox.add_extra_ignore("big/\n*.blob\n");

    sandbox.write("small.txt", "keep me\n");
    sandbox.write("big/blob.bin", "x");
    sandbox.write("model.blob", "y");
    let commit = sandbox.snapshot("checkpoint");

    assert_eq!(sandbox.tracked_files(commit), vec!["small.txt".to_owned()]);
    assert!(
        sandbox.exists("big/blob.bin") && sandbox.exists("model.blob"),
        "excluding a file must not delete it from the workspace"
    );
}

#[test]
fn checkpoint_history_and_shadow_size_are_queryable_for_the_budget() {
    let sandbox = Sandbox::new(false);
    for round in 0..5 {
        sandbox.write("file.txt", &format!("round {round}\n"));
        sandbox.snapshot(&format!("checkpoint {round}"));
    }

    let repo = sandbox.open_shadow();
    let mut walk = repo.revwalk().expect("revwalk");
    walk.push_head().expect("push head");
    assert_eq!(walk.count(), 5, "every snapshot must be a commit");

    let bytes = sandbox.shadow_dir_bytes();
    assert!(bytes > 0, "the shadow git dir should hold objects");
    // Budget enforcement (ADR-0006) compares this number against a limit; the threshold logic and
    // GC land in M2. Walking the directory must stay cheap enough to do per snapshot.
    println!("shadow git dir after 5 snapshots: {bytes} bytes");
}

#[test]
fn snapshot_of_many_files_is_fast_enough() {
    let sandbox = Sandbox::new(false);
    const FILES: usize = 500;
    write_files(&sandbox, "src/file", FILES);

    let started = Instant::now();
    let cold = sandbox.snapshot("cold snapshot");
    let cold_elapsed = started.elapsed();

    // The common case in practice: a handful of edits on top of an existing checkpoint.
    for index in 0..10 {
        sandbox.write(
            &format!("src/file{index:04}.rs"),
            &format!("// edited {index}\n"),
        );
    }
    let started = Instant::now();
    let warm = sandbox.snapshot("warm snapshot");
    let warm_elapsed = started.elapsed();

    let started = Instant::now();
    sandbox.restore(cold, false);
    let restore_elapsed = started.elapsed();

    assert_eq!(sandbox.tracked_files(warm).len(), FILES);
    assert_eq!(sandbox.read("src/file0000.rs").as_deref(), Some("// 0\n"));

    // Generous bounds: CI machines are slow and shared. The measurements are the point.
    assert!(
        cold_elapsed.as_secs() < 60,
        "cold snapshot of {FILES} files took {cold_elapsed:?}"
    );
    assert!(
        warm_elapsed.as_secs() < 60,
        "warm snapshot took {warm_elapsed:?}"
    );
    assert!(
        restore_elapsed.as_secs() < 60,
        "restore took {restore_elapsed:?}"
    );
    println!(
        "measured: cold snapshot of {FILES} files {cold_elapsed:?}, warm snapshot after 10 edits \
         {warm_elapsed:?}, hard reset restore {restore_elapsed:?}"
    );
}

#[test]
fn workspace_without_a_user_repo_works() {
    let sandbox = Sandbox::new(false);
    assert!(!sandbox.workspace.join(".git").exists());
    sandbox.write("file.txt", "v1\n");
    let first = sandbox.snapshot("checkpoint 1");
    sandbox.write("file.txt", "v2\n");
    sandbox.snapshot("checkpoint 2");
    sandbox.restore(first, false);
    assert_eq!(sandbox.read("file.txt").as_deref(), Some("v1\n"));
}

// ---------------------------------------------------------------------------
// Reading the user's repository: what is safe for background features to do.
// ---------------------------------------------------------------------------

/// The prompt `environment` section wants to know whether the workspace is a git repository
/// (docs/design/platform.md §2.1). With the CLI backend `git status` rewrote the user's index; this
/// measures whether libgit2's `statuses()` does the same. A background feature must never dirty the
/// user's repository.
#[test]
fn reading_user_status_does_not_rewrite_their_index() {
    let sandbox = Sandbox::new(true);
    // Make a tracked file's cached stat info stale so a status refresh has something to do.
    sandbox.write("committed.txt", "modified behind git's back\n");
    settle();

    let before = sandbox
        .user_index_mtime()
        .expect("the user repo has an index");

    let repo = sandbox.open_user_repo();
    let mut options = StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    // Scoped so the borrow of `repo` ends before we drop it.
    let entries = {
        let statuses = repo.statuses(Some(&mut options)).expect("statuses");
        statuses.len()
    };
    assert!(
        entries > 0,
        "the dirty user repo should report at least one entry"
    );
    drop(repo);
    settle();

    assert_eq!(
        before,
        sandbox
            .user_index_mtime()
            .expect("the user repo has an index"),
        "reading statuses through libgit2 rewrote the user's .git/index"
    );

    // The read-only queries the environment section needs must be side-effect free too.
    let repo = sandbox.open_user_repo();
    let _ = repo.head().ok().and_then(|head| head.target());
    let _ = repo.references().map(|refs| refs.count());
    let _ = repo.revparse_single("HEAD");
    drop(repo);
    settle();
    assert_eq!(
        before,
        sandbox.user_index_mtime().expect("index"),
        "read-only repository queries rewrote the user's index"
    );
}

/// Sleeps briefly so that an index rewrite produces a distinguishable mtime even on filesystems
/// with coarse timestamp granularity.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(20));
}
