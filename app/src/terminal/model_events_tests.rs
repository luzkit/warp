use warpui::App;

use super::*;
use crate::terminal::model::session::SessionsEvent;
use crate::test_util::terminal::initialize_app_for_terminal_view;

#[derive(Default)]
struct ReplayEvents {
    bootstrap_events: usize,
    restored_events: usize,
    replay_finished_events: usize,
}

impl Entity for ReplayEvents {
    type Event = ();
}

#[test]
fn persistent_replay_hydrates_sessions_without_bootstrap_actions() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let sessions = app.add_model(|_| Sessions::new_for_test());
        let (_events_tx, events_rx) = async_channel::unbounded();
        let dispatcher = app.add_model(|ctx| {
            ModelEventDispatcher::new_with_ssh_remote_server_support(
                events_rx,
                sessions.clone(),
                SshRemoteServerSupport::Disabled,
                ctx,
            )
        });
        let events =
            app.add_model(|ctx| {
                ctx.subscribe_to_model(&dispatcher, |events: &mut ReplayEvents, _, event, _| {
                    match event {
                        ModelEvent::Handler(AnsiHandlerEvent::Bootstrapped { .. }) => {
                            events.bootstrap_events += 1;
                        }
                        ModelEvent::PersistentReplayFinished { .. } => {
                            events.replay_finished_events += 1;
                        }
                        _ => {}
                    }
                });
                ctx.subscribe_to_model(&sessions, |events: &mut ReplayEvents, _, event, _| {
                    match event {
                        SessionsEvent::SessionBootstrapped(_) => events.bootstrap_events += 1,
                        SessionsEvent::SessionRestored { .. } => events.restored_events += 1,
                        SessionsEvent::SessionInitialized { .. }
                        | SessionsEvent::EnvironmentVariablesUpdated { .. } => {}
                    }
                });
                ReplayEvents::default()
            });

        dispatcher.update(&mut app, |dispatcher, ctx| {
            dispatcher.handle_terminal_model_event(Event::PersistentReplayStarted, ctx);
            dispatcher.handle_terminal_model_event(
                Event::Handler(HandlerEvent::Bootstrapped(BootstrappedEvent {
                    session_info: Box::new(SessionInfo::new_for_test()),
                    spawning_command: String::new(),
                    restored_block_commands: Vec::new(),
                    rcfiles_duration_seconds: None,
                })),
                ctx,
            );
            dispatcher.handle_terminal_model_event(
                Event::PersistentReplayFinished {
                    at_prompt: false,
                    active_metadata: None,
                },
                ctx,
            );
        });

        sessions.read(&app, |sessions, _| {
            assert!(sessions.get(SessionId::from(0)).is_some());
        });
        events.read(&app, |events, _| {
            assert_eq!(events.bootstrap_events, 0);
            assert_eq!(events.restored_events, 1);
            assert_eq!(events.replay_finished_events, 1);
        });
    });
}

#[test]
fn enabled_support_uses_remote_server_for_ssh_wrapper_when_feature_is_enabled() {
    assert!(SshRemoteServerSupport::Enabled.should_use_remote_server(true, true,));
}

#[test]
fn disabled_support_skips_remote_server_for_ssh_wrapper() {
    assert!(!SshRemoteServerSupport::Disabled.should_use_remote_server(true, true,));
}

#[test]
fn enabled_support_skips_remote_server_when_feature_is_disabled() {
    assert!(!SshRemoteServerSupport::Enabled.should_use_remote_server(false, true,));
}

#[test]
fn enabled_support_skips_remote_server_for_non_ssh_session() {
    assert!(!SshRemoteServerSupport::Enabled.should_use_remote_server(true, false,));
}
