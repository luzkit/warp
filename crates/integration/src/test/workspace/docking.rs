use anyhow::{Context, Result};
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use sysinfo::{Pid, ProcessStatus, ProcessesToUpdate, System};
use warp::cmd_or_ctrl_shift;
use warp::features::FeatureFlag;
use warp::integration_testing::pane_group::{assert_focused_pane_index, assert_num_panes_in_tab};
use warp::integration_testing::terminal::util::current_shell_starter_and_version;
use warp::integration_testing::terminal::{
    execute_long_running_command, execute_long_running_command_for_pane,
    wait_until_bootstrapped_pane, wait_until_bootstrapped_single_pane_for_tab,
};
use warp::integration_testing::view_getters::{pane_group_view, terminal_view, workspace_view};
use warp::integration_testing::workspace::{assert_focused_tab_index, assert_tab_count};
use warp::terminal::local_tty::TerminalManager as LocalTtyTerminalManager;
use warp::terminal::shell::ShellType;
use warp::terminal::view::TerminalView;
use warp::workspace::WorkspaceAction;
use warp_core::SessionId;
use warpui_core::event::{Event, ModifiersState};
use warpui_core::integration::{AssertionOutcome, StepDataMap, TestStep};
use warpui_core::{App, EntityId, TypedActionView, WindowId, async_assert, async_assert_eq};

use super::{dispatch_mouse_event, tab_center};
use crate::Builder;

mod history;
mod keyboard;
mod keyboard_single;
mod rearrangement;
mod saved_layouts;
mod windows;
pub use history::{
    test_pane_layout_resize_history_preserves_processes,
    test_pane_layout_undo_redo_preserves_processes,
};
pub use keyboard::test_keyboard_docking_groups_live_panes;
pub use keyboard_single::test_keyboard_docking_single_pane_actions;
pub use rearrangement::{
    test_dock_entire_tab_layout, test_dock_pane_across_tabs, test_dock_tab_into_nested_split,
    test_stack_terminal_tabs_and_extract_member, test_swap_panes_from_header_drag,
};
pub use saved_layouts::test_saved_pane_layout_preserves_processes;
pub use windows::{
    test_keyboard_docking_groups_panes_across_windows,
    test_move_pane_between_windows_preserves_processes,
    test_tear_out_tab_layout_preserves_processes,
};

const PROCESS_SNAPSHOT: &str = "processes before transfer";
const DOCK_POINT: &str = "dock point";
const PREVIEW_POSITION: &str = "workspace_view:docking_preview";
const MOVE_TO_OWN_TAB: &str = "Move to own tab";
const COMMAND: &str = r#"sh -c 'printf "dock-child=%s\n" "$$"; exec sleep 120'"#;

#[derive(Debug, PartialEq, Eq)]
struct TerminalProcesses {
    view: EntityId,
    session: SessionId,
    shell: (Pid, u64),
    command: (Pid, u64),
}

fn terminal_processes(
    app: &App,
    window_id: WindowId,
    tab: usize,
    pane: usize,
) -> Result<TerminalProcesses> {
    let shell = pane_group_view(app, window_id, tab).read(app, |group, ctx| {
        let manager = group.terminal_manager(pane, ctx)?;
        manager
            .as_ref(ctx)
            .as_any()
            .downcast_ref::<LocalTtyTerminalManager<TerminalView>>()?
            .pid()
            .map(Pid::from_u32)
    });
    let shell = shell.context("Terminal has no local shell PID")?;
    let view = terminal_view(app, window_id, tab, pane);
    let (session, command) = view.read(app, |view, _| {
        let model = view.model.lock();
        let block = model.block_list().active_block();
        let output = block.output_to_string();
        let command = output.lines().find_map(|line| {
            line.trim()
                .strip_prefix("dock-child=")?
                .parse::<u32>()
                .ok()
                .map(Pid::from_u32)
        });
        (block.session_id(), command)
    });
    let session = session.context("Active block has no session ID")?;
    let command = command.context("Active block has not printed its command PID")?;
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[shell, command]), true);
    let live_start_time = |pid| {
        system
            .process(pid)
            .filter(|process| process.status() != ProcessStatus::Zombie)
            .map(|process| (pid, process.start_time()))
            .with_context(|| format!("PID {pid} is no longer running"))
    };
    Ok(TerminalProcesses {
        view: view.id(),
        session,
        shell: live_start_time(shell)?,
        command: live_start_time(command)?,
    })
}

fn assert_processes_preserved(
    app: &mut App,
    window_id: WindowId,
    data: &mut StepDataMap,
    layout: &[(usize, usize)],
) -> AssertionOutcome {
    let expected = data
        .get::<_, Vec<TerminalProcesses>>(PROCESS_SNAPSHOT)
        .expect("The terminals must have been observed before the transfer");
    match processes_at(app, window_id, layout) {
        Ok(actual) => async_assert_eq!(&actual, expected),
        Err(error) => AssertionOutcome::failure(error.to_string()),
    }
}

fn processes_at(
    app: &App,
    window_id: WindowId,
    layout: &[(usize, usize)],
) -> Result<Vec<TerminalProcesses>> {
    layout
        .iter()
        .map(|&(tab, pane)| terminal_processes(app, window_id, tab, pane))
        .collect()
}

fn record_processes(layout: &'static [(usize, usize)]) -> TestStep {
    TestStep::new("Record live shells and commands").add_named_assertion_with_data_from_prior_step(
        "Each terminal has its original live shell and command",
        move |app, window_id, data| match processes_at(app, window_id, layout) {
            Ok(snapshot) => {
                data.insert(PROCESS_SNAPSHOT, snapshot);
                AssertionOutcome::Success
            }
            Err(error) => AssertionOutcome::failure(error.to_string()),
        },
    )
}

fn shell_supports_process_tests() -> bool {
    cfg!(unix)
        && matches!(
            current_shell_starter_and_version().0.shell_type(),
            ShellType::Bash | ShellType::Zsh
        )
}

fn saved_bounds(app: &App, window_id: WindowId, position_id: &str) -> Option<RectF> {
    let presenter = app
        .presenter(window_id)
        .expect("Workspace presenter exists");
    presenter
        .borrow()
        .position_cache()
        .get_position(position_id)
}

fn pane_bounds(app: &App, window_id: WindowId, tab: usize, pane: usize) -> RectF {
    let position_id = pane_position_id(app, window_id, tab, pane);
    saved_bounds(app, window_id, &position_id).expect("Pane must be visible")
}

fn pane_position_id(app: &App, window_id: WindowId, tab: usize, pane: usize) -> String {
    pane_group_view(app, window_id, tab).read(app, |group, _| {
        group
            .pane_id_from_index(pane)
            .expect("Pane exists")
            .position_id()
    })
}

fn drag_tab_to_right_edge(app: &mut App, window_id: WindowId, data: &mut StepDataMap) {
    let start = tab_center(app, window_id, 1);
    let modifiers = ModifiersState::default();
    dispatch_mouse_event(
        app,
        window_id,
        Event::LeftMouseDown {
            position: start,
            modifiers,
            click_count: 1,
            is_first_mouse: false,
        },
    );
    dispatch_mouse_event(
        app,
        window_id,
        Event::LeftMouseDragged {
            position: start + vec2f(12.0, 0.0),
            modifiers,
        },
    );
    move_drag_over_pane(app, window_id, data, vec2f(0.9, 0.5));
}

fn move_drag_over_pane(app: &mut App, window_id: WindowId, data: &mut StepDataMap, at: Vector2F) {
    let pane = pane_bounds(app, window_id, 0, 0);
    let position = pane.origin() + pane.size() * at;
    data.insert(DOCK_POINT, position);
    dispatch_mouse_event(
        app,
        window_id,
        Event::LeftMouseDragged {
            position,
            modifiers: ModifiersState::default(),
        },
    );
}

fn release_tab(app: &mut App, window_id: WindowId, data: &mut StepDataMap) {
    let position = *data
        .get::<_, Vector2F>(DOCK_POINT)
        .expect("Drag must have stored its destination");
    dispatch_mouse_event(
        app,
        window_id,
        Event::LeftMouseUp {
            position,
            modifiers: ModifiersState::default(),
        },
    );
}

fn with_preview_assertion(step: TestStep, center: Vector2F, size: Vector2F) -> TestStep {
    step.add_named_assertion(
        "Preview occupies the expected half of the pane",
        move |app, window_id| {
            let Some(preview) = saved_bounds(app, window_id, PREVIEW_POSITION) else {
                return AssertionOutcome::failure("Docking preview is missing".to_owned());
            };
            let pane = pane_bounds(app, window_id, 0, 0);
            let center_delta = preview.center() - (pane.origin() + pane.size() * center);
            let size_delta = preview.size() - pane.size() * size;
            async_assert!(
                center_delta.x().abs() <= 5.0
                    && center_delta.y().abs() <= 5.0
                    && size_delta.x().abs() <= 5.0
                    && size_delta.y().abs() <= 5.0,
                "Preview {preview:?} is misplaced or mis-sized relative to pane {pane:?}"
            )
        },
    )
}

fn no_preview(app: &mut App, window_id: WindowId) -> AssertionOutcome {
    async_assert!(saved_bounds(app, window_id, PREVIEW_POSITION).is_none())
}

fn preview_steps() -> Vec<TestStep> {
    [
        ("left", vec2f(0.1, 0.5), vec2f(0.25, 0.5), vec2f(0.5, 1.0)),
        ("top", vec2f(0.5, 0.1), vec2f(0.5, 0.25), vec2f(1.0, 0.5)),
        ("bottom", vec2f(0.5, 0.9), vec2f(0.5, 0.75), vec2f(1.0, 0.5)),
    ]
    .into_iter()
    .map(|(direction, cursor, center, size)| {
        with_preview_assertion(
            TestStep::new(&format!("Preview docking on the {direction}"))
                .with_action(move |app, window_id, data| {
                    move_drag_over_pane(app, window_id, data, cursor)
                })
                .add_assertion(assert_tab_count(2)),
            center,
            size,
        )
    })
    .collect()
}

fn preview_right() -> TestStep {
    with_preview_assertion(
        TestStep::new("Preview docking the second tab on the right")
            .with_action(drag_tab_to_right_edge)
            .add_assertion(assert_focused_tab_index(0))
            .add_assertion(assert_tab_count(2)),
        vec2f(0.75, 0.5),
        vec2f(0.5, 1.0),
    )
}

fn release_unchanged(name: &str) -> TestStep {
    TestStep::new(name)
        .with_action(release_tab)
        .add_assertion(assert_tab_count(2))
        .add_assertion(assert_focused_tab_index(0))
        .add_named_assertion_with_data_from_prior_step(
            "Both terminals and processes remain in their original tabs",
            |app, window_id, data| {
                assert_processes_preserved(app, window_id, data, &[(0, 0), (1, 0)])
            },
        )
}

fn terminal_tabs_with_running_commands() -> Builder {
    FeatureFlag::EnhancedPaneManagement.set_enabled(true);
    Builder::new()
        .set_should_run_test(shell_supports_process_tests)
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(TestStep::new("Open a second tab").with_keystrokes(&[cmd_or_ctrl_shift("t")]))
        .with_step(wait_until_bootstrapped_single_pane_for_tab(1))
        .with_step(execute_long_running_command(1, COMMAND.to_owned()))
        .with_step(
            TestStep::new("Focus the first tab as the docking destination")
                .with_action(|app, window_id, _| {
                    workspace_view(app, window_id).update(app, |workspace, ctx| {
                        workspace.handle_action(&WorkspaceAction::ActivateTab(0), ctx);
                    });
                })
                .add_assertion(assert_focused_tab_index(0)),
        )
        .with_step(execute_long_running_command(0, COMMAND.to_owned()))
}

pub fn test_dock_tab_preserves_running_processes() -> Builder {
    terminal_tabs_with_running_commands()
        .with_step(record_processes(&[(0, 0), (1, 0)]))
        .with_step(preview_right())
        .with_steps(preview_steps())
        .with_step(
            TestStep::new("Reject the center as a directional target")
                .with_action(|app, window_id, data| move_drag_over_pane(app, window_id, data, vec2f(0.5, 0.5)))
                .add_named_assertion("The center has no split preview", no_preview),
        )
        .with_step(release_unchanged("Release at the center without docking"))
        .with_step(preview_right())
        .with_step(
            TestStep::new("Cancel docking with Escape")
                .with_keystrokes(&["escape"])
                .add_named_assertion("Escape removes the split preview", no_preview),
        )
        .with_step(release_unchanged("Release the canceled drag over a valid target"))
        .with_step(preview_right())
        .with_step(
            TestStep::new("Drop the second terminal into a right split")
                .with_action(release_tab)
                .add_assertion(assert_tab_count(1))
                .add_named_assertion("Both terminals share one tab", |app, window_id| {
                    pane_group_view(app, window_id, 0)
                        .read(app, |group, _| async_assert_eq!(group.pane_count(), 2))
                })
                .add_named_assertion_with_data_from_prior_step(
                    "The original shells, commands, views, and sessions survive docking",
                    |app, window_id, data| {
                        let outcome = assert_processes_preserved(app, window_id, data, &[(0, 0), (0, 1)]);
                        if matches!(outcome, AssertionOutcome::Success) {
                            let [destination, source] = data
                                .get::<_, Vec<TerminalProcesses>>(PROCESS_SNAPSHOT)
                                .expect("Both terminals must have been observed before docking")
                                .as_slice() else {
                                    panic!("Docking test records two terminals");
                                };
                            println!(
                                "Docking preserved destination shell PID {} and child PID {}, and source shell PID {} and child PID {}",
                                destination.shell.0, destination.command.0, source.shell.0, source.command.0,
                            );
                        }
                        outcome
                    },
                ),
        )
}

fn pane_overflow_button_id(app: &App, window_id: WindowId, tab: usize, pane: usize) -> String {
    pane_group_view(app, window_id, tab).read(app, |group, _| {
        let pane_id = group
            .pane_id_from_index(pane)
            .expect("Terminal pane exists");
        let configuration_id = group
            .pane_by_id(pane_id)
            .expect("Terminal exists")
            .pane_configuration()
            .id();
        format!("pane_header_overflow_button:{configuration_id}")
    })
}

fn right_pane_overflow_button_id(app: &mut App, window_id: WindowId) -> String {
    pane_overflow_button_id(app, window_id, 0, 1)
}

fn split_terminal_builder() -> Builder {
    FeatureFlag::EnhancedPaneManagement.set_enabled(true);
    Builder::new()
        .set_should_run_test(shell_supports_process_tests)
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(
            TestStep::new("Split the terminal to the right")
                .with_keystrokes(&[cmd_or_ctrl_shift("d")])
                .add_assertion(assert_focused_pane_index(0, 1)),
        )
        .with_step(wait_until_bootstrapped_pane(0, 1))
}

fn with_split_commands(builder: Builder) -> Builder {
    builder
        .with_step(execute_long_running_command_for_pane(0, 1, COMMAND))
        .with_step(
            TestStep::new("Focus the left pane")
                .with_action(|app, window_id, _| {
                    pane_group_view(app, window_id, 0).update(app, |group, ctx| {
                        let pane = group.pane_id_from_index(0).expect("Left pane exists");
                        group.focus_pane(pane, true, ctx);
                    });
                })
                .add_assertion(assert_focused_pane_index(0, 0)),
        )
        .with_step(execute_long_running_command_for_pane(0, 0, COMMAND))
}

pub fn test_move_split_pane_to_new_tab_from_overflow() -> Builder {
    with_split_commands(split_terminal_builder())
        .with_step(record_processes(&[(0, 0), (0, 1)]))
        .with_step(
            TestStep::new("Reveal the inactive pane's header controls")
                .with_event_fn(|app, window_id| {
                    let bounds = pane_bounds(app, window_id, 0, 1);
                    Event::MouseMoved {
                        position: vec2f(bounds.center().x(), bounds.min_y() + 10.0),
                        cmd: false,
                        shift: false,
                        is_synthetic: false,
                    }
                })
                .add_assertion(assert_focused_pane_index(0, 0))
                .add_named_assertion("Right pane overflow button is visible", |app, window_id| {
                    let button_id = right_pane_overflow_button_id(app, window_id);
                    async_assert!(saved_bounds(app, window_id, &button_id).is_some())
                }),
        )
        .with_step(
            TestStep::new("Open the inactive right pane's overflow menu")
                .with_click_on_saved_position_fn(right_pane_overflow_button_id)
                .add_named_assertion(
                    "Move to own tab is below Maximize pane",
                    |app, window_id| {
                        let maximize = saved_bounds(app, window_id, "Maximize pane");
                        let move_pane = saved_bounds(app, window_id, MOVE_TO_OWN_TAB);
                        if let (Some(maximize), Some(move_pane)) = (maximize, move_pane) {
                            async_assert!(move_pane.center().y() > maximize.center().y())
                        } else {
                            AssertionOutcome::failure("Expected both pane menu items".to_owned())
                        }
                    },
                ),
        )
        .with_step(
            TestStep::new("Move the right pane to a new tab")
                .with_click_on_saved_position(MOVE_TO_OWN_TAB)
                .add_assertion(assert_tab_count(2))
                .add_assertion(assert_focused_tab_index(1))
                .add_assertion(assert_num_panes_in_tab(0, 1))
                .add_assertion(assert_num_panes_in_tab(1, 1))
                .add_named_assertion_with_data_from_prior_step(
                    "The selected pane and its original processes are now in the new tab",
                    |app, window_id, data| {
                        assert_processes_preserved(app, window_id, data, &[(0, 0), (1, 0)])
                    },
                ),
        )
}
