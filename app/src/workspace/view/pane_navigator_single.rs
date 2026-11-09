use warpui::{AppContext, SingletonEntity, ViewHandle};

use super::super::Workspace;
use super::super::docking_overlay::DOCKING_SURFACE_POSITION;
use super::super::enhanced_docking::{DockingOperation, DockingSource};
use super::super::group_docking::{PaneLocation, PaneSelection};
use crate::pane_group::{DockingLayoutSnapshot, PaneGroup};
use crate::workspace::cross_window_tab_drag::CrossWindowTabDrag;
use crate::workspace::{PaneViewLocator, WorkspaceRegistry};

#[derive(Clone, Copy)]
pub(super) enum SingleDockAction {
    Center {
        target: PaneLocation,
        operation: DockingOperation,
    },
    NewWindow,
}

#[derive(Clone)]
pub(super) struct SingleDockPlan {
    source: PaneLocation,
    action: SingleDockAction,
    source_snapshot: DockingLayoutSnapshot,
    target_snapshot: Option<DockingLayoutSnapshot>,
}

impl SingleDockPlan {
    pub(super) fn new(
        source: PaneLocation,
        action: SingleDockAction,
        ctx: &AppContext,
    ) -> Result<Self, String> {
        let workspace = source_workspace(source, ctx)
            .ok_or_else(|| "The source window is unavailable.".to_owned())?;
        let workspace = workspace.as_ref(ctx);
        let docking_source = DockingSource::Pane(locator(source));
        let source_snapshot = snapshot(workspace, source, ctx)
            .ok_or_else(|| "The selected pane is unavailable for docking.".to_owned())?;
        let target_snapshot =
            match action {
                SingleDockAction::Center { target, operation } => {
                    if source.window_id != target.window_id
                        || !matches!(operation, DockingOperation::Swap | DockingOperation::Stack)
                        || !workspace.can_dock(docking_source, locator(target), operation, ctx)
                        || ctx
                            .element_position_by_id_at_last_frame(
                                source.window_id,
                                DOCKING_SURFACE_POSITION,
                            )
                            .is_none()
                    {
                        return Err("This pane cannot be used for that placement.".to_owned());
                    }
                    Some(snapshot(workspace, target, ctx).ok_or_else(|| {
                        "The destination pane is unavailable for docking.".to_owned()
                    })?)
                }
                SingleDockAction::NewWindow => {
                    if !workspace.can_tear_out_docking_source(docking_source, ctx) {
                        return Err("The selected pane cannot move to a new window.".to_owned());
                    }
                    None
                }
            };
        Ok(Self {
            source,
            action,
            source_snapshot,
            target_snapshot,
        })
    }

    pub(super) fn commit(
        &self,
        keep_focus: Option<PaneLocation>,
        ctx: &mut AppContext,
    ) -> Result<(), String> {
        let stale = || "The panes or window changed. Preview the placement again.".to_owned();
        let current = Self::new(self.source, self.action, ctx).map_err(|_| stale())?;
        if !self.source_snapshot.matches(&current.source_snapshot)
            || !match (&self.target_snapshot, &current.target_snapshot) {
                (Some(before), Some(after)) => before.matches(after),
                (None, None) => true,
                _ => false,
            }
        {
            return Err(stale());
        }

        let workspace = source_workspace(self.source, ctx).ok_or_else(stale)?;
        let source = DockingSource::Pane(locator(self.source));
        let moved = workspace.update(ctx, |workspace, ctx| match self.action {
            SingleDockAction::Center { target, operation } => {
                let Some(bounds) = ctx.element_position_by_id_at_last_frame(
                    self.source.window_id,
                    DOCKING_SURFACE_POSITION,
                ) else {
                    return false;
                };
                workspace.dock(source, locator(target), operation, bounds, ctx)
            }
            SingleDockAction::NewWindow => workspace.tear_out_docking_source(source, None, ctx),
        });
        if !moved {
            return Err("The pane could not be moved. Choose another placement.".to_owned());
        }

        PaneSelection::handle(ctx).update(ctx, |selection, ctx| {
            if selection.members().contains(&self.source) {
                selection.toggle(self.source);
                ctx.notify();
            }
        });
        if let Some(origin) = keep_focus {
            restore_focus(origin, ctx);
        }
        Ok(())
    }
}

fn source_workspace(source: PaneLocation, ctx: &AppContext) -> Option<ViewHandle<Workspace>> {
    let handle = WorkspaceRegistry::as_ref(ctx).get(source.window_id, ctx)?;
    let workspace = handle.try_as_ref(ctx)?;
    (workspace.accepts_window_docking(ctx)
        && !CrossWindowTabDrag::as_ref(ctx).is_active()
        && workspace.docking_pane_drag.is_none()
        && workspace.docking_tab_drag.is_none())
    .then_some(handle)
}

fn snapshot(
    workspace: &Workspace,
    pane: PaneLocation,
    ctx: &AppContext,
) -> Option<DockingLayoutSnapshot> {
    let group = workspace.get_pane_group_view_with_id(pane.pane_group_id)?;
    let snapshot = PaneGroup::capture_docking_layout(group, ctx)?;
    let exists = snapshot.pane_ids().any(|id| id == pane.pane_id);
    exists.then_some(snapshot)
}

fn locator(pane: PaneLocation) -> PaneViewLocator {
    PaneViewLocator {
        pane_group_id: pane.pane_group_id,
        pane_id: pane.pane_id,
    }
}

fn restore_focus(origin: PaneLocation, ctx: &mut AppContext) {
    let Some(handle) = source_workspace(origin, ctx) else {
        return;
    };
    let workspace = handle.as_ref(ctx);
    let mut available = workspace.tabs.iter().filter(|tab| !tab.detached);
    let Some(first) = available.next() else {
        return;
    };
    let mut groups = std::iter::once(first)
        .chain(available)
        .map(|tab| &tab.pane_group);
    let group = groups.find(|group| group.as_ref(ctx).has_pane_id(origin.pane_id));
    let locator = group.map(|group| PaneViewLocator {
        pane_group_id: group.id(),
        pane_id: origin.pane_id,
    });
    ctx.windows().show_window_and_focus_app(origin.window_id);
    handle.update(ctx, |workspace, ctx| {
        if let Some(locator) = locator {
            workspace.focus_pane(locator, ctx);
        } else {
            workspace.focus_active_tab(ctx);
        }
    });
}
