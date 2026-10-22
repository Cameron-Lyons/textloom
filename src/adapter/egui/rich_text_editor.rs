//! The [`RichTextEditor`] widget and the per-widget state it keeps between frames.

use super::accessibility::AccessibilityError;
use super::composition::CompositionPreview;
use super::input::InputOptions;
use super::layout::{Appearance, LayoutCache, ParagraphLayouts};
use super::navigation::{CaretPlacement, VerticalNavigation};
use super::paint::{BackgroundRanges, PaintCache};
use super::{accessibility, input, input_routing, navigation, paint};
use crate::adapter::{self, ControlCharacters};
use crate::{Composition, Document, Editor, Error, Fragment, Link, Position, Selection};
use egui::text::{CharIndex, LayoutJob};
use egui::{
    Color32, Event, FontFamily, FontId, Galley, Id, ImeEvent, Pos2, Rect, Response, Sense,
    TextFormat, TextStyle, Ui, Vec2, Widget,
};
use std::fmt::{self, Debug, Formatter};
use std::ops::Range;
use std::ptr;
use std::sync::Arc;

/// Exchanges rich [`Fragment`]s with the host's clipboard.
///
/// egui's clipboard carries only plain text. Attach an implementation with
/// [`RichTextEditor::rich_clipboard`] to also publish and read rich formats,
/// for example TLFR, HTML, and plain text from the fragment's export methods.
/// Both methods have defaults that keep egui's plain-text behavior. The widget
/// calls them in input order while it handles input, and follows its read-only
/// and IME rules: a read-only cut copies without deleting, a read-only widget
/// ignores pastes, and a nonempty preedit blocks paste and cut. Native clipboard
/// access and its errors remain the host's responsibility.
pub trait RichClipboard {
    /// Called when the user copies or cuts a nonempty selection, with that
    /// selection.
    ///
    /// Return `true` after placing `fragment` on the clipboard together with a
    /// plain-text alternative: the widget then removes the text and image copy
    /// commands already queued in egui's output for this frame, so that they
    /// cannot overwrite the newer clipboard item. Return `false`, the default,
    /// to have the widget copy the selection's plain text through egui.
    fn copy(&mut self, _fragment: &Fragment) -> bool {
        false
    }

    /// Called for each paste event with the text egui pasted, unless it is a
    /// plain-text paste (Command/Ctrl+Shift+V, which egui also delivers as a
    /// paste) or contains nothing but control characters.
    ///
    /// Return the fragment this paste carries to insert it in place of the
    /// selection as one undo step, or `None`, the default, to insert
    /// `plain_text` without its control characters other than line breaks and
    /// tabs. Rich data must belong to this paste event: capture the native rich
    /// and plain representations together when producing the event, because
    /// equal text cannot tell apart clipboard items with different formatting.
    fn paste(&mut self, _plain_text: &str) -> Option<Fragment> {
        None
    }
}

/// An egui widget that displays and edits an [`Editor`].
///
/// Create it with [`new`](Self::new) every frame, configure it with the
/// builder methods, and add it with [`show`](Self::show), which returns a
/// [`RichTextEditorOutput`], or with `ui.add(...)`, which returns only the
/// [`Response`]. The document, selection, and undo history live in the
/// [`Editor`]; the widget keeps layouts, IME tracking, and its accessibility
/// tree in egui's temporary memory under its ID until
/// [`forget_state`](Self::forget_state) releases them. Give each independently
/// stored editor a stable ID with [`id`](Self::id) or [`id_salt`](Self::id_salt).
///
/// The widget applies pointer input, AccessKit actions, and, while it has
/// focus, keyboard, clipboard, and IME input to the editor in input order. It
/// never opens links: it reports activated ones in
/// [`RichTextEditorOutput::clicked_link`]. For a scrolling editor, place it
/// inside an egui `ScrollArea`.
///
/// `'editor` is the lifetime of the borrowed editor, highlight ranges, and
/// rich clipboard.
///
/// # Examples
///
/// ```
/// use textloom::Editor;
/// use textloom::adapter::egui::{RichTextEditor, install_input_routing};
///
/// let mut editor = Editor::from_text("Hello, world");
/// let ctx = egui::Context::default();
/// install_input_routing(&ctx);
/// let matches = editor.document().find("world", Default::default());
/// let output = ctx.run_ui(egui::RawInput::default(), |ui| {
///     egui::CentralPanel::default().show(ui, |ui| {
///         let output = RichTextEditor::new(&mut editor)
///             .id_salt("notes")
///             .hint_text("Write something…")
///             .highlight_ranges(&matches, egui::Color32::YELLOW)
///             .show(ui);
///         assert!(output.errors.is_empty());
///         if let Some(link) = output.clicked_link {
///             println!("open {}", link.as_str());
///         }
///     });
/// });
/// output.drop_without_applying_deltas();
/// ```
pub struct RichTextEditor<'editor> {
    editor: &'editor mut Editor,
    id: Option<Id>,
    salt: Option<egui::IdSalt>,
    desired_width: Option<f32>,
    min_rows: usize,
    font: Option<FontId>,
    bold_family: Option<FontFamily>,
    hint: String,
    read_only: bool,
    heading_shortcuts: bool,
    highlights: Vec<BackgroundRanges<'editor>>,
    rich_clipboard: Option<&'editor mut dyn RichClipboard>,
}

impl Debug for RichTextEditor<'_> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RichTextEditor")
            .field("id", &self.id)
            .field("salt", &self.salt)
            .field("desired_width", &self.desired_width)
            .field("min_rows", &self.min_rows)
            .field("font", &self.font)
            .field("bold_family", &self.bold_family)
            .field("hint", &self.hint)
            .field("read_only", &self.read_only)
            .field("heading_shortcuts", &self.heading_shortcuts)
            .field("highlights", &self.highlights)
            .field("rich_clipboard", &self.rich_clipboard.is_some())
            .finish_non_exhaustive()
    }
}

/// What [`RichTextEditor::show`] reports about one frame.
///
/// Every field is computed each frame, also while the widget is unfocused.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RichTextEditorOutput {
    /// The widget's response. [`Response::changed`] reports whether the widget
    /// changed the document this frame.
    pub response: Response,
    /// Errors returned while applying this frame's input to the editor or
    /// previewing its IME composition, in the order they occurred.
    pub errors: Vec<Error>,
    /// Problems publishing the accessibility tree or applying AccessKit
    /// actions this frame.
    pub accessibility_errors: Vec<AccessibilityError>,
    /// The caret's rectangle in the coordinates of the `Ui` the widget was
    /// shown in, inside the preedit during an IME composition. Pass it to
    /// `ui.scroll_to_rect` after moving the selection programmatically, or
    /// anchor pop-ups to it.
    pub caret_rect: Rect,
    /// The bounds of the selected text in the same coordinates, or `None` when
    /// nothing is selected. A selection spanning several rows covers the full
    /// text width.
    pub selection_rect: Option<Rect>,
    /// The link activated this frame, by a click that presses and releases on
    /// the same link, with Command/Ctrl held unless the widget is read-only, or
    /// by an AccessKit click action on it. The widget does not open it.
    pub clicked_link: Option<Link>,
}

impl<'editor> RichTextEditor<'editor> {
    /// Creates a widget for `editor` that is editable, has heading shortcuts,
    /// fills the available width, shows at least four rows, and uses the
    /// `Ui`'s body text style.
    #[must_use]
    pub fn new(editor: &'editor mut Editor) -> Self {
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
            heading_shortcuts: true,
            highlights: Vec::new(),
            rich_clipboard: None,
        }
    }

    /// Uses `id` as the widget's ID, under which egui also stores its state.
    ///
    /// It takes precedence over [`id_salt`](Self::id_salt). Without either,
    /// the widget takes the `Ui`'s next automatic ID, which changes when
    /// widgets before it come and go.
    #[must_use]
    pub fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    /// Derives the widget's ID from `salt` and the parent `Ui`'s ID, as
    /// `Ui::make_persistent_id` does.
    #[must_use]
    pub fn id_salt(mut self, salt: impl egui::AsIdSalt) -> Self {
        self.salt = Some(egui::IdSalt::new(salt));
        self
    }

    /// Sets the widget's width in points, including its padding.
    ///
    /// The width is at least 32 points and never exceeds the available width,
    /// which is also the default.
    #[must_use]
    pub fn desired_width(mut self, width: f32) -> Self {
        self.desired_width = Some(width.max(32.0));
        self
    }

    /// Sets the number of body text rows the widget is tall enough for while
    /// the document is shorter, at least one; the default is four. Longer
    /// documents make the widget taller.
    #[must_use]
    pub fn min_rows(mut self, rows: usize) -> Self {
        self.min_rows = rows.max(1);
        self
    }

    /// Sets the body text font, which headings scale up. The default is the
    /// `Ui`'s body text style.
    #[must_use]
    pub fn font(mut self, font: FontId) -> Self {
        self.font = Some(font);
        self
    }

    /// Selects a registered font family for bold glyphs.
    ///
    /// Bold text without a color of its own uses egui's strong text color.
    /// Without this family, or when `family` is not registered with egui, bold
    /// text keeps the body font's family.
    #[must_use]
    pub fn bold_font_family(mut self, family: FontFamily) -> Self {
        self.bold_family = Some(family);
        self
    }

    /// Sets placeholder text that appears in the weak text color while the
    /// document is a single empty paragraph and no IME composition is active.
    /// The widget's egui `WidgetInfo` also reports it as the hint text.
    #[must_use]
    pub fn hint_text(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    /// Makes the widget read-only, or editable again.
    ///
    /// A read-only widget keeps selection, navigation, copy, and accessibility
    /// but ignores edits and IME, cancels any composition, activates links on a
    /// plain click, and leaves Tab to focus traversal. It consumes only the
    /// editing chords an editable widget would handle.
    #[must_use]
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Enables or disables the Command/Ctrl+0 (body text) and Command/Ctrl+1–6
    /// (heading) shortcuts. Disable them to leave these chords to the host, for
    /// example to egui's zoom reset. They are enabled by default.
    #[must_use]
    pub fn heading_shortcuts(mut self, enabled: bool) -> Self {
        self.heading_shortcuts = enabled;
        self
    }

    /// Paints `color` behind the text in `ranges`, such as the results of
    /// [`Document::find`].
    ///
    /// `ranges` must be sorted and nonoverlapping, as `find` returns them;
    /// otherwise some may not be painted. Ranges that are empty or do not lie
    /// on character boundaries of the document are skipped. Each call adds a
    /// layer over the previous ones, and the selection is painted over all of
    /// them. Highlights are hidden while an IME preedit is shown.
    #[must_use]
    pub fn highlight_ranges(mut self, ranges: &'editor [Range<Position>], color: Color32) -> Self {
        self.highlights.push(BackgroundRanges {
            ranges,
            fill: color,
        });
        self
    }

    /// Exchanges rich fragments with the host's clipboard through `clipboard`
    /// instead of copying and pasting only plain text. See [`RichClipboard`].
    #[must_use]
    pub fn rich_clipboard(mut self, clipboard: &'editor mut dyn RichClipboard) -> Self {
        self.rich_clipboard = Some(clipboard);
        self
    }

    /// Releases the layouts, IME tracking, and accessibility tree that the
    /// widget with `id` keeps in egui's temporary memory, for example when its
    /// editor closes.
    ///
    /// `id` is the widget's ID, which `output.response.id` also reports when
    /// the widget derived it from a salt or the `Ui`. Showing the widget again
    /// rebuilds the state.
    pub fn forget_state(ctx: &egui::Context, id: Id) {
        ctx.data_mut(|data| data.remove::<Cache>(id));
    }

    /// Adds the widget to `ui`, applies this frame's input to the editor, and
    /// paints the document.
    ///
    /// It handles pointer input on the widget, keyboard, clipboard, and IME
    /// input while the widget has focus, and AccessKit actions that target the
    /// widget or its links. A disabled `Ui` shows the document without applying
    /// input and cancels any composition. It also installs
    /// [`install_input_routing`](super::install_input_routing) if the context lacks it, taking effect on the
    /// next pass.
    pub fn show(mut self, ui: &mut Ui) -> RichTextEditorOutput {
        input_routing::install_input_routing(ui.ctx());
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
            link_color: ui.visuals().hyperlink_color,
            width: width - PADDING * 2.0,
        };
        let old_revision = self.editor.document().revision();
        let old_selection = self.editor.selection();
        let old_composition = self.editor.composition_snapshot();
        let was_focused = cache.interaction.focused;
        // A focus request or Shift+Tab after this widget ran moved focus before this pass's input.
        let focus_left = cache.interaction.focus_exit.take().is_some_and(|exit| {
            ui.memory(|memory| match exit {
                FocusExit::Request(target) => memory
                    .focused()
                    .is_some_and(|focused| focused.accesskit_id() == target),
                FocusExit::Traversal => !memory.has_focus(id),
            })
        });
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
        let tab_moves_focus = self.read_only || !cache.selection_contains_list(self.editor);
        cache.validate_navigation(self.editor.selection());
        let old_text = publish_text.then(|| cache.plain_text(self.editor.document()));
        let now = ui.input(|input| input.time);
        let mut interrupted =
            (self.read_only || !ui.is_enabled()) && self.editor.cancel_composition();
        if interrupted && let Some(composition) = &old_composition {
            cache.interaction.remember_composition_end(
                adapter::clean_text(&composition.text, ControlCharacters::KeepWhitespace)
                    .into_owned(),
                now,
            );
        }
        if self
            .editor
            .composition()
            .is_none_or(|composition| !composition.text.is_empty())
        {
            cache.interaction.cleared_preedit = None;
        }
        let initial_preview = record(
            cache.layout_preview(ui, self.editor, &appearance),
            &mut errors,
        )
        .flatten();
        let row_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
        let rows = self.min_rows as f32;
        let hint = self.hint_galley(ui, initial_preview.as_ref().unwrap_or(&layouts));
        let height = initial_preview
            .as_ref()
            .unwrap_or(&layouts)
            .content_height()
            .unwrap_or(row_height)
            .max(hint.as_ref().map_or(0.0, |(_, galley)| galley.size().y));
        let (_, rect) = ui.allocate_space(Vec2::new(
            width,
            height.max(row_height * rows + PARAGRAPH_GAP * (rows - 1.0)) + PADDING * 2.0,
        ));
        let drag_to_select =
            !ui.input(|input| input.has_touch_screen()) || ui.memory(|memory| memory.has_focus(id));
        let sense = if drag_to_select {
            Sense::click_and_drag()
        } else {
            Sense::click()
        };
        let mut response = ui.interact(rect, id, sense);
        response
            .flags
            .remove(egui::response::Flags::FAKE_PRIMARY_CLICKED);
        cache.observe_focus(self.editor, ui.is_enabled() && response.has_focus());
        let origin = rect.min + Vec2::splat(PADDING);
        let mut pointer_interaction = false;

        let mut clicked_link = None;
        if ui.is_enabled() {
            let options = InputOptions {
                read_only: self.read_only,
                page_height: (ui.clip_rect().height() - row_height).max(row_height),
                heading_shortcuts: self.heading_shortcuts,
            };
            let press_event = if !drag_to_select {
                None
            } else if response.is_pointer_button_down_on() {
                primary_pointer_event(ui, PointerEventKind::Press)
            } else if response.clicked() {
                primary_pointer_event(ui, PointerEventKind::ClickPress)
            } else {
                None
            };
            let pressed = press_event.is_some();
            let clicked = response.clicked()
                && (!drag_to_select || response.double_clicked() || response.triple_clicked());
            let pointer = response.interact_pointer_pos();
            let pointer_event = if pressed {
                press_event
            } else if clicked {
                primary_pointer_event(ui, PointerEventKind::Release)
            } else {
                None
            };
            let click_pointer = pointer_event.map(|(position, _)| position).or(pointer);
            let click_modifiers = pointer_event
                .map(|(_, modifiers)| modifiers)
                .unwrap_or_else(|| ui.input(|input| input.modifiers));
            let drag_stopped = response.drag_stopped_by(egui::PointerButton::Primary);
            let released_pointer = if response.clicked() || drag_stopped {
                let event_kind = if drag_stopped {
                    PointerEventKind::DragRelease
                } else {
                    PointerEventKind::Release
                };
                primary_pointer_event(ui, event_kind)
                    .map(|(position, _)| position)
                    .or(pointer)
            } else {
                pointer
            };
            let drag_pointer = if drag_stopped {
                released_pointer
            } else {
                pointer
            };
            let dragged = drag_to_select
                && (response.dragged_by(egui::PointerButton::Primary) || drag_stopped)
                && drag_pointer.is_some();
            if cache
                .interaction
                .commit_echo
                .as_ref()
                .is_some_and(|echo| now - echo.time > COMMIT_ECHO_SECONDS)
            {
                cache.interaction.commit_echo = None;
            }
            let ends_composition = ((pressed || clicked) && click_pointer.is_some())
                || dragged
                || !(pressed || clicked || response.has_focus());
            let follows_links = self.read_only || click_modifiers.command;
            let previous_press = cache.interaction.link_press.take();
            let applied = if was_focused && ends_composition && !focus_left {
                apply_queued_ime(
                    self.editor,
                    ui,
                    &mut cache,
                    &appearance,
                    &mut layouts,
                    &mut errors,
                    &mut interrupted,
                    &options,
                    tab_moves_focus,
                )
            } else {
                Vec::new()
            };
            if pressed || clicked {
                pointer_interaction = true;
                cache.interaction.navigation = None;
                if !response.has_focus() {
                    response.request_focus();
                }
                if let Some(pointer) = click_pointer {
                    if let Some(committed) = finish_composition(self.editor, &mut errors) {
                        interrupted = true;
                        layouts = cache.layout(ui, self.editor, &appearance);
                        cache.interaction.remember_composition_end(committed, now);
                    }
                    let single = !response.double_clicked() && !response.triple_clicked();
                    let link = (follows_links && single)
                        .then(|| {
                            navigation::link_at_point(
                                self.editor.document(),
                                &layouts,
                                pointer - origin,
                            )
                        })
                        .flatten();
                    if let Some(link) = link {
                        if pressed {
                            let anchor =
                                navigation::hit_test(self.editor, &layouts, pointer - origin)
                                    .position;
                            cache.interaction.link_press = Some(LinkPress {
                                link,
                                anchor,
                                revision: self.editor.document().revision(),
                            });
                        } else {
                            clicked_link = Some(link);
                        }
                    } else {
                        let CaretPlacement {
                            position,
                            prefer_next_row,
                        } = navigation::hit_test(self.editor, &layouts, pointer - origin);
                        let shift = click_modifiers.shift;
                        let result = if response.triple_clicked() {
                            self.editor.select_paragraph(position.paragraph)
                        } else if response.double_clicked() {
                            let under_pointer = navigation::grapheme_at_point(
                                self.editor.document(),
                                &layouts,
                                pointer - origin,
                            );
                            self.editor
                                .select_word_at(under_pointer.unwrap_or(position))
                        } else {
                            let anchor = if shift {
                                self.editor.selection().anchor
                            } else {
                                position
                            };
                            self.editor.set_selection(Selection::new(anchor, position))
                        };
                        record(result, &mut errors);
                        cache.interaction.navigation = navigation::pointer_navigation(
                            self.editor,
                            &layouts,
                            position,
                            prefer_next_row,
                        );
                    }
                }
            }
            let mut link_press = if pressed {
                cache.interaction.link_press.take()
            } else {
                previous_press
            };
            if response.clicked()
                && !response.double_clicked()
                && !response.triple_clicked()
                && let Some(press) = link_press.take()
                && press.revision == self.editor.document().revision()
                && let Some(pointer) = released_pointer
                && navigation::link_at_point(self.editor.document(), &layouts, pointer - origin)
                    .as_ref()
                    == Some(&press.link)
            {
                clicked_link = Some(press.link);
            }
            if dragged && let Some(pointer) = drag_pointer {
                pointer_interaction = true;
                cache.interaction.navigation = None;
                if let Some(committed) = finish_composition(self.editor, &mut errors) {
                    interrupted = true;
                    layouts = cache.layout(ui, self.editor, &appearance);
                    cache.interaction.remember_composition_end(committed, now);
                }
                let CaretPlacement {
                    position,
                    prefer_next_row,
                } = navigation::hit_test(self.editor, &layouts, pointer - origin);
                let anchor = link_press
                    .take()
                    .filter(|press| press.revision == self.editor.document().revision())
                    .map_or(self.editor.selection().anchor, |press| press.anchor);
                record(
                    self.editor.set_selection(Selection::new(anchor, position)),
                    &mut errors,
                );
                cache.interaction.navigation = navigation::pointer_navigation(
                    self.editor,
                    &layouts,
                    position,
                    prefer_next_row,
                );
            }
            if response.is_pointer_button_down_on() {
                cache.interaction.link_press = link_press;
            }

            cache.observe_focus(self.editor, response.has_focus());
            if response.has_focus() {
                cache.validate_navigation(self.editor.selection());
                self.set_focus_filter(ui, id, &cache);
            }
            let published = cache.accessibility.clone();
            let link_click = |request: &egui::accesskit::ActionRequest| {
                (request.action == egui::accesskit::Action::Click
                    && request.target_tree == egui::accesskit::TreeId::ROOT)
                    .then(|| published.as_ref()?.link(request.target_node).cloned())
                    .flatten()
            };
            let process_events = response.has_focus()
                || ui.input(|input| {
                    input.events.iter().any(|event| {
                        matches!(event, Event::AccessKitActionRequest(request)
                            if (request.target_node == id.accesskit_id()
                                && request.target_tree == egui::accesskit::TreeId::ROOT)
                                || link_click(request).is_some())
                    })
                });
            let events = if process_events {
                ui.input(|input| input.events.clone())
            } else {
                Vec::new()
            };
            let mut consumed = Vec::new();
            let mut layout_stale = false;
            let mut traversed = false;
            let mut event_modifiers = input_routing::modifiers_at_pass_start(ui.ctx());
            for (index, event) in events.iter().enumerate() {
                match event {
                    Event::ModifiersChanged(modifiers) => event_modifiers = *modifiers,
                    Event::WindowFocused(false) => event_modifiers = egui::Modifiers::NONE,
                    _ => {}
                }
                // egui moves focus for Shift+Tab next pass; a later preedit belongs to that widget,
                // while commits stay here like Text.
                if applied.binary_search(&index).is_ok()
                    || (traversed && matches!(event, Event::Ime(ImeEvent::Preedit { .. })))
                {
                    continue;
                }
                if layout_stale && input::reads_layout(ui.os(), event, &options) {
                    layouts = cache.layout(ui, self.editor, &appearance);
                    layout_stale = false;
                }
                let revision = self.editor.document().revision();
                let composition = composition_before_event(self.editor, event);
                let link = match event {
                    Event::AccessKitActionRequest(request) => link_click(request),
                    _ => None,
                };
                let handled = if let Some(link) = link {
                    if published
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.matches_document(self.editor))
                    {
                        clicked_link = Some(link);
                    } else {
                        accessibility_errors.push(AccessibilityError::StaleDocument);
                    }
                    true
                } else if let Event::AccessKitActionRequest(request) = event
                    && request.target_node == id.accesskit_id()
                    && request.target_tree == egui::accesskit::TreeId::ROOT
                {
                    handle_accessibility_action(
                        self.editor,
                        request,
                        cache.accessibility.as_deref(),
                        &layouts,
                        &mut cache.interaction.navigation,
                        self.read_only,
                        &mut interrupted,
                        &mut errors,
                        &mut accessibility_errors,
                    )
                } else if response.has_focus() {
                    input::handle_event(
                        self.editor,
                        ui,
                        event,
                        event_modifiers,
                        &layouts,
                        &mut cache.interaction.navigation,
                        &mut cache.interaction.commit_echo,
                        &mut errors,
                        &mut interrupted,
                        &options,
                        &mut self.rich_clipboard,
                    )
                } else {
                    false
                };
                if handled {
                    consumed.push(index);
                }
                observe_composition_event(
                    &mut cache.interaction,
                    composition,
                    self.editor,
                    event,
                    now,
                );
                if revision != self.editor.document().revision() {
                    cache.layout.track(self.editor.document());
                    layout_stale = true;
                }
                cache.validate_navigation(self.editor.selection());
                traversed |= response.has_focus()
                    && tab_moves_focus
                    && focus_traversal(event) == Some(egui::FocusDirection::Previous);
            }
            if layout_stale {
                layouts = cache.layout(ui, self.editor, &appearance);
            }
            consumed.extend(applied);
            consumed.sort_unstable();
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
            if response.has_focus() {
                // egui applies only the last focus request of a pass.
                let request = events
                    .iter()
                    .rev()
                    .find_map(input_routing::focus_request_target)
                    .filter(|&target| target != id.accesskit_id());
                cache.interaction.focus_exit = if traversed {
                    Some(FocusExit::Traversal)
                } else {
                    request.map(FocusExit::Request)
                };
                self.set_focus_filter(ui, id, &cache);
            } else if let Some(committed) = finish_composition(self.editor, &mut errors) {
                interrupted = true;
                cache.interaction.remember_composition_end(committed, now);
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
            (Some(before), Some(after)) => !ptr::eq(before, after) && before != after,
            (None, None) => false,
            _ => true,
        };
        if composition_changed {
            ui.ctx().request_repaint();
        }
        let preview_layouts = record(
            cache.layout_preview(ui, self.editor, &appearance),
            &mut errors,
        )
        .flatten();
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
            let highlights = if preview.is_some() {
                &[][..]
            } else {
                &self.highlights[..]
            };
            paint::paint_document(
                &painter,
                &mut cache.paint,
                display_document,
                display_layouts,
                origin,
                highlights,
                display_selection,
                ui.visuals().selection.bg_fill,
                &appearance.font.family,
                appearance.color,
                ui.visuals().weak_text_color(),
            );
            if let Some((position, galley)) = self.hint_galley(ui, display_layouts) {
                let color = ui.visuals().weak_text_color();
                painter.galley(origin + position.to_vec2(), galley, color);
            }
        }
        let caret_position = preview.map_or(self.editor.selection().focus, |preview| preview.caret);
        let prefer_next_row = preview.is_some()
            || cache
                .interaction
                .navigation
                .as_ref()
                .is_none_or(|navigation| navigation.prefer_next_row);
        let local_caret = navigation::document_caret_rect(
            display_document,
            display_layouts,
            Pos2::ZERO,
            caret_position,
            prefer_next_row,
        );
        let caret = local_caret.translate(origin.to_vec2());
        let selection_rect = display_selection.and_then(|selection| {
            navigation::selection_bounds(
                display_document,
                display_layouts,
                origin,
                selection,
                appearance.width,
            )
        });
        if pointer_interaction
            || !was_focused
            || composition_changed
            || old_selection != self.editor.selection()
            || old_revision != self.editor.document().revision()
            || cache.interaction.caret_rect != Some(local_caret)
        {
            cache.interaction.last_interaction = now;
        }
        if response.has_focus() {
            let show_caret = preview.map_or(self.editor.selection().is_caret(), |preview| {
                preview.selection.is_some()
            });
            if visible && show_caret {
                egui::text_selection::visuals::paint_text_cursor(
                    ui,
                    &painter,
                    caret,
                    now - cache.interaction.last_interaction,
                );
            }
            if cache.interaction.caret_rect != Some(local_caret)
                || cache.interaction.scroll_to_caret
                || old_selection != self.editor.selection()
                || response.changed()
                || composition_changed
            {
                ui.scroll_to_rect(caret.expand(4.0), None);
                cache.interaction.scroll_to_caret = display_layouts
                    .content_height()
                    .is_some_and(|content_height| content_height > height);
                if cache.interaction.scroll_to_caret {
                    ui.ctx().request_repaint();
                }
            }
            cache.interaction.caret_rect = Some(local_caret);
        } else {
            cache.interaction.scroll_to_caret = false;
        }
        // Like egui's TextEdit, keep requesting IME while the window is in the
        // background. Winit 0.30 on X11 leaves an input context created after the
        // window regains focus unfocused, so IME would stop after an app switch.
        if !self.read_only
            && ui.is_enabled()
            && ui.memory(|memory| memory.owns_ime_events(response.id))
        {
            let transform = ui
                .ctx()
                .layer_transform_to_global(ui.layer_id())
                .unwrap_or_default();
            let mut ime_area = caret;
            if let Some(preview) = preview {
                let start = navigation::document_caret_rect(
                    display_document,
                    display_layouts,
                    origin,
                    preview.composition.replacement.range().start,
                    /* prefer_next_row= */ true,
                );
                if start.top() == caret.top() {
                    ime_area = ime_area.union(start);
                }
            }
            ui.output_mut(|output| {
                output.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect: transform * ime_area.intersect(rect),
                    cursor_rect: transform * caret,
                    should_interrupt_composition: interrupted,
                })
            });
        }

        let hovered_link = response
            .hover_pos()
            .filter(|_| preview.is_none() && ui.is_enabled())
            .and_then(|pointer| {
                navigation::link_at_point(self.editor.document(), &layouts, pointer - origin)
            });
        if response.hovered() {
            let follows_links = self.read_only || ui.input(|input| input.modifiers.command);
            ui.ctx()
                .set_cursor_icon(if hovered_link.is_some() && follows_links {
                    egui::CursorIcon::PointingHand
                } else {
                    egui::CursorIcon::Text
                });
        }
        if let Some(link) = hovered_link {
            response = response.on_hover_text_at_pointer(link.as_str());
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
            let mut info = egui::WidgetInfo::new(egui::Role::TextInput);
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
            let mut info = egui::WidgetInfo::new(egui::Role::TextInput);
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
                let next_row = cache
                    .interaction
                    .navigation
                    .as_ref()
                    .is_none_or(|navigation| navigation.prefer_next_row);
                snapshot.publish(
                    ui,
                    id,
                    self.editor,
                    &layouts,
                    origin,
                    self.read_only,
                    next_row,
                );
            }
        } else {
            cache.accessibility = None;
        }
        ui.ctx().data_mut(|data| data.insert_temp(id, cache));
        RichTextEditorOutput {
            response,
            errors,
            accessibility_errors,
            caret_rect: caret,
            selection_rect,
            clicked_link,
        }
    }

    fn hint_galley(&self, ui: &Ui, layouts: &ParagraphLayouts) -> Option<(Pos2, Arc<Galley>)> {
        let paragraphs = self.editor.document().paragraphs();
        if self.hint.is_empty()
            || paragraphs.len() != 1
            || !paragraphs[0].text().is_empty()
            || self.editor.composition().is_some()
        {
            return None;
        }
        let layout = layouts.get(0)?;
        let font = layout.galley.job.sections.first()?.format.font_id.clone();
        let mut job = LayoutJob::single_section(
            self.hint.clone(),
            TextFormat::simple(font, ui.visuals().weak_text_color()),
        );
        job.wrap.max_width = layout.galley.job.wrap.max_width;
        Some((layout.rect.min, ui.fonts_mut(|fonts| fonts.layout_job(job))))
    }

    fn set_focus_filter(&self, ui: &Ui, id: Id, cache: &Cache) {
        let tab = !self.read_only && cache.selection_contains_list(self.editor);
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    tab,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            );
        });
    }
}

/// Shows the widget with [`RichTextEditor::show`] and returns only its
/// response, dropping the rest of the [`RichTextEditorOutput`], including any
/// errors and activated link.
impl Widget for RichTextEditor<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui).response
    }
}

pub(super) const PADDING: f32 = 8.0;
pub(super) const PARAGRAPH_GAP: f32 = 4.0;

#[derive(Clone, Default)]
pub(super) struct Cache {
    pub(super) layout: LayoutCache,
    paint: PaintCache,
    plain_text: Option<Arc<str>>,
    pub(super) preview: Option<Box<CompositionPreview>>,
    pub(super) accessibility: Option<Arc<accessibility::Snapshot>>,
    interaction: InteractionState,
}

#[derive(Clone, Default)]
struct InteractionState {
    navigation: Option<VerticalNavigation>,
    caret_rect: Option<Rect>,
    scroll_to_caret: bool,
    focused: bool,
    focus_exit: Option<FocusExit>,
    last_interaction: f64,
    commit_echo: Option<CommitEcho>,
    cleared_preedit: Option<Arc<Composition>>,
    link_press: Option<LinkPress>,
}

impl InteractionState {
    fn remember_composition_end(&mut self, mut text: String, time: f64) {
        if let Some(previous) = self.cleared_preedit.take()
            && text.is_empty()
        {
            text =
                adapter::clean_text(&previous.text, ControlCharacters::KeepWhitespace).into_owned();
        }
        self.commit_echo = CommitEcho::new(text, time);
    }
}

#[derive(Clone, Copy)]
enum FocusExit {
    Request(egui::accesskit::NodeId),
    Traversal,
}

#[derive(Clone)]
struct LinkPress {
    link: Link,
    anchor: Position,
    revision: u64,
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
    pub(super) fn layout(
        &mut self,
        ui: &Ui,
        editor: &Editor,
        appearance: &Appearance,
    ) -> ParagraphLayouts {
        let update = self.layout.update(ui, editor.document(), appearance);
        if update.document_changed {
            self.plain_text = None;
            self.preview = None;
            self.interaction.link_press = None;
        }
        if update.layout_changed {
            self.interaction.navigation = None;
        }
        update.layouts
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
                        .unwrap_or_else(|| paragraph.scalar_count())
                })
    }
    fn selection_contains_list(&self, editor: &Editor) -> bool {
        self.layout
            .layouts
            .contains_list(editor.selected_paragraphs())
    }
}

pub(super) fn record<T>(result: Result<T, Error>, errors: &mut Vec<Error>) -> Option<T> {
    result.map_err(|error| errors.push(error)).ok()
}

#[derive(Clone, Copy)]
enum PointerEventKind {
    Press,
    ClickPress,
    Release,
    DragRelease,
}

fn primary_pointer_event(ui: &Ui, kind: PointerEventKind) -> Option<(Pos2, egui::Modifiers)> {
    let (position, modifiers) = ui.input(|input| {
        // Each primary button event as (pressed, position, modifiers).
        let mut events = input.events.iter().filter_map(|event| match event {
            Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            } => Some((*pressed, *pos, *modifiers)),
            _ => None,
        });
        let event = match kind {
            PointerEventKind::Press => events.rfind(|(pressed, _, _)| *pressed),
            PointerEventKind::ClickPress => {
                // A later press can belong to another widget. Only the press
                // immediately preceding this click's release belongs to it.
                events.rfind(|(pressed, _, _)| !*pressed)?;
                events.next_back().filter(|(pressed, _, _)| *pressed)
            }
            PointerEventKind::Release => events.rfind(|(pressed, _, _)| !*pressed),
            PointerEventKind::DragRelease => {
                // This drag began in an earlier frame. Its release can precede a
                // complete click on another widget within the current batch.
                events.find(|(pressed, _, _)| !*pressed)
            }
        }?;
        let (_, position, modifiers) = event;
        Some((position, modifiers))
    })?;
    let position = ui
        .ctx()
        .layer_transform_from_global(ui.layer_id())
        .map_or(position, |transform| transform * position);
    Some((position, modifiers))
}

fn finish_composition(editor: &mut Editor, errors: &mut Vec<Error>) -> Option<String> {
    let composition = editor.composition()?;
    let text =
        adapter::clean_text(&composition.text, ControlCharacters::KeepWhitespace).into_owned();
    if text.is_empty() {
        editor.cancel_composition();
        return Some(text);
    }
    Some(match editor.commit_composition(&text) {
        Ok(()) => text,
        Err(error) => {
            errors.push(error);
            String::new()
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn apply_queued_ime(
    editor: &mut Editor,
    ui: &Ui,
    cache: &mut Cache,
    appearance: &Appearance,
    layouts: &mut ParagraphLayouts,
    errors: &mut Vec<Error>,
    interrupted: &mut bool,
    options: &InputOptions,
    tab_moves_focus: bool,
) -> Vec<usize> {
    let queued: Vec<(usize, Event)> = ui.input(|input| {
        let end = input.events.iter().position(|event| match event {
            Event::PointerButton {
                button: egui::PointerButton::Primary,
                pressed: true,
                ..
            } => true,
            _ => tab_moves_focus && focus_traversal(event).is_some(),
        });
        input.events[..end.unwrap_or(input.events.len())]
            .iter()
            .enumerate()
            .filter(|(_, event)| matches!(event, Event::Ime(_)))
            .map(|(index, event)| (index, event.clone()))
            .collect()
    });
    let mut applied = Vec::new();
    let now = ui.input(|input| input.time);
    let revision = editor.document().revision();
    for (index, event) in queued {
        let composition = composition_before_event(editor, &event);
        if input::handle_event(
            editor,
            ui,
            &event,
            egui::Modifiers::NONE, // Queued IME input cannot contain a paste.
            layouts,
            &mut cache.interaction.navigation,
            &mut cache.interaction.commit_echo,
            errors,
            interrupted,
            options,
            &mut None,
        ) {
            applied.push(index);
        }
        observe_composition_event(&mut cache.interaction, composition, editor, &event, now);
        cache.layout.track(editor.document());
    }
    if revision != editor.document().revision() {
        *layouts = cache.layout(ui, editor, appearance);
    }
    applied
}

fn focus_traversal(event: &Event) -> Option<egui::FocusDirection> {
    match event {
        Event::Key {
            key: egui::Key::Tab,
            pressed: true,
            modifiers,
            ..
        } => {
            if !modifiers.any() {
                Some(egui::FocusDirection::Next)
            } else if modifiers.shift_only() {
                Some(egui::FocusDirection::Previous)
            } else {
                None
            }
        }
        _ => None,
    }
}

const COMMIT_ECHO_SECONDS: f64 = 0.5;

#[derive(Clone)]
pub(super) struct CommitEcho {
    pub(super) text: String,
    time: f64,
}

impl CommitEcho {
    fn new(text: String, time: f64) -> Option<Self> {
        (!text.is_empty()).then_some(Self { text, time })
    }
}

#[allow(deprecated)]
fn composition_before_event(editor: &Editor, event: &Event) -> Option<Arc<Composition>> {
    use egui::accesskit::Action;

    let preserve = match event {
        Event::Ime(ImeEvent::Preedit { text, .. }) => {
            text.is_empty()
                && editor
                    .composition()
                    .is_some_and(|composition| !composition.text.is_empty())
        }
        Event::Key {
            key: egui::Key::Escape,
            pressed: true,
            ..
        }
        | Event::Ime(ImeEvent::Disabled) => true,
        Event::Text(_) | Event::Paste(_) | Event::Cut | Event::Key { pressed: true, .. } => editor
            .composition()
            .is_some_and(|composition| composition.text.is_empty()),
        Event::AccessKitActionRequest(request) => matches!(
            request.action,
            Action::SetTextSelection | Action::SetValue | Action::ReplaceSelectedText
        ),
        _ => false,
    };
    // Surrounding deletion belongs to an IME edit whose following commit is still valid.
    preserve.then(|| editor.composition_snapshot()).flatten()
}

fn observe_composition_event(
    interaction: &mut InteractionState,
    previous: Option<Arc<Composition>>,
    editor: &Editor,
    event: &Event,
    time: f64,
) {
    let empty = editor
        .composition()
        .is_some_and(|composition| composition.text.is_empty());
    if matches!(event, Event::Ime(ImeEvent::Preedit { text, .. }) if text.is_empty()) {
        if empty && let Some(previous) = previous {
            interaction.cleared_preedit = Some(previous);
        }
    } else if let Some(previous) = previous
        && editor.composition().is_none()
    {
        // Some input methods still send the old preedit while processing an interruption.
        interaction.remember_composition_end(
            adapter::clean_text(&previous.text, ControlCharacters::KeepWhitespace).into_owned(),
            time,
        );
    }
    if !empty {
        interaction.cleared_preedit = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_accessibility_action(
    editor: &mut Editor,
    request: &egui::accesskit::ActionRequest,
    snapshot: Option<&accessibility::Snapshot>,
    layouts: &ParagraphLayouts,
    vertical_navigation: &mut Option<VerticalNavigation>,
    read_only: bool,
    interrupted: &mut bool,
    errors: &mut Vec<Error>,
    accessibility_errors: &mut Vec<AccessibilityError>,
) -> bool {
    use crate::accessibility::AccessibleAction;
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
            if let (
                Some(anchor),
                Some(CaretPlacement {
                    position: focus,
                    prefer_next_row,
                }),
            ) = (
                snapshot.decode(selection.anchor),
                snapshot.decode_focus(selection.focus),
            ) {
                *interrupted |= editor.cancel_composition();
                let result = editor.set_selection(Selection::new(anchor, focus));
                if result.is_ok() {
                    *vertical_navigation =
                        navigation::pointer_navigation(editor, layouts, focus, prefer_next_row);
                }
                record(result, errors);
            } else {
                accessibility_errors.push(AccessibilityError::InvalidTextPosition);
            }
        }
        (Action::SetValue | Action::ReplaceSelectedText, Some(ActionData::Value(value))) => {
            *interrupted |= editor.cancel_composition();
            let action = if request.action == Action::SetValue {
                AccessibleAction::ReplaceAllText(value)
            } else {
                AccessibleAction::ReplaceSelectedText(value)
            };
            // The core module shares this module's name, so name it in full.
            record(
                crate::accessibility::apply_action(editor, action).map(drop),
                errors,
            );
        }
        _ => accessibility_errors.push(AccessibilityError::InvalidActionData),
    }
    true
}

#[cfg(test)]
mod tests;
