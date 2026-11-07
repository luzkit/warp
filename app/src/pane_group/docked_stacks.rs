use warpui::{AppContext, EntityId, ViewContext, ViewHandle};

use super::pane::{AnyPaneContent, DetachType, PaneConfigurationEvent};
use super::{Event, PaneGroup, PaneId};
use crate::app_state::{self, BranchSnapshot, PaneNodeSnapshot};

pub(super) struct DockedStack {
    active: PaneId,
    members: Vec<PaneId>,
}

impl PaneGroup {
    pub(crate) fn docked_stack_members(&self, pane: PaneId) -> Option<&[PaneId]> {
        self.docked_stacks
            .iter()
            .find(|stack| stack.members.contains(&pane))
            .map(|stack| stack.members.as_slice())
    }

    pub(crate) fn can_stack_terminal_at(
        destination: &ViewHandle<Self>,
        target: PaneId,
        source: PaneId,
        ctx: &AppContext,
    ) -> bool {
        let group = destination.as_ref(ctx);
        target != source
            && Self::can_drag_terminal_pane(destination, target, ctx)
            && (!group.pane_contents.contains_key(&source)
                || (group.docked_stack_members(source).is_none()
                    && Self::can_drag_terminal_pane(destination, source, ctx)))
    }

    pub(crate) fn stack_existing_pane(
        &mut self,
        moved: PaneId,
        target: PaneId,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        if moved == target
            || self.docked_stack_members(moved).is_some()
            || !self.panes.is_pane_in_tree(target)
            || !self.panes.remove(moved)
        {
            return false;
        }

        self.stack_panes(target, moved, ctx);
        true
    }

    pub(super) fn add_docked_stack_member(
        &mut self,
        target: PaneId,
        content: Box<dyn AnyPaneContent>,
        ctx: &mut ViewContext<Self>,
    ) {
        let pane = content.as_pane().id();
        self.pane_contents.insert(pane, content);
        self.attach_pane(self.pane_contents[&pane].as_ref(), ctx);
        self.stack_panes(target, pane, ctx);
    }

    fn stack_panes(&mut self, target: PaneId, added: PaneId, ctx: &mut ViewContext<Self>) {
        if let Some(stack) = self
            .docked_stacks
            .iter_mut()
            .find(|stack| stack.active == target)
        {
            stack.members.push(added);
        } else {
            self.docked_stacks.push(DockedStack {
                active: target,
                members: vec![target, added],
            });
        }

        self.handle_pane_count_change(ctx);
        self.focus_pane_and_record_in_history(added, ctx);
        ctx.notify();
        ctx.emit(Event::TerminalViewStateChanged);
        ctx.emit(Event::AppStateChanged);
    }

    pub(super) fn reveal_docked_stack_member(
        &mut self,
        pane: PaneId,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let Some(stack) = self
            .docked_stacks
            .iter_mut()
            .find(|stack| stack.members.contains(&pane))
        else {
            return true;
        };
        if stack.active == pane {
            return self.panes.is_pane_in_tree(pane);
        }

        if self.panes.is_pane_hidden(&stack.active)
            || self.panes.is_pane_hidden(&pane)
            || !self.panes.replace_pane(stack.active, pane, false)
        {
            return false;
        }

        stack.active = pane;
        self.refresh_docked_stack_header(pane, ctx);
        ctx.notify();
        ctx.emit(Event::TerminalViewStateChanged);
        ctx.emit(Event::AppStateChanged);
        true
    }

    pub(super) fn replace_docked_stack_member(
        &mut self,
        original: PaneId,
        replacement: PaneId,
        ctx: &mut ViewContext<Self>,
    ) {
        if let Some(stack) = self
            .docked_stacks
            .iter_mut()
            .find(|stack| stack.active == original)
        {
            let active = stack
                .members
                .iter_mut()
                .find(|member| **member == original)
                .expect("active stack pane is a member");
            *active = replacement;
            stack.active = replacement;
            self.refresh_docked_stack_header(replacement, ctx);
        }
    }

    pub(super) fn close_docked_stack_member(
        &mut self,
        pane: PaneId,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let Some((former_slot, surviving)) = self.remove_docked_stack_member(pane) else {
            return self.docked_stack_members(pane).is_some();
        };

        self.transfer_child_agent_conversations_to_parents_on_close(pane, ctx);
        if let Some(view) = self.terminal_view_from_pane_id(pane, ctx) {
            self.remove_child_agent_panes(view.id(), ctx);
        }
        self.forget_transitively_shared_pane(pane);
        self.detach_docked_stack_member(pane, former_slot, surviving, DetachType::Closed, ctx);
        true
    }

    pub(super) fn take_docked_stack_member_for_move(
        &mut self,
        pane: PaneId,
        ctx: &mut ViewContext<Self>,
    ) -> Option<Box<dyn AnyPaneContent>> {
        if !self.panes.is_pane_in_tree(pane) || self.panes.is_pane_hidden(&pane) {
            return None;
        }

        let (former_slot, surviving) = self.remove_docked_stack_member(pane)?;
        Some(self.detach_docked_stack_member(pane, former_slot, surviving, DetachType::Moved, ctx))
    }

    fn remove_docked_stack_member(&mut self, pane: PaneId) -> Option<(PaneId, PaneId)> {
        let (stack_index, member_index) =
            self.docked_stacks
                .iter()
                .enumerate()
                .find_map(|(index, stack)| {
                    stack
                        .members
                        .iter()
                        .position(|id| *id == pane)
                        .map(|member| (index, member))
                })?;

        let stack = &self.docked_stacks[stack_index];
        let mut surviving = stack.active;
        let mut former_slot = pane;
        if stack.active == pane {
            surviving = stack.members[(member_index + 1) % stack.members.len()];
            if let Some(replacement) = self.panes.replacement_pane_for_original(pane) {
                former_slot = replacement;
                self.panes.revert_temporary_replacement(replacement)?;
            }
            if !self.panes.replace_pane(pane, surviving, false) {
                return None;
            }
        }

        let stack = &mut self.docked_stacks[stack_index];
        stack.members.remove(member_index);
        stack.active = surviving;
        if stack.members.len() == 1 {
            self.docked_stacks.remove(stack_index);
        }
        Some((former_slot, surviving))
    }

    fn detach_docked_stack_member(
        &mut self,
        pane: PaneId,
        former_slot: PaneId,
        surviving: PaneId,
        detach_type: DetachType,
        ctx: &mut ViewContext<Self>,
    ) -> Box<dyn AnyPaneContent> {
        self.panes.remove_hidden_pane(pane);
        self.remove_from_pane_history(pane);
        let content = self
            .pane_contents
            .remove(&pane)
            .expect("stack pane content is attached");
        content.as_pane().detach(self, detach_type, ctx);
        self.restore_docking_source_subscriptions(content.as_ref(), ctx);

        if self.focused_pane_id(ctx) == former_slot {
            self.focus_pane_and_record_in_history(surviving, ctx);
        } else if self.active_session_id(ctx) == pane.as_terminal_pane_id() {
            self.focus_state.update(ctx, |state, ctx| {
                state.set_active_session(surviving.as_terminal_pane_id(), ctx);
            });
        }

        self.refresh_docked_stack_header(surviving, ctx);
        self.handle_pane_count_change(ctx);
        ctx.notify();
        ctx.emit(Event::TerminalViewStateChanged);
        ctx.emit(Event::AppStateChanged);
        content
    }

    fn refresh_docked_stack_header(&self, pane: PaneId, ctx: &mut ViewContext<Self>) {
        if let Some(content) = self.pane_by_id(pane) {
            content.pane_configuration().update(ctx, |_, ctx| {
                ctx.emit(PaneConfigurationEvent::RefreshPaneHeaderOverflowMenuItems);
            });
        }
    }

    pub(super) fn snapshot_docked_stack(
        &self,
        visible: PaneId,
        app: &AppContext,
    ) -> Option<PaneNodeSnapshot> {
        let active = self
            .panes
            .original_pane_for_replacement(visible)
            .unwrap_or(visible);
        let members = self.docked_stack_members(active)?;
        // Save each member in the existing snapshot format so the active member does not
        // replace the rest of the stack in restoration data.
        Some(PaneNodeSnapshot::Branch(BranchSnapshot {
            direction: app_state::SplitDirection::Horizontal,
            children: members
                .iter()
                .map(|&id| {
                    let leaf = if id == active { visible } else { id };
                    (app_state::PaneFlex(1.), self.snapshot_for_leaf(app, leaf))
                })
                .collect(),
        }))
    }

    pub(super) fn docked_stack_pane_ids(&self) -> impl Iterator<Item = PaneId> + '_ {
        self.docked_stacks
            .iter()
            .flat_map(|stack| stack.members.iter().copied())
    }

    pub(super) fn docked_stack_view_ids(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.docked_stack_pane_ids()
            .filter_map(|id| self.terminal_session_by_id(id))
            .map(|pane| pane.pane_view().id())
    }
}
