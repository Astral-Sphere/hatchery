//! Engine gate tests: the M0 storage spike, kept as permanent regression tests
//! (docs/design/testing.md §0.2 — every third-party behaviour we rely on must be measured, not
//! assumed).
//!
//! These tests speak to the SQL engine directly rather than through `SessionStore`, because what
//! they pin down is *the engine's* behaviour: which pragmas it honours, whether the append-only
//! trigger really aborts, what foreign keys do, and what survives a reopen. The outcome of this
//! file decides the engine (ADR-0002; verdict recorded in docs/worklog/storage.md).
//!
//! Constants prefixed with `SPIKE_` record a measured outcome that the design currently depends
//! on being *absent*. When the engine gains the feature the test fails on purpose, so the
//! workaround gets reconsidered instead of silently staying.

use std::path::{Path, PathBuf};
use std::time::Instant;

use tempfile::TempDir;
use turso::{Builder, Connection, Database, Value};

/// `WITH RECURSIVE` support, measured during the M0 spike.
///
/// `docs/design/storage.md` originally rebuilt history and collected branch subtrees with
/// recursive CTEs. Upstream COMPAT.md states "WITH RECURSIVE not yet supported", and this
/// constant records what we measured locally. If the test below starts failing, recursive CTEs
/// now work: flip this constant and decide whether the in-Rust tree walk is still worth keeping.
const SPIKE_RECURSIVE_CTE_SUPPORTED: bool = false;

/// `PRAGMA synchronous = NORMAL` support, measured during the M0 spike.
///
/// ADR-0002 specifies `synchronous = NORMAL`, and upstream COMPAT.md claims only `OFF` and `FULL`
/// are supported — that claim is **stale**: turso 0.7.2 accepts NORMAL and reads it back as `1`.
/// `item_boundary_commits_roundtrip_within_budget` additionally measures whether NORMAL behaves
/// differently from FULL or is silently mapped onto it. If this test starts failing, NORMAL broke:
/// fall back to FULL and record the write-latency cost against ADR-0002.
const SPIKE_SYNCHRONOUS_NORMAL_SUPPORTED: bool = true;

/// The v1 schema from docs/design/storage.md §2, verbatim except for three spike findings:
///
/// - pragmas are applied by the caller, not embedded in the DDL batch;
/// - `sessions.active_head` is nullable, because `items.session_id` references `sessions` while
///   `sessions.active_head` references `items`, so with foreign keys enforced neither row could
///   ever be inserted first. NULL means "no items yet";
/// - the checkpoint column is `commit_id`, not `commit`: `commit` is a reserved word and the
///   parser rejects it as a column name (`near "commit": syntax error`).
const SCHEMA_V1: &str = r#"
CREATE TABLE sessions (
  id             TEXT PRIMARY KEY,
  title          TEXT,
  mode           TEXT NOT NULL,
  workspace      TEXT,
  model_provider TEXT NOT NULL,
  model_id       TEXT NOT NULL,
  config_patch   TEXT,
  active_head    TEXT REFERENCES items(id),
  generation     INTEGER NOT NULL DEFAULT 0,
  status         TEXT NOT NULL,
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);

CREATE TABLE items (
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  parent_id  TEXT REFERENCES items(id) ON DELETE CASCADE,
  turn_id    TEXT,
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_items_session_parent ON items(session_id, parent_id);
CREATE INDEX idx_items_turn ON items(turn_id);

CREATE TABLE turns (
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  started_at  INTEGER NOT NULL,
  ended_at    INTEGER,
  stop_reason TEXT,
  usage       TEXT
);

CREATE TABLE checkpoints (
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  item_id    TEXT REFERENCES items(id) ON DELETE CASCADE,
  workspace  TEXT NOT NULL,
  commit_id  TEXT NOT NULL,
  kind       TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE approval_rules (
  id         TEXT PRIMARY KEY,
  scope      TEXT NOT NULL,
  matcher    TEXT NOT NULL,
  decision   TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TRIGGER items_no_update BEFORE UPDATE ON items
BEGIN
  SELECT RAISE(ABORT, 'items are append-only');
END;
"#;

/// A database file that lives as long as the test.
struct TempDb {
    // Held only to keep the directory alive; the underscore keeps dead_code quiet.
    _dir: TempDir,
    path: PathBuf,
}

impl TempDb {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("spike.db");
        Self { _dir: dir, path }
    }

    fn path_str(&self) -> &str {
        self.path
            .to_str()
            .expect("tempdir path is valid unicode on every supported platform")
    }

    async fn open(&self) -> Database {
        Builder::new_local(self.path_str())
            .build()
            .await
            .expect("open database")
    }

    /// Opens the database with the v1 schema applied and foreign keys enforced.
    async fn open_with_schema(&self) -> (Database, Connection) {
        let db = self.open().await;
        let conn = db.connect().expect("connect");
        conn.execute("PRAGMA foreign_keys = ON", ())
            .await
            .expect("enable foreign keys");
        conn.execute_batch(SCHEMA_V1)
            .await
            .expect("apply schema v1");
        (db, conn)
    }

    /// Reopens an existing database without touching its schema.
    async fn reopen(&self) -> (Database, Connection) {
        let db = self.open().await;
        let conn = db.connect().expect("connect");
        conn.execute("PRAGMA foreign_keys = ON", ())
            .await
            .expect("enable foreign keys");
        (db, conn)
    }

    fn sidecar(&self, suffix: &str) -> PathBuf {
        Path::new(&self.path).with_file_name(format!("spike.db{suffix}"))
    }
}

/// Runs a statement expected to yield at most one row with one column.
async fn scalar(conn: &Connection, sql: &str) -> Option<Value> {
    let mut rows = conn
        .query(sql, ())
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    let row = rows.next().await.unwrap_or_else(|e| panic!("{sql}: {e}"))?;
    Some(row.get_value(0).expect("column 0"))
}

async fn scalar_int(conn: &Connection, sql: &str) -> i64 {
    match scalar(conn, sql).await {
        Some(Value::Integer(n)) => n,
        other => panic!("{sql} returned {other:?}, expected an integer"),
    }
}

async fn scalar_text(conn: &Connection, sql: &str) -> Option<String> {
    match scalar(conn, sql).await {
        Some(Value::Text(text)) => Some(text),
        Some(other) => panic!("{sql} returned {other:?}, expected text"),
        None => None,
    }
}

async fn count(conn: &Connection, sql: &str) -> i64 {
    scalar_int(conn, sql).await
}

/// Turns on WAL journaling.
///
/// `PRAGMA journal_mode` answers with a row, and `Connection::execute` rejects any statement that
/// produces one (`Misuse("unexpected row during execution")`), so this pragma has to go through
/// `query`. Every other pragma used here returns nothing and works with `execute`.
async fn set_wal(conn: &Connection) {
    let mut rows = conn
        .query("PRAGMA journal_mode = WAL", ())
        .await
        .expect("set journal_mode = WAL");
    let row = rows
        .next()
        .await
        .expect("step")
        .expect("journal_mode reports the resulting mode");
    assert_eq!(
        row.get_value(0).expect("column 0"),
        Value::Text("wal".to_owned()),
        "journal_mode did not switch to wal (ADR-0002 assumes WAL)"
    );
}

/// Inserts a session plus `items` chained child items, returning the last item id.
///
/// This is also the write pattern the writer actor will use: one statement per item, with
/// `sessions.active_head` advanced at every item boundary.
async fn insert_chain(conn: &Connection, session: &str, items: usize) -> String {
    conn.execute(
        "INSERT INTO sessions (id, mode, model_provider, model_id, status, created_at, updated_at)
         VALUES (?1, 'code', 'deepseek', 'deepseek-chat', 'idle', 0, 0)",
        [session],
    )
    .await
    .expect("insert session");

    let mut parent: Option<String> = None;
    let mut last = String::new();
    for index in 0..items {
        let id = format!("{session}-item-{index:04}");
        match &parent {
            Some(parent_id) => {
                conn.execute(
                    "INSERT INTO items (id, session_id, parent_id, kind, payload, created_at)
                     VALUES (?1, ?2, ?3, 'user_message', '{}', 0)",
                    [id.as_str(), session, parent_id.as_str()],
                )
                .await
                .expect("insert child item");
            }
            None => {
                conn.execute(
                    "INSERT INTO items (id, session_id, parent_id, kind, payload, created_at)
                     VALUES (?1, ?2, NULL, 'user_message', '{}', 0)",
                    [id.as_str(), session],
                )
                .await
                .expect("insert root item");
            }
        }
        conn.execute(
            "UPDATE sessions SET active_head = ?1 WHERE id = ?2",
            [id.as_str(), session],
        )
        .await
        .expect("advance active_head");
        parent = Some(id.clone());
        last = id;
    }
    last
}

// ---------------------------------------------------------------------------
// Gate 1: the real schema must apply, and its foreign keys must be real.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn schema_v1_applies_in_one_batch() {
    let db = TempDb::new();
    let (_, conn) = db.open_with_schema().await;

    for table in [
        "sessions",
        "items",
        "turns",
        "checkpoints",
        "approval_rules",
        "schema_meta",
    ] {
        let found = count(
            &conn,
            &format!(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = '{table}'"
            ),
        )
        .await;
        assert_eq!(found, 1, "table {table} was not created");
    }

    let triggers = count(
        &conn,
        "SELECT count(*) FROM sqlite_master WHERE type = 'trigger' AND name = 'items_no_update'",
    )
    .await;
    assert_eq!(triggers, 1, "the append-only trigger was not created");

    for index in ["idx_items_session_parent", "idx_items_turn"] {
        let found = count(
            &conn,
            &format!(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = '{index}'"
            ),
        )
        .await;
        assert_eq!(found, 1, "index {index} was not created");
    }
}

/// The schema's mutual foreign keys force an insert order; this pins the order that works and
/// proves the keys are enforced rather than silently ignored.
#[tokio::test]
async fn foreign_keys_are_enforced_and_dictate_insert_order() {
    let db = TempDb::new();
    let (_, conn) = db.open_with_schema().await;

    assert_eq!(
        scalar_int(&conn, "PRAGMA foreign_keys").await,
        1,
        "PRAGMA foreign_keys = ON did not take effect"
    );

    let orphan = conn
        .execute(
            "INSERT INTO items (id, session_id, parent_id, kind, payload, created_at)
             VALUES ('orphan', 'nope', NULL, 'user_message', '{}', 0)",
            (),
        )
        .await;
    assert!(
        orphan.is_err(),
        "foreign key on items.session_id was not enforced: {orphan:?}"
    );

    // Session first (active_head NULL), then the item, then advance the head.
    let head = insert_chain(&conn, "s1", 1).await;
    assert_eq!(head, "s1-item-0000");
    assert_eq!(
        scalar_text(&conn, "SELECT active_head FROM sessions WHERE id = 's1'")
            .await
            .as_deref(),
        Some("s1-item-0000")
    );
}

// ---------------------------------------------------------------------------
// Gate 2: invariant 3 — items are append-only, enforced by the database.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invariant_items_update_trigger_aborts() {
    let db = TempDb::new();
    let (_, conn) = db.open_with_schema().await;
    insert_chain(&conn, "s1", 2).await;

    let before = scalar_text(&conn, "SELECT payload FROM items WHERE id = 's1-item-0000'").await;

    let update = conn
        .execute(
            "UPDATE items SET payload = '{\"tampered\":true}' WHERE id = 's1-item-0000'",
            (),
        )
        .await;

    assert!(
        update.is_err(),
        "UPDATE on items must be rejected by the items_no_update trigger (invariant 3)"
    );
    let message = update.unwrap_err().to_string();
    assert!(
        message.contains("append-only"),
        "the abort should carry the trigger's message, got: {message}"
    );

    let after = scalar_text(&conn, "SELECT payload FROM items WHERE id = 's1-item-0000'").await;
    assert_eq!(before, after, "the rejected UPDATE still changed the row");

    // Appending and deleting stay legal: append-only is not immutable-forever.
    conn.execute(
        "INSERT INTO items (id, session_id, parent_id, kind, payload, created_at)
         VALUES ('s1-item-0002', 's1', 's1-item-0001', 'assistant_message', '{}', 0)",
        (),
    )
    .await
    .expect("append must stay allowed");
    let deleted = conn
        .execute("DELETE FROM items WHERE id = 's1-item-0002'", ())
        .await
        .expect("delete must stay allowed");
    assert_eq!(deleted, 1);
}

/// Branch deletion cascades — and is refused by the database while `active_head` still points into
/// the subtree. That refusal is the storage-level backstop behind
/// `delete_branch_refuses_when_active_head_inside` (docs/design/testing.md §3.4).
#[tokio::test]
async fn delete_branch_cascades_and_refuses_while_head_is_inside() {
    let db = TempDb::new();
    let (_, conn) = db.open_with_schema().await;
    insert_chain(&conn, "s1", 3).await;
    conn.execute(
        "INSERT INTO checkpoints (id, session_id, item_id, workspace, commit_id, kind, created_at)
         VALUES ('c1', 's1', 's1-item-0001', '/tmp/ws', 'deadbeef', 'pre_write', 0)",
        (),
    )
    .await
    .expect("insert checkpoint");

    // active_head is s1-item-0002, inside the subtree rooted at s1-item-0000.
    let refused = conn
        .execute("DELETE FROM items WHERE id = 's1-item-0000'", ())
        .await;
    assert!(
        refused.is_err(),
        "deleting a branch that active_head points into must be refused by the foreign key on \
         sessions.active_head, so a session can never end up with a dangling head"
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM items WHERE session_id = 's1'").await,
        3,
        "the refused delete still removed rows"
    );

    // Move the head out of the subtree first, then the cascade must do the whole job.
    conn.execute("UPDATE sessions SET active_head = NULL WHERE id = 's1'", ())
        .await
        .expect("clear active_head");
    let deleted = conn
        .execute("DELETE FROM items WHERE id = 's1-item-0000'", ())
        .await
        .expect("delete subtree root");
    assert_eq!(
        deleted, 1,
        "only the targeted row was deleted, not the subtree"
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM items WHERE session_id = 's1'").await,
        0,
        "ON DELETE CASCADE did not walk the parent_id chain"
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM checkpoints").await,
        0,
        "the checkpoint of a deleted item was left behind"
    );

    conn.execute("DELETE FROM sessions WHERE id = 's1'", ())
        .await
        .expect("delete session");
    assert_eq!(count(&conn, "SELECT count(*) FROM sessions").await, 0);
}

// ---------------------------------------------------------------------------
// Gate 3: the documented gaps the design has to work around.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recursive_cte_support_matches_the_recorded_spike_outcome() {
    let db = TempDb::new();
    let (_, conn) = db.open_with_schema().await;
    insert_chain(&conn, "s1", 3).await;

    let query = "WITH RECURSIVE spine(id, parent_id) AS (
                   SELECT id, parent_id FROM items WHERE id = 's1-item-0002'
                   UNION ALL
                   SELECT i.id, i.parent_id FROM items i JOIN spine s ON i.id = s.parent_id
                 )
                 SELECT count(*) FROM spine";

    let supported = match conn.query(query, ()).await {
        Ok(mut rows) => match rows.next().await {
            Ok(Some(row)) => {
                assert_eq!(
                    row.get_value(0).expect("column 0"),
                    Value::Integer(3),
                    "recursive CTE ran but walked the wrong number of rows"
                );
                true
            }
            Ok(None) => panic!("recursive CTE returned no rows"),
            Err(e) => panic!("recursive CTE produced a row set but failed to step: {e}"),
        },
        Err(_) => false,
    };

    assert_eq!(
        supported, SPIKE_RECURSIVE_CTE_SUPPORTED,
        "WITH RECURSIVE support changed since the M0 spike. Flip \
         SPIKE_RECURSIVE_CTE_SUPPORTED and re-decide whether rebuild_history and DeleteBranch \
         should go back to recursive CTEs (docs/design/storage.md §4-§5)."
    );
}

#[tokio::test]
async fn synchronous_pragma_support_matches_the_recorded_spike_outcome() {
    let db = TempDb::new();
    let (_, conn) = db.open_with_schema().await;

    for (name, value) in [("FULL", 2), ("OFF", 0)] {
        conn.execute(&format!("PRAGMA synchronous = {name}"), ())
            .await
            .unwrap_or_else(|e| panic!("PRAGMA synchronous = {name} must be accepted: {e}"));
        assert_eq!(
            scalar_int(&conn, "PRAGMA synchronous").await,
            value,
            "PRAGMA synchronous = {name} was accepted but reads back differently"
        );
    }

    let accepted = conn
        .execute("PRAGMA synchronous = NORMAL", ())
        .await
        .is_ok();
    let reads_back_as_normal = scalar_int(&conn, "PRAGMA synchronous").await == 1;
    let supported = accepted && reads_back_as_normal;

    assert_eq!(
        supported, SPIKE_SYNCHRONOUS_NORMAL_SUPPORTED,
        "PRAGMA synchronous = NORMAL support changed since the M0 spike. Flip \
         SPIKE_SYNCHRONOUS_NORMAL_SUPPORTED and re-read ADR-0002, which specifies NORMAL."
    );
}

// ---------------------------------------------------------------------------
// Gate 4: journal mode, migration versioning, concurrency, durability, throughput.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn journal_mode_wal_is_active() {
    let db = TempDb::new();
    let handle = db.open().await;
    let conn = handle.connect().expect("connect");

    set_wal(&conn).await;
    assert_eq!(
        scalar_text(&conn, "PRAGMA journal_mode").await.as_deref(),
        Some("wal"),
        "journal_mode did not read back as wal (ADR-0002 assumes WAL)"
    );
    drop(conn);
    drop(handle);
}

#[tokio::test]
async fn user_version_roundtrips_across_reopen() {
    let db = TempDb::new();
    {
        let handle = db.open().await;
        let conn = handle.connect().expect("connect");
        conn.execute("PRAGMA user_version = 7", ())
            .await
            .expect("set user_version");
    }

    let (handle, conn) = db.reopen().await;
    assert_eq!(
        scalar_int(&conn, "PRAGMA user_version").await,
        7,
        "user_version must survive a reopen: the migration framework depends on it"
    );
    drop(conn);
    drop(handle);
}

#[tokio::test]
async fn readers_see_committed_rows_while_a_write_transaction_is_open() {
    let db_file = TempDb::new();
    let (database, reader) = db_file.open_with_schema().await;
    let writer = database.connect().expect("second connection");

    insert_chain(&reader, "s1", 1).await;

    let tx = writer
        .unchecked_transaction()
        .await
        .expect("open a write transaction on a second connection");
    assert!(
        !writer.is_autocommit().expect("autocommit state"),
        "a connection with an open transaction should not report autocommit"
    );

    tx.execute(
        "INSERT INTO sessions (id, mode, model_provider, model_id, status, created_at, updated_at)
         VALUES ('s2', 'chat', 'qwen', 'qwen-max', 'idle', 0, 0)",
        (),
    )
    .await
    .expect("write inside the open transaction");

    assert_eq!(
        count(&reader, "SELECT count(*) FROM sessions WHERE id = 's1'").await,
        1,
        "a reader was blocked or lost committed data while a write transaction was open"
    );
    assert_eq!(
        count(&reader, "SELECT count(*) FROM sessions WHERE id = 's2'").await,
        0,
        "an uncommitted transaction leaked to another connection"
    );

    tx.commit().await.expect("commit");
    assert_eq!(
        count(&reader, "SELECT count(*) FROM sessions WHERE id = 's2'").await,
        1,
        "the committed row did not become visible to the already-open reader connection"
    );
}

#[tokio::test]
async fn committed_rows_survive_reopen_with_wal_sidecars() {
    let db = TempDb::new();
    {
        let (handle, conn) = db.open_with_schema().await;
        set_wal(&conn).await;
        conn.execute("PRAGMA synchronous = FULL", ())
            .await
            .expect("synchronous");
        insert_chain(&conn, "s1", 20).await;
        drop(conn);
        drop(handle);
    }

    // Whether the engine checkpointed on close or left the WAL behind, a reopen must see every
    // committed row. This is the in-process half of crash recovery; the kill -9 half needs a
    // dedicated writer process and lands with the store implementation in M0b
    // (docs/design/testing.md §3.4).
    let wal = db.sidecar("-wal").exists();
    let shm = db.sidecar("-shm").exists();

    let (handle, conn) = db.reopen().await;
    assert_eq!(
        count(&conn, "SELECT count(*) FROM items WHERE session_id = 's1'").await,
        20,
        "committed items were lost across a reopen (wal sidecar: {wal}, shm sidecar: {shm})"
    );
    println!("reopen after close: wal sidecar present = {wal}, shm sidecar present = {shm}");
    drop(conn);
    drop(handle);
}

#[tokio::test]
async fn item_boundary_commits_roundtrip_within_budget() {
    // The writer actor commits at every item boundary (ADR-0002), so per-commit latency is the
    // number that matters. Measured for all three synchronous settings: NORMAL is what we ship,
    // FULL and OFF bracket it and reveal whether NORMAL is honoured or silently mapped to FULL.
    const ITEMS: usize = 500;
    let mut measured = Vec::new();

    for mode in ["NORMAL", "FULL", "OFF"] {
        let db = TempDb::new();
        let (handle, conn) = db.open_with_schema().await;
        set_wal(&conn).await;
        conn.execute(&format!("PRAGMA synchronous = {mode}"), ())
            .await
            .expect("set synchronous");
        assert_eq!(
            scalar_int(&conn, "PRAGMA synchronous").await,
            match mode {
                "NORMAL" => 1,
                "FULL" => 2,
                _ => 0,
            },
            "synchronous = {mode} did not stick"
        );

        let session = format!("s-{mode}");
        let started = Instant::now();
        insert_chain(&conn, &session, ITEMS).await;
        let write = started.elapsed();

        let started = Instant::now();
        let mut rows = conn
            .query(
                &format!(
                    "SELECT id, parent_id FROM items WHERE session_id = '{session}' ORDER BY id"
                ),
                (),
            )
            .await
            .expect("read the spine");
        let mut walked = 0;
        while let Some(row) = rows.next().await.expect("step") {
            let _ = row.get_value(0).expect("id");
            let _ = row.get_value(1).expect("parent_id");
            walked += 1;
        }
        let read = started.elapsed();

        assert_eq!(
            walked, ITEMS,
            "not every item came back at synchronous={mode}"
        );
        measured.push((mode, write, read));
        drop(conn);
        drop(handle);
    }

    for (mode, write, read) in &measured {
        println!(
            "measured synchronous={mode}: {ITEMS} item-boundary commits in {write:?} \
             ({} us/commit), spine read in {read:?}",
            write.as_micros() / ITEMS as u128
        );
    }

    // Generous on purpose: CI machines are slow and shared. These bounds only catch a
    // pathological regression; the numbers above are what the worklog records.
    let (shipped_mode, shipped_write, _) = measured
        .iter()
        .find(|(mode, _, _)| *mode == "NORMAL")
        .expect("NORMAL was measured");
    assert_eq!(*shipped_mode, "NORMAL");
    assert!(
        shipped_write.as_secs() < 60,
        "{ITEMS} item-boundary commits took {shipped_write:?} — the writer actor design would \
         need batching (docs/design/storage.md §3)"
    );
}

/// ADR-0002 commits at every item boundary instead of batching, trading a little throughput for
/// "the model-visible history is never more than one item behind". This measures what batching
/// would buy, so that trade stays evidence-based: if the gap ever grows large, the writer actor
/// should group commits.
#[tokio::test]
async fn per_item_commits_versus_one_batched_transaction() {
    const ITEMS: usize = 500;
    let db = TempDb::new();
    let (handle, conn) = db.open_with_schema().await;
    set_wal(&conn).await;
    conn.execute("PRAGMA synchronous = NORMAL", ())
        .await
        .expect("set synchronous");

    for session in ["autocommit", "batched"] {
        conn.execute(
            "INSERT INTO sessions (id, mode, model_provider, model_id, status, created_at, updated_at)
             VALUES (?1, 'code', 'deepseek', 'deepseek-chat', 'idle', 0, 0)",
            [session],
        )
        .await
        .expect("insert session");
    }

    let insert = |session: &'static str, index: usize| {
        format!(
            "INSERT INTO items (id, session_id, parent_id, kind, payload, created_at) \
             VALUES ('{session}-{index:04}', '{session}', NULL, 'user_message', '{{}}', 0)"
        )
    };

    let started = Instant::now();
    for index in 0..ITEMS {
        conn.execute(insert("autocommit", index), ())
            .await
            .expect("autocommit insert");
    }
    let per_item = started.elapsed();

    let started = Instant::now();
    let tx = conn
        .unchecked_transaction()
        .await
        .expect("open a batched transaction");
    for index in 0..ITEMS {
        tx.execute(insert("batched", index), ())
            .await
            .expect("batched insert");
    }
    tx.commit().await.expect("commit the batch");
    let batched = started.elapsed();

    for session in ["autocommit", "batched"] {
        assert_eq!(
            count(
                &conn,
                &format!("SELECT count(*) FROM items WHERE session_id = '{session}'")
            )
            .await,
            ITEMS as i64,
            "{session} lost items"
        );
    }

    println!(
        "measured: {ITEMS} inserts as one statement per transaction in {per_item:?} \
         ({} us/commit) vs a single batched transaction in {batched:?} ({} us/insert)",
        per_item.as_micros() / ITEMS as u128,
        batched.as_micros() / ITEMS as u128
    );
    drop(conn);
    drop(handle);
}
