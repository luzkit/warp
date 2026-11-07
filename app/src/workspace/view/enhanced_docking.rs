use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use warpui::{AppContext, EntityId, ViewContext};

use super::Workspace;
use crate::features::FeatureFlag;
use crate::pane_group::tree::get_divider_layout_extent;
use crate::pane_group::{Direction, PaneGroup, PaneId, get_minimum_pane_size};
use crate::workspace::PaneViewLocator;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DockingSource {
    Tab(EntityId),
    Pane(PaneViewLocator),
}

impl DockingSource {
    pub(super) fn pane_group_id(self) -> EntityId {
        match self {
            Self::Tab(group) => group,
            Self::Pane(locator) => locator.pane_group_id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DockingOperation {
    Split { direction: Direction, flatten: bool },
    Swap,
    Stack,
}

struct DockingPlan {
    source_index: usize,
    destination_index: usize,
    source_pane: Option<PaneId>,
}

impl Workspace {
    pub(super) fn move_pane_to_new_tab(&mut self, pane: PaneId, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::EnhancedPaneManagement.is_enabled() {
            return;
        }
        let Some(active_tab) = self.tabs.get(self.active_tab_index) else {
            return;
        };
        let source = active_tab.pane_group.clone();
        if active_tab.detached
            || (FeatureFlag::AgentManagementView.is_enabled()
                && self.current_workspace_state.is_agent_management_view_open)
            || self.has_docking_child_tab(source.id(), ctx)
        {
            return;
        }

        let before = self.capture_layout_state([source.id()], ctx);
        let (new_index, group) = self.new_tab_index_and_group(ctx);
        if let Some(content) = source.update(ctx, |source, ctx| {
            source.take_terminal_pane_for_new_tab(pane, ctx)
        }) {
            self.add_tab_from_existing_pane(content, new_index, group, ctx);
            self.record_layout_change(before, Some(self.active_tab_pane_group().id()), None, ctx);
        }
    }

    fn docking_source_index(&self, source: DockingSource, ctx: &AppContext) -> Option<usize> {
        if !FeatureFlag::EnhancedPaneManagement.is_enabled() {
            return None;
        }
        let group_id = source.pane_group_id();
        if self.has_docking_child_tab(group_id, ctx) {
            return None;
        }
        self.tabs.iter().position(|tab| {
            !tab.detached
                && tab.pane_group.id() == group_id
                && match source {
                    DockingSource::Tab(_) => {
                        PaneGroup::can_dock_terminal_layout(&tab.pane_group, ctx)
                    }
                    DockingSource::Pane(locator) => {
                        PaneGroup::can_drag_terminal_pane(&tab.pane_group, locator.pane_id, ctx)
                    }
                }
        })
    }

    pub(super) fn has_docking_child_tab(&self, group: EntityId, ctx: &AppContext) -> bool {
        self.tabs.iter().any(|tab| {
            tab.pane_group
                .as_ref(ctx)
                .child_agent_origin()
                .is_some_and(|origin| origin.source_pane_group.id() == group)
        })
    }

    pub(super) fn can_start_dock(&self, source: DockingSource, ctx: &AppContext) -> bool {
        self.docking_source_index(source, ctx).is_some()
    }

    pub(super) fn dock_source_is_layout(&self, source: DockingSource, ctx: &AppContext) -> bool {
        match source {
            DockingSource::Tab(group) => self
                .get_pane_group_view_with_id(group)
                .is_some_and(|handle| handle.as_ref(ctx).pane_count() > 1),
            DockingSource::Pane(_) => false,
        }
    }

    fn plan_dock(
        &self,
        source: DockingSource,
        target: PaneViewLocator,
        operation: DockingOperation,
        ctx: &AppContext,
    ) -> Option<DockingPlan> {
        let source_index = self.docking_source_index(source, ctx)?;
        if (FeatureFlag::AgentManagementView.is_enabled()
            && self.current_workspace_state.is_agent_management_view_open)
            || self.has_docking_child_tab(target.pane_group_id, ctx)
        {
            return None;
        }
        let destination_index = self
            .tabs
            .iter()
            .position(|tab| !tab.detached && tab.pane_group.id() == target.pane_group_id)?;
        let source_pane = match (source, operation) {
            (DockingSource::Pane(locator), _) => Some(locator.pane_id),
            (DockingSource::Tab(_), DockingOperation::Split { .. }) => {
                if source_index == destination_index {
                    return None;
                }
                None
            }
            (DockingSource::Tab(_), DockingOperation::Swap | DockingOperation::Stack) => {
                let source_group = self.tabs[source_index].pane_group.as_ref(ctx);
                if source_group.pane_count() != 1 {
                    return None;
                }
                Some(source_group.pane_id_by_index(0)?)
            }
        };
        if source_pane == Some(target.pane_id) {
            return None;
        }
        let source_group = &self.tabs[source_index].pane_group;
        let destination = &self.tabs[destination_index].pane_group;
        let eligible = match operation {
            DockingOperation::Split { .. } | DockingOperation::Swap => {
                source_pane
                    .is_none_or(|pane| PaneGroup::can_dock_terminal_pane(source_group, pane, ctx))
                    && PaneGroup::can_dock_terminal_pane(destination, target.pane_id, ctx)
            }
            DockingOperation::Stack => source_pane.is_some_and(|pane| {
                (source_index == destination_index
                    || PaneGroup::can_dock_terminal_pane(source_group, pane, ctx))
                    && PaneGroup::can_stack_terminal_at(destination, target.pane_id, pane, ctx)
            }),
        };
        if !eligible {
            return None;
        }
        Some(DockingPlan {
            source_index,
            destination_index,
            source_pane,
        })
    }

    pub(super) fn can_dock(
        &self,
        source: DockingSource,
        target: PaneViewLocator,
        operation: DockingOperation,
        ctx: &AppContext,
    ) -> bool {
        self.plan_dock(source, target, operation, ctx).is_some()
    }

    pub(super) fn dock_source_fits(
        &self,
        source: DockingSource,
        bounds: RectF,
        operation: DockingOperation,
        ctx: &AppContext,
    ) -> bool {
        match (source, operation) {
            (_, DockingOperation::Swap | DockingOperation::Stack) => true,
            (DockingSource::Pane(_), DockingOperation::Split { .. }) => {
                let minimum = get_minimum_pane_size(ctx);
                bounds.width() >= minimum && bounds.height() >= minimum
            }
            (DockingSource::Tab(group), DockingOperation::Split { direction, flatten }) => self
                .get_pane_group_view_with_id(group)
                .is_some_and(|handle| {
                    handle
                        .as_ref(ctx)
                        .docking_layout_fits(bounds.size(), direction, flatten, ctx)
                }),
        }
    }

    pub(super) fn dock(
        &mut self,
        source: DockingSource,
        target: PaneViewLocator,
        operation: DockingOperation,
        target_bounds: RectF,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let Some(plan) = self.plan_dock(source, target, operation, ctx) else {
            return false;
        };
        if let DockingOperation::Split { direction, .. } = operation {
            let bounds = target_bounds;
            let divider = get_divider_layout_extent();
            let size = match direction {
                Direction::Left | Direction::Right => {
                    vec2f((bounds.width() - divider) / 2., bounds.height())
                }
                Direction::Up | Direction::Down => {
                    vec2f(bounds.width(), (bounds.height() - divider) / 2.)
                }
            };
            let minimum = get_minimum_pane_size(ctx);
            if size.x() < minimum || size.y() < minimum {
                return false;
            }
            let incoming = RectF::new(Vector2F::zero(), size);
            if !self.dock_source_fits(source, incoming, operation, ctx) {
                return false;
            }
        }

        let source_group = &self.tabs[plan.source_index].pane_group;
        let destination = &self.tabs[plan.destination_index].pane_group;
        let same_tab = plan.source_index == plan.destination_index;
        let before = self.capture_layout_state(
            [
                Some(source_group.id()),
                (!same_tab).then_some(destination.id()),
            ]
            .into_iter()
            .flatten(),
            ctx,
        );
        let moved = match (plan.source_pane, operation, same_tab) {
            (Some(pane), DockingOperation::Stack, true) => source_group
                .update(ctx, |group, ctx| {
                    group.stack_existing_pane(pane, target.pane_id, ctx)
                }),
            (Some(pane), DockingOperation::Stack, false) => PaneGroup::stack_terminal_between_tabs(
                source_group,
                pane,
                destination,
                target.pane_id,
                ctx,
            ),
            (Some(pane), DockingOperation::Split { direction, .. }, true) => source_group
                .update(ctx, |group, ctx| {
                    group.dock_pane_within_tab(pane, target.pane_id, Some(direction), ctx)
                }),
            (Some(pane), DockingOperation::Swap, true) => source_group.update(ctx, |group, ctx| {
                group.dock_pane_within_tab(pane, target.pane_id, None, ctx)
            }),
            (Some(pane), DockingOperation::Split { direction, .. }, false) => {
                PaneGroup::dock_terminal_pane_between_tabs(
                    source_group,
                    pane,
                    destination,
                    target.pane_id,
                    direction,
                    ctx,
                )
            }
            (None, DockingOperation::Split { direction, flatten }, false) => {
                PaneGroup::dock_terminal_layout_between_tabs(
                    source_group,
                    destination,
                    target.pane_id,
                    direction,
                    flatten,
                    ctx,
                )
            }
            (Some(pane), DockingOperation::Swap, false) => {
                PaneGroup::swap_terminal_panes_between_tabs(
                    source_group,
                    pane,
                    destination,
                    target.pane_id,
                    ctx,
                )
            }
            (None, _, _) => false,
        };
        if !moved {
            return false;
        }
        let remove_source = source_group.as_ref(ctx).pane_ids().next().is_none();
        let retired =
            (remove_source && before.is_some()).then(|| self.retired_layout_tab(plan.source_index));
        if remove_source {
            self.cancel_tab_rename(ctx);
            self.remove_tab(plan.source_index, false, true, ctx);
        }
        if self.active_tab_pane_group().id() != target.pane_group_id {
            self.activate_tab_by_pane_group_id(target.pane_group_id, ctx);
        }
        self.record_layout_change(before, None, retired, ctx);
        true
    }
}
