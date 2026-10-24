//! Tool call and tool result value types.
//!
//! These are shared with the kernel (`ToolHost::invoke` returns a [`ToolOutput`]) and persisted
//! inside `ItemKind::ToolCall` / `ItemKind::ToolResult`, which is why they live in the shared
//! vocabulary rather than next to the tool implementations (M0b layering decision — see
//! `docs/architecture.md` §3).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where a tool call is in its lifecycle.
///
/// `Denied` is not an error: a user refusing a write is an outcome the UI renders differently
/// from a command that failed, and the model is told about it as a normal tool result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// Recorded but not started (the model emitted the call, execution has not begun).
    Pending,
    /// Executing.
    Running,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
    /// Refused by the approval gate.
    Denied,
    /// Interrupted before it finished.
    Cancelled,
}

impl ToolStatus {
    /// True once the call will not change state any more.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Denied | Self::Cancelled
        )
    }
}

/// What a tool produced.
///
/// A struct with optional halves rather than an enum: `docs/design/capabilities.md` §3 describes
/// it as `{ text, artifacts?, spilled? }` and `docs/design/kernel.md` §5 as a `Spilled` variant,
/// but the honest shape is that a spilled result still has a preview *text* — and an enum would
/// force every reader to handle an either/or that does not exist. Optional fields also keep the
/// wire model additive.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// The text form: what the model sees and what the UI renders.
    pub text: String,
    /// Files the tool produced or referenced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ToolArtifact>,
    /// Set when the full output was written to disk and `text` is only a preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spilled: Option<SpilledOutput>,
}

impl ToolOutput {
    /// A plain text result, the common case.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            artifacts: Vec::new(),
            spilled: None,
        }
    }

    /// A preview plus a reference to the file holding the full output.
    #[must_use]
    pub fn spilled(preview: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            text: preview.into(),
            artifacts: Vec::new(),
            spilled: Some(SpilledOutput {
                path: path.into(),
                bytes: None,
            }),
        }
    }

    /// The text as long as nothing was spilled; callers that must hand a single string to a
    /// provider use this to detect the simple case.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        self.spilled.is_none().then_some(self.text.as_str())
    }
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl From<&str> for ToolOutput {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

/// A file a tool produced or referenced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolArtifact {
    /// Where the file is.
    pub path: PathBuf,
    /// MIME type when the tool knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// A reference to an oversized tool result that was written to disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpilledOutput {
    /// Where the full output lives.
    pub path: PathBuf,
    /// Size of the full output, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// A progress chunk emitted while a tool runs, surfaced as `ToolCallProgress`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProgress {
    /// Incremental output. May be a partial line: the tool decides chunk boundaries.
    pub chunk: String,
}

impl From<String> for ToolProgress {
    fn from(chunk: String) -> Self {
        Self { chunk }
    }
}

/// The human-readable summary of a tool call, for UIs that must not re-derive meaning from raw
/// arguments.
///
/// Borrowed from ACP's tool-call updates: the model's arguments are JSON and can be enormous,
/// while a timeline wants one scannable line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallSummary {
    /// One line a human can scan, e.g. `edit src/main.rs (+12 -3)`.
    pub title: String,
    /// A second line with the detail a curious user needs: the full command, the matched paths,
    /// the search pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ToolCallSummary {
    /// A one-line summary.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: None,
        }
    }

    /// Adds the detail line.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_result_has_a_minimal_wire_form() {
        let output = ToolOutput::text("42 files changed");
        assert_eq!(
            serde_json::to_string(&output).expect("serialize"),
            r#"{"text":"42 files changed"}"#
        );
        assert_eq!(output.as_text(), Some("42 files changed"));
    }

    #[test]
    fn a_spilled_result_keeps_its_preview_and_reference() {
        let output = ToolOutput::spilled("first 200 lines…", "/tmp/state/tool-results/7.txt");
        let json = serde_json::to_string(&output).expect("serialize");
        assert!(
            json.contains(r#""path":"/tmp/state/tool-results/7.txt""#),
            "{json}"
        );
        let back: ToolOutput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, output);
        assert_eq!(
            back.as_text(),
            None,
            "a spilled result has no single text form to hand to a provider"
        );
    }

    #[test]
    fn artifacts_survive_a_roundtrip_without_null_padding() {
        let output = ToolOutput {
            text: "wrote two files".to_owned(),
            artifacts: vec![
                ToolArtifact {
                    path: PathBuf::from("/ws/a.rs"),
                    mime_type: Some("text/x-rust".to_owned()),
                },
                ToolArtifact {
                    path: PathBuf::from("/ws/b.rs"),
                    mime_type: None,
                },
            ],
            spilled: Some(SpilledOutput {
                path: PathBuf::from("/tmp/out.log"),
                bytes: Some(4096),
            }),
        };
        let json = serde_json::to_string(&output).expect("serialize");
        assert!(!json.contains("null"), "{json}");
        assert_eq!(
            serde_json::from_str::<ToolOutput>(&json).expect("deserialize"),
            output
        );
    }

    #[test]
    fn denied_is_terminal_but_pending_is_not() {
        assert!(ToolStatus::Denied.is_terminal());
        assert!(ToolStatus::Cancelled.is_terminal());
        assert!(!ToolStatus::Pending.is_terminal());
        assert!(!ToolStatus::Running.is_terminal());
    }

    #[test]
    fn summaries_omit_the_detail_line_when_absent() {
        let summary = ToolCallSummary::new("read src/main.rs");
        assert_eq!(
            serde_json::to_string(&summary).expect("serialize"),
            r#"{"title":"read src/main.rs"}"#
        );
        let with_detail = summary.with_detail("1-40 of 120 lines");
        assert!(with_detail.detail.is_some());
    }
}
