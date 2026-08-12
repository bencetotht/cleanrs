//! Reclaimability interval calculation.

use std::collections::HashMap;

use serde::Serialize;

use crate::fs::{Arena, NodeIndex};

/// Confidence derived from the available bounds and scan coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Both bounds are available and effectively agree.
    High,
    /// Both bounds are available but sharing widens the interval.
    Medium,
    /// A bound is unavailable or traversal coverage is incomplete.
    Low,
}

/// An honest interval for bytes expected to become free after deleting a set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReclaimEstimate {
    /// Sum of per-file immediately-private bytes, when available.
    pub floor: Option<u64>,
    /// Clone-family and hardlink-deduplicated allocated bytes.
    pub ceiling: Option<u64>,
    /// Confidence derived from the bounds, never hand assigned.
    pub confidence: Confidence,
    /// Short explanation for a widened or incomplete estimate.
    pub explanation: Option<String>,
}

/// Compute an interval for a node and all of its descendants.
#[must_use]
pub fn estimate_subtree(
    arena: &Arena,
    root: NodeIndex,
    coverage_complete: bool,
) -> ReclaimEstimate {
    let mut stack = vec![root];
    let mut inodes: HashMap<(u64, u64), InodeFacts> = HashMap::new();

    while let Some(index) = stack.pop() {
        let node = &arena.nodes[index.as_usize()];
        stack.extend(arena.children(index));

        if !node.kind.is_file_like() {
            continue;
        }

        let facts = inodes
            .entry((node.device, node.inode))
            .or_insert(InodeFacts {
                allocated: node.allocated_size,
                private: node.private_size,
                link_count: node.link_count.max(1),
                clone_id: node.clone_id.filter(|id| *id != 0),
                clone_refcount: node.clone_refcount,
                observed_links: 0,
            });
        facts.observed_links += 1;
    }

    let mut clone_allocated: HashMap<u64, u64> = HashMap::new();
    let mut unshared_allocated = 0_u64;
    let mut floor = 0_u64;
    let mut private_available = true;
    let mut sharing = false;
    for facts in inodes.values() {
        let all_hardlinks_selected = facts.observed_links as u64 >= facts.link_count;
        sharing |= facts.link_count > 1 || facts.clone_refcount.unwrap_or(1) > 1;
        // An outside hardlink pins the whole inode. When all names are selected,
        // private allocation is included once for the inode.
        if all_hardlinks_selected {
            match facts.private {
                Some(value) => floor = floor.saturating_add(value),
                None => private_available = false,
            }
            if let Some(clone_id) = facts.clone_id {
                clone_allocated
                    .entry(clone_id)
                    .and_modify(|value| *value = (*value).max(facts.allocated))
                    .or_insert(facts.allocated);
            } else {
                unshared_allocated = unshared_allocated.saturating_add(facts.allocated);
            }
        }
    }
    let ceiling = unshared_allocated.saturating_add(clone_allocated.values().sum::<u64>());
    let floor = private_available.then_some(floor);
    let confidence = if !coverage_complete || floor.is_none() {
        Confidence::Low
    } else if bounds_are_tight(floor.unwrap_or_default(), ceiling) {
        Confidence::High
    } else {
        Confidence::Medium
    };

    let explanation = match confidence {
        Confidence::High => None,
        Confidence::Medium => Some(if sharing {
            "cloned or hardlinked blocks widen the interval".into()
        } else {
            "filesystem metadata yields different lower and upper bounds".into()
        }),
        Confidence::Low if !coverage_complete => {
            Some("scan coverage is incomplete; inaccessible paths have unknown size".into())
        }
        Confidence::Low => {
            Some("the filesystem did not provide immediately-private size metadata".into())
        }
    };

    ReclaimEstimate {
        floor,
        ceiling: Some(ceiling),
        confidence,
        explanation,
    }
}

struct InodeFacts {
    allocated: u64,
    private: Option<u64>,
    link_count: u64,
    clone_id: Option<u64>,
    clone_refcount: Option<u32>,
    observed_links: usize,
}

fn bounds_are_tight(floor: u64, ceiling: u64) -> bool {
    let width = ceiling.saturating_sub(floor);
    // A one-filesystem-block discrepancy is noise; otherwise use the 1% design
    // tolerance from the confidence contract.
    width <= 4096 || (ceiling > 0 && width.saturating_mul(100) <= ceiling)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{Arena, Node, NodeFlags, NodeKind};

    #[test]
    fn mutual_clones_keep_a_nonzero_ceiling() {
        let mut arena = Arena::new("fixture");
        let root = arena.root();
        for inode in [2, 3] {
            arena.push_child(
                root,
                "clone",
                Node {
                    kind: NodeKind::File,
                    device: 1,
                    inode,
                    allocated_size: 8192,
                    private_size: Some(0),
                    clone_id: Some(42),
                    clone_refcount: Some(2),
                    flags: NodeFlags::empty(),
                    ..Node::default()
                },
            );
        }

        let estimate = estimate_subtree(&arena, root, true);
        assert_eq!(estimate.floor, Some(0));
        assert_eq!(estimate.ceiling, Some(8192));
        assert_eq!(estimate.confidence, Confidence::Medium);
    }

    #[test]
    fn hardlinks_are_counted_once() {
        let mut arena = Arena::new("fixture");
        let root = arena.root();
        for name in ["a", "b"] {
            arena.push_child(
                root,
                name,
                Node {
                    kind: NodeKind::File,
                    device: 1,
                    inode: 7,
                    link_count: 2,
                    allocated_size: 4096,
                    private_size: Some(4096),
                    flags: NodeFlags::empty(),
                    ..Node::default()
                },
            );
        }
        let estimate = estimate_subtree(&arena, root, true);
        assert_eq!(estimate.floor, Some(4096));
        assert_eq!(estimate.ceiling, Some(4096));
    }

    #[test]
    fn hardlink_outside_selection_pins_the_inode() {
        let mut arena = Arena::new("fixture");
        let root = arena.root();
        arena.push_child(
            root,
            "one-of-two-links",
            Node {
                kind: NodeKind::File,
                device: 1,
                inode: 8,
                link_count: 2,
                allocated_size: 4096,
                private_size: Some(4096),
                flags: NodeFlags::empty(),
                ..Node::default()
            },
        );
        let estimate = estimate_subtree(&arena, root, true);
        assert_eq!(estimate.floor, Some(0));
        assert_eq!(estimate.ceiling, Some(0));
        assert_eq!(estimate.confidence, Confidence::High);
    }
}
