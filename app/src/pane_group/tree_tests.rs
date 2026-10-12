use super::*;

#[test]
fn test_split_pane_layout() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];
    let mut root_pane = PaneData::new(panes[0]);

    // Add a pane to the right.
    root_pane.split(panes[0], panes[1], Direction::Right);
    assert_eq!(root_pane.pane_ids(), vec![panes[0], panes[1]]);

    // Insert a vertical (below) pane after the first pane.
    root_pane.split(panes[0], panes[2], Direction::Down);
    assert_eq!(root_pane.pane_ids(), vec![panes[0], panes[2], panes[1]]);

    // Remove the last pane.
    root_pane.remove(panes[1]);
    assert_eq!(root_pane.pane_ids(), vec![panes[0], panes[2]]);

    let panes = [PaneId::dummy_pane_id(); 3];
    let mut root_pane = PaneData::new(panes[0]);

    // Add a pane to the left.
    root_pane.split(panes[0], panes[1], Direction::Left);
    assert_eq!(root_pane.pane_ids(), vec![panes[1], panes[0]]);

    // Add a pane above the first pane.
    root_pane.split(panes[0], panes[2], Direction::Up);
    assert_eq!(root_pane.pane_ids(), vec![panes[2], panes[0], panes[1]]);
}

#[test]
fn test_left_pane_split() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];
    let mut root_pane = PaneData::new(panes[0]);

    root_pane.split(panes[0], panes[1], Direction::Left);
    assert_eq!(root_pane.pane_ids(), vec![panes[1], panes[0]]);

    root_pane.split(panes[0], panes[2], Direction::Left);
    assert_eq!(root_pane.pane_ids(), vec![panes[1], panes[2], panes[0]]);

    root_pane.split(panes[0], panes[3], Direction::Left);
    assert_eq!(
        root_pane.pane_ids(),
        vec![panes[1], panes[2], panes[3], panes[0]]
    );
}

#[test]
fn test_root_split_leaf() {
    let panes = [PaneId::dummy_pane_id(), PaneId::dummy_pane_id()];

    let mut tree = PaneData::new(panes[0]);
    tree.split_root(panes[1], Direction::Down);
    assert_eq!(tree.pane_ids(), vec![panes[0], panes[1]]);
    assert_eq!(
        tree.active_root().as_branch().expect("Should be a branch").axis(),
        SplitDirection::Vertical
    );
}

#[test]
fn test_root_split_same_axis() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];

    // Start with a horizontal split.
    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Right);

    // Add a pane at the start of the split.
    tree.split_root(panes[2], Direction::Left);

    // Add a pane at the end of the split.
    tree.split_root(panes[3], Direction::Right);

    let root = tree.active_root().as_branch().expect("Should be a branch");
    assert_eq!(root.axis(), SplitDirection::Horizontal);
    assert_eq!(
        root.direct_children(),
        vec![panes[2], panes[0], panes[1], panes[3]]
    );
}

#[test]
fn test_root_split_different_axis() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];

    // Start with a horizontal split:
    // -------------
    // |  0  |  1  |
    // -------------
    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Right);

    // Add a pane above, converting the root to a vertical split:
    // -------------
    // |     2     |
    // -------------
    // |  0  |  1  |
    // -------------
    tree.split_root(panes[2], Direction::Up);
    let root = tree.active_root().as_branch().expect("Should be a branch");
    assert_eq!(root.axis(), SplitDirection::Vertical);
    assert_eq!(root.node(0).as_leaf(), Some(panes[2]));
    assert_eq!(
        root.node(1)
            .as_branch()
            .expect("Should be a branch")
            .direct_children(),
        vec![panes[0], panes[1]]
    );

    // Add a pane to the right, converting the root to a horizontal split.
    // -------------------
    // |     2     |     |
    // ------------+  3  |
    // |  0  |  1  |     |
    // -------------------
    tree.split_root(panes[3], Direction::Right);
    let root = tree.active_root().as_branch().expect("Should be a branch");
    assert_eq!(
        root.node(0)
            .as_branch()
            .expect("Should be a branch")
            .get_children(),
        vec![panes[2], panes[0], panes[1]]
    );
    assert_eq!(root.node(1).as_leaf(), Some(panes[3]));
}

#[test]
fn test_move_pane_basic() {
    let panes = [PaneId::dummy_pane_id(), PaneId::dummy_pane_id()];

    // Start with a horizontal split:
    // -------------
    // |  0  |  1  |
    // -------------
    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Right);

    // Move pane 0 to the right of pane 1, which should result in
    // -------------
    // |  1  |  0  |
    // -------------
    tree.move_pane(panes[0], panes[1], Direction::Right);

    let root = tree.active_root().as_branch().expect("Should be a branch");
    assert_eq!(root.axis(), SplitDirection::Horizontal);
    assert_eq!(root.direct_children(), vec![panes[1], panes[0]]);

    // Move pane 0 on top of pane 1, which should result in
    // --------------
    // |     0      |
    // -------------
    // |     1      |
    // -------------
    tree.move_pane(panes[0], panes[1], Direction::Up);

    let root = tree.active_root().as_branch().expect("Should be a branch");
    assert_eq!(root.axis(), SplitDirection::Vertical);
    assert_eq!(root.direct_children(), vec![panes[0], panes[1]]);
}

#[test]
fn test_move_pane_multiple_splits() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];

    // Start with a horizontal split:
    // -------------
    // |  0  |  1  |
    // -------------
    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Right);

    // Add a pane above, converting the root to a vertical split:
    // -------------
    // |     2     |
    // -------------
    // |  0  |  1  |
    // -------------
    tree.split_root(panes[2], Direction::Up);
    let root = tree.active_root().as_branch().expect("Should be a branch");
    assert_eq!(root.axis(), SplitDirection::Vertical);
    assert_eq!(root.node(0).as_leaf(), Some(panes[2]));
    assert_eq!(
        root.node(1)
            .as_branch()
            .expect("Should be a branch")
            .direct_children(),
        vec![panes[0], panes[1]]
    );

    // Add a pane to the right, converting the root to a horizontal split.
    // -------------------
    // |     2     |     |
    // ------------+  3  |
    // |  0  |  1  |     |
    // -------------------
    tree.split_root(panes[3], Direction::Right);
    let root = tree.active_root().as_branch().expect("Should be a branch");
    assert_eq!(
        root.node(0)
            .as_branch()
            .expect("Should be a branch")
            .get_children(),
        vec![panes[2], panes[0], panes[1]]
    );
    assert_eq!(root.node(1).as_leaf(), Some(panes[3]));

    // Move Pane 2 to the left of pane 3, which would result in
    // -------------------------
    // |     |     |     |      |
    // | 0   |  1  |  2  |   3  |
    // |     |     |     |      |
    // -------------------------
    tree.move_pane(panes[2], panes[3], Direction::Left);
    let root = tree.active_root().as_branch().expect("Should be a branch");
    assert_eq!(root.axis(), SplitDirection::Horizontal);
    assert_eq!(
        root.node(0).as_branch().expect("should be branch").axis(),
        SplitDirection::Horizontal
    );
    assert_eq!(
        root.node(0)
            .as_branch()
            .expect("Should be a branch")
            .get_children(),
        vec![panes[0], panes[1]]
    );
    assert_eq!(root.node(1).as_leaf(), Some(panes[2]));
    assert_eq!(root.node(2).as_leaf(), Some(panes[3]));
}

#[test]
fn test_move_pane_no_short_circuit() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];

    // Setup
    // -------------
    // |     0     |
    // -------------
    // |  1  |  2  |
    // -------------
    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Down);
    tree.split(panes[1], panes[2], Direction::Right);

    // Move Pane 1 to the bottom of pane 0.  This should result in a single vertical split
    // with 3 panes, but currently is short circuiting because 1 is already below 0.

    tree.move_pane(panes[1], panes[0], Direction::Down);
    let root = tree.active_root().as_branch().expect("Should be a branch");
    assert_eq!(root.axis(), SplitDirection::Vertical);
    assert_eq!(root.direct_children(), panes.to_vec());
}

#[test]
fn test_move_pane_no_short_circuit_2() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];

    // Setup
    // -------------
    // |     0     |
    // -------------
    // |     1     |
    // -------------
    // |     2     |
    // -------------
    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Down);
    tree.split(panes[1], panes[2], Direction::Down);

    // Move Pane 1 to the left of pane 2.  This should result in a horizontal split
    // with 2 panes, below pane 0.

    tree.move_pane(panes[1], panes[2], Direction::Left);
    let root = tree.active_root().as_branch().expect("Should be a branch");
    assert_eq!(root.axis(), SplitDirection::Vertical);
    assert_eq!(root.node(0).as_leaf().expect("Should be a leaf"), panes[0]);
    assert_eq!(
        root.node(1)
            .as_branch()
            .expect("Should be a branch")
            .direct_children(),
        vec![panes[1], panes[2]]
    );
}

#[test]
fn test_sibling_by_direction() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];

    // Setup
    // -----------------------
    // |         0           |
    // -----------------------
    // |     |     |   3     |
    // |  1  |  2  |---------|
    // |     |     |   4     |
    // -----------------------
    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Down);
    tree.split(panes[1], panes[2], Direction::Right);
    tree.split(panes[2], panes[3], Direction::Right);
    tree.split(panes[3], panes[4], Direction::Down);

    assert_eq!(
        tree.sibling_by_direction(panes[1], Direction::Right),
        Some(panes[2])
    );
    assert_eq!(
        tree.sibling_by_direction(panes[2], Direction::Left),
        Some(panes[1])
    );
    assert_eq!(tree.sibling_by_direction(panes[0], Direction::Right), None);
    assert_eq!(tree.sibling_by_direction(panes[0], Direction::Left), None);
    assert_eq!(tree.sibling_by_direction(panes[2], Direction::Right), None);
    assert_eq!(tree.sibling_by_direction(panes[1], Direction::Left), None);
    assert_eq!(tree.sibling_by_direction(panes[1], Direction::Up), None);
    assert_eq!(tree.sibling_by_direction(panes[1], Direction::Down), None);
    assert_eq!(tree.sibling_by_direction(panes[0], Direction::Up), None);
    assert_eq!(tree.sibling_by_direction(panes[0], Direction::Down), None);

    assert_eq!(tree.sibling_by_direction(panes[3], Direction::Up), None);
    assert_eq!(
        tree.sibling_by_direction(panes[3], Direction::Down),
        Some(panes[4])
    );
    assert_eq!(
        tree.sibling_by_direction(panes[4], Direction::Up),
        Some(panes[3])
    );
    assert_eq!(tree.sibling_by_direction(panes[4], Direction::Down), None);
}

#[test]
fn test_pane_by_direction_simple() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];

    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Right);

    assert_eq!(
        tree.active_root().panes_by_direction(panes[0], Direction::Right),
        FindPaneByDirectionResult::Found(HashSet::from([panes[1]]))
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[0], Direction::Left),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[1], Direction::Right),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[1], Direction::Left),
        FindPaneByDirectionResult::Found(HashSet::from([panes[0]]))
    );

    assert_eq!(
        tree.active_root().panes_by_direction(panes[0], Direction::Up),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[0], Direction::Down),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[1], Direction::Up),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[1], Direction::Down),
        FindPaneByDirectionResult::Located
    );

    assert_eq!(
        tree.active_root().panes_by_direction(panes[2], Direction::Right),
        FindPaneByDirectionResult::NotFound
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[2], Direction::Left),
        FindPaneByDirectionResult::NotFound
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[2], Direction::Up),
        FindPaneByDirectionResult::NotFound
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[2], Direction::Down),
        FindPaneByDirectionResult::NotFound
    );
}

#[test]
fn test_pane_by_direction_multi_split() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];

    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Right);
    tree.split(panes[0], panes[2], Direction::Down);
    tree.split(panes[1], panes[3], Direction::Down);

    assert_eq!(
        tree.active_root().panes_by_direction(panes[0], Direction::Right),
        FindPaneByDirectionResult::Found(HashSet::from([panes[1], panes[3]]))
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[0], Direction::Left),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[0], Direction::Up),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[0], Direction::Down),
        FindPaneByDirectionResult::Found(HashSet::from([panes[2]]))
    );

    assert_eq!(
        tree.active_root().panes_by_direction(panes[1], Direction::Right),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[1], Direction::Left),
        FindPaneByDirectionResult::Found(HashSet::from([panes[0], panes[2]]))
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[1], Direction::Up),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[1], Direction::Down),
        FindPaneByDirectionResult::Found(HashSet::from([panes[3]]))
    );

    assert_eq!(
        tree.active_root().panes_by_direction(panes[2], Direction::Right),
        FindPaneByDirectionResult::Found(HashSet::from([panes[1], panes[3]]))
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[2], Direction::Left),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[2], Direction::Up),
        FindPaneByDirectionResult::Found(HashSet::from([panes[0]]))
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[2], Direction::Down),
        FindPaneByDirectionResult::Located
    );

    assert_eq!(
        tree.active_root().panes_by_direction(panes[3], Direction::Right),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[3], Direction::Left),
        FindPaneByDirectionResult::Found(HashSet::from([panes[0], panes[2]]))
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[3], Direction::Up),
        FindPaneByDirectionResult::Found(HashSet::from([panes[1]]))
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[3], Direction::Down),
        FindPaneByDirectionResult::Located
    );
}

#[test]
fn test_pane_by_direction_multi_level_split() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];

    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[3], Direction::Right);
    tree.split(panes[0], panes[2], Direction::Down);
    tree.split(panes[0], panes[1], Direction::Right);
    tree.split(panes[3], panes[6], Direction::Down);
    tree.split(panes[3], panes[5], Direction::Right);
    tree.split(panes[3], panes[4], Direction::Down);

    assert_eq!(
        tree.active_root().panes_by_direction(panes[0], Direction::Right),
        FindPaneByDirectionResult::Found(HashSet::from([panes[1]]))
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[1], Direction::Right),
        FindPaneByDirectionResult::Found(HashSet::from([panes[3], panes[4], panes[6]]))
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[5], Direction::Left),
        FindPaneByDirectionResult::Found(HashSet::from([panes[3], panes[4]]))
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[6], Direction::Up),
        FindPaneByDirectionResult::Found(HashSet::from([panes[4], panes[5]]))
    );
    assert_eq!(
        tree.active_root().panes_by_direction(panes[4], Direction::Down),
        FindPaneByDirectionResult::Found(HashSet::from([panes[6]]))
    );
}

#[test]
fn test_are_rects_overlapping_on_axis() {
    let rect1 = RectF::from_points(Vector2F::new(0.0, 0.0), Vector2F::new(10.0, 10.0));
    let rect2 = RectF::from_points(Vector2F::new(10.0, -5.0), Vector2F::new(20.0, 5.0));
    let rect3 = RectF::from_points(Vector2F::new(10.0, 10.0), Vector2F::new(20.0, 20.0));
    let rect4 = RectF::from_points(Vector2F::new(-5.0, 10.0), Vector2F::new(5.0, 20.0));
    let rect5 = RectF::from_points(Vector2F::new(30.0, 30.0), Vector2F::new(40.0, 40.0));
    let rect6 = RectF::from_points(Vector2F::new(-20.0, -20.0), Vector2F::new(-10.0, -10.0));

    assert!(PaneData::are_rects_overlapping(
        &rect1,
        &rect2,
        SplitDirection::Horizontal
    ));
    assert!(!PaneData::are_rects_overlapping(
        &rect1,
        &rect5,
        SplitDirection::Horizontal
    ));
    assert!(!PaneData::are_rects_overlapping(
        &rect1,
        &rect3,
        SplitDirection::Horizontal
    ));
    assert!(!PaneData::are_rects_overlapping(
        &rect1,
        &rect6,
        SplitDirection::Horizontal
    ));

    assert!(PaneData::are_rects_overlapping(
        &rect1,
        &rect4,
        SplitDirection::Vertical
    ));
    assert!(!PaneData::are_rects_overlapping(
        &rect1,
        &rect5,
        SplitDirection::Vertical
    ),);
    assert!(!PaneData::are_rects_overlapping(
        &rect1,
        &rect3,
        SplitDirection::Vertical
    ));
}

#[test]
fn test_hide_and_show_child_agent_pane() {
    let panes = [PaneId::dummy_pane_id(), PaneId::dummy_pane_id()];
    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Right);

    // Both panes visible initially.
    assert_eq!(tree.visible_pane_ids(), vec![panes[0], panes[1]]);
    assert!(!tree.is_pane_hidden(&panes[1]));

    // Hide the child agent pane.
    tree.hide_pane_for_child_agent(panes[1]);
    assert!(tree.is_pane_hidden(&panes[1]));
    assert_eq!(tree.visible_pane_ids(), vec![panes[0]]);
    // pane_ids still includes hidden panes (they remain in the tree).
    assert_eq!(tree.pane_ids(), vec![panes[0], panes[1]]);

    // Show the child agent pane.
    tree.show_pane_for_child_agent(panes[1]);
    assert!(!tree.is_pane_hidden(&panes[1]));
    assert_eq!(tree.visible_pane_ids(), vec![panes[0], panes[1]]);
}

#[test]
fn test_hide_child_agent_pane_is_idempotent() {
    let panes = [PaneId::dummy_pane_id(), PaneId::dummy_pane_id()];
    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Right);

    // Hiding the same pane twice should not create duplicate entries.
    tree.hide_pane_for_child_agent(panes[1]);
    tree.hide_pane_for_child_agent(panes[1]);
    assert_eq!(tree.num_hidden_panes(), 1);

    // A single show call should fully unhide it.
    tree.show_pane_for_child_agent(panes[1]);
    assert!(!tree.is_pane_hidden(&panes[1]));
    assert_eq!(tree.num_hidden_panes(), 0);
}

#[test]
fn test_original_pane_for_replacement() {
    let original = PaneId::dummy_pane_id();
    let replacement = PaneId::dummy_pane_id();
    let unrelated = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(original);
    tree.split(original, unrelated, Direction::Right);

    // No replacement yet.
    assert_eq!(tree.original_pane_for_replacement(original), None);
    assert_eq!(tree.original_pane_for_replacement(replacement), None);

    // Perform a temporary replacement.
    assert!(tree.replace_pane(original, replacement, true));
    assert_eq!(
        tree.original_pane_for_replacement(replacement),
        Some(original)
    );
    // The original itself is not a replacement.
    assert_eq!(tree.original_pane_for_replacement(original), None);
    // Unrelated pane is unaffected.
    assert_eq!(tree.original_pane_for_replacement(unrelated), None);

    // Revert — lookup should return None again.
    assert_eq!(
        tree.revert_temporary_replacement(replacement),
        Some(original)
    );
    assert_eq!(tree.original_pane_for_replacement(replacement), None);
}

#[test]
fn test_hide_multiple_child_agent_panes() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];
    let mut tree = PaneData::new(panes[0]);
    tree.split(panes[0], panes[1], Direction::Right);
    tree.split(panes[1], panes[2], Direction::Right);

    tree.hide_pane_for_child_agent(panes[1]);
    tree.hide_pane_for_child_agent(panes[2]);
    assert_eq!(tree.visible_pane_ids(), vec![panes[0]]);

    // Reveal only one child.
    tree.show_pane_for_child_agent(panes[1]);
    assert_eq!(tree.visible_pane_ids(), vec![panes[0], panes[1]]);
    assert!(tree.is_pane_hidden(&panes[2]));
}

// --- Terminal-tab layer -------------------------------------------------------

#[test]
fn test_new_pane_data_has_single_tab() {
    let pane = PaneId::dummy_pane_id();
    let tree = PaneData::new(pane);
    assert_eq!(tree.tab_count(), 1);
    assert_eq!(tree.active_tab_index(), 0);
    assert_eq!(tree.active_root().as_leaf(), Some(pane));
}

#[test]
fn test_add_tab_activates_new_tab_and_counts_panes() {
    let first = PaneId::dummy_pane_id();
    let second = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(first);

    let new_id = tree.add_tab(second);
    assert_eq!(tree.tab_count(), 2);
    // The new tab is active and its id is returned.
    assert_eq!(tree.active_tab_id(), new_id);
    assert_eq!(tree.active_tab_index(), 1);
    // Global pane enumeration sees both tabs' panes.
    assert_eq!(tree.pane_ids(), vec![first, second]);
    assert_eq!(tree.len(), 2);
    // Active-tab-only count reflects just the new tab.
    assert_eq!(tree.active_tab_visible_count(), 1);
}

#[test]
fn test_activate_tab_by_id_switches_active() {
    let first = PaneId::dummy_pane_id();
    let second = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(first);
    let first_tab = tree.active_tab_id();
    tree.add_tab(second); // now active

    assert!(tree.activate_tab(first_tab));
    assert_eq!(tree.active_tab_id(), first_tab);
    assert_eq!(tree.active_root().as_leaf(), Some(first));

    // Activating an unknown id is a no-op returning false.
    assert!(!tree.activate_tab(PaneTabId::new()));
    assert_eq!(tree.active_tab_id(), first_tab);
}

#[test]
fn test_remove_tab_returns_panes_and_picks_neighbour() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let c = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    let tab_a = tree.active_tab_id();
    let tab_b = tree.add_tab(b);
    let tab_c = tree.add_tab(c); // active

    // Remove the active (last) tab; active falls back to the previous tab.
    let (removed, new_active) = tree.remove_tab(tab_c);
    assert_eq!(removed, vec![c]);
    assert_eq!(tree.tab_count(), 2);
    assert_eq!(new_active, tab_b);
    assert_eq!(tree.active_tab_id(), tab_b);
    assert_eq!(tree.len(), 2);

    // Remove a tab before the active one; active index shifts to keep tracking it.
    let _ = tree.add_tab(c); // re-add a third tab (now active)
    tree.activate_tab(tab_b);
    let active_before = tree.active_tab_id();
    let (removed, _) = tree.remove_tab(tab_a);
    assert_eq!(removed, vec![a]);
    assert_eq!(tree.active_tab_id(), active_before);
}

#[test]
fn test_remove_last_tab_is_noop() {
    let a = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    let only = tree.active_tab_id();
    let (removed, active) = tree.remove_tab(only);
    assert!(removed.is_empty(), "closing the final tab must remove nothing");
    assert_eq!(active, only);
    assert_eq!(tree.tab_count(), 1);
}

#[test]
fn test_split_targets_correct_tab() {
    // A pane in a non-active tab can still be split (e.g. via a stored handle):
    // the split lands in the tab that owns the base pane, not the active tab.
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let split_into = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    let tab_a = tree.active_tab_id();
    tree.add_tab(b); // active is now tab b

    assert!(tree.split(a, split_into, Direction::Right));
    // Tab a now has two panes; tab b still one. Global len == 3.
    assert_eq!(tree.len(), 3);
    tree.activate_tab(tab_a);
    assert_eq!(tree.active_tab_visible_count(), 2);
}

#[test]
fn test_focus_target_prefers_last_focused_then_first() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    tree.split(a, b, Direction::Right); // active tab has [a, b]

    // No focus recorded yet → first pane.
    assert_eq!(tree.active_tab_focus_target(), Some(a));

    // Record focus on b → focus target follows.
    tree.record_focused_pane(b);
    assert_eq!(tree.active_tab_focus_target(), Some(b));

    // Remove b → stale last_focused falls back to the first remaining pane.
    tree.remove(b);
    assert_eq!(tree.active_tab_focus_target(), Some(a));
}

#[test]
fn test_set_tab_title_and_tab_lookup() {
    let a = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    let tab = tree.active_tab_id();

    assert!(tree.set_tab_title(tab, Some("Build".to_string())));
    assert_eq!(tree.tabs()[0].title.as_deref(), Some("Build"));
    assert_eq!(tree.tab_id_for_pane(a), Some(tab));
    assert_eq!(tree.tab_index_by_id(tab), Some(0));

    // Unknown tab id → false / None.
    assert!(!tree.set_tab_title(PaneTabId::new(), Some("x".into())));
    assert_eq!(tree.tab_id_for_pane(PaneId::dummy_pane_id()), None);
}

#[test]
fn test_from_tabs_constructor() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let tabs = vec![
        PaneTab::from_parts(PaneNode::leaf(a), PaneTabId::new(), Some("first".into())),
        PaneTab::from_parts(PaneNode::leaf(b), PaneTabId::new(), None),
    ];
    let tree = PaneData::from_tabs(tabs, 1);
    assert_eq!(tree.tab_count(), 2);
    assert_eq!(tree.active_tab_index(), 1);
    assert_eq!(tree.len(), 2);
    assert_eq!(tree.active_root().as_leaf(), Some(b));
    assert_eq!(tree.tabs()[0].title.as_deref(), Some("first"));
}

#[test]
fn test_from_tabs_clamps_out_of_range_active() {
    let a = PaneId::dummy_pane_id();
    let tabs = vec![PaneTab::new(PaneNode::leaf(a))];
    let tree = PaneData::from_tabs(tabs, 99);
    assert_eq!(tree.active_tab_index(), 0);
}

#[test]
fn test_reorder_tab_moves_and_makes_active() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let c = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    let tab_a = tree.active_tab_id();
    let tab_b = tree.add_tab(b);
    let tab_c = tree.add_tab(c); // [a, b, c], active = c

    // Move the first tab to the end; the moved tab becomes active.
    assert!(tree.reorder_tab(tab_a, 2));
    assert_eq!(
        tree.tabs().iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![tab_b, tab_c, tab_a]
    );
    assert_eq!(tree.active_tab_id(), tab_a);

    // Move it back to the front.
    assert!(tree.reorder_tab(tab_a, 0));
    assert_eq!(
        tree.tabs().iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![tab_a, tab_b, tab_c]
    );
    assert_eq!(tree.active_tab_id(), tab_a);

    // Out-of-range index clamps to the last slot.
    assert!(tree.reorder_tab(tab_a, 99));
    assert_eq!(
        tree.tabs().iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![tab_b, tab_c, tab_a]
    );

    // Same-slot move is a no-op-but-true and keeps order.
    assert!(tree.reorder_tab(tab_a, 2));
    assert_eq!(tree.active_tab_id(), tab_a);

    // Unknown id → false.
    assert!(!tree.reorder_tab(PaneTabId::new(), 0));
}

#[test]
fn test_take_tab_extracts_root_and_keeps_active_valid() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let c = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    let _tab_a = tree.active_tab_id();
    let tab_b = tree.add_tab(b);
    let tab_c = tree.add_tab(c); // active = c
    assert_eq!(tree.len(), 3);

    // Take the middle (non-active) tab; len drops, the extracted root carries b.
    tree.activate_tab(tab_c);
    let (root, panes) = tree.take_tab(tab_b).expect("non-last tab is extractable");
    assert_eq!(panes, vec![b]);
    assert_eq!(root.as_leaf(), Some(b));
    assert_eq!(tree.tab_count(), 2);
    assert_eq!(tree.len(), 2);
    assert_eq!(tree.active_tab_id(), tab_c, "active stays on c");

    // The last remaining-after pair: taking one leaves a single valid tab.
    let (_root, panes) = tree.take_tab(tree.active_tab_id()).expect("still > 1 tab");
    assert_eq!(panes, vec![c]);
    assert_eq!(tree.tab_count(), 1);

    // Now only one tab — take_tab refuses (never empty the container).
    let only = tree.active_tab_id();
    assert!(tree.take_tab(only).is_none());
    assert_eq!(tree.tab_count(), 1);
}

#[test]
fn test_tab_active_pane_prefers_last_focused() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    let tab = tree.active_tab_id();
    tree.split(a, b, Direction::Right); // tab has [a, b]

    // No recorded focus → first pane.
    assert_eq!(tree.tab_active_pane(tab), Some(a));

    // Record focus on b → returned.
    tree.record_focused_pane(b);
    assert_eq!(tree.tab_active_pane(tab), Some(b));

    // Unknown tab → None.
    assert_eq!(tree.tab_active_pane(PaneTabId::new()), None);
}

#[test]
fn test_move_pane_pops_group_member_out_to_adjacent_slot() {
    let a = PaneId::dummy_pane_id();
    let m = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    tree.split(a, b, Direction::Right); // A | B
    tree.split(a, m, Direction::Right); // A | M | B
    assert!(tree.merge_pane_into_leaf_group(m, a)); // [A,M] | B, M active

    // Drag tab M onto the ADJACENT pane B's left edge. M is B's left-hand
    // sibling identity (dragging activated it), but popping it out of the
    // group is a real mutation — the old "no tree change" guard ate this.
    assert!(
        tree.move_pane(m, b, Direction::Left),
        "adjacent edge drop must pop the member out, not no-op"
    );
    assert_eq!(tree.pane_ids(), vec![a, m, b]);

    // The still-correct no-op: a SOLO pane moved to the slot it already
    // occupies (A is immediately left of M).
    assert!(!tree.move_pane(a, m, Direction::Left));
    assert_eq!(tree.pane_ids(), vec![a, m, b]);
}

#[test]
fn test_extract_pane_to_new_tab_stacks_split_pane_as_tab() {
    // NOTE: with PaneLeafGroups the center drop no longer routes here (it merges
    // into a leaf group instead); `extract_pane_to_new_tab` remains as the
    // workspace-tab "park" path and its semantics are unchanged for split panes.
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    let tab_a = tree.active_tab_id();
    tree.split(a, b, Direction::Right); // one tab with split [a, b]
    assert_eq!(tree.tab_count(), 1);
    assert_eq!(tree.len(), 2);

    // Park pane b as a workspace tab → b becomes its own (active) tab.
    assert!(tree.extract_pane_to_new_tab(b));
    assert_eq!(tree.tab_count(), 2);
    assert_eq!(tree.len(), 2, "total pane count is unchanged");
    assert_ne!(tree.active_tab_id(), tab_a, "the new tab becomes active");
    assert_eq!(tree.active_root().as_leaf(), Some(b));
    assert_eq!(tree.tab_active_pane(tab_a), Some(a), "a stays in its tab");

    // b is now its tab's only pane — extracting again would empty the tab.
    assert!(!tree.extract_pane_to_new_tab(b));
    assert_eq!(tree.tab_count(), 2);

    // Unknown pane → refused.
    assert!(!tree.extract_pane_to_new_tab(PaneId::dummy_pane_id()));
}

// --- Leaf tab groups (per-pane tabs) --------------------------------------------

#[test]
fn test_merge_pane_into_leaf_group_happy_path() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    tree.split(a, b, Direction::Right); // split [a | b]

    // Center-drop b onto a → b becomes a stacked member of a's leaf group.
    assert!(tree.merge_pane_into_leaf_group(b, a));

    // The split collapsed into a single leaf group [a, b] with b active.
    let group = tree
        .active_root()
        .as_leaf_group()
        .expect("root should be a leaf group");
    assert_eq!(group.members(), &[a, b]);
    assert_eq!(group.active_pane(), b, "the merged pane becomes active");
    assert_eq!(tree.tab_count(), 1, "no terminal tab is created");
    assert_eq!(tree.len(), 2, "total pane count unchanged");
    assert_eq!(tree.pane_ids(), vec![a, b], "both members stay alive");
    assert_eq!(
        tree.visible_pane_ids(),
        vec![b],
        "only the active member is visible"
    );
    assert_eq!(tree.visible_pane_count(), 1);
    assert_eq!(
        tree.num_hidden_panes(),
        0,
        "inactive members are NOT hidden_panes entries"
    );
    // The whole group still reports membership for both panes.
    assert!(tree.contains_pane(a));
    assert!(tree.contains_pane(b));
}

#[test]
fn test_merge_keeps_split_siblings_untouched() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let c = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    tree.split(a, b, Direction::Right);
    tree.split(b, c, Direction::Right); // [a | b | c]

    // Merge c onto a → [group(a, c) | b]; sibling b keeps its frame.
    assert!(tree.merge_pane_into_leaf_group(c, a));
    let root = tree.active_root().as_branch().expect("root stays a branch");
    assert_eq!(root.axis(), SplitDirection::Horizontal);
    // The group's active member (c) fronts the first frame.
    assert_eq!(root.direct_children(), vec![c, b]);
    assert_eq!(tree.pane_ids(), vec![a, c, b]);
    assert_eq!(tree.visible_pane_ids(), vec![c, b]);
    assert_eq!(tree.len(), 3);
}

#[test]
fn test_merge_duplicate_member_guard() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    tree.split(a, b, Direction::Right);

    // Self-merge is refused.
    assert!(!tree.merge_pane_into_leaf_group(a, a));

    assert!(tree.merge_pane_into_leaf_group(b, a)); // group [a, b]

    // Already stacked together — both directions refused, tree unchanged.
    assert!(!tree.merge_pane_into_leaf_group(b, a));
    assert!(!tree.merge_pane_into_leaf_group(a, b));
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(group.members(), &[a, b]);
    assert_eq!(group.active_pane(), b);

    // Unknown source or target — refused.
    assert!(!tree.merge_pane_into_leaf_group(PaneId::dummy_pane_id(), a));
    assert!(!tree.merge_pane_into_leaf_group(a, PaneId::dummy_pane_id()));
}

#[test]
fn test_merge_refused_when_source_is_tabs_only_pane() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    tree.add_tab(b); // two terminal tabs: [a], [b]

    // a is its tab's only pane — removing it would empty the tab.
    assert!(!tree.merge_pane_into_leaf_group(a, b));
    assert_eq!(tree.tab_count(), 2);
    assert_eq!(tree.len(), 2);
}

/// Builds a single-tab tree whose root is the leaf group [a, b, c] with c active.
fn build_three_member_group() -> (PaneData, PaneId, PaneId, PaneId) {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let c = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    tree.split(a, b, Direction::Right);
    assert!(tree.merge_pane_into_leaf_group(b, a)); // group [a, b]
    tree.split(a, c, Direction::Right); // [group(a, b) | c]
    assert!(tree.merge_pane_into_leaf_group(c, a)); // group [a, b, c], active c
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(group.members(), &[a, b, c]);
    assert_eq!(group.active_pane(), c);
    (tree, a, b, c)
}

#[test]
fn test_member_close_activates_last_remaining() {
    let (mut tree, a, b, c) = build_three_member_group();

    // Close the ACTIVE member (c) → the last remaining member (b) activates.
    assert!(tree.remove(c));
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(group.members(), &[a, b]);
    assert_eq!(
        group.active_pane(),
        b,
        "TCB removeTab: last remaining member activates"
    );
    assert_eq!(tree.visible_pane_ids(), vec![b]);
    assert_eq!(tree.len(), 2);
}

#[test]
fn test_active_index_clamps_when_background_member_closes() {
    let (mut tree, a, b, c) = build_three_member_group();

    // Close a BACKGROUND member before the active one → active member unchanged.
    assert!(tree.remove(a));
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(group.members(), &[b, c]);
    assert_eq!(
        group.active_pane(),
        c,
        "closing a background member keeps the active one"
    );
    assert_eq!(group.active_index(), 1, "index shifted left with the removal");
    assert_eq!(tree.visible_pane_ids(), vec![c]);

    // Close the other background member too; the active member survives alone.
    assert!(tree.remove(b));
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(group.members(), &[c]);
    assert_eq!(group.active_index(), 0);
}

#[test]
fn test_last_member_out_collapses_parent_split() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let c = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    tree.split(a, c, Direction::Right); // [a | c]
    tree.split(a, b, Direction::Down); // [V(a, b) | c]
    assert!(tree.merge_pane_into_leaf_group(b, a)); // [group(a, b) | c]

    // Close b (a member) → the group keeps the frame with a alone.
    assert!(tree.remove(b));
    let root = tree.active_root().as_branch().expect("still a branch");
    assert_eq!(root.direct_children(), vec![a, c]);

    // Close a — the group's LAST member — → the leaf disappears and the split
    // collapses to c alone (existing Collapse path).
    assert!(tree.remove(a));
    assert_eq!(tree.active_root().as_leaf(), Some(c));
    assert_eq!(tree.pane_ids(), vec![c]);
    assert_eq!(tree.len(), 1);
}

#[test]
fn test_directional_nav_skips_inactive_members() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let c = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    tree.split(a, c, Direction::Right); // [a | c]
    tree.split(a, b, Direction::Down); // [V(a, b) | c]
    assert!(tree.merge_pane_into_leaf_group(b, a)); // [group(a, b) active=b | c]

    // From c looking left, only the group's ACTIVE member (b) is a target.
    assert_eq!(
        tree.active_root().panes_by_direction(c, Direction::Left),
        FindPaneByDirectionResult::Found(HashSet::from([b]))
    );
    // From the active member, right → c.
    assert_eq!(
        tree.active_root().panes_by_direction(b, Direction::Right),
        FindPaneByDirectionResult::Found(HashSet::from([c]))
    );
    // The INACTIVE member a is not part of the visible layout at all.
    assert_eq!(
        tree.active_root().panes_by_direction(a, Direction::Right),
        FindPaneByDirectionResult::NotFound
    );

    // Sibling resolution also works on active members only.
    assert_eq!(tree.sibling_by_direction(c, Direction::Left), Some(b));
    assert_eq!(tree.sibling_by_direction(b, Direction::Right), Some(c));
    assert_eq!(tree.sibling_by_direction(a, Direction::Right), None);
}

#[test]
fn test_extract_pane_to_new_tab_removes_member_not_leaf() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    tree.split(a, b, Direction::Right);
    assert!(tree.merge_pane_into_leaf_group(b, a)); // root = group [a, b]

    // Parking a group MEMBER as a workspace tab removes the member, not the leaf.
    assert!(tree.extract_pane_to_new_tab(b));
    assert_eq!(tree.tab_count(), 2);
    let first_root = &tree.tabs()[0].root;
    let group = first_root.as_leaf_group().expect("group frame survives");
    assert_eq!(group.members(), &[a]);
    assert_eq!(group.active_pane(), a);
    // The new tab holds b and is active.
    assert_eq!(tree.active_root().as_leaf(), Some(b));
    assert_eq!(tree.len(), 2);
}

#[test]
fn test_focus_target_skips_inactive_members() {
    let a = PaneId::dummy_pane_id();
    let b = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(a);
    tree.split(a, b, Direction::Right);
    tree.record_focused_pane(a);
    assert!(tree.merge_pane_into_leaf_group(b, a)); // group [a, b], active b

    // a was last focused but is now an inactive member — never a focus target.
    assert_eq!(tree.active_tab_focus_target(), Some(b));
    assert_eq!(tree.tab_active_pane(tree.active_tab_id()), Some(b));
}

#[test]
fn test_activate_leaf_group_member_switches_active() {
    let (mut tree, a, b, c) = build_three_member_group();

    // Activate the inactive member a → it becomes the visible member.
    assert!(tree.activate_leaf_group_member(a));
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(group.active_pane(), a);
    assert_eq!(tree.visible_pane_ids(), vec![a]);

    // Activating the already-active member is a successful no-op.
    assert!(tree.activate_leaf_group_member(a));
    assert_eq!(tree.visible_pane_ids(), vec![a]);

    // Every member remains alive; b and c are just invisible.
    assert_eq!(tree.pane_ids(), vec![a, b, c]);

    // Unknown pane → false, no change.
    assert!(!tree.activate_leaf_group_member(PaneId::dummy_pane_id()));
    assert_eq!(tree.visible_pane_ids(), vec![a]);
}

#[test]
fn test_hide_closed_active_member_hands_activation_to_survivor() {
    let (mut tree, a, b, c) = build_three_member_group(); // active c

    // Undo-close path: the active member is HIDDEN (not removed). Activation
    // must move to the last remaining non-hidden member so the group's frame
    // keeps rendering.
    tree.hide_closed_pane(c);
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(
        group.active_pane(),
        b,
        "activation hands off to the last non-hidden member"
    );
    assert_eq!(group.members(), &[a, b, c], "membership is untouched");
    assert_eq!(tree.visible_pane_ids(), vec![b]);

    // Undoing the close restores c as the active (visible) member.
    assert!(tree.unhide_closed_pane(c));
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(group.active_pane(), c);
    assert_eq!(tree.visible_pane_ids(), vec![c]);

    // Hiding a BACKGROUND member leaves the active member alone.
    tree.hide_closed_pane(a);
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(group.active_pane(), c);
    assert_eq!(tree.visible_pane_ids(), vec![c]);
}

#[test]
fn test_hide_for_job_active_member_hands_activation_to_survivor() {
    let (mut tree, a, b, c) = build_three_member_group(); // active c

    // Hiding the ACTIVE member for a job must hand activation to a surviving
    // member (same rule as `hide_closed_pane`), or the whole group frame would
    // stop rendering.
    tree.hide_pane_for_job(c);
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(
        group.active_pane(),
        b,
        "activation hands off to the last non-hidden member"
    );
    assert_eq!(group.members(), &[a, b, c], "membership is untouched");
    assert_eq!(tree.visible_pane_ids(), vec![b]);

    // Revealing the pane again makes it the active (visible) member.
    tree.show_pane_for_job(c);
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(group.active_pane(), c);
    assert_eq!(tree.visible_pane_ids(), vec![c]);
}

#[test]
fn test_non_hidden_member_count_drives_strip_presence() {
    let (mut tree, _a, b, c) = build_three_member_group();

    // All three visible → strip present (count >= 2), so `pane_size` adds the
    // strip-row height.
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(group.non_hidden_member_count(&[]), 3);

    // Hiding two members leaves a single visible member: no strip, so no extra
    // height (this is the undo-grace-period case the fix guards against).
    tree.hide_closed_pane(c);
    tree.hide_closed_pane(b);
    let hidden = tree.hidden_panes.clone();
    let group = tree.active_root().as_leaf_group().expect("leaf group root");
    assert_eq!(group.non_hidden_member_count(&hidden), 1);
}

#[test]
fn test_leaf_group_membership_queries() {
    let (mut tree, a, b, c) = build_three_member_group();

    // Every member (active or not) resolves to the same member list.
    assert_eq!(tree.leaf_group_members(a), Some([a, b, c].as_slice()));
    assert_eq!(tree.leaf_group_members(c), Some([a, b, c].as_slice()));
    assert_eq!(tree.leaf_group_members(PaneId::dummy_pane_id()), None);

    // All three belong to a multi-member group.
    assert_eq!(tree.leaf_group_member_ids(), vec![a, b, c]);
    let set = tree.panes_in_multi_member_groups();
    assert!(set.contains(&a) && set.contains(&b) && set.contains(&c));

    // Hiding members shrinks the ×-predicate set: with only one NON-HIDDEN
    // member left, nobody in the group qualifies any more.
    tree.hide_closed_pane(c);
    tree.hide_closed_pane(b);
    assert!(tree.panes_in_multi_member_groups().is_empty());
    // (State reconciliation still sees the full membership.)
    assert_eq!(tree.leaf_group_member_ids(), vec![a, b, c]);
}
