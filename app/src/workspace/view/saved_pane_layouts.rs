use std::collections::HashMap;

use uuid::Uuid;
use warpui::{AppContext, ViewContext, ViewHandle};

use super::Workspace;
use super::docking_overlay::DOCKING_SURFACE_POSITION;
use super::layout_history::LayoutState;
use crate::pane_group::tree::{PaneLayout, get_divider_layout_extent};
use crate::pane_group::{AnyPaneContent, PaneGroup, PaneId, SplitDirection, get_minimum_pane_size};
use crate::tab::TabData;
use crate::workspace::pane_layouts::{
    LayoutAxis, LayoutChild, LayoutDocument, LayoutKind, LayoutNode, LayoutPlan, LayoutSpace,
    LayoutStrategy, LayoutTab, plan_layout,
};

#[derive(Clone)]
pub(super) struct SavedLayoutPreview {
    pub(super) alternatives: Vec<LayoutPlan>,
    pub(super) pane_titles: HashMap<Uuid, String>,
    pub(super) space: LayoutSpace,
    source: LayoutState,
    sessions: Vec<Uuid>,
}

struct LiveLayout {
    tabs: Vec<LayoutTab>,
    sessions: Vec<Uuid>,
    panes: HashMap<Uuid, LivePane>,
}

struct LivePane {
    id: PaneId,
    tab: usize,
    title: String,
}

struct TabPlacement {
    tree: PaneLayout,
    stacks: Vec<Vec<PaneId>>,
    focused: PaneId,
    title: Option<String>,
}

impl Workspace {
    pub(super) fn capture_saved_pane_layout(
        &self,
        name: String,
        snapshot: bool,
        ctx: &AppContext,
    ) -> Result<LayoutDocument, String> {
        let kind = if snapshot {
            LayoutKind::Snapshot
        } else {
            LayoutKind::Template
        };
        let layout = self.saved_layout_inventory(kind, ctx)?;
        LayoutDocument::new(name, kind, layout.tabs).map_err(|error| error.to_string())
    }

    pub(super) fn preview_saved_pane_layout(
        &self,
        document: &LayoutDocument,
        ctx: &AppContext,
    ) -> Result<SavedLayoutPreview, String> {
        let (live, source, space) = self.saved_layout_source(ctx)?;
        let preserve = plan_layout(document, &live.sessions, space, LayoutStrategy::Preserve);
        let alternatives = match preserve {
            Ok(plan) => vec![plan],
            Err(error) => {
                let alternatives: Vec<_> = [
                    LayoutStrategy::Balance,
                    LayoutStrategy::Stack,
                    LayoutStrategy::Tabs,
                ]
                .into_iter()
                .filter_map(|strategy| plan_layout(document, &live.sessions, space, strategy).ok())
                .collect();
                if alternatives.is_empty() {
                    return Err(error.to_string());
                }
                alternatives
            }
        };
        Ok(SavedLayoutPreview {
            alternatives,
            pane_titles: live
                .panes
                .into_iter()
                .map(|(session, pane)| (session, pane.title))
                .collect(),
            space,
            source,
            sessions: live.sessions,
        })
    }

    pub(super) fn apply_saved_pane_layout(
        &mut self,
        document: &LayoutDocument,
        preview: &SavedLayoutPreview,
        alternative_index: usize,
        ctx: &mut ViewContext<Self>,
    ) -> Result<(), String> {
        let expected = preview
            .alternatives
            .get(alternative_index)
            .ok_or_else(|| "Choose a layout from the preview.".to_owned())?;
        let (live, before, space) = self.saved_layout_source(ctx)?;
        let stale = || "The panes or window changed. Preview the layout again.".to_owned();
        if space != preview.space
            || live.sessions != preview.sessions
            || !before.matches(&preview.source)
        {
            return Err(stale());
        }
        let plan =
            plan_layout(document, &live.sessions, space, expected.strategy).map_err(|_| stale())?;
        if plan != *expected {
            return Err(stale());
        }

        let active_pane = self
            .active_tab_pane_group()
            .as_ref(ctx)
            .focused_pane_id(ctx);
        let mut destinations = HashMap::new();
        let placements = plan
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let mut stacks = Vec::new();
                let mut leaves = Vec::new();
                let tree = place_layout_node(
                    &tab.root,
                    index,
                    &live,
                    &mut destinations,
                    &mut leaves,
                    &mut stacks,
                )?;
                let previous_group = self.tabs.get(index).map(|tab| tab.pane_group.as_ref(ctx));
                let first = leaves
                    .first()
                    .copied()
                    .ok_or_else(|| "A preview tab has no visible terminal.".to_owned())?;
                let focused = std::iter::once(active_pane)
                    .chain(previous_group.map(|group| group.focused_pane_id(ctx)))
                    .find(|pane| leaves.contains(pane))
                    .unwrap_or(first);
                let title = tab.name.clone().or_else(|| {
                    (document.kind == LayoutKind::Template)
                        .then(|| previous_group.and_then(|group| group.custom_title(ctx)))
                        .flatten()
                });
                Ok(TabPlacement {
                    tree,
                    stacks,
                    focused,
                    title,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        if placements.is_empty() || destinations.len() != live.panes.len() {
            return Err("The preview does not account for every terminal pane.".to_owned());
        }
        let active_index = *destinations
            .get(&active_pane)
            .ok_or_else(|| "The preview does not include the active terminal.".to_owned())?;
        let mut outgoing = vec![Vec::new(); self.tabs.len()];
        for session in &live.sessions {
            let pane = &live.panes[session];
            if pane.tab != destinations[&pane.id] {
                outgoing[pane.tab].push(pane.id);
            }
        }

        self.cancel_tab_rename(ctx);
        let mut incoming = HashMap::<usize, Vec<Box<dyn AnyPaneContent>>>::new();
        for (tab, panes) in self
            .tabs
            .iter()
            .zip(outgoing)
            .filter(|(_, panes)| !panes.is_empty())
        {
            for pane in tab.pane_group.update(ctx, |group, ctx| {
                group.take_layout_history_panes(&panes, ctx)
            }) {
                incoming
                    .entry(destinations[&pane.as_pane().id()])
                    .or_default()
                    .push(pane);
            }
        }
        let tab_count = placements.len();
        for (index, placement) in placements.into_iter().enumerate() {
            let mut panes = incoming.remove(&index).unwrap_or_default();
            let group = if let Some(tab) = self.tabs.get(index) {
                tab.pane_group.clone()
            } else {
                let initial = panes
                    .iter()
                    .position(|pane| pane.as_pane().id() == placement.focused)
                    .expect("A new layout tab receives its focused pane");
                self.append_layout_tab(panes.swap_remove(initial), ctx)
            };
            group.update(ctx, |group, ctx| {
                group.apply_saved_layout_tree(
                    placement.tree,
                    placement.stacks,
                    placement.focused,
                    panes,
                    ctx,
                );
                group.set_saved_layout_tab_title(placement.title, ctx);
            });
        }

        let mut retired = Vec::new();
        for index in (tab_count..self.tabs.len()).rev() {
            retired.push(self.retired_layout_tab(index));
            self.remove_tab(index, false, true, ctx);
        }
        self.activate_tab_internal(active_index, ctx);
        self.record_saved_layout_change(before, retired, ctx);
        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
        Ok(())
    }

    pub(super) fn append_layout_tab(
        &mut self,
        pane: Box<dyn AnyPaneContent>,
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<PaneGroup> {
        let group = ctx.add_typed_action_view(|ctx| {
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
        ctx.subscribe_to_view(&group, |me, group, event, ctx| {
            me.handle_file_tree_event(group, event, ctx)
        });
        self.tab_mru_order.push(group.id());
        self.tabs.push(TabData::new(group.clone()));
        group
    }

    fn saved_layout_source(
        &self,
        ctx: &AppContext,
    ) -> Result<(LiveLayout, LayoutState, LayoutSpace), String> {
        if self
            .tabs
            .iter()
            .any(|tab| tab.pinned || tab.group_id.is_some())
        {
            return Err(
                "Apply pane layouts in a window without pinned or grouped tabs.".to_owned(),
            );
        }
        if self
            .tab_views()
            .any(|group| group.as_ref(ctx).has_saved_layout_side_panel())
        {
            return Err(
                "Close the tools and assistant panels before applying a pane layout.".to_owned(),
            );
        }
        let live = self.saved_layout_inventory(LayoutKind::Snapshot, ctx)?;
        let source = self
            .capture_saved_layout_state(ctx)
            .ok_or_else(|| "This window contains a tab that cannot be rearranged.".to_owned())?;
        let bounds = ctx
            .element_position_by_id_at_last_frame(self.window_id, DOCKING_SURFACE_POSITION)
            .ok_or_else(|| {
                "Open a visible terminal tab before previewing a pane layout.".to_owned()
            })?;
        let space = LayoutSpace {
            width: bounds.width(),
            height: bounds.height(),
            minimum: get_minimum_pane_size(ctx),
            divider: get_divider_layout_extent(),
        };
        Ok((live, source, space))
    }

    fn saved_layout_inventory(
        &self,
        kind: LayoutKind,
        ctx: &AppContext,
    ) -> Result<LiveLayout, String> {
        if self.tabs.is_empty() || self.current_workspace_state.is_agent_management_view_open {
            return Err("Open a terminal window to use pane layouts.".to_owned());
        }
        if self.docking_pane_drag.is_some()
            || self.docking_tab_drag.is_some()
            || self.current_workspace_state.is_tab_being_dragged
            || self.tab_views().any(|group| {
                let group = group.as_ref(ctx);
                group.is_being_resized()
                    || group.is_previewing_resize()
                    || group.any_pane_being_dragged(ctx)
            })
        {
            return Err("Finish dragging or resizing before using a pane layout.".to_owned());
        }
        let mut layout = LiveLayout {
            tabs: Vec::new(),
            sessions: Vec::new(),
            panes: HashMap::new(),
        };
        for (tab_index, tab) in self.tabs.iter().enumerate() {
            if tab.detached
                || self.has_docking_child_tab(tab.pane_group.id(), ctx)
                || !PaneGroup::can_move_terminal_layout_to_window(&tab.pane_group, ctx)
            {
                return Err("Use a window with only movable terminal panes. Restore any maximized panes and turn off synchronized input.".to_owned());
            }
            let group = tab.pane_group.as_ref(ctx);
            let snapshot =
                PaneGroup::capture_docking_layout(&tab.pane_group, ctx).ok_or_else(|| {
                    "This window contains a tab that cannot be rearranged.".to_owned()
                })?;
            let root =
                capture_layout_node(snapshot.tree, kind, group, tab_index, &mut layout, ctx)?;
            let name = if kind == LayoutKind::Snapshot {
                group
                    .custom_title(ctx)
                    .map(|title| title.trim().to_owned())
                    .filter(|title| !title.is_empty())
            } else {
                None
            };
            layout.tabs.push(LayoutTab { name, root });
        }
        Ok(layout)
    }
}

fn capture_layout_node(
    tree: PaneLayout,
    kind: LayoutKind,
    group: &PaneGroup,
    tab: usize,
    layout: &mut LiveLayout,
    ctx: &AppContext,
) -> Result<LayoutNode<Option<Uuid>>, String> {
    match tree {
        PaneLayout::Leaf(pane) => {
            let members = group
                .docked_stack_members(pane)
                .unwrap_or(std::slice::from_ref(&pane));
            let active = members
                .iter()
                .position(|&member| member == pane)
                .unwrap_or(0);
            let sessions = members[active..]
                .iter()
                .chain(&members[..active])
                .copied()
                .map(|id| {
                    if kind == LayoutKind::Template {
                        return Ok(None);
                    }
                    let session = group.saved_layout_session_id(id, ctx).ok_or_else(|| {
                        "Wait for every terminal to connect before using a saved pane layout."
                            .to_owned()
                    })?;
                    let title = group
                        .pane_by_id(id)
                        .map(|pane| {
                            let configuration = pane.pane_configuration();
                            let configuration = configuration.as_ref(ctx);
                            configuration
                                .custom_vertical_tabs_title()
                                .unwrap_or_else(|| configuration.title())
                                .to_owned()
                        })
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| "Terminal pane".to_owned());
                    if layout
                        .panes
                        .insert(session, LivePane { id, tab, title })
                        .is_some()
                    {
                        return Err("Two terminals have the same session identity.".to_owned());
                    }
                    layout.sessions.push(session);
                    Ok(Some(session))
                })
                .collect::<Result<Vec<_>, String>>()?;
            if sessions.len() == 1 {
                Ok(LayoutNode::Pane {
                    session: sessions[0],
                })
            } else {
                Ok(LayoutNode::Stack { sessions })
            }
        }
        PaneLayout::Branch(direction, children) => {
            let axis = match direction {
                SplitDirection::Horizontal => LayoutAxis::Horizontal,
                SplitDirection::Vertical => LayoutAxis::Vertical,
            };
            let children = children
                .into_iter()
                .map(|(weight, child)| {
                    let node = capture_layout_node(child, kind, group, tab, layout, ctx)?;
                    Ok(LayoutChild { weight, node })
                })
                .collect::<Result<_, String>>()?;
            Ok(LayoutNode::Split { axis, children })
        }
    }
}

fn place_layout_node(
    node: &LayoutNode<Uuid>,
    tab: usize,
    live: &LiveLayout,
    destinations: &mut HashMap<PaneId, usize>,
    leaves: &mut Vec<PaneId>,
    stacks: &mut Vec<Vec<PaneId>>,
) -> Result<PaneLayout, String> {
    let mut place_pane = |session: &Uuid| {
        let pane = live
            .panes
            .get(session)
            .ok_or_else(|| "A terminal in the preview is missing.".to_owned())?;
        if destinations.insert(pane.id, tab).is_some() {
            return Err("A terminal appears more than once in the preview.".to_owned());
        }
        Ok(pane.id)
    };
    match node {
        LayoutNode::Pane { session } => {
            let pane = place_pane(session)?;
            leaves.push(pane);
            Ok(PaneLayout::Leaf(pane))
        }
        LayoutNode::Stack { sessions } => {
            let members = sessions
                .iter()
                .map(place_pane)
                .collect::<Result<Vec<_>, String>>()?;
            let active = *members
                .first()
                .ok_or_else(|| "A preview stack is empty.".to_owned())?;
            stacks.push(members);
            leaves.push(active);
            Ok(PaneLayout::Leaf(active))
        }
        LayoutNode::Split { axis, children } => {
            let direction = match axis {
                LayoutAxis::Horizontal => SplitDirection::Horizontal,
                LayoutAxis::Vertical => SplitDirection::Vertical,
            };
            let children = children
                .iter()
                .map(|child| {
                    let pane =
                        place_layout_node(&child.node, tab, live, destinations, leaves, stacks)?;
                    Ok((child.weight, pane))
                })
                .collect::<Result<_, String>>()?;
            Ok(PaneLayout::Branch(direction, children))
        }
    }
}
