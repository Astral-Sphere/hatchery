//! The store's branch operations against an independently written model.
//!
//! `hatchery-testkit`'s `ReferenceTree` is a second implementation: a `HashMap` and a head
//! pointer, no SQL, no shared code. Every step of a random script is applied to both, and after
//! each one the active branch must match — same items, same parents, same order, same head.
//!
//! This is the test that would catch a tree walk that silently drops a branch, a cascade that
//! removes one item too many, or a head that moves when it should not.

use proptest::prelude::*;

use hatchery_protocol::{
    Content, Item, ItemId, ItemKind, ItemKindTag, ModelRef, Session, SessionId, SessionModeId,
    SessionStatus, Timestamp,
};
use hatchery_store::{SessionStore, TursoStore};
use hatchery_testkit::ReferenceTree;

/// What one scripted step does. The generator emits codes; `code % 4` picks the operation, so the
/// script is reproducible from the failure output alone.
const APPEND: u8 = 0;
const EDIT: u8 = 1;
const SWITCH: u8 = 2;
// 3 is a delete, matched by the wildcard arm below: an enum-free `u8` match cannot be proved
// exhaustive over named constants.

/// Every method the test has as an oracle.
fn kinds() -> [fn(u64) -> ItemKind; 3] {
    [
        |n| ItemKind::UserMessage(Content::text(format!("message {n}"))),
        |n| ItemKind::AssistantMessage(Content::text(format!("answer {n}"))),
        |n| {
            ItemKind::BranchNote(hatchery_protocol::BranchNote {
                note: format!("note {n}"),
            })
        },
    ]
}

fn kind_for(n: u64) -> ItemKind {
    let table = kinds();
    (table[(n % table.len() as u64) as usize])(n)
}

/// The branch as `(id, parent, kind)`, which is the shape both implementations are compared on.
type ChainShape = Vec<(ItemId, Option<ItemId>, ItemKindTag)>;

fn shape(items: &[Item]) -> ChainShape {
    items
        .iter()
        .map(|item| (item.id, item.parent, item.kind_tag()))
        .collect()
}

/// Applies one step to the store and the model.
async fn apply(
    store: &TursoStore,
    session: &Session,
    model: &mut ReferenceTree,
    code: u8,
    step: u64,
) {
    let op = code % 4;
    let ids = model.ids();

    match op {
        APPEND => {
            let item = model.append(kind_for(step));
            store
                .append_item(item)
                .await
                .unwrap_or_else(|error| panic!("append failed at step {step}: {error}"));
        }
        EDIT => {
            let Some(target) = ids.get(usize::from(code / 4) % ids.len().max(1)).copied() else {
                // Nothing to edit yet; append instead so every step changes something.
                let item = model.append(kind_for(step));
                store.append_item(item).await.expect("append");
                return;
            };
            let expected = model.edit_fork(target);
            let stored = store
                .edit_fork(
                    session.id,
                    target,
                    Content::text(format!("edited at step {step}")),
                )
                .await;
            match expected {
                None => assert!(
                    stored.is_err(),
                    "the model refused to edit {target} at step {step} but the store did not"
                ),
                Some(expected) => {
                    let stored = stored
                        .unwrap_or_else(|error| panic!("edit failed at step {step}: {error}"));
                    assert_eq!(
                        (stored.parent, stored.kind_tag()),
                        (expected.parent, expected.kind_tag()),
                        "the store's fork put the item somewhere else at step {step}"
                    );
                    // The two sides mint their own ids; the model adopts the store's so the
                    // branches stay comparable.
                    model.rename(expected.id, stored.id);
                }
            }
        }
        SWITCH => {
            let Some(target) = ids.get(usize::from(code / 4) % ids.len().max(1)).copied() else {
                return;
            };
            assert!(model.switch_branch(target), "{target} came from the model");
            store
                .switch_branch(session.id, target)
                .await
                .unwrap_or_else(|error| panic!("switch failed at step {step}: {error}"));
        }
        _ => {
            let Some(target) = ids.get(usize::from(code / 4) % ids.len().max(1)).copied() else {
                return;
            };
            let expected = model.delete_branch(target);
            let stored = store.delete_branch(session.id, target).await;
            match expected {
                None => assert!(
                    stored.is_err(),
                    "the model refused to delete {target} at step {step} but the store did not"
                ),
                Some(expected_count) => {
                    let stored_count = stored
                        .unwrap_or_else(|error| panic!("delete failed at step {step}: {error}"));
                    assert_eq!(
                        stored_count, expected_count,
                        "the two sides removed a different number of items at step {step}"
                    );
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        // Enough cases to reach every op several times over, few enough that the PR gate stays
        // fast: each case builds a database and walks the tree after every step.
        cases: 64,
        max_shrink_iters: 2000,
        ..ProptestConfig::default()
    })]

    #[test]
    fn the_store_matches_the_reference_model(script in prop::collection::vec(0u8..64, 1..32)) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("a tempdir");
            let store = TursoStore::open(dir.path().join("hatchery.db"))
                .await
                .expect("open the store");
            let now = Timestamp::now();
            let session = store
                .create_session(Session {
                    id: SessionId::new(),
                    title: None,
                    mode: SessionModeId::code(),
                    workspace: None,
                    model: ModelRef::new("deepseek", "deepseek-chat"),
                    config_patch: None,
                    created_at: now,
                    updated_at: now,
                    active_branch_head: None,
                    generation: 0,
                    status: SessionStatus::Idle,
                })
                .await
                .expect("create the session");

            let mut model = ReferenceTree::new(session.id);
            for (index, code) in script.iter().enumerate() {
                let step = index as u64;
                apply(&store, &session, &mut model, *code, step).await;

                let chain = store
                    .rebuild_chain(session.id, None)
                    .await
                    .unwrap_or_else(|error| panic!("rebuild failed at step {step}: {error}"));
                assert_eq!(
                    shape(&chain),
                    model.active_chain(),
                    "the active branch diverged after step {step} (op {code})"
                );

                let tree = store
                    .branch_tree(session.id)
                    .await
                    .expect("branch tree");
                assert_eq!(
                    tree.nodes.len(),
                    model.len(),
                    "the two sides hold a different number of items after step {step}"
                );
                assert_eq!(
                    tree.head,
                    model.head(),
                    "the branch heads diverged after step {step}"
                );

                // The active flag must agree with the chain the model walked.
                let active: Vec<ItemId> = tree
                    .nodes
                    .iter()
                    .filter(|node| node.active)
                    .map(|node| node.row.id)
                    .collect();
                let expected: Vec<ItemId> = model
                    .active_chain()
                    .into_iter()
                    .map(|(id, _, _)| id)
                    .collect();
                let mut active = active;
                active.sort_unstable();
                let mut expected = expected;
                expected.sort_unstable();
                assert_eq!(active, expected, "the active set diverged after step {step}");
            }

            // Every item the model knows about must be readable through the store, which is the
            // other half of "nothing was silently dropped".
            for id in model.ids() {
                store
                    .item(session.id, id)
                    .await
                    .unwrap_or_else(|error| panic!("{id} is in the model but not in the store: {error}"));
            }
        });
    }
}
