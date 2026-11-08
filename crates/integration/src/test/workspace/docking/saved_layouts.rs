use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::vec2f;
use warp::integration_testing::command_palette::{
    TestStepsExt, open_command_palette_and_run_action,
};
use warp::integration_testing::pane_group::assert_num_panes_in_tab;
use warp::integration_testing::workspace::assert_tab_count;
use warpui_core::async_assert;
use warpui_core::integration::{AssertionOutcome, TestStep};

use super::history::UNDO_LAYOUT;
use super::{
    assert_processes_preserved, preview_right, record_processes, release_tab, saved_bounds,
    terminal_tabs_with_running_commands,
};
use crate::Builder;

const MANAGE_LAYOUTS: &str = "Manage Pane Layouts";
const NAME_FIELD: &str = "pane_layouts:name";
const SNAPSHOT_NAME: &str = "Original tabs";
const SPLIT_SNAPSHOT_NAME: &str = "Side-by-side panes";
const APPLY_LAYOUT: &str = "Apply Layout";
const SEPARATE_TABS: &[(usize, usize)] = &[(0, 0), (1, 0)];
const SPLIT_TAB: &[(usize, usize)] = &[(0, 0), (0, 1)];

pub fn test_saved_pane_layout_preserves_processes() -> Builder {
    let preview_button = format!("pane_layouts:preview:{SNAPSHOT_NAME}");
    let split_preview_button = format!("pane_layouts:preview:{SPLIT_SNAPSHOT_NAME}");
    terminal_tabs_with_running_commands()
        .with_setup(|utils| {
            utils.set_env("XDG_STATE_HOME", Some(utils.test_dir().join("state")));
        })
        .with_step(
            TestStep::new("Size the window so both preview tiles can show their titles")
                .with_action(|app, window_id, _| {
                    let origin = app
                        .window_bounds(&window_id)
                        .expect("Workspace window has bounds")
                        .origin();
                    app.update(|ctx| {
                        ctx.set_and_cache_window_bounds(
                            window_id,
                            RectF::new(origin, vec2f(1200., 640.)),
                        );
                    });
                }),
        )
        .with_step(record_processes(SEPARATE_TABS))
        .with_steps(
            open_command_palette_and_run_action(MANAGE_LAYOUTS)
                .add_named_assertion("The layout name field is visible", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, NAME_FIELD).is_some())
                }),
        )
        .with_step(
            TestStep::new("Name the snapshot")
                .with_click_on_saved_position(NAME_FIELD)
                .with_typed_characters(&[SNAPSHOT_NAME]),
        )
        .with_step(
            TestStep::new("Save the two tabs as a snapshot")
                .with_click_on_saved_position("Save Snapshot")
                .add_named_assertion("The saved snapshot has a Preview button", {
                    let preview_button = preview_button.clone();
                    move |app, window_id| {
                        async_assert!(saved_bounds(app, window_id, &preview_button).is_some())
                    }
                }),
        )
        .with_step(
            TestStep::new("Close the layout manager")
                .with_keystrokes(&["escape"])
                .add_named_assertion("The layout manager is closed", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, NAME_FIELD).is_none())
                }),
        )
        .with_step(preview_right())
        .with_step(
            TestStep::new("Dock the two terminals in one tab")
                .with_action(release_tab)
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 2)),
        )
        .with_steps(
            open_command_palette_and_run_action(MANAGE_LAYOUTS).add_named_assertion(
                "The saved snapshot is available after reopening the manager",
                {
                    let preview_button = preview_button.clone();
                    move |app, window_id| {
                        async_assert!(saved_bounds(app, window_id, &preview_button).is_some())
                    }
                },
            ),
        )
        .with_step(
            TestStep::new("Name the split snapshot")
                .with_click_on_saved_position(NAME_FIELD)
                .with_typed_characters(&[SPLIT_SNAPSHOT_NAME]),
        )
        .with_step(
            TestStep::new("Save the split snapshot")
                .with_click_on_saved_position("Save Snapshot")
                .add_named_assertion("The split snapshot has a Preview button", {
                    let split_preview_button = split_preview_button.clone();
                    move |app, window_id| {
                        async_assert!(saved_bounds(app, window_id, &split_preview_button).is_some())
                    }
                }),
        )
        .with_step(
            TestStep::new("Preview two side-by-side panes")
                .with_click_on_saved_position(split_preview_button)
                .add_named_assertion("Both tiles fill the preview vertically", |app, window_id| {
                    let bounds = |id| saved_bounds(app, window_id, id);
                    let (Some(canvas), Some(left), Some(right)) = (
                        bounds("pane_layouts:preview_canvas:0"),
                        bounds("pane_layouts:preview_tile:0:0"),
                        bounds("pane_layouts:preview_tile:0:1"),
                    ) else {
                        return AssertionOutcome::failure("Missing preview bounds".to_owned());
                    };

                    async_assert!(
                        left.width() > 68.
                            && right.width() > 68.
                            && (left.min_x() - canvas.min_x()).abs() < 1.
                            && (left.max_x() - right.min_x()).abs() < 1.
                            && (right.max_x() - canvas.max_x()).abs() < 1.
                            && (left.min_y() - canvas.min_y()).abs() < 1.
                            && (right.min_y() - canvas.min_y()).abs() < 1.
                            && (left.max_y() - canvas.max_y()).abs() < 1.
                            && (right.max_y() - canvas.max_y()).abs() < 1.,
                        "Expected titled tiles side by side across {canvas:?}, got {left:?} and {right:?}"
                    )
                }),
        )
        .with_step(
            TestStep::new("Return to the list of saved layouts")
                .with_click_on_saved_position("Back")
                .add_named_assertion("The layout list is visible", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, NAME_FIELD).is_some())
                }),
        )
        .with_step(
            TestStep::new("Preview the snapshot without applying it")
                .with_click_on_saved_position(preview_button)
                .add_named_assertion("The preview can be applied", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, APPLY_LAYOUT).is_some())
                })
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 2))
                .add_named_assertion_with_data_from_prior_step(
                    "Both original terminals and processes are still in the split",
                    |app, window_id, data| {
                        assert_processes_preserved(app, window_id, data, SPLIT_TAB)
                    },
                ),
        )
        .with_step(
            TestStep::new("Apply the snapshot and restore separate tabs")
                .with_click_on_saved_position(APPLY_LAYOUT)
                .add_assertion(assert_tab_count(2))
                .add_assertion(assert_num_panes_in_tab(0, 1))
                .add_assertion(assert_num_panes_in_tab(1, 1))
                .add_named_assertion_with_data_from_prior_step(
                    "Both original terminals and processes returned to their own tabs",
                    |app, window_id, data| {
                        assert_processes_preserved(app, window_id, data, SEPARATE_TABS)
                    },
                ),
        )
        .with_steps(open_command_palette_and_run_action(UNDO_LAYOUT))
        .with_step(
            TestStep::new("Undo the applied layout")
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 2))
                .add_named_assertion_with_data_from_prior_step(
                    "Undo returns both original terminals and processes to the split",
                    |app, window_id, data| {
                        assert_processes_preserved(app, window_id, data, SPLIT_TAB)
                    },
                ),
        )
}
