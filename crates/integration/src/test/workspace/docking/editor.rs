use std::path::PathBuf;

use warp::features::FeatureFlag;
use warp::integration_testing::command_palette::{
    TestStepsExt, open_command_palette_and_run_action,
};
use warp::integration_testing::pane_group::assert_num_panes_in_tab;
use warp::integration_testing::terminal::wait_until_bootstrapped_single_pane_for_tab;
use warp::integration_testing::view_getters::{pane_group_view, workspace_view};
use warp::integration_testing::workspace::assert_tab_count;
use warp::workspace::WorkspaceAction;
use warpui_core::integration::{AssertionOutcome, TestStep};
use warpui_core::{App, EntityId, TypedActionView, WindowId, async_assert, async_assert_eq};

use super::history::UNDO_LAYOUT;
use super::saved_bounds;
use crate::Builder;

const EDITOR_PATH: &str = "WARP_DOCKING_TEST_EDITOR";
const EDITOR_VIEW: &str = "original editor view";
const ORIGINAL: &str = "original file contents";
const UNSAVED: &str = "edited in memory before docking";

fn editor_id(app: &App, window: WindowId, tab: usize) -> Option<EntityId> {
    let group = pane_group_view(app, window, tab);
    group.read(app, |group, ctx| group.code_views(ctx).first().map(|view| view.id()))
}

fn test_path() -> PathBuf {
    std::env::var_os(EDITOR_PATH).expect("Test file path is set").into()
}

fn assert_dirty_editor(tab: usize) -> TestStep {
    let select_all = if cfg!(target_os = "macos") { "cmd-a" } else { "ctrl-a" };
    TestStep::new("Read the live editor and its on-disk file")
        .with_keystrokes(&[select_all])
        .add_named_assertion_with_data_from_prior_step(
            "The same editor view still owns the unsaved buffer",
            move |app, window, data| {
                let original = data.get::<_, EntityId>(EDITOR_VIEW).copied();
                let group = pane_group_view(app, window, tab);
                group.read(app, |group, ctx| {
                    let views = group.code_views(ctx);
                    let Some(view) = views.first() else {
                        return AssertionOutcome::failure("The editor is unavailable".to_owned());
                    };
                    let editor = view.as_ref(ctx);
                    async_assert!(
                        Some(view.id()) == original
                            && editor.selected_text(ctx).as_deref() == Some(UNSAVED)
                            && editor.active_tab_has_unsaved_changes(ctx),
                        "Expected the original dirty editor, selected text was {:?}",
                        editor.selected_text(ctx)
                    )
                })
            },
        )
        .add_named_assertion("The file has its original on-disk contents", |_, _| {
            let saved = std::fs::read_to_string(test_path()).expect("Read the fixture file");
            async_assert_eq!(saved, ORIGINAL)
        })
}

pub fn test_dock_editor_preserves_unsaved_buffer() -> Builder {
    FeatureFlag::EnhancedPaneManagement.set_enabled(true);
    let select_all = if cfg!(target_os = "macos") { "cmd-a" } else { "ctrl-a" };
    Builder::new()
        .with_setup(|utils| {
            let path = utils.test_dir().join("docking-editor.txt");
            std::fs::write(&path, ORIGINAL).expect("Write the editor fixture");
            utils.set_env(EDITOR_PATH, Some(path));
        })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(
            TestStep::new("Open a local file in a new editor tab")
                .with_action(|app, window, _| {
                    workspace_view(app, window).update(app, |workspace, ctx| {
                        workspace.handle_action(&WorkspaceAction::OpenFileInNewTab {
                            full_path: test_path(),
                            line_and_column: None,
                        }, ctx);
                    });
                })
                .add_assertion(assert_tab_count(2))
                .add_named_assertion_with_data_from_prior_step(
                    "Remember the existing editor view",
                    |app, window, data| {
                        let Some(view) = editor_id(app, window, 1) else {
                            return AssertionOutcome::failure("The editor is opening".to_owned());
                        };
                        data.insert(EDITOR_VIEW, view);
                        AssertionOutcome::Success
                    },
                ),
        )
        .with_step(
            TestStep::new("Edit without saving")
                .with_keystrokes(&[select_all])
                .with_typed_characters(&[UNSAVED]),
        )
        .with_step(assert_dirty_editor(1))
        .with_steps(
            open_command_palette_and_run_action("Keyboard Docking").add_named_assertion(
                "The docking destination search is visible",
                |app, window| async_assert!(saved_bounds(app, window, "pane_navigator:search").is_some()),
            ),
        )
        .with_step(TestStep::new("Find the terminal tab").with_typed_characters(&["Tab 1"]))
        .with_step(TestStep::new("Choose the terminal as the split destination").with_keystrokes(&["enter"]))
        .with_step(TestStep::new("Search for the right split").with_typed_characters(&["Right"]))
        .with_step(
            TestStep::new("Preview a split with the live editor")
                .with_keystrokes(&["enter"])
                .add_named_assertion("The editor move is ready to confirm", |app, window| {
                    async_assert!(saved_bounds(app, window, "Move Panes").is_some())
                })
                .add_assertion(assert_tab_count(2)),
        )
        .with_step(
            TestStep::new("Dock the live editor beside the terminal")
                .with_keystrokes(&["enter"])
                .add_assertion(assert_tab_count(1))
                .add_assertion(assert_num_panes_in_tab(0, 2)),
        )
        .with_step(assert_dirty_editor(0))
        .with_steps(open_command_palette_and_run_action(UNDO_LAYOUT))
        .with_step(
            TestStep::new("Undo restores the editor tab")
                .add_assertion(assert_tab_count(2)),
        )
        .with_step(assert_dirty_editor(1))
}
