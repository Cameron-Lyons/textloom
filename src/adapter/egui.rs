//! An egui widget that renders the document's rich spans directly.
//!
//! Use a stable [`RichTextEditor::id_salt`] when the widget can move in the UI.
//! The host's egui integration provides the clipboard, IME, and platform accessibility.
//! For a true bold face, register one in `FontDefinitions` and select it with
//! [`RichTextEditor::bold_font_family`]. Default fonts use egui's strong text color.

use std::{collections::HashMap, sync::Arc};

use egui::{
    Color32, Event, FontFamily, FontId, Galley, Id, ImeEvent, Key, Modifiers, Pos2, Rect, Response,
    Sense, Stroke, TextFormat, TextStyle, Ui, Vec2, Widget,
    text::{ByteIndex, CCursor, CharIndex, LayoutJob, LayoutSection},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Composition, Document, Editor, Error, InlineStyle, Movement, Paragraph, ParagraphKind,
    Position, Selection, StylePatch,
};

mod accessibility;
pub use accessibility::AccessibilityError;

/// A reusable, multiline rich-text editor with pointer and keyboard selection.
///
/// Place it in `ScrollArea::vertical()` for a scrolling document. The core owns
/// document and history state; egui owns focus and cached paragraph layouts. Clipboard
/// interchange is plain text, so copying to another application remains portable.
/// Full text in egui output metadata is generated when accessibility, a screen
/// reader, or the debug widget inspector needs it. Other output events retain
/// their widget type and selection without copying the whole document.
pub struct RichTextEditor<'a> {
    editor: &'a mut Editor,
    id: Option<Id>,
    salt: Option<egui::IdSalt>,
    desired_width: Option<f32>,
    min_rows: usize,
    font: Option<FontId>,
    bold_family: Option<FontFamily>,
    hint: String,
    read_only: bool,
}

/// The response plus any rejected editor commands.
///
/// `ui.add(widget)` returns the standard response. Use `widget.show(ui)` when
/// the application also wants to inspect errors. Invalid external IME ranges are
/// reported here rather than panicking or altering the document.
pub struct RichTextEditorOutput {
    /// Focus, hover, interaction, and document-change state for the widget.
    pub response: Response,
    /// Editor commands rejected by core validation.
    pub errors: Vec<Error>,
    /// Errors from publishing or applying native accessibility text actions.
    pub accessibility_errors: Vec<AccessibilityError>,
}

impl<'a> RichTextEditor<'a> {
    /// Borrow an editor for one frame, using the body font and four minimum rows.
    pub fn new(editor: &'a mut Editor) -> Self {
        Self {
            editor,
            id: None,
            salt: None,
            desired_width: None,
            min_rows: 4,
            font: None,
            bold_family: None,
            hint: String::new(),
            read_only: false,
        }
    }

    /// Set the persistent widget ID, overriding any ID salt.
    pub fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    /// Derive a persistent ID from the containing UI and this salt.
    pub fn id_salt(mut self, salt: impl egui::AsIdSalt) -> Self {
        self.salt = Some(egui::IdSalt::new(salt));
        self
    }

    /// Set the desired width in points, clamped to the UI width and at least 32.
    pub fn desired_width(mut self, width: f32) -> Self {
        self.desired_width = Some(width.max(32.0));
        self
    }

    /// Reserve at least this many body-font rows, with a minimum of one.
    pub fn min_rows(mut self, rows: usize) -> Self {
        self.min_rows = rows.max(1);
        self
    }

    /// Set the base font; headings scale its size and code uses monospace.
    pub fn font(mut self, font: FontId) -> Self {
        self.font = Some(font);
        self
    }

    /// Select a host-registered font family for bold spans.
    pub fn bold_font_family(mut self, family: FontFamily) -> Self {
        self.bold_family = Some(family);
        self
    }

    /// Display placeholder text when the document and composition are empty.
    pub fn hint_text(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    /// Permit focus, selection, navigation, and copying while preventing edits.
    ///
    /// A read-only editor remains enabled and available to assistive technology.
    /// Existing composition is canceled when the widget becomes read-only.
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Process focused input, render the document, and return interaction errors.
    pub fn show(self, ui: &mut Ui) -> RichTextEditorOutput {
        let id = self.id.unwrap_or_else(|| {
            self.salt
                .map_or_else(|| ui.next_auto_id(), |salt| ui.make_persistent_id(salt))
        });
        let mut cache = ui
            .ctx()
            .data_mut(|data| data.remove_temp::<Cache>(id))
            .unwrap_or_default();
        let font = self
            .font
            .clone()
            .unwrap_or_else(|| TextStyle::Body.resolve(ui.style()));
        let width = self
            .desired_width
            .unwrap_or(ui.available_width())
            .min(ui.available_width())
            .max(32.0);
        let appearance = Appearance {
            font: font.clone(),
            bold_family: self.bold_family.clone(),
            color: ui.visuals().text_color(),
            strong_color: ui.visuals().strong_text_color(),
            code_background: ui.visuals().code_bg_color,
            width: width - PADDING * 2.0,
        };
        let old_revision = self.editor.document().revision();
        let old_selection = self.editor.selection();
        let old_composition = self.editor.composition().cloned();
        let publish_text = ui
            .ctx()
            .accesskit_node_builder(id, |node| {
                node.set_role(egui::accesskit::Role::MultilineTextInput)
            })
            .is_some()
            || ui.memory(|memory| memory.options.screen_reader);
        #[cfg(debug_assertions)]
        let publish_text = publish_text || ui.style().debug.show_interactive_widgets;
        let mut errors = Vec::new();
        let mut accessibility_errors = Vec::new();
        let mut layouts = cache.layout(ui, self.editor, &appearance);
        cache.validate_navigation(self.editor.selection());
        let old_text = publish_text.then(|| cache.plain_text(self.editor.document()));
        let mut interrupted =
            (self.read_only || !ui.is_enabled()) && self.editor.cancel_composition();
        let initial_preview = match cache.layout_preview(ui, self.editor, &appearance) {
            Ok(layouts) => layouts,
            Err(error) => {
                errors.push(error);
                None
            }
        };
        let height = initial_preview
            .as_ref()
            .unwrap_or(&layouts)
            .last()
            .map_or(font.size, |p| p.rect.bottom());
        let (_, rect) = ui.allocate_space(Vec2::new(
            width,
            height.max(font.size * self.min_rows as f32) + PADDING * 2.0,
        ));
        let mut response = ui.interact(rect, id, Sense::click_and_drag());
        let origin = rect.min + Vec2::splat(PADDING);

        if ui.is_enabled() {
            ui.input_mut(|input| {
                input.consume_accesskit_action_requests(id, |request| {
                    use egui::accesskit::{Action, ActionData};
                    if self.read_only
                        && matches!(
                            request.action,
                            Action::SetValue | Action::ReplaceSelectedText
                        )
                    {
                        return true;
                    }
                    if matches!(
                        request.action,
                        Action::SetTextSelection | Action::SetValue | Action::ReplaceSelectedText
                    ) && !cache
                        .accessibility
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.matches_document(self.editor))
                    {
                        accessibility_errors.push(AccessibilityError::StaleDocument);
                        return true;
                    }
                    match (&request.action, &request.data) {
                        (
                            Action::SetTextSelection,
                            Some(ActionData::SetTextSelection(selection)),
                        ) => {
                            if let (Some(anchor), Some(focus)) = (
                                cache
                                    .accessibility
                                    .as_ref()
                                    .and_then(|snapshot| snapshot.decode(selection.anchor)),
                                cache
                                    .accessibility
                                    .as_ref()
                                    .and_then(|snapshot| snapshot.decode(selection.focus)),
                            ) {
                                interrupted |= self.editor.cancel_composition();
                                record(
                                    self.editor.set_selection(Selection::new(anchor, focus)),
                                    &mut errors,
                                );
                            } else {
                                accessibility_errors.push(AccessibilityError::InvalidTextPosition);
                            }
                            true
                        }
                        (Action::SetValue, Some(ActionData::Value(value))) => {
                            interrupted |= self.editor.cancel_composition();
                            self.editor.select_all();
                            self.editor.break_history_group();
                            record(self.editor.insert_text(value), &mut errors);
                            self.editor.break_history_group();
                            true
                        }
                        (Action::ReplaceSelectedText, Some(ActionData::Value(value))) => {
                            interrupted |= self.editor.cancel_composition();
                            self.editor.break_history_group();
                            record(self.editor.insert_text(value), &mut errors);
                            self.editor.break_history_group();
                            true
                        }
                        _ => false,
                    }
                })
            });
            if old_revision != self.editor.document().revision() {
                layouts = cache.layout(ui, self.editor, &appearance);
            }
            if response.clicked() || response.drag_started() {
                cache.navigation = None;
                response.request_focus();
                if let Some(pointer) = response.interact_pointer_pos() {
                    interrupted |= self.editor.cancel_composition();
                    let position = hit_test(self.editor, &layouts, pointer - origin);
                    let shift = ui.input(|input| input.modifiers.shift);
                    let selection = if response.triple_clicked() {
                        let end = self
                            .editor
                            .document()
                            .paragraph(position.paragraph)
                            .map_or(0, |p| p.text().len());
                        Selection::new(
                            Position::new(position.paragraph, 0),
                            Position::new(position.paragraph, end),
                        )
                    } else if response.double_clicked() {
                        word_selection(self.editor, position)
                    } else if shift {
                        Selection::new(self.editor.selection().anchor, position)
                    } else {
                        Selection::caret(position)
                    };
                    record(self.editor.set_selection(selection), &mut errors);
                }
            }
            if response.dragged()
                && let Some(pointer) = response.interact_pointer_pos()
            {
                cache.navigation = None;
                interrupted |= self.editor.cancel_composition();
                let position = hit_test(self.editor, &layouts, pointer - origin);
                record(
                    self.editor
                        .set_selection(Selection::new(self.editor.selection().anchor, position)),
                    &mut errors,
                );
            }

            if response.has_focus() {
                cache.validate_navigation(self.editor.selection());
                ui.memory_mut(|memory| {
                    memory.set_focus_lock_filter(
                        id,
                        egui::EventFilter {
                            tab: !self.read_only && cache.selection_contains_list(self.editor),
                            horizontal_arrows: true,
                            vertical_arrows: true,
                            escape: true,
                        },
                    )
                });
                let events = ui.input(|input| input.events.clone());
                let mut consumed = Vec::new();
                for (index, event) in events.iter().enumerate() {
                    if !ui.memory(|memory| memory.has_focus(id)) {
                        break;
                    }
                    let revision = self.editor.document().revision();
                    if handle_event(
                        self.editor,
                        ui,
                        event,
                        &layouts,
                        &mut cache.navigation,
                        &mut errors,
                        &mut interrupted,
                        self.read_only,
                    ) {
                        consumed.push(index);
                    }
                    if revision != self.editor.document().revision() {
                        layouts = cache.layout(ui, self.editor, &appearance);
                    }
                    cache.validate_navigation(self.editor.selection());
                }
                // Leave pointer events and unrelated shortcuts available to the host.
                ui.input_mut(|input| {
                    let mut consumed = consumed.into_iter().peekable();
                    let mut index = 0;
                    input.events.retain(|_| {
                        let retain = consumed.peek() != Some(&index);
                        if !retain {
                            consumed.next();
                        }
                        index += 1;
                        retain
                    });
                });
            } else {
                interrupted |= self.editor.cancel_composition();
            }
        }

        if interrupted {
            ui.memory_mut(|memory| memory.interrupt_ime());
        }
        if old_revision != self.editor.document().revision() {
            response.mark_changed();
            layouts = cache.layout(ui, self.editor, &appearance);
            ui.ctx().request_repaint();
        }
        if old_selection != self.editor.selection() {
            ui.ctx().request_repaint();
        }
        if old_composition.as_ref() != self.editor.composition() {
            ui.ctx().request_repaint();
        }
        let preview_layouts = match cache.layout_preview(ui, self.editor, &appearance) {
            Ok(layouts) => layouts,
            Err(error) => {
                errors.push(error);
                None
            }
        };
        let display_layouts = preview_layouts.as_ref().unwrap_or(&layouts);
        let preview = cache.preview.as_ref();
        let display_document = preview.map_or(self.editor.document(), |preview| &preview.document);
        let display_selection =
            preview.map_or(Some(self.editor.selection()), |preview| preview.selection);

        let visuals = ui.style().interact(&response);
        let border = if response.has_focus() {
            ui.visuals().selection.stroke
        } else {
            visuals.bg_stroke
        };
        ui.painter().rect(
            rect,
            visuals.corner_radius,
            ui.visuals().text_edit_bg_color(),
            border,
            egui::StrokeKind::Inside,
        );
        let painter = ui
            .painter()
            .with_clip_rect(ui.clip_rect().intersect(rect.shrink(1.0)));
        if ui.is_rect_visible(rect) {
            if let Some(selection) = display_selection {
                paint_document_selection(
                    display_document,
                    selection,
                    display_layouts,
                    origin,
                    &painter,
                    ui.visuals().selection.bg_fill,
                );
            }
            let top = painter.clip_rect().top() - origin.y;
            let bottom = painter.clip_rect().bottom() - origin.y;
            let first_visible =
                display_layouts.partition_point(|paragraph| paragraph.rect.bottom() < top);
            let last_visible = display_layouts
                .partition_point(|paragraph| paragraph.rect.top() <= bottom)
                .max(first_visible);
            for paragraph in &display_layouts[first_visible..last_visible] {
                let text_origin = origin + paragraph.rect.min.to_vec2();
                if !painter
                    .clip_rect()
                    .intersects(paragraph.rect.translate(origin.to_vec2()))
                {
                    continue;
                }
                if let Some(marker) = &paragraph.marker {
                    painter.galley(
                        origin + paragraph.marker_position.to_vec2(),
                        marker.clone(),
                        appearance.color,
                    );
                }
                painter.galley(text_origin, paragraph.galley.clone(), appearance.color);
            }
            if self.editor.document().paragraphs().len() == 1
                && self.editor.document().paragraphs()[0].text().is_empty()
                && self.editor.composition().is_none()
                && !self.hint.is_empty()
            {
                painter.text(
                    origin,
                    egui::Align2::LEFT_TOP,
                    &self.hint,
                    font.clone(),
                    ui.visuals().weak_text_color(),
                );
            }

            if response.has_focus() {
                let caret_position =
                    preview.map_or(self.editor.selection().focus, |preview| preview.caret);
                let prefer_next_row = preview.is_some()
                    || cache
                        .navigation
                        .as_ref()
                        .is_none_or(|navigation| navigation.prefer_next_row);
                let caret = document_caret_rect(
                    display_document,
                    display_layouts,
                    origin,
                    caret_position,
                    prefer_next_row,
                );
                let show_caret = self
                    .editor
                    .composition()
                    .map_or(self.editor.selection().is_caret(), |composition| {
                        composition.selection.is_some()
                    });
                if show_caret {
                    painter.line_segment(
                        [caret.left_top(), caret.left_bottom()],
                        ui.visuals().text_cursor.stroke,
                    );
                }
                if old_selection != self.editor.selection() || response.changed() {
                    ui.scroll_to_rect(caret.expand(4.0), None);
                }
                let transform = ui
                    .ctx()
                    .layer_transform_to_global(ui.layer_id())
                    .unwrap_or_default();
                if !self.read_only && ui.is_enabled() {
                    ui.output_mut(|output| {
                        output.ime = Some(egui::output::IMEOutput {
                            purpose: egui::IMEPurpose::Normal,
                            rect: transform * rect,
                            cursor_rect: transform * caret,
                            should_interrupt_composition: interrupted,
                        })
                    });
                }
            }
        }

        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        let range = self.editor.selection().range();
        let char_range = CharIndex(cache.char_offset(self.editor.document(), range.start))
            ..CharIndex(cache.char_offset(self.editor.document(), range.end));
        let text = publish_text.then(|| cache.plain_text(self.editor.document()));
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::new(egui::WidgetType::TextEdit);
            info.enabled = ui.is_enabled();
            info.hint_text = Some(self.hint.clone());
            if let Some(text) = &text {
                info.current_text_value = Some(text.to_string());
                if old_text.as_ref() != Some(text) {
                    info.prev_text_value = old_text.as_ref().map(ToString::to_string);
                }
            }
            info.text_selection = Some(char_range.clone());
            info
        });
        if old_selection != self.editor.selection() {
            let mut info = egui::WidgetInfo::new(egui::WidgetType::TextEdit);
            info.enabled = ui.is_enabled();
            info.text_selection = Some(char_range);
            info.current_text_value = text.as_ref().map(ToString::to_string);
            response.output_event(egui::output::OutputEvent::TextSelectionChanged(info));
        }
        if ui
            .ctx()
            .accesskit_node_builder(id, |node| {
                node.set_role(egui::accesskit::Role::MultilineTextInput)
            })
            .is_some()
        {
            if !cache
                .accessibility
                .as_ref()
                .is_some_and(|snapshot| snapshot.is_current(self.editor, &layouts))
            {
                match accessibility::build(self.editor, &layouts, id) {
                    Ok(snapshot) => cache.accessibility = Some(Arc::new(snapshot)),
                    Err(error) => {
                        cache.accessibility = None;
                        accessibility_errors.push(error);
                    }
                }
            }
            if let Some(snapshot) = &cache.accessibility {
                snapshot.publish(ui, id, self.editor, &layouts, origin, self.read_only);
            }
        } else {
            cache.accessibility = None;
        }
        ui.ctx().data_mut(|data| data.insert_temp(id, cache));
        RichTextEditorOutput {
            response,
            errors,
            accessibility_errors,
        }
    }
}

impl Widget for RichTextEditor<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui).response
    }
}

const PADDING: f32 = 8.0;
const PARAGRAPH_GAP: f32 = 4.0;

#[derive(Clone, PartialEq)]
struct Appearance {
    font: FontId,
    bold_family: Option<FontFamily>,
    color: Color32,
    strong_color: Color32,
    code_background: Color32,
    width: f32,
}

#[derive(Clone)]
struct ParagraphCache {
    paragraph: Arc<Paragraph>,
    galley: Arc<Galley>,
    marker: Option<Arc<Galley>>,
    chars: usize,
    indent: f32,
}

#[derive(Clone, Default)]
struct Cache {
    appearance: Option<Appearance>,
    /// A tiny egui-cached galley changes identity when egui resets its font
    /// atlas/cache. Retaining it detects both font-definition/text-option
    /// changes and atlas recreation without rehashing every paragraph job.
    font_probe: Option<Arc<Galley>>,
    document_identity: Option<Arc<()>>,
    paragraphs: Vec<ParagraphCache>,
    layouts: Arc<[ParagraphLayout]>,
    char_offsets: Vec<usize>,
    list_paragraphs: Vec<usize>,
    plain_text: Option<Arc<str>>,
    preview: Option<Box<CompositionPreview>>,
    accessibility: Option<Arc<accessibility::Snapshot>>,
    navigation: Option<VerticalNavigation>,
}

#[derive(Clone)]
struct VerticalNavigation {
    selection: Selection,
    preferred_x: f32,
    prefer_next_row: bool,
}

#[derive(Clone)]
struct CompositionPreview {
    composition: Composition,
    style: InlineStyle,
    document: Document,
    render_cache: Cache,
    caret: Position,
    selection: Option<Selection>,
}

#[derive(Clone)]
struct ParagraphLayout {
    galley: Arc<Galley>,
    rect: Rect,
    marker: Option<Arc<Galley>>,
    marker_position: Pos2,
}

impl Cache {
    fn validate_navigation(&mut self, selection: Selection) {
        if self
            .navigation
            .as_ref()
            .is_some_and(|navigation| navigation.selection != selection)
        {
            self.navigation = None;
        }
    }

    fn layout(
        &mut self,
        ui: &Ui,
        editor: &Editor,
        appearance: &Appearance,
    ) -> Arc<[ParagraphLayout]> {
        self.layout_document(ui, editor.document(), appearance)
    }

    fn layout_document(
        &mut self,
        ui: &Ui,
        document: &Document,
        appearance: &Appearance,
    ) -> Arc<[ParagraphLayout]> {
        let appearance_changed = self.appearance.as_ref() != Some(appearance);
        let document_identity = document.content_identity();
        let document_changed = self
            .document_identity
            .as_ref()
            .is_none_or(|cached| !Arc::ptr_eq(cached, &document_identity));
        ui.fonts_mut(|fonts| {
            let probe = fonts.layout_job(LayoutJob::simple_singleline(
                " ".to_owned(),
                appearance.font.clone(),
                Color32::WHITE,
            ));
            let fonts_changed = self
                .font_probe
                .as_ref()
                .is_none_or(|previous| !Arc::ptr_eq(previous, &probe));
            self.font_probe = Some(probe);
            if !appearance_changed && !document_changed && !fonts_changed {
                return;
            }
            self.navigation = None;
            let mut create = |paragraph: &Arc<Paragraph>| {
                let (indent, marker) = match paragraph.kind() {
                    ParagraphKind::Bullet { indent } => {
                        (24.0 + f32::from(indent) * 20.0, Some("•".to_owned()))
                    }
                    ParagraphKind::Ordered { indent, start } => {
                        (32.0 + f32::from(indent) * 20.0, Some(format!("{start}.")))
                    }
                    _ => (0.0, None),
                };
                let marker = marker.map(|text| {
                    fonts.layout_job(LayoutJob::simple_singleline(
                        text,
                        appearance.font.clone(),
                        appearance.color,
                    ))
                });
                let marker_width = marker.as_ref().map_or(0.0, |galley| galley.size().x);
                let indent = if marker.is_some() {
                    indent.max(marker_width + 8.0)
                } else {
                    indent
                };
                let indent = indent.min((appearance.width - appearance.font.size * 2.0).max(0.0));
                let job =
                    paragraph_job(paragraph, appearance, (appearance.width - indent).max(8.0));
                let chars = paragraph.scalar_count();
                let mut galley = fonts.layout_job(job);
                if galley.end().index.0 != chars {
                    // egui's shaper can duplicate scalar slots for descending
                    // clusters. Retry independent scalars to retain valid text
                    // coordinates and every original section's appearance.
                    galley = fonts.layout_job(scalar_layout_job(&galley.job));
                }
                ParagraphCache {
                    paragraph: paragraph.clone(),
                    galley,
                    marker,
                    chars,
                    indent,
                }
            };
            if self.paragraphs.len() == document.paragraphs().len() {
                // Typing, formatting, and undo usually preserve paragraph count.
                // Update only changed entries without allocating a lookup table.
                for (cache, paragraph) in self.paragraphs.iter_mut().zip(document.paragraphs()) {
                    if appearance_changed
                        || fonts_changed
                        || !Arc::ptr_eq(&cache.paragraph, paragraph)
                    {
                        *cache = create(paragraph);
                    }
                }
            } else {
                let mut previous: HashMap<_, _> = std::mem::take(&mut self.paragraphs)
                    .into_iter()
                    .map(|cache| (Arc::as_ptr(&cache.paragraph), cache))
                    .collect();
                self.paragraphs = document
                    .paragraphs()
                    .iter()
                    .map(|paragraph| {
                        if !appearance_changed
                            && !fonts_changed
                            && let Some(cache) = previous.remove(&Arc::as_ptr(paragraph))
                        {
                            return cache;
                        }
                        create(paragraph)
                    })
                    .collect();
            }
            if document_changed {
                self.plain_text = None;
                self.preview = None;
            }
            self.appearance = Some(appearance.clone());
            self.document_identity = Some(document_identity);
            let mut y = 0.0;
            let mut chars = 0;
            self.char_offsets.clear();
            self.list_paragraphs.clear();
            self.layouts = self
                .paragraphs
                .iter()
                .enumerate()
                .map(|(index, cache)| {
                    if matches!(
                        cache.paragraph.kind(),
                        ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. }
                    ) {
                        self.list_paragraphs.push(index);
                    }
                    self.char_offsets.push(chars);
                    chars += cache.chars + 1;
                    let rect = Rect::from_min_size(Pos2::new(cache.indent, y), cache.galley.size());
                    let marker_position = Pos2::new(
                        cache.indent
                            - cache.marker.as_ref().map_or(0.0, |marker| marker.size().x)
                            - 8.0,
                        y,
                    );
                    y += cache.galley.size().y + PARAGRAPH_GAP;
                    ParagraphLayout {
                        galley: cache.galley.clone(),
                        rect,
                        marker: cache.marker.clone(),
                        marker_position,
                    }
                })
                .collect();
        });
        self.layouts.clone()
    }

    fn plain_text(&mut self, document: &Document) -> Arc<str> {
        self.plain_text
            .get_or_insert_with(|| Arc::from(document.plain_text()))
            .clone()
    }

    fn char_offset(&self, document: &Document, position: Position) -> usize {
        self.char_offsets
            .get(position.paragraph)
            .copied()
            .unwrap_or(0)
            + document
                .paragraph(position.paragraph)
                .map_or(0, |paragraph| {
                    paragraph
                        .scalar_index(position.byte)
                        .unwrap_or(paragraph.scalar_count())
                })
    }

    fn selection_contains_list(&self, editor: &Editor) -> bool {
        let range = selected_paragraphs(editor);
        let index = self
            .list_paragraphs
            .partition_point(|paragraph| *paragraph < range.start);
        self.list_paragraphs
            .get(index)
            .is_some_and(|paragraph| *paragraph < range.end)
    }

    fn layout_preview(
        &mut self,
        ui: &Ui,
        editor: &Editor,
        appearance: &Appearance,
    ) -> Result<Option<Arc<[ParagraphLayout]>>, Error> {
        let Some(composition) = editor.composition() else {
            self.preview = None;
            return Ok(None);
        };
        let style = editor.typing_style();
        if !self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.composition == *composition && preview.style == style)
        {
            let mut document = editor.document().clone();
            let range = composition.replacement.range();
            let mut preedit_style = style;
            preedit_style.underline = true;
            document.replace(range.clone(), &composition.text, preedit_style)?;
            let caret_byte = composition
                .selection
                .as_ref()
                .map_or(composition.text.len(), |range| range.end);
            let caret =
                position_after_prefix(&document, range.start, &composition.text[..caret_byte]);
            let selection = composition.selection.as_ref().map(|selection| {
                Selection::new(
                    position_after_prefix(
                        &document,
                        range.start,
                        &composition.text[..selection.start],
                    ),
                    caret,
                )
            });
            self.preview = Some(Box::new(CompositionPreview {
                composition: composition.clone(),
                style,
                document,
                render_cache: Cache::default(),
                caret,
                selection,
            }));
        }
        let preview = self.preview.as_mut().expect("preview initialized above");
        Ok(Some(preview.render_cache.layout_document(
            ui,
            &preview.document,
            appearance,
        )))
    }
}

fn position_after_prefix(document: &Document, start: Position, prefix: &str) -> Position {
    let normalized = prefix.replace("\r\n", "\n").replace('\r', "\n");
    let paragraphs = normalized.bytes().filter(|byte| *byte == b'\n').count();
    let byte = if paragraphs == 0 {
        start.byte + normalized.len()
    } else {
        normalized.rsplit('\n').next().map_or(0, str::len)
    };
    let paragraph = start.paragraph + paragraphs;
    let Some(source) = document.paragraph(paragraph) else {
        return document.end();
    };
    let byte = source.boundary_at_or_after(byte);
    Position::new(paragraph, byte)
}

fn paragraph_job(paragraph: &Paragraph, appearance: &Appearance, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    job.keep_trailing_whitespace = true;
    let size_scale = match paragraph.kind() {
        ParagraphKind::Heading { level } => match level {
            1 => 1.8,
            2 => 1.5,
            3 => 1.25,
            _ => 1.1,
        },
        _ => 1.0,
    };
    for span in paragraph.spans() {
        job.append(
            &paragraph.text()[span.range.clone()],
            0.0,
            text_format(span.style, appearance, size_scale),
        );
    }
    if job.sections.is_empty() {
        // An empty paragraph still has a caret row with the right font height.
        job.append(
            "",
            0.0,
            text_format(InlineStyle::default(), appearance, size_scale),
        );
    }
    job
}

fn scalar_layout_job(source: &LayoutJob) -> LayoutJob {
    let mut job = source.clone();
    job.sections.clear();
    for section in &source.sections {
        let start = section.byte_range.start.0;
        let end = section.byte_range.end.0;
        for (offset, scalar) in source.text[start..end].char_indices() {
            job.sections.push(LayoutSection {
                leading_space: if offset == 0 {
                    section.leading_space
                } else {
                    0.0
                },
                byte_range: ByteIndex(start + offset)
                    ..ByteIndex(start + offset + scalar.len_utf8()),
                format: section.format.clone(),
            });
        }
    }
    job
}

fn text_format(style: InlineStyle, appearance: &Appearance, size_scale: f32) -> TextFormat {
    let mut font = appearance.font.clone();
    font.size *= size_scale;
    if style.code {
        font.family = FontFamily::Monospace;
    } else if style.bold
        && let Some(family) = &appearance.bold_family
    {
        font.family = family.clone();
    }
    let color = style.foreground.map_or_else(
        || {
            if style.bold {
                appearance.strong_color
            } else {
                appearance.color
            }
        },
        |color| Color32::from_rgba_unmultiplied(color.0[0], color.0[1], color.0[2], color.0[3]),
    );
    TextFormat {
        font_id: font,
        color,
        italics: style.italic,
        underline: if style.underline {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        strikethrough: if style.strikethrough {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        background: if style.code {
            appearance.code_background
        } else {
            Color32::TRANSPARENT
        },
        ..Default::default()
    }
}

fn record(result: Result<(), Error>, errors: &mut Vec<Error>) {
    if let Err(error) = result {
        errors.push(error);
    }
}

fn hit_test(editor: &Editor, layouts: &[ParagraphLayout], point: Vec2) -> Position {
    let paragraph = layouts
        .partition_point(|layout| point.y > layout.rect.bottom() + PARAGRAPH_GAP / 2.0)
        .min(layouts.len().saturating_sub(1));
    let Some(layout) = layouts.get(paragraph) else {
        return Position::default();
    };
    let Some(source) = editor.document().paragraph(paragraph) else {
        return Position::default();
    };
    let cursor = layout
        .galley
        .cursor_from_pos(point - layout.rect.min.to_vec2());
    let byte = source
        .byte_from_scalar(cursor.index.0)
        .unwrap_or(source.text().len());
    let before = source.boundary_at_or_before(byte);
    let after = source.boundary_at_or_after(byte);
    Position::new(
        paragraph,
        if byte - before <= after - byte {
            before
        } else {
            after
        },
    )
}

fn byte_from_char(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(byte, _)| byte)
}

fn snap_grapheme(text: &str, byte: usize) -> usize {
    let mut previous = 0;
    for (next, _) in text.grapheme_indices(true) {
        if next >= byte {
            return if byte - previous <= next - byte {
                previous
            } else {
                next
            };
        }
        previous = next;
    }
    if byte - previous <= text.len() - byte {
        previous
    } else {
        text.len()
    }
}

fn word_selection(editor: &Editor, position: Position) -> Selection {
    let Some(text) = editor
        .document()
        .paragraph(position.paragraph)
        .map(Paragraph::text)
    else {
        return Selection::caret(position);
    };
    // Hit testing snaps the right half of the final grapheme to paragraph end.
    // Double clicking there should still select the trailing word or symbol.
    let byte = if position.byte == text.len() {
        text.grapheme_indices(true)
            .next_back()
            .map_or(0, |(start, _)| start)
    } else {
        position.byte
    };
    for (start, word) in text.split_word_bound_indices() {
        if start <= byte && byte < start + word.len() {
            return Selection::new(
                Position::new(position.paragraph, snap_grapheme(text, start)),
                Position::new(position.paragraph, snap_grapheme(text, start + word.len())),
            );
        }
    }
    Selection::caret(position)
}

fn document_caret_rect(
    document: &Document,
    layouts: &[ParagraphLayout],
    origin: Pos2,
    position: Position,
    prefer_next_row: bool,
) -> Rect {
    let Some(layout) = layouts.get(position.paragraph) else {
        return Rect::from_min_size(origin, Vec2::new(0.0, 16.0));
    };
    let index = document
        .paragraph(position.paragraph)
        .map_or(0, |paragraph| {
            paragraph
                .scalar_index(position.byte)
                .unwrap_or(paragraph.scalar_count())
        });
    layout
        .galley
        .pos_from_cursor(CCursor {
            index: CharIndex(index),
            prefer_next_row,
        })
        .translate(origin.to_vec2() + layout.rect.min.to_vec2())
}

fn vertical_position(
    editor: &Editor,
    layouts: &[ParagraphLayout],
    navigation: Option<&VerticalNavigation>,
    down: bool,
) -> Option<(Position, f32, bool)> {
    let focus = editor.selection().focus;
    let source = editor.document().paragraph(focus.paragraph)?;
    let layout = layouts.get(focus.paragraph)?;
    let cursor = CCursor {
        index: CharIndex(source.scalar_index(focus.byte)?),
        prefer_next_row: navigation.is_none_or(|navigation| navigation.prefer_next_row),
    };
    let row = layout.galley.layout_from_cursor(cursor).row;
    let preferred_x = navigation.map_or_else(
        || layout.rect.left() + layout.galley.pos_from_cursor(cursor).center().x,
        |navigation| navigation.preferred_x,
    );
    let (paragraph, cursor) = if down && row + 1 < layout.galley.rows.len() {
        (
            focus.paragraph,
            layout
                .galley
                .cursor_down_one_row(&cursor, Some(preferred_x - layout.rect.left()))
                .0,
        )
    } else if !down && row > 0 {
        (
            focus.paragraph,
            layout
                .galley
                .cursor_up_one_row(&cursor, Some(preferred_x - layout.rect.left()))
                .0,
        )
    } else if down && focus.paragraph + 1 < layouts.len() {
        let paragraph = focus.paragraph + 1;
        let target = &layouts[paragraph];
        let row = target.galley.rows.first()?;
        (
            paragraph,
            target.galley.cursor_from_pos(Vec2::new(
                preferred_x - target.rect.left(),
                row.rect().center().y,
            )),
        )
    } else if !down && focus.paragraph > 0 {
        let paragraph = focus.paragraph - 1;
        let target = &layouts[paragraph];
        let row = target.galley.rows.last()?;
        (
            paragraph,
            target.galley.cursor_from_pos(Vec2::new(
                preferred_x - target.rect.left(),
                row.rect().center().y,
            )),
        )
    } else {
        (
            focus.paragraph,
            if down {
                layout.galley.end()
            } else {
                layout.galley.begin()
            },
        )
    };
    let source = editor.document().paragraph(paragraph)?;
    let (byte, prefer_next_row) =
        snap_vertical_cursor(source, &layouts[paragraph].galley, cursor, down)?;
    Some((Position::new(paragraph, byte), preferred_x, prefer_next_row))
}

fn snap_vertical_cursor(
    source: &Paragraph,
    galley: &Galley,
    cursor: CCursor,
    down: bool,
) -> Option<(usize, bool)> {
    let byte = source.byte_from_scalar(cursor.index.0)?;
    let before = source.boundary_at_or_before(byte);
    let after = source.boundary_at_or_after(byte);
    if before == after {
        return Some((byte, cursor.prefer_next_row));
    }
    // A space followed by combining marks can wrap between its scalar glyphs.
    // Prefer a real caret boundary on the target row rather than snapping back
    // onto the row we just left. Recompute affinity for the snapped boundary.
    let target_row = galley.layout_from_cursor(cursor).row;
    let on_target_row = |byte| {
        let index = CharIndex(source.scalar_index(byte)?);
        [cursor.prefer_next_row, !cursor.prefer_next_row]
            .into_iter()
            .find(|&prefer_next_row| {
                galley
                    .layout_from_cursor(CCursor {
                        index,
                        prefer_next_row,
                    })
                    .row
                    == target_row
            })
    };
    match (on_target_row(before), on_target_row(after)) {
        (Some(before_affinity), Some(after_affinity)) => Some(if byte - before <= after - byte {
            (before, before_affinity)
        } else {
            (after, after_affinity)
        }),
        (Some(affinity), None) => Some((before, affinity)),
        (None, Some(affinity)) => Some((after, affinity)),
        // A grapheme spanning the entire target row has no legal caret there.
        // Cross the cluster in the requested direction, using the closest row
        // representation of that boundary when it falls on another wrap seam.
        (None, None) => Some(if down { (after, false) } else { (before, true) }),
    }
}

fn paint_document_selection(
    document: &Document,
    selection: Selection,
    layouts: &[ParagraphLayout],
    origin: Pos2,
    painter: &egui::Painter,
    color: Color32,
) {
    let selection = selection.range();
    if selection.start == selection.end {
        return;
    }
    let top = painter.clip_rect().top() - origin.y - 5.0;
    let bottom = painter.clip_rect().bottom() - origin.y + 5.0;
    let first = layouts
        .partition_point(|layout| layout.rect.bottom() < top)
        .max(selection.start.paragraph);
    let last = layouts
        .partition_point(|layout| layout.rect.top() <= bottom)
        .min(selection.end.paragraph.saturating_add(1));
    for index in first..last {
        let Some(layout) = layouts.get(index) else {
            continue;
        };
        if !painter
            .clip_rect()
            .intersects(layout.rect.translate(origin.to_vec2()).expand(5.0))
        {
            continue;
        }
        let Some(source) = document.paragraph(index) else {
            continue;
        };
        let start = if index == selection.start.paragraph {
            selection.start.byte
        } else {
            0
        };
        let end = if index == selection.end.paragraph {
            selection.end.byte
        } else {
            source.text().len()
        };
        let char_start = source.scalar_index(start).unwrap_or(0);
        let char_end = source.scalar_index(end).unwrap_or(source.scalar_count());
        let mut row_start = 0;
        for row in &layout.galley.rows {
            let count = row.char_count_excluding_newline().0;
            let local_start = char_start.saturating_sub(row_start).min(count);
            let local_end = char_end.saturating_sub(row_start).min(count);
            let includes_break = index < selection.end.paragraph && char_end >= row_start + count;
            if local_start < local_end || includes_break {
                let left = row.pos.x + row.x_offset(CharIndex(local_start));
                let mut right = row.pos.x + row.x_offset(CharIndex(local_end));
                if includes_break {
                    right += 5.0;
                }
                let selected = Rect::from_min_max(
                    Pos2::new(left, row.rect().top()),
                    Pos2::new(right, row.rect().bottom()),
                );
                painter.rect_filled(
                    selected.translate(origin.to_vec2() + layout.rect.min.to_vec2()),
                    0.0,
                    color,
                );
            }
            row_start += row.char_count_including_newline().0;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_event(
    editor: &mut Editor,
    ui: &Ui,
    event: &Event,
    layouts: &[ParagraphLayout],
    navigation: &mut Option<VerticalNavigation>,
    errors: &mut Vec<Error>,
    interrupted: &mut bool,
    read_only: bool,
) -> bool {
    if read_only {
        match event {
            Event::Cut => {
                if !editor.selection().is_caret() {
                    ui.ctx().copy_text(editor.selected_text());
                }
                return true;
            }
            Event::Text(_) | Event::Paste(_) | Event::Ime(_) => return true,
            _ => {}
        }
    }
    if matches!(event, Event::Paste(text) if text.is_empty()) {
        return true;
    }
    if editor
        .composition()
        .is_some_and(|composition| composition.text.is_empty())
        && (matches!(event, Event::Text(_) | Event::Paste(_) | Event::Cut)
            || matches!(event, Event::Key { key, pressed: true, .. } if *key != Key::Escape))
    {
        // Empty preedit can be followed by either Commit or normal typing.
        // Keep the replacement for Commit, then cancel it when typing resumes.
        *interrupted |= editor.cancel_composition();
    }
    match event {
        Event::Copy => {
            if !editor.selection().is_caret() {
                ui.ctx().copy_text(editor.selected_text());
            }
            true
        }
        Event::Cut if editor.composition().is_none() => {
            if !editor.selection().is_caret() {
                ui.ctx().copy_text(editor.selected_text());
                editor.break_history_group();
                record(editor.insert_text(""), errors);
                editor.break_history_group();
            }
            true
        }
        Event::Paste(text) if editor.composition().is_none() => {
            editor.break_history_group();
            record(editor.insert_text(text), errors);
            editor.break_history_group();
            true
        }
        Event::Text(text) if editor.composition().is_none() => {
            if !text.is_empty() && text != "\n" && text != "\r" {
                record(editor.insert_text(text), errors);
            }
            true
        }
        Event::Ime(ImeEvent::Preedit {
            text,
            active_range_chars,
        }) => {
            if let Some(range) = active_range_chars
                && (range.start > range.end || range.end > text.chars().count())
            {
                errors.push(Error::InvalidCompositionSelection);
                return true;
            }
            if !text.is_empty() || editor.composition().is_some() {
                let range = active_range_chars.as_ref().map(|range| {
                    byte_from_char(text, range.start)..byte_from_char(text, range.end)
                });
                record(editor.update_composition(text, range), errors);
            }
            true
        }
        Event::Ime(ImeEvent::Commit(text)) => {
            if !text.is_empty() || editor.composition().is_some() {
                record(editor.commit_composition(text), errors);
            }
            true
        }
        Event::Ime(ImeEvent::DeleteSurrounding {
            before_chars,
            after_chars,
        }) => {
            *interrupted |= editor.cancel_composition();
            delete_surrounding(editor, *before_chars, *after_chars, errors);
            true
        }
        #[allow(deprecated)]
        Event::Ime(ImeEvent::Disabled) => {
            editor.cancel_composition();
            true
        }
        #[allow(deprecated)]
        Event::Ime(ImeEvent::Enabled) => true,
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            if *key == Key::Escape {
                let canceled = editor.cancel_composition();
                *interrupted |= canceled;
                if !canceled {
                    ui.memory_mut(|memory| {
                        if let Some(id) = memory.focused() {
                            memory.surrender_focus(id);
                        }
                    });
                }
                return true;
            }
            if editor.composition().is_some() {
                return false;
            }
            handle_key(
                editor, *key, *modifiers, layouts, navigation, errors, read_only,
            )
        }
        _ => false,
    }
}

fn handle_key(
    editor: &mut Editor,
    key: Key,
    modifiers: Modifiers,
    layouts: &[ParagraphLayout],
    navigation: &mut Option<VerticalNavigation>,
    errors: &mut Vec<Error>,
    read_only: bool,
) -> bool {
    // Ctrl+Alt may be AltGr, and Command+Control is a host shortcut chord.
    let command = modifiers.command && !modifiers.alt && !(modifiers.mac_cmd && modifiers.ctrl);
    if command || !matches!(key, Key::ArrowUp | Key::ArrowDown) {
        *navigation = None;
    }
    if read_only
        && (matches!(key, Key::Backspace | Key::Delete | Key::Enter)
            || command
                && matches!(
                    key,
                    Key::Z
                        | Key::Y
                        | Key::B
                        | Key::I
                        | Key::U
                        | Key::Num0
                        | Key::Num1
                        | Key::Num2
                        | Key::Num3
                        | Key::Num4
                        | Key::Num5
                        | Key::Num6
                        | Key::Num7
                        | Key::Num8
                ))
    {
        return true;
    }
    if command {
        match key {
            Key::A => {
                editor.select_all();
                return true;
            }
            Key::Z => {
                if modifiers.shift {
                    editor.redo();
                } else {
                    editor.undo();
                }
                return true;
            }
            Key::Y => {
                editor.redo();
                return true;
            }
            Key::B => {
                record(
                    editor.apply_style(StylePatch {
                        bold: Some(!editor.typing_style().bold),
                        ..Default::default()
                    }),
                    errors,
                );
                return true;
            }
            Key::I => {
                record(
                    editor.apply_style(StylePatch {
                        italic: Some(!editor.typing_style().italic),
                        ..Default::default()
                    }),
                    errors,
                );
                return true;
            }
            Key::U => {
                record(
                    editor.apply_style(StylePatch {
                        underline: Some(!editor.typing_style().underline),
                        ..Default::default()
                    }),
                    errors,
                );
                return true;
            }
            Key::Num0 => {
                record(editor.set_paragraph_kind(ParagraphKind::Body), errors);
                return true;
            }
            Key::Num1 | Key::Num2 | Key::Num3 | Key::Num4 | Key::Num5 | Key::Num6 => {
                let level = match key {
                    Key::Num1 => 1,
                    Key::Num2 => 2,
                    Key::Num3 => 3,
                    Key::Num4 => 4,
                    Key::Num5 => 5,
                    _ => 6,
                };
                record(
                    editor.set_paragraph_kind(ParagraphKind::Heading { level }),
                    errors,
                );
                return true;
            }
            Key::Num7 if modifiers.shift => {
                record(
                    editor.set_paragraph_kind(ParagraphKind::Ordered {
                        indent: 0,
                        start: 1,
                    }),
                    errors,
                );
                return true;
            }
            Key::Num8 if modifiers.shift => {
                record(
                    editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 }),
                    errors,
                );
                return true;
            }
            _ => {}
        }
    }
    let by_word = modifiers.ctrl || modifiers.alt;
    let movement = match key {
        Key::ArrowLeft if command && modifiers.mac_cmd => Some(Movement::ParagraphStart),
        Key::ArrowRight if command && modifiers.mac_cmd => Some(Movement::ParagraphEnd),
        Key::ArrowUp if command => Some(Movement::DocumentStart),
        Key::ArrowDown if command => Some(Movement::DocumentEnd),
        Key::ArrowLeft => Some(if by_word {
            Movement::WordBackward
        } else {
            Movement::GraphemeBackward
        }),
        Key::ArrowRight => Some(if by_word {
            Movement::WordForward
        } else {
            Movement::GraphemeForward
        }),
        Key::Home => Some(if modifiers.command {
            Movement::DocumentStart
        } else {
            Movement::ParagraphStart
        }),
        Key::End => Some(if modifiers.command {
            Movement::DocumentEnd
        } else {
            Movement::ParagraphEnd
        }),
        _ => None,
    };
    if let Some(movement) = movement {
        record(editor.move_cursor(movement, modifiers.shift), errors);
        return true;
    }
    match key {
        Key::ArrowUp | Key::ArrowDown => {
            let Some((position, preferred_x, prefer_next_row)) =
                vertical_position(editor, layouts, navigation.as_ref(), key == Key::ArrowDown)
            else {
                return true;
            };
            let anchor = if modifiers.shift {
                editor.selection().anchor
            } else {
                position
            };
            record(
                editor.set_selection(Selection::new(anchor, position)),
                errors,
            );
            *navigation = Some(VerticalNavigation {
                selection: editor.selection(),
                preferred_x,
                prefer_next_row,
            });
            true
        }
        Key::Backspace => {
            record(
                if by_word {
                    editor.delete_word_backward()
                } else {
                    editor.delete_backward()
                },
                errors,
            );
            true
        }
        Key::Delete => {
            record(
                if by_word {
                    editor.delete_word_forward()
                } else {
                    editor.delete_forward()
                },
                errors,
            );
            true
        }
        Key::Enter if !modifiers.command => {
            record(editor.insert_paragraph(), errors);
            true
        }
        Key::Tab
            if !read_only
                && !modifiers.command
                && !modifiers.ctrl
                && !modifiers.alt
                && selection_contains_list(editor) =>
        {
            record(
                if modifiers.shift {
                    editor.outdent_list()
                } else {
                    editor.indent_list()
                },
                errors,
            );
            true
        }
        _ => false,
    }
}

fn selection_contains_list(editor: &Editor) -> bool {
    editor.document().paragraphs()[selected_paragraphs(editor)]
        .iter()
        .any(|paragraph| {
            matches!(
                paragraph.kind(),
                ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. }
            )
        })
}

fn selected_paragraphs(editor: &Editor) -> std::ops::Range<usize> {
    let range = editor.selection().range();
    let end = if range.end.paragraph > range.start.paragraph && range.end.byte == 0 {
        range.end.paragraph
    } else {
        range.end.paragraph + 1
    };
    range.start.paragraph..end
}

fn delete_surrounding(editor: &mut Editor, before: usize, after: usize, errors: &mut Vec<Error>) {
    let focus = editor.selection().focus;
    let start = surrounding_position(editor.document(), focus, before, false);
    let end = surrounding_position(editor.document(), focus, after, true);
    if start < end {
        record(editor.replace_range(start..end, ""), errors);
    }
}

fn surrounding_position(
    document: &Document,
    focus: Position,
    mut remaining: usize,
    forward: bool,
) -> Position {
    let mut paragraph = focus.paragraph;
    let mut index = document
        .paragraph(paragraph)
        .expect("editor maintains a valid selection")
        .scalar_index(focus.byte)
        .expect("editor maintains a valid selection");
    loop {
        let source = document.paragraph(paragraph).expect("valid paragraph");
        let chars = source.scalar_count();
        let available = if forward { chars - index } else { index };
        if remaining <= available {
            let target = if forward {
                index + remaining
            } else {
                index - remaining
            };
            let byte = source
                .byte_from_scalar(target)
                .unwrap_or(source.text().len());
            let boundary = if forward {
                source.boundary_at_or_after(byte)
            } else {
                source.boundary_at_or_before(byte)
            };
            return Position::new(paragraph, boundary);
        }
        // Only visit paragraphs crossed by the requested native scalar count.
        // Each paragraph separator contributes one scalar, including empty rows.
        remaining -= available;
        if forward {
            if paragraph + 1 == document.paragraphs().len() {
                return document.end();
            }
            paragraph += 1;
            index = 0;
        } else {
            if paragraph == 0 {
                return Position::default();
            }
            paragraph -= 1;
            index = document
                .paragraph(paragraph)
                .expect("valid paragraph")
                .scalar_count();
        }
        remaining -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        context: &egui::Context,
        editor: &mut Editor,
        events: Vec<Event>,
        focus: bool,
    ) -> egui::FullOutput {
        frame_options(context, editor, events, focus, false)
    }

    fn frame_options(
        context: &egui::Context,
        editor: &mut Editor,
        events: Vec<Event>,
        focus: bool,
        read_only: bool,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
            events,
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let id = ui.make_persistent_id("editor");
                if focus {
                    ui.memory_mut(|memory| {
                        if !memory.has_focus(id) {
                            memory.request_focus(id);
                        }
                    });
                }
                let output = RichTextEditor::new(editor)
                    .id(id)
                    .read_only(read_only)
                    .show(ui);
                assert!(output.errors.is_empty(), "{:?}", output.errors);
                assert!(
                    output.accessibility_errors.is_empty(),
                    "{:?}",
                    output.accessibility_errors
                );
            });
        });
        output.textures_delta.clear();
        output
    }

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn appearance() -> Appearance {
        Appearance {
            font: FontId::proportional(16.0),
            bold_family: None,
            color: Color32::WHITE,
            strong_color: Color32::WHITE,
            code_background: Color32::BLACK,
            width: 400.0,
        }
    }

    fn cached_layout_frame(
        context: &egui::Context,
        cache: &mut Cache,
        editor: &Editor,
        appearance: &Appearance,
    ) -> Arc<[ParagraphLayout]> {
        let mut layouts = Arc::from([]);
        context
            .run_ui(egui::RawInput::default(), |ui| {
                layouts = cache.layout(ui, editor, appearance);
            })
            .drop_without_applying_deltas();
        layouts
    }

    #[test]
    fn idle_layouts_and_unchanged_paragraph_galleys_are_reused() {
        let context = egui::Context::default();
        let mut cache = Cache::default();
        let mut editor = Editor::from_text("first\nsecond\nthird");
        let appearance = appearance();
        let first = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        let idle = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        assert!(Arc::ptr_eq(&first, &idle));
        editor
            .set_selection(Selection::caret(Position::new(1, 0)))
            .unwrap();
        editor.insert_text("edited ").unwrap();
        let edited = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        assert!(Arc::ptr_eq(&first[0].galley, &edited[0].galley));
        assert!(!Arc::ptr_eq(&first[1].galley, &edited[1].galley));
        assert!(Arc::ptr_eq(&first[2].galley, &edited[2].galley));
        assert_eq!(edited[1].galley.job.text, "edited second");
        assert!(cache.plain_text.is_none());
        assert_eq!(
            cache.char_offset(editor.document(), Position::new(2, 2)),
            22
        );
    }

    #[test]
    fn layout_cache_refreshes_for_width_scale_fonts_and_colors() {
        let context = egui::Context::default();
        let mut cache = Cache::default();
        let editor = Editor::from_text(&"words ".repeat(30));
        let mut appearance = appearance();
        let wide = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        appearance.width = 100.0;
        let narrow = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        assert!(narrow[0].galley.size().y > wide[0].galley.size().y);
        context.set_pixels_per_point(2.0);
        let scaled = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        assert!(!Arc::ptr_eq(&narrow[0].galley, &scaled[0].galley));
        let mut definitions = egui::FontDefinitions::default();
        definitions.families.insert(
            FontFamily::Proportional,
            definitions.families[&FontFamily::Monospace].clone(),
        );
        context.set_fonts(definitions);
        let replaced_fonts = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        assert!(!Arc::ptr_eq(&scaled[0].galley, &replaced_fonts[0].galley));
        appearance.color = Color32::RED;
        let recolored = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        assert_eq!(
            recolored[0].galley.job.sections[0].format.color,
            Color32::RED
        );
        let idle = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        assert!(Arc::ptr_eq(&recolored, &idle));
    }

    #[test]
    fn malformed_shaping_retries_preserve_rendered_text_styles_and_widget_navigation() {
        let context = egui::Context::default();
        let cluster = format!("{}z", "\u{600}".repeat(16));
        let text = format!("a {cluster} end");
        let mut editor = Editor::from_text(&text);
        editor
            .set_selection(Selection::new(
                Position::new(0, 2),
                Position::new(0, 2 + cluster.len()),
            ))
            .unwrap();
        editor
            .apply_style(StylePatch {
                italic: Some(true),
                underline: Some(true),
                foreground: Some(Some(crate::Color([42, 100, 200, 255]))),
                ..Default::default()
            })
            .unwrap();
        editor
            .set_selection(Selection::caret(Position::default()))
            .unwrap();
        let revision = editor.document().revision();
        let draw = |editor: &mut Editor, events| {
            context
                .run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| {
                            let id = ui.make_persistent_id("malformed-shaping");
                            ui.memory_mut(|memory| memory.request_focus(id));
                            let output = RichTextEditor::new(editor)
                                .id(id)
                                .font(FontId::proportional(14.0))
                                .desired_width(48.0)
                                .show(ui);
                            assert!(output.errors.is_empty(), "{:?}", output.errors);
                        });
                    },
                )
                .drop_without_applying_deltas();
        };
        draw(&mut editor, vec![]);
        let id = context.memory(|memory| memory.focused()).unwrap();
        let cache = context.data(|data| data.get_temp::<Cache>(id)).unwrap();
        let galley = &cache.layouts[0].galley;
        assert_eq!(galley.job.text, text);
        assert_eq!(galley.end().index.0, text.chars().count());
        let rendered: String = galley
            .rows
            .iter()
            .flat_map(|row| row.glyphs.iter().map(|glyph| glyph.chr))
            .collect();
        assert_eq!(rendered, text);
        let source = editor.document().paragraph(0).unwrap();
        for section in &galley.job.sections {
            let style = source
                .spans()
                .iter()
                .find(|span| span.range.contains(&section.byte_range.start.0))
                .unwrap()
                .style;
            assert_eq!(section.format.italics, style.italic);
            assert_eq!(section.format.underline != Stroke::NONE, style.underline);
            if let Some(color) = style.foreground {
                assert_eq!(
                    section.format.color,
                    Color32::from_rgba_unmultiplied(color.0[0], color.0[1], color.0[2], color.0[3])
                );
            }
        }
        draw(&mut editor, vec![]);
        let idle = context.data(|data| data.get_temp::<Cache>(id)).unwrap();
        assert!(Arc::ptr_eq(&cache.layouts, &idle.layouts));
        assert!(Arc::ptr_eq(galley, &idle.layouts[0].galley));
        for _ in 0..galley.rows.len() {
            draw(&mut editor, vec![key(Key::ArrowDown, Modifiers::NONE)]);
            editor
                .document()
                .validate_position(editor.selection().focus)
                .unwrap();
        }
        assert_eq!(editor.selection().focus, editor.document().end());
        for _ in 0..galley.rows.len() {
            draw(&mut editor, vec![key(Key::ArrowUp, Modifiers::NONE)]);
            editor
                .document()
                .validate_position(editor.selection().focus)
                .unwrap();
        }
        assert_eq!(editor.selection().focus, Position::default());
        assert_eq!(editor.document().revision(), revision);
        assert_eq!(editor.document().plain_text(), text);
    }

    #[test]
    fn font_probe_refreshes_layouts_when_text_options_recreate_the_atlas() {
        let context = egui::Context::default();
        let mut cache = Cache::default();
        let editor = Editor::from_text("atlas glyphs café 👩‍💻\nunchanged paragraph");
        let appearance = appearance();
        let mut previous = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        for change_hinting in [true, false] {
            let probe = cache.font_probe.clone().unwrap();
            context.global_style_mut(|style| {
                let options = &mut style.visuals.text_options;
                if change_hinting {
                    options.font_hinting = !options.font_hinting;
                } else {
                    options.subpixel_binning = !options.subpixel_binning;
                }
            });
            let refreshed = cached_layout_frame(&context, &mut cache, &editor, &appearance);
            assert!(!Arc::ptr_eq(&probe, cache.font_probe.as_ref().unwrap()));
            for (old, new) in previous.iter().zip(refreshed.iter()) {
                assert!(!Arc::ptr_eq(&old.galley, &new.galley));
                assert_eq!(old.galley.job.text, new.galley.job.text);
            }
            let idle = cached_layout_frame(&context, &mut cache, &editor, &appearance);
            assert!(Arc::ptr_eq(&refreshed, &idle));
            previous = refreshed;
        }
    }

    #[test]
    fn inactive_accessibility_keeps_text_lazy_but_events_report_changes() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("hello");
        frame(&context, &mut editor, vec![], true);
        let output = frame(&context, &mut editor, vec![Event::Text("x".into())], true);
        let id = context.memory(|memory| memory.focused()).unwrap();
        assert!(
            context
                .data(|data| data.get_temp::<Cache>(id))
                .unwrap()
                .plain_text
                .is_none()
        );
        assert!(
            output
                .platform_output
                .events
                .iter()
                .any(|event| matches!(event, egui::output::OutputEvent::ValueChanged(_)))
        );
        context.enable_accesskit();
        let output = frame(&context, &mut editor, vec![Event::Text("y".into())], true);
        assert!(output.platform_output.events.iter().any(|event| {
            matches!(event, egui::output::OutputEvent::ValueChanged(info)
                if info.current_text_value.as_deref() == Some("xyhello")
                && info.prev_text_value.as_deref() == Some("xhello"))
        }));
    }

    #[test]
    fn read_only_consumes_mutations_and_preserves_copy_selection_and_history() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("hello");
        editor.insert_text("x").unwrap();
        editor.select_all();
        let revision = editor.document().revision();
        let output = frame_options(
            &context,
            &mut editor,
            vec![
                Event::Cut,
                Event::Paste("pasted".into()),
                Event::Text("typed".into()),
                key(Key::Backspace, Modifiers::NONE),
                key(Key::Delete, Modifiers::NONE),
                key(Key::Enter, Modifiers::NONE),
                key(Key::B, Modifiers::COMMAND),
                key(Key::Z, Modifiers::COMMAND),
                key(Key::Y, Modifiers::COMMAND),
                key(Key::Num1, Modifiers::COMMAND),
                Event::Ime(ImeEvent::Preedit {
                    text: "に".into(),
                    active_range_chars: Some(1..1),
                }),
                Event::Ime(ImeEvent::Commit("日本".into())),
                Event::Ime(ImeEvent::DeleteSurrounding {
                    before_chars: 1,
                    after_chars: 1,
                }),
            ],
            true,
            true,
        );
        assert_eq!(editor.document().plain_text(), "xhello");
        assert_eq!(editor.document().revision(), revision);
        assert!(!editor.typing_style().bold);
        assert!(editor.composition().is_none());
        assert!(output.platform_output.ime.is_none());
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(text) if text == "xhello")
        }));
        assert!(context.memory(|memory| memory.focused()).is_some());
        frame_options(
            &context,
            &mut editor,
            vec![key(Key::ArrowRight, Modifiers::NONE)],
            true,
            true,
        );
        assert!(editor.selection().is_caret());
        frame_options(
            &context,
            &mut editor,
            vec![key(Key::A, Modifiers::COMMAND)],
            true,
            true,
        );
        assert_eq!(editor.selected_text(), "xhello");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "hello");
    }

    #[test]
    fn read_only_cancels_existing_composition() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("original");
        editor.update_composition("candidate", None).unwrap();
        frame_options(&context, &mut editor, vec![], true, true);
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "original");
        assert!(!editor.can_undo());
    }

    #[test]
    fn disabling_widget_cancels_composition_without_processing_input() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("original");
        frame(&context, &mut editor, vec![], true);
        editor.update_composition("candidate", None).unwrap();
        let output = context.run_ui(
            egui::RawInput {
                events: vec![Event::Text("typed".into())],
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.disable();
                    let id = ui.make_persistent_id("editor");
                    ui.add(RichTextEditor::new(&mut editor).id(id));
                });
            },
        );
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "original");
        assert!(output.platform_output.ime.is_none());
        assert!(!editor.can_undo());
        output.drop_without_applying_deltas();
    }

    #[test]
    fn escape_releases_focus_before_later_text_events() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("original");
        frame(&context, &mut editor, vec![], true);
        frame(
            &context,
            &mut editor,
            vec![
                key(Key::Escape, Modifiers::NONE),
                Event::Text("typed".into()),
            ],
            true,
        );
        assert_eq!(editor.document().plain_text(), "original");
        assert!(context.memory(|memory| memory.focused()).is_none());
        assert_eq!(
            context.input(|input| input.events.clone()),
            vec![Event::Text("typed".into())]
        );
    }

    #[test]
    fn second_escape_after_canceling_composition_releases_focus() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("original");
        frame(&context, &mut editor, vec![], true);
        editor.update_composition("candidate", None).unwrap();
        frame(
            &context,
            &mut editor,
            vec![
                key(Key::Escape, Modifiers::NONE),
                key(Key::Escape, Modifiers::NONE),
                Event::Text("typed".into()),
            ],
            true,
        );
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "original");
        assert!(context.memory(|memory| memory.focused()).is_none());
        assert_eq!(
            context.input(|input| input.events.clone()),
            vec![Event::Text("typed".into())]
        );
    }

    #[test]
    fn first_escape_cancels_empty_composition_and_second_releases_focus() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("original");
        frame(&context, &mut editor, vec![], true);
        editor.update_composition("", None).unwrap();
        frame(
            &context,
            &mut editor,
            vec![key(Key::Escape, Modifiers::NONE)],
            true,
        );
        assert!(editor.composition().is_none());
        assert!(context.memory(|memory| memory.focused()).is_some());
        frame(
            &context,
            &mut editor,
            vec![
                key(Key::Escape, Modifiers::NONE),
                Event::Text("typed".into()),
            ],
            false,
        );
        assert_eq!(editor.document().plain_text(), "original");
        assert!(context.memory(|memory| memory.focused()).is_none());
    }

    #[test]
    fn escape_releases_focus_after_read_only_cancels_composition() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("original");
        frame(&context, &mut editor, vec![], true);
        editor.update_composition("candidate", None).unwrap();
        frame_options(
            &context,
            &mut editor,
            vec![key(Key::Escape, Modifiers::NONE)],
            true,
            true,
        );
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "original");
        assert!(context.memory(|memory| memory.focused()).is_none());
    }

    #[test]
    fn command_navigation_and_altgr_preserve_platform_shortcut_semantics() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("first\nsecond");
        let mac_command = Modifiers::MAC_CMD | Modifiers::COMMAND;
        editor
            .set_selection(Selection::caret(Position::new(1, 3)))
            .unwrap();
        frame(
            &context,
            &mut editor,
            vec![key(Key::ArrowLeft, mac_command)],
            true,
        );
        assert_eq!(editor.selection().focus, Position::new(1, 0));
        frame(
            &context,
            &mut editor,
            vec![key(Key::ArrowRight, mac_command)],
            true,
        );
        assert_eq!(editor.selection().focus, Position::new(1, 6));
        frame(
            &context,
            &mut editor,
            vec![key(Key::ArrowUp, mac_command)],
            true,
        );
        assert_eq!(editor.selection().focus, Position::default());
        frame(
            &context,
            &mut editor,
            vec![key(Key::ArrowDown, mac_command)],
            true,
        );
        assert_eq!(editor.selection().focus, editor.document().end());
        let altgr = Modifiers::ALT | Modifiers::CTRL | Modifiers::COMMAND;
        frame(
            &context,
            &mut editor,
            vec![key(Key::B, altgr), Event::Text("β".into())],
            true,
        );
        assert_eq!(editor.document().plain_text(), "first\nsecondβ");
        assert!(!editor.typing_style().bold);
    }

    #[test]
    fn modifier_delete_shortcuts_remove_whole_words_and_restore_selection_on_undo() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("café 東京 next");
        editor
            .set_selection(Selection::caret(editor.document().end()))
            .unwrap();
        let before = editor.selection();
        frame(
            &context,
            &mut editor,
            vec![key(Key::Backspace, Modifiers::CTRL)],
            true,
        );
        assert_eq!(editor.document().plain_text(), "café 東京 ");
        assert!(editor.undo());
        assert_eq!(editor.selection(), before);
        editor
            .set_selection(Selection::caret(Position::default()))
            .unwrap();
        frame(
            &context,
            &mut editor,
            vec![key(Key::Delete, Modifiers::ALT)],
            true,
        );
        assert_eq!(editor.document().plain_text(), " 東京 next");
    }

    #[test]
    fn tab_and_shift_tab_indent_lists_while_body_tab_keeps_host_focus_navigation() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("first\nsecond");
        editor.select_all();
        editor
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
            .unwrap();
        frame(&context, &mut editor, vec![], true);
        frame(&context, &mut editor, vec![], true);
        frame(
            &context,
            &mut editor,
            vec![key(Key::Tab, Modifiers::NONE)],
            true,
        );
        assert!(
            editor
                .document()
                .paragraphs()
                .iter()
                .all(|paragraph| paragraph.kind() == ParagraphKind::Bullet { indent: 1 })
        );
        frame(
            &context,
            &mut editor,
            vec![key(Key::Tab, Modifiers::SHIFT)],
            true,
        );
        assert!(
            editor
                .document()
                .paragraphs()
                .iter()
                .all(|paragraph| paragraph.kind() == ParagraphKind::Bullet { indent: 0 })
        );
        frame_options(
            &context,
            &mut editor,
            vec![key(Key::Tab, Modifiers::NONE)],
            true,
            true,
        );
        assert!(
            editor
                .document()
                .paragraphs()
                .iter()
                .all(|paragraph| paragraph.kind() == ParagraphKind::Bullet { indent: 0 })
        );
        editor.set_paragraph_kind(ParagraphKind::Body).unwrap();
        frame(
            &context,
            &mut editor,
            vec![key(Key::Tab, Modifiers::NONE)],
            true,
        );
        assert_eq!(editor.document().plain_text(), "first\nsecond");
        assert!(!selection_contains_list(&editor));
    }

    #[test]
    fn read_only_native_node_advertises_selection_and_rejects_mutation_actions() {
        use egui::accesskit::{
            Action, ActionData, ActionRequest, Role, TextPosition, TextSelection, TreeId,
        };
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut editor = Editor::from_text("original");
        let output = frame_options(&context, &mut editor, vec![], true, true);
        let update = output.platform_output.accesskit_update.unwrap();
        let (id, node) = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::MultilineTextInput)
            .unwrap();
        assert!(node.is_read_only());
        assert!(node.supports_action(Action::SetTextSelection));
        assert!(!node.supports_action(Action::SetValue));
        assert!(!node.supports_action(Action::ReplaceSelectedText));
        let (run_id, _) = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::TextRun)
            .unwrap();
        frame_options(
            &context,
            &mut editor,
            vec![Event::AccessKitActionRequest(ActionRequest {
                action: Action::SetTextSelection,
                target_node: *id,
                target_tree: TreeId::ROOT,
                data: Some(ActionData::SetTextSelection(TextSelection {
                    anchor: TextPosition {
                        node: *run_id,
                        character_index: 0,
                    },
                    focus: TextPosition {
                        node: *run_id,
                        character_index: 3,
                    },
                })),
            })],
            true,
            true,
        );
        assert_eq!(editor.selected_text(), "ori");
        for action in [Action::SetValue, Action::ReplaceSelectedText] {
            frame_options(
                &context,
                &mut editor,
                vec![Event::AccessKitActionRequest(ActionRequest {
                    action,
                    target_node: *id,
                    target_tree: TreeId::ROOT,
                    data: Some(ActionData::Value("replacement".into())),
                })],
                true,
                true,
            );
        }
        assert_eq!(editor.document().plain_text(), "original");
        assert_eq!(editor.selected_text(), "ori");
        assert!(!editor.can_undo());
    }

    #[test]
    fn focused_widget_edits_and_undoes() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("hello");
        frame(
            &context,
            &mut editor,
            vec![Event::Text("世界".into())],
            true,
        );
        assert_eq!(editor.document().plain_text(), "世界hello");
        frame(
            &context,
            &mut editor,
            vec![key(Key::Z, Modifiers::COMMAND)],
            true,
        );
        assert_eq!(editor.document().plain_text(), "hello");
        frame(
            &context,
            &mut editor,
            vec![key(Key::Y, Modifiers::COMMAND)],
            true,
        );
        assert_eq!(editor.document().plain_text(), "世界hello");
    }

    #[test]
    fn unfocused_widget_leaves_text_events_for_host() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("hello");
        frame(
            &context,
            &mut editor,
            vec![Event::Text("world".into())],
            false,
        );
        assert_eq!(editor.document().plain_text(), "hello");
    }

    #[test]
    fn ime_preedit_is_transient_then_commits_once() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("hello");
        frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::Preedit {
                text: "に".into(),
                active_range_chars: Some(1..1),
            })],
            true,
        );
        assert_eq!(editor.document().plain_text(), "hello");
        assert_eq!(
            editor
                .composition()
                .map(|composition| composition.text.as_str()),
            Some("に")
        );
        frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::Commit("日本".into()))],
            true,
        );
        assert_eq!(editor.document().plain_text(), "日本hello");
        assert!(editor.composition().is_none());
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "hello");
    }

    #[test]
    fn formatting_shortcut_produces_formatted_layout_sections() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("styled");
        editor.select_all();
        frame(
            &context,
            &mut editor,
            vec![key(Key::I, Modifiers::COMMAND)],
            true,
        );
        let mut checked = false;
        context
            .run_ui(egui::RawInput::default(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let appearance = Appearance {
                        font: FontId::proportional(16.0),
                        bold_family: None,
                        color: Color32::WHITE,
                        strong_color: Color32::WHITE,
                        code_background: Color32::BLACK,
                        width: 400.0,
                    };
                    let job = paragraph_job(
                        editor.document().paragraph(0).expect("paragraph"),
                        &appearance,
                        400.0,
                    );
                    assert!(job.sections.iter().all(|section| section.format.italics));
                    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                    assert_eq!(galley.job.text, "styled");
                    checked = true;
                });
            })
            .drop_without_applying_deltas();
        assert!(checked);
    }

    #[test]
    fn character_hit_positions_snap_to_whole_graphemes() {
        let text = "a👨‍👩‍👧‍👦e\u{301}";
        for char_index in 0..=text.chars().count() {
            let byte = snap_grapheme(text, byte_from_char(text, char_index));
            assert!(
                byte == text.len()
                    || text
                        .grapheme_indices(true)
                        .any(|(boundary, _)| boundary == byte)
            );
        }
    }

    #[test]
    fn ordered_list_reserves_space_for_long_numbers() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("item");
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: 1_000_000,
            })
            .expect("valid list");
        context
            .run_ui(egui::RawInput::default(), |ui| {
                let appearance = Appearance {
                    font: FontId::proportional(16.0),
                    bold_family: None,
                    color: Color32::WHITE,
                    strong_color: Color32::WHITE,
                    code_background: Color32::BLACK,
                    width: 400.0,
                };
                let mut cache = Cache::default();
                let layouts = cache.layout(ui, &editor, &appearance);
                let marker = layouts[0].marker.as_ref().expect("ordered marker");
                assert_eq!(marker.job.text, "1000000.");
                assert!(layouts[0].marker_position.x >= 0.0);
                assert!(layouts[0].rect.left() >= marker.size().x + 8.0);
            })
            .drop_without_applying_deltas();
    }

    #[test]
    fn empty_paste_preserves_selection_and_typing_history() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("original");
        editor.insert_text("typed ").unwrap();
        editor.select_all();
        let selection = editor.selection();
        let revision = editor.document().revision();
        frame(
            &context,
            &mut editor,
            vec![Event::Paste(String::new())],
            true,
        );
        assert_eq!(editor.document().plain_text(), "typed original");
        assert_eq!(editor.document().revision(), revision);
        assert_eq!(editor.selection(), selection);
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "original");
        assert!(!editor.can_undo());

        frame(
            &context,
            &mut editor,
            vec![
                Event::Text("a".into()),
                Event::Paste(String::new()),
                Event::Text("b".into()),
            ],
            true,
        );
        assert_eq!(editor.document().plain_text(), "aboriginal");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "original");
        assert!(!editor.can_undo());

        for preedit in ["", "候"] {
            editor.update_composition(preedit, None).unwrap();
            let composition = editor.composition().cloned();
            frame(
                &context,
                &mut editor,
                vec![Event::Paste(String::new())],
                true,
            );
            assert_eq!(editor.composition(), composition.as_ref());
            assert_eq!(editor.document().plain_text(), "original");
            assert!(!editor.can_undo());
        }
    }

    #[test]
    fn double_click_at_paragraph_end_selects_the_trailing_word() {
        for (text, expected) in [
            ("hello café", "café"),
            ("hello 👩🏽‍💻", "👩🏽‍💻"),
            ("e\u{301}", "e\u{301}"),
            ("hello ", " "),
        ] {
            let editor = Editor::from_text(text);
            let selection = word_selection(&editor, editor.document().end());
            assert_eq!(
                &text[selection.range().start.byte..selection.range().end.byte],
                expected,
                "{text:?}"
            );
            editor
                .document()
                .validate_position(selection.anchor)
                .unwrap();
            editor
                .document()
                .validate_position(selection.focus)
                .unwrap();
        }
        let editor = Editor::from_text("");
        assert!(word_selection(&editor, editor.document().end()).is_caret());
    }

    #[test]
    fn input_batches_consume_editing_and_preserve_host_events_in_order() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("");
        frame(&context, &mut editor, vec![], true);
        let mut events = Vec::new();
        let mut retained = Vec::new();
        for index in 0..64 {
            events.push(key(Key::ArrowLeft, Modifiers::NONE));
            let mut shortcut = key(Key::F1, Modifiers::NONE);
            if let Event::Key { repeat, .. } = &mut shortcut {
                *repeat = index > 0;
            }
            events.push(shortcut.clone());
            retained.push(shortcut);
            let pointer = Event::PointerMoved(Pos2::new(index as f32, 0.0));
            events.push(pointer.clone());
            retained.push(pointer);
            events.push(Event::Text("x".into()));
        }
        frame(&context, &mut editor, events, true);
        assert_eq!(editor.document().plain_text(), "x".repeat(64));
        assert_eq!(context.input(|input| input.events.clone()), retained);
    }

    #[test]
    fn paste_is_independent_from_adjacent_typing() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("");
        frame(
            &context,
            &mut editor,
            vec![
                Event::Text("one".into()),
                Event::Paste("two".into()),
                Event::Text("three".into()),
            ],
            true,
        );
        assert_eq!(editor.document().plain_text(), "onetwothree");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "onetwo");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "one");
    }

    #[test]
    fn enter_then_movement_in_same_frame_uses_current_layout() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("");
        frame(
            &context,
            &mut editor,
            vec![
                Event::Text("first".into()),
                key(Key::Enter, Modifiers::NONE),
                Event::Text("second".into()),
                key(Key::ArrowUp, Modifiers::NONE),
            ],
            true,
        );
        assert_eq!(editor.document().plain_text(), "first\nsecond");
        assert_eq!(editor.selection().focus.paragraph, 0);
    }

    #[test]
    fn vertical_navigation_keeps_the_column_across_short_paragraphs() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("aaaaaaaaa\nx\naaaaaaaaa");
        editor
            .set_selection(Selection::caret(Position::new(0, 8)))
            .unwrap();
        frame(&context, &mut editor, vec![], true);
        for position in [Position::new(1, 1), Position::new(2, 8)] {
            frame(
                &context,
                &mut editor,
                vec![key(Key::ArrowDown, Modifiers::NONE)],
                true,
            );
            assert_eq!(editor.selection().focus, position);
        }
    }

    #[test]
    fn vertical_navigation_preserves_wrapped_row_affinity_and_horizontal_position() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("aaaaaaaaa x aaaaaaaaa");
        editor
            .set_selection(Selection::caret(Position::new(0, 8)))
            .unwrap();
        let mut cache = Cache::default();
        let mut appearance = appearance();
        appearance.font = FontId::monospace(16.0);
        appearance.width = 100.0;
        let layouts = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        assert_eq!(layouts[0].galley.rows.len(), 3);
        let mut navigation = None;
        let mut errors = Vec::new();
        for (key, expected_row, expected_byte) in [
            (Key::ArrowDown, 1, 12),
            (Key::ArrowDown, 2, 20),
            (Key::ArrowUp, 1, 12),
            (Key::ArrowUp, 0, 8),
        ] {
            assert!(handle_key(
                &mut editor,
                key,
                Modifiers::NONE,
                &layouts,
                &mut navigation,
                &mut errors,
                false,
            ));
            assert_eq!(editor.selection().focus.byte, expected_byte);
            let navigation = navigation.as_ref().unwrap();
            let cursor = CCursor {
                index: CharIndex(expected_byte),
                prefer_next_row: navigation.prefer_next_row,
            };
            assert_eq!(
                layouts[0].galley.layout_from_cursor(cursor).row,
                expected_row
            );
        }
        assert!(errors.is_empty());
    }

    #[test]
    fn vertical_navigation_crosses_combining_graphemes_split_at_indented_wraps() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("previous\na \u{301}bbbbbbbbbbbbbbbbbbbbbbbb");
        editor
            .set_selection(Selection::caret(Position::new(1, 0)))
            .unwrap();
        editor
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
            .unwrap();
        editor
            .set_selection(Selection::caret(Position::default()))
            .unwrap();
        let mut cache = Cache::default();
        let mut appearance = appearance();
        appearance.font = FontId::proportional(14.0);
        appearance.width = 104.0;
        let layouts = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        let galley = &layouts[1].galley;
        assert!(galley.rows.len() >= 3);
        // egui can wrap between the space and its zero-width combining mark.
        assert_eq!(galley.rows[0].char_count_excluding_newline().0, 2);
        let mut navigation = None;
        let mut errors = Vec::new();
        for (key, paragraph, row, byte) in [
            (Key::ArrowDown, 1, 0, Some(0)),
            (Key::ArrowDown, 1, 1, Some("a \u{301}".len())),
            (Key::ArrowDown, 1, 2, None),
            (Key::ArrowUp, 1, 1, Some("a \u{301}".len())),
            (Key::ArrowUp, 1, 0, Some(0)),
            (Key::ArrowUp, 0, 0, Some(0)),
        ] {
            assert!(handle_key(
                &mut editor,
                key,
                Modifiers::NONE,
                &layouts,
                &mut navigation,
                &mut errors,
                false,
            ));
            let focus = editor.selection().focus;
            editor.document().validate_position(focus).unwrap();
            assert_eq!(focus.paragraph, paragraph);
            if let Some(byte) = byte {
                assert_eq!(focus.byte, byte);
            }
            let cursor = CCursor {
                index: CharIndex(
                    editor
                        .document()
                        .paragraph(paragraph)
                        .unwrap()
                        .scalar_index(focus.byte)
                        .unwrap(),
                ),
                prefer_next_row: navigation.as_ref().unwrap().prefer_next_row,
            };
            assert_eq!(
                layouts[paragraph].galley.layout_from_cursor(cursor).row,
                row
            );
        }
        assert!(errors.is_empty());
    }

    #[test]
    fn vertical_navigation_roundtrips_graphemes_spanning_entire_visual_rows() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text(&format!("a {} end", "👨\u{200d}".repeat(8)));
        let source = editor.document().paragraphs()[0].clone();
        let mut cache = Cache::default();
        let mut appearance = appearance();
        appearance.font = FontId::proportional(14.0);
        appearance.width = 32.0;
        let layouts = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        let galley = &layouts[0].galley;
        assert_eq!(galley.rows.len(), 6);
        let mut navigation = None;
        let mut errors = Vec::new();
        let mut previous_row = 0;
        for (key, expected_row, scalar) in [
            (Key::ArrowDown, 1, 2),
            (Key::ArrowDown, 4, 18),
            (Key::ArrowDown, 5, 19),
            (Key::ArrowUp, 4, 18),
            (Key::ArrowUp, 1, 2),
            (Key::ArrowUp, 0, 0),
        ] {
            assert!(handle_key(
                &mut editor,
                key,
                Modifiers::NONE,
                &layouts,
                &mut navigation,
                &mut errors,
                false,
            ));
            let focus = editor.selection().focus;
            editor.document().validate_position(focus).unwrap();
            assert_eq!(
                focus,
                Position::new(0, source.byte_from_scalar(scalar).unwrap())
            );
            let cursor = CCursor {
                index: CharIndex(scalar),
                prefer_next_row: navigation.as_ref().unwrap().prefer_next_row,
            };
            let row = galley.layout_from_cursor(cursor).row;
            assert_eq!(row, expected_row);
            assert!(if key == Key::ArrowDown {
                row > previous_row
            } else {
                row < previous_row
            });
            previous_row = row;
        }
        assert!(errors.is_empty());
        assert_eq!(editor.selection(), Selection::caret(Position::default()));
    }

    #[test]
    fn read_only_vertical_selection_extends_and_external_or_horizontal_movement_resets_column() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("aaaaaaaaa\nx\naaaaaaaaa");
        let anchor = Position::new(0, 8);
        editor.set_selection(Selection::caret(anchor)).unwrap();
        for focus in [Position::new(1, 1), Position::new(2, 8)] {
            frame_options(
                &context,
                &mut editor,
                vec![key(Key::ArrowDown, Modifiers::SHIFT)],
                true,
                true,
            );
            assert_eq!(editor.selection(), Selection::new(anchor, focus));
        }
        editor
            .set_selection(Selection::caret(Position::new(1, 0)))
            .unwrap();
        frame_options(
            &context,
            &mut editor,
            vec![key(Key::ArrowDown, Modifiers::NONE)],
            true,
            true,
        );
        assert_eq!(editor.selection().focus, Position::new(2, 0));

        editor.set_selection(Selection::caret(anchor)).unwrap();
        frame_options(
            &context,
            &mut editor,
            vec![
                key(Key::ArrowDown, Modifiers::NONE),
                key(Key::ArrowLeft, Modifiers::NONE),
                key(Key::ArrowDown, Modifiers::NONE),
            ],
            true,
            true,
        );
        assert_eq!(editor.selection().focus, Position::new(2, 0));
        assert_eq!(editor.document().plain_text(), "aaaaaaaaa\nx\naaaaaaaaa");
        assert!(!editor.can_undo());
    }

    #[test]
    fn empty_preedit_then_empty_commit_deletes_replacement() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("selected");
        editor.select_all();
        frame(
            &context,
            &mut editor,
            vec![
                Event::Ime(ImeEvent::Preedit {
                    text: "candidate".into(),
                    active_range_chars: None,
                }),
                Event::Ime(ImeEvent::Preedit {
                    text: "".into(),
                    active_range_chars: None,
                }),
                Event::Ime(ImeEvent::Commit("".into())),
            ],
            true,
        );
        assert_eq!(editor.document().plain_text(), "");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "selected");
    }

    #[test]
    fn canceled_empty_preedit_allows_typing_to_resume() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("");
        frame(
            &context,
            &mut editor,
            vec![
                Event::Ime(ImeEvent::Preedit {
                    text: "候".into(),
                    active_range_chars: None,
                }),
                Event::Ime(ImeEvent::Preedit {
                    text: "".into(),
                    active_range_chars: None,
                }),
                Event::Text("text".into()),
            ],
            true,
        );
        assert_eq!(editor.document().plain_text(), "text");
        assert!(editor.composition().is_none());
    }

    #[test]
    fn ime_preview_replaces_selected_text_reflows_and_reuses_document() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("before OLD after");
        editor
            .set_selection(Selection::new(Position::new(0, 7), Position::new(0, 10)))
            .expect("valid selection");
        let revision = editor.document().revision();
        let preedit = "candidate ".repeat(80);
        frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::Preedit {
                text: preedit.clone(),
                active_range_chars: None,
            })],
            true,
        );
        let id = context
            .memory(|memory| memory.focused())
            .expect("focused editor");
        let first = context
            .data(|data| data.get_temp::<Cache>(id))
            .expect("cached widget");
        let preview = first.preview.expect("transient composition document");
        assert_eq!(
            preview.document.plain_text(),
            format!("before {preedit} after")
        );
        assert!(!preview.document.plain_text().contains("OLD"));
        assert_eq!(editor.document().plain_text(), "before OLD after");
        assert_eq!(editor.document().revision(), revision);
        assert!(!editor.can_undo());
        assert!(
            preview.render_cache.paragraphs[0]
                .galley
                .job
                .sections
                .iter()
                .any(|section| section.format.underline.width > 0.0)
        );
        let preview_paragraph = preview.document.paragraphs()[0].clone();
        frame(&context, &mut editor, vec![], true);
        let second = context
            .data(|data| data.get_temp::<Cache>(id))
            .expect("cached widget");
        assert!(Arc::ptr_eq(
            &preview_paragraph,
            &second
                .preview
                .expect("retained preview")
                .document
                .paragraphs()[0]
        ));
        assert_eq!(editor.document().revision(), revision);
    }

    #[test]
    fn ime_cursor_is_hidden_when_preedit_range_is_none() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("");
        let hidden = frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::Preedit {
                text: "に".into(),
                active_range_chars: None,
            })],
            true,
        );
        let vertical_lines = |output: &egui::FullOutput| {
            output.shapes.iter().filter(|shape| {
            matches!(&shape.shape, egui::epaint::Shape::LineSegment { points, .. } if points[0].x == points[1].x)
        }).count()
        };
        assert_eq!(vertical_lines(&hidden), 0);
        let visible = frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::Preedit {
                text: "に".into(),
                active_range_chars: Some(1..1),
            })],
            true,
        );
        assert_eq!(vertical_lines(&visible), 1);
    }

    #[test]
    fn surrounding_deletion_expands_graphemes_and_undo_restores_caret() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("e\u{301}!\nnext");
        let original = Selection::caret(Position::new(0, "e\u{301}".len()));
        editor.set_selection(original).expect("valid caret");
        frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::DeleteSurrounding {
                before_chars: 1,
                after_chars: 0,
            })],
            true,
        );
        assert_eq!(editor.document().plain_text(), "!\nnext");
        assert!(editor.undo());
        assert_eq!(editor.selection(), original);
        editor
            .set_selection(Selection::caret(Position::new(1, 0)))
            .expect("valid paragraph caret");
        frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::DeleteSurrounding {
                before_chars: 1,
                after_chars: 0,
            })],
            true,
        );
        assert_eq!(editor.document().plain_text(), "e\u{301}!next");
    }

    #[test]
    fn surrounding_offsets_cross_empty_paragraphs_and_clamp_large_counts() {
        let editor = Editor::from_text("aé\n\ne\u{301}👩🏽‍💻!\nlast");
        let document = editor.document();
        let focus = Position::new(2, "e\u{301}👩🏽‍💻".len());
        for (count, expected) in [
            (0, focus),
            (1, Position::new(2, "e\u{301}".len())),
            (4, Position::new(2, "e\u{301}".len())),
            (5, Position::new(2, 0)),
            (6, Position::new(2, 0)),
            (7, Position::new(1, 0)),
            (8, Position::new(0, "aé".len())),
            (9, Position::new(0, 1)),
            (usize::MAX, Position::default()),
        ] {
            assert_eq!(
                surrounding_position(document, focus, count, false),
                expected
            );
        }
        let focus = Position::new(0, 1);
        for (count, expected) in [
            (0, focus),
            (1, Position::new(0, "aé".len())),
            (2, Position::new(1, 0)),
            (3, Position::new(2, 0)),
            (4, Position::new(2, "e\u{301}".len())),
            (5, Position::new(2, "e\u{301}".len())),
            (6, Position::new(2, "e\u{301}👩🏽‍💻".len())),
            (usize::MAX, document.end()),
        ] {
            assert_eq!(surrounding_position(document, focus, count, true), expected);
        }
    }

    #[test]
    fn surrounding_deletion_uses_selection_focus_and_restores_it_on_undo() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("prefix\n\né!\nsuffix");
        let selection = Selection::new(Position::new(3, 3), Position::new(2, 0));
        editor.set_selection(selection).unwrap();
        frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::DeleteSurrounding {
                before_chars: 2,
                after_chars: 1,
            })],
            true,
        );
        assert_eq!(editor.document().plain_text(), "prefix!\nsuffix");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "prefix\n\né!\nsuffix");
        assert_eq!(editor.selection(), selection);
    }

    #[test]
    fn accessibility_exports_text_runs_and_applies_selection_action() {
        use egui::accesskit::{Action, ActionData, ActionRequest, Role, TextSelection, TreeId};
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut editor = Editor::from_text("first\nsecond");
        let output = frame(&context, &mut editor, vec![], true);
        let update = output
            .platform_output
            .accesskit_update
            .expect("accessibility tree");
        let (root_id, node) = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::MultilineTextInput)
            .expect("editor node");
        let caret = node.text_selection().expect("native text selection").focus;
        assert!(
            update
                .nodes
                .iter()
                .any(|(id, node)| *id == caret.node && node.role() == Role::TextRun)
        );
        let second_run = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::TextRun && node.value() == Some("second"))
            .expect("second paragraph");
        let focus = egui::accesskit::TextPosition {
            node: second_run.0,
            character_index: 3,
        };
        frame(
            &context,
            &mut editor,
            vec![Event::AccessKitActionRequest(ActionRequest {
                action: Action::SetTextSelection,
                target_node: *root_id,
                target_tree: TreeId::ROOT,
                data: Some(ActionData::SetTextSelection(TextSelection {
                    anchor: caret,
                    focus,
                })),
            })],
            true,
        );
        assert_eq!(editor.selection().focus, Position::new(1, 3));
        assert_eq!(editor.selected_text(), "first\nsec");
    }

    #[test]
    fn accessibility_emoji_selection_and_replacement_use_graphemes() {
        use egui::accesskit::{Action, ActionData, ActionRequest, Role, TextSelection, TreeId};
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut editor = Editor::from_text("a👩🏽‍💻e\u{301}");
        let output = frame(&context, &mut editor, vec![], true);
        let update = output
            .platform_output
            .accesskit_update
            .expect("accessibility tree");
        let (root_id, _) = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::MultilineTextInput)
            .expect("editor node");
        let (run_id, run) = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::TextRun)
            .expect("grapheme run");
        assert_eq!(run.character_lengths(), &[1, 15, 3]);
        let anchor = egui::accesskit::TextPosition {
            node: *run_id,
            character_index: 1,
        };
        let focus = egui::accesskit::TextPosition {
            node: *run_id,
            character_index: 2,
        };
        frame(
            &context,
            &mut editor,
            vec![Event::AccessKitActionRequest(ActionRequest {
                action: Action::SetTextSelection,
                target_node: *root_id,
                target_tree: TreeId::ROOT,
                data: Some(ActionData::SetTextSelection(TextSelection {
                    anchor,
                    focus,
                })),
            })],
            true,
        );
        assert_eq!(editor.selected_text(), "👩🏽‍💻");
        frame(
            &context,
            &mut editor,
            vec![Event::AccessKitActionRequest(ActionRequest {
                action: Action::ReplaceSelectedText,
                target_node: *root_id,
                target_tree: TreeId::ROOT,
                data: Some(ActionData::Value("x".into())),
            })],
            true,
        );
        assert_eq!(editor.document().plain_text(), "axe\u{301}");
        assert!(editor.undo());
        assert_eq!(editor.selected_text(), "👩🏽‍💻");
    }
}
