use warpui::{AppContext, SingletonEntity, ViewContext, ViewHandle};

use super::{Direction, Event, PaneGroup, PaneId, TerminalPane};
use crate::pane_group::pane::{AnyPaneContent, DetachType};
use crate::workspace::sync_inputs::SyncedInputState;

impl PaneGroup {
    pub(crate) fn dockable_single_terminal(
        handle: &ViewHandle<Self>,
        ctx: &AppContext,
    ) -> Option<PaneId> {
        let group = handle.as_ref(ctx);
        if group.pane_contents.len() != 1
            || group.pane_count() != 1
            || group.child_agent_origin.is_some()
            || group.is_focused_pane_maximized(ctx)
            || (group.right_panel_open && group.is_right_panel_maximized)
            // Attaching to a synced group can overwrite the incoming terminal's input.
            || SyncedInputState::as_ref(ctx)
                .should_sync_this_pane_group(handle.id(), handle.window_id(ctx))
        {
            return None;
        }

        let pane = group.pane_id_by_index(0)?;
        group
            .terminal_session_by_id(pane)?
            .supports_terminal_docking(ctx)
            .then_some(pane)
    }

    pub(crate) fn take_terminal_pane_for_new_tab(
        &mut self,
        pane: PaneId,
        ctx: &mut ViewContext<Self>,
    ) -> Option<Box<dyn AnyPaneContent>> {
        if self.pane_count() < 2
            || self.panes.num_hidden_panes() != 0
            || self.pane_contents.len() != self.pane_count()
            || self.child_agent_origin.is_some()
            || !self.child_agent_panes.is_empty()
            || (self.is_focused_pane_maximized(ctx) && self.focused_pane_id(ctx) != pane)
            || (self.right_panel_open && self.is_right_panel_maximized)
            || !self.panes.is_pane_in_tree(pane)
            || SyncedInputState::as_ref(ctx)
                .should_sync_this_pane_group(ctx.view_id(), ctx.window_id())
            || !self
                .terminal_session_by_id(pane)?
                .supports_terminal_docking(ctx)
        {
            return None;
        }

        let content = self.remove_pane_for_move(&pane, ctx)?;
        ctx.unsubscribe_to_model(&content.as_pane().pane_configuration());

        // Unsubscribing from a shared model removes all callbacks for this pane group.
        // Restore the pane group and terminal subscriptions for the original tab.
        #[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
        Self::subscribe_to_child_sharing_events(ctx);
        for sibling in self.panes_of::<TerminalPane>() {
            sibling.subscribe_global_pane_models(ctx);
        }
        Some(content)
    }

    /// Moves an existing terminal between two single-terminal tabs in the same window.
    /// On success, the now-empty source tab must be removed in the same workspace update.
    pub(crate) fn dock_single_terminal(
        source: &ViewHandle<Self>,
        destination: &ViewHandle<Self>,
        target: PaneId,
        direction: Direction,
        ctx: &mut AppContext,
    ) -> bool {
        if source.id() == destination.id()
            || source.window_id(ctx) != destination.window_id(ctx)
            || Self::dockable_single_terminal(destination, ctx) != Some(target)
        {
            return false;
        }
        let Some(moved) = Self::dockable_single_terminal(source, ctx) else {
            return false;
        };
        if moved == target {
            return false;
        }

        // The source's root leaf stays in place until its empty tab is removed. Deferring
        // detach until after the split succeeds keeps failure free of lifecycle effects.
        let Some(content) = source.update(ctx, |group, _| group.pane_contents.remove(&moved))
        else {
            return false;
        };
        let split = destination.update(ctx, |group, _| group.panes.split(target, moved, direction));
        if !split {
            source.update(ctx, |group, _| group.pane_contents.insert(moved, content));
            return false;
        }

        source.update(ctx, |group, ctx| {
            content.as_pane().detach(group, DetachType::Moved, ctx);
        });
        destination.update(ctx, |group, ctx| {
            group.pane_contents.insert(moved, content);
            group.attach_pane(group.pane_contents[&moved].as_ref(), ctx);
            group.handle_pane_count_change(ctx);
            group.focus_pane_and_record_in_history(moved, ctx);
            ctx.notify();
            ctx.emit(Event::TerminalViewStateChanged);
            ctx.emit(Event::AppStateChanged);
        });
        true
    }
}
