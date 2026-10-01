//! The local filesystem backend: the real disk, behind the seam.
//!
//! Two rules make reads safe enough to hand a model (docs/design/capabilities.md §1/§5):
//!
//! * **Workspace-relative, twice over.** Paths are resolved lexically first (`..` cannot climb
//!   out), then resolved on disk with `canonicalize` — a symlink planted inside the workspace
//!   pointing out is caught by the second check, which the lexical one cannot see.
//! * **No text from binaries.** A NUL byte in the first kilobyte means the file is refused, not
//!   lossily mangled: a model that asks to read a PNG should hear so, not receive mojibake.

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
        let relative = Path::new(path);
        if path.is_empty() {
            return Ok(self.root.as_ref().clone());
        }
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

        let joined = self.root.join(&lexical);
        let resolved = std::fs::canonicalize(&joined).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => FsError::NotFound(path.to_owned()),
            _ => FsError::Io(format!("{path}: {error}")),
        })?;
        if !resolved.starts_with(self.root.as_ref()) {
            return Err(FsError::OutsideWorkspace(format!(
                "{path} resolves to {} via a symlink, outside the workspace",
                resolved.display()
            )));
        }
        Ok(resolved)
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
        let resolved = self.resolve(path)?;
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
}
