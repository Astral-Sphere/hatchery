//! `write_file`: whole-file replacement, approved before it runs.

use async_trait::async_trait;
use serde_json::Value;

use hatchery_capabilities::{FsError, Tool, ToolCtx, ToolError};
use hatchery_kernel::ToolDef;
use hatchery_protocol::{ApprovalRequest, RiskLevel, ToolCallSummary, ToolOutput};

use crate::hard_gate::hard_gate;
use crate::preview::diff_preview;

/// The replace-one-file tool.
#[derive(Default)]
pub struct WriteFile;

impl WriteFile {
    /// The tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "write_file".to_owned(),
            description: "Writes a text file in the workspace, replacing it whole and creating \
                          missing parent directories. The user is shown a diff of the change \
                          before it happens."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative file path."},
                    "content": {"type": "string", "description": "The file's new text."}
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn needs_approval(&self, ctx: &ToolCtx<'_>, args: &Value) -> Option<ApprovalRequest> {
        // Arguments that cannot name a write (or a body) fail in `execute`; asking the user to
        // approve a call that is certain to fail only spends their attention.
        let path = args["path"].as_str()?;
        let content = args["content"].as_str()?;
        if path.is_empty() {
            return None;
        }

        let (digest, gated) = match hard_gate(path) {
            Some(reason) => (format!("write {path} (sensitive: {reason})"), true),
            None => (format!("write {path}"), false),
        };
        let request = ApprovalRequest::new("write_file", digest, RiskLevel::WritesWorkspace);
        let request = if gated { request.once_only() } else { request };

        // The preview reads through the same seam execution will use. A missing file is a
        // creation, not a refusal; any other read failure still asks — the user is deciding
        // whether this call may run at all, and a preview is a convenience, not a precondition.
        let (old, existed) = match ctx.fs.read_text_file(path).await {
            Ok(text) => (text, true),
            Err(FsError::NotFound(_)) => (String::new(), false),
            Err(_) => return Some(request),
        };
        match diff_preview(path, existed, &old, content) {
            Some(preview) => Some(request.with_preview(preview)),
            None => Some(request),
        }
    }

    fn summarize(&self, args: &Value) -> ToolCallSummary {
        let path = args["path"].as_str().unwrap_or("?");
        let lines = args["content"]
            .as_str()
            .map_or(0, |content| content.lines().count());
        ToolCallSummary::new(format!("write_file {path} (+{lines})"))
    }

    async fn execute(&self, ctx: ToolCtx<'_>, args: Value) -> Result<ToolOutput, ToolError> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("write_file needs a string `path`".to_owned()))?;
        let content = args["content"].as_str().ok_or_else(|| {
            ToolError::InvalidArgs("write_file needs a string `content`".to_owned())
        })?;
        if path.is_empty() {
            return Err(ToolError::InvalidArgs(
                "write_file needs a non-empty `path`".to_owned(),
            ));
        }
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        ctx.fs.write_text_file(path, content).await?;
        Ok(ToolOutput::text(format!(
            "wrote {} bytes to {path}",
            content.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use hatchery_capabilities::{FsBackend, FsEntry, FsMetadata};
    use hatchery_protocol::{ApprovalOption, ApprovalPreview, DiffLineKind, DiffStatus};
    use hatchery_testkit::MemoryFs;
    use tokio_util::sync::CancellationToken;

    fn ctx(fs: &dyn FsBackend) -> ToolCtx<'_> {
        ToolCtx {
            fs,
            terminal: &hatchery_capabilities::NoTerminal,
            cancel: CancellationToken::new(),
            emit: &|_| {},
        }
    }

    async fn run(fs: &MemoryFs, args: Value) -> Result<ToolOutput, ToolError> {
        WriteFile::new().execute(ctx(fs), args).await
    }

    #[tokio::test]
    async fn creates_a_new_file_with_missing_parents() {
        let fs = MemoryFs::new();
        let output = run(
            &fs,
            serde_json::json!({"path": "src/deep/new.rs", "content": "fn f() {}\n"}),
        )
        .await
        .expect("write");
        assert_eq!(output.text, "wrote 10 bytes to src/deep/new.rs");
        assert_eq!(
            fs.read_text_file("src/deep/new.rs").await.expect("read"),
            "fn f() {}\n"
        );
    }

    #[tokio::test]
    async fn overwrites_the_whole_file() {
        let fs = MemoryFs::new();
        fs.file("a.txt", "old content that is longer\n");
        run(
            &fs,
            serde_json::json!({"path": "a.txt", "content": "new\n"}),
        )
        .await
        .expect("write");
        assert_eq!(fs.read_text_file("a.txt").await.expect("read"), "new\n");
    }

    #[tokio::test]
    async fn an_empty_path_is_invalid_args() {
        let fs = MemoryFs::new();
        let error = run(&fs, serde_json::json!({"path": "", "content": "x"}))
            .await
            .expect_err("empty path");
        assert!(
            matches!(&error, ToolError::InvalidArgs(m) if m.contains("non-empty")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn missing_or_mistyped_arguments_are_invalid_args() {
        let fs = MemoryFs::new();
        for args in [
            serde_json::json!({}),
            serde_json::json!({"path": "a.txt"}),
            serde_json::json!({"path": 3, "content": "x"}),
            serde_json::json!({"path": "a.txt", "content": 3}),
        ] {
            let error = run(&fs, args).await.expect_err("bad args");
            assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
        }
    }

    #[tokio::test]
    async fn the_preview_of_an_overwrite_is_a_modified_file_ending_in_the_new_text() {
        let fs = MemoryFs::new();
        fs.file("src/main.rs", "fn old() {}\n");
        let request = WriteFile::new()
            .needs_approval(
                &ctx(&fs),
                &serde_json::json!({"path": "src/main.rs", "content": "fn new() {}\n"}),
            )
            .await
            .expect("a write asks");
        assert_eq!(request.risk, RiskLevel::WritesWorkspace);
        let ApprovalPreview::Diff(diff) = request.preview.expect("preview") else {
            panic!("expected a diff preview");
        };
        let file = &diff.files[0];
        assert_eq!(file.status, DiffStatus::Modified);
        assert_eq!(file.path, "src/main.rs");
        assert!(
            file.hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .any(|line| line.kind == DiffLineKind::Added && line.text == "fn new() {}"),
            "the changed line is in the preview: {file:?}"
        );
    }

    #[tokio::test]
    async fn the_preview_of_a_new_file_is_added() {
        let fs = MemoryFs::new();
        let request = WriteFile::new()
            .needs_approval(
                &ctx(&fs),
                &serde_json::json!({"path": "note.txt", "content": "hi\n"}),
            )
            .await
            .expect("a write asks");
        let ApprovalPreview::Diff(diff) = request.preview.expect("preview") else {
            panic!("expected a diff preview");
        };
        assert_eq!(diff.files[0].status, DiffStatus::Added);
    }

    #[tokio::test]
    async fn sensitive_paths_offer_only_the_once_options() {
        for path in [".env", ".git/hooks/pre-commit"] {
            let fs = MemoryFs::new();
            let request = WriteFile::new()
                .needs_approval(
                    &ctx(&fs),
                    &serde_json::json!({"path": path, "content": "X=1\n"}),
                )
                .await
                .expect("a write asks");
            assert_eq!(
                request.options,
                [ApprovalOption::AllowOnce, ApprovalOption::Deny],
                "{path}"
            );
            assert!(
                request.args_digest.contains("sensitive"),
                "the digest says why: {}",
                request.args_digest
            );
        }
    }

    #[tokio::test]
    async fn an_ordinary_path_offers_every_option() {
        let fs = MemoryFs::new();
        let request = WriteFile::new()
            .needs_approval(
                &ctx(&fs),
                &serde_json::json!({"path": "src/main.rs", "content": "fn main() {}\n"}),
            )
            .await
            .expect("a write asks");
        assert_eq!(request.options, ApprovalOption::ALL.to_vec());
    }

    /// A backend whose reads fail in a way that is not "missing": the write still needs a
    /// decision, it just has no preview to offer.
    struct FailsReads(MemoryFs);

    #[async_trait]
    impl FsBackend for FailsReads {
        async fn read_text_file(&self, _path: &str) -> Result<String, FsError> {
            Err(FsError::Io("reads are refused".to_owned()))
        }

        async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, FsError> {
            self.0.read_dir(path).await
        }

        async fn metadata(&self, path: &str) -> Result<FsMetadata, FsError> {
            self.0.metadata(path).await
        }

        async fn write_text_file(&self, path: &str, contents: &str) -> Result<(), FsError> {
            self.0.write_text_file(path, contents).await
        }
    }

    #[tokio::test]
    async fn a_read_failure_still_asks_without_a_preview() {
        let fs = FailsReads(MemoryFs::new());
        let request = WriteFile::new()
            .needs_approval(
                &ctx(&fs),
                &serde_json::json!({"path": "a.txt", "content": "x\n"}),
            )
            .await
            .expect("the user still decides");
        assert!(request.preview.is_none(), "nothing could be read to diff");
    }

    #[test]
    fn the_summary_counts_the_lines_being_written() {
        assert_eq!(
            WriteFile::new()
                .summarize(&serde_json::json!({"path": "a.txt", "content": "one\ntwo\n"}))
                .title,
            "write_file a.txt (+2)"
        );
    }
}
