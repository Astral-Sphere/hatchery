//! `glob`: find files by pattern, walking through the seam.

use async_trait::async_trait;
use globset::{Glob as GlobPattern, GlobSet, GlobSetBuilder};
use serde_json::Value;

use hatchery_capabilities::{Tool, ToolCtx, ToolError};
use hatchery_kernel::ToolDef;
use hatchery_protocol::{ApprovalRequest, ToolCallSummary, ToolOutput};

use crate::walk::walk;

/// How many matches one call returns.
pub const MAX_MATCHES: usize = 1_000;

/// The find-by-pattern tool.
#[derive(Default)]
pub struct Glob;

impl Glob {
    /// The tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Compiles the tool's pattern dialect: workspace-relative, `/`-separated.
fn compile(pattern: &str) -> Result<GlobSet, ToolError> {
    if pattern.starts_with('/') {
        return Err(ToolError::InvalidArgs(
            "glob patterns are workspace-relative; drop the leading `/`".to_owned(),
        ));
    }
    GlobSetBuilder::new()
        .add(
            GlobPattern::new(pattern)
                .map_err(|error| ToolError::InvalidArgs(format!("bad glob pattern: {error}")))?,
        )
        .build()
        .map_err(|error| ToolError::Backend(format!("glob: {error}")))
}

#[async_trait]
impl Tool for Glob {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "glob".to_owned(),
            description: "Finds files by glob pattern relative to the workspace root, e.g. \
                          `src/**/*.rs` or `*.md`. Returns matching paths, sorted."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string",
                                "description": "Glob pattern, workspace-relative. `**` crosses directories."}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn needs_approval(&self, _args: &Value) -> Option<ApprovalRequest> {
        None
    }

    fn summarize(&self, args: &Value) -> ToolCallSummary {
        ToolCallSummary::new(format!("glob {}", args["pattern"].as_str().unwrap_or("?")))
    }

    async fn execute(&self, ctx: ToolCtx<'_>, args: Value) -> Result<ToolOutput, ToolError> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("glob needs a string `pattern`".to_owned()))?;
        let set = compile(pattern)?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        let mut matches = Vec::new();
        let paths = walk(&ctx, "").await?;
        let visited = paths.len();
        for path in paths {
            if ctx.cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            if set.is_match(&path) {
                matches.push(path);
                if matches.len() >= MAX_MATCHES {
                    break;
                }
            }
        }

        matches.sort();
        let mut body = matches.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&format!("[{}/{} files matched]", matches.len(), visited));
        if matches.len() >= MAX_MATCHES {
            body.push_str(&format!(
                " [result cap {MAX_MATCHES} reached; narrow the pattern]"
            ));
        }
        Ok(ToolOutput::text(body))
    }
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

    fn fixture() -> MemoryFs {
        let fs = MemoryFs::new();
        fs.file("src/main.rs", "fn main() {}")
            .file("src/lib/util.rs", "pub fn u() {}")
            .file("README.md", "# h")
            .dir(".git/hooks")
            .file(".git/hooks/pre-commit", "#!/bin/sh");
        fs
    }

    async fn run(args: Value) -> ToolOutput {
        let fs = fixture();
        Glob.execute(ctx(&fs), args).await.expect("run")
    }

    #[tokio::test]
    async fn star_patterns_match_within_one_directory() {
        let output = run(serde_json::json!({"pattern": "*.md"})).await;
        assert_eq!(output.text, "README.md\n[1/3 files matched]");
    }

    #[tokio::test]
    async fn doublestar_crosses_directories_and_skips_git() {
        let output = run(serde_json::json!({"pattern": "**/*.rs"})).await;
        assert_eq!(
            output.text,
            "src/lib/util.rs\nsrc/main.rs\n[2/3 files matched]"
        );
    }

    #[tokio::test]
    async fn absolute_patterns_and_bad_syntax_are_invalid_args() {
        let error = Glob
            .execute(ctx(&fixture()), serde_json::json!({"pattern": "/etc/*"}))
            .await
            .expect_err("absolute");
        assert!(matches!(error, ToolError::InvalidArgs(m) if m.contains("workspace-relative")));

        let error = Glob
            .execute(ctx(&fixture()), serde_json::json!({"pattern": "["}))
            .await
            .expect_err("unclosed bracket");
        assert!(matches!(error, ToolError::InvalidArgs(m) if m.contains("bad glob")));
    }

    #[tokio::test]
    async fn no_matches_say_so_with_the_visited_count() {
        let output = run(serde_json::json!({"pattern": "*.zzz"})).await;
        assert_eq!(output.text, "[0/3 files matched]");
    }

    #[test]
    fn the_summary_is_the_pattern() {
        assert_eq!(
            Glob.summarize(&serde_json::json!({"pattern": "src/**"}))
                .title,
            "glob src/**"
        );
    }
}
