use warp::integration_testing::command_palette::{
    TestStepsExt, open_command_palette_and_run_action,
};
use warp::integration_testing::pane_group::{assert_focused_pane_index, assert_num_panes_in_tab};
use warp::integration_testing::view_getters::pane_group_view;
use warp::integration_testing::window::save_active_window_id;
use warpui_core::integration::{AssertionOutcome, TestStep};
use warpui_core::{WindowId, async_assert, async_assert_eq};

use super::history::UNDO_LAYOUT;
use super::{
    PROCESS_SNAPSHOT, TerminalProcesses, assert_processes_preserved, record_processes,
    saved_bounds, split_terminal_builder, terminal_processes, with_split_commands,
};
use crate::Builder;

const SEARCH: &str = "pane_navigator:search";
const PREVIEW: &str = "pane_navigator:preview";
const SOURCE_WINDOW: &str = "keyboard_single:source_window";
const ORIGINAL: &[(usize, usize)] = &[(0, 0), (0, 1)];
const SWAPPED: &[(usize, usize)] = &[(0, 1), (0, 0)];

fn open_keyboard_docking() -> Vec<TestStep> {
    open_command_palette_and_run_action("Keyboard Docking")
        .add_named_assertion("The destination search is visible", |app, window_id| {
            async_assert!(saved_bounds(app, window_id, SEARCH).is_some())
        })
}

fn choose_center_placement(placement: &'static str) -> Vec<TestStep> {
    let mut steps = open_keyboard_docking();
    steps.push(
        TestStep::new("Find the other pane in the same tab")
            .with_typed_characters(&["Tab 1"])
            .with_keystrokes(&["enter"]),
    );
    steps.push(
        TestStep::new(&format!("Preview {placement}"))
            .with_typed_characters(&[placement])
            .with_keystrokes(&["enter"])
            .add_named_assertion("The placement preview is visible", |app, window_id| {
                async_assert!(saved_bounds(app, window_id, PREVIEW).is_some())
            })
            .add_assertion(assert_num_panes_in_tab(0, 2)),
    );
    steps
}

pub fn test_keyboard_docking_single_pane_actions() -> Builder {
    with_split_commands(split_terminal_builder())
        .with_step(record_processes(ORIGINAL))
        .with_step(
            TestStep::new("Remember the original window")
                .add_assertion(save_active_window_id(SOURCE_WINDOW)),
        )
        .with_steps(choose_center_placement("Swap"))
        .with_step(
            TestStep::new("Confirm Swap with Enter")
                .with_keystrokes(&["enter"])
                .add_named_assertion_with_data_from_prior_step(
                    "The live terminals exchanged positions",
                    |app, window_id, data| {
                        assert_processes_preserved(app, window_id, data, SWAPPED)
                    },
                ),
        )
        .with_steps(choose_center_placement("Stack"))
        .with_step(
            TestStep::new("Confirm Stack with Enter")
                .with_keystrokes(&["enter"])
                .add_assertion(assert_num_panes_in_tab(0, 1)),
        )
        .with_steps(open_command_palette_and_run_action(UNDO_LAYOUT))
        .with_step(
            TestStep::new("Undo Stack")
                .add_assertion(assert_num_panes_in_tab(0, 2))
                .add_named_assertion_with_data_from_prior_step(
                    "The split contains both original terminal processes",
                    |app, window_id, data| {
                        assert_processes_preserved(app, window_id, data, SWAPPED)
                    },
                ),
        )
        .with_step(
            TestStep::new("Focus the original left pane in its swapped position")
                .with_action(|app, window_id, _| {
                    pane_group_view(app, window_id, 0).update(app, |group, ctx| {
                        let pane = group
                            .pane_id_from_index(1)
                            .expect("The swapped pane exists");
                        group.focus_pane(pane, true, ctx);
                    });
                })
                .add_assertion(assert_focused_pane_index(0, 1)),
        )
        .with_steps(open_keyboard_docking())
        .with_step(
            TestStep::new("Preview a new window for the active pane")
                .with_typed_characters(&["New window"])
                .with_keystrokes(&["enter"])
                .add_named_assertion("The new-window preview is visible", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, PREVIEW).is_some())
                })
                .add_assertion(assert_num_panes_in_tab(0, 2)),
        )
        .with_step(
            TestStep::new("Confirm the move to a new window")
                .with_keystrokes(&["enter"])
                .add_named_assertion_with_data_from_prior_step(
                    "Each window contains one of the two original live terminals",
                    |app, _, data| {
                        let source = *data
                            .get::<_, WindowId>(SOURCE_WINDOW)
                            .expect("The original window was recorded");
                        let windows = app.window_ids();
                        let Some(destination) = windows.iter().copied().find(|id| *id != source)
                        else {
                            return AssertionOutcome::failure(
                                "The new window has not opened".to_owned(),
                            );
                        };
                        let Ok(moved) = terminal_processes(app, destination, 0, 0) else {
                            return AssertionOutcome::failure(
                                "The moved pane is not ready".to_owned(),
                            );
                        };
                        let Ok(remaining) = terminal_processes(app, source, 0, 0) else {
                            return AssertionOutcome::failure(
                                "The original window has no remaining pane".to_owned(),
                            );
                        };
                        let expected = data
                            .get::<_, Vec<TerminalProcesses>>(PROCESS_SNAPSHOT)
                            .expect("The original processes were recorded");
                        async_assert_eq!([moved, remaining].as_slice(), expected.as_slice())
                    },
                ),
        )
}
