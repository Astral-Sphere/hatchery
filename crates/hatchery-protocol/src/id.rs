//! Identity newtypes.
//!
//! Every id is a UUIDv7 (`uuid` crate, `v7` feature — decided 2026-09-28, see
//! `docs/design/protocol.md` 开放问题 3): time-ordered, coordination-free, and understood by
//! SQL/JSON/log tooling. On the wire and in the database it is the lowercase hyphenated
//! 36-character form — turso's `Value` has no UUID type, so ids are stored as `TEXT` (ADR-0010).
//!
//! Distinct newtypes instead of one `Id` type: mixing up a `SessionId` and an `ItemId` compiles
//! today and corrupts the database tomorrow.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Generates a UUIDv7-backed id newtype with `Display` / `FromStr` / transparent serde.
macro_rules! uuid_newtype {
    (
        $(#[$meta:meta])*
        $name:ident
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Mints a fresh id.
            ///
            /// UUIDv7 embeds the minting time, and `uuid`'s process-wide counter context keeps ids
            /// strictly increasing even inside one millisecond (`ids_minted_back_to_back_increase`
            /// pins that), so fresh ids sort in creation order with no coordination. Across
            /// processes the order is only as good as the clocks are: a process restarted under a
            /// clock that stepped backwards can mint an id smaller than one already stored.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wraps an existing UUID, e.g. one read back from the database.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// The underlying UUID.
            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }

            /// The UUID's 128-bit value.
            #[must_use]
            pub const fn as_u128(self) -> u128 {
                self.0.as_u128()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            /// Parses any UUID, not only v7: the crate only guarantees what it *mints*.
            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(text).map(Self)
            }
        }
    };
}

uuid_newtype! {
    /// Identifies a session: one conversation with its own item tree and runtime.
    ///
    /// Method names (`session/*`) and the `sessions` table use the same word — the earlier
    /// "Thread" naming was dropped to avoid two names for one thing (M0a).
    SessionId
}

uuid_newtype! {
    /// Identifies one item in a session's append-only history tree.
    ItemId
}

uuid_newtype! {
    /// Identifies one turn: a user input plus everything the agent did in response.
    ///
    /// Turns are not entities — items carry a `turn` label and `turns` holds the statistics.
    TurnId
}

uuid_newtype! {
    /// Identifies a pending approval request.
    ///
    /// The kernel picks the id, the daemon forwards it as `approval/respond { request_id }`.
    ApprovalId
}

/// A span of items along one branch, used by compaction items to record what they summarise.
///
/// `std::ops::Range<ItemId>` cannot appear in the wire model — it is not `Serialize` — and its
/// half-open convention is easy to get wrong when both endpoints name real rows. The fields are
/// named `first`/`last` so the span is inclusive by construction.
///
/// The span is **positional, not ordinal**: its members are the items between the two endpoints
/// *along the chain*, and only whoever holds that chain can name them. Comparing ids instead
/// looks equivalent and is not — UUIDv7 order follows creation *time*, so a process restarted
/// under a clock that stepped backwards mints a child whose id is smaller than its parent's, and
/// an id that merely falls between the endpoints says nothing about which branch it is on. A
/// wrong answer here is silent history corruption: a compaction that thinks an item is covered
/// when it is not stops replaying it to the model. Resolving a span therefore belongs to the tree
/// walk that produced the chain, where both endpoints can be found by position and a missing one
/// is reported as the corrupt record it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemIdRange {
    /// First item of the span (inclusive).
    pub first: ItemId,
    /// Last item of the span (inclusive).
    pub last: ItemId,
}

impl ItemIdRange {
    /// Builds an inclusive span.
    #[must_use]
    pub const fn new(first: ItemId, last: ItemId) -> Self {
        Self { first, last }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_ids_are_uuid_v7() {
        let id = ItemId::new();
        assert_eq!(
            id.as_uuid().get_version_num(),
            7,
            "new ids must be UUIDv7 so their text form sorts in creation order"
        );
        assert_eq!(id.to_string().len(), 36, "wire form is the hyphenated UUID");
        assert_eq!(id.to_string(), id.to_string().to_lowercase());
    }

    #[test]
    fn ids_roundtrip_through_their_wire_form() {
        let id = SessionId::new();
        let text = id.to_string();
        let parsed: SessionId = text.parse().expect("parse our own rendering");
        assert_eq!(id, parsed);
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, format!("\"{text}\""));
        assert_eq!(
            serde_json::from_str::<SessionId>(&json).expect("deserialize"),
            id
        );
    }

    #[test]
    fn id_order_follows_the_embedded_timestamp() {
        // Hand-built UUIDs rather than `new()`: the ordering under test is the timestamp field's,
        // and a minted pair would also depend on the counter below it.
        let older = ItemId::from_uuid(
            "01890000-0000-7000-8000-000000000001"
                .parse()
                .expect("valid uuid"),
        );
        let newer = ItemId::from_uuid(
            "018a0000-0000-7000-8000-000000000000"
                .parse()
                .expect("valid uuid"),
        );
        assert!(older < newer, "UUIDv7 byte order must follow time");
    }

    #[test]
    fn ids_minted_back_to_back_increase() {
        // The guarantee two orderings lean on: `list_sessions` breaks an `updated_at` tie with
        // `id ASC`, and an export orders by `created_at ASC, id ASC`. Both read id order as
        // creation order, which within a process holds because `uuid` mints v7 ids through one
        // shared counter context — inside a millisecond the counter increments instead of being
        // re-randomised. Crossed with a millisecond boundary or not, the sequence must not go
        // backwards; an upstream switch of that context would fail here rather than silently
        // reorder the store's listings.
        let mut previous = ItemId::new();
        for _ in 0..1000 {
            let next = ItemId::new();
            assert!(
                previous < next,
                "{previous} was not followed by a larger id"
            );
            previous = next;
        }
    }

    #[test]
    fn distinct_id_types_do_not_interchange() {
        // A compile-fail would be the real check; this at least pins that the newtypes are not
        // aliases of one another.
        let id = TurnId::new();
        assert_eq!(TurnId::from_uuid(id.as_uuid()), id);
        assert_ne!(TurnId::new().as_uuid(), id.as_uuid());
    }

    #[test]
    fn debug_names_the_type_so_logs_stay_readable() {
        let id = ApprovalId::from_uuid(Uuid::nil());
        assert_eq!(
            format!("{id:?}"),
            "ApprovalId(00000000-0000-0000-0000-000000000000)"
        );
    }

    #[test]
    fn an_item_id_range_keeps_both_endpoints_on_the_wire() {
        let first = ItemId::new();
        let last = ItemId::new();
        let range = ItemIdRange::new(first, last);
        let json = serde_json::to_string(&range).expect("serialize");
        assert!(json.contains("\"first\""), "{json}");
        assert!(json.contains("\"last\""), "{json}");
        assert_eq!(
            serde_json::from_str::<ItemIdRange>(&json).expect("deserialize"),
            range
        );
        // Membership is deliberately not answered here: the span is positional, so only the
        // holder of the chain can resolve it (see the type's docs).
    }
}
