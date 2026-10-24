//! In-memory and temp-dir filesystems for tests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use async_trait::async_trait;

use hatchery_capabilities::{FsBackend, FsEntry, FsError, FsMetadata};

/// A filesystem in a `BTreeMap`: deterministic iteration, no disk, no cleanup.
///
/// Paths are workspace-relative and normalised on the way in, mirroring `LocalFs`'s contract
/// (`..` refused, absolute refused) so a tool cannot learn to depend on leniency one backend
/// does not have.
#[derive(Default)]
pub struct MemoryFs {
    inner: RwLock<MemoryTree>,
}

struct MemoryTree {
    files: BTreeMap<String, String>,
    dirs: BTreeSet<String>,
}

impl Default for MemoryTree {
    fn default() -> Self {
        // The root always exists. `LocalFs`'s root is a directory it was handed, so `read_dir("")`
        // answers for a workspace that only ever had files written into it; without this, a
        // seam-written top-level file would be invisible to `read_dir("")` here and visible there.
        Self {
            files: BTreeMap::new(),
            dirs: BTreeSet::from([String::new()]),
        }
    }
}

impl MemoryFs {
    /// An empty filesystem.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a directory (and its parents).
    pub fn dir(&self, path: &str) -> &Self {
        let mut tree = self.inner.write().expect("memory fs is not poisoned");
        let normalised = normalise(path).expect("test paths are well-formed");
        let mut walked = String::new();
        tree.dirs.insert(String::new());
        for part in normalised.split('/') {
            if walked.is_empty() {
                walked.push_str(part);
            } else {
                walked.push('/');
                walked.push_str(part);
            }
            tree.dirs.insert(walked.clone());
        }
        self
    }

    /// Creates a file with text content (and its parent directories).
    pub fn file(&self, path: &str, content: impl Into<String>) -> &Self {
        {
            let mut tree = self.inner.write().expect("memory fs is not poisoned");
            let normalised = normalise(path).expect("test paths are well-formed");
            tree.files.insert(normalised.clone(), content.into());
        }
        if let Some((parent, _)) = path.rsplit_once('/') {
            self.dir(parent);
        } else {
            self.dir("");
        }
        self
    }

    /// Whether a file exists.
    #[must_use]
    pub fn has_file(&self, path: &str) -> bool {
        normalise(path).is_ok_and(|p| {
            self.inner
                .read()
                .expect("memory fs is not poisoned")
                .files
                .contains_key(&p)
        })
    }

    fn lookup(&self, path: &str) -> Result<String, FsError> {
        let normalised = normalise(path)?;
        let tree = self.inner.read().expect("memory fs is not poisoned");
        if tree.files.contains_key(&normalised) {
            return Ok(normalised);
        }
        if tree.dirs.contains(&normalised) {
            return Err(FsError::WrongKind(format!("{path} is a directory")));
        }
        // A file whose ancestor was never declared as a directory is simply absent.
        Err(FsError::NotFound(path.to_owned()))
    }
}

/// Refuses everything `LocalFs` refuses before touching its tree: absolute paths and `..`.
fn normalise(path: &str) -> Result<String, FsError> {
    if path.starts_with('/') {
        return Err(FsError::OutsideWorkspace(format!(
            "{path} is absolute; paths are workspace-relative"
        )));
    }
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(FsError::OutsideWorkspace(format!(
                    "{path} climbs out of the workspace"
                )));
            }
            part => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

#[async_trait]
impl FsBackend for MemoryFs {
    async fn read_text_file(&self, path: &str) -> Result<String, FsError> {
        let key = self.lookup(path)?;
        let content = self
            .inner
            .read()
            .expect("memory fs is not poisoned")
            .files
            .get(&key)
            .cloned()
            .unwrap_or_default();
        // The refusal `LocalFs` performs with a byte sniff, mirrored so tools cannot grow
        // behaviour that only shows up on one backend.
        if content.contains('\0') {
            return Err(FsError::Binary(path.to_owned()));
        }
        Ok(content)
    }

    async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, FsError> {
        let normalised = normalise(path)?;
        let tree = self.inner.read().expect("memory fs is not poisoned");
        if !tree.dirs.contains(&normalised) {
            if tree.files.contains_key(&normalised) {
                return Err(FsError::WrongKind(format!("{path} is not a directory")));
            }
            return Err(FsError::NotFound(path.to_owned()));
        }
        let prefix = if normalised.is_empty() {
            String::new()
        } else {
            format!("{normalised}/")
        };
        let mut entries: BTreeMap<String, bool> = BTreeMap::new();
        for name in tree.files.keys().chain(tree.dirs.iter()) {
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            let (head, tail) = rest
                .split_once('/')
                .map_or((rest, None), |(h, t)| (h, Some(t)));
            let is_dir = tail.is_some() || tree.dirs.contains(name);
            entries.entry(head.to_owned()).or_insert(is_dir);
        }
        Ok(entries
            .into_iter()
            .map(|(name, is_dir)| FsEntry { name, is_dir })
            .collect())
    }

    async fn metadata(&self, path: &str) -> Result<FsMetadata, FsError> {
        let normalised = normalise(path)?;
        let tree = self.inner.read().expect("memory fs is not poisoned");
        if tree.dirs.contains(&normalised) {
            return Ok(FsMetadata {
                is_dir: true,
                is_file: false,
                len: 0,
            });
        }
        if let Some(content) = tree.files.get(&normalised) {
            return Ok(FsMetadata {
                is_dir: false,
                is_file: true,
                len: content.len() as u64,
            });
        }
        Err(FsError::NotFound(path.to_owned()))
    }

    async fn write_text_file(&self, path: &str, contents: &str) -> Result<(), FsError> {
        let normalised = normalise(path)?;
        if normalised.is_empty() {
            return Err(FsError::WrongKind(
                "the empty path is the workspace itself, not a file".to_owned(),
            ));
        }
        let mut tree = self.inner.write().expect("memory fs is not poisoned");
        if tree.dirs.contains(&normalised) {
            return Err(FsError::WrongKind(format!("{path} is a directory")));
        }
        // Every ancestor becomes a directory, mirroring `LocalFs`'s `create_dir_all`: a backend
        // that disagreed about missing parents would let a tool grow behaviour that only exists on
        // one of them.
        let mut ancestor = String::new();
        for part in normalised.split('/') {
            if !ancestor.is_empty() {
                tree.dirs.insert(ancestor.clone());
            }
            ancestor = if ancestor.is_empty() {
                part.to_owned()
            } else {
                format!("{ancestor}/{part}")
            };
        }
        tree.files.insert(normalised, contents.to_owned());
        Ok(())
    }
}

/// A real temp directory with a real `LocalFs`, for tests that must exercise the disk path
/// (canonicalisation, symlinks, permissions) — plus a writer for arranging fixtures.
pub struct TempWorkspace {
    _dir: tempfile::TempDir,
    fs: hatchery_capabilities::LocalFs,
    root: PathBuf,
}

impl TempWorkspace {
    /// Creates the workspace.
    ///
    /// # Panics
    ///
    /// Panics when the temp dir cannot be created or the root cannot be canonicalised: a test
    /// environment fault, not a case to branch on.
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let fs = hatchery_capabilities::LocalFs::new(&root).expect("canonical root");
        Self {
            _dir: dir,
            fs,
            root,
        }
    }

    /// The workspace-relative path of a fixture, as the seam spells it.
    pub fn path(&self, relative: &str) -> String {
        relative.to_owned()
    }

    /// Writes a fixture file (creating parents), directly on the disk — test setup, not a tool.
    ///
    /// # Panics
    ///
    /// Panics on write failure: a test that cannot arrange its own fixtures has nothing to test.
    pub fn write(&self, relative: &str, content: impl AsRef<[u8]>) {
        let target = self.root.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("mkdir -p");
        }
        std::fs::write(target, content).expect("write fixture");
    }

    /// Chainable [`Self::write`], for arranging a tree in one expression.
    pub fn file(&self, relative: &str, content: impl AsRef<[u8]>) -> &Self {
        self.write(relative, content);
        self
    }

    /// Creates a directory (and its parents).
    pub fn dir(&self, relative: &str) -> &Self {
        std::fs::create_dir_all(self.root.join(relative)).expect("mkdir -p");
        self
    }

    /// Stages one path in the workspace's own repository.
    ///
    /// # Panics
    ///
    /// Panics when the workspace is not a git repository — see [`Self::git`].
    pub fn stage(&self, relative: &str) -> &Self {
        let repo = self.user_repo();
        let mut index = repo.index().expect("user index");
        index
            .add_path(Path::new(relative))
            .unwrap_or_else(|error| panic!("staging {relative}: {error}"));
        index.write().expect("write index");
        self
    }

    /// Commits whatever is staged in the workspace's own repository.
    ///
    /// # Panics
    ///
    /// Panics when there is nothing to commit, or when the workspace is not a git repository.
    pub fn commit(&self, message: &str) -> &Self {
        let repo = self.user_repo();
        let mut index = repo.index().expect("user index");
        let tree = repo
            .find_tree(index.write_tree().expect("write tree"))
            .expect("find tree");
        let signature =
            git2::Signature::now("hatchery-testkit", "testkit@localhost").expect("signature");
        let parent = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .and_then(|oid| repo.find_commit(oid).ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .unwrap_or_else(|error| panic!("committing {message}: {error}"));
        self
    }

    /// The workspace's own repository, if it has one.
    #[must_use]
    pub fn repo(&self) -> Option<git2::Repository> {
        git2::Repository::open(&self.root).ok()
    }

    /// The workspace's own repository state, for the before/after comparison invariant 6 is.
    ///
    /// # Panics
    ///
    /// Panics when the workspace is not a git repository — see [`Self::git`].
    #[must_use]
    pub fn repo_state(&self) -> UserRepoState {
        user_repo_state(&self.root)
    }

    fn user_repo(&self) -> git2::Repository {
        self.repo()
            .expect("the workspace is a git repository; use TempWorkspace::git()")
    }

    /// The seam-bound backend.
    pub fn fs(&self) -> &hatchery_capabilities::LocalFs {
        &self.fs
    }

    /// The canonical root, for tests that assert against absolute paths.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl TempWorkspace {
    /// A workspace that is **also a user git repository**, and a dirty one.
    ///
    /// The state is the one invariant 6 needs to be interesting: one commit, one staged change, one
    /// unstaged file and one untracked file. If the workspace were clean, "the shadow repository
    /// never touched the user's repository" could pass by doing nothing at all.
    ///
    /// The repository is hardened the same way the shadow one is (`core.excludesFile` pointing
    /// nowhere, no autocrlf, no fsmonitor, fixed identity), because libgit2 reads the *developer's*
    /// global and system configuration and a test whose outcome depends on whose machine it runs on
    /// is not a test.
    ///
    /// # Panics
    ///
    /// Panics when git cannot initialise the repository.
    #[must_use]
    pub fn git() -> Self {
        let workspace = Self::new();
        let repo = git2::Repository::init(workspace.root()).expect("init the user's repository");
        {
            let mut config = repo.config().expect("repo config");
            config
                .set_str("user.name", "hatchery-testkit")
                .expect("name");
            config
                .set_str("user.email", "testkit@localhost")
                .expect("email");
            config.set_str("core.autocrlf", "false").expect("autocrlf");
            config
                .set_str("core.excludesFile", "/nonexistent/hatchery-excludes")
                .expect("excludesFile");
            config.set_bool("core.fsmonitor", false).expect("fsmonitor");
        }
        workspace
            .file("committed.txt", "committed\n")
            .stage("committed.txt")
            .commit("initial");
        workspace.file("staged.txt", "staged\n").stage("staged.txt");
        workspace.file("unstaged.txt", "unstaged\n");
        workspace.file("untracked.txt", "untracked\n");
        workspace
    }
}

impl Default for TempWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

/// Everything about a user's own repository that a shadow operation must not change.
///
/// The fields are the ones the shadow-git spike measured libgit2 could plausibly touch: HEAD, the
/// branch, every ref, the index's mtime (the `git status` *binary* rewrites it; libgit2's
/// `statuses()` does not, which is why the prompt can read the user's branch at all) and the `.git`
/// directory's entries — a planted gitlink file shows up there before it shows up anywhere else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRepoState {
    /// One line per status entry, flags then path, sorted: `git status`'s own order is not
    /// guaranteed, and a comparison that depended on it would flake.
    pub status: Vec<String>,
    /// HEAD's commit id, or `unborn`.
    pub head: String,
    /// HEAD's branch shorthand, or `none`.
    pub branch: String,
    /// Every ref, name and target, sorted.
    pub refs: Vec<String>,
    /// When `.git/index` was last modified.
    pub index_mtime: Option<std::time::SystemTime>,
    /// The names in `.git`, sorted.
    pub git_dir_entries: Vec<String>,
}

/// Reads a workspace's own repository state.
///
/// A free function as well as [`TempWorkspace::repo_state`]: the shadow-git spike builds its own
/// sandbox instead of a `TempWorkspace`, and invariant 6 only means something if both measure the
/// same fields the same way.
///
/// # Panics
///
/// Panics when `workspace` is not a git repository — see [`TempWorkspace::git`].
#[must_use]
pub fn user_repo_state(workspace: &Path) -> UserRepoState {
    let repo = git2::Repository::open(workspace).expect("the workspace is a git repository");
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let listed = repo.statuses(Some(&mut options)).expect("statuses");
    let mut status: Vec<String> = listed
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
        index_mtime: mtime(&workspace.join(".git").join("index")),
        git_dir_entries: sorted_entries(&workspace.join(".git")),
    }
}

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_tree_round_trips_through_the_seam() {
        let fs = MemoryFs::new();
        fs.dir("src/deep")
            .file("src/deep/main.rs", "fn main() {}")
            .file("README.md", "hi");

        assert_eq!(
            fs.read_text_file("src/deep/main.rs").await.expect("read"),
            "fn main() {}"
        );
        let root = fs.read_dir("").await.expect("root");
        let names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["README.md", "src"]);
        assert!(root[1].is_dir);
        let src = fs.read_dir("src").await.expect("src");
        assert_eq!(src.len(), 1);
        assert_eq!(src[0].name, "deep");
    }

    #[tokio::test]
    async fn the_same_refusals_as_local_fs() {
        let fs = MemoryFs::new();
        for attempt in ["/etc/passwd", "../x", "a/../../b"] {
            let error = fs.read_text_file(attempt).await.expect_err(attempt);
            assert!(
                matches!(error, FsError::OutsideWorkspace(_)),
                "{attempt}: {error}"
            );
        }
        let error = fs.read_text_file("missing").await.expect_err("missing");
        assert!(matches!(error, FsError::NotFound(_)));
    }

    #[tokio::test]
    async fn temp_workspace_exercises_the_real_disk() {
        let ws = TempWorkspace::new();
        ws.write("src/lib.rs", "pub fn f() {}\n");
        assert_eq!(
            ws.fs().read_text_file("src/lib.rs").await.expect("read"),
            "pub fn f() {}\n"
        );
        let meta = ws.fs().metadata("src").await.expect("dir meta");
        assert!(meta.is_dir);
    }
}
