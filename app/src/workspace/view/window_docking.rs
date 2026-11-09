use std::collections::HashMap;
use std::iter;
use std::sync::Arc;

use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use warpui::{
    AppContext, DisplayIdx, EntityId, SingletonEntity, ViewContext, ViewHandle, WindowId,
};

use super::enhanced_docking::{DockingOperation, DockingSource};
use super::{TransferredTab, Workspace};
use crate::features::FeatureFlag;
use crate::pane_group::tree::get_divider_layout_extent;
use crate::pane_group::{
    Direction, NewTerminalOptions, PaneGroup, PanesLayout, get_minimum_pane_size,
};
use crate::root_view::{create_docked_window, quake_mode_window_id};
use crate::tab::SelectedTabColor;
use crate::themes::theme::AnsiColorIdentifier;
use crate::window_settings::WindowSettings;
use crate::workspace::cross_window_tab_drag::CrossWindowTabDrag;
use crate::workspace::{PaneViewLocator, WorkspaceRegistry};

impl Workspace {
    pub(super) fn accepts_window_docking(&self, ctx: &AppContext) -> bool {
        !self.tabs.is_empty()
            && !self.is_tab_drag_preview()
            && !self.pending_pane_group_transfer
            && !self.suppress_detach_panes_on_window_close
            && !self.current_workspace_state.is_agent_management_view_open
            && !self.current_workspace_state.is_native_quit_modal_open
            && quake_mode_window_id() != Some(self.window_id)
            && ctx.is_window_open(self.window_id)
    }

    pub(super) fn window_docking_tab_index(
        &self,
        pane_group: EntityId,
        ctx: &AppContext,
    ) -> Option<usize> {
        if cfg!(target_family = "wasm")
            || !FeatureFlag::EnhancedPaneManagement.is_enabled()
            || CrossWindowTabDrag::as_ref(ctx).is_active()
            || !self.accepts_window_docking(ctx)
            || self.has_docking_child_tab(pane_group, ctx)
        {
            return None;
        }

        self.tabs.iter().position(|tab| {
            !tab.detached
                && tab.pane_group.id() == pane_group
                && !tab.pane_group.as_ref(ctx).is_being_resized()
        })
    }

    fn window_docking_source_index(
        &self,
        source: DockingSource,
        ctx: &AppContext,
    ) -> Option<usize> {
        let index = self.window_docking_tab_index(source.pane_group_id(), ctx)?;
        let group = &self.tabs[index].pane_group;
        let eligible = match source {
            DockingSource::Pane(locator) => {
                PaneGroup::can_drag_terminal_pane(group, locator.pane_id, ctx)
            }
            DockingSource::Tab(_) => PaneGroup::can_move_terminal_layout_to_window(group, ctx),
        };
        eligible.then_some(index)
    }

    pub(super) fn can_offer_window_move_for_pane(
        &self,
        pane: PaneViewLocator,
        ctx: &AppContext,
    ) -> bool {
        self.window_docking_tab_index(pane.pane_group_id, ctx)
            .is_some_and(|index| {
                PaneGroup::has_window_docking_pane(&self.tabs[index].pane_group, pane.pane_id, ctx)
            })
    }

    pub(super) fn window_docking_destination(
        &self,
        window_id: WindowId,
        ctx: &AppContext,
    ) -> Option<ViewHandle<Self>> {
        if self.window_id == window_id {
            return None;
        }
        let workspace = WorkspaceRegistry::as_ref(ctx).get(window_id, ctx)?;
        let destination = workspace.as_ref(ctx);
        (destination.accepts_window_docking(ctx) && self.team_uid(ctx) == destination.team_uid(ctx))
            .then_some(workspace)
    }

    pub(super) fn can_move_docking_source_to_window(
        &self,
        source: DockingSource,
        destination: WindowId,
        ctx: &AppContext,
    ) -> bool {
        self.window_docking_source_index(source, ctx).is_some()
            && self.window_docking_destination(destination, ctx).is_some()
    }

    pub(super) fn move_docking_source_to_window(
        &mut self,
        source: DockingSource,
        destination: WindowId,
        tab_index: Option<usize>,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let Some(index) = self.window_docking_source_index(source, ctx) else {
            return false;
        };
        let Some(destination_workspace) = self.window_docking_destination(destination, ctx) else {
            return false;
        };
        let Some((transferred_tab, moves_entire_tab)) =
            self.take_window_docking_tab(source, index, ctx)
        else {
            return false;
        };

        ctx.transfer_view_tree_to_window(
            transferred_tab.pane_group.id(),
            self.window_id,
            destination,
        );
        destination_workspace.update(ctx, |workspace, ctx| {
            let insertion_index = tab_index.unwrap_or(workspace.tab_count());
            workspace.adopt_window_docking_tab(transferred_tab, insertion_index, ctx);
        });
        if moves_entire_tab {
            self.finish_window_docking_tab_transfer(index, ctx);
        }

        focus_docking_window(destination, ctx);
        ctx.dispatch_global_action("workspace:save_app", ());
        true
    }

    pub(super) fn can_tear_out_docking_source(
        &self,
        source: DockingSource,
        ctx: &AppContext,
    ) -> bool {
        self.window_docking_source_index(source, ctx).is_some()
            && ctx.window_bounds(&self.window_id).is_some()
    }

    pub(super) fn tear_out_docking_source(
        &mut self,
        source: DockingSource,
        bounds: Option<RectF>,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let Some(index) = self.window_docking_source_index(source, ctx) else {
            return false;
        };
        let Some(source_bounds) = ctx.window_bounds(&self.window_id) else {
            return false;
        };
        let bounds = bounds.unwrap_or_else(|| {
            RectF::new(
                source_bounds.origin() + vec2f(32., 32.),
                source_bounds.size(),
            )
        });
        let Some(bounds) = fit_new_docking_window(bounds, ctx) else {
            return false;
        };
        let Some((transferred_tab, moves_entire_tab)) =
            self.take_window_docking_tab(source, index, ctx)
        else {
            return false;
        };

        let window_id = create_docked_window(transferred_tab, self.window_id, bounds, ctx);
        if moves_entire_tab {
            self.finish_window_docking_tab_transfer(index, ctx);
        }

        focus_docking_window(window_id, ctx);
        ctx.dispatch_global_action("workspace:save_app", ());
        true
    }

    fn take_window_docking_tab(
        &mut self,
        source: DockingSource,
        index: usize,
        ctx: &mut ViewContext<Self>,
    ) -> Option<(TransferredTab, bool)> {
        let source_group = self.tabs[index].pane_group.clone();
        if let DockingSource::Pane(locator) = source
            && source_group.as_ref(ctx).pane_ids().count() > 1
        {
            let pane = source_group.update(ctx, |group, ctx| {
                group.take_terminal_pane_for_new_tab(locator.pane_id, ctx)
            })?;
            let pane_group = ctx.add_typed_action_view(|ctx| {
                PaneGroup::new_from_existing_pane(
                    pane,
                    self.tips_completed.clone(),
                    self.user_default_shell_unsupported_banner_model_handle
                        .clone(),
                    self.server_api.clone(),
                    self.model_event_sender.clone(),
                    ctx,
                )
            });
            return Some((
                TransferredTab {
                    pane_group,
                    color: None,
                    custom_title: None,
                    left_panel_open: false,
                    vertical_tabs_panel_open: self.vertical_tabs_panel_open,
                    right_panel_open: false,
                    is_right_panel_maximized: false,
                    draggable_state: Default::default(),
                },
                false,
            ));
        }

        let mut tab = self.get_tab_transfer_info_for_attach(index, ctx)?;
        tab.draggable_state = Default::default();
        self.begin_window_docking_tab_transfer(index, ctx);
        Some((tab, true))
    }

    pub(super) fn initialize_transferred_tab(
        &mut self,
        tab_to_adopt: Option<TransferredTab>,
        tab_color: Option<AnsiColorIdentifier>,
        custom_title: Option<String>,
        ctx: &mut ViewContext<Self>,
    ) {
        if let Some(tab) = tab_to_adopt {
            self.adopt_window_docking_tab(tab, 0, ctx);
        } else {
            self.add_tab_with_pane_layout(
                Default::default(),
                Arc::new(HashMap::new()),
                custom_title,
                ctx,
            );
            if let (Some(color), Some(tab)) = (tab_color, self.tabs.last_mut()) {
                tab.selected_color = SelectedTabColor::Color(color);
            }
            self.pending_pane_group_transfer = true;
        }
    }

    pub(super) fn adopt_window_docking_tab(
        &mut self,
        transferred_tab: TransferredTab,
        index: usize,
        ctx: &mut ViewContext<Self>,
    ) {
        let pane_group = transferred_tab.pane_group.id();
        let workspace = ctx.view_id();
        let window = ctx.window_id();
        ctx.record_structural_view_parent(window, pane_group, workspace);
        self.insert_transferred_tab_at_index(transferred_tab, index, ctx);
    }

    fn begin_window_docking_tab_transfer(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let tab = &mut self.tabs[index];
        tab.detached = true;
        let pane_group = tab.pane_group.clone();
        self.prepare_for_transferred_tab_attach(&pane_group, ctx);
    }

    fn finish_window_docking_tab_transfer(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        if self.tab_count() == 1 && !self.prepare_empty_docking_window(ctx) {
            return;
        }
        self.remove_tab_without_undo(index, ctx);
        self.set_suppress_detach_panes_on_window_close(false);
    }

    #[cfg(not(target_family = "wasm"))]
    pub(super) fn remove_empty_group_docking_tabs(
        &mut self,
        empty_groups: &[EntityId],
        ctx: &mut ViewContext<Self>,
    ) {
        if empty_groups.len() == self.tab_count() && !self.prepare_empty_docking_window(ctx) {
            return;
        }
        if empty_groups.contains(&self.active_tab_pane_group().id())
            && let Some(index) = self
                .tabs
                .iter()
                .position(|tab| !empty_groups.contains(&tab.pane_group.id()))
        {
            self.activate_tab_internal(index, ctx);
        }
        for index in (0..self.tabs.len()).rev() {
            if empty_groups.contains(&self.tabs[index].pane_group.id()) {
                self.remove_tab_without_undo(index, ctx);
            }
        }
    }

    fn prepare_empty_docking_window(&mut self, ctx: &mut ViewContext<Self>) -> bool {
        if *WindowSettings::as_ref(ctx).close_empty_windows_after_pane_transfer {
            CrossWindowTabDrag::handle(ctx).update(ctx, |drag, _| {
                drag.register_pending_source_close(self.window_id);
            });
            self.close_window_for_content_transfer(ctx);
            return false;
        }

        self.add_tab_with_pane_layout(
            PanesLayout::SingleTerminal(Box::new(
                NewTerminalOptions::default().with_homepage_hidden(),
            )),
            Arc::new(HashMap::new()),
            None,
            ctx,
        );
        true
    }

    fn plan_window_docking_split(
        &self,
        source: DockingSource,
        destination: WindowId,
        target: PaneViewLocator,
        operation: DockingOperation,
        target_bounds: RectF,
        ctx: &AppContext,
    ) -> Option<(usize, ViewHandle<PaneGroup>)> {
        let DockingOperation::Split { direction, .. } = operation else {
            return None;
        };
        let source_index = self.window_docking_source_index(source, ctx)?;
        let source_group = &self.tabs[source_index].pane_group;
        let source_can_split = match source {
            DockingSource::Pane(locator) => {
                PaneGroup::can_dock_terminal_pane(source_group, locator.pane_id, ctx)
            }
            DockingSource::Tab(_) => PaneGroup::can_dock_terminal_layout(source_group, ctx),
        };
        if !source_can_split {
            return None;
        }

        let destination_workspace = self.window_docking_destination(destination, ctx)?;
        let workspace = destination_workspace.as_ref(ctx);
        let target_group = workspace.active_tab_pane_group();
        if target_group.id() != target.pane_group_id
            || workspace.has_docking_child_tab(target_group.id(), ctx)
            || target_group.as_ref(ctx).is_being_resized()
            || !PaneGroup::can_dock_terminal_pane(target_group, target.pane_id, ctx)
        {
            return None;
        }

        let divider = get_divider_layout_extent();
        let incoming_size = match direction {
            Direction::Left | Direction::Right => vec2f(
                (target_bounds.width() - divider) / 2.,
                target_bounds.height(),
            ),
            Direction::Up | Direction::Down => vec2f(
                target_bounds.width(),
                (target_bounds.height() - divider) / 2.,
            ),
        };
        let minimum = get_minimum_pane_size(ctx);
        if incoming_size.x() < minimum
            || incoming_size.y() < minimum
            || !self.dock_source_fits(
                source,
                RectF::new(Vector2F::zero(), incoming_size),
                operation,
                ctx,
            )
        {
            return None;
        }
        Some((source_index, target_group.clone()))
    }

    pub(super) fn can_dock_across_windows(
        &self,
        source: DockingSource,
        destination: WindowId,
        target: PaneViewLocator,
        operation: DockingOperation,
        target_bounds: RectF,
        ctx: &AppContext,
    ) -> bool {
        self.plan_window_docking_split(source, destination, target, operation, target_bounds, ctx)
            .is_some()
    }

    pub(super) fn dock_across_windows(
        &mut self,
        source: DockingSource,
        destination: WindowId,
        target: PaneViewLocator,
        operation: DockingOperation,
        target_bounds: RectF,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let DockingOperation::Split { direction, flatten } = operation else {
            return false;
        };
        let Some((index, destination_group)) = self.plan_window_docking_split(
            source,
            destination,
            target,
            operation,
            target_bounds,
            ctx,
        ) else {
            return false;
        };
        let source_group = self.tabs[index].pane_group.clone();
        let moved = match source {
            DockingSource::Pane(locator) => PaneGroup::dock_terminal_pane_between_tabs(
                &source_group,
                locator.pane_id,
                &destination_group,
                target.pane_id,
                direction,
                ctx,
            ),
            DockingSource::Tab(_) => PaneGroup::dock_terminal_layout_between_tabs(
                &source_group,
                &destination_group,
                target.pane_id,
                direction,
                flatten,
                ctx,
            ),
        };
        if !moved {
            return false;
        }
        if source_group.as_ref(ctx).pane_ids().next().is_none() {
            self.begin_window_docking_tab_transfer(index, ctx);
            self.finish_window_docking_tab_transfer(index, ctx);
        }

        focus_docking_window(destination, ctx);
        ctx.dispatch_global_action("workspace:save_app", ());
        true
    }
}

fn fit_new_docking_window(bounds: RectF, ctx: &AppContext) -> Option<RectF> {
    if !bounds.min_x().is_finite()
        || !bounds.min_y().is_finite()
        || !bounds.width().is_finite()
        || !bounds.height().is_finite()
        || bounds.width() <= 0.
        || bounds.height() <= 0.
    {
        return None;
    }
    let displays = iter::once(DisplayIdx::Primary)
        .chain((0..ctx.windows().display_count().saturating_sub(1)).map(DisplayIdx::External));
    let display = displays
        .filter_map(|idx| ctx.windows().bounds_for_display_idx(idx))
        .find(|display| display.contains_point(bounds.origin()))
        .unwrap_or_else(|| ctx.windows().active_display_bounds());
    if display.width() <= 0. || display.height() <= 0. {
        return Some(bounds);
    }

    let size = vec2f(
        bounds.width().min(display.width()),
        bounds.height().min(display.height()),
    );
    let origin = vec2f(
        bounds
            .min_x()
            .min(display.max_x() - size.x())
            .max(display.min_x()),
        bounds
            .min_y()
            .min(display.max_y() - size.y())
            .max(display.min_y()),
    );
    Some(RectF::new(origin, size))
}

fn focus_docking_window(window_id: WindowId, ctx: &mut AppContext) {
    ctx.windows().show_window_and_focus_app(window_id);
    if let Some(workspace) = WorkspaceRegistry::as_ref(ctx).get(window_id, ctx) {
        workspace.update(ctx, |workspace, ctx| workspace.focus_active_tab(ctx));
    }
}
