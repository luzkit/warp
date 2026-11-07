use pathfinder_geometry::rect::RectF;
use warpui::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, OffsetPositioning,
    PositionedElementAnchor, PositionedElementOffsetBounds, Radius, Rect, SavePosition, Stack,
    Text,
};
use warpui::event::Event;
use warpui::geometry::vector::{Vector2F, vec2f};
use warpui::{AppContext, Element, EntityId, SingletonEntity, ViewContext, WindowId};

use super::{Workspace, tab_bar_rects_for_window};
use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::pane_group::tree::get_divider_thickness;
use crate::pane_group::{Direction, PaneGroup, PaneId, get_minimum_pane_size};
use crate::workspace::WorkspaceRegistry;

#[derive(Clone, Copy)]
pub(super) struct DockingTabDrag {
    pub(super) source: EntityId,
    destination: EntityId,
    pane: Option<PaneId>,
    canceled: bool,
}

struct DockingTarget {
    pane: PaneId,
    bounds: RectF,
    horizontal: bool,
    vertical: bool,
}

impl DockingTarget {
    fn direction_at(&self, cursor: Vector2F) -> Option<Direction> {
        let relative = cursor - self.bounds.origin();
        let x = relative.x() / self.bounds.width();
        let y = relative.y() / self.bounds.height();
        [
            (Direction::Left, x, self.horizontal),
            (Direction::Right, 1. - x, self.horizontal),
            (Direction::Up, y, self.vertical),
            (Direction::Down, 1. - y, self.vertical),
        ]
        .into_iter()
        .filter(|(_, distance, enabled)| *enabled && (0. ..=0.30).contains(distance))
        .min_by(|(_, a, _), (_, b, _)| a.total_cmp(b))
        .map(|(direction, _, _)| direction)
    }

    fn preview(&self, direction: Direction) -> RectF {
        let divider = get_divider_thickness();
        let width = (self.bounds.width() - divider) / 2.;
        let height = (self.bounds.height() - divider) / 2.;
        let (offset, size) = match direction {
            Direction::Left => (vec2f(0., 0.), vec2f(width, self.bounds.height())),
            Direction::Right => (
                vec2f(width + divider, 0.),
                vec2f(width, self.bounds.height()),
            ),
            Direction::Up => (vec2f(0., 0.), vec2f(self.bounds.width(), height)),
            Direction::Down => (
                vec2f(0., height + divider),
                vec2f(self.bounds.width(), height),
            ),
        };
        RectF::new(offset, size)
    }
}

pub(super) fn tab_drag_owns_pointer(event: &Event, window_id: WindowId, app: &AppContext) -> bool {
    if !matches!(
        event,
        Event::LeftMouseDragged { .. } | Event::LeftMouseUp { .. }
    ) {
        return false;
    }
    WorkspaceRegistry::as_ref(app)
        .get(window_id, app)
        .is_some_and(|workspace| {
            let workspace = workspace.as_ref(app);
            workspace.docking_tab_drag.is_some()
                || workspace.tabs.iter().enumerate().any(|(index, tab)| {
                    index != workspace.active_tab_index
                        && tab.draggable_state.is_tracking_pointer()
                        && PaneGroup::dockable_single_terminal(&tab.pane_group, app).is_some()
                })
        })
}

pub(crate) fn handle_docking_input(
    event: &Event,
    window_id: WindowId,
    ctx: &mut AppContext,
) -> bool {
    let cancel = matches!(
        event,
        Event::KeyDown { keystroke, .. } if keystroke.is_unmodified_key("escape")
    );
    if !FeatureFlag::EnhancedPaneManagement.is_enabled()
        || !(cancel || matches!(event, Event::LeftMouseDown { .. }))
    {
        return false;
    }
    let Some(workspace) = WorkspaceRegistry::as_ref(ctx).get(window_id, ctx) else {
        return false;
    };
    if workspace.as_ref(ctx).docking_tab_drag.is_none() {
        return false;
    }
    if cancel {
        workspace.update(ctx, Workspace::cancel_docking_tab_drag);
    } else {
        // A new press can recover a gesture whose source stopped receiving mouse events.
        workspace.update(ctx, Workspace::reset_docking_tab_drag);
    }
    cancel
}

impl Workspace {
    pub(super) fn start_docking_tab_drag(&mut self, source: EntityId, app: &AppContext) {
        if !FeatureFlag::EnhancedPaneManagement.is_enabled() {
            return;
        }
        let destination = self.active_tab_pane_group();
        if source != destination.id()
            && let Some(tab) = self.tabs.iter().find(|tab| tab.pane_group.id() == source)
            && tab.draggable_state.is_dragging()
            && !tab.detached
            && PaneGroup::dockable_single_terminal(&tab.pane_group, app).is_some()
        {
            for mouse_state in [
                &tab.tab_mouse_state,
                &tab.tooltip_mouse_state,
                &tab.close_mouse_state,
            ] {
                if let Ok(mut state) = mouse_state.lock() {
                    state.reset_interaction_state();
                }
            }
            self.docking_tab_drag = Some(DockingTabDrag {
                source,
                destination: destination.id(),
                pane: PaneGroup::dockable_single_terminal(destination, app),
                canceled: false,
            });
        }
    }

    fn cancel_docking_tab_drag(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some(drag) = &mut self.docking_tab_drag {
            drag.canceled = true;
            if let Some(tab) = self
                .tabs
                .iter()
                .find(|tab| tab.pane_group.id() == drag.source)
            {
                tab.draggable_state.set_suppress_overlay_paint(true);
            }
            ctx.notify();
        }
    }

    fn reset_docking_tab_drag(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some(drag) = self.docking_tab_drag.take() {
            if let Some(tab) = self
                .tabs
                .iter()
                .find(|tab| tab.pane_group.id() == drag.source)
            {
                tab.draggable_state.cancel_drag();
                tab.draggable_state.set_suppress_overlay_paint(false);
            }
            self.current_workspace_state.is_tab_being_dragged = false;
            ctx.notify();
        }
    }

    pub(super) fn finish_docking_tab_drag(
        &mut self,
        source: EntityId,
        tab_position: RectF,
        cursor: Vector2F,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let Some(drag) = self.docking_tab_drag else {
            return false;
        };
        if source != drag.source {
            return true;
        }
        self.docking_tab_drag = None;
        self.current_workspace_state.is_tab_being_dragged = false;
        if let Some(source_index) = self
            .tabs
            .iter()
            .position(|tab| tab.pane_group.id() == drag.source)
        {
            self.tabs[source_index]
                .draggable_state
                .set_suppress_overlay_paint(false);
            if !drag.canceled {
                if let Some(target) = self.docking_target(drag, cursor, ctx)
                    && let Some(direction) = target.direction_at(cursor)
                {
                    self.dock_terminal_tab(drag.source, target.pane, direction, ctx);
                } else if tab_bar_rects_for_window(self.window_id, ctx)
                    .iter()
                    .any(|rect| rect.contains_point(cursor))
                {
                    self.reorder_tab_in_tab_bar(source_index, tab_position, ctx);
                    if let Some(tab) = self
                        .tabs
                        .iter_mut()
                        .find(|tab| tab.pane_group.id() == drag.source)
                        && tab.group_id.is_some()
                    {
                        tab.pinned = false;
                    }
                    ctx.dispatch_global_action("workspace:save_app", ());
                }
            }
        }
        ctx.notify();
        true
    }

    fn docking_target(
        &self,
        drag: DockingTabDrag,
        cursor: Vector2F,
        app: &AppContext,
    ) -> Option<DockingTarget> {
        let group = self.active_tab_pane_group();
        let pane_group = group.as_ref(app);
        if drag.canceled
            || drag.destination != group.id()
            || (pane_group.right_panel_open && pane_group.is_right_panel_maximized)
            || (FeatureFlag::AgentManagementView.is_enabled()
                && self.current_workspace_state.is_agent_management_view_open)
        {
            return None;
        }
        let pane = drag.pane?;
        let bounds =
            app.element_position_by_id_at_last_frame(self.window_id, pane.position_id())?;
        if !bounds.contains_point(cursor) || !self.can_dock_terminal_tab(drag.source, pane, app) {
            return None;
        }
        let minimum = get_minimum_pane_size(app);
        let split_minimum = minimum * 2. + get_divider_thickness();
        let horizontal = bounds.width() >= split_minimum && bounds.height() >= minimum;
        let vertical = bounds.height() >= split_minimum && bounds.width() >= minimum;
        Some(DockingTarget {
            pane,
            bounds,
            horizontal,
            vertical,
        })
    }

    pub(super) fn render_docking_tab_overlay(
        &self,
        stack: &mut Stack,
        appearance: &Appearance,
        app: &AppContext,
    ) {
        let Some(drag) = self.docking_tab_drag else {
            return;
        };
        let Some(cursor) = self
            .tabs
            .iter()
            .find(|tab| tab.pane_group.id() == drag.source)
            .and_then(|tab| tab.draggable_state.dragging_mouse_position())
        else {
            return;
        };
        let Some(target) = self.docking_target(drag, cursor, app) else {
            return;
        };
        let theme = appearance.theme();
        let pane_position = target.pane.position_id();
        let direction = target.direction_at(cursor);
        if let Some(direction) = direction {
            let preview = target.preview(direction);
            let preview_element = ConstrainedBox::new(
                Rect::new()
                    .with_background(theme.accent().with_opacity(18))
                    .with_border(Border::all(2.).with_border_fill(theme.accent()))
                    .finish(),
            )
            .with_width(preview.width())
            .with_height(preview.height())
            .finish();
            stack.add_positioned_overlay_child(
                SavePosition::new(preview_element, "workspace_view:docking_preview")
                    .for_single_frame()
                    .finish(),
                OffsetPositioning::offset_from_save_position_element(
                    &pane_position,
                    preview.origin(),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::TopLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }

        use Direction::{Down, Left, Right, Up};
        use PositionedElementAnchor::{BottomMiddle, MiddleLeft, MiddleRight, TopMiddle};
        let targets = [
            (
                Left,
                "Left",
                target.horizontal,
                MiddleLeft,
                ChildAnchor::MiddleLeft,
                vec2f(12., 0.),
            ),
            (
                Right,
                "Right",
                target.horizontal,
                MiddleRight,
                ChildAnchor::MiddleRight,
                vec2f(-12., 0.),
            ),
            (
                Up,
                "Top",
                target.vertical,
                TopMiddle,
                ChildAnchor::TopMiddle,
                vec2f(0., 12.),
            ),
            (
                Down,
                "Bottom",
                target.vertical,
                BottomMiddle,
                ChildAnchor::BottomMiddle,
                vec2f(0., -12.),
            ),
        ];
        for (edge, label, enabled, pane_anchor, label_anchor, offset) in targets {
            if !enabled {
                continue;
            }
            let active = direction.is_some_and(|direction| direction == edge);
            let background = if active {
                theme.accent()
            } else {
                theme.surface_2()
            };
            stack.add_positioned_overlay_child(
                Container::new(
                    Text::new_inline(label, appearance.ui_font_family(), 12.)
                        .with_color(theme.main_text_color(background).into())
                        .finish(),
                )
                .with_uniform_padding(6.)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                .with_background(background)
                .finish(),
                OffsetPositioning::offset_from_save_position_element(
                    &pane_position,
                    offset,
                    PositionedElementOffsetBounds::WindowByPosition,
                    pane_anchor,
                    label_anchor,
                ),
            );
        }
    }
}
