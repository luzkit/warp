use std::collections::HashMap;
#[cfg(not(target_family = "wasm"))]
use std::collections::HashSet;

use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::Vector2F;
#[cfg(not(target_family = "wasm"))]
use pathfinder_geometry::vector::vec2f;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use warp_core::SessionId;
use warpui::elements::DraggableState;
use warpui::{AppContext, EntityId, SingletonEntity, ViewContext, ViewHandle};

use super::docked_stacks::DockedStack;
#[cfg(not(target_family = "wasm"))]
use super::tree::get_divider_layout_extent;
use super::tree::{PaneData, PaneLayout};
use super::{
    Direction, Event, PaneGroup, PaneId, TerminalPane, TerminalPaneId, get_minimum_pane_size,
};
use crate::pane_group::pane::{AnyPaneContent, DetachType};
use crate::workspace::sync_inputs::SyncedInputState;

type TerminalIdentity = (EntityId, Option<SessionId>);

#[derive(Clone)]
pub(crate) struct DockingLayoutSnapshot {
    pub(crate) tree: PaneLayout,
    stacks: Vec<DockedStack>,
    panes: HashMap<PaneId, Option<TerminalIdentity>>,
    focused: PaneId,
    active_session: Option<TerminalPaneId>,
}

impl DockingLayoutSnapshot {
    pub(crate) fn pane_ids(&self) -> impl Iterator<Item = PaneId> + '_ {
        self.panes.keys().copied()
    }

    pub(crate) fn matches(&self, current: &Self) -> bool {
        self.panes == current.panes
            && self
                .stacks
                .iter()
                .map(|stack| &stack.members)
                .eq(current.stacks.iter().map(|stack| &stack.members))
            && self.normalized_tree() == current.normalized_tree()
    }

    pub(crate) fn fits(&self, size: Vector2F, ctx: &AppContext) -> bool {
        self.tree.clone().into_pane_data().docked_subtree_fits(
            size,
            get_minimum_pane_size(ctx),
            None,
        )
    }

    fn normalized_tree(&self) -> PaneLayout {
        let mut tree = self.tree.clone();
        for stack in &self.stacks {
            tree.remap_leaf(stack.active, stack.members[0]);
        }
        tree
    }
}

impl PaneGroup {
    pub(crate) fn capture_docking_layout(
        handle: &ViewHandle<Self>,
        ctx: &AppContext,
    ) -> Option<DockingLayoutSnapshot> {
        let group = handle.as_ref(ctx);
        if !group.supports_docking_context(handle, ctx) || group.pane_contents.is_empty() {
            return None;
        }

        let panes = group
            .pane_contents
            .keys()
            .map(|&pane| {
                let terminal = group.terminal_view_from_pane_id(pane, ctx).map(|view| {
                    let session = view
                        .as_ref(ctx)
                        .active_session()
                        .as_ref(ctx)
                        .session_id(ctx);
                    (view.id(), session)
                });
                (pane, terminal)
            })
            .collect();
        Some(DockingLayoutSnapshot {
            tree: group.panes.layout_snapshot(),
            stacks: group.docked_stacks.clone(),
            panes,
            focused: group.focused_pane_id(ctx),
            active_session: group.active_session_id(ctx),
        })
    }

    pub(crate) fn can_replay_docking_move(&self, pane: PaneId, ctx: &AppContext) -> bool {
        self.terminal_session_by_id(pane)
            .is_some_and(|terminal| terminal.supports_terminal_docking(ctx))
    }

    pub(crate) fn take_layout_history_panes(
        &mut self,
        ids: &[PaneId],
        ctx: &mut ViewContext<Self>,
    ) -> Vec<Box<dyn AnyPaneContent>> {
        let panes = ids
            .iter()
            .map(|id| {
                let pane = self
                    .pane_contents
                    .remove(id)
                    .expect("Validated layout pane");
                pane.as_pane().detach(self, DetachType::Moved, ctx);
                ctx.unsubscribe_to_model(&pane.as_pane().pane_configuration());
                pane
            })
            .collect();
        if !ids.is_empty() {
            self.restore_docking_group_subscriptions(ctx);
        }
        panes
    }

    pub(crate) fn replay_docking_layout(
        &mut self,
        snapshot: DockingLayoutSnapshot,
        incoming: Vec<Box<dyn AnyPaneContent>>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.replace_docking_layout(
            snapshot.tree,
            snapshot.stacks,
            snapshot.focused,
            snapshot.active_session,
            incoming,
            ctx,
        );
    }

    pub(crate) fn apply_saved_layout_tree(
        &mut self,
        tree: PaneLayout,
        stacks: Vec<Vec<PaneId>>,
        focused: PaneId,
        incoming: Vec<Box<dyn AnyPaneContent>>,
        ctx: &mut ViewContext<Self>,
    ) {
        let stacks = stacks
            .into_iter()
            .map(|members| DockedStack {
                active: members[0],
                members,
            })
            .collect();
        self.replace_docking_layout(
            tree,
            stacks,
            focused,
            focused.as_terminal_pane_id(),
            incoming,
            ctx,
        );
    }

    pub(crate) fn set_saved_layout_tab_title(
        &mut self,
        title: Option<String>,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.custom_title != title {
            self.custom_title = title;
            ctx.emit(Event::PaneTitleUpdated);
            ctx.notify();
        }
    }

    pub(crate) fn saved_layout_session_id(&self, pane: PaneId, ctx: &AppContext) -> Option<Uuid> {
        let terminal = self.terminal_session_by_id(pane)?;
        #[cfg(target_os = "macos")]
        {
            let manager = terminal.terminal_manager(ctx);
            let manager = manager.as_ref(ctx);
            if manager.persistent_session_is_attached() {
                return manager.persistent_session_id();
            }
        }

        let view = terminal.terminal_view(ctx);
        let session = view
            .as_ref(ctx)
            .active_session()
            .as_ref(ctx)
            .session_id(ctx)?;
        // The session ID doubles as a shell hook authentication token. Store an opaque
        // identifier that also distinguishes new shells restored into the same pane.
        let mut hash = Sha256::new();
        hash.update(b"Warp pane layout session v1");
        hash.update(terminal.session_uuid());
        hash.update(session.as_u64().to_le_bytes());
        let digest = hash.finalize();
        let mut id = [0; 16];
        id.copy_from_slice(&digest[..16]);
        Some(Uuid::from_bytes(id))
    }

    pub(crate) fn has_saved_layout_side_panel(&self) -> bool {
        self.left_panel_open || self.right_panel_open
    }

    fn replace_docking_layout(
        &mut self,
        tree: PaneLayout,
        stacks: Vec<DockedStack>,
        focused: PaneId,
        active_session: Option<TerminalPaneId>,
        incoming: Vec<Box<dyn AnyPaneContent>>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.panes = tree.into_pane_data();
        self.docked_stacks = stacks;
        for pane in incoming {
            let id = pane.as_pane().id();
            self.pane_contents.insert(id, pane);
            self.attach_pane(self.pane_contents[&id].as_ref(), ctx);
        }
        self.pane_history
            .retain(|pane| self.pane_contents.contains_key(pane));
        self.handle_pane_count_change(ctx);
        self.focus_pane(focused, false, ctx);
        self.update_pane_history(focused);
        self.focus_state.update(ctx, |state, ctx| {
            state.set_active_session(active_session, ctx);
        });
        for pane in self.panes.pane_ids() {
            self.refresh_docked_stack_header(pane, ctx);
        }
        ctx.notify();
        ctx.emit(Event::TerminalViewStateChanged);
        ctx.emit(Event::AppStateChanged);
    }

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
        self.restore_docking_group_subscriptions(ctx);
    }

    fn restore_docking_group_subscriptions(&self, ctx: &mut ViewContext<Self>) {
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
        Self::has_window_docking_pane(handle, pane, ctx)
            && handle
                .as_ref(ctx)
                .terminal_session_by_id(pane)
                .is_some_and(|terminal| terminal.supports_terminal_docking(ctx))
    }

    pub(crate) fn has_window_docking_pane(
        handle: &ViewHandle<Self>,
        pane: PaneId,
        ctx: &AppContext,
    ) -> bool {
        let group = handle.as_ref(ctx);
        group.supports_docking_context(handle, ctx)
            && group.panes.is_pane_in_tree(pane)
            && group.pane_contents.contains_key(&pane)
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

    pub(crate) fn refresh_terminal_pane_header_drag(
        &self,
        pane: PaneId,
        ctx: &mut ViewContext<Self>,
    ) {
        if let Some(terminal) = self.terminal_session_by_id(pane) {
            let header = terminal.pane_view().as_ref(ctx).header().clone();
            header.update(ctx, |header, ctx| header.refresh_docking_drag(ctx));
        }
    }

    pub(crate) fn can_move_terminal_layout_to_window(
        handle: &ViewHandle<Self>,
        ctx: &AppContext,
    ) -> bool {
        let group = handle.as_ref(ctx);
        group.supports_docking_context(handle, ctx)
            && !group.pane_contents.is_empty()
            && group.pane_contents.values().all(|pane| {
                pane.as_any()
                    .downcast_ref::<TerminalPane>()
                    .is_some_and(|terminal| terminal.supports_terminal_docking(ctx))
            })
    }

    pub(crate) fn can_dock_terminal_layout(handle: &ViewHandle<Self>, ctx: &AppContext) -> bool {
        let group = handle.as_ref(ctx);
        group.pane_count() == group.pane_contents.len()
            && Self::can_move_terminal_layout_to_window(handle, ctx)
    }

    pub(crate) fn docking_preview_rects(
        &self,
        bounds: RectF,
        ctx: &AppContext,
    ) -> Vec<(PaneId, RectF)> {
        if self.is_focused_pane_maximized(ctx) || self.right_panel_open || self.left_panel_open {
            return Vec::new();
        }
        self.panes
            .docking_preview_rects(bounds, get_minimum_pane_size(ctx))
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

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn plan_group_docking_split(
        destination: PaneLayout,
        incoming: PaneLayout,
        target: PaneId,
        direction: Direction,
        bounds: RectF,
        ctx: &AppContext,
    ) -> Option<(PaneLayout, RectF, RectF)> {
        let minimum = get_minimum_pane_size(ctx);
        let mut destination = destination.into_pane_data();
        let incoming = incoming.into_pane_data();
        let target_bounds = destination
            .docking_preview_rects(bounds, minimum)
            .into_iter()
            .find_map(|(pane, bounds)| (pane == target).then_some(bounds))?;
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
        if !incoming.docked_subtree_fits(incoming_size, minimum, None) {
            return None;
        }

        let incoming_panes: HashSet<_> = incoming.pane_ids().into_iter().collect();
        destination
            .insert_docked_subtree(target, incoming, direction, false)
            .ok()?;
        let mut target_bounds = None;
        let mut incoming_bounds = None;
        for (pane, bounds) in destination.docking_preview_rects(bounds, minimum) {
            if pane == target {
                target_bounds = Some(bounds);
            } else if incoming_panes.contains(&pane) {
                incoming_bounds = Some(incoming_bounds.map_or(bounds, |previous: RectF| {
                    RectF::from_points(
                        previous.origin().min(bounds.origin()),
                        previous.lower_right().max(bounds.lower_right()),
                    )
                }));
            }
        }
        Some((
            destination.layout_snapshot(),
            target_bounds?,
            incoming_bounds?,
        ))
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn group_docking_layout_fits(
        layout: PaneLayout,
        size: Vector2F,
        ctx: &AppContext,
    ) -> bool {
        layout
            .into_pane_data()
            .docked_subtree_fits(size, get_minimum_pane_size(ctx), None)
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

    fn transfer_docking_pane_views(
        source: &ViewHandle<Self>,
        destination: &ViewHandle<Self>,
        panes: impl IntoIterator<Item = PaneId>,
        ctx: &mut AppContext,
    ) {
        let from = source.window_id(ctx);
        let to = destination.window_id(ctx);
        if from != to {
            for pane in panes {
                ctx.transfer_view_tree_to_window(pane.creation_order_id(), from, to);
            }
        }
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
        Self::transfer_docking_pane_views(source, destination, panes.keys().copied(), ctx);
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
        Self::transfer_docking_pane_views(source, destination, [pane], ctx);
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
