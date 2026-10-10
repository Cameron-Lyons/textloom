//! An egui widget that renders the document's rich spans directly.
//!
//! Use a stable [`RichTextEditor::id_salt`] when the widget can move in the UI.
//! The host's egui integration provides the clipboard, IME, and platform accessibility.
//! For a true bold face, register one in `FontDefinitions` and select it with
//! [`RichTextEditor::bold_font_family`]. Default fonts use egui's strong text color.

use crate::{Document, Editor, Error, Fragment, Position, Selection};
#[cfg(test)]
use crate::{ParagraphKind, StylePatch};
#[cfg(test)]
use egui::{Color32, ImeEvent, Key, Modifiers, Stroke, text::CCursor};
use egui::{
    Event, FontFamily, FontId, Id, Pos2, Rect, Response, Sense, TextStyle, Ui, Vec2, Widget,
    text::CharIndex,
};
#[cfg(test)]
use std::ops::Range;
use std::sync::Arc;
#[cfg(test)]
use unicode_segmentation::UnicodeSegmentation;

mod accessibility;
mod composition;
mod input;
mod layout;
mod navigation;
mod paint;
pub use accessibility::AccessibilityError;
use composition::CompositionPreview;
use input::{handle_event, selected_paragraphs};
#[cfg(test)]
use input::{handle_key, selection_contains_list, surrounding_position};
#[cfg(test)]
use layout::paragraph_job;
use layout::{Appearance, LayoutCache, ParagraphLayout, ParagraphLayouts};
use navigation::{
    VerticalNavigation, document_caret_rect, hit_test, pointer_navigation, word_selection,
};
#[cfg(test)]
use navigation::{preedit_byte_range, snap_grapheme};
use paint::paint_document_selection;

/// Optional host transport for rich clipboard representations.
///
/// The host owns native clipboard access and any transport errors. These methods
/// run synchronously when a focused, enabled widget handles a clipboard event,
/// preserving its order relative to typing, selection, and IME input. Defaults
/// retain egui's plain-text clipboard behavior, so a host can implement either
/// direction independently.
pub trait RichClipboard {
    /// Publish a selected fragment for copy or cut, before cut removes text.
    ///
    /// Use [`Fragment::to_bytes`], [`Fragment::to_html`], and
    /// [`Fragment::plain_text`] to provide native, HTML, and plain representations.
    /// Return `true` only when the host has published the clipboard, including a
    /// plain-text alternative. This suppresses egui's `CopyText` command, which
    /// would otherwise overwrite the host's rich representations. Return `false`
    /// to let egui copy the selected plain text instead.
    /// Successful publication also discards earlier queued egui text/image copy
    /// commands, so deferred writes cannot replace the newer rich clipboard item.
    /// Unrelated commands and clipboard writes queued afterward remain intact.
    ///
    /// Caret selections do not call this method. Read-only cut calls it without
    /// removing text. Copy during composition captures the committed document
    /// selection, excluding preedit text.
    fn copy(&mut self, _fragment: &Fragment) -> bool {
        false
    }

    /// Read a rich representation for a nonempty egui paste event.
    ///
    /// `plain_text` is that event's plain-text alternative. Return a decoded
    /// fragment to replace the selection in one undo step, or `None` to insert
    /// the event's plain text. Decode untrusted native data with
    /// [`Fragment::from_bytes`]; an unavailable or rejected format should fall
    /// back to plain text. The fragment must represent this paste event's
    /// clipboard item. Hosts can capture rich and plain data together when
    /// producing the event; a later clipboard read may retrieve a different item.
    /// Comparing plain text can reject mismatches but cannot distinguish equal-text
    /// items with different formatting. Do not infer clipboard identity from a
    /// previously copied string.
    ///
    /// Empty paste events and read-only widgets do not call this method. Active
    /// nonempty IME preedit blocks paste; an empty preedit is canceled first.
    /// Egui integrations may emit no paste event when native plain text is absent
    /// or empty, so rich clipboard writers must also publish plain text.
    fn paste(&mut self, _plain_text: &str) -> Option<Fragment> {
        None
    }
}

/// A reusable, multiline rich-text editor with pointer and keyboard selection.
///
/// Place it in `ScrollArea::vertical()` for a scrolling document. The core owns
/// document and history state; egui owns focus and cached paragraph layouts.
/// Clipboard interchange uses plain text by default; [`Self::rich_clipboard`]
/// lets the host provide rich formats while retaining a plain-text alternative.
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
    rich_clipboard: Option<&'a mut dyn RichClipboard>,
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
            rich_clipboard: None,
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

    /// Use the host's rich clipboard transport for this frame.
    ///
    /// The widget applies normal focus, read-only, IME, and undo rules before
    /// calling the transport. Without this hook, clipboard events remain plain
    /// text and do not capture rich fragments. Native MIME transport and its
    /// lifetime remain the host's responsibility.
    pub fn rich_clipboard(mut self, clipboard: &'a mut dyn RichClipboard) -> Self {
        self.rich_clipboard = Some(clipboard);
        self
    }

    /// Process focused input, render the document, and return interaction errors.
    pub fn show(mut self, ui: &mut Ui) -> RichTextEditorOutput {
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
        let old_composition = self.editor.composition_snapshot();
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
            .content_height()
            .unwrap_or(font.size);
        let (_, rect) = ui.allocate_space(Vec2::new(
            width,
            height.max(font.size * self.min_rows as f32) + PADDING * 2.0,
        ));
        let mut response = ui.interact(rect, id, Sense::click_and_drag());
        cache.observe_focus(self.editor, ui.is_enabled() && response.has_focus());
        let origin = rect.min + Vec2::splat(PADDING);

        if ui.is_enabled() {
            if response.clicked() || response.drag_started() {
                cache.interaction.navigation = None;
                response.request_focus();
                if let Some(pointer) = response.interact_pointer_pos() {
                    interrupted |= self.editor.cancel_composition();
                    let (position, prefer_next_row) =
                        hit_test(self.editor, &layouts, pointer - origin);
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
                    cache.interaction.navigation =
                        pointer_navigation(self.editor, &layouts, position, prefer_next_row);
                }
            }
            if response.dragged()
                && let Some(pointer) = response.interact_pointer_pos()
            {
                cache.interaction.navigation = None;
                interrupted |= self.editor.cancel_composition();
                let (position, prefer_next_row) = hit_test(self.editor, &layouts, pointer - origin);
                record(
                    self.editor
                        .set_selection(Selection::new(self.editor.selection().anchor, position)),
                    &mut errors,
                );
                cache.interaction.navigation =
                    pointer_navigation(self.editor, &layouts, position, prefer_next_row);
            }

            cache.observe_focus(self.editor, response.has_focus());
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
            }
            let process_events = response.has_focus()
                || ui.input(|input| {
                    input.events.iter().any(|event| {
                        matches!(event, Event::AccessKitActionRequest(request)
                            if request.target_node == id.accesskit_id()
                                && request.target_tree == egui::accesskit::TreeId::ROOT)
                    })
                });
            let events = if process_events {
                ui.input(|input| input.events.clone())
            } else {
                Vec::new()
            };
            let mut consumed = Vec::new();
            for (index, event) in events.iter().enumerate() {
                let revision = self.editor.document().revision();
                let handled = if let Event::AccessKitActionRequest(request) = event
                    && request.target_node == id.accesskit_id()
                    && request.target_tree == egui::accesskit::TreeId::ROOT
                {
                    handle_accessibility_action(
                        self.editor,
                        request,
                        cache.accessibility.as_deref(),
                        self.read_only,
                        &mut interrupted,
                        &mut errors,
                        &mut accessibility_errors,
                    )
                } else if response.has_focus() {
                    handle_event(
                        self.editor,
                        ui,
                        event,
                        &layouts,
                        &mut cache.interaction.navigation,
                        &mut errors,
                        &mut interrupted,
                        self.read_only,
                        &mut self.rich_clipboard,
                    )
                } else {
                    false
                };
                if handled {
                    consumed.push(index);
                }
                if revision != self.editor.document().revision() {
                    layouts = cache.layout(ui, self.editor, &appearance);
                }
                cache.validate_navigation(self.editor.selection());
            }
            // Keep unrelated events in their original order for the host.
            if !consumed.is_empty() {
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
            }
            if !response.has_focus() {
                interrupted |= self.editor.cancel_composition();
            }
        }

        cache.observe_focus(self.editor, ui.is_enabled() && response.has_focus());
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
        let composition_changed = match (old_composition.as_deref(), self.editor.composition()) {
            (Some(before), Some(after)) => !std::ptr::eq(before, after) && before != after,
            (None, None) => false,
            _ => true,
        };
        if composition_changed {
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
        let visible = ui.is_rect_visible(rect);
        if visible {
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
            let first_visible = display_layouts.first_bottom_at_least(top);
            let last_visible = display_layouts.first_top_after(bottom).max(first_visible);
            for paragraph in display_layouts.views_range(first_visible..last_visible) {
                let text_origin = origin + paragraph.rect.min.to_vec2();
                if !painter
                    .clip_rect()
                    .intersects(paragraph.rect.translate(origin.to_vec2()))
                {
                    continue;
                }
                if let Some(marker) = paragraph.marker {
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
        }
        if response.has_focus() {
            let caret_position =
                preview.map_or(self.editor.selection().focus, |preview| preview.caret);
            let prefer_next_row = preview.is_some()
                || cache
                    .interaction
                    .navigation
                    .as_ref()
                    .is_none_or(|navigation| navigation.prefer_next_row);
            let local_caret = document_caret_rect(
                display_document,
                display_layouts,
                Pos2::ZERO,
                caret_position,
                prefer_next_row,
            );
            let caret = local_caret.translate(origin.to_vec2());
            let show_caret = self
                .editor
                .composition()
                .map_or(self.editor.selection().is_caret(), |composition| {
                    composition.selection.is_some()
                });
            if visible && show_caret {
                painter.line_segment(
                    [caret.left_top(), caret.left_bottom()],
                    ui.visuals().text_cursor.stroke,
                );
            }
            if cache.interaction.caret_rect != Some(local_caret)
                || cache.interaction.scroll_to_caret
                || old_selection != self.editor.selection()
                || response.changed()
                || composition_changed
            {
                // Scroll even if the whole widget is clipped. Input can grow
                // the displayed document after its space was allocated; repeat
                // the request next frame after the new content height is known.
                ui.scroll_to_rect(caret.expand(4.0), None);
                cache.interaction.scroll_to_caret = display_layouts
                    .content_height()
                    .is_some_and(|content_height| content_height > height);
                if cache.interaction.scroll_to_caret {
                    ui.ctx().request_repaint();
                }
            }
            cache.interaction.caret_rect = Some(local_caret);
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
        } else {
            cache.interaction.scroll_to_caret = false;
        }

        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        let range = self.editor.selection().range();
        let char_start = cache.char_offset(self.editor.document(), range.start);
        let char_end = if range.is_empty() {
            char_start
        } else {
            cache.char_offset(self.editor.document(), range.end)
        };
        let char_range = CharIndex(char_start)..CharIndex(char_end);
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
            if let Err(error) =
                accessibility::update_cached(self.editor, &layouts, id, &mut cache.accessibility)
            {
                cache.accessibility = None;
                accessibility_errors.push(error);
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

#[derive(Clone, Default)]
struct Cache {
    layout: LayoutCache,
    plain_text: Option<Arc<str>>,
    preview: Option<Box<CompositionPreview>>,
    accessibility: Option<Arc<accessibility::Snapshot>>,
    interaction: InteractionState,
}

#[derive(Clone, Default)]
struct InteractionState {
    navigation: Option<VerticalNavigation>,
    caret_rect: Option<Rect>,
    scroll_to_caret: bool,
    focused: bool,
}

impl Cache {
    fn observe_focus(&mut self, editor: &mut Editor, focused: bool) {
        if self.interaction.focused && !focused {
            editor.break_history_group();
        }
        self.interaction.focused = focused;
    }
    fn validate_navigation(&mut self, selection: Selection) {
        if self
            .interaction
            .navigation
            .as_ref()
            .is_some_and(|navigation| navigation.selection != selection)
        {
            self.interaction.navigation = None;
        }
    }
    fn layout(&mut self, ui: &Ui, editor: &Editor, appearance: &Appearance) -> ParagraphLayouts {
        let (layouts, document_changed, layout_changed) =
            self.layout.update(ui, editor.document(), appearance);
        if document_changed {
            self.plain_text = None;
            self.preview = None;
        }
        if layout_changed {
            self.interaction.navigation = None;
        }
        layouts
    }
    fn plain_text(&mut self, document: &Document) -> Arc<str> {
        self.plain_text
            .get_or_insert_with(|| Arc::from(document.plain_text()))
            .clone()
    }
    fn char_offset(&self, document: &Document, position: Position) -> usize {
        self.layout.layouts.char_offset(position.paragraph)
            + document
                .paragraph(position.paragraph)
                .map_or(0, |paragraph| {
                    paragraph
                        .scalar_index(position.byte)
                        .unwrap_or(paragraph.scalar_count())
                })
    }
    fn selection_contains_list(&self, editor: &Editor) -> bool {
        self.layout
            .layouts
            .contains_list(selected_paragraphs(editor))
    }
}

fn record(result: Result<(), Error>, errors: &mut Vec<Error>) {
    if let Err(error) = result {
        errors.push(error);
    }
}

fn handle_accessibility_action(
    editor: &mut Editor,
    request: &egui::accesskit::ActionRequest,
    snapshot: Option<&accessibility::Snapshot>,
    read_only: bool,
    interrupted: &mut bool,
    errors: &mut Vec<Error>,
    accessibility_errors: &mut Vec<AccessibilityError>,
) -> bool {
    use egui::accesskit::{Action, ActionData};
    if read_only
        && matches!(
            request.action,
            Action::SetValue | Action::ReplaceSelectedText
        )
    {
        return true;
    }
    if !matches!(
        request.action,
        Action::SetTextSelection | Action::SetValue | Action::ReplaceSelectedText
    ) {
        return false;
    }
    let Some(snapshot) = snapshot.filter(|snapshot| snapshot.matches_document(editor)) else {
        accessibility_errors.push(AccessibilityError::StaleDocument);
        return true;
    };
    match (&request.action, &request.data) {
        (Action::SetTextSelection, Some(ActionData::SetTextSelection(selection))) => {
            if let (Some(anchor), Some(focus)) = (
                snapshot.decode(selection.anchor),
                snapshot.decode(selection.focus),
            ) {
                *interrupted |= editor.cancel_composition();
                record(editor.set_selection(Selection::new(anchor, focus)), errors);
            } else {
                accessibility_errors.push(AccessibilityError::InvalidTextPosition);
            }
        }
        (Action::SetValue | Action::ReplaceSelectedText, Some(ActionData::Value(value))) => {
            *interrupted |= editor.cancel_composition();
            editor.break_history_group();
            let result = if request.action == Action::SetValue {
                editor.replace_range(Position::default()..editor.document().end(), value)
            } else {
                editor.insert_text(value)
            };
            record(result, errors);
            editor.break_history_group();
        }
        _ => accessibility_errors.push(AccessibilityError::InvalidActionData),
    }
    true
}

#[cfg(test)]
mod tests;
