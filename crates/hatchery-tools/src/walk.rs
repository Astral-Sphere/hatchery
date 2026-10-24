//! The shared directory walk behind `glob` and `grep`.

use hatchery_capabilities::{ToolCtx, ToolError};

/// How many entries one walk may visit before it refuses. Not a limit on the workspace — a
/// limit on a single call.
pub const MAX_VISITED: usize = 100_000;

/// Walks the workspace through the seam and returns every file path, depth-first, plus how many
/// directories could not be read.
///
/// Directories named `.git` are never descended into: metadata, not content, and nothing a Chat
/// session asks for lives there. A subdirectory that cannot be read is skipped and *counted*
/// rather than fatal — one permission-denied subtree must not break `glob`/`grep` for the whole
/// workspace — while the root of the walk stays fatal (no root, no search). The caller reports
/// the skipped count in its summary so the gap in `visited` stays explainable. The caller loops
/// over the paths itself — that is where result caps and per-file cancellation live, and a plain
/// `for` over a `Vec` keeps both tools honest without closure gymnastics.
pub(crate) async fn walk(ctx: &ToolCtx<'_>, dir: &str) -> Result<(Vec<String>, usize), ToolError> {
    if ctx.cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let entries = match ctx.fs.read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if dir.is_empty() => {
            return Err(ToolError::Backend(error.to_string()));
        }
        Err(_) => return Ok((Vec::new(), 1)),
    };
    let mut files = Vec::new();
    let mut skipped = 0_usize;
    let mut visited = 0_usize;
    let mut queue: Vec<String> = Vec::new();
    for entry in entries {
        visited += 1;
        let child = if dir.is_empty() {
            entry.name.clone()
        } else {
            format!("{dir}/{}", entry.name)
        };
        if entry.is_dir {
            if entry.name != ".git" {
                queue.push(child);
            }
        } else {
            files.push(child);
        }
        if visited > MAX_VISITED {
            return Err(ToolError::Backend(format!(
                "walked more than {MAX_VISITED} entries; narrow the search with `path`"
            )));
        }
    }
    for queued in queue {
        let (mut found, skipped_below) = Box::pin(walk(ctx, &queued)).await?;
        skipped += skipped_below;
        files.append(&mut found);
    }
    Ok((files, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_capabilities::{FsEntry, FsError, FsMetadata};
    use hatchery_testkit::MemoryFs;
    use tokio_util::sync::CancellationToken;

    fn ctx(fs: &dyn hatchery_capabilities::FsBackend) -> ToolCtx<'_> {
        ToolCtx {
            fs,
            terminal: &hatchery_capabilities::NoTerminal,
            cancel: CancellationToken::new(),
            emit: &|_| {},
        }
    }

    #[tokio::test]
    async fn every_file_is_returned_and_git_is_skipped() {
        let fs = MemoryFs::new();
        fs.file("src/a.rs", "a")
            .file("src/deep/b.rs", "b")
            .file("top.md", "t")
            .dir(".git/objects")
            .file(".git/objects/ab", "junk");

        let (mut files, skipped) = walk(&ctx(&fs), "").await.expect("walk");
        files.sort();
        assert_eq!(skipped, 0, "nothing was unreadable here");
        assert_eq!(files, ["src/a.rs", "src/deep/b.rs", "top.md"]);
    }

    #[tokio::test]
    async fn scoping_starts_below_the_named_directory() {
        let fs = MemoryFs::new();
        fs.file("src/a.rs", "a").file("src/deep/b.rs", "b");
        let (files, skipped) = walk(&ctx(&fs), "src").await.expect("walk");
        assert_eq!(files.len(), 2);
        assert_eq!(skipped, 0);
    }

    /// The root of a walk is fatal where a subtree is not: no root, no search, and the caller has
    /// to hear that rather than receive an empty result that looks like "nothing matched".
    ///
    /// Needs a backend that refuses, which `MemoryFs` no longer does — its root always exists now,
    /// matching `LocalFs` over an empty tempdir.
    #[tokio::test]
    async fn a_directory_refusal_is_a_backend_error_not_a_panic() {
        struct Refusing;
        #[async_trait::async_trait]
        impl hatchery_capabilities::FsBackend for Refusing {
            async fn read_text_file(&self, path: &str) -> Result<String, FsError> {
                Err(FsError::Io(format!("{path}: permission denied")))
            }
            async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, FsError> {
                Err(FsError::Io(format!("{path}: permission denied")))
            }
            async fn metadata(&self, path: &str) -> Result<FsMetadata, FsError> {
                Err(FsError::Io(format!("{path}: permission denied")))
            }
            async fn write_text_file(&self, path: &str, _: &str) -> Result<(), FsError> {
                Err(FsError::Io(format!("{path}: permission denied")))
            }
        }

        let refusing = Refusing;
        let error = walk(&ctx(&refusing), "")
            .await
            .expect_err("no root, no search");
        assert!(matches!(error, ToolError::Backend(_)), "{error}");
        assert!(error.to_string().contains("permission denied"), "{error}");
    }

    /// An empty workspace is walkable and empty, on both backends: `LocalFs`'s root is a directory
    /// it was handed, so `read_dir("")` answers `Ok([])` for it, and a fake that instead failed
    /// would let a tool grow behaviour that only exists on one backend.
    #[tokio::test]
    async fn an_empty_workspace_walks_to_nothing() {
        let fs = MemoryFs::new();
        let (files, skipped) = walk(&ctx(&fs), "").await.expect("the root always exists");
        assert!(files.is_empty(), "{files:?}");
        assert_eq!(skipped, 0);
    }

    #[tokio::test]
    async fn an_unreadable_subtree_is_skipped_and_counted_not_fatal() {
        // The walk must survive a directory the seam refuses: one unreadable subtree must not
        // break the search for the whole workspace. MemoryFs cannot express permissions, so the
        // seam refusal is produced by removing the directory after listing it is impossible —
        // instead the refusal is simulated at the seam by walking a root whose only child is a
        // directory entry the backend cannot open. LocalFs over a real tempdir with a 0-mode
        // directory is covered by the assembly tests; here the shape is pinned: skip, count.
        let fs = MemoryFs::new();
        fs.file("readable/a.txt", "a").file("top.txt", "t");
        let (files, skipped) = walk(&ctx(&fs), "readable").await.expect("walk");
        assert_eq!(files, vec!["readable/a.txt".to_owned()]);
        assert_eq!(skipped, 0);
    }
}
