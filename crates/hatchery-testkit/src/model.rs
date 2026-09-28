//! A second implementation of the item tree, written for the store's property tests.
//!
//! Deliberately naive: a `HashMap` of items plus a head pointer, with no database, no SQL and no
//! shared code with the store. A property test is only as good as the independence of its oracle,
//! so this file must never import anything from `hatchery-store`.

use std::collections::HashMap;

use hatchery_protocol::{Item, ItemId, ItemKind, ItemKindTag, SessionId, Timestamp};

/// An in-memory item tree with the same operations the store offers.
pub struct ReferenceTree {
    session: SessionId,
    items: HashMap<ItemId, Item>,
    head: Option<ItemId>,
    /// Monotonic clock, so items get distinct timestamps without asking the wall clock.
    clock: i64,
}

/// What one item looks like when the two implementations are compared.
///
/// Ids and parents, not payloads: the property under test is the shape of the branch, and payload
/// equality is already covered by the protocol's own round-trip tests.
pub type ChainShape = Vec<(ItemId, Option<ItemId>, ItemKindTag)>;

impl ReferenceTree {
    /// An empty tree for one session.
    #[must_use]
    pub fn new(session: SessionId) -> Self {
        Self {
            session,
            items: HashMap::new(),
            head: None,
            clock: 1_780_000_000_000,
        }
    }

    /// The active branch head.
    #[must_use]
    pub fn head(&self) -> Option<ItemId> {
        self.head
    }

    /// How many items exist.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when the tree holds no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Appends an item after the head and moves the head onto it.
    pub fn append(&mut self, kind: ItemKind) -> Item {
        let item = self.build(self.head, kind);
        self.head = Some(item.id);
        item
    }

    /// Forks: inserts a sibling of `target` with the *same kind*, and moves the head onto it.
    ///
    /// Keeping the kind is the point of an edit — the user rewrote what that item said, they did
    /// not turn a message into a tool call. The content itself is the editor's business; this model
    /// tracks structure only.
    ///
    /// Returns `None` for an unknown item, and for a kind that carries no editable content — the
    /// same rule the store enforces with `StoreError::NotEditable`.
    pub fn edit_fork(&mut self, target: ItemId) -> Option<Item> {
        let original = self.items.get(&target)?;
        match original.kind {
            ItemKind::UserMessage(_) | ItemKind::AssistantMessage(_) => {}
            _ => return None,
        }
        let parent = original.parent;
        let kind = original.kind.clone();
        let item = self.build(parent, kind);
        self.head = Some(item.id);
        Some(item)
    }

    /// Points the head at an existing item.
    pub fn switch_branch(&mut self, head: ItemId) -> bool {
        if self.items.contains_key(&head) {
            self.head = Some(head);
            true
        } else {
            false
        }
    }

    /// Deletes the subtree rooted at `root`.
    ///
    /// Returns `None` when the active head lies inside the subtree — the store refuses that, and
    /// its foreign key refuses it too, so the model must agree.
    pub fn delete_branch(&mut self, root: ItemId) -> Option<u64> {
        if !self.items.contains_key(&root) {
            return Some(0);
        }
        let doomed = self.subtree(root);
        if let Some(head) = self.head
            && doomed.contains(&head)
        {
            return None;
        }
        for id in &doomed {
            self.items.remove(id);
        }
        Some(doomed.len() as u64)
    }

    /// The active branch, root first.
    #[must_use]
    pub fn chain(&self, head: Option<ItemId>) -> ChainShape {
        let mut shape = Vec::new();
        let mut cursor = head;
        let mut guard = 0;
        while let Some(id) = cursor {
            let Some(item) = self.items.get(&id) else {
                break;
            };
            shape.push((item.id, item.parent, item.kind_tag()));
            cursor = item.parent;
            guard += 1;
            assert!(guard <= self.items.len(), "the reference tree has a cycle");
        }
        shape.reverse();
        shape
    }

    /// The active branch, root first.
    #[must_use]
    pub fn active_chain(&self) -> ChainShape {
        self.chain(self.head)
    }

    /// Every item id in the tree, for comparing counts.
    #[must_use]
    pub fn ids(&self) -> Vec<ItemId> {
        let mut ids: Vec<ItemId> = self.items.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Adopts the id another implementation assigned to the item this call just created.
    ///
    /// Ids are each implementation's own business — both mint UUIDv7s and neither knows the
    /// other's — while the *structure* is what the property test compares. Renaming one item keeps
    /// the two comparable without making either side copy the other's logic.
    pub fn rename(&mut self, from: ItemId, to: ItemId) {
        let mut item = self
            .items
            .remove(&from)
            .unwrap_or_else(|| panic!("{from} is not in the reference tree"));
        item.id = to;
        for candidate in self.items.values_mut() {
            if candidate.parent == Some(from) {
                candidate.parent = Some(to);
            }
        }
        if self.head == Some(from) {
            self.head = Some(to);
        }
        self.items.insert(to, item);
    }

    fn build(&mut self, parent: Option<ItemId>, kind: ItemKind) -> Item {
        self.clock += 1;
        let item = Item::with_id(ItemId::new(), self.session, kind)
            .with_turn(hatchery_protocol::TurnId::new())
            .with_created_at(Timestamp::from_unix_millis(self.clock));
        let item = match parent {
            Some(parent) => item.with_parent(parent),
            None => item,
        };
        self.items.insert(item.id, item.clone());
        item
    }

    /// Every id in the subtree rooted at `root`, `root` included.
    fn subtree(&self, root: ItemId) -> Vec<ItemId> {
        let mut found = vec![root];
        let mut index = 0;
        while index < found.len() {
            let parent = found[index];
            for (id, item) in &self.items {
                if item.parent == Some(parent) && !found.contains(id) {
                    found.push(*id);
                }
            }
            index += 1;
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_protocol::{BranchNote, Content};

    fn message(text: &str) -> ItemKind {
        ItemKind::UserMessage(Content::text(text))
    }

    #[test]
    fn appending_builds_a_chain() {
        let session = SessionId::new();
        let mut tree = ReferenceTree::new(session);
        let first = tree.append(message("one"));
        let second = tree.append(message("two"));

        assert_eq!(first.parent, None);
        assert_eq!(second.parent, Some(first.id));
        assert_eq!(tree.head(), Some(second.id));
        assert_eq!(
            tree.active_chain(),
            vec![
                (first.id, None, ItemKindTag::UserMessage),
                (second.id, Some(first.id), ItemKindTag::UserMessage),
            ]
        );
    }

    #[test]
    fn forking_keeps_the_old_branch_and_moves_the_head() {
        let session = SessionId::new();
        let mut tree = ReferenceTree::new(session);
        let first = tree.append(message("one"));
        let second = tree.append(message("two"));

        let forked = tree.edit_fork(second.id).expect("fork");
        assert_eq!(
            forked.parent,
            Some(first.id),
            "the fork hangs off the target's parent"
        );
        assert_eq!(
            forked.kind_tag(),
            second.kind_tag(),
            "an edit keeps the item's kind"
        );
        assert_eq!(tree.head(), Some(forked.id));
        assert_eq!(tree.len(), 3, "the old branch is kept");
        assert_eq!(tree.active_chain().len(), 2);

        // And switching back restores the old branch.
        assert!(tree.switch_branch(second.id));
        assert_eq!(tree.active_chain().len(), 2);
        assert_eq!(tree.active_chain()[1].0, second.id);
    }

    #[test]
    fn forking_an_item_without_content_is_refused() {
        let session = SessionId::new();
        let mut tree = ReferenceTree::new(session);
        let note = tree.append(ItemKind::BranchNote(BranchNote {
            note: "why".to_owned(),
        }));

        assert!(tree.edit_fork(note.id).is_none(), "a note has no content");
        assert!(
            tree.edit_fork(ItemId::new()).is_none(),
            "nor does a missing item"
        );
        assert_eq!(tree.len(), 1, "a refused fork adds nothing");
    }

    #[test]
    fn deleting_refuses_while_the_head_is_inside() {
        let session = SessionId::new();
        let mut tree = ReferenceTree::new(session);
        let root = tree.append(message("root"));
        let first = tree.append(message("one"));
        let tip = tree.append(message("tip"));

        assert_eq!(
            tree.delete_branch(root.id),
            None,
            "the active head lies inside the subtree, so the store (and its foreign key) refuses"
        );
        assert_eq!(tree.len(), 3, "a refused delete changes nothing");

        // Fork from `first`: the head moves outside the tip's subtree, and now it may go.
        let forked = tree.edit_fork(first.id).expect("fork");
        assert_eq!(forked.parent, Some(root.id));
        assert_eq!(tree.head(), Some(forked.id));

        let deleted = tree
            .delete_branch(tip.id)
            .expect("the head is no longer inside");
        assert_eq!(deleted, 1);
        assert_eq!(tree.len(), 3, "the root, the fork point and the fork stay");
        assert_eq!(tree.active_chain().len(), 2, "root then the fork");
    }

    #[test]
    fn deleting_a_sibling_branch_leaves_the_active_one_alone() {
        let session = SessionId::new();
        let mut tree = ReferenceTree::new(session);
        let root = tree.append(message("root"));
        let kept = tree.append(message("kept"));
        let forked = tree.edit_fork(kept.id).expect("fork");

        assert!(tree.switch_branch(forked.id));
        let deleted = tree
            .delete_branch(kept.id)
            .expect("a sibling is not the active branch");
        assert_eq!(deleted, 1);
        assert_eq!(
            tree.active_chain(),
            vec![
                (root.id, None, ItemKindTag::UserMessage),
                (forked.id, Some(root.id), ItemKindTag::UserMessage),
            ]
        );
    }
}
