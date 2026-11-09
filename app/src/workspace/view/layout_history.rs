use std::collections::{HashMap, HashSet, VecDeque};

use warpui::{AppContext, EntityId, ViewContext};

use super::Workspace;
use super::docking_overlay::DOCKING_SURFACE_POSITION;
use crate::features::FeatureFlag;
use crate::pane_group::tree::PaneLayout;
use crate::pane_group::{AnyPaneContent, DockingLayoutSnapshot, PaneGroup, PaneId};
use crate::tab::TabData;
use crate::workspace::tab_group::{TabGroup, TabGroupId};

#[cfg(test)]
#[path = "layout_history_tests.rs"]
mod tests;

const MAX_LAYOUT_CHANGES: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
struct TabPosition {
    id: EntityId,
    group: Option<TabGroupId>,
    pinned: bool,
}

#[derive(Clone)]
pub(super) struct LayoutState {
    tabs: Vec<TabPosition>,
    layouts: HashMap<EntityId, DockingLayoutSnapshot>,
    active_tab: EntityId,
    tab_titles: HashMap<EntityId, Option<String>>,
}

impl LayoutState {
    pub(super) fn matches(&self, current: &Self) -> bool {
        self.tabs == current.tabs
            && (self.tab_titles.is_empty() || self.tab_titles == current.tab_titles)
            && self.layouts.len() == current.layouts.len()
            && self.layouts.iter().all(|(id, expected)| {
                current
                    .layouts
                    .get(id)
                    .is_some_and(|live| expected.matches(live))
            })
    }

    fn capture_tab_titles(&mut self, tabs: &[TabData], ctx: &AppContext) {
        self.tab_titles = tabs
            .iter()
            .map(|tab| {
                (
                    tab.pane_group.id(),
                    tab.pane_group.as_ref(ctx).custom_title(ctx),
                )
            })
            .collect();
    }

    fn pane_owners(&self) -> HashMap<PaneId, EntityId> {
        self.layouts
            .iter()
            .flat_map(|(&id, layout)| layout.pane_ids().map(move |pane| (pane, id)))
            .collect()
    }
}

struct LayoutChange {
    before: LayoutState,
    after: LayoutState,
}

pub(super) struct RetiredLayoutTab {
    tab: TabData,
    group: Option<TabGroup>,
}

#[derive(Default)]
pub(super) struct LayoutHistory {
    undo: VecDeque<LayoutChange>,
    redo: Vec<LayoutChange>,
    // Only empty pane groups are retained here. Snapshots never retain live pane handles.
    retired_tabs: HashMap<EntityId, RetiredLayoutTab>,
}

impl LayoutHistory {
    fn record(
        &mut self,
        change: LayoutChange,
        retired: impl IntoIterator<Item = RetiredLayoutTab>,
    ) {
        self.redo.clear();
        if self.undo.len() == MAX_LAYOUT_CHANGES {
            self.undo.pop_front();
        }
        self.undo.push_back(change);
        for retired in retired {
            self.retired_tabs
                .insert(retired.tab.pane_group.id(), retired);
        }
        self.prune_retired_tabs();
    }

    fn prune_retired_tabs(&mut self) {
        self.retired_tabs.retain(|id, _| {
            self.undo.iter().chain(&self.redo).any(|change| {
                change.before.layouts.contains_key(id) || change.after.layouts.contains_key(id)
            })
        });
    }
}

#[derive(Clone, Copy)]
enum Replay {
    Undo,
    Redo,
}

enum ReplayResult {
    Applied,
    Stale,
    InsufficientSpace,
}

impl Workspace {
    pub(super) fn capture_layout_state(
        &self,
        groups: impl IntoIterator<Item = EntityId>,
        ctx: &AppContext,
    ) -> Option<LayoutState> {
        if !FeatureFlag::EnhancedPaneManagement.is_enabled()
            || self.current_workspace_state.is_agent_management_view_open
        {
            return None;
        }
        let mut layouts = HashMap::new();
        for id in groups {
            let tab = self.tabs.iter().find(|tab| tab.pane_group.id() == id)?;
            if tab.detached || self.has_docking_child_tab(id, ctx) {
                return None;
            }
            layouts.insert(id, PaneGroup::capture_docking_layout(&tab.pane_group, ctx)?);
        }

        Some(LayoutState {
            tabs: self
                .tabs
                .iter()
                .map(|tab| TabPosition {
                    id: tab.pane_group.id(),
                    group: tab.group_id,
                    pinned: tab.pinned,
                })
                .collect(),
            layouts,
            active_tab: self.active_tab_pane_group().id(),
            tab_titles: HashMap::new(),
        })
    }

    pub(super) fn retired_layout_tab(&self, index: usize) -> RetiredLayoutTab {
        let tab = self.tabs[index].clone();
        let group = tab
            .group_id
            .and_then(|id| self.tab_groups.get(&id))
            .cloned();
        RetiredLayoutTab { tab, group }
    }

    pub(super) fn record_layout_change(
        &mut self,
        before: Option<LayoutState>,
        created: Option<EntityId>,
        retired: impl IntoIterator<Item = RetiredLayoutTab>,
        ctx: &AppContext,
    ) {
        let Some(before) = before else {
            self.layout_history.redo.clear();
            self.layout_history.prune_retired_tabs();
            return;
        };
        let present = before
            .layouts
            .keys()
            .copied()
            .chain(created)
            .filter(|id| self.get_pane_group_view_with_id(*id).is_some());
        if let Some(after) = self.capture_layout_state(present, ctx)
            && !before.matches(&after)
        {
            self.layout_history
                .record(LayoutChange { before, after }, retired);
        }
    }

    pub(super) fn capture_saved_layout_state(&self, ctx: &AppContext) -> Option<LayoutState> {
        let mut state =
            self.capture_layout_state(self.tabs.iter().map(|tab| tab.pane_group.id()), ctx)?;
        state.capture_tab_titles(&self.tabs, ctx);
        Some(state)
    }

    pub(super) fn record_saved_layout_change(
        &mut self,
        before: LayoutState,
        retired: Vec<RetiredLayoutTab>,
        ctx: &AppContext,
    ) {
        if let Some(after) = self.capture_saved_layout_state(ctx)
            && !before.matches(&after)
        {
            self.layout_history
                .record(LayoutChange { before, after }, retired);
        }
    }

    pub(super) fn record_pane_layout_resize(
        &mut self,
        group: EntityId,
        previous: PaneLayout,
        ctx: &AppContext,
    ) {
        let Some(after) = self.capture_layout_state([group], ctx) else {
            return;
        };
        let mut before = after.clone();
        before
            .layouts
            .get_mut(&group)
            .expect("Captured pane group")
            .tree = previous;
        if !before.matches(&after) {
            self.layout_history
                .record(LayoutChange { before, after }, None);
        }
    }

    pub(super) fn undo_layout_change(&mut self, ctx: &mut ViewContext<Self>) {
        self.replay_layout_change(Replay::Undo, ctx);
    }

    pub(super) fn redo_layout_change(&mut self, ctx: &mut ViewContext<Self>) {
        self.replay_layout_change(Replay::Redo, ctx);
    }

    fn replay_layout_change(&mut self, replay: Replay, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::EnhancedPaneManagement.is_enabled()
            || self.current_workspace_state.is_agent_management_view_open
            || self.docking_tab_drag.is_some()
            || self.docking_pane_drag.is_some()
            || self
                .tab_views()
                .any(|group| group.as_ref(ctx).is_being_resized())
        {
            return;
        }

        let mut skipped = false;
        loop {
            let change = match replay {
                Replay::Undo => self.layout_history.undo.pop_back(),
                Replay::Redo => self.layout_history.redo.pop(),
            };
            let Some(change) = change else {
                break;
            };
            let (from, to) = match replay {
                Replay::Undo => (&change.after, &change.before),
                Replay::Redo => (&change.before, &change.after),
            };
            match self.apply_layout_change(from, to, ctx) {
                ReplayResult::Applied => {
                    match replay {
                        Replay::Undo => self.layout_history.redo.push(change),
                        Replay::Redo => self.layout_history.undo.push_back(change),
                    }
                    self.layout_history.prune_retired_tabs();
                    return;
                }
                ReplayResult::Stale => skipped = true,
                ReplayResult::InsufficientSpace => {
                    match replay {
                        Replay::Undo => self.layout_history.undo.push_back(change),
                        Replay::Redo => self.layout_history.redo.push(change),
                    }
                    self.layout_history.prune_retired_tabs();
                    self.show_layout_history_message(
                        "Make the window larger to restore this layout.",
                        ctx,
                    );
                    return;
                }
            }
        }
        self.layout_history.prune_retired_tabs();
        let message = match (replay, skipped) {
            (Replay::Undo, false) => "No layout changes to undo.",
            (Replay::Redo, false) => "No layout changes to redo.",
            (_, true) => "Layout history expired because a tab or pane changed.",
        };
        self.show_layout_history_message(message, ctx);
    }

    fn show_layout_history_message(&mut self, message: &str, ctx: &mut ViewContext<Self>) {
        self.toast_stack.update(ctx, |stack, ctx| {
            stack.add_ephemeral_toast(super::DismissibleToast::default(message.to_owned()), ctx);
        });
    }

    fn apply_layout_change(
        &mut self,
        from: &LayoutState,
        to: &LayoutState,
        ctx: &mut ViewContext<Self>,
    ) -> ReplayResult {
        if !self
            .capture_layout_state(from.layouts.keys().copied(), ctx)
            .is_some_and(|mut current| {
                if !from.tab_titles.is_empty() {
                    current.capture_tab_titles(&self.tabs, ctx);
                }
                from.matches(&current)
            })
        {
            return ReplayResult::Stale;
        }
        if let Some(bounds) =
            ctx.element_position_by_id_at_last_frame(self.window_id, DOCKING_SURFACE_POSITION)
            && to
                .layouts
                .values()
                .any(|layout| !layout.fits(bounds.size(), ctx))
        {
            return ReplayResult::InsufficientSpace;
        }

        let from_owners = from.pane_owners();
        let to_owners = to.pane_owners();
        if from_owners.len() != to_owners.len()
            || !from_owners.keys().all(|pane| to_owners.contains_key(pane))
        {
            return ReplayResult::Stale;
        }

        let Some(destinations) = to
            .layouts
            .keys()
            .map(|&id| {
                let group = if let Some(group) = self.get_pane_group_view_with_id(id) {
                    group
                } else {
                    let retired = self.layout_history.retired_tabs.get(&id)?;
                    let group = &retired.tab.pane_group;
                    if group.as_ref(ctx).pane_ids().next().is_some() {
                        return None;
                    }
                    group
                };
                Some((id, group.clone()))
            })
            .collect::<Option<HashMap<_, _>>>()
        else {
            return ReplayResult::Stale;
        };

        let mut outgoing = HashMap::<_, Vec<_>>::new();
        for (&pane, &from_group) in &from_owners {
            if from_group != to_owners[&pane] {
                let group = self
                    .get_pane_group_view_with_id(from_group)
                    .expect("Validated group");
                if !group.as_ref(ctx).can_replay_docking_move(pane, ctx) {
                    return ReplayResult::Stale;
                }
                outgoing.entry(from_group).or_default().push(pane);
            }
        }

        self.cancel_tab_rename(ctx);
        let mut incoming = HashMap::<_, Vec<Box<dyn AnyPaneContent>>>::new();
        for (id, panes) in outgoing {
            let group = self
                .get_pane_group_view_with_id(id)
                .expect("Validated group");
            for pane in group.update(ctx, |group, ctx| {
                group.take_layout_history_panes(&panes, ctx)
            }) {
                incoming
                    .entry(to_owners[&pane.as_pane().id()])
                    .or_default()
                    .push(pane);
            }
        }
        for (&id, layout) in &to.layouts {
            destinations[&id].update(ctx, |group, ctx| {
                group.replay_docking_layout(
                    layout.clone(),
                    incoming.remove(&id).unwrap_or_default(),
                    ctx,
                );
                if let Some(title) = to.tab_titles.get(&id) {
                    group.set_saved_layout_tab_title(title.clone(), ctx);
                }
            });
        }

        self.replay_layout_tab_changes(from, to, ctx);
        ReplayResult::Applied
    }

    fn replay_layout_tab_changes(
        &mut self,
        from: &LayoutState,
        to: &LayoutState,
        ctx: &mut ViewContext<Self>,
    ) {
        let departing: HashSet<_> = from
            .tabs
            .iter()
            .filter(|tab| !to.tabs.iter().any(|target| target.id == tab.id))
            .map(|tab| tab.id)
            .collect();

        // Keep a live tab available even when replay replaces every current tab.
        for (index, tab) in to.tabs.iter().enumerate() {
            if let Some(retired) = self.layout_history.retired_tabs.remove(&tab.id) {
                if let Some(group) = retired.group {
                    self.tab_groups.entry(group.id).or_insert(group);
                }
                let insertion_index = self
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(_, tab)| !departing.contains(&tab.pane_group.id()))
                    .nth(index)
                    .map_or(self.tabs.len(), |(index, _)| index);
                self.tab_mru_order.push(tab.id);
                self.tabs.insert(insertion_index, retired.tab);
                if insertion_index <= self.active_tab_index {
                    self.active_tab_index += 1;
                }
            }
        }

        self.activate_tab_by_pane_group_id(to.active_tab, ctx);
        for index in (0..self.tabs.len()).rev() {
            let id = self.tabs[index].pane_group.id();
            if departing.contains(&id) {
                let retired = self.retired_layout_tab(index);
                self.remove_tab(index, false, true, ctx);
                self.layout_history.retired_tabs.insert(id, retired);
            }
        }
    }
}
