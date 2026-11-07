use pathfinder_geometry::rect::RectF;
use warpui::elements::{
    Align, Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, DraggableState,
    OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds, PositionedElementAnchor,
    PositionedElementOffsetBounds, Radius, Rect, SavePosition, Stack, Text,
};
use warpui::event::Event;
use warpui::geometry::vector::{Vector2F, vec2f};
use warpui::zoom::Scale;
use warpui::{AppContext, Element, EntityId, SingletonEntity, ViewContext, ViewHandle, WindowId};

use super::enhanced_docking::{DockingOperation, DockingSource};
use super::{Workspace, tab_bar_rects_for_window, tab_position_id};
use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::pane_group::tree::get_divider_layout_extent;
use crate::pane_group::{Direction, PaneGroup, PaneId, get_minimum_pane_size};
use crate::tab::uses_vertical_tabs;
use crate::workspace::{MOVE_TO_OWN_TAB_LABEL, PaneViewLocator, WorkspaceRegistry};

pub(super) const DOCKING_SURFACE_POSITION: &str = "workspace_view:docking_surface";

fn docking_placeholder_position(pane: PaneId) -> String {
    format!("workspace_view:docking_placeholder:{}", pane.position_id())
}

#[derive(Clone, Copy)]
pub(super) struct DockingTabDrag {
    pub(super) source: EntityId,
    destination: EntityId,
    flatten: bool,
    canceled: bool,
}

pub(super) struct DockingPaneDrag {
    source: PaneViewLocator,
    destination: EntityId,
    state: DraggableState,
    cursor: Vector2F,
    canceled: bool,
}

struct DockingTarget {
    locator: PaneViewLocator,
    bounds: RectF,
    horizontal: bool,
    vertical: bool,
    swap: bool,
    stack: bool,
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

    fn center_choices(&self) -> [(DockingOperation, &'static str, RectF, bool); 2] {
        let button_size = vec2f(64., 28.);
        let center = self.bounds.size() / 2.;
        [
            (
                DockingOperation::Swap,
                "Swap",
                RectF::new(
                    center - vec2f(button_size.x() + 4., button_size.y() / 2.),
                    button_size,
                ),
                self.swap,
            ),
            (
                DockingOperation::Stack,
                "Stack",
                RectF::new(center + vec2f(4., -button_size.y() / 2.), button_size),
                self.stack,
            ),
        ]
    }

    fn operation_at(&self, cursor: Vector2F, flatten: bool) -> Option<DockingOperation> {
        let local_cursor = cursor - self.bounds.origin();
        for (operation, _, bounds, enabled) in self.center_choices() {
            if enabled && bounds.contains_point(local_cursor) {
                return Some(operation);
            }
        }
        self.direction_at(cursor)
            .map(|direction| DockingOperation::Split { direction, flatten })
    }

    fn preview(&self, direction: Direction) -> RectF {
        let divider = get_divider_layout_extent();
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
                        && workspace.can_start_dock(DockingSource::Tab(tab.pane_group.id()), app)
                })
        })
}

pub(crate) fn pane_drag_owns_pointer(event: &Event, window_id: WindowId, app: &AppContext) -> bool {
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
            let group = workspace.active_tab_pane_group();
            let pane_group = group.as_ref(app);
            pane_group.visible_pane_ids().into_iter().any(|pane| {
                pane_group
                    .terminal_pane_header_drag_state(pane, app)
                    .is_some_and(|state| state.is_tracking_pointer())
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
    let flatten = match event {
        Event::LeftMouseDragged { modifiers, .. } | Event::LeftMouseUp { modifiers, .. } => {
            Some(modifiers.alt)
        }
        _ => None,
    };
    if !FeatureFlag::EnhancedPaneManagement.is_enabled()
        || !(cancel || flatten.is_some() || matches!(event, Event::LeftMouseDown { .. }))
    {
        return false;
    }
    let Some(workspace) = WorkspaceRegistry::as_ref(ctx).get(window_id, ctx) else {
        return false;
    };
    let group = {
        let workspace = workspace.as_ref(ctx);
        workspace
            .get_pane_group_view(workspace.active_tab_index)
            .filter(|group| group.as_ref(ctx).is_previewing_resize())
            .cloned()
    };
    if let Some(group) = group {
        return group.update(ctx, |group, ctx| match event {
            Event::LeftMouseDragged { position, .. } => {
                group.preview_resize_at(position.scale_down(ctx.zoom_factor()), ctx);
                true
            }
            Event::LeftMouseUp { position, .. } => {
                group.commit_resize_at(position.scale_down(ctx.zoom_factor()), ctx);
                true
            }
            Event::LeftMouseDown { .. } => {
                group.cancel_resizing(ctx);
                false
            }
            _ => {
                group.cancel_resizing(ctx);
                true
            }
        });
    }
    if workspace.as_ref(ctx).docking_pane_drag.is_some() {
        return workspace.update(ctx, |workspace, ctx| {
            workspace.handle_pane_docking_input(event, cancel, ctx)
        });
    }
    let Some(drag) = workspace.as_ref(ctx).docking_tab_drag else {
        return false;
    };
    if cancel {
        workspace.update(ctx, Workspace::cancel_docking_tab_drag);
    } else if let Some(flatten) = flatten {
        if drag.flatten != flatten {
            workspace.update(ctx, |workspace, ctx| {
                if let Some(drag) = &mut workspace.docking_tab_drag {
                    drag.flatten = flatten;
                    ctx.notify();
                }
            });
        }
    } else {
        // A new press can recover a gesture whose source stopped receiving mouse events.
        workspace.update(ctx, Workspace::reset_docking_tab_drag);
    }
    cancel
}

impl Workspace {
    pub(super) fn start_docking_pane_drag(&mut self, pane_id: PaneId, ctx: &mut ViewContext<Self>) {
        let group = self.active_tab_pane_group();
        let source = PaneViewLocator {
            pane_group_id: group.id(),
            pane_id,
        };
        if !self.can_start_dock(DockingSource::Pane(source), ctx) {
            return;
        }
        let Some(state) = group
            .as_ref(ctx)
            .terminal_pane_header_drag_state(pane_id, ctx)
        else {
            return;
        };
        let Some(cursor) = state.dragging_mouse_position() else {
            return;
        };
        self.docking_pane_drag = Some(DockingPaneDrag {
            source,
            destination: source.pane_group_id,
            state,
            cursor,
            canceled: false,
        });
        ctx.notify();
    }

    fn handle_pane_docking_input(
        &mut self,
        event: &Event,
        cancel: bool,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let Some(mut drag) = self.docking_pane_drag.take() else {
            return false;
        };
        match event {
            Event::LeftMouseDragged { position, .. } => {
                let cursor = position.scale_down(ctx.zoom_factor());
                if !drag.canceled {
                    if let Some(destination) = self.docking_tab_at(cursor, ctx) {
                        drag.destination = destination;
                    }
                    drag.state.adjust_mouse_position(cursor - drag.cursor);
                    drag.cursor = cursor;
                    ctx.notify();
                }
                self.docking_pane_drag = Some(drag);
                true
            }
            Event::LeftMouseUp { position, .. } => {
                drag.state.cancel_drag();
                drag.state.set_suppress_overlay_paint(false);
                if !drag.canceled {
                    self.finish_docking_pane_drag(
                        &drag,
                        position.scale_down(ctx.zoom_factor()),
                        ctx,
                    );
                }
                ctx.notify();
                true
            }
            Event::KeyDown { .. } if cancel => {
                drag.canceled = true;
                drag.state.set_suppress_overlay_paint(true);
                self.docking_pane_drag = Some(drag);
                ctx.notify();
                true
            }
            Event::LeftMouseDown { .. } => {
                drag.state.cancel_drag();
                drag.state.set_suppress_overlay_paint(false);
                ctx.notify();
                false
            }
            _ => {
                self.docking_pane_drag = Some(drag);
                false
            }
        }
    }

    fn docking_tab_at(&self, cursor: Vector2F, app: &AppContext) -> Option<EntityId> {
        let bars = tab_bar_rects_for_window(self.window_id, app);
        if !bars.iter().any(|bounds| bounds.contains_point(cursor)) {
            return None;
        }
        let vertical = uses_vertical_tabs(app);
        self.tabs.iter().enumerate().find_map(|(index, tab)| {
            if tab.detached
                || tab
                    .group_id
                    .is_some_and(|id| self.tab_groups.get(&id).is_some_and(|g| g.collapsed))
            {
                return None;
            }
            let bounds =
                app.element_position_by_id_at_last_frame(self.window_id, tab_position_id(index))?;
            let point = if vertical { cursor.y() } else { cursor.x() };
            let center = if vertical {
                bounds.center().y()
            } else {
                bounds.center().x()
            };
            let extent = if vertical {
                bounds.height()
            } else {
                bounds.width()
            };
            (bounds.contains_point(cursor) && (point - center).abs() <= extent * 0.3)
                .then_some(tab.pane_group.id())
        })
    }

    fn previewed_docking_group(&self) -> Option<&ViewHandle<PaneGroup>> {
        let drag = self.docking_pane_drag.as_ref()?;
        if drag.canceled || drag.destination == drag.source.pane_group_id {
            return None;
        }
        self.tabs
            .iter()
            .find(|tab| tab.pane_group.id() == drag.destination && !tab.detached)
            .map(|tab| &tab.pane_group)
    }

    pub(super) fn render_docking_destination_preview(
        &self,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Option<Box<dyn Element>> {
        let group = self.previewed_docking_group()?.as_ref(app);
        let bounds =
            app.element_position_by_id_at_last_frame(self.window_id, DOCKING_SURFACE_POSITION)?;
        let rectangles = group.docking_preview_rects(bounds, app);
        if rectangles.is_empty() {
            return None;
        }
        let theme = appearance.theme();
        let mut preview =
            Stack::new().with_child(Rect::new().with_background(theme.background()).finish());
        for (pane, pane_bounds) in rectangles {
            let title = group
                .pane_by_id(pane)
                .map(|pane| {
                    let configuration = pane.pane_configuration();
                    let configuration = configuration.as_ref(app);
                    configuration
                        .custom_vertical_tabs_title()
                        .unwrap_or_else(|| configuration.title())
                        .to_owned()
                })
                .filter(|title| !title.is_empty());
            let label = title.map_or_else(
                || "Terminal pane".to_owned(),
                |title| format!("Terminal pane ({title})"),
            );
            let color = theme.surface_2();
            let box_element = Container::new(
                Align::new(
                    Text::new_inline(label, appearance.ui_font_family(), 12.)
                        .with_color(theme.main_text_color(color).into())
                        .finish(),
                )
                .finish(),
            )
            .with_background(color)
            .with_border(Border::all(1.).with_border_fill(theme.accent().with_opacity(45)))
            .finish();
            let element = ConstrainedBox::new(box_element)
                .with_width(pane_bounds.width())
                .with_height(pane_bounds.height())
                .finish();
            preview.add_positioned_child(
                SavePosition::new(element, &docking_placeholder_position(pane))
                    .for_single_frame()
                    .finish(),
                OffsetPositioning::offset_from_parent(
                    pane_bounds.origin() - bounds.origin(),
                    ParentOffsetBounds::ParentByPosition,
                    ParentAnchor::TopLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }
        Some(preview.finish())
    }

    fn finish_docking_pane_drag(
        &mut self,
        drag: &DockingPaneDrag,
        cursor: Vector2F,
        ctx: &mut ViewContext<Self>,
    ) {
        let source = DockingSource::Pane(drag.source);
        if let Some(target) = self.docking_target(source, drag.destination, cursor, false, ctx)
            && let Some(operation) = target.operation_at(cursor, false)
        {
            self.dock(source, target.locator, operation, target.bounds, ctx);
        } else if self.docking_tab_at(cursor, ctx).is_none()
            && tab_bar_rects_for_window(self.window_id, ctx)
                .iter()
                .any(|bounds| bounds.contains_point(cursor))
            && self.active_tab_pane_group().id() == drag.source.pane_group_id
        {
            self.move_pane_to_new_tab(drag.source.pane_id, ctx);
        }
    }

    pub(super) fn start_docking_tab_drag(&mut self, source: EntityId, app: &AppContext) {
        if !FeatureFlag::EnhancedPaneManagement.is_enabled() {
            return;
        }
        let destination = self.active_tab_pane_group();
        if source != destination.id()
            && let Some(tab) = self.tabs.iter().find(|tab| tab.pane_group.id() == source)
            && tab.draggable_state.is_dragging()
            && self.can_start_dock(DockingSource::Tab(source), app)
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
                flatten: false,
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
                let source = DockingSource::Tab(drag.source);
                if let Some(target) =
                    self.docking_target(source, drag.destination, cursor, drag.flatten, ctx)
                    && let Some(operation) = target.operation_at(cursor, drag.flatten)
                {
                    self.dock(source, target.locator, operation, target.bounds, ctx);
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
        source: DockingSource,
        destination: EntityId,
        cursor: Vector2F,
        flatten: bool,
        app: &AppContext,
    ) -> Option<DockingTarget> {
        let group = self
            .tabs
            .iter()
            .find(|tab| tab.pane_group.id() == destination && !tab.detached)?
            .pane_group
            .as_ref(app);
        if (group.right_panel_open && group.is_right_panel_maximized)
            || (FeatureFlag::AgentManagementView.is_enabled()
                && self.current_workspace_state.is_agent_management_view_open)
        {
            return None;
        }
        let (pane_id, bounds) = if destination == self.active_tab_pane_group().id() {
            group.visible_pane_ids().into_iter().find_map(|pane_id| {
                let bounds = app
                    .element_position_by_id_at_last_frame(self.window_id, pane_id.position_id())?;
                bounds.contains_point(cursor).then_some((pane_id, bounds))
            })
        } else {
            let surface =
                app.element_position_by_id_at_last_frame(self.window_id, DOCKING_SURFACE_POSITION)?;
            group
                .docking_preview_rects(surface, app)
                .into_iter()
                .find(|(_, bounds)| bounds.contains_point(cursor))
        }?;
        let locator = PaneViewLocator {
            pane_group_id: destination,
            pane_id,
        };
        let mut target = DockingTarget {
            locator,
            bounds,
            horizontal: false,
            vertical: false,
            swap: false,
            stack: false,
        };
        let minimum = get_minimum_pane_size(app);
        let split_minimum = minimum * 2. + get_divider_layout_extent();
        let fits = |direction| {
            let operation = DockingOperation::Split { direction, flatten };
            self.can_dock(source, locator, operation, app)
                && self.dock_source_fits(source, target.preview(direction), operation, app)
        };
        let horizontal =
            bounds.width() >= split_minimum && bounds.height() >= minimum && fits(Direction::Right);
        let vertical =
            bounds.height() >= split_minimum && bounds.width() >= minimum && fits(Direction::Down);
        target.horizontal = horizontal;
        target.vertical = vertical;
        if bounds.width() >= 160. && bounds.height() >= 100. {
            target.swap = self.can_dock(source, locator, DockingOperation::Swap, app);
            target.stack = self.can_dock(source, locator, DockingOperation::Stack, app);
        }
        Some(target)
    }

    pub(super) fn render_docking_overlay(
        &self,
        stack: &mut Stack,
        appearance: &Appearance,
        app: &AppContext,
    ) {
        let (source, destination, cursor, flatten) = if let Some(drag) = self
            .docking_pane_drag
            .as_ref()
            .filter(|drag| !drag.canceled)
        {
            (
                DockingSource::Pane(drag.source),
                drag.destination,
                drag.cursor,
                false,
            )
        } else if let Some(drag) = self.docking_tab_drag.filter(|drag| !drag.canceled) {
            let Some(cursor) = self
                .tabs
                .iter()
                .find(|tab| tab.pane_group.id() == drag.source)
                .and_then(|tab| tab.draggable_state.dragging_mouse_position())
            else {
                return;
            };
            (
                DockingSource::Tab(drag.source),
                drag.destination,
                cursor,
                drag.flatten,
            )
        } else {
            return;
        };
        let theme = appearance.theme();
        let Some(target) = self.docking_target(source, destination, cursor, flatten, app) else {
            if matches!(source, DockingSource::Pane(_))
                && tab_bar_rects_for_window(self.window_id, app)
                    .iter()
                    .any(|bounds| bounds.contains_point(cursor))
                && self.docking_tab_at(cursor, app).is_none()
            {
                stack.add_positioned_overlay_child(
                    Container::new(
                        Text::new_inline(MOVE_TO_OWN_TAB_LABEL, appearance.ui_font_family(), 12.)
                            .with_color(theme.main_text_color(theme.accent()).into())
                            .finish(),
                    )
                    .with_uniform_padding(6.)
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                    .with_background(theme.accent())
                    .finish(),
                    OffsetPositioning::offset_from_parent(
                        cursor + vec2f(12., 12.),
                        ParentOffsetBounds::WindowByPosition,
                        ParentAnchor::TopLeft,
                        ChildAnchor::TopLeft,
                    ),
                );
            }
            return;
        };
        let pane_position = if destination == self.active_tab_pane_group().id() {
            target.locator.pane_id.position_id()
        } else {
            docking_placeholder_position(target.locator.pane_id)
        };
        let operation = target.operation_at(cursor, flatten);
        let direction = match operation {
            Some(DockingOperation::Split { direction, .. }) => Some(direction),
            Some(DockingOperation::Swap | DockingOperation::Stack) | None => None,
        };
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

        for (choice, label, bounds, enabled) in target.center_choices() {
            if !enabled {
                continue;
            }
            let active = operation == Some(choice);
            let background = if active {
                theme.accent()
            } else {
                theme.surface_2()
            };
            let element = Container::new(
                Align::new(
                    Text::new_inline(label, appearance.ui_font_family(), 12.)
                        .with_color(theme.main_text_color(background).into())
                        .finish(),
                )
                .finish(),
            )
            .with_background(background)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
            .finish();
            let position_id = match choice {
                DockingOperation::Swap => "workspace_view:docking_swap",
                DockingOperation::Stack => "workspace_view:docking_stack",
                DockingOperation::Split { .. } => continue,
            };
            let element = ConstrainedBox::new(element)
                .with_width(bounds.width())
                .with_height(bounds.height())
                .finish();
            stack.add_positioned_overlay_child(
                SavePosition::new(element, position_id)
                    .for_single_frame()
                    .finish(),
                OffsetPositioning::offset_from_save_position_element(
                    &pane_position,
                    bounds.origin(),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::TopLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }

        if self.dock_source_is_layout(source, app) {
            let label = if flatten {
                "Flatten layout"
            } else if cfg!(target_os = "macos") {
                "Hold Option to flatten"
            } else {
                "Hold Alt to flatten"
            };
            let theme = appearance.theme();
            stack.add_positioned_overlay_child(
                Container::new(
                    Text::new_inline(label, appearance.ui_font_family(), 12.)
                        .with_color(theme.main_text_color(theme.surface_2()).into())
                        .finish(),
                )
                .with_uniform_padding(6.)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                .with_background(theme.surface_2())
                .finish(),
                OffsetPositioning::offset_from_save_position_element(
                    &pane_position,
                    vec2f(0., 0.),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::Center,
                    ChildAnchor::Center,
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
