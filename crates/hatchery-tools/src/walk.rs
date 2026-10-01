//! The shared directory walk behind `glob` and `grep`.

use hatchery_capabilities::{ToolCtx, ToolError};

/// How many entries one walk may visit before it refuses. Not a limit on the workspace — a
/// limit on a single call.
pub const MAX_VISITED: usize = 100_000;

/// Walks the workspace through the seam and returns every file path, depth-first.
///
/// Directories named `.git` are never descended into: metadata, not content, and nothing a Chat
/// session asks for lives there. The caller loops over the paths itself — that is where result
/// caps and per-file cancellation live, and a plain `for` over a `Vec` keeps both tools honest
/// without closure gymnastics.
pub(crate) async fn walk(ctx: &ToolCtx<'_>, dir: &str) -> Result<Vec<String>, ToolError> {
    if ctx.cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let entries = ctx
        .fs
        .read_dir(dir)
        .await
        .map_err(|error| ToolError::Backend(error.to_string()))?;
    let mut files = Vec::new();
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
        files.extend(Box::pin(walk(ctx, &queued)).await?);
    }
    Ok(files)
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

        let mut files = walk(&ctx(&fs), "").await.expect("walk");
        files.sort();
        assert_eq!(files, ["src/a.rs", "src/deep/b.rs", "top.md"]);
    }

    #[tokio::test]
    async fn scoping_starts_below_the_named_directory() {
        let fs = MemoryFs::new();
        fs.file("src/a.rs", "a").file("src/deep/b.rs", "b");
        let files = walk(&ctx(&fs), "src").await.expect("walk");
        assert_eq!(files.len(), 2);
    }

    #[tokio::test]
    async fn a_directory_refusal_is_a_backend_error_not_a_panic() {
        // An empty MemoryFs has no root directory entry, so walking "" fails at the seam.
        let fs = MemoryFs::new();
        let error = walk(&ctx(&fs), "").await.expect_err("no root");
        assert!(matches!(error, ToolError::Backend(_)), "{error}");
    }
}
