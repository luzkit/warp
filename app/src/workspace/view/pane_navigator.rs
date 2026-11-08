use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::vec2f;
use warp_editor::editor::NavigationKey;
use warpui::accessibility::{AccessibilityContent, WarpA11yRole};
use warpui::elements::{
    Align, Border, ChildAnchor, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox,
    Container, CrossAxisAlignment, Flex, MouseStateHandle, OffsetPositioning, ParentAnchor,
    ParentElement, ParentOffsetBounds, Rect, SavePosition, ScrollTarget, ScrollToPositionMode,
    ScrollbarWidth, Shrinkable, Stack, Text,
};
use warpui::keymap::FixedBinding;
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Element, Entity, FocusContext, SingletonEntity, TypedActionView, View, ViewContext,
    WindowId,
};

use super::Workspace;
use super::enhanced_docking::DockingOperation;
use super::group_docking::{
    GroupDockDestination, GroupDockPlan, PaneLocation, PaneSelection, can_select_pane,
    commit_group_dock, plan_group_dock,
};
use crate::appearance::Appearance;
use crate::editor::{
    EditorView, Event as EditorEvent, PropagateAndNoOpNavigationKeys, SingleLineEditorOptions,
};
use crate::features::FeatureFlag;
use crate::modal::{Modal, ModalViewState};
use crate::pane_group::Direction;
use crate::settings::PaneSettings;
use crate::workspace::{PaneViewLocator, WorkspaceRegistry};

#[path = "pane_navigator_single.rs"]
mod single;

use single::{SingleDockAction, SingleDockPlan};

pub(in crate::workspace) fn init(app: &mut AppContext) {
    use warpui::keymap::macros::*;

    app.register_fixed_bindings([FixedBinding::new(
        "enter",
        NavigatorAction::Accept,
        id!("PaneNavigatorView") & !id!("EditorView"),
    )]);
}

#[derive(Clone, Debug)]
pub(super) enum NavigatorAction {
    Accept,
    Choose(usize),
    Toggle(usize),
    Clear,
    StartDocking,
    Back,
}

pub(super) struct CloseNavigator {
    restore_focus: bool,
}

#[derive(Clone, Copy)]
enum Choice {
    Pane(PaneLocation),
    NewTab(WindowId),
    Operation(DockingOperation),
    NewWindow(PaneLocation),
}

struct Row {
    title: String,
    detail: String,
    choice: Choice,
    activate_mouse: MouseStateHandle,
    select_mouse: MouseStateHandle,
}

impl Row {
    fn new(title: String, detail: String, choice: Choice) -> Self {
        Self {
            title,
            detail,
            choice,
            activate_mouse: Default::default(),
            select_mouse: Default::default(),
        }
    }

    fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let title = self.title.to_lowercase();
        let detail = self.detail.to_lowercase();
        query
            .split_whitespace()
            .all(|word| title.contains(word) || detail.contains(word))
    }
}

enum MovePlan {
    Group(GroupDockPlan),
    Single(SingleDockPlan),
}

enum Stage {
    Panes,
    Destinations {
        sources: Vec<PaneLocation>,
    },
    Placement {
        sources: Vec<PaneLocation>,
        target: PaneLocation,
        name: String,
    },
    Preview {
        sources: Vec<PaneLocation>,
        plan: Box<MovePlan>,
        summary: String,
    },
}

impl Stage {
    fn title(&self) -> &'static str {
        match self {
            Self::Panes => "Pane Navigator",
            Self::Destinations { .. } | Self::Placement { .. } | Self::Preview { .. } => {
                "Keyboard Docking"
            }
        }
    }

    fn instructions(&self) -> &'static str {
        match self {
            Self::Panes => {
                "↑ ↓ Browse · Enter Focus · Shift+Enter Select · Alt+Enter Dock · Esc Close"
            }
            Self::Destinations { .. } => "↑ ↓ Browse · Enter Choose destination · Esc Cancel",
            Self::Placement { .. } => "↑ ↓ Browse · Enter Preview placement · Esc Cancel",
            Self::Preview { .. } => "Enter Move panes · Esc Cancel",
        }
    }

    fn placeholder(&self) -> &'static str {
        match self {
            Self::Panes => "Search panes, tabs, and windows",
            Self::Destinations { .. } => "Search destinations",
            Self::Placement { .. } => "Search placements",
            Self::Preview { .. } => "",
        }
    }

    fn sources(&self) -> &[PaneLocation] {
        match self {
            Self::Panes => &[],
            Self::Destinations { sources }
            | Self::Placement { sources, .. }
            | Self::Preview { sources, .. } => sources,
        }
    }
}

#[derive(Default)]
struct Controls {
    clear: MouseStateHandle,
    dock: MouseStateHandle,
    back: MouseStateHandle,
    apply: MouseStateHandle,
}

pub(super) struct PaneNavigatorView {
    search: warpui::ViewHandle<EditorView>,
    origin: Option<PaneLocation>,
    stage: Stage,
    rows: Vec<Row>,
    query: String,
    selected: Option<usize>,
    notice: Option<String>,
    scroll: ClippedScrollStateHandle,
    mouse: Controls,
}

impl PaneNavigatorView {
    fn new(ctx: &mut ViewContext<Self>) -> Self {
        let search = ctx.add_typed_action_view(|ctx| {
            EditorView::single_line(
                SingleLineEditorOptions {
                    propagate_and_no_op_vertical_navigation_keys:
                        PropagateAndNoOpNavigationKeys::Always,
                    ..Default::default()
                },
                ctx,
            )
        });
        ctx.subscribe_to_view(&search, Self::handle_search_event);
        ctx.observe(&PaneSelection::handle(ctx), |_, _, ctx| ctx.notify());
        Self {
            search,
            origin: None,
            stage: Stage::Panes,
            rows: Vec::new(),
            query: String::new(),
            selected: None,
            notice: None,
            scroll: Default::default(),
            mouse: Default::default(),
        }
    }

    fn reset(
        &mut self,
        origin: Option<PaneLocation>,
        stage: Stage,
        rows: Vec<Row>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.origin = origin;
        self.set_stage(stage, rows, ctx);
    }

    fn set_stage(&mut self, stage: Stage, rows: Vec<Row>, ctx: &mut ViewContext<Self>) {
        self.stage = stage;
        self.rows = rows;
        self.query.clear();
        self.selected = (!self.rows.is_empty()).then_some(0);
        self.notice = None;
        self.scroll = Default::default();
        self.search.update(ctx, |editor, ctx| {
            editor.clear_buffer_and_reset_undo_stack(ctx);
            editor.set_placeholder_text(self.stage.placeholder(), ctx);
        });
        if matches!(self.stage, Stage::Preview { .. }) {
            ctx.focus_self();
        } else {
            ctx.focus(&self.search);
        }
        self.announce(ctx);
        ctx.notify();
    }

    fn visible_rows(&self) -> impl Iterator<Item = (usize, &Row)> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.matches(&self.query))
    }

    fn handle_search_event(
        &mut self,
        search: warpui::ViewHandle<EditorView>,
        event: &EditorEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            EditorEvent::Edited(_) => {
                self.query = search.as_ref(ctx).buffer_text(ctx).to_lowercase();
                let query = self.query.trim();
                let first = self
                    .visible_rows()
                    .min_by_key(|(_, row)| {
                        let title = row.title.to_lowercase();
                        if title == query {
                            0
                        } else if format!("{title} {}", row.detail.to_lowercase()).contains(query) {
                            1
                        } else {
                            2
                        }
                    })
                    .map(|(index, _)| index);
                self.selected = first;
                self.scroll = Default::default();
                self.announce(ctx);
                ctx.notify();
            }
            EditorEvent::Navigate(NavigationKey::Up) => self.move_selection(false, ctx),
            EditorEvent::Navigate(NavigationKey::Down) => self.move_selection(true, ctx),
            EditorEvent::Enter => self.accept(ctx),
            EditorEvent::ShiftEnter if matches!(self.stage, Stage::Panes) => {
                if let Some(selected) = self.selected {
                    self.toggle_row(selected, ctx);
                }
            }
            EditorEvent::AltEnter if matches!(self.stage, Stage::Panes) => {
                self.start_docking(ctx);
            }
            EditorEvent::Escape => ctx.emit(CloseNavigator {
                restore_focus: true,
            }),
            _ => {}
        }
    }

    fn move_selection(&mut self, forward: bool, ctx: &mut ViewContext<Self>) {
        let visible: Vec<_> = self.visible_rows().map(|(index, _)| index).collect();
        if visible.is_empty() {
            return;
        }
        let next = visible
            .iter()
            .position(|&index| Some(index) == self.selected)
            .map_or(0, |index| {
                if forward {
                    (index + 1) % visible.len()
                } else {
                    (index + visible.len() - 1) % visible.len()
                }
            });
        self.selected = Some(visible[next]);
        self.scroll.scroll_to_position(ScrollTarget {
            position_id: row_position(visible[next]),
            mode: ScrollToPositionMode::FullyIntoView,
        });
        self.announce(ctx);
        ctx.notify();
    }

    fn toggle_row(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(Row {
            choice: Choice::Pane(location),
            ..
        }) = self.rows.get(index)
        else {
            return;
        };
        let location = *location;
        if can_select_pane(location, ctx) {
            PaneSelection::handle(ctx).update(ctx, |selection, ctx| {
                selection.toggle(location);
                ctx.notify();
            });
            self.selected = Some(index);
            self.announce(ctx);
            ctx.notify();
        } else {
            self.show_notice("This pane is unavailable for docking.".to_owned(), ctx);
        }
    }

    fn accept(&mut self, ctx: &mut ViewContext<Self>) {
        if let Stage::Preview { plan, .. } = &self.stage {
            let focus_moved = *PaneSettings::as_ref(ctx).focus_moved_docking_panes;
            let result = match plan.as_ref() {
                MovePlan::Group(plan) => commit_group_dock(plan.clone(), focus_moved, ctx),
                MovePlan::Single(plan) => {
                    let keep_focus = if focus_moved { None } else { self.origin };
                    plan.commit(keep_focus, ctx)
                }
            };
            match result {
                Ok(()) => ctx.emit(CloseNavigator {
                    restore_focus: false,
                }),
                Err(message) => self.show_notice(message, ctx),
            }
        } else if let Some(index) = self.selected {
            self.choose(index, ctx);
        }
    }

    fn choose(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let choice = row.choice;
        let label = format!("{} · {}", row.title, row.detail);
        match (&self.stage, choice) {
            (Stage::Panes, Choice::Pane(pane)) => self.focus_pane(pane, ctx),
            (Stage::Destinations { sources }, Choice::NewTab(window_id)) => self.preview(
                sources.clone(),
                GroupDockDestination::NewTab { window_id },
                label,
                ctx,
            ),
            (Stage::Destinations { sources }, Choice::Pane(target)) => {
                let mut rows: Vec<_> = [
                    Direction::Left,
                    Direction::Right,
                    Direction::Up,
                    Direction::Down,
                ]
                .into_iter()
                .map(|direction| {
                    Row::new(
                        placement_label(direction).to_owned(),
                        "Split pane".to_owned(),
                        Choice::Operation(DockingOperation::Split {
                            direction,
                            flatten: false,
                        }),
                    )
                })
                .collect();
                if let [source] = sources.as_slice()
                    && source.window_id == target.window_id
                {
                    rows.extend([
                        Row::new(
                            "Swap".to_owned(),
                            "Exchange panes".to_owned(),
                            Choice::Operation(DockingOperation::Swap),
                        ),
                        Row::new(
                            "Stack".to_owned(),
                            "Share one pane slot".to_owned(),
                            Choice::Operation(DockingOperation::Stack),
                        ),
                    ]);
                }
                self.set_stage(
                    Stage::Placement {
                        sources: sources.clone(),
                        target,
                        name: label,
                    },
                    rows,
                    ctx,
                );
            }
            (
                Stage::Placement {
                    sources,
                    target,
                    name,
                },
                Choice::Operation(DockingOperation::Split { direction, .. }),
            ) => self.preview(
                sources.clone(),
                GroupDockDestination::Split {
                    target: *target,
                    direction,
                },
                format!("{} of {name}", placement_label(direction)),
                ctx,
            ),
            (
                Stage::Placement {
                    sources,
                    target,
                    name,
                },
                Choice::Operation(operation),
            ) => {
                if let [source] = sources.as_slice() {
                    let verb = if operation == DockingOperation::Swap {
                        "Swap"
                    } else {
                        "Stack"
                    };
                    self.preview_single(
                        *source,
                        SingleDockAction::Center {
                            target: *target,
                            operation,
                        },
                        format!("{verb} the selected pane with {name}"),
                        ctx,
                    );
                }
            }
            (Stage::Destinations { .. }, Choice::NewWindow(source)) => self.preview_single(
                source,
                SingleDockAction::NewWindow,
                "Move the selected pane to a new window".to_owned(),
                ctx,
            ),
            _ => {}
        }
    }

    fn focus_pane(&mut self, pane: PaneLocation, ctx: &mut ViewContext<Self>) {
        let Some(workspace) = WorkspaceRegistry::as_ref(ctx).get(pane.window_id, ctx) else {
            self.show_notice("This window has closed.".to_owned(), ctx);
            return;
        };
        let locator = PaneViewLocator {
            pane_group_id: pane.pane_group_id,
            pane_id: pane.pane_id,
        };
        let is_open = workspace
            .as_ref(ctx)
            .get_pane_group_view_with_id(pane.pane_group_id)
            .is_some_and(|group| group.as_ref(ctx).has_pane_id(pane.pane_id));
        if !is_open {
            self.show_notice("This pane has closed.".to_owned(), ctx);
            return;
        }
        if pane.window_id == ctx.window_id() {
            workspace.update(ctx, |workspace, ctx| workspace.focus_pane(locator, ctx));
        } else if let Some(root) = ctx.root_view_id(pane.window_id) {
            ctx.windows().show_window_and_focus_app(pane.window_id);
            ctx.dispatch_action_for_view(
                pane.window_id,
                root,
                "root_view:handle_pane_navigation_event",
                &locator,
            );
        }
        ctx.emit(CloseNavigator {
            restore_focus: false,
        });
    }

    fn start_docking(&mut self, ctx: &mut ViewContext<Self>) {
        let sources = selected_sources(self.origin, ctx);
        if sources.is_empty() {
            self.show_notice("Select a terminal pane to dock.".to_owned(), ctx);
            return;
        }
        self.open_destinations(sources, ctx);
    }

    fn open_destinations(&mut self, sources: Vec<PaneLocation>, ctx: &mut ViewContext<Self>) {
        let rows = navigator_rows(None, Some(&sources), ctx);
        self.set_stage(Stage::Destinations { sources }, rows, ctx);
    }

    fn preview(
        &mut self,
        sources: Vec<PaneLocation>,
        destination: GroupDockDestination,
        label: String,
        ctx: &mut ViewContext<Self>,
    ) {
        match plan_group_dock(&sources, destination, ctx) {
            Ok(plan) => {
                let count = sources.len();
                let noun = if count == 1 { "pane" } else { "panes" };
                let summary = format!("Move {count} {noun} to {label}");
                self.set_stage(
                    Stage::Preview {
                        sources,
                        plan: Box::new(MovePlan::Group(plan)),
                        summary,
                    },
                    Vec::new(),
                    ctx,
                );
            }
            Err(message) => self.show_notice(message, ctx),
        }
    }

    fn preview_single(
        &mut self,
        source: PaneLocation,
        action: SingleDockAction,
        summary: String,
        ctx: &mut ViewContext<Self>,
    ) {
        match SingleDockPlan::new(source, action, ctx) {
            Ok(plan) => self.set_stage(
                Stage::Preview {
                    sources: vec![source],
                    plan: Box::new(MovePlan::Single(plan)),
                    summary,
                },
                Vec::new(),
                ctx,
            ),
            Err(message) => self.show_notice(message, ctx),
        }
    }

    fn back(&mut self, ctx: &mut ViewContext<Self>) {
        match &self.stage {
            Stage::Panes => {}
            Stage::Destinations { .. } => {
                let rows = navigator_rows(None, None, ctx);
                self.set_stage(Stage::Panes, rows, ctx);
            }
            Stage::Placement { sources, .. } | Stage::Preview { sources, .. } => {
                self.open_destinations(sources.clone(), ctx);
            }
        }
    }

    fn show_notice(&mut self, message: String, ctx: &mut ViewContext<Self>) {
        self.notice = Some(message);
        self.announce(ctx);
        ctx.notify();
    }

    fn announcement(&self, app: &AppContext) -> Option<AccessibilityContent> {
        if !*PaneSettings::as_ref(app).announce_pane_docking {
            return None;
        }
        let value = if let Some(notice) = &self.notice {
            notice.clone()
        } else if let Stage::Preview { summary, .. } = &self.stage {
            summary.clone()
        } else if let Some(row) = self.selected.and_then(|index| self.rows.get(index)) {
            let status = match row.choice {
                Choice::Pane(pane) if matches!(self.stage, Stage::Panes) => {
                    if PaneSelection::as_ref(app).members().contains(&pane) {
                        " · selected"
                    } else {
                        " · unselected"
                    }
                }
                _ => "",
            };
            format!("{} · {}{status}", row.title, row.detail)
        } else {
            self.stage.title().to_owned()
        };
        Some(AccessibilityContent::new(
            value,
            self.stage.instructions(),
            WarpA11yRole::MenuItemRole,
        ))
    }

    fn announce(&self, ctx: &mut ViewContext<Self>) {
        if let Some(content) = self.announcement(ctx) {
            ctx.emit_a11y_content(content);
        }
    }

    fn render_rows(&self, appearance: &Appearance, app: &AppContext) -> Box<dyn Element> {
        let mut list = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(5.);
        let members = PaneSelection::as_ref(app).members();
        for (index, row) in self.visible_rows() {
            let row_label = format!("{} · {}", row.title, row.detail);
            let mut layout = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.);
            if let Choice::Pane(pane) = row.choice
                && matches!(self.stage, Stage::Panes)
            {
                let mut checkbox = appearance
                    .ui_builder()
                    .checkbox(row.select_mouse.clone(), None)
                    .check(members.contains(&pane));
                if !can_select_pane(pane, app) {
                    checkbox = checkbox.disabled();
                }
                let checkbox = checkbox
                    .build()
                    .on_click(move |ctx, _, _| {
                        ctx.dispatch_typed_action(NavigatorAction::Toggle(index))
                    })
                    .finish();
                layout.add_child(
                    SavePosition::new(checkbox, &format!("pane_navigator:select:{index}"))
                        .for_single_frame()
                        .finish(),
                );
            }
            layout.add_child(
                Shrinkable::new(
                    1.,
                    navigator_button(
                        &row_label,
                        &row.activate_mouse,
                        NavigatorAction::Choose(index),
                        Some(index) == self.selected,
                        appearance,
                    ),
                )
                .finish(),
            );
            list.add_child(
                SavePosition::new(layout.finish(), &row_position(index))
                    .for_single_frame()
                    .finish(),
            );
        }
        if self.visible_rows().next().is_none() {
            list.add_child(navigator_text(
                "No matching panes or destinations.",
                appearance,
            ));
        }
        let theme = appearance.theme();
        ClippedScrollable::vertical(
            self.scroll.clone(),
            list.finish(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_text_color().into(),
            theme.active_ui_text_color().into(),
            theme.surface_2().into(),
        )
        .with_overlayed_scrollbar()
        .finish()
    }

    fn render_footer(&self, appearance: &Appearance, app: &AppContext) -> Box<dyn Element> {
        let mut row = Flex::row()
            .with_spacing(8.)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        match &self.stage {
            Stage::Panes => {
                let selected = PaneSelection::as_ref(app).members().len();
                row.add_child(navigator_text(format!("{selected} selected"), appearance));
                row.add_child(navigator_button(
                    "Dock Panes",
                    &self.mouse.dock,
                    NavigatorAction::StartDocking,
                    true,
                    appearance,
                ));
                if selected > 0 {
                    row.add_child(navigator_button(
                        "Clear Selection",
                        &self.mouse.clear,
                        NavigatorAction::Clear,
                        false,
                        appearance,
                    ));
                }
            }
            Stage::Destinations { .. } | Stage::Placement { .. } | Stage::Preview { .. } => {
                row.add_child(navigator_button(
                    "Back",
                    &self.mouse.back,
                    NavigatorAction::Back,
                    false,
                    appearance,
                ));
                if matches!(self.stage, Stage::Preview { .. }) {
                    row.add_child(navigator_button(
                        "Move Panes",
                        &self.mouse.apply,
                        NavigatorAction::Accept,
                        true,
                        appearance,
                    ));
                } else {
                    row.add_child(navigator_text(
                        format!("{} panes to move", self.stage.sources().len()),
                        appearance,
                    ));
                }
            }
        }
        row.finish()
    }
}

impl Entity for PaneNavigatorView {
    type Event = CloseNavigator;
}

impl View for PaneNavigatorView {
    fn ui_name() -> &'static str {
        "PaneNavigatorView"
    }

    fn on_focus(&mut self, focus: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus.is_self_focused() && !matches!(self.stage, Stage::Preview { .. }) {
            ctx.focus(&self.search);
        }
    }

    fn accessibility_contents(&self, app: &AppContext) -> Option<AccessibilityContent> {
        self.announcement(app)
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(10.)
            .with_child(navigator_text(self.stage.title(), appearance));
        if let Stage::Preview { plan, summary, .. } = &self.stage {
            column.add_child(
                SavePosition::new(
                    navigator_text(summary.clone(), appearance),
                    "pane_navigator:preview",
                )
                .for_single_frame()
                .finish(),
            );
            if let MovePlan::Group(plan) = plan.as_ref()
                && let Some((target, incoming)) = plan.preview()
            {
                column.add_child(render_geometry(target, incoming, appearance));
            }
        } else {
            let search = appearance
                .ui_builder()
                .text_input(self.search.clone())
                .build()
                .finish();
            column.add_child(
                SavePosition::new(search, "pane_navigator:search")
                    .for_single_frame()
                    .finish(),
            );
            column.add_child(Shrinkable::new(1., self.render_rows(appearance, app)).finish());
        }
        if let Some(notice) = &self.notice {
            column.add_child(navigator_text(notice.clone(), appearance));
        }
        column.add_child(self.render_footer(appearance, app));
        column.add_child(navigator_text(self.stage.instructions(), appearance));
        ConstrainedBox::new(column.finish())
            .with_max_height(550.)
            .finish()
    }
}

impl TypedActionView for PaneNavigatorView {
    type Action = NavigatorAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match *action {
            NavigatorAction::Accept => self.accept(ctx),
            NavigatorAction::Choose(index) => self.choose(index, ctx),
            NavigatorAction::Toggle(index) => self.toggle_row(index, ctx),
            NavigatorAction::StartDocking => self.start_docking(ctx),
            NavigatorAction::Back => self.back(ctx),
            NavigatorAction::Clear => {
                PaneSelection::handle(ctx).update(ctx, |selection, ctx| {
                    selection.clear();
                    ctx.notify();
                });
                self.announce(ctx);
                ctx.notify();
            }
        }
    }
}

impl Workspace {
    pub(super) fn open_pane_navigator(&mut self, docking: bool, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::EnhancedPaneManagement.is_enabled()
            || self.is_tab_drag_preview()
            || self.docking_tab_drag.is_some()
            || self.docking_pane_drag.is_some()
            || self.tabs.is_empty()
        {
            return;
        }
        let active = self.active_tab_pane_group();
        let origin = Some(PaneLocation {
            window_id: self.window_id,
            pane_group_id: active.id(),
            pane_id: active.as_ref(ctx).focused_pane_id(ctx),
        });
        let (stage, rows) = if docking {
            let sources = selected_sources(origin, ctx);
            let rows = navigator_rows(Some(self), Some(&sources), ctx);
            (Stage::Destinations { sources }, rows)
        } else {
            (Stage::Panes, navigator_rows(Some(self), None, ctx))
        };
        let modal = self.pane_navigator_modal.get_or_insert_with(|| {
            let body = ctx.add_typed_action_view(PaneNavigatorView::new);
            ctx.subscribe_to_view(&body, |workspace, _, event, ctx| {
                workspace.close_pane_navigator(event.restore_focus, ctx);
            });
            let modal = ctx.add_typed_action_view(|ctx| {
                Modal::new(Some("Panes".to_owned()), body, ctx)
                    .with_modal_style(UiComponentStyles {
                        width: Some(620.),
                        ..Default::default()
                    })
                    .with_max_height_percentage(0.9)
            });
            ctx.subscribe_to_view(&modal, |workspace, _, _, ctx| {
                workspace.close_pane_navigator(true, ctx);
            });
            ModalViewState::new(modal)
        });
        let body = modal.view.as_ref(ctx).body().clone();
        body.update(ctx, |body, ctx| body.reset(origin, stage, rows, ctx));
        modal.open();
        ctx.focus(&modal.view);
        ctx.notify();
    }

    fn close_pane_navigator(&mut self, restore_focus: bool, ctx: &mut ViewContext<Self>) {
        if let Some(modal) = &mut self.pane_navigator_modal {
            let modal_has_focus = modal.view.is_self_or_child_focused(ctx);
            modal.close();
            if (restore_focus || modal_has_focus) && !self.tabs.is_empty() {
                self.focus_active_tab(ctx);
            }
            ctx.notify();
        }
    }
}

fn selected_sources(origin: Option<PaneLocation>, ctx: &AppContext) -> Vec<PaneLocation> {
    let selected = PaneSelection::as_ref(ctx).members();
    if selected.is_empty() {
        origin.into_iter().collect()
    } else {
        selected.to_vec()
    }
}

fn navigator_rows(
    local: Option<&Workspace>,
    sources: Option<&[PaneLocation]>,
    ctx: &AppContext,
) -> Vec<Row> {
    let mut workspaces: Vec<_> = WorkspaceRegistry::as_ref(ctx)
        .all_workspaces(ctx)
        .into_iter()
        .filter_map(|(window, handle)| Some((window, handle.try_as_ref(ctx)?)))
        .collect();
    if let Some(local) = local {
        // A workspace being updated is temporarily unavailable to its registry handle.
        workspaces.retain(|(window, _)| *window != local.window_id);
        workspaces.push((local.window_id, local));
    }
    workspaces.sort_by_key(|(window, _)| *window);
    let mut rows = Vec::new();
    for (index, (window, workspace)) in workspaces.into_iter().enumerate() {
        if workspace.tabs.is_empty() || workspace.is_tab_drag_preview() {
            continue;
        }
        let window_name = format!("Window {}", index + 1);
        let window_title = workspace
            .active_tab_pane_group()
            .as_ref(ctx)
            .display_title(ctx);
        if sources.is_some() {
            rows.push(Row::new(
                "New tab".to_owned(),
                format!("{window_name} · {window_title}"),
                Choice::NewTab(window),
            ));
            if let Some([source]) = sources
                && source.window_id == window
            {
                rows.push(Row::new(
                    "New window".to_owned(),
                    format!("From {window_name} · {window_title}"),
                    Choice::NewWindow(*source),
                ));
            }
        }
        for (tab_index, tab) in workspace.tabs.iter().enumerate() {
            if tab.detached {
                continue;
            }
            let group = tab.pane_group.as_ref(ctx);
            let detail = format!(
                "{window_name} · Tab {} · {}",
                tab_index + 1,
                group.display_title(ctx),
            );
            for (pane_index, pane_id) in group.visible_pane_ids().into_iter().enumerate() {
                if !pane_id.is_terminal_pane() {
                    continue;
                }
                let pane = PaneLocation {
                    window_id: window,
                    pane_group_id: tab.pane_group.id(),
                    pane_id,
                };
                if sources.is_some_and(|sources| sources.contains(&pane)) {
                    continue;
                }
                let title = group.pane_by_id(pane_id).map(|pane| {
                    let configuration = pane.pane_configuration();
                    let configuration = configuration.as_ref(ctx);
                    configuration
                        .custom_vertical_tabs_title()
                        .unwrap_or_else(|| configuration.title())
                        .to_owned()
                });
                rows.push(Row::new(
                    title
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| format!("Pane {}", pane_index + 1)),
                    detail.clone(),
                    Choice::Pane(pane),
                ));
            }
        }
    }
    rows
}

fn row_position(index: usize) -> String {
    format!("pane_navigator:row:{index}")
}

fn placement_label(direction: Direction) -> &'static str {
    match direction {
        Direction::Left => "Left",
        Direction::Right => "Right",
        Direction::Up => "Above",
        Direction::Down => "Below",
    }
}

fn navigator_text(label: impl Into<String>, appearance: &Appearance) -> Box<dyn Element> {
    Text::new(
        label.into(),
        appearance.ui_font_family(),
        appearance.ui_font_size(),
    )
    .with_color(appearance.theme().active_ui_text_color().into())
    .finish()
}

fn navigator_button(
    label: &str,
    mouse: &MouseStateHandle,
    action: NavigatorAction,
    active: bool,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let variant = if active {
        ButtonVariant::Accent
    } else {
        ButtonVariant::Secondary
    };
    SavePosition::new(
        appearance
            .ui_builder()
            .button(variant, mouse.clone())
            .with_centered_text_label(label.to_owned())
            .build()
            .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
            .finish(),
        label,
    )
    .for_single_frame()
    .finish()
}

fn render_geometry(target: RectF, incoming: RectF, appearance: &Appearance) -> Box<dyn Element> {
    let top_left = target.origin().min(incoming.origin());
    let bottom_right = (target.origin() + target.size()).max(incoming.origin() + incoming.size());
    let size = bottom_right - top_left;
    let scale = (390. / size.x()).min(145. / size.y());
    let theme = appearance.theme();
    let mut diagram =
        Stack::new().with_child(Rect::new().with_background(theme.surface_1()).finish());
    for (bounds, title, active) in [
        (target, "Destination", false),
        (incoming, "Moved panes", true),
    ] {
        let tile_size = bounds.size() * scale;
        let label = navigator_text(title, appearance);
        let border = if active {
            theme.accent()
        } else {
            theme.outline()
        };
        let tile = Container::new(Align::new(label).top_left().finish())
            .with_horizontal_padding(4.)
            .with_background(theme.surface_2())
            .with_border(Border::all(1.).with_border_fill(border))
            .finish();
        let tile = ConstrainedBox::new(tile)
            .with_width(tile_size.x())
            .with_height(tile_size.y())
            .finish();
        diagram.add_positioned_child(
            tile,
            OffsetPositioning::offset_from_parent(
                (bounds.origin() - top_left) * scale,
                ParentOffsetBounds::ParentByPosition,
                ParentAnchor::TopLeft,
                ChildAnchor::TopLeft,
            ),
        );
    }
    let size = vec2f(size.x() * scale, size.y() * scale);
    ConstrainedBox::new(diagram.finish())
        .with_width(size.x())
        .with_height(size.y())
        .finish()
}
