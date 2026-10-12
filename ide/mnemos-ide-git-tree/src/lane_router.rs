// SPDX-License-Identifier: AGPL-3.0-only
//! Lane routing for the commit graph.
//!
//! Sweep top-to-bottom (newest first) over a `Vec<CommitNode>`, assign each
//! commit to a horizontal lane, and produce the edge geometry needed to draw
//! connector lines between rows.
//!
//! # Algorithm
//!
//! A *slot table* tracks which OID each active lane is waiting for:
//!
//! 1. For each commit, scan left-to-right for the first slot whose expected
//!    OID matches this commit; that is its lane. If none matches, allocate
//!    the leftmost free (None) slot, or grow the table.
//! 2. Close the matched slot. Open one slot per parent:
//!    - first parent -> same lane (linear-run preservation). If a lane to the
//!      right already waits for that parent, the parent moves to this lane:
//!      the first parent always wins the leftmost lane, so the first-parent
//!      chain (the trunk) never jumps lanes after a merge whose merged branch
//!      is newer than the trunk commit below it.
//!    - first parent already waited for by a lane to the left -> join that
//!      lane (this branch merges back into it).
//!    - additional parents -> the lane already waiting for them, else the
//!      leftmost free slot, or a new slot (octopus OK).
//! 3. Record one `Edge` per (child, parent) pair. Because a parent can move
//!    to another lane after an edge to it was recorded, every edge's
//!    `to_lane` is re-resolved once all commits are placed, and its geometry
//!    kind is derived from the final lanes:
//!    - same lane -> `Straight`
//!    - adjacent lanes -> `Diagonal`
//!    - lanes >= 2 apart -> `Bezier`

use std::collections::HashMap;

use crate::model::CommitNode;

// ── public types ─────────────────────────────────────────────────────────────

/// How the connector line between two commits should be drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeKind {
    /// Parent is directly below on the same lane column — a vertical line.
    Straight,
    /// Parent is one lane to the left or right — a short diagonal segment.
    Diagonal,
    /// Parent is two or more lanes away — a smooth Bézier curve.
    Bezier,
}

/// A directed edge from a child commit to one of its parents.
#[derive(Debug, Clone)]
pub struct Edge {
    pub from_oid: String,
    pub to_oid: String,
    pub from_lane: u32,
    pub to_lane: u32,
    pub kind: EdgeKind,
}

/// Full layout produced by `compute_layout`.
#[derive(Debug, Default)]
pub struct GraphLayout {
    /// Lane index for every commit OID in the walk.
    pub commit_lanes: HashMap<String, u32>,
    /// All directed edges (child → parent).
    pub edges: Vec<Edge>,
    /// Highest lane index used (== column count − 1).
    pub max_lane: u32,
}

// ── primary API ──────────────────────────────────────────────────────────────

/// Compute the full layout from a newest-first commit slice.
///
/// This is the main entry point. `assign_lanes` is a thin wrapper that also
/// mutates `CommitNode::lane` in-place for callers that only need lane indices.
pub fn compute_layout(commits: &[CommitNode]) -> GraphLayout {
    // Slot table: index = lane, value = OID we are waiting for in that lane.
    let mut slots: Vec<Option<String>> = Vec::new();
    let mut layout = GraphLayout::default();

    for node in commits {
        // ── 1. Determine this commit's lane ──────────────────────────────
        let my_lane = match slots
            .iter()
            .position(|s| s.as_deref() == Some(node.oid.as_str()))
        {
            Some(idx) => {
                slots[idx] = None; // close the waiting slot
                idx as u32
            }
            None => {
                // Not yet expected — find the leftmost free slot or grow.
                let idx = slots
                    .iter()
                    .position(|s| s.is_none())
                    .unwrap_or_else(|| {
                        slots.push(None);
                        slots.len() - 1
                    });
                idx as u32
            }
        };

        layout.commit_lanes.insert(node.oid.clone(), my_lane);
        if my_lane > layout.max_lane {
            layout.max_lane = my_lane;
        }

        // ── 2. Open slots for parents + record edges ─────────────────────
        for (i, parent_oid) in node.parent_oids.iter().enumerate() {
            // Another lane may already be waiting for this parent (two
            // branches sharing an ancestor). Every OID sits in at most one
            // slot: parents are moved between slots, never duplicated.
            let claimed = slots
                .iter()
                .position(|s| s.as_deref() == Some(parent_oid.as_str()));

            let parent_lane: u32 = match claimed {
                Some(claimed) if i == 0 && (my_lane as usize) < claimed => {
                    // First parent claimed by a lane to the right: take it
                    // back so the first-parent chain stays on this lane. At
                    // i == 0, slots[my_lane] is always free (it was just
                    // closed or freshly allocated).
                    slots[claimed] = None;
                    ensure_slot(&mut slots, my_lane as usize, parent_oid.clone());
                    my_lane
                }
                // Claimed by a lane to the left (or this is a merge parent):
                // the edge joins the lane that is already waiting.
                Some(claimed) => claimed as u32,
                None if i == 0 => {
                    // First parent: linear-run preservation, reuse this lane.
                    ensure_slot(&mut slots, my_lane as usize, parent_oid.clone());
                    my_lane
                }
                None => {
                    // Merge parent: allocate leftmost free slot (or grow).
                    let idx = slots
                        .iter()
                        .position(|s| s.is_none())
                        .unwrap_or_else(|| {
                            slots.push(None);
                            slots.len() - 1
                        });
                    slots[idx] = Some(parent_oid.clone());
                    if idx as u32 > layout.max_lane {
                        layout.max_lane = idx as u32;
                    }
                    idx as u32
                }
            };

            layout.edges.push(Edge {
                from_oid: node.oid.clone(),
                to_oid: parent_oid.clone(),
                from_lane: my_lane,
                to_lane: parent_lane,
                kind: edge_kind(my_lane, parent_lane),
            });
        }
    }

    // 3. Re-resolve edge targets.
    // An edge recorded before its parent moved lanes still points at the
    // slot the parent was moved out of. Resolve each target from where the
    // parent commit actually landed, falling back to the slot still waiting
    // for it (parents beyond the walked slice, e.g. past MAX_COMMITS).
    for edge in &mut layout.edges {
        let to_lane = layout
            .commit_lanes
            .get(&edge.to_oid)
            .copied()
            .or_else(|| {
                slots
                    .iter()
                    .position(|s| s.as_deref() == Some(edge.to_oid.as_str()))
                    .map(|idx| idx as u32)
            })
            .unwrap_or(edge.to_lane);
        edge.to_lane = to_lane;
        edge.kind = edge_kind(edge.from_lane, to_lane);
    }

    layout
}

/// Mutate `CommitNode::lane` in-place using `compute_layout`.
///
/// Convenience for callers that only need the lane index and not the full
/// edge geometry (see `GitTreeView::set_commits`). The app itself calls
/// `compute_layout` and `GitTreeView::set_layout`; this has no production
/// callers outside this module's tests.
pub fn assign_lanes(commits: &mut [CommitNode]) {
    let layout = compute_layout(commits);
    for node in commits.iter_mut() {
        if let Some(&lane) = layout.commit_lanes.get(&node.oid) {
            node.lane = lane as usize;
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Geometry for a connector between two lanes.
fn edge_kind(from_lane: u32, to_lane: u32) -> EdgeKind {
    match from_lane.abs_diff(to_lane) {
        0 => EdgeKind::Straight,
        1 => EdgeKind::Diagonal,
        _ => EdgeKind::Bezier,
    }
}

/// Ensure `slots[idx]` exists and set it to `oid`. Grows the vec if needed.
fn ensure_slot(slots: &mut Vec<Option<String>>, idx: usize, oid: String) {
    if idx >= slots.len() {
        slots.resize(idx + 1, None);
    }
    slots[idx] = Some(oid);
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CommitNode;

    fn node(oid: &str, parents: &[&str]) -> CommitNode {
        CommitNode {
            oid: oid.to_string(),
            short_oid: oid[..7.min(oid.len())].to_string(),
            summary: String::new(),
            author: String::new(),
            committer_time: 0,
            parent_oids: parents.iter().map(|s| s.to_string()).collect(),
            branch_refs: Vec::new(),
            is_head: false,
            lane: 0,
        }
    }

    /// Graph (newest-first):
    ///
    /// ```text
    /// E (octopus merge of B, C, D)   lane 0
    /// D ── A                         lane 1 (or 2)
    /// C ── B                         lane 2 (or 1)
    /// B ── A                         lane 0
    /// A                              lane 0
    /// ```
    #[test]
    fn three_branch_merge_lanes() {
        let commits = vec![
            node("eeeeeee", &["bbbbbbb", "ccccccc", "ddddddd"]),
            node("ddddddd", &["aaaaaaa"]),
            node("ccccccc", &["bbbbbbb"]),
            node("bbbbbbb", &["aaaaaaa"]),
            node("aaaaaaa", &[]),
        ];

        let layout = compute_layout(&commits);

        // Merge commit heads the walk — should land on lane 0.
        assert_eq!(layout.commit_lanes["eeeeeee"], 0, "merge on lane 0");

        // All lane indices within a sane bound.
        for &lane in layout.commit_lanes.values() {
            assert!(lane < 8, "lane out of range: {lane}");
        }

        // First-parent chain E → B → A must share a lane.
        assert_eq!(
            layout.commit_lanes["bbbbbbb"],
            layout.commit_lanes["aaaaaaa"],
            "B and A on same first-parent lane"
        );

        // max_lane reflects the number of parallel lanes used.
        assert!(layout.max_lane >= 1, "at least 2 lanes for 3-way merge");
    }

    #[test]
    fn three_branch_merge_edge_geometry() {
        let commits = vec![
            node("eeeeeee", &["bbbbbbb", "ccccccc", "ddddddd"]),
            node("ddddddd", &["aaaaaaa"]),
            node("ccccccc", &["bbbbbbb"]),
            node("bbbbbbb", &["aaaaaaa"]),
            node("aaaaaaa", &[]),
        ];

        let layout = compute_layout(&commits);

        // E has 3 parents → 3 edges originating from "eeeeeee".
        let e_edges: Vec<&Edge> = layout
            .edges
            .iter()
            .filter(|e| e.from_oid == "eeeeeee")
            .collect();
        assert_eq!(e_edges.len(), 3, "merge commit has 3 outgoing edges");

        // First-parent edge E→B must be Straight (same lane).
        let e_to_b = e_edges
            .iter()
            .find(|e| e.to_oid == "bbbbbbb")
            .expect("E→B edge");
        assert_eq!(
            e_to_b.kind,
            EdgeKind::Straight,
            "first-parent edge is Straight"
        );

        // Merge-parent edges must be non-straight (Diagonal or Bezier).
        for edge in e_edges.iter().filter(|e| e.to_oid != "bbbbbbb") {
            assert_ne!(
                edge.kind,
                EdgeKind::Straight,
                "merge-parent edge {} is not Straight",
                edge.to_oid
            );
        }

        // Every edge's from_lane and to_lane must be within [0, max_lane].
        for edge in &layout.edges {
            assert!(edge.from_lane <= layout.max_lane);
            assert!(edge.to_lane <= layout.max_lane);
        }
    }

    #[test]
    fn linear_history_all_straight() {
        // A simple chain: D → C → B → A — all edges should be Straight.
        let commits = vec![
            node("ddddddd", &["ccccccc"]),
            node("ccccccc", &["bbbbbbb"]),
            node("bbbbbbb", &["aaaaaaa"]),
            node("aaaaaaa", &[]),
        ];

        let layout = compute_layout(&commits);

        assert_eq!(layout.max_lane, 0, "linear history uses exactly one lane");
        for edge in &layout.edges {
            assert_eq!(edge.kind, EdgeKind::Straight, "all edges straight");
        }
    }

    #[test]
    fn assign_lanes_mutates_nodes() {
        let mut commits = vec![
            node("eeeeeee", &["bbbbbbb", "ccccccc", "ddddddd"]),
            node("ddddddd", &["aaaaaaa"]),
            node("ccccccc", &["bbbbbbb"]),
            node("bbbbbbb", &["aaaaaaa"]),
            node("aaaaaaa", &[]),
        ];

        assign_lanes(&mut commits);

        // After assign_lanes the merge commit is on lane 0.
        let merge = commits.iter().find(|n| n.oid == "eeeeeee").unwrap();
        assert_eq!(merge.lane, 0);

        // First-parent chain B and A share a lane.
        let b = commits.iter().find(|n| n.oid == "bbbbbbb").unwrap();
        let a = commits.iter().find(|n| n.oid == "aaaaaaa").unwrap();
        assert_eq!(b.lane, a.lane);
    }

    /// The merged branch F is newer than the trunk commit P, so F is walked
    /// first and its lane claims X before P is reached. The trunk must still
    /// keep lane 0 all the way down; F's edge to X bends back into it.
    ///
    /// ```text
    /// M (merge of P, F)   lane 0
    /// F -- X              lane 1
    /// P -- X              lane 0
    /// X                   lane 0
    /// ```
    #[test]
    fn trunk_keeps_its_lane_when_the_merged_branch_is_newer() {
        let commits = vec![
            node("mmmmmmm", &["ppppppp", "fffffff"]),
            node("fffffff", &["xxxxxxx"]),
            node("ppppppp", &["xxxxxxx"]),
            node("xxxxxxx", &[]),
        ];

        let layout = compute_layout(&commits);

        assert_eq!(layout.commit_lanes["mmmmmmm"], 0);
        assert_eq!(layout.commit_lanes["ppppppp"], 0, "trunk P stays on lane 0");
        assert_eq!(layout.commit_lanes["xxxxxxx"], 0, "trunk X stays on lane 0");
        assert_eq!(layout.commit_lanes["fffffff"], 1, "merged branch on lane 1");

        let f_to_x = layout
            .edges
            .iter()
            .find(|e| e.from_oid == "fffffff" && e.to_oid == "xxxxxxx")
            .expect("F->X edge");
        assert_eq!(f_to_x.from_lane, 1);
        assert_eq!(f_to_x.to_lane, 0, "edge re-resolved to where X landed");
        assert_eq!(f_to_x.kind, EdgeKind::Diagonal);

        let p_to_x = layout
            .edges
            .iter()
            .find(|e| e.from_oid == "ppppppp" && e.to_oid == "xxxxxxx")
            .expect("P->X edge");
        assert_eq!(p_to_x.kind, EdgeKind::Straight);

        // Every edge must agree with the lanes its endpoints were placed on.
        for edge in &layout.edges {
            assert_eq!(edge.from_lane, layout.commit_lanes[&edge.from_oid]);
            if let Some(&lane) = layout.commit_lanes.get(&edge.to_oid) {
                assert_eq!(edge.to_lane, lane, "edge {} -> {}", edge.from_oid, edge.to_oid);
            }
        }
    }

    /// A parent past the end of the walked slice keeps the lane still
    /// waiting for it.
    #[test]
    fn unwalked_parent_resolves_to_its_waiting_slot() {
        let commits = vec![
            node("mmmmmmm", &["ppppppp", "fffffff"]),
            node("fffffff", &["xxxxxxx"]),
            node("ppppppp", &["xxxxxxx"]),
        ];

        let layout = compute_layout(&commits);

        let f_to_x = layout
            .edges
            .iter()
            .find(|e| e.from_oid == "fffffff")
            .expect("F->X edge");
        assert_eq!(f_to_x.to_lane, 0, "X moved to P's lane before the walk ended");
        assert_eq!(f_to_x.kind, EdgeKind::Diagonal);
    }
}
