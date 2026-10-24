//! What changed, as structured hunks.
//!
//! One type serves both producers of a diff, which is why it lives in the shared vocabulary
//! rather than next to either of them:
//!
//! - **checkpoint → checkpoint**, from the shadow Git (ADR-0006): `git2` yields hunk headers and
//!   +/- lines natively, so this shape is a transcription;
//! - **buffer → file**, the preview a `write_file`/`edit` approval needs before anything is on
//!   disk: there is no commit yet, so the hunks come from a text-diff crate instead (D11 already
//!   rules for the TUI that `similar` computes them).
//!
//! Both frontends then render the same value. The alternative — carrying unified-diff *text* and
//! parsing it where it is displayed — is what codex does, and it costs each renderer a parser
//! (`references/codex/codex-rs/tui/src/diff_render.rs` is 2745 lines, most of it re-deriving
//! structure the producer already had).
//!
//! # Wire shape
//!
//! ```json
//! {
//!   "files": [
//!     {
//!       "path": "src/main.rs",
//!       "status": "modified",
//!       "hunks": [
//!         {
//!           "old_start": 1, "old_lines": 2,
//!           "new_start": 1, "new_lines": 3,
//!           "lines": [
//!             { "kind": "context", "text": "fn main() {" },
//!             { "kind": "removed", "text": "    old()" },
//!             { "kind": "added", "text": "    new()" },
//!             { "kind": "added", "text": "    more()" }
//!           ]
//!         }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! No golden fixture pins this yet: `Diff` is not an [`crate::ItemKind`] payload, a method result
//! or an event, and the fixture registry covers exactly those three categories. The golden
//! arrives with the first method that returns one (the checkpoint-diff tool, or `session/rewind`'s
//! report growing a diff). Until then the serde spelling is pinned by the unit tests below.

use serde::{Deserialize, Serialize};

/// A set of file-level differences, in the order the producer walked them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    /// One entry per changed file. Empty means "nothing changed", which is a real answer: a
    /// rewind target with no checkpoint after it is a no-op, not an error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<DiffFile>,
}

impl Diff {
    /// An empty diff.
    #[must_use]
    pub const fn new() -> Self {
        Self { files: Vec::new() }
    }

    /// Appends one file's differences.
    pub fn push_file(&mut self, file: DiffFile) {
        self.files.push(file);
    }

    /// True when nothing changed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Total `(added, removed)` line counts across every file — the `(+12 -3)` a timeline wants
    /// without walking the hunks itself.
    #[must_use]
    pub fn line_counts(&self) -> (usize, usize) {
        let mut added = 0;
        let mut removed = 0;
        for file in &self.files {
            for hunk in &file.hunks {
                for line in &hunk.lines {
                    match line.kind {
                        DiffLineKind::Added => added += 1,
                        DiffLineKind::Removed => removed += 1,
                        DiffLineKind::Context => {}
                    }
                }
            }
        }
        (added, removed)
    }
}

/// How one file changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFile {
    /// The path after the change, workspace-relative and `/`-separated — the same convention the
    /// capability seam uses for every path (`docs/design/capabilities.md` §1).
    pub path: String,
    /// Where it came from, for a rename or a copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// What kind of change this is.
    pub status: DiffStatus,
    /// True when the content is not text, so `hunks` is empty and a renderer must say "binary"
    /// instead of showing nothing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub binary: bool,
    /// The changed regions. Empty for a binary file, and empty for a pure rename with no edits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hunks: Vec<DiffHunk>,
}

impl DiffFile {
    /// A text file that changed in place.
    #[must_use]
    pub fn modified(path: impl Into<String>, hunks: Vec<DiffHunk>) -> Self {
        Self {
            path: path.into(),
            old_path: None,
            status: DiffStatus::Modified,
            binary: false,
            hunks,
        }
    }

    /// A file that did not exist before.
    #[must_use]
    pub fn added(path: impl Into<String>, hunks: Vec<DiffHunk>) -> Self {
        Self {
            path: path.into(),
            old_path: None,
            status: DiffStatus::Added,
            binary: false,
            hunks,
        }
    }

    /// A file that no longer exists.
    #[must_use]
    pub fn deleted(path: impl Into<String>, hunks: Vec<DiffHunk>) -> Self {
        Self {
            path: path.into(),
            old_path: None,
            status: DiffStatus::Deleted,
            binary: false,
            hunks,
        }
    }

    /// A binary file, whose content a renderer cannot show.
    #[must_use]
    pub fn binary(path: impl Into<String>, status: DiffStatus) -> Self {
        Self {
            path: path.into(),
            old_path: None,
            status,
            binary: true,
            hunks: Vec::new(),
        }
    }

    /// Records where a rename or copy came from.
    #[must_use]
    pub fn with_old_path(mut self, old_path: impl Into<String>) -> Self {
        self.old_path = Some(old_path.into());
        self
    }

    /// `(added, removed)` for this file alone.
    #[must_use]
    pub fn line_counts(&self) -> (usize, usize) {
        let mut added = 0;
        let mut removed = 0;
        for hunk in &self.hunks {
            for line in &hunk.lines {
                match line.kind {
                    DiffLineKind::Added => added += 1,
                    DiffLineKind::Removed => removed += 1,
                    DiffLineKind::Context => {}
                }
            }
        }
        (added, removed)
    }
}

/// The kind of change a [`DiffFile`] records.
///
/// The variant set mirrors `git2::Delta` minus the three that describe a *working-tree* state
/// rather than a difference between two trees (`Ignored`, `Untracked`, `Conflicted`): a checkpoint
/// diff is always tree-to-tree, and a buffer-to-file preview has no notion of them either.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffStatus {
    /// Did not exist before.
    Added,
    /// Does not exist any more.
    Deleted,
    /// Changed in place.
    Modified,
    /// Moved; `DiffFile::old_path` says from where.
    Renamed,
    /// Duplicated; `DiffFile::old_path` says from where.
    Copied,
    /// Same name, different file type (a file became a directory, a symlink became a file).
    TypeChanged,
}

/// One changed region, with enough of the surrounding text to place it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    /// First line of this hunk in the old file, 1-based — the `@@ -a,b` of a unified diff.
    pub old_start: u32,
    /// How many old lines the hunk spans.
    pub old_lines: u32,
    /// First line of this hunk in the new file, 1-based — the `@@ +c,d` of a unified diff.
    pub new_start: u32,
    /// How many new lines the hunk spans.
    pub new_lines: u32,
    /// The lines themselves, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<DiffLine>,
}

impl DiffHunk {
    /// A hunk with its header and no lines yet.
    #[must_use]
    pub const fn new(old_start: u32, old_lines: u32, new_start: u32, new_lines: u32) -> Self {
        Self {
            lines: Vec::new(),
            new_lines,
            new_start,
            old_lines,
            old_start,
        }
    }

    /// Appends one line.
    pub fn push(&mut self, kind: DiffLineKind, text: impl Into<String>) {
        self.lines.push(DiffLine {
            kind,
            text: text.into(),
        });
    }
}

/// One line of a hunk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    /// Whether the line is unchanged, added or removed.
    pub kind: DiffLineKind,
    /// The line's text **without** a leading `+`/`-`/space marker: [`DiffLineKind`] already says
    /// which it is, and carrying both would let them disagree. No trailing newline either — a
    /// renderer decides how lines are joined, and a wrapped line must not inherit the wrap point.
    pub text: String,
}

impl DiffLine {
    /// An unchanged context line.
    #[must_use]
    pub fn context(text: impl Into<String>) -> Self {
        Self {
            kind: DiffLineKind::Context,
            text: text.into(),
        }
    }

    /// A line only the new side has.
    #[must_use]
    pub fn added(text: impl Into<String>) -> Self {
        Self {
            kind: DiffLineKind::Added,
            text: text.into(),
        }
    }

    /// A line only the old side has.
    #[must_use]
    pub fn removed(text: impl Into<String>) -> Self {
        Self {
            kind: DiffLineKind::Removed,
            text: text.into(),
        }
    }
}

/// Whether a [`DiffLine`] is unchanged, added, or removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    /// On both sides.
    Context,
    /// Only on the new side.
    Added,
    /// Only on the old side.
    Removed,
}

/// `skip_serializing_if` for a bool that is nearly always false.
const fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Diff {
        let mut hunk = DiffHunk::new(1, 2, 1, 3);
        hunk.push(DiffLineKind::Context, "fn main() {");
        hunk.push(DiffLineKind::Removed, "    old()");
        hunk.push(DiffLineKind::Added, "    new()");
        hunk.push(DiffLineKind::Added, "    more()");
        let mut diff = Diff::new();
        diff.push_file(DiffFile::modified("src/main.rs", vec![hunk]));
        diff.push_file(DiffFile::binary("logo.png", DiffStatus::Added));
        diff
    }

    /// The serde spelling is the wire contract: these strings appear in fixtures, in the store and
    /// on the ACP bridge, so they are pinned rather than left to `rename_all` alone.
    #[test]
    fn statuses_and_line_kinds_use_the_schema_spelling() {
        let statuses = [
            (DiffStatus::Added, "added"),
            (DiffStatus::Deleted, "deleted"),
            (DiffStatus::Modified, "modified"),
            (DiffStatus::Renamed, "renamed"),
            (DiffStatus::Copied, "copied"),
            (DiffStatus::TypeChanged, "type_changed"),
        ];
        for (status, spelling) in statuses {
            assert_eq!(serde_json::to_value(status).unwrap(), spelling);
        }

        let kinds = [
            (DiffLineKind::Context, "context"),
            (DiffLineKind::Added, "added"),
            (DiffLineKind::Removed, "removed"),
        ];
        for (kind, spelling) in kinds {
            assert_eq!(serde_json::to_value(kind).unwrap(), spelling);
        }
    }

    #[test]
    fn a_diff_round_trips_through_json() {
        let diff = sample();
        let json = serde_json::to_value(&diff).unwrap();
        assert_eq!(serde_json::from_value::<Diff>(json.clone()).unwrap(), diff);
        assert_eq!(json["files"][0]["status"], "modified");
        assert_eq!(json["files"][0]["hunks"][0]["old_start"], 1);
        assert_eq!(json["files"][0]["hunks"][0]["lines"][1]["kind"], "removed");
        assert_eq!(json["files"][1]["binary"], true);
    }

    /// A field added by a newer peer must be ignored, and one it omitted must fall back to its
    /// default (`docs/design/protocol.md` §6): both directions are what "the vocabulary only
    /// grows" has to mean in practice.
    #[test]
    fn omitted_and_unknown_fields_are_tolerated() {
        let without_optionals = serde_json::json!({
            "files": [{
                "path": "a.txt",
                "status": "added",
                "hunks": [{ "old_start": 0, "old_lines": 0, "new_start": 1, "new_lines": 1 }],
            }],
        });
        let diff: Diff = serde_json::from_value(without_optionals).unwrap();
        assert!(!diff.files[0].binary, "an omitted bool is false");
        assert!(diff.files[0].old_path.is_none());
        assert!(
            diff.files[0].hunks[0].lines.is_empty(),
            "an omitted line list is empty"
        );

        let with_an_extra_field = serde_json::json!({
            "files": [{
                "path": "a.txt",
                "status": "added",
                "syntax": "rust",
            }],
        });
        let diff: Diff = serde_json::from_value(with_an_extra_field).unwrap();
        assert_eq!(diff.files[0].path, "a.txt");
    }

    /// An unknown status is refused rather than guessed at: a diff that says "modified" when the
    /// producer meant something this version has no name for would be silently wrong.
    #[test]
    fn an_unknown_status_is_refused() {
        let json = serde_json::json!({ "files": [{ "path": "a", "status": "exploded" }] });
        assert!(serde_json::from_value::<Diff>(json).is_err());
    }

    #[test]
    fn line_counts_add_up_across_files_and_skip_binary_ones() {
        assert_eq!(sample().line_counts(), (2, 1));
        assert_eq!(Diff::new().line_counts(), (0, 0));
        assert!(Diff::new().is_empty());
        assert_eq!(sample().files[1].line_counts(), (0, 0));
    }

    #[test]
    fn a_rename_carries_its_source() {
        let file = DiffFile::modified("b.txt", Vec::new()).with_old_path("a.txt");
        let json = serde_json::to_value(&file).unwrap();
        assert_eq!(json["old_path"], "a.txt");
        assert_eq!(file.old_path.as_deref(), Some("a.txt"));
    }

    /// `binary: false` must not reach the wire: it is the common case, and a diff of a large
    /// changeset is already the biggest payload in the protocol.
    #[test]
    fn the_common_case_is_not_serialized() {
        let json = serde_json::to_value(DiffFile::modified("a.txt", Vec::new())).unwrap();
        assert!(json.get("binary").is_none(), "{json}");
        assert!(json.get("old_path").is_none(), "{json}");
        assert!(json.get("hunks").is_none(), "{json}");
    }
}
