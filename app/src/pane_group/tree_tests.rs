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
        tree.root.as_branch().expect("Should be a branch").axis(),
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

    let root = tree.root.as_branch().expect("Should be a branch");
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
    let root = tree.root.as_branch().expect("Should be a branch");
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
    let root = tree.root.as_branch().expect("Should be a branch");
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

    let root = tree.root.as_branch().expect("Should be a branch");
    assert_eq!(root.axis(), SplitDirection::Horizontal);
    assert_eq!(root.direct_children(), vec![panes[1], panes[0]]);

    // Move pane 0 on top of pane 1, which should result in
    // --------------
    // |     0      |
    // -------------
    // |     1      |
    // -------------
    tree.move_pane(panes[0], panes[1], Direction::Up);

    let root = tree.root.as_branch().expect("Should be a branch");
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
    let root = tree.root.as_branch().expect("Should be a branch");
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
    let root = tree.root.as_branch().expect("Should be a branch");
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
    let root = tree.root.as_branch().expect("Should be a branch");
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
    let root = tree.root.as_branch().expect("Should be a branch");
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
    let root = tree.root.as_branch().expect("Should be a branch");
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
        tree.root.panes_by_direction(panes[0], Direction::Right),
        FindPaneByDirectionResult::Found(HashSet::from([panes[1]]))
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[0], Direction::Left),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[1], Direction::Right),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[1], Direction::Left),
        FindPaneByDirectionResult::Found(HashSet::from([panes[0]]))
    );

    assert_eq!(
        tree.root.panes_by_direction(panes[0], Direction::Up),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[0], Direction::Down),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[1], Direction::Up),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[1], Direction::Down),
        FindPaneByDirectionResult::Located
    );

    assert_eq!(
        tree.root.panes_by_direction(panes[2], Direction::Right),
        FindPaneByDirectionResult::NotFound
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[2], Direction::Left),
        FindPaneByDirectionResult::NotFound
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[2], Direction::Up),
        FindPaneByDirectionResult::NotFound
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[2], Direction::Down),
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
        tree.root.panes_by_direction(panes[0], Direction::Right),
        FindPaneByDirectionResult::Found(HashSet::from([panes[1], panes[3]]))
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[0], Direction::Left),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[0], Direction::Up),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[0], Direction::Down),
        FindPaneByDirectionResult::Found(HashSet::from([panes[2]]))
    );

    assert_eq!(
        tree.root.panes_by_direction(panes[1], Direction::Right),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[1], Direction::Left),
        FindPaneByDirectionResult::Found(HashSet::from([panes[0], panes[2]]))
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[1], Direction::Up),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[1], Direction::Down),
        FindPaneByDirectionResult::Found(HashSet::from([panes[3]]))
    );

    assert_eq!(
        tree.root.panes_by_direction(panes[2], Direction::Right),
        FindPaneByDirectionResult::Found(HashSet::from([panes[1], panes[3]]))
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[2], Direction::Left),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[2], Direction::Up),
        FindPaneByDirectionResult::Found(HashSet::from([panes[0]]))
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[2], Direction::Down),
        FindPaneByDirectionResult::Located
    );

    assert_eq!(
        tree.root.panes_by_direction(panes[3], Direction::Right),
        FindPaneByDirectionResult::Located
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[3], Direction::Left),
        FindPaneByDirectionResult::Found(HashSet::from([panes[0], panes[2]]))
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[3], Direction::Up),
        FindPaneByDirectionResult::Found(HashSet::from([panes[1]]))
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[3], Direction::Down),
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
        tree.root.panes_by_direction(panes[0], Direction::Right),
        FindPaneByDirectionResult::Found(HashSet::from([panes[1]]))
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[1], Direction::Right),
        FindPaneByDirectionResult::Found(HashSet::from([panes[3], panes[4], panes[6]]))
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[5], Direction::Left),
        FindPaneByDirectionResult::Found(HashSet::from([panes[3], panes[4]]))
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[6], Direction::Up),
        FindPaneByDirectionResult::Found(HashSet::from([panes[4], panes[5]]))
    );
    assert_eq!(
        tree.root.panes_by_direction(panes[4], Direction::Down),
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
fn test_reset_pane_sizes_resets_containing_branch() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];
    let mut tree = PaneData::new(panes[0]);

    tree.split(panes[0], panes[1], Direction::Right);
    tree.split(panes[1], panes[2], Direction::Right);

    let root = tree.root.as_branch().expect("Should be a branch");
    let border_id = root.dividers[0].id;

    let root = match &mut tree.root {
        PaneNode::Branch(root) => root,
        PaneNode::Leaf(_) => panic!("Should be a branch"),
    };
    root.nodes[0].0 = PaneFlex(0.2);
    root.nodes[1].0 = PaneFlex(0.5);
    root.nodes[2].0 = PaneFlex(0.3);

    assert!(tree.reset_pane_sizes(border_id));

    let root = tree.root.as_branch().expect("Should be a branch");
    assert_eq!(
        root.nodes
            .iter()
            .map(|(flex, _)| flex.0)
            .collect::<Vec<_>>(),
        vec![DEFAULT_FLEX_VALUE, DEFAULT_FLEX_VALUE, DEFAULT_FLEX_VALUE]
    );
}

#[test]
fn test_reset_pane_sizes_only_resets_containing_branch() {
    let panes = [
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
        PaneId::dummy_pane_id(),
    ];
    let mut tree = PaneData::new(panes[0]);

    tree.split(panes[0], panes[1], Direction::Down);
    tree.split(panes[1], panes[2], Direction::Right);

    let root = match &mut tree.root {
        PaneNode::Branch(root) => root,
        PaneNode::Leaf(_) => panic!("Should be a branch"),
    };
    root.nodes[0].0 = PaneFlex(0.25);
    root.nodes[1].0 = PaneFlex(0.75);

    let nested = match &mut root.nodes[1].1 {
        PaneNode::Branch(nested) => nested,
        PaneNode::Leaf(_) => panic!("Should be a branch"),
    };
    nested.nodes[0].0 = PaneFlex(0.8);
    nested.nodes[1].0 = PaneFlex(0.2);
    let nested_border_id = nested.dividers[0].id;

    assert!(tree.reset_pane_sizes(nested_border_id));

    let root = tree.root.as_branch().expect("Should be a branch");
    assert_eq!(
        root.nodes
            .iter()
            .map(|(flex, _)| flex.0)
            .collect::<Vec<_>>(),
        vec![0.25, 0.75]
    );
    let nested = root.node(1).as_branch().expect("Should be a branch");
    assert_eq!(
        nested
            .nodes
            .iter()
            .map(|(flex, _)| flex.0)
            .collect::<Vec<_>>(),
        vec![DEFAULT_FLEX_VALUE, DEFAULT_FLEX_VALUE]
    );
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

#[test]
fn docking_subtree_keeps_destination_flex_and_incoming_layout() {
    let target = PaneId::dummy_pane_id();
    let neighbor = PaneId::dummy_pane_id();
    let incoming_top = PaneId::dummy_pane_id();
    let incoming_bottom = PaneId::dummy_pane_id();

    let mut destination = PaneData::new_branch(
        SplitDirection::Horizontal,
        vec![
            (PaneFlex(3.), PaneNode::Leaf(target)),
            (PaneFlex(1.), PaneNode::Leaf(neighbor)),
        ],
        2,
    );
    let incoming = PaneData::new_branch(
        SplitDirection::Vertical,
        vec![
            (PaneFlex(2.), PaneNode::Leaf(incoming_top)),
            (PaneFlex(5.), PaneNode::Leaf(incoming_bottom)),
        ],
        2,
    );
    let existing_divider = destination.root.as_branch().unwrap().dividers[0].id;
    let incoming_divider = incoming.root.as_branch().unwrap().dividers[0].id;

    destination
        .insert_docked_subtree(target, incoming, Direction::Left, false)
        .unwrap();

    assert_eq!(destination.len(), 4);
    assert_eq!(
        destination.pane_ids(),
        [incoming_top, incoming_bottom, target, neighbor]
    );
    let root = destination.root.as_branch().unwrap();
    assert_eq!(root.axis(), SplitDirection::Horizontal);
    assert_eq!(root.dividers[0].id, existing_divider);
    assert_eq!(root.nodes.len(), 2);
    assert_eq!(root.nodes[0].0.0, 3.);
    assert_eq!(root.nodes[1].0.0, 1.);
    let dock = root.node(0).as_branch().unwrap();
    assert_eq!(dock.axis(), SplitDirection::Horizontal);
    assert_eq!(dock.nodes[0].0.0, 1.);
    assert_eq!(dock.nodes[1].0.0, 1.);
    let moved_branch = dock.node(0).as_branch().unwrap();
    assert_eq!(moved_branch.axis(), SplitDirection::Vertical);
    assert_eq!(moved_branch.dividers[0].id, incoming_divider);
    assert_eq!(moved_branch.nodes[0].0.0, 2.);
    assert_eq!(moved_branch.nodes[1].0.0, 5.);
}

#[test]
fn docking_subtree_rejects_an_overlapping_pane_id() {
    let destination_id = PaneId::dummy_pane_id();
    let unique_id = PaneId::dummy_pane_id();
    let mut destination = PaneData::new(destination_id);
    let mut incoming = PaneData::new(unique_id);
    assert!(incoming.split(unique_id, destination_id, Direction::Down));

    let returned = destination
        .insert_docked_subtree(destination_id, incoming, Direction::Right, false)
        .unwrap_err();

    assert_eq!(destination.pane_ids(), [destination_id]);
    assert_eq!(destination.len(), 1);
    assert_eq!(returned.pane_ids(), [unique_id, destination_id]);
    assert_eq!(returned.len(), 2);
}

#[test]
fn docking_subtree_rejects_a_hidden_target() {
    let target = PaneId::dummy_pane_id();
    let incoming_id = PaneId::dummy_pane_id();
    let mut destination = PaneData::new(target);
    destination.hide_pane_for_job(target);

    let returned = destination
        .insert_docked_subtree(target, PaneData::new(incoming_id), Direction::Left, false)
        .unwrap_err();

    assert_eq!(destination.pane_ids(), [target]);
    assert!(destination.visible_pane_ids().is_empty());
    assert_eq!(returned.pane_ids(), [incoming_id]);
}

#[test]
fn swapping_nested_leaves_keeps_layout_slots_and_flex() {
    let first = PaneId::dummy_pane_id();
    let sibling = PaneId::dummy_pane_id();
    let second = PaneId::dummy_pane_id();
    let mut tree = PaneData::new_branch(
        SplitDirection::Horizontal,
        vec![
            (
                PaneFlex(4.),
                PaneNode::Branch(PaneBranch::for_leaves(first, sibling, Direction::Down)),
            ),
            (PaneFlex(1.), PaneNode::Leaf(second)),
        ],
        3,
    );

    assert!(tree.swap_panes(first, second));

    assert_eq!(tree.pane_ids(), [second, sibling, first]);
    assert_eq!(tree.len(), 3);
    let root = tree.root.as_branch().unwrap();
    assert_eq!(root.nodes[0].0.0, 4.);
    assert_eq!(root.nodes[1].0.0, 1.);
    let nested = root.node(0).as_branch().unwrap();
    assert_eq!(nested.axis(), SplitDirection::Vertical);
    assert_eq!(nested.direct_children(), [second, sibling]);
}

#[test]
fn swapping_with_a_hidden_pane_keeps_the_existing_order() {
    let first = PaneId::dummy_pane_id();
    let second = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(first);
    assert!(tree.split(first, second, Direction::Right));
    tree.hide_pane_for_job(second);

    assert!(!tree.swap_panes(first, second));

    assert_eq!(tree.pane_ids(), [first, second]);
    assert_eq!(tree.visible_pane_ids(), [first]);
}

#[test]
fn flattening_a_docked_subtree_uses_equal_flex_in_visual_order() {
    let target = PaneId::dummy_pane_id();
    let top = PaneId::dummy_pane_id();
    let bottom_left = PaneId::dummy_pane_id();
    let bottom_right = PaneId::dummy_pane_id();
    let mut destination = PaneData::new(target);
    let mut incoming = PaneData::new(top);
    assert!(incoming.split(top, bottom_left, Direction::Down));
    assert!(incoming.split(bottom_left, bottom_right, Direction::Right));

    destination
        .insert_docked_subtree(target, incoming, Direction::Right, true)
        .unwrap();

    assert_eq!(destination.len(), 4);
    let root = destination.root.as_branch().unwrap();
    assert_eq!(root.node(0).as_leaf(), Some(target));
    let moved = root.node(1).as_branch().unwrap();
    assert_eq!(moved.axis(), SplitDirection::Horizontal);
    assert_eq!(moved.direct_children(), [top, bottom_left, bottom_right]);
    assert_eq!(
        moved
            .nodes
            .iter()
            .map(|(flex, _)| flex.0)
            .collect::<Vec<_>>(),
        [1., 1., 1.]
    );
}

#[test]
fn docking_fit_uses_flex_and_reserves_divider_space() {
    let _minimalist = FeatureFlag::MinimalistUI.override_enabled(false);
    let narrow = PaneId::dummy_pane_id();
    let wide = PaneId::dummy_pane_id();
    let tree = PaneData::new_branch(
        SplitDirection::Horizontal,
        vec![
            (PaneFlex(1.), PaneNode::Leaf(narrow)),
            (PaneFlex(3.), PaneNode::Leaf(wide)),
        ],
        2,
    );

    assert!(!tree.docked_subtree_fits(Vector2F::new(200., 75.), 50., None));
    assert!(tree.docked_subtree_fits(Vector2F::new(201., 75.), 50., None));
}

#[test]
fn flattened_docking_fit_uses_the_new_split_axis() {
    let _minimalist = FeatureFlag::MinimalistUI.override_enabled(false);
    let top = PaneId::dummy_pane_id();
    let bottom = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(top);
    assert!(tree.split(top, bottom, Direction::Down));

    assert!(!tree.docked_subtree_fits(Vector2F::new(101., 50.), 50., None));
    assert!(tree.docked_subtree_fits(Vector2F::new(101., 50.), 50., Some(Direction::Right)));
}

#[test]
fn moving_a_pane_into_a_nested_split_preserves_neighbor_slots() {
    let target = PaneId::dummy_pane_id();
    let neighbor = PaneId::dummy_pane_id();
    let moved = PaneId::dummy_pane_id();
    let mut tree = PaneData::new_branch(
        SplitDirection::Horizontal,
        vec![
            (PaneFlex(3.), PaneNode::Leaf(target)),
            (PaneFlex(2.), PaneNode::Leaf(neighbor)),
            (PaneFlex(1.), PaneNode::Leaf(moved)),
        ],
        3,
    );

    assert!(tree.move_pane_to_split(moved, target, Direction::Left));

    assert_eq!(tree.len(), 3);
    assert_eq!(tree.pane_ids(), [moved, target, neighbor]);
    let root = tree.root.as_branch().unwrap();
    assert_eq!(root.nodes[0].0.0, 3.);
    assert_eq!(root.nodes[1].0.0, 2.);
    let dock = root.node(0).as_branch().unwrap();
    assert_eq!(dock.axis(), SplitDirection::Horizontal);
    assert_eq!(dock.direct_children(), [moved, target]);
}

#[test]
fn moving_a_pane_to_a_missing_target_leaves_the_tree_intact() {
    let moved = PaneId::dummy_pane_id();
    let sibling = PaneId::dummy_pane_id();
    let missing = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(moved);
    assert!(tree.split(moved, sibling, Direction::Right));

    assert!(!tree.move_pane_to_split(moved, missing, Direction::Left));

    assert_eq!(tree.pane_ids(), [moved, sibling]);
    assert_eq!(tree.len(), 2);
}

#[test]
fn docking_preview_uses_nested_flex_and_window_coordinates() {
    let top_left = PaneId::dummy_pane_id();
    let bottom_left = PaneId::dummy_pane_id();
    let right = PaneId::dummy_pane_id();
    let mut tree = PaneData::new(top_left);
    assert!(tree.split(top_left, right, Direction::Right));
    assert!(tree.split(top_left, bottom_left, Direction::Down));
    let PaneNode::Branch(root) = &mut tree.root else {
        panic!("split layout must have a branch at the root");
    };
    root.nodes[1].0 = PaneFlex(2.);

    let mut panes = Vec::new();
    tree.root.collect_docking_rects(
        RectF::new(Vector2F::new(10., 20.), Vector2F::new(301., 201.)),
        1.,
        &mut panes,
    );

    assert_eq!(
        panes,
        vec![
            (
                top_left,
                RectF::new(Vector2F::new(10., 20.), Vector2F::new(100., 100.))
            ),
            (
                bottom_left,
                RectF::new(Vector2F::new(10., 121.), Vector2F::new(100., 100.))
            ),
            (
                right,
                RectF::new(Vector2F::new(111., 20.), Vector2F::new(200., 201.))
            ),
        ],
    );
}
