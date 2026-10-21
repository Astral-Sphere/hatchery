//! The approval preview a write tool shows before anything reaches the disk (D14).
//!
//! The prompt has to be decidable in seconds, so the preview is deliberately bounded. A
//! megabyte diff of a generated file would defeat that: it costs the user more attention than
//! reading the file ever would, and the UI has to render it before the user can even decide to
//! deny. Past the caps below the preview becomes an honest excerpt of the new content instead of
//! a diff that claims to be complete.

use similar::{ChangeTag, TextDiff};

use hatchery_protocol::{ApprovalPreview, Diff, DiffFile, DiffHunk, DiffLineKind, DiffStatus};

/// Above this combined size the structured diff is not worth building for a human to read.
const PREVIEW_BYTE_CAP: usize = 256 * 1024;

/// Above this many added + removed lines the same is true: the prompt stops being a decision.
const PREVIEW_CHANGED_LINE_CAP: usize = 200;

/// How much of the new content an over-cap preview shows.
const EXCERPT_CHARS: usize = 4_000;

/// Context lines on each side of a change, matching the unified-diff convention.
const CONTEXT_LINES: usize = 3;

/// The preview for replacing `old` with `new`, or `None` when the bytes are identical.
///
/// `None` is not an error: the tool will write the same content, and an unchanged tree is already
/// a no-op for the checkpoint store, so there is nothing the user could decide.
#[must_use]
pub fn diff_preview(path: &str, existed: bool, old: &str, new: &str) -> Option<ApprovalPreview> {
    if old == new {
        return None;
    }
    // Decided before the diff is built: a megabyte pair is exactly the case where computing the
    // structured diff to then throw it away would cost the most.
    if old.len() + new.len() > PREVIEW_BYTE_CAP {
        return Some(excerpt(new));
    }

    let text_diff = TextDiff::from_lines(old, new);
    let groups = text_diff.grouped_ops(CONTEXT_LINES);
    let mut hunks = Vec::with_capacity(groups.len());
    let mut changed_lines = 0_usize;

    for group in &groups {
        let mut first_old = None;
        let mut first_new = None;
        let mut old_lines = 0_u32;
        let mut new_lines = 0_u32;
        let mut lines = Vec::new();

        for op in group {
            for change in text_diff.iter_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => DiffLineKind::Context,
                    ChangeTag::Insert => DiffLineKind::Added,
                    ChangeTag::Delete => DiffLineKind::Removed,
                };
                if kind != DiffLineKind::Context {
                    changed_lines += 1;
                }
                if let Some(index) = change.old_index() {
                    first_old.get_or_insert(index + 1);
                    old_lines += 1;
                }
                if let Some(index) = change.new_index() {
                    first_new.get_or_insert(index + 1);
                    new_lines += 1;
                }
                lines.push((kind, strip_line_end(change.value()).to_owned()));
            }
        }

        // A pure insertion (or deletion) has no line on one side, so its index list is empty.
        // Borrowing the other side's first line keeps the header at 1 rather than the 0 that
        // would spell an empty side's index; a hunk header must never read `-0`/`+0` unless the
        // file itself is empty, and here it is not.
        let old_start = first_old.or(first_new).unwrap_or(1);
        let new_start = first_new.or(first_old).unwrap_or(1);
        let mut hunk = DiffHunk::new(to_u32(old_start), old_lines, to_u32(new_start), new_lines);
        for (kind, text) in lines {
            hunk.push(kind, text);
        }
        hunks.push(hunk);
    }

    if old.len() + new.len() > PREVIEW_BYTE_CAP || changed_lines > PREVIEW_CHANGED_LINE_CAP {
        return Some(excerpt(new));
    }

    let status = if existed {
        DiffStatus::Modified
    } else {
        DiffStatus::Added
    };
    Some(ApprovalPreview::Diff(Diff {
        files: vec![DiffFile {
            path: path.to_owned(),
            old_path: None,
            status,
            binary: false,
            hunks,
        }],
    }))
}

/// An honest fallback: the head of the new content, followed by what was left out and why.
fn excerpt(new: &str) -> ApprovalPreview {
    let total = new.chars().count();
    let shown: String = new.chars().take(EXCERPT_CHARS).collect();
    let notice = if total > EXCERPT_CHARS {
        format!(
            "[preview truncated: the change is too large to diff; showing the first {} of {} \
             characters of the new content]",
            shown.chars().count(),
            total
        )
    } else {
        format!(
            "[preview truncated: the change is too large to diff; showing all {} characters of \
             the new content]",
            total
        )
    };
    ApprovalPreview::Excerpt(format!("{shown}\n{notice}"))
}

/// One line's text without its terminator: `\n`, plus the `\r` of a CRLF pair.
fn strip_line_end(text: &str) -> &str {
    let Some(text) = text.strip_suffix('\n') else {
        return text;
    };
    text.strip_suffix('\r').unwrap_or(text)
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_protocol::DiffLineKind;

    fn diff_of(preview: ApprovalPreview) -> Diff {
        match preview {
            ApprovalPreview::Diff(diff) => diff,
            other => panic!("expected a diff, got {other:?}"),
        }
    }

    #[test]
    fn a_modified_file_has_one_hunk_with_the_right_counts_and_kinds() {
        let preview = diff_preview("a.txt", true, "one\ntwo\nthree\n", "one\nTWO\nthree\n")
            .expect("a change is visible");
        let diff = diff_of(preview);
        assert_eq!(diff.files.len(), 1);
        let file = &diff.files[0];
        assert_eq!(file.path, "a.txt");
        assert_eq!(file.status, DiffStatus::Modified);
        assert!(!file.binary);
        assert_eq!(file.hunks.len(), 1);

        let hunk = &file.hunks[0];
        assert_eq!((hunk.old_start, hunk.old_lines), (1, 3));
        assert_eq!((hunk.new_start, hunk.new_lines), (1, 3));
        let kinds: Vec<DiffLineKind> = hunk.lines.iter().map(|line| line.kind).collect();
        assert_eq!(
            kinds,
            [
                DiffLineKind::Context,
                DiffLineKind::Removed,
                DiffLineKind::Added,
                DiffLineKind::Context,
            ]
        );
        assert_eq!(hunk.lines[1].text, "two");
        assert_eq!(hunk.lines[2].text, "TWO");
    }

    #[test]
    fn a_new_file_is_added_and_has_only_added_lines() {
        let preview = diff_preview("new.txt", false, "", "first\nsecond\n").expect("diff");
        let diff = diff_of(preview);
        let file = &diff.files[0];
        assert_eq!(file.status, DiffStatus::Added);
        let hunk = &file.hunks[0];
        assert_eq!((hunk.old_start, hunk.old_lines), (1, 0));
        assert_eq!((hunk.new_start, hunk.new_lines), (1, 2));
        assert!(
            hunk.lines
                .iter()
                .all(|line| line.kind == DiffLineKind::Added),
            "{:?}",
            hunk.lines
        );
    }

    #[test]
    fn identical_content_has_nothing_to_show() {
        assert!(diff_preview("a.txt", true, "same\n", "same\n").is_none());
    }

    #[test]
    fn a_very_large_input_falls_back_to_a_truncated_excerpt() {
        let old = "x\n".repeat(300_000);
        let new = format!("{old}y\n");
        let preview = diff_preview("big.txt", true, &old, &new).expect("diff");
        let ApprovalPreview::Excerpt(excerpt) = preview else {
            panic!("a megabyte diff must not be shown as one");
        };
        assert!(
            excerpt.contains("[preview truncated:"),
            "the excerpt says it is not the whole file: {excerpt}"
        );
        assert!(
            excerpt.chars().count() < old.len() / 2,
            "the excerpt is bounded"
        );
    }

    #[test]
    fn crlf_terminators_are_stripped_from_line_text() {
        let preview = diff_preview("a.txt", true, "a\r\nb\r\n", "a\r\nB\r\n").expect("diff");
        let diff = diff_of(preview);
        let hunk = &diff.files[0].hunks[0];
        assert_eq!(hunk.lines[0].text, "a");
        assert_eq!(hunk.lines[1].text, "b");
        assert_eq!(hunk.lines[2].text, "B");
    }
}
