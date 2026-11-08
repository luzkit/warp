use std::iter;

use pathfinder_geometry::rect::RectF;
use warpui::accessibility::{AccessibilityContent, WarpA11yRole};
use warpui::elements::{
    Align, Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, DraggableState,
    OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds, PositionedElementAnchor,
    PositionedElementOffsetBounds, Radius, Rect, SavePosition, Stack, Text,
};
use warpui::event::Event;
use warpui::geometry::vector::{Vector2F, vec2f};
use warpui::platform::FullscreenState;
use warpui::windowing::System;
use warpui::zoom::Scale;
use warpui::{
    AppContext, DisplayIdx, Element, Entity, EntityId, SingletonEntity, ViewContext, ViewHandle,
    WindowId,
};

use super::enhanced_docking::{DockingOperation, DockingSource};
use super::{Workspace, tab_bar_rects_for_window, tab_position_id};
use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::pane_group::tree::get_divider_layout_extent;
use crate::pane_group::{Direction, PaneGroup, PaneId, get_minimum_pane_size};
use crate::settings::PaneSettings;
use crate::tab::uses_vertical_tabs;
use crate::workspace::{MOVE_TO_OWN_TAB_LABEL, PaneViewLocator, WorkspaceRegistry};

pub(super) const DOCKING_SURFACE_POSITION: &str = "workspace_view:docking_surface";
const MAX_LABELED_PREVIEW_PANES: usize = 24;

fn docking_placeholder_position(pane: PaneId) -> String {
    format!("workspace_view:docking_placeholder:{}", pane.position_id())
}

#[derive(Clone, Copy)]
pub(super) struct DockingTabDrag {
    pub(super) source: EntityId,
    destination: EntityId,
    flatten: bool,
    canceled: bool,
    window_drop: WindowDockingDrop,
}

pub(super) struct DockingPaneDrag {
    source: PaneViewLocator,
    destination: EntityId,
    state: DraggableState,
    cursor: Vector2F,
    canceled: bool,
    window_drop: WindowDockingDrop,
}

#[derive(Clone, Copy, PartialEq)]
enum WindowDockingDrop {
    Local,
    Invalid,
    Tab {
        window: WindowId,
        index: usize,
        bounds: RectF,
    },
    Split {
        window: WindowId,
        locator: PaneViewLocator,
        operation: DockingOperation,
        bounds: RectF,
        preview: RectF,
    },
    TearOut(RectF),
}

impl WindowDockingDrop {
    fn preview_window(self) -> Option<WindowId> {
        match self {
            Self::Tab { window, .. } | Self::Split { window, .. } => Some(window),
            Self::Local | Self::Invalid | Self::TearOut(_) => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct IncomingPreview {
    source: WindowId,
    drop: WindowDockingDrop,
}

#[derive(Default)]
struct IncomingWindowDockingPreview(Option<IncomingPreview>);

impl Entity for IncomingWindowDockingPreview {
    type Event = ();
}

impl SingletonEntity for IncomingWindowDockingPreview {}

pub(in crate::workspace) fn init_window_docking_preview(app: &mut AppContext) {
    app.add_singleton_model(|_| IncomingWindowDockingPreview::default());
}

pub(super) struct DockingDestinationPreview {
    group: ViewHandle<PaneGroup>,
    surface: RectF,
    rectangles: Vec<(PaneId, RectF)>,
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
                    tab.draggable_state.is_tracking_pointer()
                        && (index != workspace.active_tab_index
                            || supports_window_docking_pointer(window_id, app))
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
    let pointer = match event {
        Event::LeftMouseDragged {
            position,
            modifiers,
        }
        | Event::LeftMouseUp {
            position,
            modifiers,
        } => Some((*position, modifiers.alt)),
        _ => None,
    };
    if !FeatureFlag::EnhancedPaneManagement.is_enabled()
        || !(cancel || pointer.is_some() || matches!(event, Event::LeftMouseDown { .. }))
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
    if workspace.as_ref(ctx).docking_tab_drag.is_none() {
        return false;
    }
    if cancel {
        workspace.update(ctx, Workspace::cancel_docking_tab_drag);
        true
    } else if let Some((position, flatten)) = pointer {
        workspace.update(ctx, |workspace, ctx| {
            workspace.update_docking_tab_pointer(
                position,
                flatten,
                matches!(event, Event::LeftMouseUp { .. }),
                ctx,
            )
        })
    } else {
        // A new press can recover a gesture whose source stopped receiving mouse events.
        workspace.update(ctx, Workspace::reset_docking_tab_drag);
        false
    }
}

fn supports_window_docking_pointer(window: WindowId, app: &AppContext) -> bool {
    // Wayland does not expose a shared screen coordinate space for app windows.
    let windows = app.windows();
    let has_screen_coordinates = match windows.windowing_system() {
        Some(System::AppKit | System::X11 { .. } | System::Windows) => true,
        Some(System::Wayland) => false,
        // The macOS integration manager has no protocol label but uses real AppKit windows.
        None => cfg!(target_os = "macos") && windows.ordered_window_ids().contains(&window),
    };
    has_screen_coordinates
        && windows
            .platform_window(window)
            .is_some_and(|window| window.fullscreen_state() != FullscreenState::Fullscreen)
}

fn publish_window_docking_preview(source: WindowId, drop: WindowDockingDrop, app: &mut AppContext) {
    let preview = drop
        .preview_window()
        .map(|_| IncomingPreview { source, drop });
    replace_window_docking_preview(preview, app);
}

fn clear_outgoing_window_docking_preview(source: WindowId, app: &mut AppContext) {
    if IncomingWindowDockingPreview::as_ref(app)
        .0
        .is_some_and(|preview| preview.source == source)
    {
        replace_window_docking_preview(None, app);
    }
}

fn replace_window_docking_preview(current: Option<IncomingPreview>, app: &mut AppContext) {
    let previous = IncomingWindowDockingPreview::as_ref(app).0;
    if previous == current {
        return;
    }
    IncomingWindowDockingPreview::handle(app).update(app, |state, _| state.0 = current);
    let current_window = current.and_then(|preview| preview.drop.preview_window());
    let previous_window = previous
        .and_then(|preview| preview.drop.preview_window())
        .filter(|id| Some(*id) != current_window);
    for window in [previous_window, current_window].into_iter().flatten() {
        if let Some(workspace) = WorkspaceRegistry::as_ref(app).get(window, app) {
            workspace.update(app, |_, ctx| ctx.notify());
        }
    }
}

fn window_tab_preview_bounds(
    window: WindowId,
    index: usize,
    cursor: Vector2F,
    tab_bar: RectF,
    app: &AppContext,
) -> RectF {
    let tab_bounds = |index| {
        app.element_position_by_id_at_last_frame(window, tab_position_id(index))
            .filter(|bounds| tab_bar.contains_point(bounds.center()))
    };
    let vertical = uses_vertical_tabs(app);
    let along = |point: Vector2F| if vertical { point.y() } else { point.x() };
    let start = along(tab_bar.origin());
    let end = start
        + if vertical {
            tab_bar.height()
        } else {
            tab_bar.width()
        };
    let indicator = tab_bounds(index)
        .map(|bounds| along(bounds.origin()))
        .or_else(|| {
            let preceding = tab_bounds(index.checked_sub(1)?)?;
            Some(if vertical {
                preceding.max_y()
            } else {
                preceding.max_x()
            })
        })
        .unwrap_or_else(|| along(cursor))
        .clamp(start, (end - 3.).max(start));
    if vertical {
        RectF::new(
            vec2f(tab_bar.min_x(), indicator),
            vec2f(tab_bar.width(), 3.),
        )
    } else {
        RectF::new(
            vec2f(indicator, tab_bar.min_y()),
            vec2f(3., tab_bar.height()),
        )
    }
}

fn tear_out_bounds(source: RectF, screen_cursor: Vector2F, app: &AppContext) -> Option<RectF> {
    let margin = vec2f(16., 16.);
    let source_with_margin = RectF::new(source.origin() - margin, source.size() + margin * 2.);
    if source_with_margin.contains_point(screen_cursor) {
        return None;
    }
    let windows = app.windows();
    let displays = iter::once(DisplayIdx::Primary)
        .chain((0..windows.display_count().saturating_sub(1)).map(DisplayIdx::External));
    displays
        .filter_map(|index| windows.bounds_for_display_idx(index))
        .any(|bounds| bounds.contains_point(screen_cursor))
        .then(|| RectF::new(screen_cursor, source.size()))
}

impl Workspace {
    fn window_docking_drop(
        &self,
        source: DockingSource,
        position: Vector2F,
        flatten: bool,
        app: &AppContext,
    ) -> WindowDockingDrop {
        if !supports_window_docking_pointer(self.window_id, app) {
            return WindowDockingDrop::Local;
        }
        let Some(source_bounds) = app.window_bounds(&self.window_id) else {
            return WindowDockingDrop::Local;
        };
        let screen_cursor = source_bounds.origin() + position;
        let ordered = app.windows().ordered_window_ids();
        let hit = ordered.into_iter().find_map(|window| {
            let bounds = app.window_bounds(&window)?;
            bounds
                .contains_point(screen_cursor)
                .then_some((window, bounds))
        });
        let Some((window, window_bounds)) = hit else {
            if source_bounds.contains_point(screen_cursor) {
                return WindowDockingDrop::Local;
            }
            return tear_out_bounds(source_bounds, screen_cursor, app)
                .filter(|_| self.can_tear_out_docking_source(source, app))
                .map_or(WindowDockingDrop::Invalid, WindowDockingDrop::TearOut);
        };
        if window == self.window_id {
            return WindowDockingDrop::Local;
        }
        // The frontmost hit window owns the point even if it cannot accept docking.
        let Some(workspace) = WorkspaceRegistry::as_ref(app).get(window, app) else {
            return WindowDockingDrop::Invalid;
        };
        if !supports_window_docking_pointer(window, app) {
            return WindowDockingDrop::Invalid;
        }
        let workspace = workspace.as_ref(app);
        let cursor = (screen_cursor - window_bounds.origin()).scale_down(app.zoom_factor());
        if let Some(bounds) = tab_bar_rects_for_window(window, app)
            .into_iter()
            .find(|bounds| bounds.contains_point(cursor))
        {
            if !self.can_move_docking_source_to_window(source, window, app) {
                return WindowDockingDrop::Invalid;
            }
            // Tab position caches use UI points at the current zoom.
            let index = workspace.tab_insertion_index_for_cursor(
                window,
                window_bounds.origin() + cursor,
                app,
            );
            return WindowDockingDrop::Tab {
                window,
                index,
                bounds: window_tab_preview_bounds(window, index, cursor, bounds, app),
            };
        }
        let Some(group) = workspace.get_pane_group_view(workspace.active_tab_index) else {
            return WindowDockingDrop::Invalid;
        };
        let Some(mut target) = workspace.docking_target_geometry(group.id(), cursor, None, app)
        else {
            return WindowDockingDrop::Invalid;
        };
        let can_split = |direction| {
            self.can_dock_across_windows(
                source,
                window,
                target.locator,
                DockingOperation::Split { direction, flatten },
                target.bounds,
                app,
            )
        };
        let horizontal = can_split(Direction::Right);
        let vertical = can_split(Direction::Down);
        target.horizontal = horizontal;
        target.vertical = vertical;
        let Some(direction) = target.direction_at(cursor) else {
            return WindowDockingDrop::Invalid;
        };
        let preview = target.preview(direction);
        WindowDockingDrop::Split {
            window,
            locator: target.locator,
            operation: DockingOperation::Split { direction, flatten },
            bounds: target.bounds,
            preview,
        }
    }

    fn commit_window_docking_drop(
        &mut self,
        source: DockingSource,
        drop: WindowDockingDrop,
        ctx: &mut ViewContext<Self>,
    ) {
        let (placement, committed) = match drop {
            WindowDockingDrop::Tab { window, index, .. } => (
                "tab",
                self.move_docking_source_to_window(source, window, Some(index), ctx),
            ),
            WindowDockingDrop::Split {
                window,
                locator,
                operation,
                bounds,
                ..
            } => (
                "split",
                self.dock_across_windows(source, window, locator, operation, bounds, ctx),
            ),
            WindowDockingDrop::TearOut(bounds) => (
                "new window",
                self.tear_out_docking_source(source, Some(bounds), ctx),
            ),
            WindowDockingDrop::Local | WindowDockingDrop::Invalid => return,
        };
        if !committed {
            log::debug!("Pointer docking commit rejected for {placement} placement");
        }
    }

    pub(super) fn clear_outgoing_docking_preview(&self, ctx: &mut ViewContext<Self>) {
        clear_outgoing_window_docking_preview(self.window_id, ctx);
    }

    pub(super) fn cancel_docking_from_removed_group(
        &mut self,
        group: EntityId,
        ctx: &mut ViewContext<Self>,
    ) {
        if self
            .docking_pane_drag
            .as_ref()
            .is_some_and(|drag| drag.source.pane_group_id == group)
            && let Some(drag) = self.docking_pane_drag.take()
        {
            drag.state.cancel_drag();
            drag.state.set_suppress_overlay_paint(false);
            clear_outgoing_window_docking_preview(self.window_id, ctx);
            ctx.reset_cursor();
        }
    }

    pub(super) fn clear_closed_window_docking_preview(&self, ctx: &mut ViewContext<Self>) {
        if IncomingWindowDockingPreview::as_ref(ctx)
            .0
            .is_some_and(|preview| {
                preview.source == self.window_id
                    || preview.drop.preview_window() == Some(self.window_id)
            })
        {
            replace_window_docking_preview(None, ctx);
        }
    }

    fn update_docking_tab_pointer(
        &mut self,
        position: Vector2F,
        flatten: bool,
        release: bool,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let Some(mut drag) = self.docking_tab_drag else {
            return false;
        };
        let source = DockingSource::Tab(drag.source);
        let drop = if drag.canceled {
            WindowDockingDrop::Invalid
        } else {
            self.window_docking_drop(source, position, flatten, ctx)
        };
        if release && drop != WindowDockingDrop::Local {
            self.reset_docking_tab_drag(ctx);
            ctx.reset_cursor();
            self.commit_window_docking_drop(source, drop, ctx);
            return true;
        }
        let previous = drag.window_drop;
        if previous != drop || drag.flatten != flatten {
            drag.window_drop = drop;
            drag.flatten = flatten;
            self.docking_tab_drag = Some(drag);
            publish_window_docking_preview(self.window_id, drop, ctx);
            ctx.notify();
        }
        // The tab Draggable still owns within-window reordering and the local mouse-up.
        false
    }

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
        let window_drop = self.window_docking_drop(
            DockingSource::Pane(source),
            cursor.scale_up(ctx.zoom_factor()),
            false,
            ctx,
        );
        self.docking_pane_drag = Some(DockingPaneDrag {
            source,
            destination: source.pane_group_id,
            state,
            cursor,
            canceled: false,
            window_drop,
        });
        publish_window_docking_preview(self.window_id, window_drop, ctx);
        ctx.notify();
    }

    fn refresh_pane_header_drag(&self, source: PaneViewLocator, ctx: &mut ViewContext<Self>) {
        // Docking consumes the mouse-up before the header's Draggable can repaint itself.
        if let Some(group) = self.get_pane_group_view_with_id(source.pane_group_id) {
            group.update(ctx, |group, ctx| {
                group.refresh_terminal_pane_header_drag(source.pane_id, ctx);
            });
        }
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
                let previous_destination = drag.destination;
                if !drag.canceled {
                    drag.window_drop = self.window_docking_drop(
                        DockingSource::Pane(drag.source),
                        *position,
                        false,
                        ctx,
                    );
                    if drag.window_drop == WindowDockingDrop::Local
                        && let Some(destination) = self.docking_tab_at(cursor, ctx)
                    {
                        drag.destination = destination;
                    }
                    drag.state.adjust_mouse_position(cursor - drag.cursor);
                    drag.cursor = cursor;
                    publish_window_docking_preview(self.window_id, drag.window_drop, ctx);
                    ctx.notify();
                }
                let destination_changed = drag.destination != previous_destination;
                self.docking_pane_drag = Some(drag);
                if destination_changed
                    && *PaneSettings::as_ref(ctx).announce_pane_docking
                    && let Some(preview) = self.docking_destination_preview(ctx)
                    && preview.rectangles.len() > MAX_LABELED_PREVIEW_PANES
                {
                    ctx.emit_a11y_content(AccessibilityContent::new(
                        format!("Docking preview with {} panes", preview.rectangles.len()),
                        "Open Pane Navigator to select a named destination with the keyboard.",
                        WarpA11yRole::TextRole,
                    ));
                }
                true
            }
            Event::LeftMouseUp { position, .. } => {
                drag.state.cancel_drag();
                drag.state.set_suppress_overlay_paint(false);
                self.refresh_pane_header_drag(drag.source, ctx);
                clear_outgoing_window_docking_preview(self.window_id, ctx);
                if !drag.canceled {
                    let source = DockingSource::Pane(drag.source);
                    let drop = self.window_docking_drop(source, *position, false, ctx);
                    if drop == WindowDockingDrop::Local {
                        self.finish_docking_pane_drag(
                            &drag,
                            position.scale_down(ctx.zoom_factor()),
                            ctx,
                        );
                    } else {
                        self.commit_window_docking_drop(source, drop, ctx);
                    }
                }
                ctx.reset_cursor();
                ctx.notify();
                true
            }
            Event::KeyDown { .. } if cancel => {
                drag.canceled = true;
                drag.state.set_suppress_overlay_paint(true);
                self.refresh_pane_header_drag(drag.source, ctx);
                clear_outgoing_window_docking_preview(self.window_id, ctx);
                drag.window_drop = WindowDockingDrop::Invalid;
                self.docking_pane_drag = Some(drag);
                ctx.notify();
                true
            }
            Event::LeftMouseDown { .. } => {
                drag.state.cancel_drag();
                drag.state.set_suppress_overlay_paint(false);
                self.refresh_pane_header_drag(drag.source, ctx);
                clear_outgoing_window_docking_preview(self.window_id, ctx);
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

    pub(super) fn docking_destination_preview(
        &self,
        app: &AppContext,
    ) -> Option<DockingDestinationPreview> {
        let drag = self.docking_pane_drag.as_ref()?;
        if !FeatureFlag::EnhancedPaneManagement.is_enabled()
            || drag.canceled
            || drag.window_drop != WindowDockingDrop::Local
            || drag.destination == self.active_tab_pane_group().id()
        {
            return None;
        }
        let group = self
            .tabs
            .iter()
            .find(|tab| tab.pane_group.id() == drag.destination && !tab.detached)?
            .pane_group
            .clone();
        let surface =
            app.element_position_by_id_at_last_frame(self.window_id, DOCKING_SURFACE_POSITION)?;
        let rectangles = group.as_ref(app).docking_preview_rects(surface, app);
        Some(DockingDestinationPreview {
            group,
            surface,
            rectangles,
        })
    }

    pub(super) fn render_docking_destination_preview(
        geometry: &DockingDestinationPreview,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Option<Box<dyn Element>> {
        if geometry.rectangles.is_empty() {
            return None;
        }
        let group = geometry.group.as_ref(app);
        let theme = appearance.theme();
        let color = theme.surface_2();
        let mut preview =
            Stack::new().with_child(Rect::new().with_background(theme.background()).finish());
        let dense = geometry.rectangles.len() > MAX_LABELED_PREVIEW_PANES;
        for &(pane, bounds) in &geometry.rectangles {
            let border = Border::all(1.).with_border_fill(theme.accent().with_opacity(45));
            let tile = if dense {
                Rect::new()
                    .with_background(color)
                    .with_border(border)
                    .finish()
            } else {
                let title = group
                    .pane_by_id(pane)
                    .map(|pane| {
                        let configuration = pane.pane_configuration();
                        let configuration = configuration.as_ref(app);
                        configuration
                            .custom_vertical_tabs_title()
                            .unwrap_or_else(|| configuration.title())
                    })
                    .filter(|title| !title.is_empty());
                let label =
                    title.map_or_else(|| "Pane".to_owned(), |title| format!("Pane ({title})"));
                Container::new(
                    Align::new(
                        Text::new_inline(label, appearance.ui_font_family(), 12.)
                            .with_color(theme.main_text_color(color).into())
                            .finish(),
                    )
                    .finish(),
                )
                .with_background(color)
                .with_border(border)
                .finish()
            };
            let element = ConstrainedBox::new(tile)
                .with_width(bounds.width())
                .with_height(bounds.height())
                .finish();
            preview.add_positioned_child(
                SavePosition::new(element, &docking_placeholder_position(pane))
                    .for_single_frame()
                    .finish(),
                OffsetPositioning::offset_from_parent(
                    bounds.origin() - geometry.surface.origin(),
                    ParentOffsetBounds::ParentByPosition,
                    ParentAnchor::TopLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }
        if dense {
            let label = format!("Preview · {} panes", geometry.rectangles.len());
            preview.add_positioned_child(
                Container::new(
                    Text::new_inline(label, appearance.ui_font_family(), 12.)
                        .with_color(theme.main_text_color(color).into())
                        .finish(),
                )
                .with_uniform_padding(6.)
                .with_background(color)
                .finish(),
                OffsetPositioning::offset_from_parent(
                    vec2f(8., 8.),
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
        if let Some(target) =
            self.docking_target(source, drag.destination, cursor, false, None, ctx)
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

    pub(super) fn start_docking_tab_drag(&mut self, source: EntityId, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::EnhancedPaneManagement.is_enabled() {
            return;
        }
        let destination = self.active_tab_pane_group().id();
        if (source != destination || supports_window_docking_pointer(self.window_id, ctx))
            && let Some(tab) = self.tabs.iter().find(|tab| tab.pane_group.id() == source)
            && let Some(cursor) = tab.draggable_state.dragging_mouse_position()
            && self.can_start_dock(DockingSource::Tab(source), ctx)
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
            let window_drop = self.window_docking_drop(
                DockingSource::Tab(source),
                cursor.scale_up(ctx.zoom_factor()),
                false,
                ctx,
            );
            self.docking_tab_drag = Some(DockingTabDrag {
                source,
                destination,
                flatten: false,
                canceled: false,
                window_drop,
            });
            publish_window_docking_preview(self.window_id, window_drop, ctx);
            ctx.notify();
        }
    }

    fn cancel_docking_tab_drag(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some(drag) = &mut self.docking_tab_drag {
            drag.canceled = true;
            clear_outgoing_window_docking_preview(self.window_id, ctx);
            drag.window_drop = WindowDockingDrop::Invalid;
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
            clear_outgoing_window_docking_preview(self.window_id, ctx);
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
        clear_outgoing_window_docking_preview(self.window_id, ctx);
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
                    self.docking_target(source, drag.destination, cursor, drag.flatten, None, ctx)
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

    fn docking_target_geometry(
        &self,
        destination: EntityId,
        cursor: Vector2F,
        preview: Option<&DockingDestinationPreview>,
        app: &AppContext,
    ) -> Option<DockingTarget> {
        let preview = preview.filter(|preview| preview.group.id() == destination);
        let group = preview
            .map(|preview| &preview.group)
            .or_else(|| {
                self.tabs
                    .iter()
                    .find(|tab| tab.pane_group.id() == destination && !tab.detached)
                    .map(|tab| &tab.pane_group)
            })?
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
        } else if let Some(preview) = preview {
            preview
                .rectangles
                .iter()
                .copied()
                .find(|(_, bounds)| bounds.contains_point(cursor))
        } else {
            let surface =
                app.element_position_by_id_at_last_frame(self.window_id, DOCKING_SURFACE_POSITION)?;
            group
                .docking_preview_rects(surface, app)
                .into_iter()
                .find(|(_, bounds)| bounds.contains_point(cursor))
        }?;
        Some(DockingTarget {
            locator: PaneViewLocator {
                pane_group_id: destination,
                pane_id,
            },
            bounds,
            horizontal: false,
            vertical: false,
            swap: false,
            stack: false,
        })
    }

    fn docking_target(
        &self,
        source: DockingSource,
        destination: EntityId,
        cursor: Vector2F,
        flatten: bool,
        preview: Option<&DockingDestinationPreview>,
        app: &AppContext,
    ) -> Option<DockingTarget> {
        let mut target = self.docking_target_geometry(destination, cursor, preview, app)?;
        let DockingTarget {
            locator, bounds, ..
        } = target;
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

    fn render_incoming_window_docking_overlay(
        &self,
        stack: &mut Stack,
        appearance: &Appearance,
        app: &AppContext,
    ) {
        let incoming = IncomingWindowDockingPreview::as_ref(app)
            .0
            .map(|preview| preview.drop)
            .filter(|drop| drop.preview_window() == Some(self.window_id));
        match incoming {
            Some(WindowDockingDrop::Split {
                locator, preview, ..
            }) => {
                render_split_preview(stack, appearance, &locator.pane_id.position_id(), preview);
            }
            Some(WindowDockingDrop::Tab { bounds, .. }) => {
                let preview = ConstrainedBox::new(
                    Rect::new()
                        .with_background(appearance.theme().accent())
                        .finish(),
                )
                .with_width(bounds.width())
                .with_height(bounds.height())
                .finish();
                stack.add_positioned_overlay_child(
                    SavePosition::new(preview, "workspace_view:window_docking_tab_preview")
                        .for_single_frame()
                        .finish(),
                    OffsetPositioning::offset_from_parent(
                        bounds.origin(),
                        ParentOffsetBounds::WindowByPosition,
                        ParentAnchor::TopLeft,
                        ChildAnchor::TopLeft,
                    ),
                );
            }
            Some(
                WindowDockingDrop::Local
                | WindowDockingDrop::Invalid
                | WindowDockingDrop::TearOut(_),
            )
            | None => {}
        }
    }

    pub(super) fn render_docking_overlay(
        &self,
        stack: &mut Stack,
        preview: Option<&DockingDestinationPreview>,
        appearance: &Appearance,
        app: &AppContext,
    ) {
        if !FeatureFlag::EnhancedPaneManagement.is_enabled() {
            return;
        }
        self.render_incoming_window_docking_overlay(stack, appearance, app);
        let (source, destination, cursor, flatten) = if let Some(drag) = self
            .docking_pane_drag
            .as_ref()
            .filter(|drag| !drag.canceled && drag.window_drop == WindowDockingDrop::Local)
        {
            (
                DockingSource::Pane(drag.source),
                drag.destination,
                drag.cursor,
                false,
            )
        } else if let Some(drag) = self
            .docking_tab_drag
            .filter(|drag| !drag.canceled && drag.window_drop == WindowDockingDrop::Local)
        {
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
        let Some(target) = self.docking_target(source, destination, cursor, flatten, preview, app)
        else {
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
            render_split_preview(stack, appearance, &pane_position, target.preview(direction));
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

fn render_split_preview(
    stack: &mut Stack,
    appearance: &Appearance,
    pane_position: &str,
    preview: RectF,
) {
    let accent = appearance.theme().accent();
    let element = ConstrainedBox::new(
        Rect::new()
            .with_background(accent.with_opacity(18))
            .with_border(Border::all(2.).with_border_fill(accent))
            .finish(),
    )
    .with_width(preview.width())
    .with_height(preview.height())
    .finish();
    stack.add_positioned_overlay_child(
        SavePosition::new(element, "workspace_view:docking_preview")
            .for_single_frame()
            .finish(),
        OffsetPositioning::offset_from_save_position_element(
            pane_position,
            preview.origin(),
            PositionedElementOffsetBounds::WindowByPosition,
            PositionedElementAnchor::TopLeft,
            ChildAnchor::TopLeft,
        ),
    );
}
