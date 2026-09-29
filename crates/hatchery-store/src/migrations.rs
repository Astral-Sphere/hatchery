//! Schema migrations.
//!
//! Two records of the schema version, on purpose: `PRAGMA user_version` is the engine's own
//! counter (measured to survive a reopen in the M0 spike) and `schema_meta.schema_version` is
//! ours. If they ever disagree the database has been touched by something that does not respect
//! migrations, and the store refuses to guess which one is right.

use turso::Connection;

use crate::error::StoreError;
use crate::sql;

/// Every migration, in order.
///
/// `include_str!` embeds the SQL: a database file must be usable by a daemon with no loose `.sql`
/// files next to it, and an installed binary has none.
pub const MIGRATIONS: &[(u32, &str)] = &[(1, include_str!("../migrations/v1.sql"))];

/// The version this build creates and expects.
#[must_use]
pub fn latest_version() -> u32 {
    MIGRATIONS.last().map_or(0, |(version, _)| *version)
}

/// The engine's own schema counter.
///
/// Goes through `query()`: `PRAGMA user_version` answers with a row, and the engine rejects any
/// statement that produces one when executed (measured in the M0 spike, ADR-0010).
pub async fn user_version(conn: &Connection) -> Result<u32, StoreError> {
    let mut rows = conn
        .query("PRAGMA user_version", ())
        .await
        .map_err(StoreError::database)?;
    let value = match rows.next().await.map_err(StoreError::database)? {
        Some(row) => row.get_value(0).map_err(StoreError::database)?,
        None => {
            return Err(StoreError::Database(
                "PRAGMA user_version returned no row".to_owned(),
            ));
        }
    };
    sql::drain(rows).await?;
    u32::try_from(crate::sql::as_int(&value).unwrap_or_default()).map_err(|_| {
        StoreError::Database("PRAGMA user_version is not a non-negative integer".to_owned())
    })
}

/// Our own schema version, from `schema_meta`.
pub async fn schema_version(conn: &Connection) -> Result<Option<u32>, StoreError> {
    let mut rows = conn
        .query(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            (),
        )
        .await
        .map_err(StoreError::database)?;
    let found = match rows.next().await.map_err(StoreError::database)? {
        Some(row) => {
            let value = row.get_value(0).map_err(StoreError::database)?;
            crate::sql::as_text(&value).and_then(|text| text.parse().ok())
        }
        None => None,
    };
    sql::drain(rows).await?;
    Ok(found)
}

/// Applies every migration the database has not seen, and returns the resulting version.
///
/// # Errors
///
/// [`StoreError::Migration`] when a migration fails, or when the database was written by a newer
/// build — reading a schema it does not know is worse than refusing to start.
pub async fn migrate(conn: &Connection) -> Result<u32, StoreError> {
    let current = user_version(conn).await?;
    let latest = latest_version();
    if current > latest {
        return Err(StoreError::Migration {
            version: current,
            message: format!(
                "this database was written by a newer hatchery (schema {current}; this build \
                 knows {latest})"
            ),
        });
    }

    for (version, sql) in MIGRATIONS.iter().filter(|(version, _)| *version > current) {
        // One transaction per migration: a half-applied schema is worse than none, because the
        // retry would trip over the tables the first attempt already created.
        let tx = conn
            .unchecked_transaction()
            .await
            .map_err(|error| migration_error(*version, error))?;
        tx.execute_batch(sql)
            .await
            .map_err(|error| migration_error(*version, error))?;
        record_version(&tx, *version).await?;
        tx.commit()
            .await
            .map_err(|error| migration_error(*version, error))?;
        tracing::info!(version, "applied a schema migration");
    }

    let recorded = user_version(conn).await?;
    match schema_version(conn).await? {
        Some(meta) if meta == recorded => Ok(recorded),
        Some(meta) => Err(StoreError::Migration {
            version: recorded,
            message: format!("user_version says {recorded} but schema_meta says {meta}"),
        }),
        // No exception for `recorded == 0`: `MIGRATIONS` starts at version 1 (pinned by
        // `the_migration_list_is_ordered_and_starts_at_one`), so a database that reached this
        // point has run one and written both records. A missing row means something removed it
        // behind our back, and opening anyway would be opening a database with no schema and
        // reporting that as success.
        None => Err(StoreError::Migration {
            version: recorded,
            message: format!("schema_meta has no schema_version (user_version is {recorded})"),
        }),
    }
}

/// Writes both records of the schema version.
async fn record_version(conn: &Connection, version: u32) -> Result<(), StoreError> {
    // The version is a number this process computed, not user input, so formatting it into the
    // pragma is safe; pragmas do not take bound parameters.
    conn.execute(&format!("PRAGMA user_version = {version}"), ())
        .await
        .map_err(|error| migration_error(version, error))?;
    conn.execute("DELETE FROM schema_meta WHERE key = 'schema_version'", ())
        .await
        .map_err(|error| migration_error(version, error))?;
    let text = version.to_string();
    conn.execute(
        "INSERT INTO schema_meta (key, value) VALUES ('schema_version', ?1)",
        [text.as_str()],
    )
    .await
    .map_err(|error| migration_error(version, error))?;
    Ok(())
}

fn migration_error(version: u32, error: impl std::fmt::Display) -> StoreError {
    StoreError::Migration {
        version,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_migration_list_is_ordered_and_starts_at_one() {
        let mut previous = 0;
        for (version, sql) in MIGRATIONS {
            assert_eq!(
                *version,
                previous + 1,
                "migrations must be consecutive and ordered; version {version} follows {previous}"
            );
            assert!(!sql.trim().is_empty(), "migration {version} is empty");
            previous = *version;
        }
        assert_eq!(latest_version(), previous);
    }

    #[test]
    fn the_embedded_schema_carries_the_rules_we_measured() {
        let sql = MIGRATIONS[0].1;
        assert!(
            sql.contains("'items are append-only'"),
            "invariant 3 lives in the trigger"
        );
        assert!(
            sql.contains("ON DELETE CASCADE"),
            "branch deletion relies on the cascade"
        );
        assert!(
            sql.contains("commit_id"),
            "`commit` is a reserved word the parser rejects (ADR-0010)"
        );
        assert!(
            !sql.contains("active_head    TEXT NOT NULL"),
            "a NOT NULL head would make the first insert impossible (ADR-0010)"
        );
        assert!(
            !sql.lines()
                .any(|line| line.trim_start().to_ascii_uppercase().starts_with("PRAGMA")),
            "pragmas are per connection, not part of the migration"
        );
    }
}
