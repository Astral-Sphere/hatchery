//! `grep`: regex search over workspace text files, through the seam.

use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;

use hatchery_capabilities::{FsError, Tool, ToolCtx, ToolError};
use hatchery_kernel::ToolDef;
use hatchery_protocol::{ApprovalRequest, ToolCallSummary, ToolOutput};

use crate::walk::walk;

/// How many matches one call returns.
pub const MAX_MATCHES: usize = 200;
/// How long one displayed line may be, in characters.
const LINE_CLIP: usize = 240;
/// Files larger than this are skipped: grep is for source, not for data dumps.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The search-content tool.
#[derive(Default)]
pub struct Grep;

impl Grep {
    /// The tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// One reported hit: `path:line: text`, clipped.
fn hit(path: &str, line_number: usize, line: &str) -> String {
    let clipped = match line.char_indices().nth(LINE_CLIP) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_owned(),
    };
    format!("{path}:{line_number}: {clipped}")
}

/// Reads one file for searching, treating unreadable and binary content as "nothing here".
///
/// grep reports what matched, not every way a file can be unsearchable; the summary counts the
/// binaries it skipped so silence stays explainable.
async fn read_for_search(
    ctx: &ToolCtx<'_>,
    path: &str,
    skipped_binaries: &mut usize,
) -> Option<String> {
    if ctx.cancel.is_cancelled() {
        return None;
    }
    if let Ok(meta) = ctx.fs.metadata(path).await
        && meta.len > MAX_FILE_BYTES
    {
        return None;
    }
    match ctx.fs.read_text_file(path).await {
        Ok(text) => Some(text),
        Err(FsError::Binary(_)) => {
            *skipped_binaries += 1;
            None
        }
        Err(_) => None,
    }
}

fn collect(regex: &Regex, path: &str, text: &str, hits: &mut Vec<String>) {
    for (index, line) in text.lines().enumerate() {
        if regex.is_match(line) {
            hits.push(hit(path, index + 1, line));
            if hits.len() >= MAX_MATCHES {
                return;
            }
        }
    }
}

#[async_trait]
impl Tool for Grep {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "grep".to_owned(),
            description: "Searches file contents with a regular expression (Rust syntax) across \
                          the workspace. Returns `path:line: text` per hit. Binary and oversized \
                          files are skipped."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regular expression."},
                    "path": {"type": "string",
                             "description": "Optional workspace-relative directory or file to search instead of the whole workspace."},
                    "include": {"type": "string",
                                "description": "Optional glob filter on file names, e.g. `*.rs`."}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn needs_approval(&self, _args: &Value) -> Option<ApprovalRequest> {
        None
    }

    fn summarize(&self, args: &Value) -> ToolCallSummary {
        let pattern = args["pattern"].as_str().unwrap_or("?");
        let mut summary = ToolCallSummary::new(format!("grep {pattern}"));
        let mut details = Vec::new();
        if let Some(path) = args["path"].as_str() {
            details.push(format!("in {path}"));
        }
        if let Some(include) = args["include"].as_str() {
            details.push(format!("matching {include}"));
        }
        if !details.is_empty() {
            summary.detail = Some(details.join(", "));
        }
        summary
    }

    async fn execute(&self, ctx: ToolCtx<'_>, args: Value) -> Result<ToolOutput, ToolError> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("grep needs a string `pattern`".to_owned()))?;
        let regex = Regex::new(pattern)
            .map_err(|error| ToolError::InvalidArgs(format!("bad regex: {error}")))?;
        let scope = args["path"].as_str().unwrap_or("");
        let filter = match args["include"].as_str() {
            Some(raw) if raw.starts_with('/') => {
                return Err(ToolError::InvalidArgs(
                    "`include` filters file names; drop the leading `/`".to_owned(),
                ));
            }
            Some(raw) => Some(
                globset::Glob::new(raw)
                    .map_err(|error| ToolError::InvalidArgs(format!("bad glob: {error}")))?
                    .compile_matcher(),
            ),
            None => None,
        };
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        let root_meta = ctx
            .fs
            .metadata(scope)
            .await
            .map_err(|error| ToolError::Backend(error.to_string()))?;
        let mut hits = Vec::new();
        let mut searched = 0_usize;
        let mut skipped_binaries = 0_usize;

        if root_meta.is_file {
            if let Some(text) = read_for_search(&ctx, scope, &mut skipped_binaries).await {
                searched += 1;
                collect(&regex, scope, &text, &mut hits);
            }
        } else {
            let mut paths = walk(&ctx, scope).await?;
            // Files before directories' deeper branches: the walk is depth-first per subtree, so
            // a stable sort keeps shallow files first, which reads better and caps sooner.
            paths.sort_by_key(|path| path.matches('/').count());
            for path in paths {
                if ctx.cancel.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }
                let named_ok = path
                    .rsplit('/')
                    .next()
                    .is_none_or(|name| filter.as_ref().is_none_or(|f| f.is_match(name)));
                if !named_ok {
                    continue;
                }
                if let Some(text) = read_for_search(&ctx, &path, &mut skipped_binaries).await {
                    searched += 1;
                    collect(&regex, &path, &text, &mut hits);
                }
                if hits.len() >= MAX_MATCHES {
                    break;
                }
            }
        }

        let mut body = hits.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&format!(
            "[{}/{} matches in {searched} file(s)]",
            hits.len(),
            MAX_MATCHES
        ));
        if hits.len() >= MAX_MATCHES {
            body.push_str(" [result cap reached; narrow the pattern or the path]");
        }
        if skipped_binaries > 0 {
            body.push_str(&format!(" [skipped {skipped_binaries} binary file(s)]"));
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
        fs.file("src/main.rs", "fn main() {\n    todo!(\"later\");\n}\n")
            .file("src/lib.rs", "pub fn helper() {}\n")
            .file("notes/todo.md", "- [ ] write tests\n")
            .file("logo.png.bin", "\0\0\0binary");
        fs
    }

    #[tokio::test]
    async fn hits_come_back_as_path_line_text() {
        let output = Grep
            .execute(ctx(&fixture()), serde_json::json!({"pattern": "todo"}))
            .await
            .expect("run");
        assert_eq!(
            output.text,
            "src/main.rs:2:     todo!(\"later\");\n[1/200 matches in 3 file(s)] [skipped 1 binary file(s)]"
        );
    }

    #[tokio::test]
    async fn include_filters_by_file_name() {
        let output = Grep
            .execute(
                ctx(&fixture()),
                serde_json::json!({"pattern": "fn ", "include": "*.rs"}),
            )
            .await
            .expect("run");
        assert!(output.text.contains("src/main.rs:1:"), "{}", output.text);
        assert!(output.text.contains("src/lib.rs:1:"), "{}", output.text);
        assert!(!output.text.contains("todo.md"), "{}", output.text);
        assert!(output.text.contains("in 2 file(s)"), "{}", output.text);
    }

    #[tokio::test]
    async fn scoping_to_a_subtree_narrows_the_search() {
        let output = Grep
            .execute(
                ctx(&fixture()),
                serde_json::json!({"pattern": "write", "path": "notes"}),
            )
            .await
            .expect("run");
        assert!(
            output.text.starts_with("notes/todo.md:1:"),
            "{}",
            output.text
        );
        assert!(output.text.contains("in 1 file(s)"), "{}", output.text);
    }

    #[tokio::test]
    async fn content_matches_even_when_the_name_does_not() {
        let output = Grep
            .execute(
                ctx(&fixture()),
                serde_json::json!({"pattern": "write tests", "path": "notes"}),
            )
            .await
            .expect("run");
        assert!(
            output
                .text
                .starts_with("notes/todo.md:1: - [ ] write tests"),
            "{}",
            output.text
        );
    }

    #[tokio::test]
    async fn bad_regex_and_missing_scope_report_their_own_kinds() {
        let error = Grep
            .execute(ctx(&fixture()), serde_json::json!({"pattern": "("}))
            .await
            .expect_err("bad regex");
        assert!(matches!(error, ToolError::InvalidArgs(m) if m.contains("bad regex")));

        let error = Grep
            .execute(
                ctx(&fixture()),
                serde_json::json!({"pattern": "x", "path": "missing"}),
            )
            .await
            .expect_err("missing scope");
        assert!(matches!(error, ToolError::Backend(_)), "{error}");
    }

    #[tokio::test]
    async fn no_hits_say_so_plainly() {
        let output = Grep
            .execute(
                ctx(&fixture()),
                serde_json::json!({"pattern": "zzz-nowhere"}),
            )
            .await
            .expect("run");
        assert_eq!(
            output.text,
            "[0/200 matches in 3 file(s)] [skipped 1 binary file(s)]"
        );
    }

    #[tokio::test]
    async fn the_binary_skip_is_counted_not_silent() {
        let fs = fixture();
        let output = Grep
            .execute(ctx(&fs), serde_json::json!({"pattern": "binary"}))
            .await
            .expect("run");
        assert_eq!(
            output.text,
            "[0/200 matches in 3 file(s)] [skipped 1 binary file(s)]"
        );
    }

    #[test]
    fn long_lines_are_clipped_with_an_ellipsis() {
        let long = "x".repeat(400);
        let line = hit("f.rs", 1, &long);
        assert!(line.chars().count() < 260, "{}", line.chars().count());
        assert!(line.ends_with('…'));
    }

    #[test]
    fn the_summary_carries_scope_and_filter() {
        let summary = Grep.summarize(&serde_json::json!({
            "pattern": "todo", "path": "notes", "include": "*.md"
        }));
        assert_eq!(summary.title, "grep todo");
        assert_eq!(summary.detail.expect("detail"), "in notes, matching *.md");
    }
}
