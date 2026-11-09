use pathfinder_geometry::vector::{Vector2F, vec2f};
use warp::cmd_or_ctrl_shift;
use warp::integration_testing::command_palette::open_command_palette_and_run_action;
use warp::integration_testing::pane_group::assert_num_panes_in_tab;
use warp::integration_testing::terminal::{
    execute_long_running_command, wait_until_bootstrapped_single_pane_for_tab,
};
use warp::integration_testing::view_getters::{pane_group_view, workspace_view};
use warp::integration_testing::workspace::{assert_focused_tab_index, assert_tab_count};
use warp::workspace::WorkspaceAction;
use warpui_core::event::{Event, ModifiersState};
use warpui_core::integration::{AssertionOutcome, StepDataMap, TestStep};
use warpui_core::{App, TypedActionView, WindowId, async_assert, async_assert_eq};

use super::history::UNDO_LAYOUT;
use super::{
    COMMAND, DOCK_POINT, MOVE_TO_OWN_TAB, PREVIEW_POSITION, PROCESS_SNAPSHOT, TerminalProcesses,
    assert_processes_preserved, dispatch_mouse_event, pane_bounds, pane_overflow_button_id,
    pane_position_id, record_processes, release_tab, saved_bounds, split_terminal_builder,
    tab_center, terminal_processes, terminal_tabs_with_running_commands, with_split_commands,
};
use crate::Builder;

const SWAP_POSITION: &str = "workspace_view:docking_swap";
const STACK_POSITION: &str = "workspace_view:docking_stack";
const STACK_DESTINATION_TITLE: &str = "Stack destination";

fn focus_tab(tab: usize) -> TestStep {
    TestStep::new(&format!("Focus tab {}", tab + 1))
        .with_action(move |app, window_id, _| {
            workspace_view(app, window_id).update(app, |workspace, ctx| {
                workspace.handle_action(&WorkspaceAction::ActivateTab(tab), ctx);
            });
        })
        .add_assertion(assert_focused_tab_index(tab))
}

fn split_and_tab_with_running_commands() -> Builder {
    with_split_commands(
        split_terminal_builder()
            .with_step(
                TestStep::new("Open a second tab").with_keystrokes(&[cmd_or_ctrl_shift("t")]),
            )
            .with_step(wait_until_bootstrapped_single_pane_for_tab(1))
            .with_step(execute_long_running_command(1, COMMAND.to_owned()))
            .with_step(focus_tab(0)),
    )
}

pub fn test_dock_entire_tab_layout() -> Builder {
    split_and_tab_with_running_commands()
        .with_step(focus_tab(1))
        .with_step(record_processes(&[(0, 0), (0, 1), (1, 0)]))
        .with_step(
            TestStep::new("Preview the split tab above the active terminal")
                .with_action(|app, window_id, data| {
                    let source = tab_center(app, window_id, 0);
                    start_drag(app, window_id, source);
                    let target = pane_bounds(app, window_id, 1, 0);
                    drag_to_drop_point(app, window_id, data, target.origin() + target.size() * vec2f(0.5, 0.1));
                })
                .add_assertion(assert_focused_tab_index(1))
                .add_assertion(assert_num_panes_in_tab(0, 2))
                .add_assertion(assert_num_panes_in_tab(1, 1))
                .add_named_assertion("The incoming layout previews above the active terminal", |app, window_id| {
                    let Some(preview) = saved_bounds(app, window_id, PREVIEW_POSITION) else {
                        return AssertionOutcome::failure("Docking preview is missing".to_owned());
                    };
                    let target = pane_bounds(app, window_id, 1, 0);
                    async_assert!(preview.max_y() <= target.center().y() && target.contains_point(preview.center()))
                }),
        )
        .with_step(check_moved_processes(
            TestStep::new("Dock both original panes above the destination terminal")
                .with_action(release_tab)
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 3))
                .add_named_assertion("The incoming panes keep their horizontal split", |app, window_id| {
                    let [upper_left, upper_right, lower] = [0, 1, 2].map(|pane| pane_bounds(app, window_id, 0, pane));
                    async_assert!(
                        upper_left.max_x() <= upper_right.min_x()
                            && (upper_left.center().y() - upper_right.center().y()).abs() <= 1.0
                            && upper_left.max_y() <= lower.min_y()
                            && (lower.width() - (upper_right.max_x() - upper_left.min_x())).abs() <= 1.0,
                        "Expected two side-by-side upper panes and a full-width lower pane, got {upper_left:?}, {upper_right:?}, {lower:?}"
                    )
                }),
            &[(0, 0), (0, 1), (0, 2)],
        ))
}

pub(super) fn drag_to(app: &mut App, window_id: WindowId, point: Vector2F) {
    dispatch_mouse_event(
        app,
        window_id,
        Event::LeftMouseDragged {
            position: point,
            modifiers: ModifiersState::default(),
        },
    );
}

pub(super) fn start_drag(app: &mut App, window_id: WindowId, point: Vector2F) {
    dispatch_mouse_event(
        app,
        window_id,
        Event::LeftMouseDown {
            position: point,
            modifiers: ModifiersState::default(),
            click_count: 1,
            is_first_mouse: false,
        },
    );
    drag_to(app, window_id, point + vec2f(12., 0.));
}

pub(super) fn start_pane_header_drag(app: &mut App, window_id: WindowId, pane: usize) {
    let bounds = pane_bounds(app, window_id, 0, pane);
    start_drag(
        app,
        window_id,
        vec2f(bounds.center().x(), bounds.min_y() + 10.),
    );
}

fn drag_to_drop_point(app: &mut App, window_id: WindowId, data: &mut StepDataMap, point: Vector2F) {
    data.insert(DOCK_POINT, point);
    drag_to(app, window_id, point);
}

fn check_moved_processes(step: TestStep, layout: &'static [(usize, usize)]) -> TestStep {
    step.add_named_assertion_with_data_from_prior_step(
        "Each terminal retained its view, session, shell, and running command",
        move |app, window_id, data| assert_processes_preserved(app, window_id, data, layout),
    )
}

pub fn test_dock_tab_into_nested_split() -> Builder {
    split_and_tab_with_running_commands()
        .with_step(record_processes(&[(0, 0), (0, 1), (1, 0)]))
        .with_step(
            TestStep::new("Preview a bottom split within the existing right pane")
                .with_action(|app, window_id, data| {
                    let source = tab_center(app, window_id, 1);
                    start_drag(app, window_id, source);
                    let right = pane_bounds(app, window_id, 0, 1);
                    drag_to_drop_point(app, window_id, data, right.origin() + right.size() * vec2f(0.5, 0.9));
                })
                .add_assertion(assert_tab_count(2))
                .add_assertion(assert_num_panes_in_tab(0, 2))
                .add_named_assertion("Preview is inside the right pane's lower half", |app, window_id| {
                    let Some(preview) = saved_bounds(app, window_id, PREVIEW_POSITION) else {
                        return AssertionOutcome::failure("Docking preview is missing".to_owned());
                    };
                    let right = pane_bounds(app, window_id, 0, 1);
                    async_assert!(
                        preview.min_y() >= right.center().y() && right.contains_point(preview.center()),
                        "Preview {preview:?} should dock inside the right pane {right:?}"
                    )
                }),
        )
        .with_step(check_moved_processes(
            TestStep::new("Drop the third terminal below the right pane")
                .with_action(release_tab)
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 3))
                .add_named_assertion("The left pane spans both rows of the nested right split", |app, window_id| {
                    let [left, upper, lower] = [0, 1, 2].map(|pane| pane_bounds(app, window_id, 0, pane));
                    async_assert!(
                        left.max_x() <= upper.min_x()
                            && (upper.center().x() - lower.center().x()).abs() <= 1.0
                            && upper.max_y() <= lower.min_y()
                            && (left.height() - (lower.max_y() - upper.min_y())).abs() <= 1.0,
                        "Expected a full-height left pane and two stacked right panes, got {left:?}, {upper:?}, {lower:?}"
                    )
                }),
            &[(0, 0), (0, 1), (0, 2)],
        ))
}

pub fn test_swap_panes_from_header_drag() -> Builder {
    with_split_commands(split_terminal_builder())
        .with_step(record_processes(&[(0, 0), (0, 1)]))
        .with_step(
            TestStep::new("Drag the left pane header over the right pane")
                .with_action(|app, window_id, _| {
                    start_pane_header_drag(app, window_id, 0);
                    let center = pane_bounds(app, window_id, 0, 1).center();
                    drag_to(app, window_id, center);
                })
                .add_named_assertion("Swap is available in the center", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, SWAP_POSITION).is_some())
                }),
        )
        .with_step(check_moved_processes(
            TestStep::new("Drop the left pane on Swap")
                .with_action(|app, window_id, data| {
                    let point = saved_bounds(app, window_id, SWAP_POSITION)
                        .expect("Swap target is visible")
                        .center();
                    drag_to_drop_point(app, window_id, data, point);
                    release_tab(app, window_id, data);
                })
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 2)),
            &[(0, 1), (0, 0)],
        ))
}

pub fn test_dock_pane_across_tabs() -> Builder {
    split_and_tab_with_running_commands()
        .with_step(record_processes(&[(0, 0), (0, 1), (1, 0)]))
        .with_step(
            TestStep::new("Drag the right pane's header over the second tab")
                .with_action(|app, window_id, _| {
                    let destination = pane_position_id(app, window_id, 1, 0);
                    app.presenter(window_id)
                        .expect("Workspace presenter exists")
                        .borrow_mut()
                        .position_cache_mut()
                        .clear_position(destination);
                    start_pane_header_drag(app, window_id, 1);
                    let tab = tab_center(app, window_id, 1);
                    drag_to(app, window_id, tab);
                })
                .add_assertion(assert_focused_tab_index(0))
                .add_named_assertion(
                    "Hover paints a placeholder without laying out the destination terminal",
                    |app, window_id| {
                        let destination = pane_position_id(app, window_id, 1, 0);
                        let placeholder =
                            format!("workspace_view:docking_placeholder:{destination}");
                        async_assert!(
                            saved_bounds(app, window_id, &placeholder).is_some()
                                && saved_bounds(app, window_id, &destination).is_none()
                        )
                    },
                ),
        )
        .with_step(
            TestStep::new("Preview a left split in the second tab")
                .with_action(|app, window_id, data| {
                    let destination = pane_position_id(app, window_id, 1, 0);
                    let placeholder = format!("workspace_view:docking_placeholder:{destination}");
                    let pane = saved_bounds(app, window_id, &placeholder)
                        .expect("Destination placeholder is visible");
                    drag_to_drop_point(
                        app,
                        window_id,
                        data,
                        pane.origin() + pane.size() * vec2f(0.1, 0.5),
                    );
                })
                .add_assertion(assert_focused_tab_index(0))
                .add_assertion(assert_num_panes_in_tab(0, 2))
                .add_assertion(assert_num_panes_in_tab(1, 1))
                .add_named_assertion("A directional preview is displayed", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, PREVIEW_POSITION).is_some())
                }),
        )
        .with_step(check_moved_processes(
            TestStep::new("Drop the original right pane into the second tab")
                .with_action(release_tab)
                .add_assertion(assert_tab_count(2))
                .add_assertion(assert_focused_tab_index(1))
                .add_assertion(assert_num_panes_in_tab(0, 1))
                .add_assertion(assert_num_panes_in_tab(1, 2)),
            &[(0, 0), (1, 0), (1, 1)],
        ))
}

fn check_visible_stack_member(step: TestStep, original_index: usize) -> TestStep {
    step.add_named_assertion_with_data_from_prior_step(
        "The visible member still owns its original session and processes",
        move |app, window_id, data| {
            let original = data
                .get::<_, Vec<TerminalProcesses>>(PROCESS_SNAPSHOT)
                .expect("Both terminals were observed before stacking");
            match terminal_processes(app, window_id, 0, 0) {
                Ok(actual) => async_assert_eq!(&actual, &original[original_index]),
                Err(error) => AssertionOutcome::failure(error.to_string()),
            }
        },
    )
}

fn open_stack_menu() -> TestStep {
    TestStep::new("Open the visible stack member's header menu").with_click_on_saved_position_fn(
        |app, window_id| pane_overflow_button_id(app, window_id, 0, 0),
    )
}

pub fn test_stack_terminal_tabs_and_extract_member() -> Builder {
    terminal_tabs_with_running_commands()
        .with_step(
            TestStep::new("Name the destination pane").with_action(|app, window_id, _| {
                pane_group_view(app, window_id, 0).update(app, |group, ctx| {
                    let pane = group
                        .pane_id_from_index(0)
                        .expect("Destination pane exists");
                    let configuration = group
                        .pane_by_id(pane)
                        .expect("Terminal exists")
                        .pane_configuration();
                    configuration.update(ctx, |configuration, ctx| {
                        configuration.set_custom_vertical_tabs_title(
                            STACK_DESTINATION_TITLE.to_owned(),
                            ctx,
                        );
                    });
                });
            }),
        )
        .with_step(record_processes(&[(0, 0), (1, 0)]))
        .with_step(
            TestStep::new("Drag the second tab over the first terminal")
                .with_action(|app, window_id, _| {
                    let source = tab_center(app, window_id, 1);
                    start_drag(app, window_id, source);
                    let center = pane_bounds(app, window_id, 0, 0).center();
                    drag_to(app, window_id, center);
                })
                .add_assertion(assert_tab_count(2))
                .add_named_assertion("Stack is available in the center", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, STACK_POSITION).is_some())
                }),
        )
        .with_step(check_visible_stack_member(
            TestStep::new("Drop the second tab on Stack")
                .with_action(|app, window_id, data| {
                    let point = saved_bounds(app, window_id, STACK_POSITION)
                        .expect("Stack target is visible")
                        .center();
                    drag_to_drop_point(app, window_id, data, point);
                    release_tab(app, window_id, data);
                })
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 1)),
            1,
        ))
        .with_step(open_stack_menu())
        .with_step(check_visible_stack_member(
            TestStep::new("Show the original destination terminal").with_click_on_saved_position(
                format!("Stacked pane 1 ({STACK_DESTINATION_TITLE})"),
            ),
            0,
        ))
        .with_step(open_stack_menu())
        .with_step(check_moved_processes(
            TestStep::new("Move the active member to a new tab")
                .with_click_on_saved_position(MOVE_TO_OWN_TAB)
                .add_assertion(assert_tab_count(2))
                .add_assertion(assert_focused_tab_index(1))
                .add_assertion(assert_num_panes_in_tab(0, 1))
                .add_assertion(assert_num_panes_in_tab(1, 1)),
            &[(1, 0), (0, 0)],
        ))
        .with_steps(open_command_palette_and_run_action(UNDO_LAYOUT))
        .with_step(check_visible_stack_member(
            TestStep::new("Undo extraction restores the active member in its stack")
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 1)),
            0,
        ))
        .with_steps(open_command_palette_and_run_action(UNDO_LAYOUT))
        .with_step(check_moved_processes(
            TestStep::new("Undo stacking restores the original two tabs")
                .add_assertion(assert_tab_count(2))
                .add_assertion(assert_num_panes_in_tab(0, 1))
                .add_assertion(assert_num_panes_in_tab(1, 1)),
            &[(0, 0), (1, 0)],
        ))
}
