use warpui::{AppContext, EntityId, ViewContext};

use super::Workspace;
use crate::features::FeatureFlag;
use crate::pane_group::{Direction, PaneGroup, PaneId};

impl Workspace {
    fn terminal_tab_docking_source_index(
        &self,
        source: EntityId,
        target: PaneId,
        ctx: &AppContext,
    ) -> Option<usize> {
        if !FeatureFlag::EnhancedPaneManagement.is_enabled() {
            return None;
        }
        let destination = self.tabs.get(self.active_tab_index)?;
        if destination.pane_group.id() == source
            || destination.detached
            || PaneGroup::dockable_single_terminal(&destination.pane_group, ctx) != Some(target)
        {
            return None;
        }
        self.tabs.iter().position(|tab| {
            tab.pane_group.id() == source
                && !tab.detached
                && PaneGroup::dockable_single_terminal(&tab.pane_group, ctx).is_some()
        })
    }

    pub(super) fn can_dock_terminal_tab(
        &self,
        source: EntityId,
        target: PaneId,
        ctx: &AppContext,
    ) -> bool {
        self.terminal_tab_docking_source_index(source, target, ctx)
            .is_some()
    }

    pub(super) fn dock_terminal_tab(
        &mut self,
        source: EntityId,
        target: PaneId,
        direction: Direction,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let Some(source_index) = self.terminal_tab_docking_source_index(source, target, ctx) else {
            return false;
        };
        if !PaneGroup::dock_single_terminal(
            &self.tabs[source_index].pane_group,
            self.active_tab_pane_group(),
            target,
            direction,
            ctx,
        ) {
            return false;
        }

        self.cancel_tab_rename(ctx);
        self.remove_tab(source_index, false, true, ctx);
        true
    }
}
