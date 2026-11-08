use std::collections::HashMap;
use std::iter;

use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::vec2f;
use warpui::platform::FullscreenState;
use warpui::windowing::System;
use warpui::{AppContext, DisplayIdx, Entity, SingletonEntity, WindowId};

use super::WorkspaceRegistry;
use super::cross_window_tab_drag::CrossWindowTabDrag;
use crate::features::FeatureFlag;
use crate::root_view::quake_mode_window_id;

#[derive(Default)]
pub(crate) struct DisplayLayoutState {
    displays: Vec<RectF>,
    last_bounds: HashMap<WindowId, RectF>,
    displaced: HashMap<WindowId, DisplacedWindow>,
}

impl Entity for DisplayLayoutState {
    type Event = ();
}

impl SingletonEntity for DisplayLayoutState {}

#[derive(Clone, Copy)]
struct DisplacedWindow {
    original: RectF,
    original_display: RectF,
    recovered: RectF,
}

impl DisplayLayoutState {
    fn observe(
        &mut self,
        displays: Vec<RectF>,
        mut bounds: HashMap<WindowId, RectF>,
    ) -> Vec<(WindowId, RectF)> {
        self.displaced
            .retain(|window_id, _| bounds.contains_key(window_id));
        let mut moves = Vec::new();

        if !self.displays.is_empty() && self.displays != displays {
            for (&window_id, current) in &mut bounds {
                let untouched = self
                    .displaced
                    .remove(&window_id)
                    .filter(|window| approximately_same_bounds(window.recovered, *current));

                if let Some(window) = untouched
                    && displays.contains(&window.original_display)
                {
                    *current = window.original;
                    moves.push((window_id, *current));
                    continue;
                }

                let original = untouched.or_else(|| {
                    let original = *self.last_bounds.get(&window_id)?;
                    let original_display = reachable_display(original, &self.displays)?;
                    (!displays.contains(&original_display)).then_some(DisplacedWindow {
                        original,
                        original_display,
                        recovered: *current,
                    })
                });

                if reachable_display(*current, &displays).is_none() {
                    *current = fit_on_display(*current, displays[0]);
                    moves.push((window_id, *current));
                }

                if let Some(mut window) = original
                    && !approximately_same_bounds(window.original, *current)
                {
                    window.recovered = *current;
                    self.displaced.insert(window_id, window);
                }
            }
        } else {
            self.displaced.retain(|window_id, window| {
                approximately_same_bounds(window.recovered, bounds[window_id])
            });
        }

        self.displays = displays;
        self.last_bounds = bounds;
        moves
    }
}

pub(crate) fn current_display_key(ctx: &AppContext) -> Option<String> {
    Some(display_key(&display_frames(ctx)?))
}

pub(crate) fn handle_screen_change(ctx: &mut AppContext) {
    // Windows converts each display with its own scale factor, so its display
    // rectangles and window bounds can use different coordinate spaces.
    if !FeatureFlag::EnhancedPaneManagement.is_enabled()
        || !matches!(
            ctx.windows().windowing_system(),
            Some(
                System::AppKit
                    | System::X11 {
                        is_x_wayland: false
                    }
            )
        )
        || ctx.windows().is_tiling_window_manager()
        || !ctx.has_singleton_model::<DisplayLayoutState>()
        || CrossWindowTabDrag::as_ref(ctx).is_active()
    {
        return;
    }

    let Some(displays) = display_frames(ctx) else {
        return;
    };
    let quake_window = quake_mode_window_id();
    let bounds = WorkspaceRegistry::as_ref(ctx)
        .all_workspaces(ctx)
        .into_iter()
        .filter_map(|(id, _)| {
            if Some(id) == quake_window {
                return None;
            }
            let window = ctx.windows().platform_window(id)?;
            if window.fullscreen_state() != FullscreenState::Normal {
                return None;
            }
            let frame = ctx.window_bounds(&id)?;
            valid_bounds(frame).then_some((id, frame))
        })
        .collect();

    let moves =
        DisplayLayoutState::handle(ctx).update(ctx, |state, _| state.observe(displays, bounds));
    for (id, bounds) in moves {
        ctx.set_and_cache_window_bounds(id, bounds);
    }
}

fn display_frames(ctx: &AppContext) -> Option<Vec<RectF>> {
    let count = ctx.windows().display_count();
    if !(1..=64).contains(&count) {
        return None;
    }
    iter::once(DisplayIdx::Primary)
        .chain((0..count - 1).map(DisplayIdx::External))
        .map(|idx| {
            ctx.windows()
                .bounds_for_display_idx(idx)
                .filter(|frame| valid_bounds(*frame))
        })
        .collect()
}

fn display_key(displays: &[RectF]) -> String {
    let mut frames: Vec<_> = displays
        .iter()
        .map(|frame| {
            [frame.min_x(), frame.min_y(), frame.width(), frame.height()]
                .map(|coordinate| coordinate.round() as i32)
        })
        .collect();
    frames.sort_unstable();
    frames
        .into_iter()
        .map(|[x, y, width, height]| format!("{x},{y},{width},{height}"))
        .collect::<Vec<_>>()
        .join(";")
}

fn valid_bounds(bounds: RectF) -> bool {
    bounds.min_x().is_finite()
        && bounds.min_y().is_finite()
        && bounds.max_x().is_finite()
        && bounds.max_y().is_finite()
        && bounds.width() > 0.
        && bounds.height() > 0.
}

fn reachable_display(window: RectF, displays: &[RectF]) -> Option<RectF> {
    let header_bottom = window.min_y() + window.height().min(36.);
    displays.iter().copied().find(|display| {
        let visible_width =
            window.max_x().min(display.max_x()) - window.min_x().max(display.min_x());
        let visible_height =
            header_bottom.min(display.max_y()) - window.min_y().max(display.min_y());
        visible_width >= window.width().min(96.) && visible_height >= window.height().min(20.)
    })
}

fn fit_on_display(window: RectF, display: RectF) -> RectF {
    let padding = vec2f(
        24_f32.min(display.width() / 8.),
        24_f32.min(display.height() / 8.),
    );
    let available = display.size() - padding * 2.;
    let size = vec2f(
        window.width().min(available.x()),
        window.height().min(available.y()),
    );
    RectF::new(display.origin() + (display.size() - size) * 0.5, size)
}

fn approximately_same_bounds(left: RectF, right: RectF) -> bool {
    (left.min_x() - right.min_x()).abs() <= 2.
        && (left.min_y() - right.min_y()).abs() <= 2.
        && (left.width() - right.width()).abs() <= 2.
        && (left.height() - right.height()).abs() <= 2.
}

#[cfg(test)]
#[path = "display_layouts_tests.rs"]
mod tests;
