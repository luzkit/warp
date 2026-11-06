use warpui::App;

use super::*;
use crate::terminal::model::session::SessionInfo;
use crate::terminal::model_events::SshRemoteServerSupport;

#[test]
fn persistent_replay_running_command_cancels_pending_editor_activation() {
    App::test((), |mut app| async move {
        let sessions = app.add_model(|_| {
            let mut sessions = Sessions::new_for_test();
            sessions.register_session_for_test(SessionInfo::new_for_test());
            sessions
        });
        let (_events_tx, events_rx) = async_channel::unbounded();
        let dispatcher = app.add_model(|ctx| {
            let mut dispatcher = ModelEventDispatcher::new_with_ssh_remote_server_support(
                events_rx,
                sessions.clone(),
                SshRemoteServerSupport::Disabled,
                ctx,
            );
            dispatcher.set_active_session_id(0.into());
            dispatcher
        });
        let line_editor = app.add_model(|ctx| LineEditorStatus::new(dispatcher, sessions, ctx));

        let activation = line_editor.update(&mut app, |editor, ctx| {
            editor.handle_model_event(
                &ModelEvent::PersistentReplayFinished { at_prompt: true },
                ctx,
            );
            let activation = ctx.await_spawned_future(
                editor
                    .mark_line_editor_active_abort_handle
                    .as_ref()
                    .unwrap()
                    .future_id(),
            );
            editor.handle_model_event(
                &ModelEvent::PersistentReplayFinished { at_prompt: false },
                ctx,
            );
            activation
        });
        activation.await;

        line_editor.read(&app, |editor, _| {
            assert!(!editor.is_line_editor_active());
            assert!(editor.mark_line_editor_active_abort_handle.is_none());
        });
    });
}
