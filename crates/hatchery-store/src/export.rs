//! JSONL export (ADR-0002's escape hatch).
//!
//! One file per session, one item per line. The format is deliberately boring: a reader only needs
//! to know what an item looks like, which is already the protocol's business.
//!
//! The actor passes the rows in, so this file holds only the rendering and the file write — both of
//! which are pure enough to test without a database.

use std::collections::HashMap;
use std::path::Path;

use hatchery_protocol::{Item, ItemId};

use crate::error::StoreError;

/// The format version written into every line.
///
/// A reader that does not know this number must refuse the file rather than guess: a future
/// version may change what a field means, and silently misreading history is worse than not
/// reading it.
pub const FORMAT_VERSION: u32 = 1;

/// One line to write: an item, and where it sits in the tree when branches are included.
#[derive(Clone, Copy, Debug)]
pub struct ExportLine<'a> {
    /// The item.
    pub item: &'a Item,
    /// The tips of every branch this item is an ancestor of. `None` for an active-branch export.
    pub branches: Option<&'a [ItemId]>,
}

/// Renders the lines.
///
/// # Errors
///
/// Fails only if an item is not representable as JSON, which no item is.
pub fn render(lines: &[ExportLine<'_>]) -> Result<String, StoreError> {
    let mut body = String::new();
    for line in lines {
        let mut object = serde_json::Map::new();
        object.insert("v".to_owned(), serde_json::Value::from(FORMAT_VERSION));
        object.insert(
            "item".to_owned(),
            serde_json::to_value(line.item).map_err(StoreError::database)?,
        );
        if let Some(branches) = line.branches {
            object.insert(
                "branches".to_owned(),
                serde_json::to_value(branches).map_err(StoreError::database)?,
            );
        }
        let text = serde_json::to_string(&serde_json::Value::Object(object))
            .map_err(StoreError::database)?;
        body.push_str(&text);
        body.push('\n');
    }
    Ok(body)
}

/// Writes the body, refusing to overwrite an existing file.
///
/// Blocking `std::fs`: the tokio feature set has no `fs` module, and an export is a rare,
/// user-initiated operation over a file that is usually a few kilobytes. Blocking the writer actor
/// for that is cheaper than adding a feature for it.
///
/// # Errors
///
/// [`StoreError::ExportExists`] when the target is already there — an audit export must never
/// silently replace an earlier one. The refusal is `create_new` (O_CREAT|O_EXCL), which is
/// atomic: an `exists()` check followed by a write is a TOCTOU race, and this write runs outside
/// the writer actor, so two concurrent exports of the same session are possible.
pub fn write(path: &Path, body: &str) -> Result<u64, StoreError> {
    use std::fs::OpenOptions;
    use std::io::{ErrorKind, Write};

    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            return Err(StoreError::ExportExists(path.to_path_buf()));
        }
        Err(error) => return Err(StoreError::database(error)),
    };
    if let Err(error) = file.write_all(body.as_bytes()) {
        // Take the half-written file back out: leaving it would refuse the retry (`create_new`
        // now sees it) and leave the operator guessing which artifact is the broken one.
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(StoreError::database(error));
    }
    Ok(body.lines().count() as u64)
}

/// Renders an active-branch export.
///
/// # Errors
///
/// See [`render`].
pub fn render_chain(items: &[Item]) -> Result<String, StoreError> {
    let lines: Vec<ExportLine<'_>> = items
        .iter()
        .map(|item| ExportLine {
            item,
            branches: None,
        })
        .collect();
    render(&lines)
}

/// Renders a whole-tree export, annotating each item with the branches below it.
///
/// # Errors
///
/// See [`render`].
pub fn render_all_branches(
    items: &[Item],
    tips: &HashMap<ItemId, Vec<ItemId>>,
) -> Result<String, StoreError> {
    let empty: Vec<ItemId> = Vec::new();
    let lines: Vec<ExportLine<'_>> = items
        .iter()
        .map(|item| ExportLine {
            item,
            branches: Some(tips.get(&item.id).map_or(empty.as_slice(), Vec::as_slice)),
        })
        .collect();
    render(&lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_protocol::{Content, ItemKind, SessionId, Timestamp};

    fn item(text: &str) -> Item {
        Item::new(SessionId::new(), ItemKind::UserMessage(Content::text(text)))
            .with_created_at(Timestamp::from_unix_millis(1))
    }

    #[test]
    fn a_line_is_versioned_and_carries_the_item() {
        let one = item("hello");
        let body = render_chain(std::slice::from_ref(&one)).expect("render");
        assert_eq!(body.lines().count(), 1);

        let parsed: serde_json::Value = serde_json::from_str(body.trim()).expect("valid json");
        assert_eq!(parsed["v"], FORMAT_VERSION);
        assert_eq!(parsed["item"]["payload"]["text"], "hello");
        assert!(
            parsed.get("branches").is_none(),
            "an active-branch export says nothing about branches: {parsed}"
        );
    }

    #[test]
    fn a_whole_tree_export_names_the_branches_below_each_item() {
        let shared = item("shared");
        let kept = item("kept");
        let mut tips = HashMap::new();
        tips.insert(shared.id, vec![kept.id]);

        let body = render_all_branches(&[shared.clone(), kept.clone()], &tips).expect("render");
        let lines: Vec<serde_json::Value> = body
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid json"))
            .collect();
        assert_eq!(lines[0]["branches"][0], kept.id.to_string());
        assert_eq!(
            lines[1]["branches"],
            serde_json::json!([]),
            "an unknown item gets an empty list, not a missing key"
        );
    }

    #[test]
    fn an_existing_file_is_not_overwritten() {
        let dir = tempfile::tempdir().expect("a tempdir");
        let path = dir.path().join("export.jsonl");
        std::fs::write(&path, "already here\n").expect("seed the file");

        let error = write(&path, "new\n").expect_err("must refuse");
        assert!(matches!(error, StoreError::ExportExists(_)));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "already here\n",
            "the earlier export is untouched"
        );
    }

    #[test]
    fn writing_reports_the_line_count() {
        let dir = tempfile::tempdir().expect("a tempdir");
        let path = dir.path().join("export.jsonl");
        let one = item("one");
        let body = render_chain(&[one.clone(), one]).expect("render");
        assert_eq!(write(&path, &body).expect("write"), 2);
    }
}
