//! The local filesystem backend: the real disk, behind the seam.
//!
//! Two rules make reads safe enough to hand a model (docs/design/capabilities.md §1/§5):
//!
//! * **Workspace-relative, twice over.** Paths are resolved lexically first (`..` cannot climb
//!   out), then resolved on disk with `canonicalize` — a symlink planted inside the workspace
//!   pointing out is caught by the second check, which the lexical one cannot see.
//! * **No text from binaries.** A NUL byte in the first kilobyte means the file is refused, not
//!   lossily mangled: a model that asks to read a PNG should hear so, not receive mojibake.
//!
//! Writes obey the same two rules and add a third problem of their own: a write target usually does
//! not exist yet, so it cannot be canonicalised. `resolve_write` therefore anchors on the deepest
//! ancestor that *does* resolve, which is also what keeps `create_dir_all` from building directories
//! on the far side of a symlinked ancestor.
//!
//! What this backend does **not** do is checkpoint. That is [`crate::CheckpointedFs`]'s job, so the
//! snapshot precedes a write through *any* backend and the local one stays a filesystem.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::fs::{FsEntry, FsError, FsMetadata};

/// The local disk, rooted at one workspace.
#[derive(Clone)]
pub struct LocalFs {
    root: Arc<PathBuf>,
}

impl LocalFs {
    /// Roots the backend at `root`.
    ///
    /// # Errors
    ///
    /// Fails when the root does not exist or is not a directory — an assembly-time fault the
    /// startup audit should catch, not a per-call surprise.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, FsError> {
        let root = root.into();
        let canonical = std::fs::canonicalize(&root)
            .map_err(|error| FsError::Io(format!("workspace root {}: {error}", root.display())))?;
        if !canonical.is_dir() {
            return Err(FsError::WrongKind(format!(
                "workspace root {} is not a directory",
                root.display()
            )));
        }
        Ok(Self {
            root: Arc::new(canonical),
        })
    }

    /// The canonical workspace root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a workspace-relative path to a real path inside the root.
    ///
    /// The two-stage check (lexical, then on-disk) is the whole security story: absolute paths
    /// and `..` are rejected before touching the disk, and symlink escapes after.
    fn resolve(&self, path: &str) -> Result<PathBuf, FsError> {
        if path.is_empty() {
            return Ok(self.root.as_ref().clone());
        }
        let joined = self.lexical(path)?;
        let resolved = std::fs::canonicalize(&joined).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => FsError::NotFound(path.to_owned()),
            _ => FsError::Io(format!("{path}: {error}")),
        })?;
        self.confine(&resolved, path)
    }

    /// The lexical half of path resolution: workspace-relative in, root-joined out.
    ///
    /// Shared with [`Self::resolve_write`], which cannot canonicalise its way through a path that
    /// does not exist yet.
    fn lexical(&self, path: &str) -> Result<PathBuf, FsError> {
        let relative = Path::new(path);
        if relative.is_absolute() {
            return Err(FsError::OutsideWorkspace(format!(
                "{path} is absolute; paths are workspace-relative"
            )));
        }
        let mut lexical = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => lexical.push(part),
                Component::CurDir => {}
                // `..` is refused outright rather than resolved: climbing is not a use case a
                // read tool needs, and resolving it inside the root would hide the intent.
                Component::ParentDir => {
                    return Err(FsError::OutsideWorkspace(format!(
                        "{path} climbs out of the workspace"
                    )));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(FsError::OutsideWorkspace(format!(
                        "{path} is absolute; paths are workspace-relative"
                    )));
                }
            }
        }
        Ok(self.root.join(&lexical))
    }

    /// Refuses a resolved path that is not inside the root.
    fn confine(&self, resolved: &Path, path: &str) -> Result<PathBuf, FsError> {
        if !resolved.starts_with(self.root.as_ref()) {
            return Err(FsError::OutsideWorkspace(format!(
                "{path} resolves to {} via a symlink, outside the workspace",
                resolved.display()
            )));
        }
        Ok(resolved.to_path_buf())
    }

    /// Resolves a path that may not exist yet, which is the normal case for a write.
    ///
    /// [`Self::resolve`] cannot be reused: `canonicalize` fails on a missing file, so the write
    /// path anchors on the deepest ancestor that *does* resolve and rebuilds the rest below it.
    /// Two escapes this has to close, both of which the lexical check cannot see:
    ///
    /// * a **symlinked ancestor** — `link/` pointing at `/etc` would let `create_dir_all` build
    ///   `/etc/new/` before anything was verified, so the ancestor is canonicalised *first* and
    ///   the missing components are only appended once it is inside the root;
    /// * a **dangling symlink** at the target — writing "through" it creates the file at the
    ///   link's destination, outside the workspace and outside every checkpoint, so an entry that
    ///   exists but does not resolve is refused rather than followed.
    fn resolve_write(&self, path: &str) -> Result<PathBuf, FsError> {
        let joined = self.lexical(path)?;

        if std::fs::symlink_metadata(&joined).is_ok() {
            let resolved = std::fs::canonicalize(&joined).map_err(|error| {
                FsError::Io(format!(
                    "{path} exists but does not resolve (a broken symlink?): {error}"
                ))
            })?;
            return self.confine(&resolved, path);
        }

        let name = joined
            .file_name()
            .ok_or_else(|| {
                FsError::WrongKind(format!("{path} is the workspace itself, not a file"))
            })?
            .to_os_string();
        let parent = joined.parent().unwrap_or(self.root.as_ref()).to_path_buf();
        let (ancestor, missing) = self.canonical_ancestor(&parent, path)?;
        let mut target = self.confine(&ancestor, path)?;
        for component in &missing {
            target.push(component);
        }
        target.push(name);
        Ok(target)
    }

    /// Canonicalises the deepest ancestor of `path` that resolves, returning it together with the
    /// component names below it that do not exist yet.
    fn canonical_ancestor(
        &self,
        path: &Path,
        original: &str,
    ) -> Result<(PathBuf, Vec<std::ffi::OsString>), FsError> {
        let mut candidate = path.to_path_buf();
        let mut missing: Vec<std::ffi::OsString> = Vec::new();
        loop {
            match std::fs::canonicalize(&candidate) {
                Ok(canonical) => {
                    missing.reverse();
                    return Ok((canonical, missing));
                }
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(FsError::Io(format!("{original}: {error}")));
                }
                Err(_) => {}
            }
            let name = candidate.file_name().ok_or_else(|| {
                FsError::NotFound(format!(
                    "{original} has no ancestor inside the workspace that exists"
                ))
            })?;
            missing.push(name.to_os_string());
            candidate.pop();
        }
    }

    fn sniff_text(resolved: &Path, path: &str) -> Result<(), FsError> {
        use std::io::Read;
        let mut file = std::fs::File::open(resolved).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => FsError::NotFound(path.to_owned()),
            _ => FsError::Io(format!("{path}: {error}")),
        })?;
        let mut head = [0_u8; 1024];
        let read = file
            .read(&mut head)
            .map_err(|error| FsError::Io(format!("{path}: {error}")))?;
        if head[..read].contains(&0) {
            return Err(FsError::Binary(path.to_owned()));
        }
        Ok(())
    }
}

#[async_trait]
impl crate::fs::FsBackend for LocalFs {
    async fn read_text_file(&self, path: &str) -> Result<String, FsError> {
        if path.is_empty() {
            return Err(FsError::WrongKind(
                "the empty path is the workspace itself, not a file".to_owned(),
            ));
        }
        let resolved = self.resolve(path)?;
        // A directory is a wrong kind, not an io accident: `File::open` succeeds on one and the
        // failure only surfaces mid-read as EISDIR, which says nothing a caller can act on.
        if tokio::fs::metadata(&resolved)
            .await
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
        {
            return Err(FsError::WrongKind(format!(
                "{path} is a directory, not a file"
            )));
        }
        Self::sniff_text(&resolved, path)?;
        tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|error| FsError::Io(format!("{path}: {error}")))
    }

    async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, FsError> {
        let resolved = self.resolve(path)?;
        let mut read =
            tokio::fs::read_dir(&resolved)
                .await
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => FsError::NotFound(path.to_owned()),
                    _ if error.kind() == std::io::ErrorKind::NotADirectory
                        || error.raw_os_error() == Some(20) =>
                    {
                        FsError::WrongKind(format!("{path} is not a directory"))
                    }
                    _ => FsError::Io(format!("{path}: {error}")),
                })?;
        let mut entries = Vec::new();
        while let Some(entry) = read
            .next_entry()
            .await
            .map_err(|error| FsError::Io(format!("{path}: {error}")))?
        {
            let is_dir = entry.file_type().await.is_ok_and(|kind| kind.is_dir());
            entries.push(FsEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir,
            });
        }
        Ok(entries)
    }

    async fn metadata(&self, path: &str) -> Result<FsMetadata, FsError> {
        let resolved = self.resolve(path)?;
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => FsError::NotFound(path.to_owned()),
                _ => FsError::Io(format!("{path}: {error}")),
            })?;
        Ok(FsMetadata {
            is_dir: meta.is_dir(),
            is_file: meta.is_file(),
            len: meta.len(),
        })
    }

    async fn write_text_file(&self, path: &str, contents: &str) -> Result<(), FsError> {
        if path.is_empty() {
            return Err(FsError::WrongKind(
                "the empty path is the workspace itself, not a file".to_owned(),
            ));
        }
        let target = self.resolve_write(path)?;
        // Checked before the parents are created: a directory in the way is a wrong kind, not an
        // io accident, and `create_dir_all` on its parent would otherwise succeed and leave the
        // real failure to surface from `write` as EISDIR.
        if tokio::fs::metadata(&target)
            .await
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
        {
            return Err(FsError::WrongKind(format!(
                "{path} is a directory, not a file"
            )));
        }
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| FsError::Io(format!("{path}: {error}")))?;
        }
        tokio::fs::write(&target, contents)
            .await
            .map_err(|error| FsError::Io(format!("{path}: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::FsBackend;

    fn workspace() -> (tempfile::TempDir, LocalFs) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "hello").expect("write");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub").join("b.txt"), "world").expect("write");
        let fs = LocalFs::new(dir.path()).expect("root");
        (dir, fs)
    }

    #[tokio::test]
    async fn reads_inside_the_workspace() {
        let (_dir, fs) = workspace();
        assert_eq!(fs.read_text_file("a.txt").await.expect("read"), "hello");
        assert_eq!(fs.read_text_file("sub/b.txt").await.expect("read"), "world");
        let entries = fs.read_dir("").await.expect("root listing");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"a.txt") && names.contains(&"sub"));
        let sub = fs.metadata("sub").await.expect("meta");
        assert!(sub.is_dir && !sub.is_file);
    }

    #[tokio::test]
    async fn absolute_paths_and_climbing_are_refused_before_the_disk() {
        let (_dir, fs) = workspace();
        for attempt in ["/etc/passwd", "../outside.txt", "sub/../../x", "./a.txt"] {
            let result = fs.read_text_file(attempt).await;
            if attempt == "./a.txt" {
                assert!(result.is_ok(), "./ normalises away: {result:?}");
            } else {
                let error = result.expect_err(attempt);
                assert!(
                    matches!(error, FsError::OutsideWorkspace(_)),
                    "{attempt}: {error}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_symlink_escaping_the_workspace_is_caught_on_disk() {
        let (_dir, fs) = workspace();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", _dir.path().join("escape")).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir("C:\\ProgramData", _dir.path().join("escape"))
            .expect("symlink");

        let error = fs
            .read_dir("escape")
            .await
            .expect_err("the symlink resolves outside");
        assert!(matches!(error, FsError::OutsideWorkspace(_)), "{error}");
    }

    #[tokio::test]
    async fn binary_content_is_refused_not_mangled() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("img.png"), b"\x89PNG\r\n\x1a\n\0\0").expect("write");
        let fs = LocalFs::new(dir.path()).expect("root");
        let error = fs.read_text_file("img.png").await.expect_err("binary");
        assert!(matches!(error, FsError::Binary(_)), "{error}");
    }

    #[tokio::test]
    async fn missing_paths_report_not_found_with_the_original_spelling() {
        let (_dir, fs) = workspace();
        let error = fs.read_text_file("nope.txt").await.expect_err("missing");
        assert!(
            matches!(&error, FsError::NotFound(name) if name == "nope.txt"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn reading_a_directory_is_wrong_kind_not_an_io_accident() {
        // Both backends must answer the same input the same way; MemoryFs pins its side in the
        // testkit, this pins the disk side. `File::open` succeeds on a directory, so without the
        // explicit check this surfaced as a bare EISDIR string.
        let ws = hatchery_testkit::TempWorkspace::new();
        ws.write("src/lib.rs", "pub fn f() {}\n");
        let fs = LocalFs::new(ws.root()).expect("workspace");
        let error = fs.read_text_file("src").await.expect_err("a directory");
        assert!(matches!(error, crate::fs::FsError::WrongKind(_)), "{error}");
    }

    #[tokio::test]
    async fn the_empty_path_is_the_workspace_not_a_file() {
        let ws = hatchery_testkit::TempWorkspace::new();
        ws.write("a.txt", "x\n");
        let fs = LocalFs::new(ws.root()).expect("workspace");
        let error = fs.read_text_file("").await.expect_err("empty path");
        assert!(matches!(error, crate::fs::FsError::WrongKind(_)), "{error}");
        // ...while the same empty path stays the root for directory reads.
        let entries = fs.read_dir("").await.expect("the workspace root");
        assert!(!entries.is_empty());
    }

    // The construction and lookup edges, pinned on the disk side: `new` refuses a non-directory
    // root, the accessor tells the truth, and the read seams name a missing path as their own
    // kind of refusal.
    #[tokio::test]
    async fn the_workspace_root_must_be_a_directory_that_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("a-file");
        std::fs::write(&file, b"x").expect("write");
        assert!(
            matches!(LocalFs::new(&file), Err(FsError::WrongKind(_))),
            "a file is not a workspace"
        );
        assert!(
            matches!(LocalFs::new(dir.path().join("absent")), Err(FsError::Io(_))),
            "a missing root is an io refusal, not a panic"
        );
    }

    #[tokio::test]
    async fn directory_reads_name_missing_and_misworn_paths() {
        use crate::fs::FsBackend as _;
        let ws = hatchery_testkit::TempWorkspace::new();
        ws.write("src/lib.rs", "pub fn f() {}\n");
        let fs = LocalFs::new(ws.root()).expect("workspace");

        assert_eq!(fs.root(), ws.root().canonicalize().expect("root").as_path());

        let error = fs.read_dir("absent").await.expect_err("missing");
        assert!(matches!(error, crate::fs::FsError::NotFound(_)), "{error}");
        let error = fs.read_dir("src/lib.rs").await.expect_err("a file");
        assert!(matches!(error, crate::fs::FsError::WrongKind(_)), "{error}");
        let names: Vec<String> = fs
            .read_dir("src")
            .await
            .expect("a real directory")
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, vec!["lib.rs".to_owned()]);

        let error = fs.metadata("absent").await.expect_err("missing");
        assert!(matches!(error, crate::fs::FsError::NotFound(_)), "{error}");
    }
}
