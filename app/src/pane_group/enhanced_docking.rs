use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::Vector2F;
use warpui::elements::DraggableState;
use warpui::{AppContext, SingletonEntity, ViewContext, ViewHandle};

use super::tree::PaneData;
use super::{Direction, Event, PaneGroup, PaneId, TerminalPane, get_minimum_pane_size};
use crate::pane_group::pane::{AnyPaneContent, DetachType};
use crate::workspace::sync_inputs::SyncedInputState;

impl PaneGroup {
    pub(crate) fn take_terminal_pane_for_new_tab(
        &mut self,
        pane: PaneId,
        ctx: &mut ViewContext<Self>,
    ) -> Option<Box<dyn AnyPaneContent>> {
        if self.panes.num_hidden_panes() != 0
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

        if self.docked_stack_members(pane).is_some() {
            return self.take_docked_stack_member_for_move(pane, ctx);
        }
        if self.pane_count() < 2 || self.pane_contents.len() != self.pane_count() {
            return None;
        }

        self.take_terminal_pane_for_move(pane, ctx)
    }

    fn take_terminal_pane_for_move(
        &mut self,
        pane: PaneId,
        ctx: &mut ViewContext<Self>,
    ) -> Option<Box<dyn AnyPaneContent>> {
        let content = self.remove_pane_for_move(&pane, ctx)?;
        self.restore_docking_source_subscriptions(content.as_ref(), ctx);
        Some(content)
    }

    pub(super) fn restore_docking_source_subscriptions(
        &self,
        moved: &dyn AnyPaneContent,
        ctx: &mut ViewContext<Self>,
    ) {
        ctx.unsubscribe_to_model(&moved.as_pane().pane_configuration());

        // Unsubscribing from a shared model removes all callbacks for this pane group.
        #[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
        Self::subscribe_to_child_sharing_events(ctx);
        for sibling in self.panes_of::<TerminalPane>() {
            sibling.subscribe_global_pane_models(ctx);
        }
    }

    fn supports_docking_context(&self, handle: &ViewHandle<Self>, ctx: &AppContext) -> bool {
        self.panes.num_hidden_panes() == 0
            && self.child_agent_origin.is_none()
            && self.child_agent_panes.is_empty()
            && !self.is_focused_pane_maximized(ctx)
            && !(self.right_panel_open && self.is_right_panel_maximized)
            && !SyncedInputState::as_ref(ctx)
                .should_sync_this_pane_group(handle.id(), handle.window_id(ctx))
    }

    pub(crate) fn can_drag_terminal_pane(
        handle: &ViewHandle<Self>,
        pane: PaneId,
        ctx: &AppContext,
    ) -> bool {
        let group = handle.as_ref(ctx);
        group.supports_docking_context(handle, ctx)
            && group.panes.is_pane_in_tree(pane)
            && group
                .terminal_session_by_id(pane)
                .is_some_and(|terminal| terminal.supports_terminal_docking(ctx))
    }

    pub(crate) fn can_dock_terminal_pane(
        handle: &ViewHandle<Self>,
        pane: PaneId,
        ctx: &AppContext,
    ) -> bool {
        let group = handle.as_ref(ctx);
        group.pane_count() == group.pane_contents.len()
            && Self::can_drag_terminal_pane(handle, pane, ctx)
    }

    pub(crate) fn terminal_pane_header_drag_state(
        &self,
        pane: PaneId,
        ctx: &AppContext,
    ) -> Option<DraggableState> {
        Some(self.terminal_session_by_id(pane)?.header_drag_state(ctx))
    }

    pub(crate) fn can_dock_terminal_layout(handle: &ViewHandle<Self>, ctx: &AppContext) -> bool {
        let group = handle.as_ref(ctx);
        group.supports_docking_context(handle, ctx)
            && group.pane_count() == group.pane_contents.len()
            && !group.pane_contents.is_empty()
            && group.pane_contents.values().all(|pane| {
                pane.as_any()
                    .downcast_ref::<TerminalPane>()
                    .is_some_and(|terminal| terminal.supports_terminal_docking(ctx))
            })
    }

    pub(crate) fn docking_preview_rects(
        &self,
        bounds: RectF,
        ctx: &AppContext,
    ) -> Vec<(PaneId, RectF)> {
        if self.is_focused_pane_maximized(ctx) || self.right_panel_open || self.left_panel_open {
            return Vec::new();
        }
        self.panes.docking_preview_rects(bounds)
    }

    pub(crate) fn docking_layout_fits(
        &self,
        size: Vector2F,
        direction: Direction,
        flatten: bool,
        ctx: &AppContext,
    ) -> bool {
        self.panes.docked_subtree_fits(
            size,
            get_minimum_pane_size(ctx),
            flatten.then_some(direction),
        )
    }

    pub(crate) fn dock_pane_within_tab(
        &mut self,
        source: PaneId,
        target: PaneId,
        direction: Option<Direction>,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let changed = match direction {
            Some(direction) => self.panes.move_pane_to_split(source, target, direction),
            None => self.panes.swap_panes(source, target),
        };
        if changed {
            self.finish_docking_update(source, ctx);
        }
        changed
    }

    fn finish_docking_update(&mut self, focused: PaneId, ctx: &mut ViewContext<Self>) {
        self.handle_pane_count_change(ctx);
        self.focus_pane_and_record_in_history(focused, ctx);
        ctx.notify();
        ctx.emit(Event::TerminalViewStateChanged);
        ctx.emit(Event::AppStateChanged);
    }

    /// The enclosing workspace update owns source-tab cleanup if the transfer empties it.
    pub(crate) fn dock_terminal_layout_between_tabs(
        source: &ViewHandle<Self>,
        destination: &ViewHandle<Self>,
        target: PaneId,
        direction: Direction,
        flatten: bool,
        ctx: &mut AppContext,
    ) -> bool {
        let focused = source.as_ref(ctx).focused_pane_id(ctx);
        let layout = source.update(ctx, |group, _| {
            std::mem::replace(&mut group.panes, PaneData::new(focused))
        });
        let result = destination.update(ctx, |group, _| {
            group
                .panes
                .insert_docked_subtree(target, layout, direction, flatten)
        });
        if let Err(layout) = result {
            source.update(ctx, |group, _| group.panes = layout);
            return false;
        }
        let panes = source.update(ctx, |group, ctx| {
            let panes = std::mem::take(&mut group.pane_contents);
            for pane in panes.values() {
                pane.as_pane().detach(group, DetachType::Moved, ctx);
            }
            panes
        });
        destination.update(ctx, |group, ctx| {
            for (id, pane) in panes {
                group.pane_contents.insert(id, pane);
                group.attach_pane(group.pane_contents[&id].as_ref(), ctx);
            }
            group.finish_docking_update(focused, ctx);
        });
        true
    }

    pub(crate) fn dock_terminal_pane_between_tabs(
        source: &ViewHandle<Self>,
        pane: PaneId,
        destination: &ViewHandle<Self>,
        target: PaneId,
        direction: Direction,
        ctx: &mut AppContext,
    ) -> bool {
        if source.as_ref(ctx).pane_count() == 1 {
            return Self::dock_terminal_layout_between_tabs(
                source,
                destination,
                target,
                direction,
                false,
                ctx,
            );
        }
        if destination
            .update(ctx, |group, _| {
                group
                    .panes
                    .insert_docked_subtree(target, PaneData::new(pane), direction, false)
            })
            .is_err()
        {
            return false;
        }
        let content = source.update(ctx, |group, ctx| {
            group.take_terminal_pane_for_move(pane, ctx)
        });
        let Some(content) = content else {
            destination.update(ctx, |group, _| group.panes.remove(pane));
            return false;
        };
        destination.update(ctx, |group, ctx| {
            group.pane_contents.insert(pane, content);
            group.attach_pane(group.pane_contents[&pane].as_ref(), ctx);
            group.finish_docking_update(pane, ctx);
        });
        true
    }

    pub(crate) fn swap_terminal_panes_between_tabs(
        source: &ViewHandle<Self>,
        pane: PaneId,
        destination: &ViewHandle<Self>,
        target: PaneId,
        ctx: &mut AppContext,
    ) -> bool {
        if !source.update(ctx, |group, _| {
            group.panes.replace_pane(pane, target, false)
        }) {
            return false;
        }
        if !destination.update(ctx, |group, _| {
            group.panes.replace_pane(target, pane, false)
        }) {
            source.update(ctx, |group, _| {
                group.panes.replace_pane(target, pane, false)
            });
            return false;
        }

        let source_content = source.update(ctx, |group, ctx| {
            group.detach_docking_pane_content(pane, ctx)
        });
        let target_content = destination.update(ctx, |group, ctx| {
            group.detach_docking_pane_content(target, ctx)
        });
        source.update(ctx, |group, ctx| {
            group.pane_contents.insert(target, target_content);
            group.attach_pane(group.pane_contents[&target].as_ref(), ctx);
            for id in &mut group.pane_history {
                if *id == pane {
                    *id = target;
                }
            }
            if group.focused_pane_id(ctx) == pane {
                group.focus_pane(target, false, ctx);
            } else if group.active_session_id(ctx) == pane.as_terminal_pane_id() {
                group.focus_state.update(ctx, |state, ctx| {
                    state.set_active_session(target.as_terminal_pane_id(), ctx);
                });
            }
            ctx.notify();
            ctx.emit(Event::TerminalViewStateChanged);
            ctx.emit(Event::AppStateChanged);
        });
        destination.update(ctx, |group, ctx| {
            group.pane_contents.insert(pane, source_content);
            group.attach_pane(group.pane_contents[&pane].as_ref(), ctx);
            group.remove_from_pane_history(target);
            group.finish_docking_update(pane, ctx);
        });
        true
    }

    pub(crate) fn stack_terminal_between_tabs(
        source: &ViewHandle<Self>,
        pane: PaneId,
        destination: &ViewHandle<Self>,
        target: PaneId,
        ctx: &mut AppContext,
    ) -> bool {
        let content = source.update(ctx, |group, ctx| {
            if group.pane_count() == 1 {
                Some(group.detach_docking_pane_content(pane, ctx))
            } else {
                group.take_terminal_pane_for_move(pane, ctx)
            }
        });
        let Some(content) = content else {
            return false;
        };
        destination.update(ctx, |group, ctx| {
            group.add_docked_stack_member(target, content, ctx)
        });
        true
    }

    fn detach_docking_pane_content(
        &mut self,
        pane: PaneId,
        ctx: &mut ViewContext<Self>,
    ) -> Box<dyn AnyPaneContent> {
        let content = self
            .pane_contents
            .remove(&pane)
            .expect("Validated docking pane");
        content.as_pane().detach(self, DetachType::Moved, ctx);
        self.restore_docking_source_subscriptions(content.as_ref(), ctx);
        content
    }
}
