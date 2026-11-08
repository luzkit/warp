use std::collections::{HashMap, HashSet};

use pathfinder_geometry::rect::RectF;
use warpui::platform::FullscreenState;
use warpui::{AppContext, Entity, EntityId, SingletonEntity, ViewHandle, WindowId};

use super::Workspace;
use super::docking_overlay::DOCKING_SURFACE_POSITION;
use crate::features::FeatureFlag;
use crate::pane_group::tree::PaneLayout;
use crate::pane_group::{Direction, DockingLayoutSnapshot, PaneGroup, PaneId, SplitDirection};
use crate::workspace::cross_window_tab_drag::CrossWindowTabDrag;
use crate::workspace::{PaneViewLocator, WorkspaceRegistry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PaneLocation {
    pub(super) window_id: WindowId,
    pub(super) pane_group_id: EntityId,
    pub(super) pane_id: PaneId,
}

impl PaneLocation {
    fn key(self) -> GroupKey {
        (self.window_id, self.pane_group_id)
    }

    fn locator(self) -> PaneViewLocator {
        PaneViewLocator {
            pane_group_id: self.pane_group_id,
            pane_id: self.pane_id,
        }
    }
}

#[derive(Default)]
pub(in crate::workspace) struct PaneSelection {
    members: Vec<PaneLocation>,
}

impl PaneSelection {
    pub(super) fn members(&self) -> &[PaneLocation] {
        &self.members
    }

    pub(super) fn toggle(&mut self, location: PaneLocation) -> bool {
        if let Some(index) = self
            .members
            .iter()
            .position(|member| member.pane_id == location.pane_id)
        {
            self.members.remove(index);
            false
        } else {
            self.members.push(location);
            true
        }
    }

    pub(super) fn clear(&mut self) {
        self.members.clear();
    }
}

impl Entity for PaneSelection {
    type Event = ();
}

impl SingletonEntity for PaneSelection {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GroupDockDestination {
    Split {
        target: PaneLocation,
        direction: Direction,
    },
    NewTab {
        window_id: WindowId,
    },
}

impl GroupDockDestination {
    fn window_id(self) -> WindowId {
        match self {
            Self::Split { target, .. } => target.window_id,
            Self::NewTab { window_id } => window_id,
        }
    }
}

type GroupKey = (WindowId, EntityId);

#[derive(Clone)]
struct SourceGroup {
    key: GroupKey,
    snapshot: DockingLayoutSnapshot,
    moving: Vec<PaneId>,
    remaining: Option<(PaneLayout, PaneId)>,
}

#[derive(Clone)]
pub(super) struct GroupDockPlan {
    sources: Vec<PaneLocation>,
    groups: Vec<SourceGroup>,
    destination: GroupDockDestination,
    destination_snapshot: Option<DockingLayoutSnapshot>,
    destination_tree: PaneLayout,
    viewport: RectF,
    fullscreen_state: Option<FullscreenState>,
    preview: Option<(RectF, RectF)>,
}

impl GroupDockPlan {
    pub(super) fn preview(&self) -> Option<(RectF, RectF)> {
        self.preview
    }

    fn matches(&self, current: &Self) -> bool {
        self.viewport == current.viewport
            && self.fullscreen_state == current.fullscreen_state
            && self.destination_tree == current.destination_tree
            && self.preview == current.preview
            && self.groups.len() == current.groups.len()
            && self
                .groups
                .iter()
                .zip(&current.groups)
                .all(|(before, after)| {
                    before.key == after.key && before.snapshot.matches(&after.snapshot)
                })
            && match (&self.destination_snapshot, &current.destination_snapshot) {
                (Some(before), Some(after)) => before.matches(after),
                (None, None) => true,
                _ => false,
            }
    }
}

fn docking_workspace(window: WindowId, ctx: &AppContext) -> Option<ViewHandle<Workspace>> {
    let workspace = WorkspaceRegistry::as_ref(ctx).get(window, ctx)?;
    workspace
        .try_as_ref(ctx)?
        .accepts_window_docking(ctx)
        .then_some(workspace)
}

fn docking_group(
    workspace: &Workspace,
    group: EntityId,
    ctx: &AppContext,
) -> Result<ViewHandle<PaneGroup>, &'static str> {
    if workspace.has_docking_child_tab(group, ctx) {
        return Err("Keep linked agent panes in their original tab.");
    }
    let index = workspace.window_docking_tab_index(group, ctx)
        .ok_or("This tab is unavailable for docking.");
    let handle = &workspace.tabs[index?].pane_group;
    let group = handle.as_ref(ctx);
    if group.is_previewing_resize() || group.any_pane_being_dragged(ctx) {
        return Err("Finish dragging or resizing before docking.");
    }
    if let Some(reason) = PaneGroup::group_docking_restriction(handle, ctx) {
        return Err(reason);
    }
    if group.has_saved_layout_side_panel() {
        return Err("Close the tools and assistant panels before docking.");
    }
    Ok(handle.clone())
}

impl Workspace {
    pub(super) fn pane_group_selection_restriction(
        &self,
        pane_group_id: EntityId,
        ctx: &AppContext,
    ) -> Option<&'static str> {
        docking_group(self, pane_group_id, ctx).err()
    }
}

pub(super) fn pane_selection_restriction(
    location: PaneLocation,
    ctx: &AppContext,
) -> Option<&'static str> {
    let Some(workspace) = docking_workspace(location.window_id, ctx) else {
        return Some("This window is unavailable for docking.");
    };
    match docking_group(workspace.as_ref(ctx), location.pane_group_id, ctx) {
        Ok(group) if group.as_ref(ctx).has_pane_id(location.pane_id) => None,
        Ok(_) => Some("This pane is no longer available."),
        Err(reason) => Some(reason),
    }
}

pub(super) fn plan_group_dock(
    sources: &[PaneLocation],
    destination: GroupDockDestination,
    ctx: &AppContext,
) -> Result<GroupDockPlan, String> {
    if !FeatureFlag::EnhancedPaneManagement.is_enabled()
        || CrossWindowTabDrag::as_ref(ctx).is_active()
    {
        return Err("Finish the current drag before docking panes.".to_owned());
    }
    if sources.is_empty() {
        return Err("Select at least one pane.".to_owned());
    }
    let destination_window = destination.window_id();
    let workspace = docking_workspace(destination_window, ctx)
        .ok_or_else(|| "The destination window is unavailable.".to_owned())?;
    let workspace = workspace.as_ref(ctx);
    let team = workspace.team_uid(ctx);
    let viewport = ctx
        .element_position_by_id_at_last_frame(destination_window, DOCKING_SURFACE_POSITION)
        .filter(|bounds| {
            bounds.width().is_finite()
                && bounds.height().is_finite()
                && bounds.width() > 0.
                && bounds.height() > 0.
        })
        .ok_or_else(|| "The destination window does not have a visible pane area.".to_owned())?;

    let mut seen = HashSet::new();
    let mut editor_files = HashSet::new();
    let mut groups = Vec::<SourceGroup>::new();
    for &source in sources {
        if !seen.insert(source.pane_id) {
            return Err("A pane appears more than once in the selection.".to_owned());
        }
        let source_workspace = docking_workspace(source.window_id, ctx)
            .ok_or_else(|| "A selected pane’s window is unavailable.".to_owned())?;
        let source_workspace = source_workspace.as_ref(ctx);
        if source_workspace.team_uid(ctx) != team {
            return Err("Move panes between windows in the same team.".to_owned());
        }
        let group = docking_group(source_workspace, source.pane_group_id, ctx)
            .map_err(str::to_owned)?;
        let group_ref = group.as_ref(ctx);
        if !group_ref.has_pane_id(source.pane_id) {
            return Err("A selected pane is no longer available for docking.".to_owned());
        }
        if let Some(editor) = group_ref.code_pane_by_id(source.pane_id) {
            if source.window_id != destination_window {
                return Err("Keep this editor in its current window.".to_owned());
            }
            for file in editor.docking_locations(ctx) {
                if !editor_files.insert(file) {
                    return Err("Two selected editors have the same file open.".to_owned());
                }
            }
        }
        if let Some(source_group) = groups.iter_mut().find(|group| group.key == source.key()) {
            source_group.moving.push(source.pane_id);
        } else {
            let snapshot = PaneGroup::capture_docking_layout(&group, ctx)
                .ok_or_else(|| "A selected tab is unavailable for docking.".to_owned())?;
            groups.push(SourceGroup {
                key: source.key(),
                snapshot,
                moving: vec![source.pane_id],
                remaining: None,
            });
        }
    }

    let mut subtrees = Vec::with_capacity(groups.len());
    for group in &mut groups {
        let moving: HashSet<_> = group.moving.iter().copied().collect();
        let snapshot = &group.snapshot.tree;
        let mut selected = snapshot.clone().into_pane_data();
        for pane in selected.pane_ids() {
            if !moving.contains(&pane) {
                selected.remove(pane);
            }
        }
        subtrees.push(selected.layout_snapshot());

        let mut remaining = snapshot.clone().into_pane_data();
        if group.moving.len() != remaining.len() {
            for &pane in &group.moving {
                remaining.remove(pane);
            }
            let workspace = docking_workspace(group.key.0, ctx).expect("Validated source window");
            let original_focus = workspace
                .as_ref(ctx)
                .get_pane_group_view_with_id(group.key.1)
                .expect("Validated source tab")
                .as_ref(ctx)
                .focused_pane_id(ctx);
            let focused = if remaining.is_pane_in_tree(original_focus) {
                original_focus
            } else {
                remaining.pane_ids()[0]
            };
            group.remaining = Some((remaining.layout_snapshot(), focused));
        }
    }

    let (destination_snapshot, destination_tree, preview) = match destination {
        GroupDockDestination::Split { target, direction } => {
            if seen.contains(&target.pane_id) {
                return Err("Choose a destination outside the selection.".to_owned());
            }
            let target_group = docking_group(workspace, target.pane_group_id, ctx)
                .map_err(str::to_owned)?;
            let target_ref = target_group.as_ref(ctx);
            if !target_ref.has_pane_id(target.pane_id) {
                return Err("The destination pane is unavailable for docking.".to_owned());
            }
            if !editor_files.is_empty() && target_ref.pane_ids()
                .filter(|pane| !seen.contains(pane))
                .filter_map(|pane| target_ref.code_pane_by_id(pane))
                .flat_map(|editor| editor.docking_locations(ctx))
                .any(|file| editor_files.contains(&file))
            {
                return Err("The destination already has one of the selected files open.".to_owned());
            }
            let snapshot = PaneGroup::capture_docking_layout(&target_group, ctx)
                .ok_or_else(|| "The destination tab is unavailable for docking.".to_owned())?;
            let destination_tree = groups
                .iter()
                .find(|group| group.key == target.key())
                .and_then(|group| group.remaining.as_ref().map(|(tree, _)| tree))
                .unwrap_or(&snapshot.tree);
            let axes = match direction {
                Direction::Left | Direction::Right => {
                    [SplitDirection::Vertical, SplitDirection::Horizontal]
                }
                Direction::Up | Direction::Down => {
                    [SplitDirection::Horizontal, SplitDirection::Vertical]
                }
            };
            let (tree, target_bounds, group_bounds) = axes
                .into_iter()
                .find_map(|axis| {
                    PaneGroup::plan_group_docking_split(
                        destination_tree.clone(),
                        combined_tree(&subtrees, axis),
                        target.pane_id,
                        direction,
                        viewport,
                        ctx,
                    )
                })
                .ok_or_else(|| {
                    "There is not enough room for the selected panes at this split.".to_owned()
                })?;
            (Some(snapshot), tree, Some((target_bounds, group_bounds)))
        }
        GroupDockDestination::NewTab { .. } => {
            let tree = [SplitDirection::Horizontal, SplitDirection::Vertical]
                .into_iter()
                .map(|axis| combined_tree(&subtrees, axis))
                .find(|tree| {
                    PaneGroup::group_docking_layout_fits(tree.clone(), viewport.size(), ctx)
                })
                .ok_or_else(|| {
                    "There is not enough room for the selected panes in this window.".to_owned()
                })?;
            (None, tree, None)
        }
    };

    Ok(GroupDockPlan {
        sources: sources.to_vec(),
        groups,
        destination,
        destination_snapshot,
        destination_tree,
        viewport,
        fullscreen_state: ctx.windows().platform_window(destination_window)
            .map(|window| window.fullscreen_state()),
        preview,
    })
}

fn combined_tree(subtrees: &[PaneLayout], axis: SplitDirection) -> PaneLayout {
    if let [tree] = subtrees {
        tree.clone()
    } else {
        PaneLayout::Branch(
            axis,
            subtrees.iter().cloned().map(|tree| (1., tree)).collect(),
        )
    }
}

fn active_pane_location(workspace: &Workspace, ctx: &AppContext) -> PaneLocation {
    let group = workspace.active_tab_pane_group();
    PaneLocation {
        window_id: workspace.window_id,
        pane_group_id: group.id(),
        pane_id: group.as_ref(ctx).focused_pane_id(ctx),
    }
}

fn surviving_focus(origin: PaneLocation, ctx: &AppContext) -> Option<PaneLocation> {
    let workspace = docking_workspace(origin.window_id, ctx)?;
    let workspace = workspace.as_ref(ctx);
    if workspace
        .get_pane_group_view_with_id(origin.pane_group_id)
        .is_some_and(|group| group.as_ref(ctx).has_pane_id(origin.pane_id))
    {
        Some(origin)
    } else {
        Some(active_pane_location(workspace, ctx))
    }
}

pub(super) fn commit_group_dock(
    plan: GroupDockPlan,
    focus_moved: bool,
    ctx: &mut AppContext,
) -> Result<(), String> {
    let stale = || "The panes or window changed. Preview the move again.".to_owned();
    let current = plan_group_dock(&plan.sources, plan.destination, ctx).map_err(|_| stale())?;
    if !plan.matches(&current) {
        return Err(stale());
    }

    let destination_window = current.destination.window_id();
    let workspace = docking_workspace(destination_window, ctx).expect("Validated destination");
    let target = match current.destination {
        GroupDockDestination::Split { target, .. } => Some(target),
        GroupDockDestination::NewTab { .. } => None,
    };
    let history_before = current
        .groups
        .iter()
        .all(|group| group.key.0 == destination_window)
        .then(|| {
            workspace.as_ref(ctx).capture_layout_state(
                current
                    .groups
                    .iter()
                    .map(|group| group.key.1)
                    .chain(target.map(|target| target.pane_group_id)),
                ctx,
            )
        })
        .flatten();
    let origin = ctx
        .windows()
        .active_window()
        .and_then(|window| docking_workspace(window, ctx))
        .map(|workspace| active_pane_location(workspace.as_ref(ctx), ctx));
    let preferred_pane = current.sources[0].pane_id;
    let destination_pane = if focus_moved {
        preferred_pane
    } else {
        target.map_or(preferred_pane, |target| {
            workspace
                .as_ref(ctx)
                .get_pane_group_view_with_id(target.pane_group_id)
                .expect("Validated destination tab")
                .as_ref(ctx)
                .focused_pane_id(ctx)
        })
    };

    let mut incoming = Vec::new();
    let mut empty = HashMap::<WindowId, Vec<EntityId>>::new();
    for source in current.groups {
        if target.is_some_and(|target| target.key() == source.key) {
            continue;
        }
        let (source_window, source_tab) = source.key;
        let source_workspace =
            docking_workspace(source_window, ctx).expect("Validated source window");
        let group = source_workspace
            .as_ref(ctx)
            .get_pane_group_view_with_id(source_tab)
            .expect("Validated source tab")
            .clone();
        let mut panes = group.update(ctx, |group, ctx| {
            let panes = group.take_layout_history_panes(&source.moving, ctx);
            if let Some((tree, focused)) = source.remaining {
                group.apply_saved_layout_tree(tree, Vec::new(), focused, Vec::new(), ctx);
            }
            panes
        });
        if group.as_ref(ctx).pane_ids().next().is_none() {
            empty.entry(source_window).or_default().push(source_tab);
        }
        if source_window != destination_window {
            for &pane in &source.moving {
                ctx.transfer_view_tree_to_window(
                    pane.creation_order_id(),
                    source_window,
                    destination_window,
                );
            }
        }
        incoming.append(&mut panes);
    }

    let destination_id = workspace.update(ctx, |workspace, ctx| {
        let group = if let Some(target) = target {
            workspace
                .get_pane_group_view_with_id(target.pane_group_id)
                .expect("Validated destination tab")
                .clone()
        } else {
            let index = incoming
                .iter()
                .position(|pane| pane.as_pane().id() == destination_pane)
                .expect("New tab receives the focused pane");
            workspace.append_layout_tab(incoming.swap_remove(index), ctx)
        };
        group.update(ctx, |group, ctx| {
            group.apply_saved_layout_tree(
                current.destination_tree,
                Vec::new(),
                destination_pane,
                incoming,
                ctx,
            );
        });
        group.id()
    });

    for (window, empty_groups) in empty
        .iter()
        .filter(|(window, _)| **window != destination_window)
    {
        let source_workspace = docking_workspace(*window, ctx).expect("Validated source window");
        source_workspace.update(ctx, |workspace, ctx| {
            workspace.remove_empty_group_docking_tabs(empty_groups, ctx);
        });
    }

    let retired = workspace.update(ctx, |workspace, ctx| {
        let empty_groups = empty.remove(&destination_window).unwrap_or_default();
        let retired = if history_before.is_some() {
            workspace
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, tab)| empty_groups.contains(&tab.pane_group.id()))
                .map(|(index, _)| workspace.retired_layout_tab(index))
                .collect()
        } else {
            Vec::new()
        };
        if !empty_groups.is_empty() {
            workspace.remove_empty_group_docking_tabs(&empty_groups, ctx);
        }
        retired
    });

    let destination_focus = PaneLocation {
        window_id: destination_window,
        pane_group_id: destination_id,
        pane_id: destination_pane,
    };
    let focus = origin
        .filter(|_| !focus_moved)
        .and_then(|origin| surviving_focus(origin, ctx))
        .unwrap_or(destination_focus);
    if ctx.windows().active_window() != Some(focus.window_id) {
        ctx.windows().show_window_and_focus_app(focus.window_id);
    }
    let focused_workspace =
        docking_workspace(focus.window_id, ctx).expect("Validated focus window");
    focused_workspace.update(ctx, |workspace, ctx| {
        workspace.focus_pane(focus.locator(), ctx);
    });
    if history_before.is_some() {
        workspace.update(ctx, |workspace, ctx| {
            workspace.record_layout_change(
                history_before,
                target.is_none().then_some(destination_id),
                retired,
                ctx,
            );
        });
    }

    PaneSelection::handle(ctx).update(ctx, |selection, ctx| {
        selection
            .members
            .retain(|member| !current.sources.contains(member));
        ctx.notify();
    });
    ctx.dispatch_global_action("workspace:save_app", &());
    Ok(())
}
