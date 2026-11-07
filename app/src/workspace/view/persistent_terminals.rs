use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use settings::Setting as _;
use uuid::Uuid;
use warp_terminal::local_tty::persistent::Runtime;
use warpui::modals::{AlertDialogWithCallbacks, ModalButton};
use warpui::{AppContext, SingletonEntity, ViewContext, ViewHandle};

use super::{DefaultSessionModeBehavior, NewSessionSource, Workspace};
use crate::palette::PaletteMode;
use crate::pane_group::pane::PaneId;
use crate::pane_group::{PaneGroup, PanesLayout, PersistentTerminalLayout};
use crate::persistent_terminals::runtime_dir;
use crate::search::data_source::QueryFilter;
use crate::server::telemetry::PaletteSource;
use crate::tab::uses_vertical_tabs;
use crate::terminal::local_tty::PersistentPtyConnection;
use crate::view_components::DismissibleToast;
use crate::workspace::registry::WorkspaceRegistry;
use crate::workspace::tab_settings::{TabSettings, VerticalTabsDisplayGranularity};
use crate::workspace::{PaneViewLocator, PersistentTerminalAction};

impl Workspace {
    pub(super) fn handle_persistent_terminal_action(
        &mut self,
        action: &PersistentTerminalAction,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(path) = runtime_dir() else { return };
        match *action {
            PersistentTerminalAction::OpenAttachPicker => {
                self.open_palette_action(
                    PaletteMode::Command,
                    PaletteSource::Keybinding,
                    None,
                    ctx,
                );
                self.palette.update(ctx, |palette, ctx| {
                    palette.set_active_query_filter(QueryFilter::DetachedSessions, ctx);
                });
            }
            PersistentTerminalAction::Attach(id) => self.open_persistent_terminal(path, id, ctx),
            PersistentTerminalAction::Detach(locator) => {
                let Some(locator) = locator.or_else(|| self.focused_persistent_terminal(ctx))
                else {
                    return;
                };
                let Some(id) = self.retainable_persistent_session(locator, ctx) else {
                    return;
                };
                ctx.spawn(
                    persistent_request(move || Runtime::new(path)?.retain(id)),
                    move |workspace, result, ctx| match result {
                        Ok(()) => {
                            if let Some(group) =
                                workspace.prepare_persistent_pane_close(locator, id, ctx)
                            {
                                group.update(ctx, |group, ctx| {
                                    group.detach_persistent_session(locator.pane_id, id, ctx);
                                });
                                ctx.dispatch_global_action("workspace:save_app", ());
                            }
                        }
                        Err(err) => workspace.persistent_terminal_error(err, ctx),
                    },
                );
            }
            PersistentTerminalAction::Rename(locator) => {
                let Some(id) = self.attached_persistent_session(locator, ctx) else {
                    return;
                };
                self.edit_persistent_session_name(locator, id, ctx);
            }
            PersistentTerminalAction::ConfirmTermination(locator) => {
                let Some(id) = self.attached_persistent_session(locator, ctx) else {
                    return;
                };
                ctx.show_native_platform_modal(AlertDialogWithCallbacks::for_view(
                    "End this terminal session?",
                    "The shell and its foreground job will stop.",
                    vec![
                        ModalButton::for_view("Cancel", |_, _| {}),
                        ModalButton::for_view("End Session", move |workspace: &mut Self, ctx| {
                            if workspace.attached_persistent_session(locator, ctx) == Some(id) {
                                workspace.terminate_persistent_terminal(path, locator, id, ctx);
                            }
                        }),
                    ],
                    |_, _| {},
                ));
            }
        }
    }

    fn open_persistent_terminal(&mut self, path: PathBuf, id: Uuid, ctx: &mut ViewContext<Self>) {
        ctx.spawn(
            persistent_request(move || {
                let runtime = Runtime::new(path)?;
                let session = runtime.status(id)?;
                let attachment = runtime.attach(session.id)?;
                let replay_gap = attachment.replay_gap;
                let layout = PersistentTerminalLayout {
                    id: session.id,
                    name: session.name,
                    snapshot: None,
                    live_conversations: Vec::new(),
                    connection: PersistentPtyConnection::Attached(runtime, attachment),
                };
                Ok((layout, replay_gap))
            }),
            move |workspace, result, ctx| {
                let (layout, replay_gap) = match result {
                    Ok(value) => value,
                    Err(err) => {
                        workspace.persistent_terminal_error(err, ctx);
                        return;
                    }
                };
                if replay_gap {
                    workspace.toast_stack.update(ctx, |stack, ctx| {
                        stack.add_ephemeral_toast(
                            DismissibleToast::default(
                                "Earlier terminal output was discarded.".into(),
                            ),
                            ctx,
                        );
                    });
                }
                workspace.show_persistent_terminal(layout, ctx);
                ctx.dispatch_global_action("workspace:save_app", ());
            },
        );
    }

    fn show_persistent_terminal(
        &mut self,
        layout: PersistentTerminalLayout,
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
        let tab_title = layout.name.clone();
        self.add_tab_with_pane_layout(
            PanesLayout::PersistentTerminal(Box::new(layout)),
            Arc::default(),
            tab_title,
            ctx,
        );
    }

    fn find_persistent_terminal(&self, id: Uuid, ctx: &AppContext) -> Option<(usize, PaneId)> {
        self.tabs.iter().enumerate().find_map(|(tab, data)| {
            let pane = data
                .pane_group
                .as_ref(ctx)
                .find_persistent_session(id, ctx)?;
            Some((tab, pane))
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
            group.attach_persistent_terminal(layout, pane, ctx);
        });
    }

    pub(super) fn focused_persistent_terminal(&self, ctx: &AppContext) -> Option<PaneViewLocator> {
        let group = &self.tabs.get(self.active_tab_index)?.pane_group;
        let pane_group = group.as_ref(ctx);
        let pane_id = pane_group.focused_pane_id(ctx);
        if !pane_group.persistent_session_is_attached(pane_id, ctx) {
            return None;
        }
        let terminal = pane_group.terminal_view_from_pane_id(pane_id, ctx)?;
        let focused = ctx.focused_view_id(self.window_id)?;
        ctx.view_ancestors(self.window_id, focused)
            .contains(&terminal.id())
            .then_some(PaneViewLocator {
                pane_group_id: group.id(),
                pane_id,
            })
    }

    fn retainable_persistent_session(
        &self,
        locator: PaneViewLocator,
        ctx: &AppContext,
    ) -> Option<Uuid> {
        let group = self
            .get_pane_group_view_with_id(locator.pane_group_id)?
            .as_ref(ctx);
        if !group.persistent_session_can_retain(locator.pane_id, ctx) {
            return None;
        }
        group.persistent_session_id(locator.pane_id, ctx)
    }

    fn attached_persistent_session(
        &self,
        locator: PaneViewLocator,
        ctx: &AppContext,
    ) -> Option<Uuid> {
        let group = self
            .get_pane_group_view_with_id(locator.pane_group_id)?
            .as_ref(ctx);
        if !group.persistent_session_is_attached(locator.pane_id, ctx) {
            return None;
        }
        group.persistent_session_id(locator.pane_id, ctx)
    }

    fn edit_persistent_session_name(
        &mut self,
        locator: PaneViewLocator,
        id: Uuid,
        ctx: &mut ViewContext<Self>,
    ) {
        if uses_vertical_tabs(ctx)
            && self.vertical_tabs_panel_open
            && matches!(
                *TabSettings::as_ref(ctx)
                    .vertical_tabs_display_granularity
                    .value(),
                VerticalTabsDisplayGranularity::Panes
            )
        {
            self.rename_pane(locator, ctx);
        } else {
            let Some(index) = self
                .tabs
                .iter()
                .position(|tab| tab.pane_group.id() == locator.pane_group_id)
            else {
                return;
            };
            let group = self.tabs[index].pane_group.as_ref(ctx);
            let Some(pane) = group.pane_by_id(locator.pane_id) else {
                return;
            };
            let configuration = pane.pane_configuration();
            let configuration = configuration.as_ref(ctx);
            let name = configuration
                .custom_vertical_tabs_title()
                .unwrap_or_else(|| configuration.title())
                .to_owned();
            self.rename_tab_internal(index, &name, ctx);
        }
        self.persistent_terminal_being_renamed = Some((locator, id));
    }

    pub(super) fn rename_persistent_terminal(
        &self,
        locator: PaneViewLocator,
        id: Uuid,
        name: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(path) = runtime_dir() else { return };
        if self.attached_persistent_session(locator, ctx) != Some(id) {
            return;
        }
        let name = name.to_owned();
        ctx.spawn(
            persistent_request(move || Runtime::new(path)?.rename(id, Some(name))),
            move |workspace, result, ctx| match result {
                Ok(session) => {
                    if let Some(name) = session.name
                        && workspace.attached_persistent_session(locator, ctx) == Some(id)
                        && let Some(group) =
                            workspace.get_pane_group_view_with_id(locator.pane_group_id)
                    {
                        if group.as_ref(ctx).pane_count() == 1 {
                            group.update(ctx, |group, ctx| group.set_title(&name, ctx));
                        }
                        workspace.set_custom_pane_name(locator, name, ctx);
                        ctx.dispatch_global_action("workspace:save_app", ());
                    }
                }
                Err(err) => workspace.persistent_terminal_error(err, ctx),
            },
        );
    }

    fn terminate_persistent_terminal(
        &self,
        path: PathBuf,
        locator: PaneViewLocator,
        id: Uuid,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(group) = self.get_pane_group_view_with_id(locator.pane_group_id) else {
            return;
        };
        if !group.update(ctx, |group, ctx| {
            group.set_persistent_session_ending(locator.pane_id, id, true, ctx)
        }) {
            return;
        }
        ctx.spawn(
            persistent_request(move || Runtime::new(path)?.terminate(id)),
            move |workspace, result, ctx| match result {
                Ok(()) => {
                    if let Some(group) = workspace.prepare_persistent_pane_close(locator, id, ctx) {
                        group.update(ctx, |group, ctx| {
                            group.close_ended_persistent_session(locator.pane_id, id, ctx);
                        });
                        ctx.dispatch_global_action("workspace:save_app", ());
                    }
                }
                Err(err) => {
                    if let Some(group) =
                        workspace.get_pane_group_view_with_id(locator.pane_group_id)
                    {
                        group.update(ctx, |group, ctx| {
                            group.set_persistent_session_ending(locator.pane_id, id, false, ctx);
                        });
                    }
                    workspace.persistent_terminal_error(err, ctx);
                }
            },
        );
    }

    fn prepare_persistent_pane_close(
        &mut self,
        locator: PaneViewLocator,
        id: Uuid,
        ctx: &mut ViewContext<Self>,
    ) -> Option<ViewHandle<PaneGroup>> {
        let group = self
            .get_pane_group_view_with_id(locator.pane_group_id)?
            .clone();
        if group
            .as_ref(ctx)
            .persistent_session_id(locator.pane_id, ctx)
            != Some(id)
        {
            return None;
        }
        if self.tabs.len() == 1 && group.as_ref(ctx).visible_pane_count() == 1 {
            self.add_new_session_tab_internal_with_default_session_mode_behavior(
                NewSessionSource::Tab,
                Some(ctx.window_id()),
                None,
                None,
                true,
                DefaultSessionModeBehavior::Ignore,
                ctx,
            );
        }
        Some(group)
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
