//! What the store refuses to do, and what that means on the wire.

use std::path::PathBuf;

use hatchery_protocol::{ErrorCode, EventError, ItemId, SessionId, TurnId};

/// A storage failure.
///
/// Deliberately specific: "the store failed" is useless to a frontend, while "that session does
/// not exist" and "the head would dangle" are things a caller can act on.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// The engine refused a statement.
    #[error("database: {0}")]
    Database(String),
    /// A stored payload does not match its stored kind.
    #[error("item {id} is corrupt: {message}")]
    CorruptItem {
        /// Which item.
        id: ItemId,
        /// What the payload could not be read as.
        message: String,
    },
    /// No such session.
    #[error("no session {0}")]
    SessionNotFound(SessionId),
    /// No such item.
    #[error("no item {0}")]
    ItemNotFound(ItemId),
    /// The item exists but belongs to a different session.
    #[error("item {item} does not belong to session {session}")]
    SessionMismatch {
        /// The item.
        item: ItemId,
        /// The session the caller asked about.
        session: SessionId,
    },
    /// The active branch head points into the subtree being deleted, so deleting it would leave a
    /// dangling head. The caller must switch branches first.
    #[error("session {0} still has its active head inside that branch")]
    ActiveHeadInside(SessionId),
    /// Only content-bearing items can be edited.
    #[error("item {0} carries no content to edit")]
    NotEditable(ItemId),
    /// No such turn. The daemon must record a turn before it can finish one.
    #[error("no turn {0}")]
    UnknownTurn(TurnId),
    /// The caller sent something the store cannot interpret.
    #[error("{0}")]
    Invalid(String),
    /// The export target already exists.
    #[error("refusing to overwrite {0}")]
    ExportExists(PathBuf),
    /// A migration failed.
    #[error("migration to version {version}: {message}")]
    Migration {
        /// Which migration.
        version: u32,
        /// What went wrong.
        message: String,
    },
    /// The writer actor has stopped.
    #[error("the store writer is gone")]
    WriterGone,
}

impl StoreError {
    /// An engine failure, with the engine's own message preserved.
    #[must_use]
    pub fn database(error: impl std::fmt::Display) -> Self {
        Self::Database(error.to_string())
    }

    /// The same failure in the shape the wire carries.
    #[must_use]
    pub fn to_event_error(&self) -> EventError {
        let code = match self {
            Self::SessionNotFound(_) | Self::ItemNotFound(_) => ErrorCode::SessionNotFound,
            Self::ActiveHeadInside(_)
            | Self::NotEditable(_)
            | Self::UnknownTurn(_)
            | Self::Invalid(_)
            | Self::SessionMismatch { .. }
            | Self::ExportExists(_) => ErrorCode::InvalidRequest,
            Self::Database(_)
            | Self::CorruptItem { .. }
            | Self::Migration { .. }
            | Self::WriterGone => ErrorCode::StoreError,
        };
        EventError::new(code, self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_session_reads_as_a_missing_session() {
        let error = StoreError::SessionNotFound(SessionId::new());
        assert_eq!(error.to_event_error().code, ErrorCode::SessionNotFound);
    }

    #[test]
    fn a_refused_operation_reads_as_a_bad_request_not_a_broken_store() {
        // "you must switch branches first" is the caller's problem, and a frontend should be able
        // to tell it apart from a disk failure.
        let error = StoreError::ActiveHeadInside(SessionId::new());
        assert_eq!(error.to_event_error().code, ErrorCode::InvalidRequest);
        assert_eq!(
            StoreError::database("disk on fire").to_event_error().code,
            ErrorCode::StoreError
        );
    }

    #[test]
    fn the_engine_message_is_preserved() {
        let error = StoreError::database("near \"commit\": syntax error");
        assert!(error.to_string().contains("syntax error"));
    }
}
