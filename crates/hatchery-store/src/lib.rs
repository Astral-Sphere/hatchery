//! Session storage: append-only item tree, branches, writer actor.
//!
//! Layer **L2** (docs/architecture.md §3). The daemon depends on the [`SessionStore`] trait only;
//! the embedded SQL engine behind it is an implementation detail (ADR-0002), which keeps the
//! escape hatch to a future remote server open.
//!
//! Invariant 3 (items are append-only) is enforced *in the database*, by a trigger that aborts any
//! `UPDATE` on `items` — the M0 spike measured that it really aborts, so history cannot be
//! rewritten by a bug in this crate either (ADR-0010).
//!
//! Three things are worth knowing before reading the code:
//!
//! * **one writer.** The engine has no multi-writer mode, so every write goes through a single
//!   task consuming a bounded channel ([`StoreCmd`]); submitters wait on a reply slot, which is
//!   how backpressure works ([`COMMAND_CAPACITY`]).
//! * **the tree is walked in Rust.** The engine has no `WITH RECURSIVE` (measured, ADR-0010), so
//!   [`tree`] holds the two walks as pure functions over a skeleton — the part a property test can
//!   compare against a second implementation.
//! * **`rebuild_chain` returns items, not messages.** Reasoning filtering, compaction and token
//!   budgeting belong to the daemon that knows the provider's capabilities, so the store returns
//!   what is *recorded*, in order (see `docs/design/storage.md` §4).
//!
//! Design: `docs/design/storage.md`.
//!
//! # Examples
//!
//! ```
//! use hatchery_protocol::{Content, Item, ItemKind, ModelRef, Session, SessionId, SessionModeId, SessionStatus, Timestamp};
//! use hatchery_store::{SessionStore, TursoStore};
//!
//! # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
//! # runtime.block_on(async {
//! # let dir = tempfile::tempdir().unwrap();
//! let store = TursoStore::open(dir.path().join("hatchery.db")).await.unwrap();
//!
//! let session = Session {
//!     id: SessionId::new(),
//!     title: Some("a doctest".to_owned()),
//!     mode: SessionModeId::code(),
//!     workspace: None,
//!     model: ModelRef::new("deepseek", "deepseek-chat"),
//!     config_patch: None,
//!     created_at: Timestamp::now(),
//!     updated_at: Timestamp::now(),
//!     active_branch_head: None,
//!     generation: 0,
//!     status: SessionStatus::Idle,
//! };
//! let session = store.create_session(session).await.unwrap();
//! assert!(session.is_empty(), "a new session has no items");
//!
//! let item = Item::new(session.id, ItemKind::UserMessage(Content::text("hello")));
//! store.append_item(item.clone()).await.unwrap();
//!
//! // The chain is what the model will see, and it is rebuilt from the database, not remembered.
//! let chain = store.rebuild_chain(session.id, None).await.unwrap();
//! assert_eq!(chain.len(), 1);
//! assert_eq!(chain[0].id, item.id);
//!
//! store.shutdown().await.unwrap();
//! # });
//! ```

mod actor;
mod error;
mod export;
mod migrations;
mod sql;
mod store;
pub mod tree;

pub use actor::{BranchNode, BranchTree, COMMAND_CAPACITY, StoreCmd, Writer};
pub use error::StoreError;
pub use export::{ExportLine, FORMAT_VERSION};
pub use migrations::{MIGRATIONS, latest_version};
pub use store::{SessionStore, TursoStore};
pub use tree::{SkeletonRow, TreeError};
