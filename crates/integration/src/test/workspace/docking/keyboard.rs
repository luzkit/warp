use warp::integration_testing::command_palette::{
    TestStepsExt, open_command_palette_and_run_action,
};
use warp::integration_testing::pane_group::assert_num_panes_in_tab;
use warp::integration_testing::workspace::{assert_focused_tab_index, assert_tab_count};
use warpui_core::async_assert;
use warpui_core::integration::TestStep;

use super::history::UNDO_LAYOUT;
use super::{
    assert_processes_preserved, record_processes, saved_bounds, terminal_tabs_with_running_commands,
};
use crate::Builder;

const NAVIGATOR: &str = "Pane Navigator";
const DOCKING: &str = "Keyboard Docking";
const SEARCH: &str = "pane_navigator:search";
const MOVE_PANES: &str = "Move Panes";
const SEPARATE_TABS: &[(usize, usize)] = &[(0, 0), (1, 0)];
const GROUPED: &[(usize, usize)] = &[(0, 0), (0, 1)];

fn open_docking_preview() -> Vec<TestStep> {
    let mut steps = open_command_palette_and_run_action(DOCKING)
        .add_named_assertion("Keyboard docking search has opened", |app, window_id| {
            async_assert!(saved_bounds(app, window_id, SEARCH).is_some())
        });
    steps.push(
        TestStep::new("Search for a new tab destination").with_typed_characters(&["New tab"]),
    );
    steps.push(
        TestStep::new("Preview the group in a new tab")
            .with_keystrokes(&["enter"])
            .add_named_assertion("Move Panes is available", |app, window_id| {
                async_assert!(saved_bounds(app, window_id, MOVE_PANES).is_some())
            })
            .add_assertion(assert_tab_count(2))
            .add_named_assertion_with_data_from_prior_step(
                "The preview leaves both live terminals in their tabs",
                |app, window_id, data| {
                    assert_processes_preserved(app, window_id, data, SEPARATE_TABS)
                },
            ),
    );
    steps
}

pub fn test_keyboard_docking_groups_live_panes() -> Builder {
    terminal_tabs_with_running_commands()
        .with_step(record_processes(SEPARATE_TABS))
        .with_steps(
            open_command_palette_and_run_action(NAVIGATOR).add_named_assertion(
                "The navigator lists panes in both tabs",
                |app, window_id| {
                    async_assert!(
                        saved_bounds(app, window_id, "pane_navigator:row:0").is_some()
                            && saved_bounds(app, window_id, "pane_navigator:row:1").is_some()
                    )
                },
            ),
        )
        .with_step(
            TestStep::new("Select the first pane with the mouse")
                .with_click_on_saved_position("pane_navigator:select:0"),
        )
        .with_step(
            TestStep::new("Search for the second tab")
                .with_click_on_saved_position(SEARCH)
                .with_typed_characters(&["Tab 2"]),
        )
        .with_step(
            TestStep::new("Select the second pane with the keyboard")
                .with_keystrokes(&["shift-enter"]),
        )
        .with_step(
            TestStep::new("Close the navigator and keep the temporary selection")
                .with_keystrokes(&["escape"])
                .add_named_assertion("The navigator closed", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, SEARCH).is_none())
                }),
        )
        .with_steps(open_docking_preview())
        .with_step(
            TestStep::new("Cancel the group preview")
                .with_keystrokes(&["escape"])
                .add_named_assertion("The group preview closed", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, MOVE_PANES).is_none())
                })
                .add_assertion(assert_tab_count(2)),
        )
        .with_steps(open_docking_preview())
        .with_step(
            TestStep::new("Confirm the group move with Enter")
                .with_keystrokes(&["enter"])
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 2))
                .add_named_assertion_with_data_from_prior_step(
                    "Both original terminals and commands are in the new tab",
                    |app, window_id, data| {
                        assert_processes_preserved(app, window_id, data, GROUPED)
                    },
                ),
        )
        .with_steps(open_command_palette_and_run_action(UNDO_LAYOUT))
        .with_step(
            TestStep::new("Undo the group move in one step")
                .add_assertion(assert_tab_count(2))
                .add_assertion(assert_num_panes_in_tab(0, 1))
                .add_assertion(assert_num_panes_in_tab(1, 1))
                .add_named_assertion_with_data_from_prior_step(
                    "Undo restores both original live terminals",
                    |app, window_id, data| {
                        assert_processes_preserved(app, window_id, data, SEPARATE_TABS)
                    },
                ),
        )
        .with_steps(open_command_palette_and_run_action(
            "Redo Pane Layout Change",
        ))
        .with_step(
            TestStep::new("Redo the group move without closing the window")
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 2))
                .add_named_assertion_with_data_from_prior_step(
                    "Redo groups the two original live terminals again",
                    |app, window_id, data| {
                        assert_processes_preserved(app, window_id, data, GROUPED)
                    },
                ),
        )
        .with_steps(open_command_palette_and_run_action(UNDO_LAYOUT))
        .with_steps(open_command_palette_and_run_action(DOCKING))
        .with_step(
            TestStep::new("Find the other pane as a split destination")
                .with_typed_characters(&["Tab 2"]),
        )
        .with_step(TestStep::new("Choose the destination pane").with_keystrokes(&["enter"]))
        .with_step(
            TestStep::new("Find the right split placement").with_typed_characters(&["Right"]),
        )
        .with_step(
            TestStep::new("Preview the right split")
                .with_keystrokes(&["enter"])
                .add_named_assertion("The right split can be committed", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, MOVE_PANES).is_some())
                })
                .add_assertion(assert_tab_count(2)),
        )
        .with_step(
            TestStep::new("Dock the original first pane to the right")
                .with_keystrokes(&["enter"])
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 2))
                .add_named_assertion_with_data_from_prior_step(
                    "The original pane is on the right with the same running processes",
                    |app, window_id, data| {
                        assert_processes_preserved(app, window_id, data, &[(0, 1), (0, 0)])
                    },
                ),
        )
        .with_steps(open_command_palette_and_run_action(
            "Keep Current Focus After Docking",
        ))
        .with_steps(open_command_palette_and_run_action(NAVIGATOR))
        .with_step(
            TestStep::new("Select the pane beside the focused pane")
                .with_click_on_saved_position("pane_navigator:select:0"),
        )
        .with_step(
            TestStep::new("Choose to dock the selected pane")
                .with_click_on_saved_position(SEARCH)
                .with_keystrokes(&["alt-enter"]),
        )
        .with_step(
            TestStep::new("Find a new tab for the selected pane")
                .with_typed_characters(&["New tab"]),
        )
        .with_step(
            TestStep::new("Preview the move with the current focus policy")
                .with_keystrokes(&["enter"])
                .add_named_assertion("The new tab can be created", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, MOVE_PANES).is_some())
                }),
        )
        .with_step(
            TestStep::new("Move the other pane and retain focus in the original tab")
                .with_keystrokes(&["enter"])
                .add_assertion(assert_tab_count(2))
                .add_assertion(assert_focused_tab_index(0))
                .add_named_assertion_with_data_from_prior_step(
                    "The focused and moved terminals still have their original processes",
                    |app, window_id, data| {
                        assert_processes_preserved(app, window_id, data, SEPARATE_TABS)
                    },
                ),
        )
}
