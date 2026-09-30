//! Walking the item tree in Rust.
//!
//! The engine has no `WITH RECURSIVE` (measured in the M0 spike, ADR-0010), so the two tree
//! operations the store needs live here as pure functions over a skeleton: pairs of ids and a
//! kind. Pure functions because this is the part that can be wrong in interesting ways — a
//! mis-walked chain silently drops history, and the property test compares exactly this against a
//! second implementation.

use std::collections::{HashMap, HashSet, VecDeque};

use hatchery_protocol::{ItemId, ItemKindTag, TurnId};

/// One row of a session's tree, without its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkeletonRow {
    /// The item.
    pub id: ItemId,
    /// Its parent, if it has one.
    pub parent: Option<ItemId>,
    /// What kind of item it is.
    pub kind: ItemKindTag,
    /// The turn it belongs to, if any.
    pub turn: Option<TurnId>,
}

impl SkeletonRow {
    /// A row with no parent and no turn.
    #[must_use]
    pub const fn new(id: ItemId, kind: ItemKindTag) -> Self {
        Self {
            id,
            parent: None,
            kind,
            turn: None,
        }
    }

    /// A row with a parent.
    #[must_use]
    pub const fn child(id: ItemId, parent: ItemId, kind: ItemKindTag) -> Self {
        Self {
            id,
            parent: Some(parent),
            kind,
            turn: None,
        }
    }
}

/// Why a tree walk could not finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    /// The requested head is not in the skeleton.
    #[error("item {0} is not part of this session")]
    MissingHead(ItemId),
    /// The parent links form a cycle, which means the data is corrupt.
    #[error("the parent links cycle at item {0}")]
    Cycle(ItemId),
}

/// The branch ending at `head`, root first.
///
/// # Errors
///
/// [`TreeError::MissingHead`] when `head` is unknown, [`TreeError::Cycle`] when the parent links
/// loop — a corrupt database is reported rather than walked forever.
pub fn chain(skeleton: &[SkeletonRow], head: Option<ItemId>) -> Result<Vec<ItemId>, TreeError> {
    let by_id: HashMap<ItemId, &SkeletonRow> = skeleton.iter().map(|row| (row.id, row)).collect();
    let Some(head) = head else {
        return Ok(Vec::new());
    };
    if !by_id.contains_key(&head) {
        return Err(TreeError::MissingHead(head));
    }

    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = Some(head);
    while let Some(id) = cursor {
        if !seen.insert(id) {
            return Err(TreeError::Cycle(id));
        }
        let row = by_id.get(&id).ok_or(TreeError::MissingHead(id))?;
        chain.push(row.id);
        cursor = row.parent;
    }
    chain.reverse();
    Ok(chain)
}

/// The subtree rooted at `root`, `root` first, then breadth-first.
///
/// Used to decide what a branch deletion would remove: the store needs the count before the
/// cascade runs, because the engine reports only the rows it deleted directly.
#[must_use]
pub fn subtree(skeleton: &[SkeletonRow], root: ItemId) -> Vec<ItemId> {
    let mut children: HashMap<ItemId, Vec<ItemId>> = HashMap::new();
    for row in skeleton {
        if let Some(parent) = row.parent {
            children.entry(parent).or_default().push(row.id);
        }
    }

    let mut found = Vec::new();
    let mut queue = VecDeque::from([root]);
    let mut seen = HashSet::new();
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        found.push(id);
        if let Some(kids) = children.get(&id) {
            queue.extend(kids.iter().copied());
        }
    }
    found
}

/// Every item's descendant tips: the branches it is an ancestor of.
///
/// Used by the JSONL export to name the branches a line belongs to. A leaf maps to itself, and a
/// shared ancestor maps to all of its branches' tips.
///
/// # Errors
///
/// [`TreeError::Cycle`] when the parent links loop, which means the data is corrupt.
pub fn tips_map(skeleton: &[SkeletonRow]) -> Result<HashMap<ItemId, Vec<ItemId>>, TreeError> {
    let parents: HashMap<ItemId, Option<ItemId>> =
        skeleton.iter().map(|row| (row.id, row.parent)).collect();
    let mut has_children: HashSet<ItemId> = HashSet::new();
    for row in skeleton {
        if let Some(parent) = row.parent {
            has_children.insert(parent);
        }
    }

    // Walked upwards from every tip rather than downwards from every item: iterative, so a
    // ten-thousand-item chain cannot overflow the stack, and a tip visits its ancestors once.
    let mut map: HashMap<ItemId, Vec<ItemId>> = HashMap::new();
    for row in skeleton {
        if has_children.contains(&row.id) {
            continue;
        }
        let tip = row.id;
        let mut cursor = Some(tip);
        let mut steps = 0;
        while let Some(id) = cursor {
            steps += 1;
            if steps > skeleton.len() {
                return Err(TreeError::Cycle(id));
            }
            map.entry(id).or_default().push(tip);
            cursor = parents.get(&id).copied().flatten();
        }
    }
    for tips in map.values_mut() {
        tips.sort_unstable();
        tips.dedup();
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_protocol::ItemId;

    fn id(n: u128) -> ItemId {
        // Built from a string rather than from `uuid::Uuid`: ids are the protocol's business, and
        // the store has no reason to depend on the crate that mints them.
        format!("01890f47-0000-7000-8000-{n:012x}")
            .parse()
            .expect("a valid uuid")
    }

    /// `root -> a -> b`, plus a fork `c` off `a`.
    fn forked() -> Vec<SkeletonRow> {
        vec![
            SkeletonRow::new(id(1), ItemKindTag::UserMessage),
            SkeletonRow::child(id(2), id(1), ItemKindTag::AssistantMessage),
            SkeletonRow::child(id(3), id(2), ItemKindTag::UserMessage),
            SkeletonRow::child(id(4), id(2), ItemKindTag::AssistantMessage),
        ]
    }

    #[test]
    fn a_chain_walks_back_to_the_root_and_comes_out_in_order() {
        let skeleton = forked();
        assert_eq!(
            chain(&skeleton, Some(id(3))).expect("a walkable chain"),
            vec![id(1), id(2), id(3)]
        );
        assert_eq!(
            chain(&skeleton, None).expect("no head means no history"),
            Vec::new()
        );
        assert_eq!(
            chain(&skeleton, Some(id(1))).expect("a single-item branch"),
            vec![id(1)]
        );
    }

    #[test]
    fn a_fork_is_not_on_its_siblings_branch() {
        let skeleton = forked();
        assert!(
            !chain(&skeleton, Some(id(3)))
                .expect("a walkable chain")
                .contains(&id(4)),
            "the sibling must not appear: that is what makes an edit a fork"
        );
    }

    #[test]
    fn an_unknown_head_is_reported_rather_than_ignored() {
        let skeleton = forked();
        assert_eq!(
            chain(&skeleton, Some(id(99))),
            Err(TreeError::MissingHead(id(99)))
        );
    }

    #[test]
    fn a_cycle_is_reported_rather_than_walked_forever() {
        let mut skeleton = forked();
        // Corrupt the data: make the root a child of its own grandchild.
        skeleton[0].parent = Some(id(3));
        assert_eq!(
            chain(&skeleton, Some(id(3))),
            Err(TreeError::Cycle(id(3))),
            "the walk must stop where it re-enters itself"
        );
    }

    #[test]
    fn a_subtree_is_the_root_and_everything_below_it() {
        let skeleton = forked();
        assert_eq!(subtree(&skeleton, id(2)).len(), 3, "two, three and four");
        assert_eq!(subtree(&skeleton, id(1)).len(), 4);
        assert_eq!(subtree(&skeleton, id(3)), vec![id(3)]);
        assert_eq!(subtree(&skeleton, id(99)), vec![id(99)]);
    }

    #[test]
    fn tips_name_every_branch_below_an_item() {
        let skeleton = forked();
        let tips = tips_map(&skeleton).expect("a walkable tree");
        assert_eq!(tips[&id(1)], vec![id(3), id(4)]);
        assert_eq!(tips[&id(2)], vec![id(3), id(4)]);
        assert_eq!(tips[&id(3)], vec![id(3)], "a leaf is its own tip");
        assert_eq!(tips[&id(4)], vec![id(4)]);
    }

    #[test]
    fn a_cycle_in_the_tips_walk_is_reported() {
        let mut skeleton = forked();
        skeleton[0].parent = Some(id(3));
        assert!(matches!(tips_map(&skeleton), Err(TreeError::Cycle(_))));
    }
}
