use warpui::clipboard::ClipboardContent;
use warpui::elements::{
    Border, ChildAnchor, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CrossAxisAlignment, Empty, Flex, MouseStateHandle, OffsetPositioning, ParentAnchor,
    ParentElement, ParentOffsetBounds, Rect, SavePosition, ScrollbarWidth, Shrinkable, Stack, Text,
};
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Element, Entity, FocusContext, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};

use super::Workspace;
use super::saved_pane_layouts::SavedLayoutPreview;
use crate::appearance::Appearance;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions};
use crate::features::FeatureFlag;
use crate::modal::{Modal, ModalViewState};
use crate::settings_view::DisplayCount;
use crate::workspace::display_layouts::current_display_key;
use crate::workspace::pane_layouts::{
    LayoutCatalog, LayoutDocument, LayoutKind, LayoutPlan, LayoutStrategy,
};

const MODAL_WIDTH: f32 = 620.;
const MODAL_HEIGHT: f32 = 560.;
const PREVIEW_WIDTH: f32 = 500.;
const PREVIEW_HEIGHT: f32 = 132.;

#[derive(Clone, Debug)]
pub(super) enum LayoutsAction {
    Save(LayoutKind),
    Preview(String),
    Copy(String),
    Import,
    PreviewDisplayProfile,
    BindDisplayProfile,
    SelectAlternative(usize),
    Back,
    Apply,
}

pub(super) enum LayoutsEvent {
    Save { name: String, kind: LayoutKind },
    Preview(String),
    Copy(String),
    Import,
    PreviewDisplayProfile,
    BindDisplayProfile,
    Apply,
    Close,
}

#[derive(Default)]
struct LayoutsMouseStates {
    save_template: MouseStateHandle,
    save_snapshot: MouseStateHandle,
    import: MouseStateHandle,
    preview_profile: MouseStateHandle,
    bind_profile: MouseStateHandle,
    apply: MouseStateHandle,
    back: MouseStateHandle,
    alternatives: [MouseStateHandle; 4],
}

struct LayoutRow {
    name: String,
    kind: LayoutKind,
    preview: MouseStateHandle,
    copy: MouseStateHandle,
}

struct LayoutSelection {
    document: LayoutDocument,
    preview: Result<SavedLayoutPreview, String>,
    alternative: usize,
}

pub(super) struct PaneLayoutsView {
    name: ViewHandle<EditorView>,
    scroll: ClippedScrollStateHandle,
    mouse: LayoutsMouseStates,
    rows: Vec<LayoutRow>,
    selection: Option<LayoutSelection>,
    display_profile: Option<String>,
    display_available: bool,
    notice: Option<String>,
}

impl PaneLayoutsView {
    fn new(ctx: &mut ViewContext<Self>) -> Self {
        let name = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(SingleLineEditorOptions::default(), ctx);
            editor.set_placeholder_text("Layout name", ctx);
            editor
        });
        ctx.subscribe_to_view(&name, |_, _, event, ctx| {
            if matches!(event, EditorEvent::Escape) {
                ctx.emit(LayoutsEvent::Close);
            }
        });
        Self {
            name,
            scroll: Default::default(),
            mouse: Default::default(),
            rows: Vec::new(),
            selection: None,
            display_profile: None,
            display_available: false,
            notice: None,
        }
    }

    fn reset(
        &mut self,
        catalog: anyhow::Result<LayoutCatalog>,
        display: Option<&str>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.selection = None;
        self.scroll = Default::default();
        self.notice = None;
        self.rows.clear();
        self.display_profile = None;
        self.name.update(ctx, |editor, ctx| {
            editor.clear_buffer_and_reset_undo_stack(ctx);
        });
        self.refresh_catalog(catalog, display, ctx);
    }

    fn refresh_catalog(
        &mut self,
        catalog: anyhow::Result<LayoutCatalog>,
        display: Option<&str>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.display_available = display.is_some();
        match catalog {
            Ok(catalog) => {
                self.rows = catalog
                    .documents()
                    .iter()
                    .map(|document| LayoutRow {
                        name: document.name.clone(),
                        kind: document.kind,
                        preview: Default::default(),
                        copy: Default::default(),
                    })
                    .collect();
                self.display_profile = display
                    .and_then(|key| catalog.display_profile(key))
                    .map(|document| document.name.clone());
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        ctx.notify();
    }

    fn set_notice(&mut self, notice: String, ctx: &mut ViewContext<Self>) {
        self.notice = Some(notice);
        ctx.notify();
    }

    fn select(
        &mut self,
        document: LayoutDocument,
        preview: Result<SavedLayoutPreview, String>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.selection = Some(LayoutSelection {
            document,
            preview,
            alternative: 0,
        });
        self.notice = None;
        self.scroll = Default::default();
        ctx.focus_self();
        ctx.notify();
    }

    fn render_catalog(&self, appearance: &Appearance) -> Box<dyn Element> {
        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(10.)
            .with_child(layout_text(
                "Templates reuse the layout. Snapshots also remember which live panes belong in it.",
                appearance,
            ));
        let editor = appearance
            .ui_builder()
            .text_input(self.name.clone())
            .build()
            .finish();
        column.add_child(
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.)
                .with_child(
                    Shrinkable::new(
                        1.,
                        SavePosition::new(editor, "pane_layouts:name")
                            .for_single_frame()
                            .finish(),
                    )
                    .finish(),
                )
                .with_child(layout_button(
                    "Save Template",
                    &self.mouse.save_template,
                    LayoutsAction::Save(LayoutKind::Template),
                    false,
                    appearance,
                ))
                .with_child(layout_button(
                    "Save Snapshot",
                    &self.mouse.save_snapshot,
                    LayoutsAction::Save(LayoutKind::Snapshot),
                    false,
                    appearance,
                ))
                .finish(),
        );
        let mut controls = Flex::row().with_spacing(8.).with_child(layout_button(
            "Import from Clipboard",
            &self.mouse.import,
            LayoutsAction::Import,
            false,
            appearance,
        ));
        if let Some(profile) = &self.display_profile {
            controls.add_child(layout_button(
                "Preview Display Profile",
                &self.mouse.preview_profile,
                LayoutsAction::PreviewDisplayProfile,
                false,
                appearance,
            ));
            column.add_child(layout_text(
                format!("Display profile · {profile}"),
                appearance,
            ));
        }
        column.add_child(controls.finish());
        if self.rows.is_empty() {
            column.add_child(layout_text("No saved pane layouts yet.", appearance));
        }
        for row in &self.rows {
            let kind = match row.kind {
                LayoutKind::Template => "Template",
                LayoutKind::Snapshot => "Snapshot",
            };
            let preview = layout_button(
                "Preview",
                &row.preview,
                LayoutsAction::Preview(row.name.clone()),
                false,
                appearance,
            );
            column.add_child(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(8.)
                    .with_child(
                        Shrinkable::new(
                            1.,
                            layout_text(format!("{} · {kind}", row.name), appearance),
                        )
                        .finish(),
                    )
                    .with_child(
                        SavePosition::new(preview, &format!("pane_layouts:preview:{}", row.name))
                            .for_single_frame()
                            .finish(),
                    )
                    .with_child(layout_button(
                        "Copy Template JSON",
                        &row.copy,
                        LayoutsAction::Copy(row.name.clone()),
                        false,
                        appearance,
                    ))
                    .finish(),
            );
        }
        column.finish()
    }

    fn render_selection(
        &self,
        selection: &LayoutSelection,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(10.)
            .with_child(layout_text(
                format!("Preview · {}", selection.document.name),
                appearance,
            ));
        match &selection.preview {
            Err(message) => column.add_child(layout_text(message.clone(), appearance)),
            Ok(preview) => {
                let mut alternatives = Flex::row().with_spacing(6.);
                for (index, plan) in preview.alternatives.iter().enumerate() {
                    alternatives.add_child(layout_button(
                        strategy_label(plan.strategy),
                        &self.mouse.alternatives[index],
                        LayoutsAction::SelectAlternative(index),
                        selection.alternative == index,
                        appearance,
                    ));
                }
                column.add_child(alternatives.finish());
                if let Some(plan) = preview.alternatives.get(selection.alternative) {
                    if plan.missing_slots > 0 {
                        column.add_child(layout_text(
                            format!("Unfilled slots · {}", plan.missing_slots),
                            appearance,
                        ));
                    }
                    if plan.overflow_sessions > 0 {
                        column.add_child(layout_text(
                            format!(
                                "Sessions kept in additional tabs · {}",
                                plan.overflow_sessions
                            ),
                            appearance,
                        ));
                    }
                    for (index, tab) in plan.tabs.iter().enumerate() {
                        column.add_child(layout_text(
                            tab.name
                                .clone()
                                .unwrap_or_else(|| format!("Tab {}", index + 1)),
                            appearance,
                        ));
                        column.add_child(render_tab_preview(plan, index, preview, appearance));
                    }
                }
            }
        }
        column.finish()
    }

    fn render_footer(&self, appearance: &Appearance) -> Box<dyn Element> {
        let Some(selection) = &self.selection else {
            return Empty::new().finish();
        };
        let mut row = Flex::row().with_spacing(8.).with_child(layout_button(
            "Back",
            &self.mouse.back,
            LayoutsAction::Back,
            false,
            appearance,
        ));
        if self.display_available {
            row.add_child(layout_button(
                "Save Display Profile",
                &self.mouse.bind_profile,
                LayoutsAction::BindDisplayProfile,
                false,
                appearance,
            ));
        }
        if selection
            .preview
            .as_ref()
            .is_ok_and(|preview| !preview.alternatives.is_empty())
        {
            row.add_child(layout_button(
                "Apply Layout",
                &self.mouse.apply,
                LayoutsAction::Apply,
                true,
                appearance,
            ));
        }
        row.finish()
    }
}

fn strategy_label(strategy: LayoutStrategy) -> &'static str {
    match strategy {
        LayoutStrategy::Preserve => "Keep proportions",
        LayoutStrategy::Balance => "Rebalance",
        LayoutStrategy::Stack => "Stack overflow",
        LayoutStrategy::Tabs => "Use more tabs",
    }
}

fn layout_text(label: impl Into<String>, appearance: &Appearance) -> Box<dyn Element> {
    Text::new(
        label.into(),
        appearance.ui_font_family(),
        appearance.ui_font_size(),
    )
    .with_color(appearance.theme().active_ui_text_color().into())
    .finish()
}

fn layout_button(
    label: &str,
    mouse: &MouseStateHandle,
    action: LayoutsAction,
    primary: bool,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let variant = if primary {
        ButtonVariant::Accent
    } else {
        ButtonVariant::Secondary
    };
    let element = appearance
        .ui_builder()
        .button(variant, mouse.clone())
        .with_centered_text_label(label.to_owned())
        .build()
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish();
    SavePosition::new(element, label)
        .for_single_frame()
        .finish()
}

fn render_tab_preview(
    plan: &LayoutPlan,
    tab: usize,
    preview: &SavedLayoutPreview,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let space = preview.space;
    let scale = (PREVIEW_WIDTH / space.width).min(PREVIEW_HEIGHT / space.height);
    let theme = appearance.theme();
    let mut tiles =
        Stack::new().with_child(Rect::new().with_background(theme.surface_1()).finish());
    for pane in plan.preview_panes(tab, space) {
        let size = pane.bounds.size() * scale;
        let title = preview
            .pane_titles
            .get(&pane.session)
            .map(String::as_str)
            .unwrap_or("Terminal");
        let label = if size.x() > 68. && size.y() > 19. {
            let max_chars = ((size.x() - 12.) / 8.) as usize;
            let title: String = title.chars().take(max_chars).collect();
            let label = if pane.stack_size > 1 {
                format!("{} · {title}", pane.stack_size)
            } else {
                title
            };
            layout_text(label, appearance)
        } else {
            Empty::new().finish()
        };
        let tile = Container::new(label)
            .with_horizontal_padding(4.)
            .with_background(theme.surface_2())
            .with_border(Border::all(1.).with_border_fill(theme.outline()))
            .finish();
        tiles.add_positioned_child(
            ConstrainedBox::new(tile)
                .with_width(size.x())
                .with_height(size.y())
                .finish(),
            OffsetPositioning::offset_from_parent(
                pane.bounds.origin() * scale,
                ParentOffsetBounds::ParentByPosition,
                ParentAnchor::TopLeft,
                ChildAnchor::TopLeft,
            ),
        );
    }
    ConstrainedBox::new(tiles.finish())
        .with_width(space.width * scale)
        .with_height(space.height * scale)
        .finish()
}

impl Entity for PaneLayoutsView {
    type Event = LayoutsEvent;
}

impl View for PaneLayoutsView {
    fn ui_name() -> &'static str {
        "PaneLayoutsView"
    }

    fn on_focus(&mut self, focus: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus.is_self_focused() && self.selection.is_none() {
            ctx.focus(&self.name);
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let content = match &self.selection {
            Some(selection) => self.render_selection(selection, appearance),
            None => self.render_catalog(appearance),
        };
        let content = ClippedScrollable::vertical(
            self.scroll.clone(),
            content,
            ScrollbarWidth::Auto,
            theme.nonactive_ui_text_color().into(),
            theme.active_ui_text_color().into(),
            theme.surface_2().into(),
        )
        .with_overlayed_scrollbar()
        .finish();
        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(8.)
            .with_child(Shrinkable::new(1., content).finish());
        if let Some(notice) = &self.notice {
            column.add_child(layout_text(notice.clone(), appearance));
        }
        column.add_child(self.render_footer(appearance));
        ConstrainedBox::new(column.finish())
            .with_max_height(MODAL_HEIGHT)
            .finish()
    }
}

impl TypedActionView for PaneLayoutsView {
    type Action = LayoutsAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            LayoutsAction::Save(kind) => {
                let name = self.name.as_ref(ctx).buffer_text(ctx).trim().to_owned();
                if name.is_empty() {
                    self.set_notice("Enter a layout name first.".to_owned(), ctx);
                } else {
                    ctx.emit(LayoutsEvent::Save { name, kind: *kind });
                }
            }
            LayoutsAction::Preview(name) => ctx.emit(LayoutsEvent::Preview(name.clone())),
            LayoutsAction::Copy(name) => ctx.emit(LayoutsEvent::Copy(name.clone())),
            LayoutsAction::Import => ctx.emit(LayoutsEvent::Import),
            LayoutsAction::PreviewDisplayProfile => ctx.emit(LayoutsEvent::PreviewDisplayProfile),
            LayoutsAction::BindDisplayProfile => ctx.emit(LayoutsEvent::BindDisplayProfile),
            LayoutsAction::SelectAlternative(alternative) => {
                if let Some(selection) = &mut self.selection {
                    selection.alternative = *alternative;
                    self.scroll = Default::default();
                    self.notice = None;
                    ctx.notify();
                }
            }
            LayoutsAction::Back => {
                self.selection = None;
                self.notice = None;
                self.scroll = Default::default();
                ctx.focus(&self.name);
                ctx.notify();
            }
            LayoutsAction::Apply => ctx.emit(LayoutsEvent::Apply),
        }
    }
}

impl Workspace {
    pub(super) fn open_pane_layouts(&mut self, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::EnhancedPaneManagement.is_enabled() {
            return;
        }

        let modal = self.pane_layouts_modal.get_or_insert_with(|| {
            let body = ctx.add_typed_action_view(PaneLayoutsView::new);
            ctx.subscribe_to_view(&body, Self::handle_pane_layouts_event);
            ctx.observe(&DisplayCount::handle(ctx), |workspace, _, ctx| {
                workspace.refresh_pane_layout_display(ctx);
            });
            let modal = ctx.add_typed_action_view(|ctx| {
                Modal::new(Some("Manage Pane Layouts".to_owned()), body, ctx)
                    .with_modal_style(UiComponentStyles {
                        width: Some(MODAL_WIDTH),
                        ..Default::default()
                    })
                    .with_max_height_percentage(0.9)
            });
            ctx.subscribe_to_view(&modal, |workspace, _, _, ctx| {
                workspace.close_pane_layouts(ctx);
            });
            ModalViewState::new(modal)
        });
        let display = current_display_key(ctx);
        modal
            .view
            .as_ref(ctx)
            .body()
            .clone()
            .update(ctx, |body, ctx| {
                body.reset(LayoutCatalog::load(), display.as_deref(), ctx);
            });
        modal.open();
        ctx.focus(&modal.view);
        ctx.notify();
    }

    fn refresh_pane_layout_display(&self, ctx: &mut ViewContext<Self>) {
        if let Some(modal) = self
            .pane_layouts_modal
            .as_ref()
            .filter(|modal| modal.is_open())
        {
            let display = current_display_key(ctx);
            modal
                .view
                .as_ref(ctx)
                .body()
                .clone()
                .update(ctx, |body, ctx| {
                    body.refresh_catalog(LayoutCatalog::load(), display.as_deref(), ctx);
                });
        }
    }

    fn close_pane_layouts(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some(modal) = &mut self.pane_layouts_modal {
            modal.close();
            self.focus_active_tab(ctx);
            ctx.notify();
        }
    }

    fn handle_pane_layouts_event(
        &mut self,
        body: ViewHandle<PaneLayoutsView>,
        event: &LayoutsEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            LayoutsEvent::Save { name, kind } => {
                let result = self
                    .capture_saved_pane_layout(name.clone(), *kind == LayoutKind::Snapshot, ctx)
                    .and_then(|doc| {
                        LayoutCatalog::save_document(doc).map_err(|err| err.to_string())
                    });
                self.finish_pane_layout_save(body, result, "Layout saved.", ctx);
            }
            LayoutsEvent::Preview(name) => {
                let document = LayoutCatalog::load().and_then(|catalog| {
                    catalog
                        .get(name)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("Layout unavailable."))
                });
                self.select_pane_layout(body, document, ctx);
            }
            LayoutsEvent::Copy(name) => {
                let json = LayoutCatalog::load().and_then(|catalog| {
                    catalog
                        .get(name)
                        .ok_or_else(|| anyhow::anyhow!("Layout unavailable."))?
                        .portable_json()
                });
                let notice = match json {
                    Ok(json) => {
                        ctx.clipboard().write(ClipboardContent::plain_text(json));
                        "Template JSON copied.".to_owned()
                    }
                    Err(error) => error.to_string(),
                };
                body.update(ctx, |view, ctx| view.set_notice(notice, ctx));
            }
            LayoutsEvent::Import => {
                let clipboard = ctx.clipboard().read().plain_text;
                let result = LayoutDocument::from_json(&clipboard)
                    .and_then(LayoutCatalog::save_document)
                    .map_err(|error| error.to_string());
                self.finish_pane_layout_save(body, result, "Layout imported.", ctx);
            }
            LayoutsEvent::PreviewDisplayProfile => {
                let document = LayoutCatalog::load().and_then(|catalog| {
                    current_display_key(ctx)
                        .as_deref()
                        .and_then(|key| catalog.display_profile(key))
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("No saved profile for this display setup."))
                });
                self.select_pane_layout(body, document, ctx);
            }
            LayoutsEvent::BindDisplayProfile => {
                let selection = body.as_ref(ctx).selection.as_ref();
                let result = current_display_key(ctx)
                    .zip(selection.map(|selection| selection.document.name.as_str()))
                    .ok_or_else(|| "Display setup unavailable.".to_owned())
                    .and_then(|(key, name)| {
                        LayoutCatalog::set_display_profile(key, name)
                            .map_err(|error| error.to_string())
                    });
                self.finish_pane_layout_save(body, result, "Display profile saved.", ctx);
            }
            LayoutsEvent::Apply => {
                let plan = body.as_ref(ctx).selection.as_ref().and_then(|selection| {
                    Some((
                        selection.document.clone(),
                        selection.preview.as_ref().ok()?.clone(),
                        selection.alternative,
                    ))
                });
                if let Some((document, preview, alternative)) = plan {
                    match self.apply_saved_pane_layout(&document, &preview, alternative, ctx) {
                        Ok(()) => self.close_pane_layouts(ctx),
                        Err(message) => body.update(ctx, |view, ctx| view.set_notice(message, ctx)),
                    }
                }
            }
            LayoutsEvent::Close => self.close_pane_layouts(ctx),
        }
    }

    fn select_pane_layout(
        &self,
        body: ViewHandle<PaneLayoutsView>,
        document: anyhow::Result<LayoutDocument>,
        ctx: &mut ViewContext<Self>,
    ) {
        match document {
            Ok(document) => {
                let preview = self.preview_saved_pane_layout(&document, ctx);
                body.update(ctx, |view, ctx| view.select(document, preview, ctx));
            }
            Err(error) => body.update(ctx, |view, ctx| view.set_notice(error.to_string(), ctx)),
        }
    }

    fn finish_pane_layout_save(
        &self,
        body: ViewHandle<PaneLayoutsView>,
        result: Result<(), String>,
        success: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        let display = current_display_key(ctx);
        body.update(ctx, |view, ctx| match result {
            Ok(()) => {
                view.set_notice(success.to_owned(), ctx);
                view.refresh_catalog(LayoutCatalog::load(), display.as_deref(), ctx);
            }
            Err(message) => view.set_notice(message, ctx),
        });
    }
}
