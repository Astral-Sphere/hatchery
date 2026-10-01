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

#[derive(Default)]
struct MemoryTree {
    files: BTreeMap<String, String>,
    dirs: BTreeSet<String>,
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

    /// The seam-bound backend.
    pub fn fs(&self) -> &hatchery_capabilities::LocalFs {
        &self.fs
    }

    /// The canonical root, for tests that assert against absolute paths.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Default for TempWorkspace {
    fn default() -> Self {
        Self::new()
    }
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
