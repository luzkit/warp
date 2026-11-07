use warpui::EntityId;

use super::{LayoutChange, LayoutHistory, LayoutState, MAX_LAYOUT_CHANGES};

fn change() -> LayoutChange {
    LayoutChange {
        before: LayoutState {
            tabs: Vec::new(),
            layouts: Default::default(),
            active_tab: EntityId::new(),
        },
        after: LayoutState {
            tabs: Vec::new(),
            layouts: Default::default(),
            active_tab: EntityId::new(),
        },
    }
}

#[test]
fn docking_layout_history_evicts_the_oldest_record() {
    let mut history = LayoutHistory::default();
    let oldest = change();
    let oldest_id = oldest.after.active_tab;
    history.record(oldest, None);

    for _ in 0..MAX_LAYOUT_CHANGES {
        history.record(change(), None);
    }

    assert_eq!(history.undo.len(), MAX_LAYOUT_CHANGES);
    assert!(
        history
            .undo
            .iter()
            .all(|change| change.after.active_tab != oldest_id)
    );
}

#[test]
fn docking_layout_history_new_record_clears_redo() {
    let mut history = LayoutHistory::default();
    history.redo.push(change());
    history.record(change(), None);

    assert!(history.redo.is_empty());
    assert_eq!(history.undo.len(), 1);
}
