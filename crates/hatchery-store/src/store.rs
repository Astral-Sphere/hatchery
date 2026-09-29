//! The storage interface the daemon depends on, and its embedded implementation.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use turso::Builder;

use hatchery_protocol::method::{SessionListParams, SessionListResult};
use hatchery_protocol::{
    Content, Item, ItemId, Session, SessionId, SessionPatch, Timestamp, TurnCompletion, TurnId,
};

use crate::actor::{BranchTree, COMMAND_CAPACITY, Reply, StoreCmd, Writer};
use crate::error::StoreError;
use crate::export;
use crate::migrations;
use crate::sql;

/// Everything the daemon may ask of storage.
///
/// A trait rather than a concrete type: ADR-0002 chose it so that the engine behind it stays an
/// implementation detail, and so a future remote server can be swapped in without touching the
/// daemon. The escape hatch is the reason, not the abstraction.
///
/// All of it is `async` and goes through the writer actor in this implementation; a future
/// implementation is free to use a read pool or a network.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Stores a session. Its branch head starts empty.
    async fn create_session(&self, session: Session) -> Result<Session, StoreError>;

    /// Reads one session.
    async fn session(&self, session: SessionId) -> Result<Session, StoreError>;

    /// Applies a metadata patch and returns the session as it is now.
    async fn update_session(
        &self,
        session: SessionId,
        patch: SessionPatch,
    ) -> Result<Session, StoreError>;

    /// Lists sessions, most recently updated first.
    async fn list_sessions(
        &self,
        params: SessionListParams,
    ) -> Result<SessionListResult, StoreError>;

    /// Deletes a session and everything under it, returning how many items went.
    async fn delete_session(&self, session: SessionId) -> Result<u64, StoreError>;

    /// Appends an item and moves the active head onto it.
    async fn append_item(&self, item: Item) -> Result<ItemId, StoreError>;

    /// Appends a batch in one transaction.
    async fn append_items(&self, items: Vec<Item>) -> Result<(), StoreError>;

    /// Reads one item, refusing an id from another session.
    async fn item(&self, session: SessionId, item: ItemId) -> Result<Item, StoreError>;

    /// The active branch, root first, payloads included.
    ///
    /// `head` of `None` means the session's active head. Payload filtering — reasoning
    /// capabilities, compaction, token budgeting — is the daemon's job, not the store's: the store
    /// returns what is recorded, in order, and nothing more (M0b decision, see
    /// `docs/design/storage.md` §4).
    async fn rebuild_chain(
        &self,
        session: SessionId,
        head: Option<ItemId>,
    ) -> Result<Vec<Item>, StoreError>;

    /// Every item of a session, with the active branch marked.
    async fn branch_tree(&self, session: SessionId) -> Result<BranchTree, StoreError>;

    /// Edits an item by forking: the new item takes its place as the branch head, and the old
    /// branch is kept (ADR-0003).
    async fn edit_fork(
        &self,
        session: SessionId,
        item: ItemId,
        new_content: Content,
    ) -> Result<Item, StoreError>;

    /// Points the active head at an existing item.
    async fn switch_branch(&self, session: SessionId, head: ItemId) -> Result<Session, StoreError>;

    /// Deletes a subtree, refusing while the active head is inside it.
    async fn delete_branch(&self, session: SessionId, head: ItemId) -> Result<u64, StoreError>;

    /// Records that a turn started.
    async fn start_turn(
        &self,
        session: SessionId,
        turn: TurnId,
        at: Timestamp,
    ) -> Result<(), StoreError>;

    /// Records how a turn ended. `None` marks a failed turn: `ended_at` is set and `stop_reason`
    /// stays NULL, so "still running" and "failed" stay distinguishable.
    async fn finish_turn(
        &self,
        session: SessionId,
        turn: TurnId,
        completion: Option<TurnCompletion>,
    ) -> Result<(), StoreError>;

    /// Writes a session out as JSONL, refusing to overwrite an existing file.
    async fn export_jsonl(
        &self,
        session: SessionId,
        path: PathBuf,
        all_branches: bool,
    ) -> Result<u64, StoreError>;

    /// Stops the writer and waits for it to finish.
    ///
    /// Graceful shutdown is about waiting, not about durability: committed items survive even an
    /// abrupt kill (`tests/crash_recovery.rs`).
    async fn shutdown(&self) -> Result<(), StoreError>;
}

/// The embedded engine (turso, ADR-0010).
///
/// Owns a writer task; dropping the store closes the channel and lets the task finish. Call
/// [`SessionStore::shutdown`] to *wait* for it — durability does not depend on being graceful:
/// committed items survive a `kill -9` (`tests/crash_recovery.rs`), and the M0 spike measured that
/// `PRAGMA synchronous` changes nothing measurable, so no explicit WAL checkpoint is issued here.
pub struct TursoStore {
    commands: mpsc::Sender<StoreCmd>,
    writer: Mutex<Option<JoinHandle<()>>>,
    path: PathBuf,
}

impl TursoStore {
    /// Opens (or creates) a database file and prepares it for use.
    ///
    /// Applies the per-connection pragmas and runs pending migrations, so a caller never sees a
    /// half-ready database.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the file cannot be opened, its pragmas cannot be applied, or a
    /// migration fails.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let path_text = path.to_string_lossy().into_owned();
        let database = Builder::new_local(&path_text)
            .build()
            .await
            .map_err(StoreError::database)?;
        let conn = database.connect().map_err(StoreError::database)?;
        sql::apply_pragmas(&conn).await?;
        migrations::migrate(&conn).await?;

        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        // The database handle has to outlive the connection, so it is handed to the writer task
        // rather than dropped here.
        let writer = tokio::spawn(Writer::new(database, conn).run(receiver));
        Ok(Self {
            commands,
            writer: Mutex::new(Some(writer)),
            path,
        })
    }

    /// The file this store writes to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sends a command and waits for its reply.
    async fn ask<T>(&self, build: impl FnOnce(Reply<T>) -> StoreCmd) -> Result<T, StoreError> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| StoreError::WriterGone)?;
        receiver.await.map_err(|_| StoreError::WriterGone)?
    }

    /// The lines and tips for an export, fetched from the database.
    async fn export_body(
        &self,
        session: SessionId,
        all_branches: bool,
    ) -> Result<String, StoreError> {
        if all_branches {
            // One command, one read pass: items and tips fetched separately could describe two
            // different snapshots of a session that is being written while the export runs.
            let (items, tips) = self
                .ask(|reply| StoreCmd::ExportBody { session, reply })
                .await?;
            export::render_all_branches(&items, &tips)
        } else {
            let items = self.rebuild_chain(session, None).await?;
            export::render_chain(&items)
        }
    }
}

#[async_trait]
impl SessionStore for TursoStore {
    async fn create_session(&self, session: Session) -> Result<Session, StoreError> {
        self.ask(|reply| StoreCmd::CreateSession { session, reply })
            .await
    }

    async fn session(&self, session: SessionId) -> Result<Session, StoreError> {
        self.ask(|reply| StoreCmd::Session { session, reply }).await
    }

    async fn update_session(
        &self,
        session: SessionId,
        patch: SessionPatch,
    ) -> Result<Session, StoreError> {
        self.ask(|reply| StoreCmd::UpdateSession {
            session,
            patch,
            reply,
        })
        .await
    }

    async fn list_sessions(
        &self,
        params: SessionListParams,
    ) -> Result<SessionListResult, StoreError> {
        self.ask(|reply| StoreCmd::ListSessions { params, reply })
            .await
    }

    async fn delete_session(&self, session: SessionId) -> Result<u64, StoreError> {
        self.ask(|reply| StoreCmd::DeleteSession { session, reply })
            .await
    }

    async fn append_item(&self, item: Item) -> Result<ItemId, StoreError> {
        self.ask(|reply| StoreCmd::AppendItem { item, reply }).await
    }

    async fn append_items(&self, items: Vec<Item>) -> Result<(), StoreError> {
        self.ask(|reply| StoreCmd::AppendItems { items, reply })
            .await
    }

    async fn item(&self, session: SessionId, item: ItemId) -> Result<Item, StoreError> {
        self.ask(|reply| StoreCmd::Item {
            session,
            item,
            reply,
        })
        .await
    }

    async fn rebuild_chain(
        &self,
        session: SessionId,
        head: Option<ItemId>,
    ) -> Result<Vec<Item>, StoreError> {
        self.ask(|reply| StoreCmd::RebuildChain {
            session,
            head,
            reply,
        })
        .await
    }

    async fn branch_tree(&self, session: SessionId) -> Result<BranchTree, StoreError> {
        self.ask(|reply| StoreCmd::BranchTree { session, reply })
            .await
    }

    async fn edit_fork(
        &self,
        session: SessionId,
        item: ItemId,
        new_content: Content,
    ) -> Result<Item, StoreError> {
        self.ask(|reply| StoreCmd::EditFork {
            session,
            item,
            new_content,
            reply,
        })
        .await
    }

    async fn switch_branch(&self, session: SessionId, head: ItemId) -> Result<Session, StoreError> {
        self.ask(|reply| StoreCmd::SwitchBranch {
            session,
            head,
            reply,
        })
        .await
    }

    async fn delete_branch(&self, session: SessionId, head: ItemId) -> Result<u64, StoreError> {
        self.ask(|reply| StoreCmd::DeleteBranch {
            session,
            head,
            reply,
        })
        .await
    }

    async fn start_turn(
        &self,
        session: SessionId,
        turn: TurnId,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        self.ask(|reply| StoreCmd::StartTurn {
            session,
            turn,
            at,
            reply,
        })
        .await
    }

    async fn finish_turn(
        &self,
        session: SessionId,
        turn: TurnId,
        completion: Option<TurnCompletion>,
    ) -> Result<(), StoreError> {
        self.ask(|reply| StoreCmd::FinishTurn {
            session,
            turn,
            completion,
            reply,
        })
        .await
    }

    async fn export_jsonl(
        &self,
        session: SessionId,
        path: PathBuf,
        all_branches: bool,
    ) -> Result<u64, StoreError> {
        // The body is rendered through the actor (it holds the connection) and written here, so a
        // slow disk does not hold the writer while a frontend waits for other commands.
        let body = self.export_body(session, all_branches).await?;
        export::write(&path, &body)
    }

    async fn shutdown(&self) -> Result<(), StoreError> {
        self.ask(|reply| StoreCmd::Shutdown { reply }).await?;
        let writer = self
            .writer
            .lock()
            .expect("the writer slot is never poisoned")
            .take();
        if let Some(writer) = writer {
            // The reply above proves the actor handled the shutdown; this proves it exited. A
            // writer that panicked or was aborted on the way out did not shut down cleanly, and
            // the caller asked to be told: swallowed here, the panic would be visible nowhere.
            writer.await.map_err(|error| {
                StoreError::Database(format!("the store writer did not exit cleanly: {error}"))
            })?;
        }
        Ok(())
    }
}
