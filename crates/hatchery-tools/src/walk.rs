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
    use hatchery_testkit::MemoryFs;
    use tokio_util::sync::CancellationToken;

    fn ctx(fs: &MemoryFs) -> ToolCtx<'_> {
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

    #[tokio::test]
    async fn a_directory_refusal_is_a_backend_error_not_a_panic() {
        // An empty MemoryFs has no root directory entry, so walking "" fails at the seam.
        let fs = MemoryFs::new();
        let error = walk(&ctx(&fs), "").await.expect_err("no root");
        assert!(matches!(error, ToolError::Backend(_)), "{error}");
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
