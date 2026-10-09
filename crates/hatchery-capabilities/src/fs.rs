//! The filesystem seam: what a tool may touch, and nothing else.

use async_trait::async_trait;

/// Why a filesystem operation failed.
///
/// Distinguishable at the seam so tools can react (a `grep` skips [`FsError::Binary`] files and
/// keeps walking) without learning what a filesystem is.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    /// The path does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// The path exists but is not the kind the operation needed (a file where a directory was
    /// asked for, or the reverse).
    #[error("wrong kind: {0}")]
    WrongKind(String),
    /// The file has content no text interface should hand out (NUL bytes in the first kilobyte).
    #[error("binary file: {0}")]
    Binary(String),
    /// The path escapes the workspace — after lexical normalisation *and* symlink resolution.
    #[error("outside the workspace: {0}")]
    OutsideWorkspace(String),
    /// The write was refused because the undo point that must precede it could not be taken.
    ///
    /// Distinct from [`FsError::Io`]: nothing is wrong with the path or the disk, and the file is
    /// untouched. An agent write that cannot be undone is refused rather than performed
    /// (docs/design/capabilities.md §2), so this is the error a model sees when the shadow
    /// repository itself failed.
    #[error("no checkpoint, no write: {0}")]
    Checkpoint(String),
    /// The host refused the operation.
    #[error("fs: {0}")]
    Io(String),
}

/// One entry of a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsEntry {
    /// The entry's name, not its path.
    pub name: String,
    /// Whether descending into it is meaningful.
    pub is_dir: bool,
}

/// What is at a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsMetadata {
    /// A directory.
    pub is_dir: bool,
    /// A regular file (or at least something with a length).
    pub is_file: bool,
    /// Size in bytes, when the host reports one.
    pub len: u64,
}

/// The filesystem a tool sees: workspace-relative paths, read and write.
///
/// Paths are **workspace-relative** and never absolute — the backend owns the root, and only the
/// backend decides what "outside" means. This is what lets a session bound to an ACP host's
/// single-file interface swap in without touching a tool (ADR-0004).
///
/// The write surface is one method on purpose. `create_dir` and `remove` were specced alongside it
/// ("`write_file` needs to make parent directories, rewind's purge needs to delete files") and both
/// turned out to have no caller: [`write_text_file`](Self::write_text_file) creates the parents it
/// needs, and purge is `git checkout-index --remove-untracked` inside the shadow repository, which
/// never comes through this seam (measured in `tests/spike_shadow_git.rs`). A primitive with no
/// consumer is a primitive nobody has tested (ADR-0009).
#[async_trait]
pub trait FsBackend: Send + Sync {
    /// The whole file as text. Binary content is refused, not mangled.
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`], [`FsError::Binary`], [`FsError::OutsideWorkspace`], or the host's
    /// refusal as [`FsError::Io`].
    async fn read_text_file(&self, path: &str) -> Result<String, FsError>;

    /// The entries of a directory, unnamed order.
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`] / [`FsError::WrongKind`] when the path is missing or a file.
    async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, FsError>;

    /// What is at the path, without reading it.
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`] when nothing is there.
    async fn metadata(&self, path: &str) -> Result<FsMetadata, FsError>;

    /// Replaces the file at `path` with `contents`, creating any missing parent directory.
    ///
    /// Whole-file replacement, not a patch: the seam carries bytes, and the *edit* semantics
    /// (finding an anchor, refusing an ambiguous match) belong to the tool that knows what an edit
    /// is. Parents are created because "write this file" from a model means the path it chose, and
    /// a missing `src/new/` is not an interesting failure to hand back.
    ///
    /// Backends that can be checkpointed are wrapped by [`crate::CheckpointedFs`], which takes the
    /// shadow-git snapshot **before** delegating here — so an implementation must not do anything
    /// else first.
    ///
    /// # Errors
    ///
    /// [`FsError::OutsideWorkspace`] for a path that escapes, [`FsError::WrongKind`] when the path
    /// is a directory, [`FsError::Checkpoint`] when the undo point could not be taken, or the
    /// host's refusal as [`FsError::Io`].
    async fn write_text_file(&self, path: &str, contents: &str) -> Result<(), FsError>;
}
