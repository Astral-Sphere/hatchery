//! The store as the daemon will use it: sessions, appends, branches, turns and exports.
//!
//! Everything runs against a real database file in a tempdir. The engine's own behaviour is pinned
//! separately by `spike_engine.rs`; these tests are about what *this* crate does with it.

use std::path::PathBuf;

use hatchery_protocol::{
    Content, Item, ItemId, ItemKind, ModelRef, Session, SessionId, SessionModeId, SessionPatch,
    SessionStatus, StopReason, Timestamp, TurnCompletion, TurnId, Usage,
};
use hatchery_store::{SessionStore, StoreError, TursoStore};

/// A store in its own tempdir, plus a session to work with.
struct Fixture {
    _dir: tempfile::TempDir,
    store: TursoStore,
    path: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().expect("a tempdir");
        let path = dir.path().join("hatchery.db");
        let store = TursoStore::open(&path).await.expect("open the store");
        Self {
            _dir: dir,
            store,
            path,
        }
    }

    async fn with_session() -> (Self, Session) {
        let fixture = Self::new().await;
        let session = fixture.new_session().await;
        (fixture, session)
    }

    async fn new_session(&self) -> Session {
        let now = Timestamp::now();
        let session = Session {
            id: SessionId::new(),
            title: Some("a session".to_owned()),
            mode: SessionModeId::code(),
            workspace: Some(PathBuf::from("/ws")),
            model: ModelRef::new("deepseek", "deepseek-chat"),
            config_patch: None,
            created_at: now,
            updated_at: now,
            active_branch_head: None,
            generation: 0,
            status: SessionStatus::Idle,
        };
        self.store
            .create_session(session.clone())
            .await
            .expect("create the session")
    }
}

fn message(session: SessionId, text: &str, parent: Option<ItemId>) -> Item {
    let item = Item::new(session, ItemKind::UserMessage(Content::text(text)));
    match parent {
        Some(parent) => item.with_parent(parent),
        None => item,
    }
}

/// Appends a message after `parent` and returns the new item.
async fn say(store: &TursoStore, session: &Session, text: &str, parent: Option<ItemId>) -> Item {
    let item = message(session.id, text, parent);
    store.append_item(item.clone()).await.expect("append");
    item
}

fn texts(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::UserMessage(content) | ItemKind::AssistantMessage(content) => {
                Some(content.text.clone())
            }
            _ => None,
        })
        .collect()
}

/// Opens the database file behind the store's back.
///
/// Used to look at what the store wrote, and to write things only it should refuse to write.
async fn open_directly(path: &std::path::Path) -> turso::Connection {
    let text = path.to_string_lossy().into_owned();
    let database = turso::Builder::new_local(&text)
        .build()
        .await
        .expect("open directly");
    database.connect().expect("connect")
}

/// Waits until the millisecond clock advances.
///
/// `Timestamp::now()` is millisecond-resolution and `list_sessions` orders by
/// `updated_at DESC, id ASC`. Three in-process round trips can land in the same millisecond, and
/// then the tiebreak decides the order — by session id, which sorts by *creation* (UUIDv7 ids are
/// monotonic within a process), not by the update this test is asserting on. Advancing the clock
/// between writes keeps a recency assertion about recency instead of about which session was
/// created first.
async fn next_millisecond() {
    let now = Timestamp::now();
    while Timestamp::now() == now {
        tokio::task::yield_now().await;
    }
}

// ---------------------------------------------------------------- sessions

#[tokio::test]
async fn a_new_session_has_no_items_and_no_head() {
    let (_fixture, session) = Fixture::with_session().await;
    assert!(session.is_empty());
    assert_eq!(session.generation, 0);
    assert_eq!(session.status, SessionStatus::Idle);
    assert_eq!(session.model.provider, "deepseek");
}

#[tokio::test]
async fn a_missing_session_is_reported_by_id() {
    let fixture = Fixture::new().await;
    let missing = SessionId::new();
    let error = fixture
        .store
        .session(missing)
        .await
        .expect_err("must not exist");
    assert_eq!(error, StoreError::SessionNotFound(missing));
}

#[tokio::test]
async fn update_session_patches_and_can_clear_the_title() {
    let (fixture, session) = Fixture::with_session().await;

    let patched = fixture
        .store
        .update_session(
            session.id,
            SessionPatch {
                title: Some(Some("renamed".to_owned())),
                mode: Some(SessionModeId::chat()),
                status: Some(SessionStatus::Running),
                config_patch: Some(serde_json::json!({"ui": {"show_reasoning": true}})),
                ..SessionPatch::default()
            },
        )
        .await
        .expect("patch");
    assert_eq!(patched.title.as_deref(), Some("renamed"));
    assert_eq!(patched.mode, SessionModeId::chat());
    assert_eq!(patched.status, SessionStatus::Running);
    assert!(patched.config_patch.is_some());

    // `Some(None)` clears, and an absent field leaves the rest alone.
    let cleared = fixture
        .store
        .update_session(
            session.id,
            SessionPatch {
                title: Some(None),
                ..SessionPatch::default()
            },
        )
        .await
        .expect("clear");
    assert_eq!(cleared.title, None);
    assert_eq!(cleared.mode, SessionModeId::chat(), "the rest is untouched");

    // And it survives a reopen, which is the point of storing it.
    let again = fixture.store.session(session.id).await.expect("read back");
    assert_eq!(again, cleared);

    // An empty patch is the no-op `SessionPatch::is_empty` documents: the store skips the
    // write, so `updated_at` must not move — a heartbeat patch would otherwise keep re-sorting
    // the session to the top of every newest-first list.
    next_millisecond().await;
    let untouched = fixture
        .store
        .update_session(session.id, SessionPatch::default())
        .await
        .expect("an empty patch is still accepted");
    assert_eq!(
        untouched.updated_at, again.updated_at,
        "an empty patch skips the write"
    );
    assert_eq!(untouched, again);
}

#[tokio::test]
async fn list_sessions_pages_newest_first() {
    let fixture = Fixture::new().await;
    let mut ids = Vec::new();
    for index in 0..3 {
        let mut session = fixture.new_session().await;
        session.title = Some(format!("session {index}"));
        // Distinct milliseconds, or the `updated_at DESC, id ASC` order falls to a UUIDv7's
        // random tail and this assertion becomes a coin flip on a fast machine.
        next_millisecond().await;
        let session = fixture
            .store
            .update_session(
                session.id,
                SessionPatch {
                    title: Some(session.title.clone()),
                    ..SessionPatch::default()
                },
            )
            .await
            .expect("retitle");
        ids.push(session.id);
    }

    let first = fixture
        .store
        .list_sessions(Default::default())
        .await
        .expect("list");
    assert_eq!(first.sessions.len(), 3);
    assert_eq!(
        first.sessions[0].id, ids[2],
        "the most recently updated session comes first"
    );

    let page = fixture
        .store
        .list_sessions(hatchery_protocol::method::SessionListParams {
            limit: Some(2),
            ..Default::default()
        })
        .await
        .expect("page");
    assert_eq!(page.sessions.len(), 2);
    let cursor = page.next_cursor.clone().expect("a cursor for page two");

    let second = fixture
        .store
        .list_sessions(hatchery_protocol::method::SessionListParams {
            limit: Some(2),
            cursor: Some(cursor),
            ..Default::default()
        })
        .await
        .expect("page two");
    assert_eq!(second.sessions.len(), 1);
    assert_eq!(second.sessions[0].id, ids[0]);
    assert!(second.next_cursor.is_none());

    let filtered = fixture
        .store
        .list_sessions(hatchery_protocol::method::SessionListParams {
            filter: Some(hatchery_protocol::SessionListFilter {
                title_contains: Some("session 1".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .expect("filter");
    assert_eq!(filtered.sessions.len(), 1);
    assert_eq!(filtered.sessions[0].id, ids[1]);
}

#[tokio::test]
async fn deleting_a_session_takes_its_items_with_it() {
    let (fixture, session) = Fixture::with_session().await;
    say(&fixture.store, &session, "one", None).await;
    say(&fixture.store, &session, "two", None).await;

    let deleted = fixture
        .store
        .delete_session(session.id)
        .await
        .expect("delete");
    assert_eq!(deleted, 2);
    assert!(matches!(
        fixture.store.session(session.id).await,
        Err(StoreError::SessionNotFound(_))
    ));
}

// ------------------------------------------------------------------- items

#[tokio::test]
async fn appending_advances_the_active_head() {
    let (fixture, session) = Fixture::with_session().await;
    let first = say(&fixture.store, &session, "one", None).await;
    assert_eq!(
        fixture
            .store
            .session(session.id)
            .await
            .expect("session")
            .active_branch_head,
        Some(first.id)
    );

    let second = say(&fixture.store, &session, "two", Some(first.id)).await;
    assert_eq!(
        fixture
            .store
            .session(session.id)
            .await
            .expect("session")
            .active_branch_head,
        Some(second.id)
    );

    let chain = fixture
        .store
        .rebuild_chain(session.id, None)
        .await
        .expect("chain");
    assert_eq!(texts(&chain), vec!["one", "two"]);
    assert_eq!(chain[1].parent, Some(first.id));
    assert_eq!(chain[0].session, session.id);
}

#[tokio::test]
async fn a_batch_commits_atomically() {
    let (fixture, session) = Fixture::with_session().await;
    let first = message(session.id, "one", None);
    // The second item points at a parent from another session, which the batch must reject.
    let other = fixture.new_session().await;
    let orphan = message(other.id, "two", None);
    let stray = Item::with_id(
        ItemId::new(),
        session.id,
        ItemKind::UserMessage(Content::text("three")),
    )
    .with_parent(orphan.id)
    .with_turn(TurnId::new());

    let error = fixture
        .store
        .append_items(vec![first.clone(), stray])
        .await
        .expect_err("the batch must be refused");
    assert!(matches!(error, StoreError::ItemNotFound(_)));

    let chain = fixture
        .store
        .rebuild_chain(session.id, None)
        .await
        .expect("chain");
    assert!(
        chain.is_empty(),
        "nothing from a refused batch may land: {chain:?}"
    );
    assert!(
        fixture
            .store
            .session(session.id)
            .await
            .expect("session")
            .active_branch_head
            .is_none(),
        "and the head must not have moved"
    );
}

#[tokio::test]
async fn an_item_cannot_be_chained_into_another_session() {
    let (fixture, session) = Fixture::with_session().await;
    let other = fixture.new_session().await;
    let foreign = say(&fixture.store, &other, "elsewhere", None).await;

    let mut intruder = message(session.id, "sneaky", None);
    intruder.parent = Some(foreign.id);
    let error = fixture
        .store
        .append_item(intruder)
        .await
        .expect_err("a cross-session parent must be refused");
    assert_eq!(
        error,
        StoreError::SessionMismatch {
            item: foreign.id,
            session: session.id
        }
    );
}

#[tokio::test]
async fn writing_into_a_missing_session_says_the_session_is_missing() {
    // Left to the foreign key, this surfaces as the engine's own message, which `to_event_error`
    // maps to a storage failure. Writing into a session that was deleted underneath the caller is
    // a caller error, and the two must not reach a frontend looking the same (storage.md §1).
    let fixture = Fixture::new().await;
    let gone = SessionId::new();

    let error = fixture
        .store
        .append_item(message(gone, "nobody will read this", None))
        .await
        .expect_err("there is no such session");
    assert_eq!(error, StoreError::SessionNotFound(gone));
    assert_eq!(
        error.to_event_error().code,
        hatchery_protocol::ErrorCode::SessionNotFound,
        "so a frontend can tell this apart from a broken disk"
    );

    assert_eq!(
        fixture
            .store
            .append_items(vec![message(gone, "one", None), message(gone, "two", None)])
            .await
            .expect_err("a batch is refused the same way"),
        StoreError::SessionNotFound(gone)
    );
}

#[tokio::test]
async fn a_batch_spanning_two_sessions_is_refused() {
    // The head advance names the *last* item's session, so a mixed batch would write into two
    // sessions and advance only one head: the other's items would be unreachable from any head,
    // and no foreign key can see it (`sessions.active_head` proves the item exists, not that it
    // belongs to the session being advanced).
    let (fixture, session) = Fixture::with_session().await;
    let other = fixture.new_session().await;

    let error = fixture
        .store
        .append_items(vec![
            message(session.id, "here", None),
            message(other.id, "there", None),
        ])
        .await
        .expect_err("one batch, one session");
    assert!(
        matches!(error, StoreError::Invalid(_)),
        "a mixed batch is the caller's mistake: {error}"
    );

    // The refusal happens before the transaction opens, so neither session gained anything.
    assert!(
        fixture
            .store
            .rebuild_chain(session.id, None)
            .await
            .expect("chain")
            .is_empty()
    );
    assert!(
        fixture
            .store
            .rebuild_chain(other.id, None)
            .await
            .expect("chain")
            .is_empty()
    );
}

#[tokio::test]
async fn rebuilding_a_chain_many_items_long_keeps_the_order() {
    // 300 items crosses the payload-lookup chunk boundary, so the chunking is exercised rather
    // than assumed.
    let (fixture, session) = Fixture::with_session().await;
    let mut parent = None;
    let mut written = Vec::new();
    for index in 0..300 {
        let item = say(&fixture.store, &session, &format!("item {index}"), parent).await;
        parent = Some(item.id);
        written.push(item.id);
    }

    let chain = fixture
        .store
        .rebuild_chain(session.id, None)
        .await
        .expect("chain");
    let ids: Vec<ItemId> = chain.iter().map(|item| item.id).collect();
    assert_eq!(ids, written, "the chain comes back root first, in order");
    assert_eq!(chain.len(), 300);
    assert_eq!(chain[299].parent, Some(chain[298].id));
}

#[tokio::test]
async fn reading_an_item_refuses_a_foreign_session() {
    let (fixture, session) = Fixture::with_session().await;
    let item = say(&fixture.store, &session, "mine", None).await;
    let read = fixture.store.item(session.id, item.id).await.expect("read");
    assert_eq!(read, item);

    let other = fixture.new_session().await;
    assert!(matches!(
        fixture.store.item(other.id, item.id).await,
        Err(StoreError::SessionMismatch { .. })
    ));
}

#[tokio::test]
async fn branch_operations_refuse_an_item_from_another_session() {
    let (fixture, session) = Fixture::with_session().await;
    let other = fixture.new_session().await;
    let foreign = say(&fixture.store, &other, "elsewhere", None).await;

    assert!(matches!(
        fixture.store.switch_branch(session.id, foreign.id).await,
        Err(StoreError::SessionMismatch { .. })
    ));
    assert!(matches!(
        fixture.store.delete_branch(session.id, foreign.id).await,
        Err(StoreError::SessionMismatch { .. })
    ));

    // A refusal that quietly did something else would be worse than no check at all.
    assert_eq!(
        fixture
            .store
            .item(other.id, foreign.id)
            .await
            .expect("item"),
        foreign,
        "the foreign item is untouched"
    );
    assert!(
        fixture
            .store
            .session(session.id)
            .await
            .expect("session")
            .active_branch_head
            .is_none(),
        "and the head did not move"
    );
}

// ----------------------------------------------------------------- branches

#[tokio::test]
async fn editing_an_item_forks_and_keeps_the_old_branch() {
    let (fixture, session) = Fixture::with_session().await;
    let first = say(&fixture.store, &session, "one", None).await;
    let second = say(&fixture.store, &session, "two", Some(first.id)).await;
    let third = say(&fixture.store, &session, "three", Some(second.id)).await;

    let forked = fixture
        .store
        .edit_fork(session.id, second.id, Content::text("two, said better"))
        .await
        .expect("fork");
    assert_eq!(
        forked.parent,
        Some(first.id),
        "the fork hangs off the target's parent"
    );
    assert_eq!(forked.turn, second.turn, "and stays in the same turn");
    assert_eq!(
        fixture
            .store
            .session(session.id)
            .await
            .expect("session")
            .active_branch_head,
        Some(forked.id)
    );

    let new_branch = fixture
        .store
        .rebuild_chain(session.id, None)
        .await
        .expect("new branch");
    assert_eq!(texts(&new_branch), vec!["one", "two, said better"]);

    let old_branch = fixture
        .store
        .rebuild_chain(session.id, Some(third.id))
        .await
        .expect("old branch");
    assert_eq!(
        texts(&old_branch),
        vec!["one", "two", "three"],
        "the old branch is still there, unchanged"
    );
}

#[tokio::test]
async fn editing_an_item_that_carries_no_content_is_refused() {
    let (fixture, session) = Fixture::with_session().await;
    let note = Item::new(
        session.id,
        ItemKind::BranchNote(hatchery_protocol::BranchNote {
            note: "why".to_owned(),
        }),
    );
    fixture
        .store
        .append_item(note.clone())
        .await
        .expect("append");

    let error = fixture
        .store
        .edit_fork(session.id, note.id, Content::text("edited"))
        .await
        .expect_err("a branch note has nothing to edit");
    assert_eq!(error, StoreError::NotEditable(note.id));
}

#[tokio::test]
async fn invariant_items_are_never_rewritten() {
    let (fixture, session) = Fixture::with_session().await;
    let original = say(&fixture.store, &session, "one", None).await;
    fixture
        .store
        .edit_fork(session.id, original.id, Content::text("one, said better"))
        .await
        .expect("fork");

    // The edit created a second item; the first is byte-identical to what was stored.
    let read = fixture
        .store
        .item(session.id, original.id)
        .await
        .expect("the original is still there");
    assert_eq!(
        read, original,
        "editing must never rewrite history (ADR-0003)"
    );
    assert_eq!(
        fixture
            .store
            .branch_tree(session.id)
            .await
            .expect("tree")
            .nodes
            .len(),
        2,
        "both branches are in the tree"
    );
}

#[tokio::test]
async fn switching_branches_changes_what_rebuild_returns() {
    let (fixture, session) = Fixture::with_session().await;
    let first = say(&fixture.store, &session, "one", None).await;
    let second = say(&fixture.store, &session, "two", Some(first.id)).await;
    let forked = fixture
        .store
        .edit_fork(session.id, second.id, Content::text("two, said better"))
        .await
        .expect("fork");

    fixture
        .store
        .switch_branch(session.id, second.id)
        .await
        .expect("switch back");
    assert_eq!(
        texts(
            &fixture
                .store
                .rebuild_chain(session.id, None)
                .await
                .expect("chain")
        ),
        vec!["one", "two"]
    );

    fixture
        .store
        .switch_branch(session.id, forked.id)
        .await
        .expect("switch forward");
    assert_eq!(
        texts(
            &fixture
                .store
                .rebuild_chain(session.id, None)
                .await
                .expect("chain")
        ),
        vec!["one", "two, said better"]
    );

    assert!(matches!(
        fixture.store.switch_branch(session.id, ItemId::new()).await,
        Err(StoreError::ItemNotFound(_))
    ));
}

#[tokio::test]
async fn delete_branch_cascades_and_refuses_while_the_head_is_inside() {
    let (fixture, session) = Fixture::with_session().await;
    let first = say(&fixture.store, &session, "one", None).await;
    let second = say(&fixture.store, &session, "two", Some(first.id)).await;
    let third = say(&fixture.store, &session, "three", Some(second.id)).await;

    let error = fixture
        .store
        .delete_branch(session.id, second.id)
        .await
        .expect_err("the head is inside the subtree");
    assert_eq!(error, StoreError::ActiveHeadInside(session.id));
    assert_eq!(
        fixture
            .store
            .branch_tree(session.id)
            .await
            .expect("tree")
            .nodes
            .len(),
        3,
        "a refused delete changes nothing"
    );

    // Move the head out of the way: fork from the first item.
    fixture
        .store
        .edit_fork(session.id, first.id, Content::text("one, said better"))
        .await
        .expect("fork");
    let deleted = fixture
        .store
        .delete_branch(session.id, second.id)
        .await
        .expect("the head moved out");
    assert_eq!(deleted, 2, "two and three go");
    assert_eq!(
        texts(
            &fixture
                .store
                .rebuild_chain(session.id, None)
                .await
                .expect("chain")
        ),
        vec!["one, said better"]
    );
    assert!(
        matches!(
            fixture.store.item(session.id, third.id).await,
            Err(StoreError::ItemNotFound(_))
        ),
        "the deleted descendant is gone"
    );
}

#[tokio::test]
async fn deleting_a_branch_leaves_a_sibling_alone() {
    let (fixture, session) = Fixture::with_session().await;
    let root = say(&fixture.store, &session, "root", None).await;
    let kept = say(&fixture.store, &session, "kept", Some(root.id)).await;
    let abandoned = say(&fixture.store, &session, "abandoned", Some(root.id)).await;
    let forked = fixture
        .store
        .edit_fork(session.id, kept.id, Content::text("forked"))
        .await
        .expect("fork");
    assert!(forked.id != kept.id);

    let deleted = fixture
        .store
        .delete_branch(session.id, abandoned.id)
        .await
        .expect("a sibling, not on the active branch");
    assert_eq!(deleted, 1);

    let chain = fixture
        .store
        .rebuild_chain(session.id, None)
        .await
        .expect("chain");
    assert_eq!(texts(&chain), vec!["root", "forked"]);
}

#[tokio::test]
async fn delete_branch_rolls_back_when_the_cascade_disagrees_with_the_walk() {
    // The cross-check is the store's only detector of a walk/cascade disagreement, and nothing
    // the store writes itself can ever trigger it — so without this test, deleting the check
    // turns no test red. The sandwich is inserted behind the store's back: a cross-session
    // parent the FK cannot express, which lets the cascade reach rows the session-filtered walk
    // cannot see. Detection must happen BEFORE the commit — an error returned after an
    // irreversible delete describes a database that no longer exists.
    let fixture = Fixture::new().await;
    let session_a = fixture.new_session().await;
    let session_b = fixture.new_session().await;
    let root = say(&fixture.store, &session_b, "root of b", None).await;
    // A second root becomes b's head, parked outside the cascade. The head must not be inside
    // the doomed subtree — not because of the store's own refusal (the walk cannot see the
    // cross-session rows), but because `sessions.active_head` has a foreign key: deleting the
    // head row trips the engine's constraint before the cross-check ever runs.
    let parked = say(&fixture.store, &session_b, "parked head", None).await;

    // x belongs to session a but hangs off b's root; y belongs to b but hangs off x. Deleting
    // root cascades root -> x -> y (3 rows), while b's own tree walk sees only {root} (1 row).
    let x = ItemId::new();
    let y = ItemId::new();
    let conn = open_directly(&fixture.path).await;
    for (index, (id, session, parent)) in [(x, session_a.id, root.id), (y, session_b.id, x)]
        .into_iter()
        .enumerate()
    {
        conn.execute(
            "INSERT INTO items (id, session_id, parent_id, kind, payload, created_at) \
             VALUES (?1, ?2, ?3, 'user_message', '{\"text\":\"sandwich\",\"parts\":[]}', ?4)",
            (
                id.to_string(),
                session.to_string(),
                parent.to_string(),
                (index + 1) as i64,
            ),
        )
        .await
        .expect("insert behind the store's back");
    }
    drop(conn);

    let error = fixture
        .store
        .delete_branch(session_b.id, root.id)
        .await
        .expect_err("the walk sees one item; the cascade would take three");
    assert!(
        matches!(error, StoreError::Database(ref message) if message.contains("rolled back")),
        "the disagreement must be reported as such: {error}"
    );

    // The rollback is the point: every row the cascade would have taken is still there, and
    // the connection still works — the disagreement was caught before the commit, not after.
    fixture
        .store
        .item(session_b.id, root.id)
        .await
        .expect("the root survived the rolled-back delete");
    fixture
        .store
        .item(session_b.id, parked.id)
        .await
        .expect("the parked head survived");
    fixture
        .store
        .item(session_b.id, y)
        .await
        .expect("y survived");
    fixture
        .store
        .item(session_a.id, x)
        .await
        .expect("x survived");
}

#[tokio::test]
async fn a_branch_view_marks_the_active_chain() {
    let (fixture, session) = Fixture::with_session().await;
    let first = say(&fixture.store, &session, "one", None).await;
    let second = say(&fixture.store, &session, "two", Some(first.id)).await;
    let forked = fixture
        .store
        .edit_fork(session.id, second.id, Content::text("two, said better"))
        .await
        .expect("fork");

    let tree = fixture.store.branch_tree(session.id).await.expect("tree");
    assert_eq!(tree.head, Some(forked.id));
    assert_eq!(tree.nodes.len(), 3);

    let active: Vec<ItemId> = tree
        .nodes
        .iter()
        .filter(|node| node.active)
        .map(|node| node.row.id)
        .collect();
    assert_eq!(active, vec![first.id, forked.id]);
    assert!(
        tree.nodes
            .iter()
            .all(|node| node.created_at > Timestamp::UNIX_EPOCH),
        "every node carries its timestamp"
    );
}

// -------------------------------------------------------------------- turns

#[tokio::test]
async fn a_turn_is_recorded_and_finished() {
    let (fixture, session) = Fixture::with_session().await;
    let turn = TurnId::new();
    fixture
        .store
        .start_turn(session.id, turn, Timestamp::now())
        .await
        .expect("start");

    fixture
        .store
        .finish_turn(
            session.id,
            turn,
            Some(
                TurnCompletion::new(StopReason::ModelDone).with_usage(Usage {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    reasoning_tokens: None,
                    requests: 1,
                }),
            ),
        )
        .await
        .expect("finish");

    // A second finish for the same turn is idempotent enough to be harmless, but an unknown turn
    // is a caller bug and is reported.
    let error = fixture
        .store
        .finish_turn(session.id, TurnId::new(), None)
        .await
        .expect_err("that turn was never started");
    assert!(matches!(error, StoreError::UnknownTurn(_)));
}

#[tokio::test]
async fn open_turns_lists_only_open_turns_across_sessions() {
    // Startup recovery's read: one open turn, one finished turn, two sessions — the answer must
    // name exactly the open one.
    let fixture = Fixture::new().await;
    let first = fixture.new_session().await.id;
    let second = fixture.new_session().await.id;
    let open_turn = TurnId::new();
    let closed_turn = TurnId::new();
    fixture
        .store
        .start_turn(first, open_turn, Timestamp::now())
        .await
        .expect("start the open one");
    fixture
        .store
        .start_turn(second, closed_turn, Timestamp::now())
        .await
        .expect("start the closed one");
    fixture
        .store
        .finish_turn(second, closed_turn, None)
        .await
        .expect("close it");

    assert_eq!(
        fixture.store.open_turns().await.expect("read"),
        vec![(first, open_turn)],
        "exactly the open turn, with its session"
    );

    fixture
        .store
        .finish_turn(first, open_turn, None)
        .await
        .expect("close the last one");
    assert!(
        fixture.store.open_turns().await.expect("read").is_empty(),
        "closing it removes it from the answer"
    );
}

#[tokio::test]
async fn a_failed_turn_keeps_its_stop_reason_empty() {
    let (fixture, session) = Fixture::with_session().await;
    let turn = TurnId::new();
    fixture
        .store
        .start_turn(session.id, turn, Timestamp::now())
        .await
        .expect("start");
    fixture
        .store
        .finish_turn(session.id, turn, None)
        .await
        .expect("finish as failed");

    // Read the row back through the engine: "still running" and "failed" are told apart by
    // `ended_at` being set while `stop_reason` stays NULL.
    let conn = open_directly(&fixture.path).await;
    let mut rows = conn
        .query(
            "SELECT ended_at, stop_reason FROM turns WHERE id = ?1",
            [turn.to_string()],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("step").expect("the turn row");
    assert!(matches!(row.get_value(0), Ok(turso::Value::Integer(_))));
    assert!(matches!(row.get_value(1), Ok(turso::Value::Null)));
}

#[tokio::test]
async fn a_turn_in_a_missing_session_says_the_session_is_missing() {
    let fixture = Fixture::new().await;
    let gone = SessionId::new();
    let turn = TurnId::new();

    assert_eq!(
        fixture
            .store
            .start_turn(gone, turn, Timestamp::now())
            .await
            .expect_err("there is no such session"),
        StoreError::SessionNotFound(gone),
        "not the foreign key's message, which would read as a broken store"
    );
    assert_eq!(
        fixture
            .store
            .finish_turn(gone, turn, None)
            .await
            .expect_err("there is no such session"),
        StoreError::SessionNotFound(gone),
        "and finishing names the session, not a turn that was never started"
    );
}

// ------------------------------------------------------------------ exports

#[tokio::test]
async fn export_writes_the_active_branch_and_refuses_to_overwrite() {
    let (fixture, session) = Fixture::with_session().await;
    let first = say(&fixture.store, &session, "one", None).await;
    let second = say(&fixture.store, &session, "two", Some(first.id)).await;
    fixture
        .store
        .edit_fork(session.id, second.id, Content::text("two, said better"))
        .await
        .expect("fork");

    let dir = tempfile::tempdir().expect("a tempdir");
    let path = dir.path().join("export.jsonl");
    let lines = fixture
        .store
        .export_jsonl(session.id, path.clone(), false)
        .await
        .expect("export");
    assert_eq!(lines, 2, "only the active branch");

    let body = std::fs::read_to_string(&path).expect("read the export");
    let parsed: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is JSON"))
        .collect();
    assert_eq!(parsed[0]["v"], hatchery_store::FORMAT_VERSION);
    assert_eq!(parsed[1]["item"]["payload"]["text"], "two, said better");

    let error = fixture
        .store
        .export_jsonl(session.id, path.clone(), false)
        .await
        .expect_err("must not overwrite");
    assert!(matches!(error, StoreError::ExportExists(_)));
}

#[tokio::test]
async fn export_all_branches_names_every_tip() {
    let (fixture, session) = Fixture::with_session().await;
    let first = say(&fixture.store, &session, "one", None).await;
    let second = say(&fixture.store, &session, "two", Some(first.id)).await;
    let forked = fixture
        .store
        .edit_fork(session.id, second.id, Content::text("two, said better"))
        .await
        .expect("fork");

    let dir = tempfile::tempdir().expect("a tempdir");
    let path = dir.path().join("all.jsonl");
    let lines = fixture
        .store
        .export_jsonl(session.id, path.clone(), true)
        .await
        .expect("export");
    assert_eq!(lines, 3, "every item, on either branch");

    let body = std::fs::read_to_string(&path).expect("read");
    let parsed: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is JSON"))
        .collect();
    let shared = &parsed[0];
    assert_eq!(
        shared["item"]["id"],
        first.id.to_string(),
        "the oldest item comes first"
    );
    let mut branches: Vec<String> = shared["branches"]
        .as_array()
        .expect("a branches list")
        .iter()
        .map(|value| value.as_str().expect("an id").to_owned())
        .collect();
    branches.sort();
    let mut expected = vec![second.id.to_string(), forked.id.to_string()];
    expected.sort();
    assert_eq!(branches, expected, "the shared ancestor names both tips");

    let order: Vec<String> = parsed
        .iter()
        .map(|line| line["item"]["id"].as_str().expect("an id").to_owned())
        .collect();
    assert_eq!(
        order,
        vec![
            first.id.to_string(),
            second.id.to_string(),
            forked.id.to_string()
        ],
        "oldest first: an audit file's line order must not depend on the tree's shape"
    );
}

#[tokio::test]
async fn exporting_an_empty_session_writes_an_empty_file() {
    // "Nothing has happened yet" is a legitimate thing to be asked for: an audit export of a
    // fresh session must produce an empty artifact rather than an error the caller has to
    // special-case, and rather than a placeholder line a reader would have to recognise.
    let (fixture, session) = Fixture::with_session().await;
    let dir = tempfile::tempdir().expect("a tempdir");

    for all_branches in [false, true] {
        let path = dir.path().join(format!("empty-{all_branches}.jsonl"));
        let lines = fixture
            .store
            .export_jsonl(session.id, path.clone(), all_branches)
            .await
            .expect("export");
        assert_eq!(lines, 0, "nothing to write");
        assert_eq!(
            std::fs::read_to_string(&path).expect("the file was created"),
            "",
            "and it is empty"
        );
    }
}

// --------------------------------------------------------------- durability

#[tokio::test]
async fn migrations_run_once_and_both_records_agree() {
    let dir = tempfile::tempdir().expect("a tempdir");
    let path = dir.path().join("hatchery.db");
    let store = TursoStore::open(&path).await.expect("open");
    let session = store
        .create_session(Session {
            id: SessionId::new(),
            title: None,
            mode: SessionModeId::new("chat"),
            workspace: None,
            model: ModelRef::new("deepseek", "deepseek-chat"),
            config_patch: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            active_branch_head: None,
            generation: 0,
            status: SessionStatus::Idle,
        })
        .await
        .expect("create");
    say(&store, &session, "survives", None).await;
    store.shutdown().await.expect("shutdown");

    // Reopening must not try to migrate again, and both version records must agree.
    let again = TursoStore::open(&path).await.expect("reopen");
    let conn = open_directly(&path).await;
    let mut rows = conn.query("PRAGMA user_version", ()).await.expect("query");
    let row = rows.next().await.expect("step").expect("a row");
    let version = match row.get_value(0) {
        Ok(turso::Value::Integer(version)) => version,
        other => panic!("user_version is not an integer: {other:?}"),
    };
    assert_eq!(version, i64::from(hatchery_store::latest_version()));

    let mut rows = conn
        .query(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            (),
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("step").expect("a row");
    assert_eq!(
        row.get_value(0).expect("a value"),
        turso::Value::Text(version.to_string()),
        "the two records must agree"
    );

    let chain = again
        .rebuild_chain(session.id, None)
        .await
        .expect("chain after reopen");
    assert_eq!(texts(&chain), vec!["survives"]);
    again.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn a_database_from_a_newer_build_is_refused() {
    // Reading a schema this build does not know is worse than refusing to start
    // (docs/design/storage.md §3). The refusal is behaviour, so it gets a test: without one,
    // inverting the comparison ships silently and old binaries start misreading new databases.
    let fixture = Fixture::new().await;
    fixture.store.shutdown().await.expect("shutdown");

    let conn = open_directly(&fixture.path).await;
    conn.execute("PRAGMA user_version = 99", ())
        .await
        .expect("pretend a newer hatchery wrote this file");
    drop(conn);

    let error = match TursoStore::open(&fixture.path).await {
        // `expect_err` would need `TursoStore: Debug`; the match says the same thing.
        Ok(_) => panic!("a newer schema must be refused, not guessed at"),
        Err(error) => error,
    };
    assert!(
        matches!(error, StoreError::Migration { version: 99, .. }),
        "the refusal must name the version it saw: {error}"
    );
}

#[tokio::test]
async fn a_database_whose_version_record_was_deleted_is_refused() {
    // `PRAGMA user_version` and `schema_meta.schema_version` are two records of one fact, so a
    // database carrying only one has been touched by something that does not respect migrations.
    // Guessing which record is right means possibly running migrations against a schema that is
    // already there, so the store refuses to open (storage.md §6).
    let fixture = Fixture::new().await;
    fixture.store.shutdown().await.expect("shutdown");

    let conn = open_directly(&fixture.path).await;
    conn.execute("DELETE FROM schema_meta WHERE key = 'schema_version'", ())
        .await
        .expect("remove our own record behind the store's back");
    drop(conn);

    let error = match TursoStore::open(&fixture.path).await {
        Ok(_) => panic!("a half-recorded schema must be refused, not opened"),
        Err(error) => error,
    };
    let latest = hatchery_store::latest_version();
    assert!(
        matches!(error, StoreError::Migration { version, .. } if version == latest),
        "the refusal must name the version the engine's counter still reports: {error}"
    );
}

#[tokio::test]
async fn dropping_rows_mid_result_does_not_wedge_the_next_write() {
    // The engine contract `sql::drain` exists for, pinned so an upgrade that changes it fails
    // here rather than in production: a statement dropped with rows still pending must not roll
    // back a LATER write on the same connection. Measured benign on turso 0.7.2 for this shape
    // (the vendor comment lives at the `Statement::query_row` implementation, which this path
    // does not go through) — the store drains on every path anyway, because "benign on the
    // version we tested" is not a contract, and the failure mode if it ever bites is a store
    // that errors on every write until it is restarted.
    let (fixture, session) = Fixture::with_session().await;
    let first = say(&fixture.store, &session, "one", None).await;
    say(&fixture.store, &session, "two", Some(first.id)).await;

    let conn = open_directly(&fixture.path).await;
    let mut rows = conn
        .query(
            "SELECT id, kind, payload FROM items WHERE session_id = ?1",
            [session.id.to_string()],
        )
        .await
        .expect("query");
    {
        // Consume one row of a multi-row result…
        let row = rows.next().await.expect("the first row");
        assert!(row.is_some(), "two items were written");
    }
    // …then drop the statement with the rest still pending: the exact shape an early `?` inside
    // a row loop used to produce.
    drop(rows);

    conn.execute(
        "INSERT INTO schema_meta (key, value) VALUES ('tripwire', '1')",
        (),
    )
    .await
    .expect("a write after an undrained drop must still succeed on this engine");
    conn.execute("DELETE FROM schema_meta WHERE key = 'tripwire'", ())
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn invariant_the_database_refuses_to_update_an_item() {
    // The store has no API that rewrites an item; this pins the layer below, which is what makes
    // invariant 3 hold even if someone later adds one (the trigger is from the M0 spike).
    let (fixture, session) = Fixture::with_session().await;
    let item = say(&fixture.store, &session, "original", None).await;

    let conn = open_directly(&fixture.path).await;
    let error = conn
        .execute(
            "UPDATE items SET payload = '{\"text\":\"rewritten\"}' WHERE id = ?1",
            [item.id.to_string()],
        )
        .await
        .expect_err("the trigger must abort");
    assert!(
        error.to_string().contains("append-only"),
        "the abort should carry the trigger's message: {error}"
    );

    let read = fixture.store.item(session.id, item.id).await.expect("read");
    assert_eq!(texts(&[read]), vec!["original"]);
}

#[tokio::test]
async fn a_corrupt_payload_is_reported_with_its_item() {
    let (fixture, session) = Fixture::with_session().await;
    let item = say(&fixture.store, &session, "ok", None).await;

    let conn = open_directly(&fixture.path).await;
    // Write a payload that does not match its kind, the way a hand-edited database or a future
    // schema would. `items` refuses UPDATE, so the corruption is done via delete and insert.
    conn.execute("DELETE FROM items WHERE id = ?1", [item.id.to_string()])
        .await
        .expect("remove the good row");
    conn.execute(
        "INSERT INTO items (id, session_id, parent_id, turn_id, kind, payload, created_at) \
         VALUES (?1, ?2, NULL, NULL, 'user_message', '{\"parts\":42}', 1)",
        (item.id.to_string(), session.id.to_string()),
    )
    .await
    .expect("insert the corrupt row");

    match fixture.store.item(session.id, item.id).await {
        Err(StoreError::CorruptItem { id, message }) => {
            assert_eq!(id, item.id);
            assert!(
                message.contains("42"),
                "the report should quote what could not be read: {message}"
            );
        }
        other => panic!("expected a corrupt-item report, got {other:?}"),
    }
}
