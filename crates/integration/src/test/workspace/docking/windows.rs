use std::time::Duration;

use anyhow::{Result, ensure};
use pathfinder_geometry::vector::vec2f;
use warp::integration_testing::command_palette::{
    TestStepsExt, open_command_palette_and_run_action,
};
use warp::integration_testing::terminal::{
    execute_long_running_command, wait_until_bootstrapped_single_pane_for_tab,
};
use warp::integration_testing::view_getters::{pane_group_view, workspace_view};
use warp::integration_testing::window::{add_and_save_window, save_active_window_id};
use warpui_core::event::Event;
use warpui_core::integration::{AssertionOutcome, StepDataMap, TestStep};
use warpui_core::platform::FullscreenState;
use warpui_core::{App, WindowId, async_assert, async_assert_eq};

use super::super::{
    SOURCE_WINDOW_KEY, TARGET_WINDOW_KEY, focus_saved_window, set_saved_window_origin,
    source_local_point_for_screen_point,
};
use super::rearrangement::{drag_to, start_drag, start_pane_header_drag};
use super::{
    COMMAND, DOCK_POINT, PREVIEW_POSITION, PROCESS_SNAPSHOT, TerminalProcesses,
    assert_processes_preserved, pane_bounds, pane_overflow_button_id, pane_position_id,
    record_processes, release_tab, saved_bounds, shell_supports_process_tests,
    split_terminal_builder, tab_center, terminal_processes, terminal_tabs_with_running_commands,
    with_split_commands,
};
use crate::Builder;

type WindowPane = (&'static str, usize);

const BEFORE_MOVE: &[WindowPane] = &[
    (SOURCE_WINDOW_KEY, 0),
    (SOURCE_WINDOW_KEY, 1),
    (TARGET_WINDOW_KEY, 0),
];
const AFTER_MOVE: &[WindowPane] = &[
    (SOURCE_WINDOW_KEY, 0),
    (TARGET_WINDOW_KEY, 1),
    (TARGET_WINDOW_KEY, 0),
];
const TAB_PREVIEW_POSITION: &str = "workspace_view:window_docking_tab_preview";
const SPLIT_LAYOUT: &[(usize, usize)] = &[(0, 0), (0, 1)];

fn saved_window(data: &StepDataMap, key: &'static str) -> WindowId {
    *data
        .get::<_, WindowId>(key)
        .unwrap_or_else(|| panic!("Window ID was not recorded for {key}"))
}

fn window_processes(
    app: &App,
    data: &StepDataMap,
    layout: &[WindowPane],
) -> Result<Vec<TerminalProcesses>> {
    layout
        .iter()
        .map(|&(key, pane)| {
            let window = saved_window(data, key);
            ensure!(app.window_ids().contains(&window), "Missing {key}");
            let has_terminal = workspace_view(app, window).read(app, |workspace, ctx| {
                workspace.get_pane_group_view(0).is_some_and(|group| {
                    group
                        .as_ref(ctx)
                        .terminal_view_at_pane_index(pane, ctx)
                        .is_some()
                })
            });
            ensure!(has_terminal, "Missing pane {pane} in {key}");
            terminal_processes(app, window, 0, pane)
        })
        .collect()
}

fn assert_window_processes(
    app: &App,
    data: &StepDataMap,
    layout: &[WindowPane],
) -> AssertionOutcome {
    let actual = match window_processes(app, data, layout) {
        Ok(processes) => processes,
        Err(error) => return AssertionOutcome::failure(error.to_string()),
    };
    let expected = data
        .get::<_, Vec<TerminalProcesses>>(PROCESS_SNAPSHOT)
        .expect("Live terminal processes were recorded");
    async_assert_eq!(&actual, expected)
}

fn reveal_source_pane_header() -> TestStep {
    TestStep::new("Move the pointer over the source pane header")
        .with_event_fn(|app, window| {
            let bounds = pane_bounds(app, window, 0, 1);
            Event::MouseMoved {
                position: vec2f(bounds.center().x(), bounds.min_y() + 10.0),
                cmd: false,
                shift: false,
                is_synthetic: false,
            }
        })
        .add_named_assertion("The pane header controls are visible", |app, window| {
            let button = pane_overflow_button_id(app, window, 0, 1);
            async_assert!(saved_bounds(app, window, &button).is_some())
        })
}

fn preview_pane_in_other_window() -> TestStep {
    TestStep::new("Preview the right split in the other window")
        .with_action(|app, _, data| {
            let source = saved_window(data, SOURCE_WINDOW_KEY);
            let target = saved_window(data, TARGET_WINDOW_KEY);
            let target_pane = pane_bounds(app, target, 0, 0);
            let window_origin = app
                .window_bounds(&target)
                .expect("Target window bounds")
                .origin();
            let screen_point =
                window_origin + target_pane.origin() + target_pane.size() * vec2f(0.9, 0.5);
            let point = source_local_point_for_screen_point(app, source, screen_point);
            start_pane_header_drag(app, source, 1);
            data.insert(DOCK_POINT, point);
            drag_to(app, source, point);
        })
        .add_named_assertion_with_data_from_prior_step(
            "Only the target window previews the split",
            |app, _, data| {
                let source = saved_window(data, SOURCE_WINDOW_KEY);
                let target = saved_window(data, TARGET_WINDOW_KEY);
                let Some(preview) = saved_bounds(app, target, PREVIEW_POSITION) else {
                    return AssertionOutcome::failure(
                        "The target window has no split preview".to_owned(),
                    );
                };
                let pane = pane_bounds(app, target, 0, 0);
                async_assert!(
                    saved_bounds(app, source, PREVIEW_POSITION).is_none()
                        && pane.contains_point(preview.center())
                        && preview.center().x() > pane.center().x(),
                    "Expected a right-side preview in {pane:?}, got {preview:?}"
                )
            },
        )
        .add_named_assertion_with_data_from_prior_step(
            "Preview leaves the original processes in their current windows",
            |app, _, data| assert_window_processes(app, data, BEFORE_MOVE),
        )
}

fn release_in_other_window(app: &mut App, _: WindowId, data: &mut StepDataMap) {
    let source = saved_window(data, SOURCE_WINDOW_KEY);
    release_tab(app, source, data);
}

pub fn test_move_pane_between_windows_preserves_processes() -> Builder {
    with_split_commands(split_terminal_builder().set_should_run_test(|| {
        // AppKit hides integration windows by default, so they have no native stacking order.
        cfg!(target_os = "macos")
            && std::env::var_os("WARPUI_USE_REAL_DISPLAY_IN_INTEGRATION_TESTS").is_some()
            && shell_supports_process_tests()
    }))
        .with_step(
            TestStep::new("Remember the source window")
                .add_assertion(save_active_window_id(SOURCE_WINDOW_KEY)),
        )
        .with_step(add_and_save_window(TARGET_WINDOW_KEY))
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(execute_long_running_command(0, COMMAND.to_owned()))
        .with_step(
            TestStep::new("Record the live terminals in both windows")
                .add_named_assertion_with_data_from_prior_step(
                    "All three terminal views, sessions, shells, and commands are running",
                    |app, _, data| match window_processes(app, data, BEFORE_MOVE) {
                        Ok(snapshot) => {
                            data.insert(PROCESS_SNAPSHOT, snapshot);
                            AssertionOutcome::Success
                        }
                        Err(error) => AssertionOutcome::failure(error.to_string()),
                    },
                ),
        )
        .with_step(set_saved_window_origin(SOURCE_WINDOW_KEY, vec2f(100.0, 100.0)))
        .with_step(set_saved_window_origin(TARGET_WINDOW_KEY, vec2f(900.0, 100.0)))
        .with_step(focus_saved_window(SOURCE_WINDOW_KEY))
        .with_step(reveal_source_pane_header())
        .with_step(preview_pane_in_other_window())
        .with_step(
            TestStep::new("Cancel the cross-window drag with Escape")
                .with_keystrokes(&["escape"])
                .add_named_assertion_with_data_from_prior_step(
                    "Cancel clears the target preview",
                    |app, _, data| {
                        let target = saved_window(data, TARGET_WINDOW_KEY);
                        async_assert!(saved_bounds(app, target, PREVIEW_POSITION).is_none())
                    },
                ),
        )
        .with_step(
            TestStep::new("Release the canceled drag over the target")
                .with_action(release_in_other_window)
                .add_named_assertion_with_data_from_prior_step(
                    "All terminals stay in their original windows",
                    |app, _, data| assert_window_processes(app, data, BEFORE_MOVE),
                ),
        )
        .with_step(reveal_source_pane_header())
        .with_step(preview_pane_in_other_window())
        .with_step(
            TestStep::new("Release to move the original pane to the other window")
                .with_action(release_in_other_window)
                .add_named_assertion_with_data_from_prior_step(
                    "Both windows preserve their terminal views, sessions, and running processes",
                    |app, _, data| assert_window_processes(app, data, AFTER_MOVE),
                )
                .add_named_assertion_with_data_from_prior_step(
                    "The source has one pane and the moved pane has focus in the destination",
                    |app, _, data| {
                        let source = saved_window(data, SOURCE_WINDOW_KEY);
                        let target = saved_window(data, TARGET_WINDOW_KEY);
                        let windows = app.window_ids();
                        if !windows.contains(&source) || !windows.contains(&target) {
                            return AssertionOutcome::failure(format!("Expected source and target windows, got {windows:?}"));
                        }
                        if app.read(|ctx| ctx.windows().active_window()) != Some(target) {
                            return AssertionOutcome::failure("The target window did not take focus".to_owned());
                        }
                        let source_count = pane_group_view(app, source, 0)
                            .read(app, |group, _| group.pane_count());
                        let (target_count, moved_focused) = pane_group_view(app, target, 0)
                            .read(app, |group, ctx| {
                                (group.pane_count(), Some(group.focused_pane_id(ctx)) == group.pane_id_from_index(1))
                            });
                        async_assert!(
                            source_count == 1 && target_count == 2 && moved_focused,
                            "Expected 1 source pane, 2 target panes, and focus on the moved pane. Got {source_count}, {target_count}, and moved focus {moved_focused}"
                        )
                    },
                ),
        )
        .with_step(preview_active_layout_in_original_window())
        .with_step(
            TestStep::new("Release the active layout onto the other tab strip")
                .with_action(|app, _, data| {
                    let layout_window = saved_window(data, TARGET_WINDOW_KEY);
                    release_tab(app, layout_window, data);
                })
                .add_named_assertion_with_data_from_prior_step(
                    "The original window gets a focused split tab and all original processes survive",
                    |app, _, data| assert_returned_layout(app, data),
                ),
        )
}

fn preview_active_layout_in_original_window() -> TestStep {
    TestStep::new("Drag the active split tab onto the other window's tab strip")
        .with_action(|app, _, data| {
            let from = saved_window(data, TARGET_WINDOW_KEY);
            let to = saved_window(data, SOURCE_WINDOW_KEY);
            let start = tab_center(app, from, 0);
            let target = tab_center(app, to, 0);
            let screen_point = app.window_bounds(&to).expect("Window bounds").origin() + target;
            let point = source_local_point_for_screen_point(app, from, screen_point);
            start_drag(app, from, start);
            data.insert(DOCK_POINT, point);
            drag_to(app, from, point);
        })
        .add_named_assertion_with_data_from_prior_step(
            "The other tab strip shows the insertion preview",
            |app, _, data| {
                let window = saved_window(data, SOURCE_WINDOW_KEY);
                async_assert!(saved_bounds(app, window, TAB_PREVIEW_POSITION).is_some())
            },
        )
        .add_named_assertion_with_data_from_prior_step(
            "The split tab remains intact in its original window before release",
            |app, _, data| assert_window_processes(app, data, AFTER_MOVE),
        )
}

fn assert_returned_layout(app: &mut App, data: &mut StepDataMap) -> AssertionOutcome {
    let destination = saved_window(data, SOURCE_WINDOW_KEY);
    let windows = app.window_ids();
    if windows.as_slice() != [destination] {
        return AssertionOutcome::failure(format!(
            "Expected only the destination window, got {windows:?}"
        ));
    }
    if app.read(|ctx| ctx.windows().active_window()) != Some(destination) {
        return AssertionOutcome::failure("The destination did not take focus".to_owned());
    }

    let (tab_count, active) = workspace_view(app, destination).read(app, |workspace, _| {
        (workspace.tab_count(), workspace.active_tab_index())
    });
    if tab_count != 2 || active >= tab_count {
        return AssertionOutcome::failure(format!(
            "Expected two tabs and a focused tab, got {tab_count} and active index {active}"
        ));
    }

    let inactive = 1 - active;
    let (split_count, right_focused) =
        pane_group_view(app, destination, active).read(app, |group, ctx| {
            (
                group.pane_count(),
                Some(group.focused_pane_id(ctx)) == group.pane_id_from_index(1),
            )
        });
    let remaining_count =
        pane_group_view(app, destination, inactive).read(app, |group, _| group.pane_count());
    if split_count != 2 || remaining_count != 1 || !right_focused {
        return AssertionOutcome::failure(format!(
            "Expected a focused right pane in the transferred split and one pane in the original tab, got {split_count}, {remaining_count}, and right focus {right_focused}"
        ));
    }

    assert_processes_preserved(
        app,
        destination,
        data,
        &[(inactive, 0), (active, 1), (active, 0)],
    )
}

fn assert_moved_layout(
    app: &mut App,
    destination: WindowId,
    data: &mut StepDataMap,
) -> AssertionOutcome {
    let workspace = workspace_view(app, destination);
    let tab_count = workspace.read(app, |workspace, _| workspace.tab_count());
    if tab_count != 1 {
        return AssertionOutcome::failure(format!(
            "Expected a single transferred tab, got {tab_count}"
        ));
    }

    let group = pane_group_view(app, destination, 0);
    let (pane_count, has_left_focus) = group.read(app, |group, ctx| {
        (
            group.pane_count(),
            Some(group.focused_pane_id(ctx)) == group.pane_id_from_index(0),
        )
    });
    if pane_count != 2 || !has_left_focus {
        return AssertionOutcome::failure(format!(
            "Expected two panes with the left selected, got {pane_count} panes and left focus {has_left_focus}"
        ));
    }
    let bounds = |pane| {
        saved_bounds(
            app,
            destination,
            &pane_position_id(app, destination, 0, pane),
        )
    };
    let (Some(left), Some(right)) = (bounds(0), bounds(1)) else {
        return AssertionOutcome::failure(
            "The moved split panes have not been laid out".to_owned(),
        );
    };
    if left.max_x() > right.min_x() || (left.center().y() - right.center().y()).abs() > 1.0 {
        return AssertionOutcome::failure(format!(
            "The original horizontal split should remain side by side: {left:?}, {right:?}"
        ));
    }
    assert_processes_preserved(app, destination, data, SPLIT_LAYOUT)
}

fn assert_tear_out_closes_source(app: &mut App, data: &mut StepDataMap) -> AssertionOutcome {
    let source = saved_window(data, SOURCE_WINDOW_KEY);
    let windows = app.window_ids();
    let [destination] = windows.as_slice() else {
        return AssertionOutcome::failure(format!(
            "Expected one window after tear-out, got {windows:?}"
        ));
    };
    if *destination == source || app.read(|ctx| ctx.windows().active_window()) != Some(*destination)
    {
        return AssertionOutcome::failure(
            "Expected the source to close and the destination to take focus".to_owned(),
        );
    }
    let outcome = assert_moved_layout(app, *destination, data);
    if matches!(outcome, AssertionOutcome::Success) {
        data.insert(TARGET_WINDOW_KEY, *destination);
    }
    outcome
}

fn assert_tear_out_keeps_source(app: &mut App, data: &mut StepDataMap) -> AssertionOutcome {
    let source = saved_window(data, TARGET_WINDOW_KEY);
    let windows = app.window_ids();
    if windows.len() != 2 || !windows.contains(&source) {
        return AssertionOutcome::failure(format!(
            "Expected a new window and the retained source, got {windows:?}"
        ));
    }
    let Some(destination) = app.read(|ctx| ctx.windows().active_window()) else {
        return AssertionOutcome::failure(
            "Expected the destination window to be focused".to_owned(),
        );
    };
    if destination == source || !windows.contains(&destination) {
        return AssertionOutcome::failure(
            "The new destination window did not take focus".to_owned(),
        );
    }

    let workspace = workspace_view(app, source);
    let tab_count = workspace.read(app, |workspace, _| workspace.tab_count());
    if tab_count != 1 {
        return AssertionOutcome::failure(format!("Expected one replacement tab, got {tab_count}"));
    }
    let group = pane_group_view(app, source, 0);
    let (pane_count, replacement) = group.read(app, |group, ctx| {
        (
            group.pane_count(),
            group
                .terminal_view_at_pane_index(0, ctx)
                .map(|view| view.id()),
        )
    });
    if pane_count != 1 {
        return AssertionOutcome::failure(format!(
            "Expected one replacement pane, got {pane_count}"
        ));
    }
    let Some(replacement) = replacement else {
        return AssertionOutcome::failure(
            "Expected a replacement terminal in the retained window".to_owned(),
        );
    };
    let original = data
        .get::<_, Vec<TerminalProcesses>>(PROCESS_SNAPSHOT)
        .expect("Live terminal processes were recorded");
    if original.iter().any(|terminal| terminal.view == replacement) {
        return AssertionOutcome::failure(
            "The retained source still contains an original terminal".to_owned(),
        );
    }

    assert_moved_layout(app, destination, data)
}

pub fn test_tear_out_tab_layout_preserves_processes() -> Builder {
    with_split_commands(split_terminal_builder())
        .with_step(
            TestStep::new("Remember the source window")
                .add_assertion(save_active_window_id(SOURCE_WINDOW_KEY)),
        )
        .with_step(record_processes(SPLIT_LAYOUT))
        .with_steps(open_command_palette_and_run_action(
            "Move Tab Layout to New Window",
        ))
        .with_step(
            TestStep::new("Verify the complete split layout in the new window")
                .add_named_assertion_with_data_from_prior_step(
                    "The original panes and processes remain together and the empty source closes",
                    |app, _, data| assert_tear_out_closes_source(app, data),
                ),
        )
        .with_steps(open_command_palette_and_run_action(
            "Keep Empty Windows After Moving Panes",
        ))
        .with_steps(open_command_palette_and_run_action(
            "Move Tab Layout to New Window",
        ))
        .with_step(
            TestStep::new("Tear out the layout again with empty-window cleanup disabled")
                .add_named_assertion_with_data_from_prior_step(
                    "The original layout moves and its former window retains a fresh terminal",
                    |app, _, data| assert_tear_out_keeps_source(app, data),
                ),
        )
}

pub fn test_keyboard_docking_groups_panes_across_windows() -> Builder {
    const SEARCH: &str = "pane_navigator:search";
    const BEFORE: &[WindowPane] = &[
        (SOURCE_WINDOW_KEY, 0),
        (SOURCE_WINDOW_KEY, 1),
        (TARGET_WINDOW_KEY, 0),
    ];
    const AFTER: &[WindowPane] = &[
        (TARGET_WINDOW_KEY, 0),
        (SOURCE_WINDOW_KEY, 0),
        (TARGET_WINDOW_KEY, 1),
    ];

    with_split_commands(split_terminal_builder())
        .with_step(
            TestStep::new("Remember the source window")
                .add_assertion(save_active_window_id(SOURCE_WINDOW_KEY)),
        )
        .with_step(add_and_save_window(TARGET_WINDOW_KEY))
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(execute_long_running_command(0, COMMAND.to_owned()))
        .with_step(
            TestStep::new("Record the live terminals in both windows")
                .add_named_assertion_with_data_from_prior_step(
                    "All three shells and commands are running",
                    |app, _, data| match window_processes(app, data, BEFORE) {
                        Ok(snapshot) => {
                            data.insert(PROCESS_SNAPSHOT, snapshot);
                            AssertionOutcome::Success
                        }
                        Err(error) => AssertionOutcome::failure(error.to_string()),
                    },
                ),
        )
        .with_steps(
            open_command_palette_and_run_action("Pane Navigator").add_named_assertion(
                "The navigator lists panes from both windows",
                |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, "pane_navigator:row:2").is_some())
                },
            ),
        )
        .with_step(
            TestStep::new("Select one pane in the first window")
                .with_click_on_saved_position("pane_navigator:select:0"),
        )
        .with_step(
            TestStep::new("Find the terminal in the second window")
                .with_click_on_saved_position(SEARCH)
                .with_typed_characters(&["Window 2"]),
        )
        .with_step(
            TestStep::new("Select the terminal in the second window")
                .with_keystrokes(&["shift-enter"]),
        )
        .with_step(
            TestStep::new("Open keyboard docking with both selected terminals")
                .with_keystrokes(&["alt-enter"]),
        )
        .with_step(
            TestStep::new("Find a new tab in the second window")
                .with_typed_characters(&["New tab Window 2"]),
        )
        .with_step(
            TestStep::new("Preview the transfer without moving any terminal")
                .with_keystrokes(&["enter"])
                .add_named_assertion("The group move can be committed", |app, window_id| {
                    async_assert!(saved_bounds(app, window_id, "Move Panes").is_some())
                })
                .add_named_assertion_with_data_from_prior_step(
                    "All three processes stay in their source panes during preview",
                    |app, _, data| assert_window_processes(app, data, BEFORE),
                ),
        )
        .with_step(
            TestStep::new("Move both selected terminals into one tab")
                .with_keystrokes(&["enter"])
                .add_named_assertion_with_data_from_prior_step(
                    "Both moved terminals and the remaining terminal retain their processes",
                    |app, _, data| assert_window_processes(app, data, AFTER),
                ),
        )
}

pub fn test_keyboard_docking_in_native_fullscreen_preserves_processes() -> Builder {
    const SEPARATE_TABS: &[(usize, usize)] = &[(0, 0), (1, 0)];
    const RIGHT_SPLIT: &[(usize, usize)] = &[(0, 1), (0, 0)];

    terminal_tabs_with_running_commands()
        .set_should_run_test(|| {
            cfg!(target_os = "macos")
                && std::env::var_os("WARPUI_USE_REAL_DISPLAY_IN_INTEGRATION_TESTS").is_some()
                && shell_supports_process_tests()
        })
        .with_step(
            TestStep::new("Remember the original window")
                .add_assertion(save_active_window_id(SOURCE_WINDOW_KEY)),
        )
        .with_step(record_processes(SEPARATE_TABS))
        .with_step(
            TestStep::new("Activate the native window before entering fullscreen")
                .with_action(|app, _, data| {
                    let window = saved_window(data, SOURCE_WINDOW_KEY);
                    app.update(|ctx| ctx.windows().show_window_and_focus_app(window));
                })
                .set_post_step_pause(Duration::from_millis(500)),
        )
        .with_step(toggle_saved_window_fullscreen(
            "Enter native fullscreen",
            FullscreenState::Fullscreen,
        ))
        .with_steps(
            open_command_palette_and_run_action("Keyboard Docking").add_named_assertion(
                "Keyboard Docking opens in fullscreen",
                |app, window| {
                    async_assert!(saved_bounds(app, window, "pane_navigator:search").is_some())
                },
            ),
        )
        .with_step(
            TestStep::new("Find the second tab as a destination").with_typed_characters(&["Tab 2"]),
        )
        .with_step(TestStep::new("Choose the destination pane").with_keystrokes(&["enter"]))
        .with_step(
            TestStep::new("Find the right split placement").with_typed_characters(&["Right"]),
        )
        .with_step(
            TestStep::new("Preview the right split in fullscreen")
                .with_keystrokes(&["enter"])
                .add_named_assertion("The split preview is visible", |app, window| {
                    async_assert!(saved_bounds(app, window, "pane_navigator:preview").is_some())
                })
                .add_named_assertion_with_data_from_prior_step(
                    "Preview leaves both live terminals in their tabs",
                    |app, _, data| {
                        let window = saved_window(data, SOURCE_WINDOW_KEY);
                        assert_processes_preserved(app, window, data, SEPARATE_TABS)
                    },
                ),
        )
        .with_step(
            TestStep::new("Dock the first pane on the right")
                .with_keystrokes(&["enter"])
                .add_named_assertion_with_data_from_prior_step(
                    "The original window stays fullscreen",
                    |app, _, data| {
                        let window = saved_window(data, SOURCE_WINDOW_KEY);
                        assert_window_fullscreen(app, window, FullscreenState::Fullscreen)
                    },
                )
                .add_named_assertion_with_data_from_prior_step(
                    "Both original processes are in the split",
                    |app, _, data| {
                        let window = saved_window(data, SOURCE_WINDOW_KEY);
                        assert_processes_preserved(app, window, data, RIGHT_SPLIT)
                    },
                ),
        )
        .with_step(toggle_saved_window_fullscreen(
            "Leave native fullscreen",
            FullscreenState::Normal,
        ))
        .with_step(
            TestStep::new("Verify the docked processes after leaving fullscreen")
                .add_named_assertion_with_data_from_prior_step(
                    "Both original processes remain in the split",
                    |app, _, data| {
                        let window = saved_window(data, SOURCE_WINDOW_KEY);
                        assert_processes_preserved(app, window, data, RIGHT_SPLIT)
                    },
                ),
        )
}

fn toggle_saved_window_fullscreen(name: &str, expected: FullscreenState) -> TestStep {
    TestStep::new(name)
        .with_action(|app, _, data| {
            let window = saved_window(data, SOURCE_WINDOW_KEY);
            workspace_view(app, window).update(app, |_, ctx| ctx.toggle_fullscreen());
        })
        .add_named_assertion_with_data_from_prior_step(
            "The original window completed the native fullscreen transition",
            move |app, _, data| {
                let window = saved_window(data, SOURCE_WINDOW_KEY);
                assert_window_fullscreen(app, window, expected)
            },
        )
}

fn assert_window_fullscreen(
    app: &App,
    window: WindowId,
    expected: FullscreenState,
) -> AssertionOutcome {
    let (active, state) = app.read(|ctx| {
        let windows = ctx.windows();
        (
            windows.active_window(),
            windows
                .platform_window(window)
                .map(|window| window.fullscreen_state()),
        )
    });
    async_assert!(
        active == Some(window) && state == Some(expected),
        "Expected {window:?} in {expected:?}, active window was {active:?} and state was {state:?}"
    )
}
