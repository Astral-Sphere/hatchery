//! Shadow-git spike: measured behaviour of `git --git-dir <shadow> --work-tree <user workspace>`,
//! kept as permanent regression tests (ADR-0006, docs/design/testing.md §3.5).
//!
//! The point of this file is invariant 6 — checkpoints must never touch the user's own repository:
//! not its HEAD, not its index, not its refs, not its untracked files. Everything here runs
//! against a real `git` binary in a tempdir with a hermetic environment (fake `HOME`, no system
//! or global config), so the tests neither read nor write the developer's git setup.
//!
//! The M0 verdict derived from these measurements is recorded in docs/worklog/capabilities.md.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use tempfile::TempDir;

/// A shadow repository living outside the user's workspace, plus the user workspace itself.
struct Sandbox {
    // Held to keep the tree alive; the underscore keeps dead_code quiet.
    _dir: TempDir,
    root: PathBuf,
    workspace: PathBuf,
    git_dir: PathBuf,
}

impl Sandbox {
    /// Creates `<tmp>/{home,workspace,shadow}`; the workspace is a git repository with dirty
    /// state when `user_repo` is set.
    fn new(user_repo: bool) -> Self {
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = dir.path().to_path_buf();
        let workspace = root.join("workspace");
        let home = root.join("home");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::create_dir_all(&home).expect("create fake home");
        // An empty global config file: GIT_CONFIG_GLOBAL must point somewhere that exists, and
        // /dev/null is not portable to Windows.
        std::fs::write(home.join("empty-gitconfig"), b"").expect("write empty gitconfig");

        let sandbox = Self {
            _dir: dir,
            git_dir: root.join("shadow").join("repo.git"),
            root,
            workspace,
        };
        if user_repo {
            sandbox.make_user_repo();
        }
        sandbox
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// Environment shared by every git invocation: no system config, no global config, no
    /// developer identity, nothing inherited from the machine running the tests.
    fn apply_env(&self, cmd: &mut Command) {
        cmd.current_dir(&self.workspace)
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home().join("empty-gitconfig"))
            .env("GIT_AUTHOR_NAME", "hatchery-spike")
            .env("GIT_AUTHOR_EMAIL", "spike@localhost")
            .env("GIT_COMMITTER_NAME", "hatchery-spike")
            .env("GIT_COMMITTER_EMAIL", "spike@localhost")
            // Never let an interactive prompt hang the test run.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never");
    }

    /// Runs git as the *user* would: inside the workspace, against the workspace's own `.git`.
    fn user_git(&self, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        self.apply_env(&mut cmd);
        cmd.args(args);
        let output = cmd.output().expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("utf-8 stdout")
    }

    /// Runs git against the shadow repository with the user's workspace as its work tree.
    fn shadow_git(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new("git");
        self.apply_env(&mut cmd);
        cmd.arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.workspace)
            .args(args);
        cmd.output().expect("spawn git")
    }

    fn shadow_ok(&self, args: &[&str]) -> String {
        let output = self.shadow_git(args);
        assert!(
            output.status.success(),
            "shadow git {args:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("utf-8 stdout")
    }

    /// A user repository with staged, unstaged and untracked changes plus one commit.
    fn make_user_repo(&self) {
        self.user_git(&["init", "--initial-branch=main"]);
        self.write("committed.txt", "committed\n");
        self.user_git(&["add", "committed.txt"]);
        self.user_git(&["commit", "-m", "initial"]);
        self.write("staged.txt", "staged\n");
        self.user_git(&["add", "staged.txt"]);
        self.write("unstaged.txt", "unstaged\n");
        self.write("untracked.txt", "untracked\n");
        // Materialise and refresh the index so its mtime is meaningful before we start.
        self.user_git(&["status", "--porcelain"]);
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

    /// Everything about the user's repository that a shadow operation must not change.
    ///
    /// Note this itself runs git in the user's repository, and `git status` rewrites the index —
    /// so index mtime is captured separately by [`Sandbox::user_index_mtime`], which touches no
    /// git at all.
    fn user_repo_state(&self) -> UserRepoState {
        UserRepoState {
            status: self.user_git(&["status", "--porcelain"]),
            head: self.user_git(&["rev-parse", "HEAD"]),
            branch: self.user_git(&["rev-parse", "--abbrev-ref", "HEAD"]),
            refs: self.user_git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            git_dir_entries: sorted_entries(&self.workspace.join(".git")),
        }
    }

    /// Modification time of the user's index, read without running git.
    fn user_index_mtime(&self) -> Option<std::time::SystemTime> {
        metadata_mtime(&self.workspace.join(".git").join("index"))
    }

    /// Initialises the shadow repository against this workspace.
    fn init_shadow(&self) {
        std::fs::create_dir_all(self.git_dir.parent().expect("shadow parent")).expect("mkdir");
        self.shadow_ok(&["init"]);
        // Never track the user's own .git directory, whatever git's default behaviour is.
        let info = self.git_dir.join("info");
        std::fs::create_dir_all(&info).expect("create info dir");
        std::fs::write(info.join("exclude"), b".git/\n").expect("write exclude");
        self.shadow_ok(&["config", "user.name", "hatchery-spike"]);
        self.shadow_ok(&["config", "user.email", "spike@localhost"]);
    }

    /// Takes a checkpoint and returns its commit id.
    fn snapshot(&self, label: &str) -> String {
        self.shadow_ok(&["add", "-A"]);
        self.shadow_ok(&["commit", "-m", label, "--allow-empty"]);
        self.shadow_ok(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn tree_files(&self, commit: &str) -> Vec<String> {
        self.shadow_ok(&["ls-tree", "-r", "--name-only", commit])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[derive(Debug)]
struct UserRepoState {
    status: String,
    head: String,
    branch: String,
    refs: String,
    git_dir_entries: Vec<String>,
}

fn metadata_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
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

    // Captured without running git, because `git status` itself rewrites the index (see
    // `status_rewrites_the_user_index_but_plumbing_commands_do_not`): measuring the index with a
    // git command would measure the measurement.
    let index_before = sandbox.user_index_mtime();

    sandbox.init_shadow();
    sandbox.write("agent-writes.txt", "first\n");
    let first = sandbox.snapshot("checkpoint 1");
    sandbox.write("agent-writes.txt", "second\n");
    sandbox.write("agent-adds.txt", "new\n");
    let _second = sandbox.snapshot("checkpoint 2");
    sandbox.shadow_ok(&["reset", "--hard", &first]);

    let index_after = sandbox.user_index_mtime();
    assert_eq!(
        index_before, index_after,
        "the shadow repository rewrote the user's index"
    );

    let after = sandbox.user_repo_state();

    // The agent's own file writes legitimately show up in the user's status as untracked files;
    // what invariant 6 forbids is the shadow repository changing anything else.
    let agent_files = ["agent-writes.txt", "agent-adds.txt"];
    let without_agent_files = |status: &str| {
        status
            .lines()
            .filter(|line| !agent_files.iter().any(|name| line.contains(name)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        without_agent_files(&before.status),
        without_agent_files(&after.status),
        "the user's `git status` changed beyond the agent's own file writes"
    );
    assert_eq!(before.head, after.head, "user HEAD moved");
    assert_eq!(before.branch, after.branch, "user branch changed");
    assert_eq!(before.refs, after.refs, "user refs changed");
    assert_eq!(
        before.git_dir_entries, after.git_dir_entries,
        "the user's .git directory gained or lost entries"
    );

    // The user's dirty files must still hold the user's content.
    assert_eq!(sandbox.read("staged.txt").as_deref(), Some("staged\n"));
    assert_eq!(sandbox.read("unstaged.txt").as_deref(), Some("unstaged\n"));
    assert_eq!(
        sandbox.read("untracked.txt").as_deref(),
        Some("untracked\n")
    );
    assert_eq!(
        sandbox.read("committed.txt").as_deref(),
        Some("committed\n")
    );
}

#[test]
fn shadow_repo_does_not_track_the_users_git_directory() {
    let sandbox = Sandbox::new(true);
    sandbox.init_shadow();
    sandbox.write("agent-writes.txt", "x\n");
    let commit = sandbox.snapshot("checkpoint");

    let tracked = sandbox.tree_files(&commit);
    assert!(
        tracked.iter().any(|f| f == "agent-writes.txt"),
        "the agent's file should be tracked: {tracked:?}"
    );
    assert!(
        !tracked.iter().any(|f| f.starts_with(".git/")),
        "the shadow repo must never track the user's .git directory: {tracked:?}"
    );
}

// ---------------------------------------------------------------------------
// Snapshot / restore semantics.
// ---------------------------------------------------------------------------

#[test]
fn restore_brings_back_modified_files_and_removes_later_tracked_ones() {
    let sandbox = Sandbox::new(false);
    sandbox.init_shadow();

    sandbox.write("file.txt", "version 1\n");
    let first = sandbox.snapshot("checkpoint 1");

    sandbox.write("file.txt", "version 2\n");
    sandbox.write("later-tracked.txt", "added then snapshotted\n");
    let _second = sandbox.snapshot("checkpoint 2");
    sandbox.write("never-tracked.txt", "created after the last checkpoint\n");

    sandbox.shadow_ok(&["reset", "--hard", &first]);

    assert_eq!(
        sandbox.read("file.txt").as_deref(),
        Some("version 1\n"),
        "restore did not roll the file back"
    );
    assert_eq!(
        sandbox.read("later-tracked.txt"),
        None,
        "a file that the later checkpoint tracked must be removed by the rollback"
    );
    // Measured behaviour, and the reason `rewind` needs an explicit policy: `reset --hard` only
    // touches files the shadow index knows about.
    assert_eq!(
        sandbox.read("never-tracked.txt").as_deref(),
        Some("created after the last checkpoint\n"),
        "reset --hard unexpectedly deleted a file that was never checkpointed"
    );
}

#[test]
fn diff_between_checkpoints_lists_the_changed_files() {
    let sandbox = Sandbox::new(false);
    sandbox.init_shadow();

    sandbox.write("a.txt", "1\n");
    sandbox.write("b.txt", "1\n");
    let first = sandbox.snapshot("checkpoint 1");

    sandbox.write("a.txt", "2\n");
    sandbox.write("c.txt", "new\n");
    let second = sandbox.snapshot("checkpoint 2");

    let names = sandbox.shadow_ok(&["diff", "--name-only", &first, &second]);
    let mut names: Vec<&str> = names
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    names.sort_unstable();
    assert_eq!(names, vec!["a.txt", "c.txt"]);

    let patch = sandbox.shadow_ok(&["diff", &first, &second]);
    assert!(
        patch.contains("-1\n") || patch.contains("-1"),
        "patch lacks the old line"
    );
    assert!(patch.contains("+2"), "patch lacks the new line");
}

#[test]
fn exclude_rules_keep_large_files_out_of_the_shadow_repo() {
    let sandbox = Sandbox::new(false);
    sandbox.init_shadow();

    // The checkpoint store will write per-workspace exclude rules into <git-dir>/info/exclude.
    let info = sandbox.git_dir.join("info");
    std::fs::create_dir_all(&info).expect("create info dir");
    std::fs::write(info.join("exclude"), b".git/\nbig/\n*.blob\n").expect("write exclude");

    sandbox.write("small.txt", "keep me\n");
    sandbox.write("big/blob.bin", "x");
    sandbox.write("model.blob", "y");
    let commit = sandbox.snapshot("checkpoint");

    let tracked = sandbox.tree_files(&commit);
    assert_eq!(tracked, vec!["small.txt".to_owned()]);
    assert!(
        sandbox.exists("big/blob.bin") && sandbox.exists("model.blob"),
        "excluding a file must not delete it from the workspace"
    );
}

#[test]
fn checkpoint_count_and_repo_size_are_queryable_for_the_budget() {
    let sandbox = Sandbox::new(false);
    sandbox.init_shadow();

    for round in 0..5 {
        sandbox.write("file.txt", &format!("round {round}\n"));
        sandbox.snapshot(&format!("checkpoint {round}"));
    }

    let commits = sandbox.shadow_ok(&["rev-list", "--count", "HEAD"]);
    assert_eq!(commits.trim(), "5", "every snapshot must be a commit");

    let count_objects = sandbox.shadow_ok(&["count-objects", "-vH"]);
    let size_pack = count_objects
        .lines()
        .find_map(|line| line.strip_prefix("size:"))
        .expect("count-objects reports a size");
    assert!(!size_pack.trim().is_empty(), "size is reported");

    // Budget enforcement (ADR-0006) needs a size it can compare against a limit; this asserts the
    // query works and shows the number, the threshold logic itself lands in M2.
    println!("shadow repo after 5 snapshots: size{size_pack}");
    println!("{count_objects}");
}

#[test]
fn snapshot_of_many_files_is_fast_enough() {
    let sandbox = Sandbox::new(false);
    sandbox.init_shadow();

    const FILES: usize = 500;
    for index in 0..FILES {
        sandbox.write(&format!("src/file{index:04}.rs"), &format!("// {index}\n"));
    }

    let started = Instant::now();
    let first = sandbox.snapshot("cold snapshot");
    let cold = started.elapsed();

    // The common case in practice: a handful of edits on top of an existing checkpoint.
    for index in 0..10 {
        sandbox.write(
            &format!("src/file{index:04}.rs"),
            &format!("// edited {index}\n"),
        );
    }
    let started = Instant::now();
    let second = sandbox.snapshot("warm snapshot");
    let warm = started.elapsed();

    let started = Instant::now();
    sandbox.shadow_ok(&["reset", "--hard", &first]);
    let restore = started.elapsed();

    assert_eq!(sandbox.tree_files(&second).len(), FILES);
    assert_eq!(sandbox.read("src/file0000.rs").as_deref(), Some("// 0\n"));

    // Generous bounds: CI machines are slow and shared. The measurements are the point.
    assert!(
        cold.as_secs() < 60,
        "cold snapshot of {FILES} files took {cold:?}"
    );
    assert!(warm.as_secs() < 60, "warm snapshot took {warm:?}");
    assert!(restore.as_secs() < 60, "restore took {restore:?}");

    println!(
        "measured: cold snapshot of {FILES} files {cold:?}, warm snapshot after 10 edits \
         {warm:?}, reset --hard restore {restore:?}"
    );
}

#[test]
fn workspace_without_a_user_repo_works() {
    let sandbox = Sandbox::new(false);
    assert!(
        !sandbox.workspace.join(".git").exists(),
        "this scenario needs a workspace that is not a git repository"
    );
    sandbox.init_shadow();
    sandbox.write("file.txt", "v1\n");
    let first = sandbox.snapshot("checkpoint 1");
    sandbox.write("file.txt", "v2\n");
    sandbox.snapshot("checkpoint 2");
    sandbox.shadow_ok(&["reset", "--hard", &first]);
    assert_eq!(sandbox.read("file.txt").as_deref(), Some("v1\n"));
}

// ---------------------------------------------------------------------------
// Which git commands are safe to run inside the user's repository.
// ---------------------------------------------------------------------------

/// Measured, and load-bearing for two features: the prompt `environment` section wants to know
/// whether the workspace is a git repository (docs/design/platform.md §2.1), and tools may run git
/// on the user's behalf. Plumbing commands leave `.git/index` alone; `git status` rewrites it as
/// soon as a tracked file's stat info is stale. Background features must therefore stick to
/// plumbing — a prompt assembler has no business dirtying the user's index.
#[test]
fn status_rewrites_the_user_index_but_plumbing_commands_do_not() {
    let sandbox = Sandbox::new(true);
    // Make a tracked file's cached stat info stale so `git status` has something to refresh.
    sandbox.write("committed.txt", "modified behind git's back\n");
    settle();

    let before = sandbox
        .user_index_mtime()
        .expect("the user repo has an index");
    let plumbing: Vec<&[&str]> = vec![
        &["rev-parse", "HEAD"],
        &["rev-parse", "--abbrev-ref", "HEAD"],
        &["rev-parse", "--is-inside-work-tree"],
        &["for-each-ref"],
        &["log", "--oneline"],
        &["ls-files"],
        &["diff", "--stat"],
    ];
    for args in plumbing {
        sandbox.user_git(args);
        settle();
        assert_eq!(
            sandbox.user_index_mtime().expect("index"),
            before,
            "`git {args:?}` rewrote the user's index"
        );
    }

    sandbox.user_git(&["status", "--porcelain"]);
    settle();
    assert_ne!(
        sandbox.user_index_mtime().expect("index"),
        before,
        "expected `git status --porcelain` to refresh and rewrite the user's index"
    );
}

/// Sleeps briefly so that an index rewrite produces a distinguishable mtime even on filesystems
/// with coarse timestamp granularity.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(20));
}
