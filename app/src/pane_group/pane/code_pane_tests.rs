use tempfile::TempDir;
use warpui::{App, SingletonEntity};

use super::{CodeManager, CodePane, CodeSource, LocalOrRemotePath, PaneContent};
use crate::code::global_buffer_model::GlobalBufferModel;
use crate::pane_group::Direction;
use crate::terminal::local_shell::LocalShellState;
use crate::workspace::PaneViewLocator;
use crate::workspace::view::tests::{initialize_app, mock_workspace};

#[test]
fn moving_an_editor_relocates_every_file_tab() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.add_singleton_model(|_| CodeManager::default());
        app.add_singleton_model(|_| LocalShellState::NotLoaded);
        app.add_singleton_model(GlobalBufferModel::new);
        let workspace = mock_workspace(&mut app);
        let dir = TempDir::new().expect("test directory");
        let first_path = dir.path().join("first.txt");
        let second_path = dir.path().join("second.txt");
        std::fs::write(&first_path, "first file").expect("create first file");
        std::fs::write(&second_path, "second file").expect("create second file");
        let first = LocalOrRemotePath::Local(first_path);
        let second = LocalOrRemotePath::Local(second_path);

        let (original_group, pane_id, editor) = workspace.update(&mut app, |workspace, ctx| {
            let group = workspace.active_tab_pane_group().clone();
            let (pane_id, editor) = group.update(ctx, |group, ctx| {
                let pane = CodePane::new(
                    CodeSource::FileTree {
                        location: first.clone(),
                    },
                    None,
                    ctx,
                );
                let pane_id = pane.id();
                let editor = pane.file_view(ctx);
                group.add_pane_with_direction(Direction::Right, pane, true, ctx);
                (pane_id, editor)
            });
            (group, pane_id, editor)
        });
        editor.update(&mut app, |editor, ctx| {
            editor.open_or_focus_existing(Some(second.clone()), None, ctx);
        });

        let original = PaneViewLocator {
            pane_group_id: original_group.id(),
            pane_id,
        };
        app.read(|ctx| {
            let manager = CodeManager::as_ref(ctx);
            assert_eq!(
                manager.get_locator_for_location_in_tab(original.pane_group_id, &first, ctx),
                Some(original)
            );
            assert_eq!(
                manager.get_locator_for_location_in_tab(original.pane_group_id, &second, ctx),
                Some(original)
            );
        });

        let moved = workspace.update(&mut app, |workspace, ctx| {
            let pane = original_group
                .update(ctx, |group, ctx| group.remove_pane_for_move(&pane_id, ctx))
                .expect("move existing editor pane");
            workspace.add_tab_from_existing_pane(pane, workspace.tab_count(), None, ctx);
            PaneViewLocator {
                pane_group_id: workspace.active_tab_pane_group().id(),
                pane_id,
            }
        });

        workspace.read(&app, |workspace, ctx| {
            let manager = CodeManager::as_ref(ctx);
            let moved_editor = workspace
                .active_tab_pane_group()
                .as_ref(ctx)
                .code_view_from_pane_id(pane_id, ctx)
                .expect("editor was attached to the destination tab");
            assert_eq!(moved_editor.id(), editor.id());
            assert_eq!(
                manager.get_locator_for_location_in_tab(original.pane_group_id, &first, ctx),
                None
            );
            assert_eq!(
                manager.get_locator_for_location_in_tab(moved.pane_group_id, &first, ctx),
                Some(moved)
            );
            assert_eq!(
                manager.get_locator_for_location_in_tab(moved.pane_group_id, &second, ctx),
                Some(moved)
            );
        });
    });
}
