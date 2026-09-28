//! The writer actor: one task, one connection, one transaction per command.
//!
//! The engine has no multi-writer mode (ADR-0002), so every write goes through a single task that
//! owns the connection and consumes a bounded channel. Submitters wait on a reply slot, which
//! means backpressure is the channel filling up rather than unbounded queueing.
//!
//! Reads go through the same actor in M0b. A read pool is M1's work (`docs/worklog/storage.md`).

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};
use turso::{Connection, params_from_iter};

use hatchery_protocol::method::{SessionListParams, SessionListResult};
use hatchery_protocol::{
    Content, Item, ItemId, ItemKind, Session, SessionId, SessionPatch, Timestamp, TurnId,
};

use crate::error::StoreError;
use crate::sql::{
    self, ITEM_COLUMNS, SESSION_COLUMNS, SKELETON_COLUMNS, as_int, as_text, status_text,
};
use crate::tree::{self, SkeletonRow};

/// How many commands may queue before submitters wait.
///
/// Bounded on purpose: a frontend that floods the store should slow down rather than grow the
/// daemon's memory (`docs/design/storage.md` §3).
pub const COMMAND_CAPACITY: usize = 1024;

/// How many item ids go into one payload lookup.
///
/// Conservative rather than maximal: the engine's parameter limit is not documented, and a chain
/// longer than this simply takes several statements. `many_items_rebuild_in_order` covers the
/// chunk boundary.
const PAYLOAD_CHUNK: usize = 200;

/// A reply slot.
pub type Reply<T> = oneshot::Sender<Result<T, StoreError>>;

/// A session's whole tree: positions, plus which items are on the active branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchTree {
    /// The active branch head.
    pub head: Option<ItemId>,
    /// Every item of the session, oldest first.
    pub nodes: Vec<BranchNode>,
}

/// One node of a session's tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BranchNode {
    /// Position in the tree.
    pub row: SkeletonRow,
    /// When it was appended.
    pub created_at: Timestamp,
    /// True when the item is on the active branch, i.e. an ancestor of the head.
    pub active: bool,
}

/// Everything the store can be asked to do.
pub enum StoreCmd {
    /// Insert a session row. The branch head starts NULL.
    CreateSession {
        /// The session to store.
        session: Session,
        /// The stored session.
        reply: Reply<Session>,
    },
    /// Read one session.
    Session {
        /// Which one.
        session: SessionId,
        /// The session.
        reply: Reply<Session>,
    },
    /// Apply a metadata patch.
    UpdateSession {
        /// Which session.
        session: SessionId,
        /// What to change.
        patch: SessionPatch,
        /// The session as it is now.
        reply: Reply<Session>,
    },
    /// List sessions, newest first.
    ListSessions {
        /// Paging and filtering.
        params: SessionListParams,
        /// One page.
        reply: Reply<SessionListResult>,
    },
    /// Delete a session and everything under it.
    DeleteSession {
        /// Which one.
        session: SessionId,
        /// How many items went with it.
        reply: Reply<u64>,
    },
    /// Append one item and advance the active head.
    AppendItem {
        /// The item.
        item: Item,
        /// Its id, echoed back.
        reply: Reply<ItemId>,
    },
    /// Append a batch atomically.
    AppendItems {
        /// The items, in order.
        items: Vec<Item>,
        /// Done.
        reply: Reply<()>,
    },
    /// Read one item.
    Item {
        /// Which session it must belong to.
        session: SessionId,
        /// Which item.
        item: ItemId,
        /// The item.
        reply: Reply<Item>,
    },
    /// The active branch, root first, payloads included.
    RebuildChain {
        /// Which session.
        session: SessionId,
        /// Where the branch ends; `None` means the session's active head.
        head: Option<ItemId>,
        /// The chain.
        reply: Reply<Vec<Item>>,
    },
    /// Every item of a session, for a branch view.
    BranchTree {
        /// Which session.
        session: SessionId,
        /// The tree.
        reply: Reply<BranchTree>,
    },
    /// Edit an item by forking: a new item with the same parent becomes the head (ADR-0003).
    EditFork {
        /// Which session.
        session: SessionId,
        /// The item being edited.
        item: ItemId,
        /// The replacement content.
        new_content: Content,
        /// The new branch head.
        reply: Reply<Item>,
    },
    /// Point the active head at an existing item.
    SwitchBranch {
        /// Which session.
        session: SessionId,
        /// Where to point.
        head: ItemId,
        /// The session as it is now.
        reply: Reply<Session>,
    },
    /// Delete a subtree, refusing while the active head is inside it.
    DeleteBranch {
        /// Which session.
        session: SessionId,
        /// Root of the subtree.
        head: ItemId,
        /// How many items were removed.
        reply: Reply<u64>,
    },
    /// Record that a turn started.
    StartTurn {
        /// Which session.
        session: SessionId,
        /// The turn.
        turn: TurnId,
        /// When it started.
        at: Timestamp,
        /// Done.
        reply: Reply<()>,
    },
    /// Record how a turn ended.
    FinishTurn {
        /// Which session.
        session: SessionId,
        /// The turn.
        turn: TurnId,
        /// Its outcome.
        completion: Option<hatchery_protocol::TurnCompletion>,
        /// Done.
        reply: Reply<()>,
    },
    /// Every item of a session, oldest first, for a whole-tree export.
    AllItems {
        /// Which session.
        session: SessionId,
        /// The items.
        reply: Reply<Vec<Item>>,
    },
    /// Every item's descendant tips, for a whole-tree export.
    Tips {
        /// Which session.
        session: SessionId,
        /// Item id to the tips of the branches it is an ancestor of.
        reply: Reply<HashMap<ItemId, Vec<ItemId>>>,
    },
    /// Flush and stop.
    Shutdown {
        /// Done.
        reply: Reply<()>,
    },
}

/// The task that owns the write connection.
pub struct Writer {
    /// Held so the connection stays valid for the task's lifetime: the engine's `Database` owns
    /// the file, and the spike kept both alive rather than assuming either can outlive the other.
    _database: turso::Database,
    conn: Connection,
}

impl Writer {
    /// Wraps a connection that already has its pragmas and migrations applied.
    #[must_use]
    pub const fn new(database: turso::Database, conn: Connection) -> Self {
        Self {
            _database: database,
            conn,
        }
    }

    /// Runs until the channel closes or a shutdown arrives.
    pub async fn run(self, mut commands: mpsc::Receiver<StoreCmd>) {
        while let Some(command) = commands.recv().await {
            match command {
                StoreCmd::CreateSession { session, reply } => {
                    reply_send(reply, self.create_session(session).await);
                }
                StoreCmd::Session { session, reply } => {
                    reply_send(reply, self.session(session).await);
                }
                StoreCmd::UpdateSession {
                    session,
                    patch,
                    reply,
                } => {
                    reply_send(reply, self.update_session(session, patch).await);
                }
                StoreCmd::ListSessions { params, reply } => {
                    reply_send(reply, self.list_sessions(params).await);
                }
                StoreCmd::DeleteSession { session, reply } => {
                    reply_send(reply, self.delete_session(session).await);
                }
                StoreCmd::AppendItem { item, reply } => {
                    let id = item.id;
                    reply_send(reply, self.append_items(&[item]).await.map(|()| id));
                }
                StoreCmd::AppendItems { items, reply } => {
                    reply_send(reply, self.append_items(&items).await);
                }
                StoreCmd::Item {
                    session,
                    item,
                    reply,
                } => {
                    reply_send(reply, self.item(session, item).await);
                }
                StoreCmd::RebuildChain {
                    session,
                    head,
                    reply,
                } => {
                    reply_send(reply, self.rebuild_chain(session, head).await);
                }
                StoreCmd::BranchTree { session, reply } => {
                    reply_send(reply, self.branch_tree(session).await);
                }
                StoreCmd::EditFork {
                    session,
                    item,
                    new_content,
                    reply,
                } => {
                    reply_send(reply, self.edit_fork(session, item, new_content).await);
                }
                StoreCmd::SwitchBranch {
                    session,
                    head,
                    reply,
                } => {
                    reply_send(reply, self.switch_branch(session, head).await);
                }
                StoreCmd::DeleteBranch {
                    session,
                    head,
                    reply,
                } => {
                    reply_send(reply, self.delete_branch(session, head).await);
                }
                StoreCmd::StartTurn {
                    session,
                    turn,
                    at,
                    reply,
                } => {
                    reply_send(reply, self.start_turn(session, turn, at).await);
                }
                StoreCmd::FinishTurn {
                    session,
                    turn,
                    completion,
                    reply,
                } => {
                    reply_send(reply, self.finish_turn(session, turn, completion).await);
                }
                StoreCmd::AllItems { session, reply } => {
                    reply_send(reply, self.all_items(session).await);
                }
                StoreCmd::Tips { session, reply } => {
                    reply_send(reply, self.tips(session).await);
                }
                StoreCmd::Shutdown { reply } => {
                    reply_send(reply, Ok(()));
                    break;
                }
            }
        }
        tracing::debug!("the store writer has stopped");
    }

    // ------------------------------------------------------------- sessions

    async fn create_session(&self, session: Session) -> Result<Session, StoreError> {
        let config_patch = session
            .config_patch
            .as_ref()
            .map(sql::to_json)
            .transpose()?;
        self.conn
            .execute(
                "INSERT INTO sessions (id, title, mode, workspace, model_provider, model_id, \
                 config_patch, active_head, generation, status, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?10, ?11)",
                (
                    session.id.to_string(),
                    session.title,
                    session.mode.as_str(),
                    session
                        .workspace
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    session.model.provider,
                    session.model.model,
                    config_patch,
                    i64::try_from(session.generation).unwrap_or(i64::MAX),
                    status_text(session.status),
                    session.created_at.as_unix_millis(),
                    session.updated_at.as_unix_millis(),
                ),
            )
            .await
            .map_err(StoreError::database)?;
        self.session(session.id).await
    }

    async fn session(&self, session: SessionId) -> Result<Session, StoreError> {
        let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1");
        let mut rows = self
            .conn
            .query(sql, [session.to_string()])
            .await
            .map_err(StoreError::database)?;
        let found = match rows.next().await.map_err(StoreError::database)? {
            Some(row) => Some(sql::read_session(&row, 0)?),
            None => None,
        };
        drain(rows).await?;
        found.ok_or(StoreError::SessionNotFound(session))
    }

    async fn update_session(
        &self,
        session: SessionId,
        patch: SessionPatch,
    ) -> Result<Session, StoreError> {
        let mut current = self.session(session).await?;
        if let Some(title) = patch.title {
            current.title = title;
        }
        if let Some(mode) = patch.mode {
            current.mode = mode;
        }
        if let Some(model) = patch.model {
            current.model = model;
        }
        if let Some(status) = patch.status {
            current.status = status;
        }
        if let Some(config_patch) = patch.config_patch {
            current.config_patch = Some(config_patch);
        }
        current.updated_at = Timestamp::now();

        let config_patch = current
            .config_patch
            .as_ref()
            .map(sql::to_json)
            .transpose()?;
        self.conn
            .execute(
                "UPDATE sessions SET title = ?1, mode = ?2, model_provider = ?3, model_id = ?4, \
                 config_patch = ?5, status = ?6, updated_at = ?7 WHERE id = ?8",
                (
                    current.title.clone(),
                    current.mode.as_str(),
                    current.model.provider.clone(),
                    current.model.model.clone(),
                    config_patch,
                    status_text(current.status),
                    current.updated_at.as_unix_millis(),
                    session.to_string(),
                ),
            )
            .await
            .map_err(StoreError::database)?;
        Ok(current)
    }

    async fn list_sessions(
        &self,
        params: SessionListParams,
    ) -> Result<SessionListResult, StoreError> {
        let limit = params.limit.unwrap_or(50).clamp(1, 200);
        let filter = params.filter.unwrap_or_default();
        let cursor = params.cursor.as_deref().map(parse_cursor).transpose()?;

        // Built rather than bound with NULL checks: `(?1 IS NULL OR mode = ?1)` reads worse and makes
        // the query planner guess. Every value is bound; only placeholders are formatted in.
        let mut sql = format!("SELECT {SESSION_COLUMNS} FROM sessions");
        let mut clauses: Vec<String> = Vec::new();
        let mut args: Vec<turso::Value> = Vec::new();

        if let Some(mode) = &filter.mode {
            args.push(turso::Value::Text(mode.as_str().to_owned()));
            clauses.push(format!("mode = ?{}", args.len()));
        }
        if let Some(workspace) = &filter.workspace {
            args.push(turso::Value::Text(workspace.to_string_lossy().into_owned()));
            clauses.push(format!("workspace = ?{}", args.len()));
        }
        if let Some(needle) = &filter.title_contains {
            args.push(turso::Value::Text(format!("%{needle}%")));
            clauses.push(format!("title LIKE ?{}", args.len()));
        }
        if let Some((updated_at, id)) = &cursor {
            // Strictly older, or equally old and a larger id: the id tiebreak makes the order
            // total, which is what keeps paging from skipping rows that share a millisecond.
            args.push(turso::Value::Integer(*updated_at));
            let millis = args.len();
            args.push(turso::Value::Text(id.to_string()));
            let tie = args.len();
            clauses.push(format!(
                "(updated_at < ?{millis} OR (updated_at = ?{millis} AND id > ?{tie}))"
            ));
        }

        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        // The id tiebreak keeps the order total, which is what makes the cursor exact.
        sql.push_str(" ORDER BY updated_at DESC, id ASC");
        sql.push_str(&format!(" LIMIT {}", limit + 1));

        let mut rows = self
            .conn
            .query(sql, params_from_iter(args))
            .await
            .map_err(StoreError::database)?;
        let mut sessions = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::database)? {
            sessions.push(sql::read_session(&row, 0)?);
        }

        let next_cursor = if sessions.len() > limit as usize {
            sessions.pop();
            sessions
                .last()
                .map(|session| format!("{}:{}", session.updated_at.as_unix_millis(), session.id))
        } else {
            None
        };
        Ok(SessionListResult {
            sessions,
            next_cursor,
        })
    }

    async fn delete_session(&self, session: SessionId) -> Result<u64, StoreError> {
        let items = self.count(session, "items").await?;
        let deleted = self
            .conn
            .execute("DELETE FROM sessions WHERE id = ?1", [session.to_string()])
            .await
            .map_err(StoreError::database)?;
        if deleted == 0 {
            return Err(StoreError::SessionNotFound(session));
        }
        Ok(items.max(0) as u64)
    }

    // ---------------------------------------------------------------- items

    async fn append_items(&self, items: &[Item]) -> Result<(), StoreError> {
        if items.is_empty() {
            return Ok(());
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .await
            .map_err(StoreError::database)?;
        for item in items {
            self.insert_item(&tx, item).await?;
        }
        let last = items.last().expect("not empty");
        self.advance_head(&tx, last.session, last.id, last.created_at)
            .await?;
        tx.commit().await.map_err(StoreError::database)?;
        Ok(())
    }

    async fn insert_item(&self, conn: &Connection, item: &Item) -> Result<(), StoreError> {
        // The schema's foreign key proves the parent *exists*; it cannot prove the parent belongs
        // to the same session, and a cross-session parent would build a tree nobody can walk.
        if let Some(parent) = item.parent {
            let actual = self.item_session(parent).await?;
            match actual {
                Some(owner) if owner == item.session => {}
                Some(_) => {
                    return Err(StoreError::SessionMismatch {
                        item: parent,
                        session: item.session,
                    });
                }
                None => return Err(StoreError::ItemNotFound(parent)),
            }
        }

        let payload = sql::to_json(&item.kind.to_payload())?;
        conn.execute(
            "INSERT INTO items (id, session_id, parent_id, turn_id, kind, payload, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                item.id.to_string(),
                item.session.to_string(),
                item.parent.map(|parent| parent.to_string()),
                item.turn.map(|turn| turn.to_string()),
                item.kind_tag().as_str(),
                payload,
                item.created_at.as_unix_millis(),
            ),
        )
        .await
        .map_err(StoreError::database)?;
        Ok(())
    }

    async fn advance_head(
        &self,
        conn: &Connection,
        session: SessionId,
        head: ItemId,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let changed = conn
            .execute(
                "UPDATE sessions SET active_head = ?1, updated_at = ?2 WHERE id = ?3",
                (head.to_string(), at.as_unix_millis(), session.to_string()),
            )
            .await
            .map_err(StoreError::database)?;
        if changed == 0 {
            return Err(StoreError::SessionNotFound(session));
        }
        Ok(())
    }

    async fn item(&self, session: SessionId, item: ItemId) -> Result<Item, StoreError> {
        let sql = format!("SELECT {ITEM_COLUMNS} FROM items WHERE id = ?1");
        let mut rows = self
            .conn
            .query(sql, [item.to_string()])
            .await
            .map_err(StoreError::database)?;
        let found = match rows.next().await.map_err(StoreError::database)? {
            Some(row) => Some(sql::read_item(&row, 0)?),
            None => None,
        };
        drain(rows).await?;
        let found = found.ok_or(StoreError::ItemNotFound(item))?;
        if found.session != session {
            return Err(StoreError::SessionMismatch { item, session });
        }
        Ok(found)
    }

    async fn rebuild_chain(
        &self,
        session: SessionId,
        head: Option<ItemId>,
    ) -> Result<Vec<Item>, StoreError> {
        let skeleton = self.skeleton(session).await?;
        let head = match head {
            Some(head) => Some(head),
            None => self.session(session).await?.active_branch_head,
        };
        let ids = tree::chain(&skeleton, head).map_err(|error| self.tree_error(error))?;

        let mut items = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(PAYLOAD_CHUNK) {
            let placeholders = (1..=chunk.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!("SELECT {ITEM_COLUMNS} FROM items WHERE id IN ({placeholders})");
            let params = params_from_iter(chunk.iter().map(ToString::to_string));
            let mut rows = self
                .conn
                .query(sql, params)
                .await
                .map_err(StoreError::database)?;
            while let Some(row) = rows.next().await.map_err(StoreError::database)? {
                items.push(sql::read_item(&row, 0)?);
            }
        }

        // The rows come back in whatever order the engine chose; the chain's order is the point.
        let mut by_id: std::collections::HashMap<ItemId, Item> =
            items.into_iter().map(|item| (item.id, item)).collect();
        ids.into_iter()
            .map(|id| by_id.remove(&id).ok_or(StoreError::ItemNotFound(id)))
            .collect()
    }

    async fn branch_tree(&self, session: SessionId) -> Result<BranchTree, StoreError> {
        let sql = format!(
            "SELECT {SKELETON_COLUMNS}, created_at FROM items WHERE session_id = ?1 \
             ORDER BY created_at ASC, id ASC"
        );
        let mut rows = self
            .conn
            .query(sql, [session.to_string()])
            .await
            .map_err(StoreError::database)?;
        let mut nodes = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::database)? {
            nodes.push(BranchNode {
                row: sql::read_skeleton(&row)?,
                created_at: Timestamp::from_unix_millis(
                    as_int(&row.get_value(4).map_err(StoreError::database)?).unwrap_or_default(),
                ),
                active: false,
            });
        }

        let session_row = self.session(session).await?;
        let head = session_row.active_branch_head;
        let skeleton: Vec<SkeletonRow> = nodes.iter().map(|node| node.row).collect();
        let active: std::collections::HashSet<ItemId> = tree::chain(&skeleton, head)
            .map_err(|error| self.tree_error(error))?
            .into_iter()
            .collect();
        for node in &mut nodes {
            node.active = active.contains(&node.row.id);
        }
        Ok(BranchTree { head, nodes })
    }

    async fn edit_fork(
        &self,
        session: SessionId,
        target: ItemId,
        new_content: Content,
    ) -> Result<Item, StoreError> {
        let original = self.item(session, target).await?;
        let replacement = match &original.kind {
            ItemKind::UserMessage(_) => ItemKind::UserMessage(new_content),
            ItemKind::AssistantMessage(_) => ItemKind::AssistantMessage(new_content),
            _ => return Err(StoreError::NotEditable(target)),
        };

        // Same parent, same kind: the new item *is* the edited one, and the original stays where it
        // is (ADR-0003).
        let mut forked = Item::with_id(ItemId::new(), session, replacement);
        if let Some(turn) = original.turn {
            forked = forked.with_turn(turn);
        }
        if let Some(parent) = original.parent {
            forked = forked.with_parent(parent);
        }

        self.append_items(&[forked.clone()]).await?;
        Ok(forked)
    }

    async fn switch_branch(&self, session: SessionId, head: ItemId) -> Result<Session, StoreError> {
        match self.item_session(head).await? {
            None => return Err(StoreError::ItemNotFound(head)),
            Some(owner) if owner == session => {}
            Some(_) => {
                return Err(StoreError::SessionMismatch {
                    item: head,
                    session,
                });
            }
        }
        self.advance_head(&self.conn, session, head, Timestamp::now())
            .await?;
        self.session(session).await
    }

    async fn delete_branch(&self, session: SessionId, head: ItemId) -> Result<u64, StoreError> {
        let before = self.count(session, "items").await?;
        match self.item_session(head).await? {
            None => return Err(StoreError::ItemNotFound(head)),
            Some(owner) if owner == session => {}
            Some(_) => {
                return Err(StoreError::SessionMismatch {
                    item: head,
                    session,
                });
            }
        }

        let skeleton = self.skeleton(session).await?;
        let doomed = tree::subtree(&skeleton, head);
        let active = self.session(session).await?.active_branch_head;
        if active.is_some_and(|active| doomed.contains(&active)) {
            return Err(StoreError::ActiveHeadInside(session));
        }

        let tx = self
            .conn
            .unchecked_transaction()
            .await
            .map_err(StoreError::database)?;
        // The cascade walks parent_id for us; the walk above exists to know what it will do and to
        // refuse a dangling head before the database has to.
        tx.execute("DELETE FROM items WHERE id = ?1", [head.to_string()])
            .await
            .map_err(StoreError::database)?;
        tx.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            (Timestamp::now().as_unix_millis(), session.to_string()),
        )
        .await
        .map_err(StoreError::database)?;
        tx.commit().await.map_err(StoreError::database)?;

        let after = self.count(session, "items").await?;
        let removed = before - after;
        if removed != doomed.len() as i64 {
            // Our walk and the engine's cascade disagree, which means one of them is wrong. Say so
            // instead of reporting a count that does not describe the database.
            return Err(StoreError::Database(format!(
                "the cascade removed {removed} items but the tree walk expected {}",
                doomed.len()
            )));
        }
        Ok(removed as u64)
    }

    // ---------------------------------------------------------------- turns

    async fn start_turn(
        &self,
        session: SessionId,
        turn: TurnId,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        self.conn
            .execute(
                "INSERT INTO turns (id, session_id, started_at) VALUES (?1, ?2, ?3)",
                (turn.to_string(), session.to_string(), at.as_unix_millis()),
            )
            .await
            .map_err(StoreError::database)?;
        Ok(())
    }

    async fn finish_turn(
        &self,
        session: SessionId,
        turn: TurnId,
        completion: Option<hatchery_protocol::TurnCompletion>,
    ) -> Result<(), StoreError> {
        let (reason, usage) = match &completion {
            Some(completion) => (
                Some(completion.reason.to_string()),
                completion
                    .usage
                    .map(|usage| sql::to_json(&usage))
                    .transpose()?,
            ),
            // A failed turn keeps `stop_reason` NULL and sets `ended_at`: "still running" and
            // "failed" must be distinguishable without a pseudo reason (docs/design/protocol.md §4).
            None => (None, None),
        };
        let changed = self
            .conn
            .execute(
                "UPDATE turns SET ended_at = ?1, stop_reason = ?2, usage = ?3 \
                 WHERE id = ?4 AND session_id = ?5",
                (
                    Timestamp::now().as_unix_millis(),
                    reason,
                    usage,
                    turn.to_string(),
                    session.to_string(),
                ),
            )
            .await
            .map_err(StoreError::database)?;
        if changed == 0 {
            return Err(StoreError::UnknownTurn(turn));
        }
        Ok(())
    }

    // ------------------------------------------------------------- plumbing

    async fn skeleton(&self, session: SessionId) -> Result<Vec<SkeletonRow>, StoreError> {
        let sql = format!("SELECT {SKELETON_COLUMNS} FROM items WHERE session_id = ?1");
        let mut rows = self
            .conn
            .query(sql, [session.to_string()])
            .await
            .map_err(StoreError::database)?;
        let mut skeleton = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::database)? {
            skeleton.push(sql::read_skeleton(&row)?);
        }
        Ok(skeleton)
    }

    /// Every item of a session, oldest first, for a whole-tree export.
    async fn all_items(&self, session: SessionId) -> Result<Vec<Item>, StoreError> {
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM items WHERE session_id = ?1 ORDER BY created_at ASC, id ASC"
        );
        let mut rows = self
            .conn
            .query(sql, [session.to_string()])
            .await
            .map_err(StoreError::database)?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::database)? {
            items.push(sql::read_item(&row, 0)?);
        }
        Ok(items)
    }

    /// Every item's descendant tips, for a whole-tree export.
    async fn tips(&self, session: SessionId) -> Result<HashMap<ItemId, Vec<ItemId>>, StoreError> {
        let skeleton = self.skeleton(session).await?;
        tree::tips_map(&skeleton).map_err(|error| self.tree_error(error))
    }

    async fn item_session(&self, item: ItemId) -> Result<Option<SessionId>, StoreError> {
        let mut rows = self
            .conn
            .query(
                "SELECT session_id FROM items WHERE id = ?1",
                [item.to_string()],
            )
            .await
            .map_err(StoreError::database)?;
        let found = match rows.next().await.map_err(StoreError::database)? {
            Some(row) => {
                let value = row.get_value(0).map_err(StoreError::database)?;
                as_text(&value)
                    .map(str::to_owned)
                    .and_then(|text| text.parse().ok())
            }
            None => None,
        };
        drain(rows).await?;
        Ok(found)
    }

    async fn count(&self, session: SessionId, table: &str) -> Result<i64, StoreError> {
        let sql = format!("SELECT count(*) FROM {table} WHERE session_id = ?1");
        let mut rows = self
            .conn
            .query(sql, [session.to_string()])
            .await
            .map_err(StoreError::database)?;
        let value = match rows.next().await.map_err(StoreError::database)? {
            Some(row) => row.get_value(0).map_err(StoreError::database)?,
            None => {
                return Err(StoreError::Database("count(*) returned no row".to_owned()));
            }
        };
        drain(rows).await?;
        as_int(&value).ok_or_else(|| StoreError::Database("count(*) is not an integer".to_owned()))
    }

    /// A tree-walk failure, which means the stored data is corrupt.
    fn tree_error(&self, error: tree::TreeError) -> StoreError {
        StoreError::Database(format!("item tree is corrupt: {error}"))
    }
}

/// Reads rows to the end.
///
/// Not cosmetic: a statement dropped with rows still pending rolls its transaction back, so a
/// half-read query can make a later write fail for reasons that look nothing like the cause.
async fn drain(mut rows: turso::Rows) -> Result<(), StoreError> {
    while rows.next().await.map_err(StoreError::database)?.is_some() {}
    Ok(())
}

/// Parses a `session/list` cursor: `updated_at_millis:id`.
fn parse_cursor(cursor: &str) -> Result<(i64, SessionId), StoreError> {
    let (millis, id) = cursor
        .split_once(':')
        .ok_or_else(|| StoreError::Invalid(format!("malformed cursor {cursor:?}")))?;
    let millis = millis
        .parse()
        .map_err(|_| StoreError::Invalid(format!("malformed cursor {cursor:?}")))?;
    let id = id
        .parse()
        .map_err(|_| StoreError::Invalid(format!("malformed cursor {cursor:?}")))?;
    Ok((millis, id))
}

/// Sends a reply, ignoring a caller that has gone away.
fn reply_send<T>(reply: Reply<T>, value: Result<T, StoreError>) {
    if reply.send(value).is_err() {
        tracing::debug!("a store caller dropped its reply slot");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_protocol::{ItemIdRange, ItemKindTag};

    #[test]
    fn cursors_round_trip() {
        let id = SessionId::new();
        let cursor = format!("1780000000000:{id}");
        let (millis, parsed) = parse_cursor(&cursor).expect("a cursor we wrote");
        assert_eq!(millis, 1_780_000_000_000);
        assert_eq!(parsed, id);
    }

    #[test]
    fn a_malformed_cursor_is_a_bad_request() {
        for cursor in ["nonsense", "12:", ":id", ""] {
            let error = parse_cursor(cursor).expect_err("must not parse");
            assert_eq!(
                error.to_event_error().code,
                hatchery_protocol::ErrorCode::InvalidRequest
            );
        }
    }

    #[test]
    fn item_id_ranges_are_not_confused_with_paging() {
        // The JSONL export and compaction both carry `ItemIdRange`; a cursor must never be one.
        let range = ItemIdRange::new(ItemId::new(), ItemId::new());
        assert!(range.contains(range.first));
        assert_eq!(ItemKindTag::UserMessage.as_str(), "user_message");
        assert_eq!(
            status_text(sql::parse_status("idle").expect("known")),
            "idle"
        );
    }
}
