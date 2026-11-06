use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use uuid::Uuid;
use warp_terminal::SizeInfo;
use warp_terminal::local_tty::persistent::{Runtime, Session, SessionState};
use warpui::modals::{AlertDialogWithCallbacks, ModalButton};
use warpui::{SingletonEntity, ViewContext, ViewHandle, WeakViewHandle};

use super::{NewSessionMenuAnchor, Workspace};
use crate::menu::{MenuItem, MenuItemFields};
use crate::pane_group::pane::PaneId;
use crate::pane_group::{PaneGroup, PanesLayout, PersistentTerminalLayout};
use crate::persistent_terminals::runtime_dir;
use crate::terminal::local_tty::PersistentPtyConnection;
use crate::view_components::DismissibleToast;
use crate::workspace::registry::WorkspaceRegistry;
use crate::workspace::{PersistentTerminalAction, WorkspaceAction};

impl Workspace {
    pub(super) fn add_persistent_terminal_menu_items(
        &self,
        items: &mut Vec<MenuItem<WorkspaceAction>>,
        ctx: &ViewContext<Self>,
    ) {
        if runtime_dir().is_none() {
            return;
        }
        items.push(MenuItem::Separator);
        items.push(menu_item(
            "New Persistent Terminal",
            PersistentTerminalAction::Create { split: false },
        ));
        if self.tabs.get(self.active_tab_index).is_some() {
            items.push(menu_item(
                "New Persistent Terminal Split",
                PersistentTerminalAction::Create { split: true },
            ));
        }
        if self.focused_persistent_terminal(ctx).is_some() {
            items.push(menu_item(
                "Detach Persistent Terminal",
                PersistentTerminalAction::Detach,
            ));
            items.push(menu_item(
                "Rename Persistent Terminal",
                PersistentTerminalAction::Rename,
            ));
        }
    }

    pub(super) fn discover_persistent_terminals(
        &self,
        anchor: NewSessionMenuAnchor,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(path) = runtime_dir() else { return };
        ctx.spawn(
            persistent_request(move || Runtime::new(path)?.list()),
            move |workspace, result, ctx| {
                if workspace
                    .show_new_session_dropdown_menu
                    .is_none_or(|open| open.position() != anchor.position())
                {
                    return;
                }
                let mut items = workspace.unified_new_session_menu_items(ctx);
                workspace.add_persistent_terminal_menu_items(&mut items, ctx);
                match result {
                    Ok(sessions) => items.extend(sessions.into_iter().flat_map(session_menu_items)),
                    Err(err) => items.push(
                        MenuItemFields::new(format!("Persistent terminals unavailable: {err}"))
                            .with_disabled(true)
                            .into_item(),
                    ),
                }
                workspace
                    .new_session_dropdown_menu
                    .update(ctx, |menu, ctx| {
                        menu.set_items(items, ctx);
                    });
            },
        );
    }

    pub(super) fn handle_persistent_terminal_action(
        &mut self,
        action: &PersistentTerminalAction,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(path) = runtime_dir() else { return };
        match *action {
            PersistentTerminalAction::Create { split } => {
                let group = self
                    .tabs
                    .get(self.active_tab_index)
                    .map(|tab| &tab.pane_group);
                let split_group = group.filter(|_| split).map(ViewHandle::downgrade);
                let directory = self
                    .active_session_view(ctx)
                    .and_then(|view| view.as_ref(ctx).pwd_if_local(ctx))
                    .map(PathBuf::from);
                let shell = std::env::var_os("SHELL")
                    .map_or_else(|| PathBuf::from("/bin/zsh"), PathBuf::from);
                self.open_persistent_terminal(
                    path,
                    move |runtime| {
                        runtime.create(
                            &shell,
                            directory.as_deref(),
                            None,
                            &SizeInfo::new_without_font_metrics(24, 80),
                        )
                    },
                    split_group,
                    ctx,
                );
            }
            PersistentTerminalAction::Attach(id) => {
                self.open_persistent_terminal(path, move |runtime| runtime.status(id), None, ctx);
            }
            PersistentTerminalAction::Detach => {
                if let Some((group, pane, _)) = self.focused_persistent_terminal(ctx) {
                    self.close_pane(group.id(), pane, ctx);
                    ctx.dispatch_global_action("workspace:save_app", ());
                }
            }
            PersistentTerminalAction::Rename => {
                if let Some((_, _, id)) = self.focused_persistent_terminal(ctx) {
                    self.rename_tab(self.active_tab_index, ctx);
                    self.persistent_terminal_being_renamed = Some(id);
                }
            }
            PersistentTerminalAction::ConfirmTermination(id) => {
                ctx.show_native_platform_modal(AlertDialogWithCallbacks::for_view(
                    "Terminate this persistent terminal?",
                    "The terminal shell and its foreground job will be stopped.",
                    vec![
                        ModalButton::for_view("Cancel", |_, _| {}),
                        ModalButton::for_view("Terminate", move |workspace: &mut Self, ctx| {
                            workspace.terminate_persistent_terminal(path, id, ctx);
                        }),
                    ],
                    |_, _| {},
                ));
            }
        }
    }

    fn open_persistent_terminal(
        &mut self,
        path: PathBuf,
        session: impl FnOnce(&Runtime) -> Result<Session> + Send + 'static,
        split_group: Option<WeakViewHandle<PaneGroup>>,
        ctx: &mut ViewContext<Self>,
    ) {
        ctx.spawn(
            persistent_request(move || {
                let runtime = Runtime::new(path)?;
                let session = session(&runtime)?;
                let attachment = runtime.attach(session.id)?;
                let title = session_label(&session);
                let truncated = attachment.truncated;
                let layout = PersistentTerminalLayout {
                    id: session.id,
                    connection: PersistentPtyConnection::Attached(runtime, attachment),
                };
                Ok((layout, title, truncated))
            }),
            move |workspace, result, ctx| {
                let (layout, title, truncated) = match result {
                    Ok(value) => value,
                    Err(err) => {
                        workspace.persistent_terminal_error(err, ctx);
                        return;
                    }
                };
                if truncated {
                    workspace.toast_stack.update(ctx, |stack, ctx| {
                        stack.add_ephemeral_toast(
                            DismissibleToast::default(
                                "Earlier terminal output was discarded.".into(),
                            ),
                            ctx,
                        );
                    });
                }
                if let Some(group) = split_group.and_then(|group| group.upgrade(ctx)) {
                    group.update(ctx, |group, ctx| {
                        group.attach_persistent_terminal(layout, None, ctx)
                    });
                } else {
                    workspace.show_persistent_terminal(layout, title, ctx);
                }
                ctx.dispatch_global_action("workspace:save_app", ());
            },
        );
    }

    fn show_persistent_terminal(
        &mut self,
        layout: PersistentTerminalLayout,
        title: String,
        ctx: &mut ViewContext<Self>,
    ) {
        if let Some((tab, pane)) = self.find_persistent_terminal(layout.id, ctx) {
            self.replace_persistent_terminal(tab, pane, layout, ctx);
            return;
        }
        let existing = WorkspaceRegistry::as_ref(ctx)
            .all_workspaces(ctx)
            .into_iter()
            .filter(|(id, _)| *id != ctx.window_id())
            .find_map(|(_, workspace)| {
                let (tab, pane) = workspace
                    .as_ref(ctx)
                    .find_persistent_terminal(layout.id, ctx)?;
                Some((workspace, tab, pane))
            });
        if let Some((workspace, tab, pane)) = existing {
            workspace.update(ctx, |workspace, ctx| {
                workspace.replace_persistent_terminal(tab, pane, layout, ctx);
            });
            return;
        }
        self.add_tab_with_pane_layout(
            PanesLayout::PersistentTerminal(Box::new(layout)),
            Arc::default(),
            Some(title),
            ctx,
        );
    }

    fn find_persistent_terminal(
        &self,
        id: Uuid,
        ctx: &ViewContext<Self>,
    ) -> Option<(usize, PaneId)> {
        self.tabs.iter().enumerate().find_map(|(tab, data)| {
            let group = data.pane_group.as_ref(ctx);
            let pane = group.find_terminal_pane_by_session_uuid(id.as_bytes())?;
            (group.persistent_session_id(pane) == Some(id)).then_some((tab, pane))
        })
    }

    fn replace_persistent_terminal(
        &mut self,
        tab: usize,
        pane: PaneId,
        layout: PersistentTerminalLayout,
        ctx: &mut ViewContext<Self>,
    ) {
        self.activate_tab(tab, ctx);
        self.tabs[tab].pane_group.update(ctx, |group, ctx| {
            group.attach_persistent_terminal(layout, Some(pane), ctx);
        });
    }

    fn focused_persistent_terminal(
        &self,
        ctx: &ViewContext<Self>,
    ) -> Option<(ViewHandle<PaneGroup>, PaneId, Uuid)> {
        let group = &self.tabs.get(self.active_tab_index)?.pane_group;
        let pane_group = group.as_ref(ctx);
        let pane = pane_group.focused_pane_id(ctx);
        let id = pane_group.persistent_session_id(pane)?;
        Some((group.clone(), pane, id))
    }

    pub(super) fn rename_persistent_terminal(
        &self,
        group: &ViewHandle<PaneGroup>,
        id: Uuid,
        name: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(path) = runtime_dir() else { return };
        let group = group.downgrade();
        let name = name.to_owned();
        ctx.spawn(
            persistent_request(move || Runtime::new(path)?.rename(id, Some(name))),
            move |workspace, result, ctx| match result {
                Ok(session) => {
                    if let Some(group) = group.upgrade(ctx) {
                        group.update(ctx, |group, ctx| {
                            if group.pane_count() == 1 {
                                group.set_title(&session_label(&session), ctx);
                            }
                        });
                        ctx.dispatch_global_action("workspace:save_app", ());
                    }
                }
                Err(err) => workspace.persistent_terminal_error(err, ctx),
            },
        );
    }

    fn terminate_persistent_terminal(&self, path: PathBuf, id: Uuid, ctx: &mut ViewContext<Self>) {
        ctx.spawn(
            persistent_request(move || Runtime::new(path)?.terminate(id)),
            |workspace, result, ctx| match result {
                Ok(()) => workspace.toast_stack.update(ctx, |stack, ctx| {
                    stack.add_ephemeral_toast(
                        DismissibleToast::success("Persistent terminal terminated.".into()),
                        ctx,
                    );
                }),
                Err(err) => workspace.persistent_terminal_error(err, ctx),
            },
        );
    }

    fn persistent_terminal_error(&self, err: anyhow::Error, ctx: &mut ViewContext<Self>) {
        self.toast_stack.update(ctx, |stack, ctx| {
            stack.add_ephemeral_toast(DismissibleToast::error(err.to_string()), ctx);
        });
    }
}

async fn persistent_request<T: Send + 'static>(
    request: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(request)
        .await
        .context("Persistent terminal request failed")?
}

fn session_label(session: &Session) -> String {
    session.name.clone().unwrap_or_else(|| {
        let id = session.id.simple().to_string();
        format!("Persistent Terminal {}", &id[..8])
    })
}

fn session_menu_items(session: Session) -> Vec<MenuItem<WorkspaceAction>> {
    let name = session_label(&session);
    let state = match session.state {
        SessionState::Attached => "attached",
        SessionState::Detached => "detached",
        SessionState::Terminated => "terminated",
        SessionState::Failed => "unavailable",
    };
    let mut items = vec![if session.state == SessionState::Detached {
        menu_item(
            format!("Attach to {name}"),
            PersistentTerminalAction::Attach(session.id),
        )
    } else {
        MenuItemFields::new(format!("{name} ({state})"))
            .with_disabled(true)
            .into_item()
    }];
    if matches!(
        session.state,
        SessionState::Attached | SessionState::Detached
    ) {
        items.push(menu_item(
            format!("Terminate {name}…"),
            PersistentTerminalAction::ConfirmTermination(session.id),
        ));
    }
    items
}

fn menu_item(
    label: impl Into<String>,
    action: PersistentTerminalAction,
) -> MenuItem<WorkspaceAction> {
    MenuItemFields::new(label)
        .with_on_select_action(WorkspaceAction::PersistentTerminal(action))
        .into_item()
}
