//! Large tool output: spill it to a file and hand the model a bounded preview (D12).
//!
//! One policy for every tool, applied where the output leaves the backend. The alternatives were
//! "each tool caps itself in band" (today's state: `read_file` at 256 KiB, `glob` at 1000 paths,
//! `grep` at 200 matches) and "a tool decides to spill". The seam is the right owner because the
//! decision is about the model's context, not about what the tool means: a shell log and a fetched
//! page have the same problem, and a per-tool cap has to be re-derived by every new tool.
//!
//! The threshold is 64 KiB of inline text. The preview keeps the head and the tail (16 KiB each):
//! a build log's tail explains the failure, a page's head explains the document, and one shape has
//! to serve both. The full text is written to the session's state directory, never truncated, and
//! is redacted first so a secret cannot hide in the file the marker points at.
//!
//! A tool that already spilled (a producer richer than this layer) is left alone beyond
//! redaction. A spill that fails is not an error for the call: the model gets a truncated result
//! plus the reason, which is better than losing the whole command.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use hatchery_protocol::{SpilledOutput, ToolOutput};

use crate::redact::redact;

/// Inline text above this many bytes is spilled to a file.
pub const MAX_INLINE_BYTES: usize = 64 * 1024;
const PREVIEW_HEAD: usize = 16 * 1024;
const PREVIEW_TAIL: usize = 16 * 1024;

/// Where a full tool output can be written.
///
/// A trait rather than a directory because the testkit's registry must be able to assert the
/// spill without touching the disk. The daemon supplies [`FileSpill`]; tests supply a recording
/// fake.
#[async_trait]
pub trait SpillSink: Send + Sync {
    /// Writes `text` and returns where it went.
    ///
    /// # Errors
    ///
    /// A human-readable reason when the file could not be written. The caller degrades to a
    /// truncated result; a failed spill must not fail the tool call.
    async fn write(&self, tool: &str, text: &str) -> Result<PathBuf, String>;
}

/// Files under one directory, one file per spill.
///
/// Creation is `create_new`, so two spills in the same millisecond cannot overwrite each other;
/// on unix the file is created mode 0600 because its contents came out of a user's workspace or
/// the network.
pub struct FileSpill {
    dir: PathBuf,
    sequence: AtomicU64,
}

impl FileSpill {
    /// Spills into `dir`, creating it on first write.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            sequence: AtomicU64::new(0),
        }
    }

    /// The directory this sink writes to.
    #[must_use]
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

#[async_trait]
impl SpillSink for FileSpill {
    async fn write(&self, tool: &str, text: &str) -> Result<PathBuf, String> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0);
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst);
        let name = format!(
            "{stamp}-{}-{sequence}-{}.txt",
            std::process::id(),
            sanitize(tool)
        );
        let path = self.dir.join(name);
        let dir = self.dir.clone();
        let text = text.to_owned();
        let written = path.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<PathBuf> {
            std::fs::create_dir_all(&dir)?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut file = options.open(&written)?;
            use std::io::Write as _;
            file.write_all(text.as_bytes())?;
            Ok(written)
        })
        .await
        .map_err(|error| format!("spill task: {error}"))?
        .map_err(|error| format!("spill file: {error}"))
    }
}

/// Redacts, then spills or truncates an oversized result.
///
/// Called once per invocation by the tool registry; the returned value is what the kernel
/// records and what the model sees.
pub async fn contain(
    tool: &str,
    mut output: ToolOutput,
    spill: Option<&dyn SpillSink>,
) -> ToolOutput {
    output.text = redact(&output.text).into_owned();
    if output.spilled.is_some() || output.text.len() <= MAX_INLINE_BYTES {
        return output;
    }
    let bytes = output.text.len() as u64;
    match spill {
        Some(sink) => match sink.write(tool, &output.text).await {
            Ok(path) => {
                output.text = preview(&output.text, &path, bytes);
                output.spilled = Some(SpilledOutput {
                    path,
                    bytes: Some(bytes),
                });
            }
            Err(error) => {
                output.text = truncated(&output.text, &format!("spill failed: {error}"));
            }
        },
        None => {
            output.text = truncated(&output.text, "no spill directory is configured");
        }
    }
    output
}

fn preview(text: &str, path: &std::path::Path, bytes: u64) -> String {
    let head = floor_boundary(text, PREVIEW_HEAD);
    let tail_start = ceil_boundary(text, text.len().saturating_sub(PREVIEW_TAIL));
    format!(
        "{}\n… {bytes} bytes in total; full output at {} …\n{}",
        &text[..head],
        path.display(),
        &text[tail_start..]
    )
}

fn truncated(text: &str, reason: &str) -> String {
    let head = floor_boundary(text, PREVIEW_HEAD);
    let tail_start = ceil_boundary(text, text.len().saturating_sub(PREVIEW_TAIL));
    format!(
        "{}\n… {} bytes truncated ({reason}) …\n{}",
        &text[..head],
        text.len(),
        &text[tail_start..]
    )
}

fn floor_boundary(text: &str, maximum: usize) -> usize {
    let mut index = maximum.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_boundary(text: &str, minimum: usize) -> usize {
    let mut index = minimum.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn sanitize(tool: &str) -> String {
    let cleaned: String = tool
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .take(32)
        .collect();
    if cleaned.is_empty() {
        "tool".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_small_result_is_returned_unchanged_and_not_spilled() {
        let output = contain("read_file", ToolOutput::text("small"), None).await;
        assert_eq!(output.text, "small");
        assert!(output.spilled.is_none());
    }

    #[tokio::test]
    async fn a_large_result_is_written_and_replaced_by_a_head_and_tail_preview() {
        let dir = tempfile::tempdir().expect("a directory");
        let sink = FileSpill::new(dir.path());
        let text = format!(
            "{}{}",
            "H".repeat(MAX_INLINE_BYTES),
            "T".repeat(MAX_INLINE_BYTES)
        );
        let output = contain("shell", ToolOutput::text(text.clone()), Some(&sink)).await;
        let spilled = output.spilled.expect("spilled");
        assert_eq!(
            std::fs::read_to_string(&spilled.path).expect("the file"),
            text
        );
        assert_eq!(spilled.bytes, Some(text.len() as u64));
        assert!(output.text.starts_with('H'));
        assert!(output.text.ends_with('T'));
        assert!(output.text.contains("full output at"));
    }

    #[tokio::test]
    async fn a_failed_spill_degrades_to_a_truncated_result_not_an_error() {
        // A file where the directory should be makes create_dir_all fail.
        let dir = tempfile::tempdir().expect("a directory");
        let blocker = dir.path().join("blocked");
        std::fs::write(&blocker, "not a directory").expect("write");
        let sink = FileSpill::new(&blocker);
        let output = contain(
            "shell",
            ToolOutput::text("x".repeat(MAX_INLINE_BYTES * 2)),
            Some(&sink),
        )
        .await;
        assert!(output.spilled.is_none());
        assert!(output.text.contains("spill failed"));
        assert!(output.text.len() < MAX_INLINE_BYTES * 2);
    }

    #[tokio::test]
    async fn redaction_happens_before_the_file_is_written() {
        let dir = tempfile::tempdir().expect("a directory");
        let sink = FileSpill::new(dir.path());
        let secret = "sk-proj-AbCdEfGhIjKlMnOpQrStUvWx";
        let text = format!("{secret}\n{}", "x".repeat(MAX_INLINE_BYTES));
        let output = contain("web_fetch", ToolOutput::text(text), Some(&sink)).await;
        let path = output.spilled.expect("spilled").path;
        let written = std::fs::read_to_string(path).expect("the file");
        assert!(
            !written.contains(secret),
            "the spilled file kept the secret"
        );
        assert!(written.contains("[redacted credential]"));
        assert!(!output.text.contains(secret));
    }

    #[test]
    fn a_multibyte_character_is_never_split_at_a_boundary() {
        let text = "é".repeat(MAX_INLINE_BYTES);
        let out = preview(&text, std::path::Path::new("/tmp/x"), text.len() as u64);
        assert!(out.is_char_boundary(0));
        for (index, _) in out.char_indices() {
            assert!(out.is_char_boundary(index));
        }
    }
}
