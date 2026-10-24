-- schema v1
--
-- This is the DDL that `crates/hatchery-store/tests/spike_engine.rs` measured against turso 0.7.2
-- during the M0 storage spike (ADR-0010). It is a migration: once released it must not change,
-- because existing databases have already run it. Adding a column or a table means adding a new
-- migration file, never editing this one.
--
-- The three corrections the spike forced are already in here:
--   * `sessions.active_head` is nullable — `items.session_id` references `sessions` while
--     `sessions.active_head` references `items`, so with foreign keys enforced neither row could
--     be inserted first. NULL means "no items yet".
--   * the checkpoint column is `commit_id`, not `commit`: `commit` is a reserved word the parser
--     rejects as a column name.
--   * pragmas are not here: they are per connection (`PRAGMA foreign_keys` in particular), and
--     `journal_mode` has to go through `query()` because it answers with a row.

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

-- append-only: the trigger below refuses UPDATE, which is invariant 3 enforced by the database
-- rather than by our discipline.
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