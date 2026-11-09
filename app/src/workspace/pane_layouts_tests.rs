use super::*;

const ROOMY: LayoutSpace = LayoutSpace {
    width: 600.,
    height: 500.,
    minimum: 60.,
    divider: 1.,
};
const TIGHT: LayoutSpace = LayoutSpace {
    width: 115.,
    height: 100.,
    minimum: 60.,
    divider: 1.,
};

fn id(number: u128) -> Uuid {
    Uuid::from_u128(number)
}

fn pane<T>(session: T) -> LayoutNode<T> {
    LayoutNode::Pane { session }
}

fn tab<T>(root: LayoutNode<T>) -> LayoutTab<T> {
    LayoutTab { name: None, root }
}

fn split<T>(
    axis: LayoutAxis,
    children: impl IntoIterator<Item = (f32, LayoutNode<T>)>,
) -> LayoutNode<T> {
    LayoutNode::Split {
        axis,
        children: children
            .into_iter()
            .map(|(weight, node)| LayoutChild { weight, node })
            .collect(),
    }
}

fn template(root: LayoutNode<Option<Uuid>>) -> LayoutDocument {
    LayoutDocument::new(
        "My layout".to_owned(),
        LayoutKind::Template,
        vec![tab(root)],
    )
    .unwrap()
}

#[test]
fn portable_export_clears_snapshot_session_ids_without_losing_structure() {
    let snapshot = LayoutDocument::new(
        "Morning".to_owned(),
        LayoutKind::Snapshot,
        vec![tab(split(
            LayoutAxis::Horizontal,
            [
                (2., pane(Some(id(1)))),
                (
                    1.,
                    LayoutNode::Stack {
                        sessions: vec![Some(id(2)), Some(id(3))],
                    },
                ),
            ],
        ))],
    )
    .unwrap();
    let exported = LayoutDocument::from_json(&snapshot.portable_json().unwrap()).unwrap();
    assert_eq!(exported.kind, LayoutKind::Template);
    assert_eq!(
        exported.tabs,
        vec![tab(split(
            LayoutAxis::Horizontal,
            [
                (2., pane(None)),
                (
                    1.,
                    LayoutNode::Stack {
                        sessions: vec![None, None]
                    }
                )
            ],
        ))]
    );
    assert_eq!(snapshot.kind, LayoutKind::Snapshot);
}

#[test]
fn json_rejects_unsupported_versions_and_non_layout_fields() {
    let unknown_version =
        r#"{"version":2,"name":"Layout","kind":"template","tabs":[{"root":{"type":"pane"}}]}"#;
    let shell_command = r#"{"version":1,"name":"Layout","kind":"template","tabs":[{"root":{"type":"pane","command":"echo should not be accepted"}}]}"#;
    assert!(LayoutDocument::from_json(unknown_version).is_err());
    assert!(LayoutDocument::from_json(shell_command).is_err());
}

#[test]
fn validation_rejects_bad_proportions_and_duplicate_bound_sessions() {
    let invalid = LayoutDocument::new(
        "Invalid".to_owned(),
        LayoutKind::Template,
        vec![tab(split(
            LayoutAxis::Horizontal,
            [(f32::NAN, pane(None)), (1., pane(None))],
        ))],
    );
    let duplicate = LayoutDocument::new(
        "Duplicate".to_owned(),
        LayoutKind::Snapshot,
        vec![tab(LayoutNode::Stack {
            sessions: vec![Some(id(1)), Some(id(1))],
        })],
    );
    assert!(invalid.is_err());
    assert!(duplicate.is_err());
}

#[test]
fn snapshot_skips_missing_ids_and_reserves_present_ids_for_their_own_slots() {
    let snapshot = LayoutDocument::new(
        "Saved".to_owned(),
        LayoutKind::Snapshot,
        vec![tab(split(
            LayoutAxis::Horizontal,
            [
                (1., pane(None)),
                (1., pane(Some(id(2)))),
                (1., pane(Some(id(3)))),
            ],
        ))],
    )
    .unwrap();
    let plan = plan_layout(
        &snapshot,
        &[id(2), id(4), id(5)],
        ROOMY,
        LayoutStrategy::Preserve,
    )
    .unwrap();

    assert_eq!(plan.missing_slots, 1);
    assert_eq!(plan.overflow_sessions, 1);
    assert_eq!(
        plan.tabs,
        vec![
            tab(split(
                LayoutAxis::Horizontal,
                [(1., pane(id(4))), (1., pane(id(2)))]
            )),
            tab(pane(id(5))),
        ]
    );
}

#[test]
fn snapshot_with_no_live_matches_does_not_rearrange_unrelated_panes() {
    let snapshot = LayoutDocument::new(
        "Saved".to_owned(),
        LayoutKind::Snapshot,
        vec![tab(pane(Some(id(1))))],
    )
    .unwrap();
    assert!(plan_layout(&snapshot, &[id(2)], ROOMY, LayoutStrategy::Preserve).is_err());
}

#[test]
fn cramped_tabs_offer_non_destructive_stack_and_tab_alternatives() {
    let document = template(split(
        LayoutAxis::Horizontal,
        [(1., pane(None)), (1., pane(None))],
    ));
    let sessions = [id(1), id(2), id(3)];
    assert!(plan_layout(&document, &sessions, TIGHT, LayoutStrategy::Preserve).is_err());
    assert!(plan_layout(&document, &sessions, TIGHT, LayoutStrategy::Balance).is_err());

    let stack = plan_layout(&document, &sessions, TIGHT, LayoutStrategy::Stack).unwrap();
    assert_eq!(
        stack.tabs,
        vec![
            tab(LayoutNode::Stack {
                sessions: vec![id(1), id(2)]
            }),
            tab(pane(id(3)))
        ]
    );
    assert_eq!(stack.overflow_sessions, 1);
    let preview = stack.preview_panes(0, TIGHT);
    assert_eq!(preview[0].session, id(1));
    assert_eq!(preview[0].stack_size, 2);

    let tabs = plan_layout(&document, &sessions, TIGHT, LayoutStrategy::Tabs).unwrap();
    assert_eq!(
        tabs.tabs,
        vec![tab(pane(id(1))), tab(pane(id(2))), tab(pane(id(3)))]
    );
}

#[test]
fn balancing_uses_space_needed_by_nested_splits_in_the_preview() {
    let document = template(split(
        LayoutAxis::Horizontal,
        [
            (
                1.,
                split(LayoutAxis::Horizontal, [(1., pane(None)), (1., pane(None))]),
            ),
            (1., pane(None)),
        ],
    ));
    let space = LayoutSpace {
        width: 192.,
        ..ROOMY
    };
    let sessions = [id(1), id(2), id(3)];
    assert!(plan_layout(&document, &sessions, space, LayoutStrategy::Preserve).is_err());
    let plan = plan_layout(&document, &sessions, space, LayoutStrategy::Balance).unwrap();

    let panes = plan.preview_panes(0, space);
    assert_eq!(panes.len(), 3);
    assert_eq!(
        panes
            .iter()
            .map(|pane| pane.bounds.width())
            .collect::<Vec<_>>(),
        vec![60., 60., 70.]
    );
    assert_eq!(panes[2].bounds.max_x(), 192.);
}

#[test]
fn catalog_round_trips_layouts_and_rejects_duplicate_names_without_rewriting() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.json");
    let document = template(pane(None));
    LayoutCatalog::update(&path, |catalog| catalog.add_document(document.clone())).unwrap();
    LayoutCatalog::update(&path, |catalog| {
        catalog.bind_display("monitor".to_owned(), "my layout")
    })
    .unwrap();
    let previous_bytes = fs::read(&path).unwrap();

    let duplicate = LayoutDocument::new(
        "my layout".to_owned(),
        LayoutKind::Template,
        vec![tab(pane(None))],
    )
    .unwrap();
    assert!(LayoutCatalog::update(&path, |catalog| catalog.add_document(duplicate)).is_err());
    assert_eq!(fs::read(&path).unwrap(), previous_bytes);

    let catalog = LayoutCatalog::load_from_path(&path).unwrap();
    assert_eq!(catalog.documents(), std::slice::from_ref(&document));
    assert_eq!(catalog.display_profile("monitor"), Some(&document));
}
