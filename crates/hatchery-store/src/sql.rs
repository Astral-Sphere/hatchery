//! Pragmas, row conversion and the statement shapes the writer uses.
//!
//! Kept apart from the writer actor so the actor reads as a sequence of operations rather than as
//! SQL, and so the conversions have one home.

use std::path::PathBuf;

use serde_json::Value as Json;
use turso::{Connection, Value};

use hatchery_protocol::{
    CheckpointKind, Item, ItemId, ItemKind, ItemKindTag, ModelRef, Session, SessionModeId,
    SessionStatus, Timestamp, TurnId, UnknownItemKind,
};

use crate::error::StoreError;
use crate::store::CheckpointRecord;

/// Columns of `sessions`, in the order the row readers expect.
pub const SESSION_COLUMNS: &str = "id, title, mode, workspace, model_provider, model_id, \
     config_patch, active_head, generation, status, created_at, updated_at";

/// Columns of `items`, in the order the row readers expect.
pub const ITEM_COLUMNS: &str = "id, session_id, parent_id, turn_id, kind, payload, created_at";

/// Columns of `checkpoints`, in the order the row readers expect.
pub const CHECKPOINT_COLUMNS: &str =
    "id, session_id, item_id, workspace, commit_id, kind, created_at";

/// Columns of the tree skeleton: everything the tree walk needs, no payload.
pub const SKELETON_COLUMNS: &str = "id, parent_id, kind, turn_id";

/// Applies the per-connection pragmas.
///
/// `journal_mode` goes through `query()`: it answers with a row, and the engine rejects any
/// statement that produces one when executed (`Misuse("unexpected row during execution")` — the
/// M0 spike measured this, ADR-0010). `foreign_keys` is per connection, so every connection needs
/// it, and it is what makes the cascade and the dangling-head refusal real.
pub async fn apply_pragmas(conn: &Connection) -> Result<(), StoreError> {
    conn.execute("PRAGMA foreign_keys = ON", ())
        .await
        .map_err(StoreError::database)?;
    let mut rows = conn
        .query("PRAGMA journal_mode = WAL", ())
        .await
        .map_err(StoreError::database)?;
    // The value is lifted out and the result set drained BEFORE the check and the next execute:
    // a statement dropped with rows pending can roll back a later write (ADR-0010), and both the
    // error path and the `synchronous` pragma below run on this same connection.
    let value = rows
        .next()
        .await
        .map_err(StoreError::database)?
        .map(|row| row.get_value(0));
    drain(rows).await?;
    let value = value
        .transpose()
        .map_err(StoreError::database)?
        .ok_or_else(|| StoreError::Database("journal_mode returned no row".to_owned()))?;
    if as_text(&value) != Some("wal") {
        return Err(StoreError::Database(
            "journal_mode did not switch to wal; the design assumes WAL (ADR-0002)".to_owned(),
        ));
    }
    conn.execute("PRAGMA synchronous = NORMAL", ())
        .await
        .map_err(StoreError::database)?;
    Ok(())
}

/// The text of a column, when it holds text.
#[must_use]
pub fn as_text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(text) => Some(text.as_str()),
        _ => None,
    }
}

/// The integer of a column, when it holds one.
#[must_use]
pub fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(number) => Some(*number),
        _ => None,
    }
}

/// A required text column.
fn text(value: &Value, column: &str) -> Result<String, StoreError> {
    as_text(value)
        .map(str::to_owned)
        .ok_or_else(|| StoreError::Database(format!("column {column} is not text: {value:?}")))
}

/// A required integer column.
fn integer(value: &Value, column: &str) -> Result<i64, StoreError> {
    as_int(value).ok_or_else(|| {
        StoreError::Database(format!("column {column} is not an integer: {value:?}"))
    })
}

/// A nullable text column.
fn optional_text(value: &Value) -> Option<String> {
    as_text(value).map(str::to_owned)
}

/// Reads a `sessions` row.
pub fn read_session(row: &turso::Row, offset: usize) -> Result<Session, StoreError> {
    let id = text(&get(row, offset)?, "sessions.id")?
        .parse()
        .map_err(|_| StoreError::Database("sessions.id is not a uuid".to_owned()))?;
    let title = optional_text(&get(row, offset + 1)?);
    let mode = SessionModeId::new(text(&get(row, offset + 2)?, "sessions.mode")?);
    let workspace = optional_text(&get(row, offset + 3)?).map(PathBuf::from);
    let model = ModelRef::new(
        text(&get(row, offset + 4)?, "sessions.model_provider")?,
        text(&get(row, offset + 5)?, "sessions.model_id")?,
    );
    let config_patch = optional_text(&get(row, offset + 6)?)
        .map(|json| parse_json(&json))
        .transpose()?;
    let active_branch_head = optional_text(&get(row, offset + 7)?)
        .map(|id| {
            id.parse()
                .map_err(|_| StoreError::Database("bad item id".to_owned()))
        })
        .transpose()?;
    let generation = u64::try_from(integer(&get(row, offset + 8)?, "sessions.generation")?)
        .map_err(|_| StoreError::Database("negative generation".to_owned()))?;
    let status = parse_status(&text(&get(row, offset + 9)?, "sessions.status")?)?;
    let created_at =
        Timestamp::from_unix_millis(integer(&get(row, offset + 10)?, "sessions.created_at")?);
    let updated_at =
        Timestamp::from_unix_millis(integer(&get(row, offset + 11)?, "sessions.updated_at")?);

    Ok(Session {
        id,
        title,
        mode,
        workspace,
        model,
        config_patch,
        created_at,
        updated_at,
        active_branch_head,
        generation,
        status,
    })
}

/// Reads an `items` row.
pub fn read_item(row: &turso::Row, offset: usize) -> Result<Item, StoreError> {
    let id: ItemId = text(&get(row, offset)?, "items.id")?
        .parse()
        .map_err(|_| StoreError::Database("items.id is not a uuid".to_owned()))?;
    let session = text(&get(row, offset + 1)?, "items.session_id")?
        .parse()
        .map_err(|_| StoreError::Database("items.session_id is not a uuid".to_owned()))?;
    let parent = optional_text(&get(row, offset + 2)?)
        .map(|id| {
            id.parse()
                .map_err(|_| StoreError::Database("items.parent_id is not a uuid".to_owned()))
        })
        .transpose()?;
    let turn: Option<TurnId> = optional_text(&get(row, offset + 3)?)
        .map(|id| {
            id.parse()
                .map_err(|_| StoreError::Database("items.turn_id is not a uuid".to_owned()))
        })
        .transpose()?;
    let kind_text = text(&get(row, offset + 4)?, "items.kind")?;
    let payload_text = text(&get(row, offset + 5)?, "items.payload")?;
    let created_at =
        Timestamp::from_unix_millis(integer(&get(row, offset + 6)?, "items.created_at")?);

    let kind_tag = kind_text
        .parse::<ItemKindTag>()
        .map_err(|error: UnknownItemKind| StoreError::CorruptItem {
            id,
            message: format!("unknown kind {:?}", error.name()),
        })?;
    let payload = parse_json(&payload_text).map_err(|error| StoreError::CorruptItem {
        id,
        message: error.to_string(),
    })?;
    let kind =
        ItemKind::from_parts(kind_tag, payload).map_err(|error| StoreError::CorruptItem {
            id,
            message: error.to_string(),
        })?;

    Ok(Item {
        id,
        session,
        parent,
        turn,
        kind,
        created_at,
    })
}

/// Reads a `checkpoints` row.
pub fn read_checkpoint(row: &turso::Row, offset: usize) -> Result<CheckpointRecord, StoreError> {
    let id = text(&get(row, offset)?, "checkpoints.id")?
        .parse()
        .map_err(|_| StoreError::Database("checkpoints.id is not a uuid".to_owned()))?;
    let session = text(&get(row, offset + 1)?, "checkpoints.session_id")?
        .parse()
        .map_err(|_| StoreError::Database("checkpoints.session_id is not a uuid".to_owned()))?;
    // NULL is a shape this row can legitimately have — the pre-restore safety snapshot records a
    // commit and no item — so it is carried out as `None` rather than repaired into one
    // (docs/design/storage.md §2). The column's nullability is the only thing that keeps that
    // snapshot out of the conversation history.
    let item = optional_text(&get(row, offset + 2)?)
        .map(|id| {
            id.parse()
                .map_err(|_| StoreError::Database("checkpoints.item_id is not a uuid".to_owned()))
        })
        .transpose()?;
    let workspace = PathBuf::from(text(&get(row, offset + 3)?, "checkpoints.workspace")?);
    let commit_id = text(&get(row, offset + 4)?, "checkpoints.commit_id")?;
    let kind = parse_checkpoint_kind(&text(&get(row, offset + 5)?, "checkpoints.kind")?)?;
    let created_at =
        Timestamp::from_unix_millis(integer(&get(row, offset + 6)?, "checkpoints.created_at")?);

    Ok(CheckpointRecord {
        id,
        session,
        item,
        workspace,
        commit_id,
        kind,
        created_at,
    })
}

/// Reads a skeleton row: `id, parent_id, kind, turn_id`.
pub fn read_skeleton(row: &turso::Row) -> Result<crate::tree::SkeletonRow, StoreError> {
    let id: ItemId = text(&get(row, 0)?, "items.id")?
        .parse()
        .map_err(|_| StoreError::Database("items.id is not a uuid".to_owned()))?;
    let parent = optional_text(&get(row, 1)?)
        .map(|id| {
            id.parse()
                .map_err(|_| StoreError::Database("items.parent_id is not a uuid".to_owned()))
        })
        .transpose()?;
    let kind = text(&get(row, 2)?, "items.kind")?
        .parse::<ItemKindTag>()
        .map_err(|error: UnknownItemKind| StoreError::CorruptItem {
            id,
            message: format!("unknown kind {:?}", error.name()),
        })?;
    let turn = optional_text(&get(row, 3)?)
        .map(|id| {
            id.parse()
                .map_err(|_| StoreError::Database("items.turn_id is not a uuid".to_owned()))
        })
        .transpose()?;
    Ok(crate::tree::SkeletonRow {
        id,
        parent,
        kind,
        turn,
    })
}

/// Parses the `sessions.status` spelling.
pub fn parse_status(text: &str) -> Result<SessionStatus, StoreError> {
    match text {
        "idle" => Ok(SessionStatus::Idle),
        "running" => Ok(SessionStatus::Running),
        "waiting_approval" => Ok(SessionStatus::WaitingApproval),
        "error" => Ok(SessionStatus::Error),
        other => Err(StoreError::Database(format!(
            "sessions.status is not a known status: {other:?}"
        ))),
    }
}

/// The `sessions.status` spelling.
#[must_use]
pub fn status_text(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "idle",
        SessionStatus::Running => "running",
        SessionStatus::WaitingApproval => "waiting_approval",
        SessionStatus::Error => "error",
    }
}

/// Parses the `checkpoints.kind` spelling.
///
/// Hand-written rather than derived from serde, and pinned against the serde spelling by
/// `checkpoint_kind_spellings_round_trip`: `CheckpointKind` is the wire form too, so a rename
/// there would silently orphan every row already written with the old spelling.
pub fn parse_checkpoint_kind(text: &str) -> Result<CheckpointKind, StoreError> {
    match text {
        "pre_write" => Ok(CheckpointKind::PreWrite),
        "pre_shell" => Ok(CheckpointKind::PreShell),
        "manual" => Ok(CheckpointKind::Manual),
        other => Err(StoreError::Database(format!(
            "checkpoints.kind is not a known kind: {other:?}"
        ))),
    }
}

/// The `checkpoints.kind` spelling.
#[must_use]
pub fn checkpoint_kind_text(kind: CheckpointKind) -> &'static str {
    match kind {
        CheckpointKind::PreWrite => "pre_write",
        CheckpointKind::PreShell => "pre_shell",
        CheckpointKind::Manual => "manual",
    }
}

/// Serialises a JSON value for a text column.
pub fn to_json(value: &impl serde::Serialize) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(StoreError::database)
}

/// Reads a JSON document out of a text column.
pub fn parse_json(text: &str) -> Result<Json, StoreError> {
    serde_json::from_str(text).map_err(StoreError::database)
}

/// Reads rows to the end.
///
/// Not cosmetic: a statement dropped with rows still pending rolls its transaction back (the
/// engine says so on `Statement::query_row`), so a half-read query can make a *later* write fail
/// for reasons that look nothing like the cause.
pub async fn drain(mut rows: turso::Rows) -> Result<(), StoreError> {
    while rows.next().await.map_err(StoreError::database)?.is_some() {}
    Ok(())
}

/// Reads every row through `read`, draining the result set even when a row fails to parse.
///
/// The loop shape this replaces — `while let Some(row) = rows.next().await? { out.push(read(&row)?) }`
/// — returns early on a parse error with rows still pending, which is exactly the drop-with-
/// pending-rows shape [`drain`] exists to prevent. A corrupt payload must be reported as the
/// corrupt payload it is, not as a wedged connection two writes later. Whether the engine of the
/// day actually punishes the undrained drop is beside the point: `tests/session_store.rs` pins
/// the contract, so the discipline cannot silently rot when the engine is upgraded.
pub async fn collect<T>(
    mut rows: turso::Rows,
    mut read: impl FnMut(&turso::Row) -> Result<T, StoreError>,
) -> Result<Vec<T>, StoreError> {
    let mut out = Vec::new();
    let mut failure = None;
    loop {
        match rows.next().await.map_err(StoreError::database) {
            Ok(Some(row)) => match read(&row) {
                Ok(value) => out.push(value),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            },
            Ok(None) => break,
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    let drained = drain(rows).await;
    if let Some(error) = failure {
        return Err(error);
    }
    drained?;
    Ok(out)
}

/// Column access that reports the engine's own error.
fn get(row: &turso::Row, index: usize) -> Result<Value, StoreError> {
    row.get_value(index).map_err(StoreError::database)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_spellings_match_the_schema_comment() {
        for status in [
            SessionStatus::Idle,
            SessionStatus::Running,
            SessionStatus::WaitingApproval,
            SessionStatus::Error,
        ] {
            assert_eq!(
                parse_status(status_text(status)).expect("round trip"),
                status
            );
        }
        assert!(
            parse_status("sleeping").is_err(),
            "unknown statuses are refused"
        );
    }

    #[test]
    fn checkpoint_kind_spellings_round_trip() {
        for kind in [
            CheckpointKind::PreWrite,
            CheckpointKind::PreShell,
            CheckpointKind::Manual,
        ] {
            assert_eq!(
                parse_checkpoint_kind(checkpoint_kind_text(kind)).expect("round trip"),
                kind
            );
            // The SQL spelling and the wire spelling are the same spelling. `CheckpointKind`
            // serialises snake_case, and a serde rename would otherwise leave every row already
            // on disk unreadable while the two halves of the code each looked consistent.
            assert_eq!(
                serde_json::to_string(&kind).expect("serialize"),
                format!("\"{}\"", checkpoint_kind_text(kind))
            );
        }
        assert!(
            parse_checkpoint_kind("PreWrite").is_err(),
            "the CamelCase variant name is not the spelling"
        );
        assert!(
            parse_checkpoint_kind("automatic").is_err(),
            "unknown kinds are refused rather than guessed"
        );
    }

    #[test]
    fn unknown_item_kinds_are_reported_with_their_id() {
        // A newer daemon's row must not be read as something else; the message names the item so
        // the operator can find it.
        let error = StoreError::CorruptItem {
            id: ItemId::new(),
            message: "unknown kind \"future_kind\"".to_owned(),
        };
        assert!(error.to_string().contains("corrupt"));
    }
}
