//! `read_file`: one text file, paged, truncated, never binary.

use async_trait::async_trait;
use serde_json::Value;

use hatchery_capabilities::{Tool, ToolCtx, ToolError};
use hatchery_kernel::ToolDef;
use hatchery_protocol::{ApprovalRequest, ToolCallSummary, ToolOutput};

/// The default byte ceiling of one read. Generous enough for source files, strict enough that
/// one call cannot flood a context window; the notice names the cut so the model can page.
pub const DEFAULT_MAX_BYTES: usize = 256 * 1024;

/// The read-one-file tool.
pub struct ReadFile {
    max_bytes: usize,
}

impl ReadFile {
    /// The tool with the default byte ceiling.
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    /// The tool with a test-sized ceiling, for exercising the truncation notice.
    #[must_use]
    pub fn with_max_bytes(max_bytes: usize) -> Self {
        Self { max_bytes }
    }
}

impl Default for ReadFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_file".to_owned(),
            description: "Reads a text file from the workspace, given as a workspace-relative \
                          path. Binary files are refused. Large files are truncated; page with \
                          `offset` (1-based first line) and `limit` (line count)."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative file path."},
                    "offset": {"type": "integer", "minimum": 1,
                               "description": "First line to return (1-based)."},
                    "limit": {"type": "integer", "minimum": 1,
                              "description": "How many lines to return."}
                },
                "required": ["path"]
            }),
        }
    }

    fn needs_approval(&self, _args: &Value) -> Option<ApprovalRequest> {
        // Chat mode: workspace-relative reads need nobody's permission (ADR-0005). The M2 Code
        // mode adds the outside-workspace approval via its own registry wiring.
        None
    }

    fn summarize(&self, args: &Value) -> ToolCallSummary {
        let path = args["path"].as_str().unwrap_or("?");
        let mut summary = ToolCallSummary::new(format!("read_file {path}"));
        match (args["offset"].as_u64(), args["limit"].as_u64()) {
            (Some(offset), Some(limit)) => {
                summary.detail = Some(format!("lines {offset}–{}", offset + limit - 1));
            }
            (Some(offset), None) => summary.detail = Some(format!("from line {offset}")),
            _ => {}
        }
        summary
    }

    async fn execute(&self, ctx: ToolCtx<'_>, args: Value) -> Result<ToolOutput, ToolError> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("read_file needs a string `path`".to_owned()))?;
        let offset = args["offset"].as_u64().map_or(0, |v| v.max(1) as usize - 1);
        let limit = args["limit"].as_u64().map(|v| v as usize);

        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        let full = ctx.fs.read_text_file(path).await?;
        let total_lines = full.lines().count();

        // Page first, then clip to the byte budget: the model asked for a window, the ceiling
        // only bounds how much of it fits.
        let mut body = String::new();
        let mut shown = 0_usize;
        let mut clipped_by_bytes = false;
        for (index, line) in full
            .lines()
            .skip(offset)
            .enumerate()
            .take(limit.unwrap_or(usize::MAX))
        {
            if body.len() + line.len() + 1 > self.max_bytes {
                clipped_by_bytes = true;
                break;
            }
            body.push_str(&(offset + index + 1).to_string());
            body.push_str(": ");
            body.push_str(line);
            body.push('\n');
            shown += 1;
        }

        let mut notices = Vec::new();
        let remaining = total_lines.saturating_sub(offset + shown);
        if remaining > 0 {
            notices.push(format!(
                "[truncated: showed lines {}–{} of {}; continue with offset {}]",
                offset + 1,
                offset + shown,
                total_lines,
                offset + shown + 1
            ));
        }
        if clipped_by_bytes {
            notices.push(format!("[read capped at {} bytes]", self.max_bytes));
        }
        for notice in &notices {
            body.push_str(notice);
            body.push('\n');
        }
        if body.is_empty() {
            body.push_str("[empty file]\n");
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

    async fn run(tool: &ReadFile, fs: &MemoryFs, args: Value) -> Result<ToolOutput, ToolError> {
        tool.execute(ctx(fs), args).await
    }

    fn fs_with(path: &str, content: &str) -> MemoryFs {
        let fs = MemoryFs::new();
        fs.file(path, content);
        fs
    }

    #[tokio::test]
    async fn reads_a_file_with_numbered_lines() {
        let fs = fs_with("a.txt", "first\nsecond\n");
        let output = run(&ReadFile::new(), &fs, serde_json::json!({"path": "a.txt"}))
            .await
            .expect("read");
        assert_eq!(output.text, "1: first\n2: second\n");
    }

    #[tokio::test]
    async fn paging_slices_by_line_and_names_the_continuation() {
        let fs = fs_with("big.txt", "one\ntwo\nthree\nfour\n");
        let output = run(
            &ReadFile::new(),
            &fs,
            serde_json::json!({"path": "big.txt", "offset": 2, "limit": 2}),
        )
        .await
        .expect("read");
        assert_eq!(
            output.text,
            "2: two\n3: three\n[truncated: showed lines 2–3 of 4; continue with offset 4]\n"
        );
    }

    #[tokio::test]
    async fn a_too_big_file_is_clipped_with_both_notices() {
        let fs = fs_with("huge.txt", &"aaaa\n".repeat(100));
        let tool = ReadFile::with_max_bytes(10);
        let output = run(&tool, &fs, serde_json::json!({"path": "huge.txt"}))
            .await
            .expect("read");
        assert!(output.text.starts_with("1: aaaa\n"), "{}", output.text);
        assert!(output.text.contains("[truncated:"), "{}", output.text);
        assert!(
            output.text.contains("[read capped at 10 bytes]"),
            "{}",
            output.text
        );
    }

    #[tokio::test]
    async fn the_empty_file_gets_an_explicit_marker() {
        let fs = fs_with("empty.txt", "");
        let output = run(
            &ReadFile::new(),
            &fs,
            serde_json::json!({"path": "empty.txt"}),
        )
        .await
        .expect("read");
        assert_eq!(output.text, "[empty file]\n");
    }

    #[tokio::test]
    async fn seam_refusals_come_back_as_backend_errors_with_the_reason() {
        let fs = MemoryFs::new();
        let error = run(
            &ReadFile::new(),
            &fs,
            serde_json::json!({"path": "nope.txt"}),
        )
        .await
        .expect_err("missing");
        assert!(
            matches!(&error, ToolError::Backend(reason) if reason.contains("not found")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn bad_arguments_are_invalid_not_backend() {
        let fs = MemoryFs::new();
        for args in [serde_json::json!({}), serde_json::json!({"path": 3})] {
            let error = run(&ReadFile::new(), &fs, args)
                .await
                .expect_err("bad args");
            assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
        }
    }

    #[test]
    fn summaries_carry_the_path_and_the_page() {
        let tool = ReadFile::new();
        assert_eq!(
            tool.summarize(&serde_json::json!({"path": "a.txt"})).title,
            "read_file a.txt"
        );
        assert_eq!(
            tool.summarize(&serde_json::json!({"path": "a.txt", "offset": 5, "limit": 10}))
                .detail
                .expect("detail"),
            "lines 5–14"
        );
    }

    #[test]
    fn the_definition_names_the_schema_and_its_required_argument() {
        let def = ReadFile::new().def();
        assert_eq!(def.name, "read_file");
        assert_eq!(def.parameters["required"][0], "path");
    }
}
