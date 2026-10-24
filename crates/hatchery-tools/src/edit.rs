//! `edit`: exact, uniquely anchored substring replacement.

use async_trait::async_trait;
use serde_json::Value;

use hatchery_capabilities::{Tool, ToolCtx, ToolError};
use hatchery_kernel::ToolDef;
use hatchery_protocol::{ApprovalPreview, ApprovalRequest, RiskLevel, ToolCallSummary, ToolOutput};

use crate::hard_gate::hard_gate;
use crate::preview::diff_preview;

/// The replace-one-occurrence tool.
#[derive(Default)]
pub struct Edit;

impl Edit {
    /// The tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// How often `needle` occurs in `haystack`. `needle` must be non-empty.
fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// The digest the approval prompt shows, naming the gate when there is one.
fn digest(path: &str) -> (String, bool) {
    match hard_gate(path) {
        Some(reason) => (format!("edit {path} (sensitive: {reason})"), true),
        None => (format!("edit {path}"), false),
    }
}

#[async_trait]
impl Tool for Edit {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "edit".to_owned(),
            description: "Replaces one exact occurrence of `old_string` with `new_string` in a \
                          workspace file. `old_string` must occur exactly once; include enough \
                          surrounding context to make it unique. The user is shown a diff before \
                          it happens."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative file path."},
                    "old_string": {"type": "string",
                                   "description": "Exact text to replace; must occur exactly once."},
                    "new_string": {"type": "string", "description": "Text to put in its place."}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    async fn needs_approval(&self, ctx: &ToolCtx<'_>, args: &Value) -> Option<ApprovalRequest> {
        let path = args["path"].as_str()?;
        let old_string = args["old_string"].as_str()?;
        let new_string = args["new_string"].as_str()?;
        // A call that is certain to fail is not worth a prompt: `execute` names the problem
        // (unreadable file, empty anchor, zero or several matches) after the decision point.
        if path.is_empty() || old_string.is_empty() {
            return None;
        }
        let original = ctx.fs.read_text_file(path).await.ok()?;
        if occurrences(&original, old_string) != 1 {
            return None;
        }

        let updated = original.replacen(old_string, new_string, 1);
        let (text, gated) = digest(path);
        let request = ApprovalRequest::new("edit", text, RiskLevel::WritesWorkspace);
        let request = if gated { request.once_only() } else { request };
        match diff_preview(path, true, &original, &updated) {
            Some(preview) => Some(request.with_preview(preview)),
            None => Some(request),
        }
    }

    fn summarize(&self, args: &Value) -> ToolCallSummary {
        ToolCallSummary::new(format!("edit {}", args["path"].as_str().unwrap_or("?")))
    }

    async fn execute(&self, ctx: ToolCtx<'_>, args: Value) -> Result<ToolOutput, ToolError> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("edit needs a string `path`".to_owned()))?;
        let old_string = args["old_string"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("edit needs a string `old_string`".to_owned()))?;
        let new_string = args["new_string"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("edit needs a string `new_string`".to_owned()))?;
        if old_string.is_empty() {
            return Err(ToolError::InvalidArgs(
                "edit needs a non-empty `old_string`".to_owned(),
            ));
        }
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        // Re-read and re-validate: the file may have changed between the approval prompt and
        // this call, and substituting into a stale copy would silently undo that change.
        let original = ctx.fs.read_text_file(path).await?;
        let count = occurrences(&original, old_string);
        if count == 0 {
            return Err(ToolError::InvalidArgs(format!(
                "old_string does not occur in {path}"
            )));
        }
        if count > 1 {
            return Err(ToolError::InvalidArgs(format!(
                "old_string occurs {count} times in {path}; include more context so the edit is \
                 unambiguous"
            )));
        }

        let updated = original.replacen(old_string, new_string, 1);
        let (added, removed) = match diff_preview(path, true, &original, &updated) {
            Some(ApprovalPreview::Diff(diff)) => diff.line_counts(),
            _ => (0, 0),
        };
        ctx.fs.write_text_file(path, &updated).await?;
        Ok(ToolOutput::text(format!(
            "edited {path} (+{added} -{removed})"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_capabilities::FsBackend;
    use hatchery_protocol::{ApprovalOption, DiffLineKind, DiffStatus};
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

    async fn run(fs: &MemoryFs, args: Value) -> Result<ToolOutput, ToolError> {
        Edit::new().execute(ctx(fs), args).await
    }

    #[tokio::test]
    async fn replaces_the_single_matching_occurrence() {
        let fs = MemoryFs::new();
        fs.file("src/main.rs", "fn main() {}\n");
        let output = run(
            &fs,
            serde_json::json!({
                "path": "src/main.rs",
                "old_string": "main",
                "new_string": "start"
            }),
        )
        .await
        .expect("edit");
        assert_eq!(output.text, "edited src/main.rs (+1 -1)");
        assert_eq!(
            fs.read_text_file("src/main.rs").await.expect("read"),
            "fn start() {}\n"
        );
    }

    #[tokio::test]
    async fn a_missing_anchor_fails_without_touching_the_file() {
        let fs = MemoryFs::new();
        fs.file("a.txt", "one\n");
        let error = run(
            &fs,
            serde_json::json!({"path": "a.txt", "old_string": "nope", "new_string": "yes"}),
        )
        .await
        .expect_err("no match");
        assert!(
            matches!(&error, ToolError::InvalidArgs(m) if m.contains("does not occur")),
            "{error}"
        );
        assert_eq!(fs.read_text_file("a.txt").await.expect("read"), "one\n");
    }

    #[tokio::test]
    async fn an_ambiguous_anchor_names_the_count_and_changes_nothing() {
        let fs = MemoryFs::new();
        fs.file("a.txt", "same\nsame\n");
        let error = run(
            &fs,
            serde_json::json!({"path": "a.txt", "old_string": "same", "new_string": "other"}),
        )
        .await
        .expect_err("two matches");
        assert!(
            matches!(
                &error,
                ToolError::InvalidArgs(m)
                    if m.contains("occurs 2 times") && m.contains("more context")
            ),
            "{error}"
        );
        assert_eq!(
            fs.read_text_file("a.txt").await.expect("read"),
            "same\nsame\n"
        );
    }

    #[tokio::test]
    async fn an_empty_anchor_is_invalid_args() {
        let fs = MemoryFs::new();
        fs.file("a.txt", "one\n");
        let error = run(
            &fs,
            serde_json::json!({"path": "a.txt", "old_string": "", "new_string": "x"}),
        )
        .await
        .expect_err("empty anchor");
        assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
    }

    #[tokio::test]
    async fn crlf_bytes_outside_the_match_survive_untouched() {
        let fs = MemoryFs::new();
        fs.file("a.txt", "a\r\nb\r\nc\r\n");
        run(
            &fs,
            serde_json::json!({"path": "a.txt", "old_string": "b", "new_string": "B"}),
        )
        .await
        .expect("edit");
        assert_eq!(
            fs.read_text_file("a.txt").await.expect("read"),
            "a\r\nB\r\nc\r\n"
        );
    }

    #[tokio::test]
    async fn environment_files_offer_only_the_once_options() {
        let fs = MemoryFs::new();
        fs.file(".env", "A=1\n");
        let request = Edit::new()
            .needs_approval(
                &ctx(&fs),
                &serde_json::json!({"path": ".env", "old_string": "A=1", "new_string": "A=2"}),
            )
            .await
            .expect("a write asks");
        assert_eq!(
            request.options,
            [ApprovalOption::AllowOnce, ApprovalOption::Deny]
        );
    }

    #[tokio::test]
    async fn the_preview_is_a_modified_file_holding_the_changed_line() {
        let fs = MemoryFs::new();
        fs.file("src/main.rs", "let a = 1;\nlet b = 2;\n");
        let request = Edit::new()
            .needs_approval(
                &ctx(&fs),
                &serde_json::json!({
                    "path": "src/main.rs",
                    "old_string": "let a = 1;",
                    "new_string": "let a = 10;"
                }),
            )
            .await
            .expect("a write asks");
        assert_eq!(request.risk, RiskLevel::WritesWorkspace);
        let ApprovalPreview::Diff(diff) = request.preview.expect("preview") else {
            panic!("expected a diff preview");
        };
        let file = &diff.files[0];
        assert_eq!(file.status, DiffStatus::Modified);
        assert!(
            file.hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .any(|line| line.kind == DiffLineKind::Added && line.text == "let a = 10;"),
            "the changed line is in the preview: {file:?}"
        );
    }

    /// A call certain to fail is not worth the user's attention.
    #[tokio::test]
    async fn an_unmatchable_call_asks_for_no_approval() {
        let fs = MemoryFs::new();
        fs.file("a.txt", "one\n");
        let request = Edit::new()
            .needs_approval(
                &ctx(&fs),
                &serde_json::json!({"path": "a.txt", "old_string": "zzz", "new_string": "x"}),
            )
            .await;
        assert!(request.is_none());

        let missing = Edit::new()
            .needs_approval(
                &ctx(&fs),
                &serde_json::json!({"path": "nope.txt", "old_string": "a", "new_string": "b"}),
            )
            .await;
        assert!(missing.is_none(), "an unreadable file asks for nothing");
    }

    /// The file may have moved on between the prompt and the call: execution must anchor to what
    /// is on disk now, not to the copy the preview was computed from.
    #[tokio::test]
    async fn a_file_changed_after_approval_is_re_read_at_execute_time() {
        let fs = MemoryFs::new();
        fs.file("note.txt", "one\ntwo\n");
        let args = serde_json::json!({
            "path": "note.txt",
            "old_string": "one",
            "new_string": "ONE"
        });
        Edit::new()
            .needs_approval(&ctx(&fs), &args)
            .await
            .expect("a write asks");

        fs.file("note.txt", "zero\none\ntwo\n");
        let output = Edit::new().execute(ctx(&fs), args).await.expect("edit");
        assert_eq!(output.text, "edited note.txt (+1 -1)");
        assert_eq!(
            fs.read_text_file("note.txt").await.expect("read"),
            "zero\nONE\ntwo\n"
        );
    }

    #[test]
    fn the_summary_is_the_path() {
        assert_eq!(
            Edit::new()
                .summarize(&serde_json::json!({"path": "src/main.rs"}))
                .title,
            "edit src/main.rs"
        );
    }
}
