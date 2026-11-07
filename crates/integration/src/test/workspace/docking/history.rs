use std::collections::HashMap;

use pathfinder_geometry::vector::{Vector2F, vec2f};
use sysinfo::{ProcessStatus, ProcessesToUpdate, System};
use warp::integration_testing::command_palette::open_command_palette_and_run_action;
use warp::integration_testing::pane_group::{assert_focused_pane_index, assert_num_panes_in_tab};
use warp::integration_testing::terminal::assert_no_block_executing;
use warp::integration_testing::view_getters::{pane_group_view, terminal_view};
use warp::integration_testing::workspace::assert_tab_count;
use warp::terminal::view::TerminalView;
use warpui_core::event::{Event, ModifiersState};
use warpui_core::integration::{AssertionOutcome, TestStep};
use warpui_core::{App, WindowId, async_assert, async_assert_eq};

use super::{
    PROCESS_SNAPSHOT, TerminalProcesses, assert_processes_preserved, dispatch_mouse_event,
    pane_bounds, preview_right, record_processes, release_tab, saved_bounds,
    split_terminal_builder, terminal_processes, terminal_tabs_with_running_commands,
    with_split_commands,
};
use crate::Builder;

pub(super) const UNDO_LAYOUT: &str = "Undo Pane Layout Change";
const REDO_LAYOUT: &str = "Redo Pane Layout Change";
const BALANCE_PANES: &str = "Balance Panes";
const SPLIT_LAYOUT: &[(usize, usize)] = &[(0, 0), (0, 1)];
const SEPARATE_TABS: &[(usize, usize)] = &[(0, 0), (1, 0)];
const BALANCED_SIZE: &str = "balanced pane dimensions";
const RESIZED_SIZE: &str = "resized pane dimensions";
const DIVIDER_START: &str = "divider drag start";
const RESIZE_PREVIEW: &str = "pane_group:resize_preview";

fn with_original_processes(step: TestStep, layout: &'static [(usize, usize)]) -> TestStep {
    step.add_named_assertion_with_data_from_prior_step(
        "Each terminal retained its view, session, shell, and running command",
        move |app, window_id, data| assert_processes_preserved(app, window_id, data, layout),
    )
}

fn with_surviving_terminal(step: TestStep) -> TestStep {
    step.add_assertion(assert_tab_count(1))
        .add_assertion(assert_num_panes_in_tab(0, 1))
        .add_named_assertion_with_data_from_prior_step(
            "The ended shell is gone and the surviving terminal is the original",
            |app, window_id, data| {
                let before = data
                    .get::<_, Vec<TerminalProcesses>>(PROCESS_SNAPSHOT)
                    .expect("Original terminal processes were recorded");
                let ended = [before[1].shell.0, before[1].command.0];
                let mut system = System::new();
                system.refresh_processes(ProcessesToUpdate::Some(&ended), true);
                if ended.into_iter().any(|pid| {
                    system
                        .process(pid)
                        .is_some_and(|process| process.status() != ProcessStatus::Zombie)
                }) {
                    return AssertionOutcome::failure(
                        "The exited terminal still has a live process".to_owned(),
                    );
                }
                let views = app
                    .views_of_type::<TerminalView>(window_id)
                    .expect("Workspace exists");
                if views.len() != 1 {
                    return AssertionOutcome::failure(format!(
                        "Expected one terminal view, got {}",
                        views.len()
                    ));
                }
                match terminal_processes(app, window_id, 0, 0) {
                    Ok(actual) => async_assert_eq!(&actual, &before[0]),
                    Err(error) => AssertionOutcome::failure(error.to_string()),
                }
            },
        )
}

pub fn test_pane_layout_undo_redo_preserves_processes() -> Builder {
    terminal_tabs_with_running_commands()
        .with_user_defaults(HashMap::from([(
            "UndoCloseEnabled".to_owned(),
            "false".to_owned(),
        )]))
        .with_step(record_processes(SEPARATE_TABS))
        .with_step(preview_right())
        .with_step(with_original_processes(
            TestStep::new("Dock the second tab into a right split")
                .with_action(release_tab)
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 2)),
            SPLIT_LAYOUT,
        ))
        .with_steps(open_command_palette_and_run_action(UNDO_LAYOUT))
        .with_step(with_original_processes(
            TestStep::new("Undo restores both original tabs")
                .add_assertion(assert_tab_count(2))
                .add_assertion(assert_num_panes_in_tab(0, 1))
                .add_assertion(assert_num_panes_in_tab(1, 1)),
            SEPARATE_TABS,
        ))
        .with_steps(open_command_palette_and_run_action(REDO_LAYOUT))
        .with_step(with_original_processes(
            TestStep::new("Redo restores the original split order")
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 2)),
            SPLIT_LAYOUT,
        ))
        .with_step(
            TestStep::new("Focus the moved terminal")
                .with_action(|app, window_id, _| {
                    pane_group_view(app, window_id, 0).update(app, |group, ctx| {
                        let pane = group.pane_id_from_index(1).expect("Moved pane exists");
                        group.focus_pane(pane, true, ctx);
                    });
                })
                .add_assertion(assert_focused_pane_index(0, 1)),
        )
        .with_step(
            TestStep::new("Stop the moved terminal's command")
                .with_keystrokes(&["ctrl-c"])
                .add_assertion(assert_no_block_executing(0, 1)),
        )
        .with_step(with_surviving_terminal(
            TestStep::new("Exit the moved terminal's shell").with_keystrokes(&["ctrl-d"]),
        ))
        .with_steps(open_command_palette_and_run_action(UNDO_LAYOUT))
        .with_step(with_surviving_terminal(TestStep::new(
            "Undo leaves the ended terminal closed",
        )))
}

#[derive(Debug, Clone, Copy)]
struct SplitSize {
    widths: [f32; 2],
    columns: [usize; 2],
}

impl SplitSize {
    fn read(app: &App, window_id: WindowId) -> Self {
        Self {
            widths: [0, 1].map(|pane| pane_bounds(app, window_id, 0, pane).width()),
            columns: [0, 1].map(|pane| {
                terminal_view(app, window_id, 0, pane).read(app, |terminal, _| {
                    terminal.model.lock().block_list().size().columns()
                })
            }),
        }
    }

    fn matches(self, expected: Self) -> bool {
        self.widths
            .into_iter()
            .zip(expected.widths)
            .all(|(actual, expected)| (actual - expected).abs() <= 1.0)
            && self.columns == expected.columns
    }
}

fn divider_center(app: &App, window_id: WindowId) -> Vector2F {
    let left = pane_bounds(app, window_id, 0, 0);
    let right = pane_bounds(app, window_id, 0, 1);
    vec2f((left.max_x() + right.min_x()) / 2., left.center().y())
}

fn press_divider(app: &mut App, window_id: WindowId, position: Vector2F, click_count: u32) {
    dispatch_mouse_event(
        app,
        window_id,
        Event::MouseMoved {
            position,
            cmd: false,
            shift: false,
            is_synthetic: false,
        },
    );
    dispatch_mouse_event(
        app,
        window_id,
        Event::LeftMouseDown {
            position,
            modifiers: ModifiersState::default(),
            click_count,
            is_first_mouse: false,
        },
    );
}

fn drag_divider(app: &mut App, window_id: WindowId, position: Vector2F) {
    dispatch_mouse_event(
        app,
        window_id,
        Event::LeftMouseDragged {
            position,
            modifiers: ModifiersState::default(),
        },
    );
}

fn release_divider(app: &mut App, window_id: WindowId, position: Vector2F) {
    dispatch_mouse_event(
        app,
        window_id,
        Event::LeftMouseUp {
            position,
            modifiers: ModifiersState::default(),
        },
    );
}

fn expect_split_size(step: TestStep, key: &'static str) -> TestStep {
    with_original_processes(
        step.add_named_assertion_with_data_from_prior_step(
            "Pane widths and terminal columns match the recorded layout",
            move |app, window_id, data| {
                let expected = *data
                    .get::<_, SplitSize>(key)
                    .expect("Split size was recorded");
                let actual = SplitSize::read(app, window_id);
                async_assert!(
                    actual.matches(expected),
                    "Expected {expected:?}, got {actual:?}"
                )
            },
        ),
        SPLIT_LAYOUT,
    )
}

fn preview_divider_drag() -> TestStep {
    expect_split_size(
        TestStep::new("Preview resizing the divider")
            .with_action(|app, window_id, data| {
                let start = divider_center(app, window_id);
                data.insert(DIVIDER_START, start);
                press_divider(app, window_id, start, 1);
                drag_divider(app, window_id, start + vec2f(60., 0.));
            })
            .add_named_assertion("The divider preview is visible", |app, window_id| {
                async_assert!(saved_bounds(app, window_id, RESIZE_PREVIEW).is_some())
            }),
        BALANCED_SIZE,
    )
}

fn no_resize_preview(app: &mut App, window_id: WindowId) -> AssertionOutcome {
    async_assert!(saved_bounds(app, window_id, RESIZE_PREVIEW).is_none())
}

pub fn test_pane_layout_resize_history_preserves_processes() -> Builder {
    with_split_commands(split_terminal_builder())
        .with_step(record_processes(SPLIT_LAYOUT))
        .with_step(
            TestStep::new("Record the equal split")
                .add_named_assertion_with_data_from_prior_step(
                    "The two panes start balanced",
                    |app, window_id, data| {
                        let size = SplitSize::read(app, window_id);
                        if (size.widths[0] - size.widths[1]).abs() > 1.0 {
                            return AssertionOutcome::failure(format!("Unbalanced split {size:?}"));
                        }
                        data.insert(BALANCED_SIZE, size);
                        AssertionOutcome::Success
                    },
                ),
        )
        .with_step(preview_divider_drag())
        .with_step(
            TestStep::new("Cancel the divider preview with Escape")
                .with_keystrokes(&["escape"])
                .add_named_assertion("The canceled preview disappears", no_resize_preview),
        )
        .with_step(expect_split_size(
            TestStep::new("Release the canceled divider drag").with_action(|app, window_id, data| {
                let start = *data.get::<_, Vector2F>(DIVIDER_START).expect("Divider was pressed");
                release_divider(app, window_id, start + vec2f(60., 0.));
            }),
            BALANCED_SIZE,
        ))
        .with_step(preview_divider_drag())
        .with_step(with_original_processes(
            TestStep::new("Complete the divider drag")
                .with_action(|app, window_id, data| {
                    let start = *data.get::<_, Vector2F>(DIVIDER_START).expect("Divider was pressed");
                    let end = start + vec2f(120., 0.);
                    drag_divider(app, window_id, end);
                    release_divider(app, window_id, end);
                })
                .add_named_assertion("The committed preview disappears", no_resize_preview)
                .add_named_assertion_with_data_from_prior_step(
                    "The left terminal grows and the right terminal shrinks",
                    |app, window_id, data| {
                        let balanced = *data.get::<_, SplitSize>(BALANCED_SIZE).expect("Balanced size was recorded");
                        let resized = SplitSize::read(app, window_id);
                        if resized.widths[0] <= balanced.widths[0] + 40.
                            || resized.widths[1] >= balanced.widths[1] - 40.
                            || resized.columns[0] <= balanced.columns[0]
                            || resized.columns[1] >= balanced.columns[1]
                        {
                            return AssertionOutcome::failure(format!("Expected both terminal widths to change from {balanced:?}, got {resized:?}"));
                        }
                        data.insert(RESIZED_SIZE, resized);
                        AssertionOutcome::Success
                    },
                ),
            SPLIT_LAYOUT,
        ))
        .with_steps(open_command_palette_and_run_action(UNDO_LAYOUT))
        .with_step(expect_split_size(TestStep::new("One Undo restores the entire drag"), BALANCED_SIZE))
        .with_steps(open_command_palette_and_run_action(REDO_LAYOUT))
        .with_step(expect_split_size(TestStep::new("Redo restores the unequal split"), RESIZED_SIZE))
        .with_step(expect_split_size(
            TestStep::new("Double-click the divider to reset it").with_action(|app, window_id, _| {
                let divider = divider_center(app, window_id);
                press_divider(app, window_id, divider, 2);
                release_divider(app, window_id, divider);
            }),
            BALANCED_SIZE,
        ))
        .with_steps(open_command_palette_and_run_action(UNDO_LAYOUT))
        .with_step(expect_split_size(TestStep::new("Undo restores the layout before the reset"), RESIZED_SIZE))
        .with_steps(open_command_palette_and_run_action(BALANCE_PANES))
        .with_step(expect_split_size(TestStep::new("Balance Panes restores the equal split"), BALANCED_SIZE))
}
