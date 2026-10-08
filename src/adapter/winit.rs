//! Winit keyboard and IME bridge, independent of the renderer and OS clipboard.
//!
//! Call [`WinitAdapter::set_focused`] when the editor gains or loses widget focus,
//! then pass window events to [`WinitAdapter::handle_window_event`]. Do not pass
//! those same text events to another editor adapter. The host renders the
//! document and `Editor::composition()`, performs pointer hit testing, services
//! clipboard requests, and calls [`WinitAdapter::sync_ime`] with the caret area.

use std::borrow::Cow;

use crate::{Editor, Error, Fragment, Movement, ParagraphKind, StylePatch};
use winit::{
    dpi::{LogicalPosition, LogicalSize},
    event::{ElementState, Ime, WindowEvent},
    keyboard::{Key, ModifiersState, NamedKey},
    window::Window,
};

/// Clipboard operations are owned by the host, avoiding platform dependencies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardEvent {
    /// Plain selected text for the host to write to its clipboard.
    Copy(String),
    /// Enabled through [`WinitAdapter::rich_clipboard`]; encode native data or HTML in the host.
    CopyRich(Fragment),
    /// The selection has already been removed from the document.
    Cut(String),
    /// Rich content captured before the selection is removed.
    CutRich(Fragment),
    /// Read the clipboard, then call [`WinitAdapter::paste`].
    PasteRequest,
}

/// A host should stop dispatching handled input and redraw after a change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputOutcome {
    /// Whether the host should stop dispatching this event to other widgets.
    pub handled: bool,
    /// Includes changes to the caret, typing style, and preedit text.
    pub changed: bool,
    /// Clipboard work for the host to service after handling this event.
    pub clipboard: Option<ClipboardEvent>,
}

/// The command modifier can be overridden for a host's shortcut preferences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandModifier {
    /// Control-based shortcuts, the default outside macOS.
    Control,
    /// Super/Command-based shortcuts, the default on macOS.
    Super,
}

impl Default for CommandModifier {
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            Self::Super
        } else {
            Self::Control
        }
    }
}

/// Focus-aware native keyboard and IME routing for a host-rendered editor.
///
/// The host owns pointer selection, clipboard transport, rendering, and window
/// IME configuration. Only route editing events here for editable widgets.
#[derive(Debug, Default)]
pub struct WinitAdapter {
    focused: bool,
    window_unfocused: bool,
    modifiers: ModifiersState,
    ime_enabled: bool,
    command_modifier: CommandModifier,
    rich_clipboard: bool,
}

impl WinitAdapter {
    /// Create an unfocused adapter using the platform's command modifier.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the platform shortcut modifier.
    pub fn command_modifier(mut self, modifier: CommandModifier) -> Self {
        self.command_modifier = modifier;
        self
    }

    /// Request rich clipboard payloads for copy/cut instead of plain strings.
    /// The host owns MIME types and clipboard transport.
    pub fn rich_clipboard(mut self, enabled: bool) -> Self {
        self.rich_clipboard = enabled;
        self
    }

    /// Whether both the editor widget and its window have focus.
    pub fn is_focused(&self) -> bool {
        self.focused && !self.window_unfocused
    }

    /// Whether the native IME has announced that it is enabled.
    pub fn ime_enabled(&self) -> bool {
        self.ime_enabled
    }

    /// Return the most recently received modifier state.
    pub fn modifiers(&self) -> ModifiersState {
        self.modifiers
    }

    /// Also useful when the host routes modifier state through its own input API.
    pub fn set_modifiers(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }

    /// Set widget focus, canceling composition and clearing modifiers on blur.
    pub fn set_focused(&mut self, focused: bool, editor: &mut Editor) -> InputOutcome {
        let changed = self.focused != focused;
        self.focused = focused;
        let composition_changed = if !focused {
            self.ime_enabled = false;
            self.modifiers = ModifiersState::empty();
            editor.cancel_composition()
        } else {
            false
        };
        InputOutcome {
            changed: changed || composition_changed,
            ..InputOutcome::default()
        }
    }

    /// Enable platform IME while this widget has focus and keep its candidate
    /// popup near the caret. Coordinates are logical window coordinates.
    pub fn sync_ime(
        &self,
        window: &Window,
        position: LogicalPosition<f64>,
        size: LogicalSize<f64>,
    ) {
        window.set_ime_allowed(self.is_focused());
        if self.is_focused() && self.ime_enabled {
            window.set_ime_cursor_area(position, size);
        }
    }

    /// Route window focus, modifier, nonsynthetic keyboard, and IME events.
    ///
    /// Other events remain unhandled for the host.
    pub fn handle_window_event(
        &mut self,
        event: &WindowEvent,
        editor: &mut Editor,
    ) -> Result<InputOutcome, Error> {
        match event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                Ok(InputOutcome::default())
            }
            WindowEvent::Focused(focused) => {
                let changed = self.window_unfocused == *focused;
                self.window_unfocused = !focused;
                let canceled = if !focused {
                    self.ime_enabled = false;
                    self.modifiers = ModifiersState::empty();
                    editor.cancel_composition()
                } else {
                    false
                };
                Ok(InputOutcome {
                    changed: changed || canceled,
                    ..InputOutcome::default()
                })
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => self.handle_key(
                &event.logical_key,
                event.state,
                event.text.as_deref(),
                editor,
            ),
            WindowEvent::Ime(event) => self.handle_ime(event, editor),
            _ => Ok(InputOutcome::default()),
        }
    }

    /// Routes a logical key and its produced text. Insertion uses `text` only;
    /// using both it and `Key::Character` would duplicate dead-key output.
    pub fn handle_key(
        &mut self,
        key: &Key,
        state: ElementState,
        text: Option<&str>,
        editor: &mut Editor,
    ) -> Result<InputOutcome, Error> {
        if !self.is_focused() || state != ElementState::Pressed {
            return Ok(InputOutcome::default());
        }
        if editor.composition().is_some_and(|composition| {
            !composition.text.is_empty() || matches!(key, Key::Named(NamedKey::Escape))
        }) {
            // The IME owns navigation and editing while composing.
            return Ok(InputOutcome {
                handled: true,
                changed: matches!(key, Key::Named(NamedKey::Escape)) && editor.cancel_composition(),
                clipboard: None,
            });
        }
        let before = EditorState::capture(editor);
        // Empty preedit can precede either Commit or IME cancellation. Keep
        // its replacement for Commit, but resume normal editing when a new
        // key arrives instead. Winit suppresses key events during composition.
        if editor.composition().is_some() {
            editor.cancel_composition();
        }
        let mut outcome = InputOutcome {
            handled: true,
            ..InputOutcome::default()
        };
        let command = self.command_pressed();
        let shift = self.modifiers.shift_key();
        if command && let Key::Character(character) = key {
            match character.to_ascii_lowercase().as_str() {
                "a" => editor.select_all(),
                "c" => {
                    if !editor.selection().is_caret() {
                        outcome.clipboard = Some(if self.rich_clipboard {
                            ClipboardEvent::CopyRich(editor.selected_fragment())
                        } else {
                            ClipboardEvent::Copy(editor.selected_text())
                        });
                    }
                }
                "x" => {
                    if !editor.selection().is_caret() {
                        let selected = if self.rich_clipboard {
                            ClipboardEvent::CutRich(editor.selected_fragment())
                        } else {
                            ClipboardEvent::Cut(editor.selected_text())
                        };
                        editor.insert_text("")?;
                        outcome.clipboard = Some(selected);
                    }
                }
                "v" => outcome.clipboard = Some(ClipboardEvent::PasteRequest),
                "z" => {
                    if shift {
                        editor.redo();
                    } else {
                        editor.undo();
                    }
                }
                "y" => {
                    editor.redo();
                }
                "b" => editor.apply_style(StylePatch {
                    bold: Some(!editor.typing_style().bold),
                    ..StylePatch::default()
                })?,
                "i" => editor.apply_style(StylePatch {
                    italic: Some(!editor.typing_style().italic),
                    ..StylePatch::default()
                })?,
                "u" => editor.apply_style(StylePatch {
                    underline: Some(!editor.typing_style().underline),
                    ..StylePatch::default()
                })?,
                "7" | "&" if shift => editor.set_paragraph_kind(ParagraphKind::Ordered {
                    indent: 0,
                    start: 1,
                })?,
                "8" | "*" if shift => {
                    editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })?
                }
                "]" => editor.indent_list()?,
                "[" => editor.outdent_list()?,
                _ => outcome.handled = false,
            }
            outcome.changed = before.changed(editor);
            return Ok(outcome);
        }

        let word = self.modifiers.alt_key()
            || (self.modifiers.control_key() && !self.modifiers.super_key());
        let super_command = command && self.command_modifier == CommandModifier::Super;
        let movement = match key {
            Key::Named(NamedKey::ArrowLeft) if super_command => Some(Movement::ParagraphStart),
            Key::Named(NamedKey::ArrowRight) if super_command => Some(Movement::ParagraphEnd),
            Key::Named(NamedKey::ArrowLeft) if word => Some(Movement::WordBackward),
            Key::Named(NamedKey::ArrowRight) if word => Some(Movement::WordForward),
            Key::Named(NamedKey::ArrowLeft) => Some(Movement::GraphemeBackward),
            Key::Named(NamedKey::ArrowRight) => Some(Movement::GraphemeForward),
            Key::Named(NamedKey::ArrowUp) if command => Some(Movement::DocumentStart),
            Key::Named(NamedKey::ArrowDown) if command => Some(Movement::DocumentEnd),
            Key::Named(NamedKey::ArrowUp) => Some(Movement::ParagraphUp),
            Key::Named(NamedKey::ArrowDown) => Some(Movement::ParagraphDown),
            Key::Named(NamedKey::Home) if command => Some(Movement::DocumentStart),
            Key::Named(NamedKey::End) if command => Some(Movement::DocumentEnd),
            Key::Named(NamedKey::Home) => Some(Movement::ParagraphStart),
            Key::Named(NamedKey::End) => Some(Movement::ParagraphEnd),
            _ => None,
        };
        if let Some(movement) = movement {
            editor.move_cursor(movement, shift)?;
        } else {
            match key {
                Key::Named(NamedKey::Enter) if !command => editor.insert_paragraph()?,
                Key::Named(NamedKey::Backspace) if word => editor.delete_word_backward()?,
                Key::Named(NamedKey::Delete) if word => editor.delete_word_forward()?,
                Key::Named(NamedKey::Backspace) => editor.delete_backward()?,
                Key::Named(NamedKey::Delete) => editor.delete_forward()?,
                Key::Named(NamedKey::Tab)
                    if !self.modifiers.intersects(
                        ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SUPER,
                    ) && selection_contains_list(editor) =>
                {
                    if shift {
                        editor.outdent_list()?;
                    } else {
                        editor.indent_list()?;
                    }
                }
                _ if !self.modifiers.super_key()
                    && (!self.modifiers.control_key() || self.modifiers.alt_key()) =>
                {
                    let produced = text.unwrap_or("");
                    let clean = clean_text(produced, false);
                    if clean.is_empty() {
                        outcome.handled = false;
                    } else {
                        editor.insert_text(&clean)?;
                    }
                }
                _ => outcome.handled = false,
            }
        }
        outcome.changed = before.changed(editor);
        Ok(outcome)
    }

    /// Route native preedit and commit events while the editor has focus.
    ///
    /// Invalid UTF-8 selection boundaries are rejected without changing preedit.
    /// Empty reset events without an active composition preserve selected text.
    pub fn handle_ime(&mut self, event: &Ime, editor: &mut Editor) -> Result<InputOutcome, Error> {
        if !self.is_focused() {
            return Ok(InputOutcome::default());
        }
        let before = EditorState::capture(editor);
        let was_enabled = self.ime_enabled;
        let mut preedit_changed = false;
        match event {
            Ime::Enabled => self.ime_enabled = true,
            Ime::Disabled => {
                self.ime_enabled = false;
                editor.cancel_composition();
            }
            Ime::Preedit(text, selection) => {
                if let Some((start, end)) = selection
                    && (start > end
                        || !text.is_char_boundary(*start)
                        || !text.is_char_boundary(*end))
                {
                    return Err(Error::InvalidCompositionSelection);
                }
                // An empty preedit preserves the captured replacement until
                // Commit. Clearing/canceling here would lose that range.
                if !text.is_empty() || editor.composition().is_some() {
                    let selection = selection.map(|(start, end)| start..end);
                    preedit_changed = editor.composition().is_none_or(|composition| {
                        composition.text != *text || composition.selection != selection
                    });
                    if preedit_changed {
                        editor.update_composition(text, selection)?;
                    }
                }
            }
            Ime::Commit(text) => {
                // Platform IME setup can emit an empty reset without preedit.
                // A real composition's empty commit still deletes its replacement.
                if !text.is_empty() || editor.composition().is_some() {
                    editor.commit_composition(text)?;
                }
            }
        }
        Ok(InputOutcome {
            handled: true,
            changed: before.changed(editor) || preedit_changed || was_enabled != self.ime_enabled,
            clipboard: None,
        })
    }

    /// Deliver a host clipboard response. Paste is a single undoable edit.
    /// Empty input or text consisting only of filtered controls leaves selection,
    /// composition, and typing history unchanged.
    pub fn paste(&mut self, editor: &mut Editor, text: &str) -> Result<InputOutcome, Error> {
        if !self.is_focused() {
            return Ok(InputOutcome::default());
        }
        let clean = clean_text(text, true);
        if clean.is_empty() {
            return Ok(InputOutcome {
                handled: true,
                ..InputOutcome::default()
            });
        }
        let before = EditorState::capture(editor);
        editor.break_history_group();
        editor.insert_text(&clean)?;
        editor.break_history_group();
        Ok(InputOutcome {
            handled: true,
            changed: before.changed(editor),
            clipboard: None,
        })
    }

    /// Deliver a rich host clipboard response as one undoable edit.
    pub fn paste_fragment(
        &mut self,
        editor: &mut Editor,
        fragment: &Fragment,
    ) -> Result<InputOutcome, Error> {
        if !self.is_focused() {
            return Ok(InputOutcome::default());
        }
        let before = EditorState::capture(editor);
        editor.insert_fragment(fragment)?;
        Ok(InputOutcome {
            handled: true,
            changed: before.changed(editor),
            clipboard: None,
        })
    }

    fn command_pressed(&self) -> bool {
        match self.command_modifier {
            // Ctrl+Alt is commonly AltGr and must remain available for text.
            CommandModifier::Control => {
                self.modifiers.control_key()
                    && !self.modifiers.alt_key()
                    && !self.modifiers.super_key()
            }
            CommandModifier::Super => {
                self.modifiers.super_key()
                    && !self.modifiers.alt_key()
                    && !self.modifiers.control_key()
            }
        }
    }
}

fn clean_text(text: &str, multiline: bool) -> Cow<'_, str> {
    let allowed = |character: char| {
        !character.is_control() || multiline && matches!(character, '\n' | '\r' | '\t')
    };
    if text.chars().all(allowed) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(
            text.chars()
                .filter(|character| allowed(*character))
                .collect(),
        )
    }
}

fn selection_contains_list(editor: &Editor) -> bool {
    let range = editor.selection().range();
    let end = if range.end.paragraph > range.start.paragraph && range.end.byte == 0 {
        range.end.paragraph
    } else {
        range.end.paragraph + 1
    };
    editor.document().paragraphs()[range.start.paragraph..end]
        .iter()
        .any(|paragraph| {
            matches!(
                paragraph.kind(),
                ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. }
            )
        })
}

struct EditorState {
    revision: u64,
    selection: crate::Selection,
    style: crate::InlineStyle,
    composing: bool,
}

impl EditorState {
    fn capture(editor: &Editor) -> Self {
        Self {
            revision: editor.document().revision(),
            selection: editor.selection(),
            style: editor.typing_style(),
            composing: editor.composition().is_some(),
        }
    }

    fn changed(&self, editor: &Editor) -> bool {
        self.revision != editor.document().revision()
            || self.selection != editor.selection()
            || self.style != editor.typing_style()
            || self.composing != editor.composition().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Position, Selection};

    fn focused(editor: &mut Editor) -> WinitAdapter {
        let mut adapter = WinitAdapter::new().command_modifier(CommandModifier::Control);
        adapter.set_focused(true, editor);
        adapter
    }

    fn key(
        adapter: &mut WinitAdapter,
        editor: &mut Editor,
        key: Key,
        text: Option<&str>,
    ) -> InputOutcome {
        adapter
            .handle_key(&key, ElementState::Pressed, text, editor)
            .unwrap()
    }

    #[test]
    fn text_dead_keys_enter_and_altgr_are_inserted_once() {
        let mut editor = Editor::from_text("");
        let mut adapter = focused(&mut editor);
        key(
            &mut adapter,
            &mut editor,
            Key::Character("e".into()),
            Some("´e"),
        );
        key(
            &mut adapter,
            &mut editor,
            Key::Named(NamedKey::Enter),
            Some("\r"),
        );
        adapter.set_modifiers(ModifiersState::CONTROL | ModifiersState::ALT);
        key(
            &mut adapter,
            &mut editor,
            Key::Character("@".into()),
            Some("@"),
        );
        assert_eq!(editor.document().plain_text(), "´e\n@");
        key(
            &mut adapter,
            &mut editor,
            Key::Character("x".into()),
            Some("\u{0}"),
        );
        assert_eq!(editor.document().plain_text(), "´e\n@");
    }

    #[test]
    fn rich_clipboard_captures_styles_before_cut_and_pastes_atomically() {
        let mut editor = Editor::from_text("café 👩‍💻");
        editor.select_all();
        editor
            .apply_style(StylePatch {
                bold: Some(true),
                ..Default::default()
            })
            .unwrap();
        let rich = editor.selected_fragment();
        let mut adapter = focused(&mut editor).rich_clipboard(true);
        adapter.set_modifiers(ModifiersState::CONTROL);
        let copy = key(&mut adapter, &mut editor, Key::Character("c".into()), None);
        assert_eq!(copy.clipboard, Some(ClipboardEvent::CopyRich(rich.clone())));
        let cut = key(&mut adapter, &mut editor, Key::Character("x".into()), None);
        assert_eq!(cut.clipboard, Some(ClipboardEvent::CutRich(rich.clone())));
        assert_eq!(editor.document().plain_text(), "");
        assert!(adapter.paste_fragment(&mut editor, &rich).unwrap().changed);
        assert_eq!(editor.document().to_bytes(), rich.to_bytes());
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "");
        assert!(editor.undo());
        assert_eq!(editor.document().to_bytes(), rich.to_bytes());
        adapter.set_focused(false, &mut editor);
        assert!(!adapter.paste_fragment(&mut editor, &rich).unwrap().handled);
    }

    #[test]
    fn word_deletion_and_list_tabs_route_to_core_and_leave_body_tab_to_host() {
        let mut editor = Editor::from_text("one café");
        editor
            .set_selection(Selection::caret(editor.document().end()))
            .unwrap();
        let mut adapter = focused(&mut editor);
        adapter.set_modifiers(ModifiersState::CONTROL);
        key(
            &mut adapter,
            &mut editor,
            Key::Named(NamedKey::Backspace),
            None,
        );
        assert_eq!(editor.document().plain_text(), "one ");
        assert!(editor.undo());
        editor
            .set_selection(Selection::caret(Position::default()))
            .unwrap();
        key(
            &mut adapter,
            &mut editor,
            Key::Named(NamedKey::Delete),
            None,
        );
        assert_eq!(editor.document().plain_text(), " café");
        editor
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
            .unwrap();
        adapter.set_modifiers(ModifiersState::empty());
        assert!(key(&mut adapter, &mut editor, Key::Named(NamedKey::Tab), None).handled);
        assert_eq!(
            editor.document().paragraph(0).unwrap().kind(),
            ParagraphKind::Bullet { indent: 1 }
        );
        adapter.set_modifiers(ModifiersState::SHIFT);
        key(&mut adapter, &mut editor, Key::Named(NamedKey::Tab), None);
        key(&mut adapter, &mut editor, Key::Named(NamedKey::Tab), None);
        assert_eq!(
            editor.document().paragraph(0).unwrap().kind(),
            ParagraphKind::Body
        );
        assert!(!key(&mut adapter, &mut editor, Key::Named(NamedKey::Tab), None).handled);
    }

    #[test]
    fn modified_tabs_remain_with_host_and_list_selection_excludes_body_endpoint() {
        let mut editor = Editor::from_text("list\nbody");
        editor
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
            .unwrap();
        let mut adapter = focused(&mut editor);
        for modifier in [
            ModifiersState::CONTROL,
            ModifiersState::ALT,
            ModifiersState::SUPER,
        ] {
            adapter.set_modifiers(modifier);
            assert!(!key(&mut adapter, &mut editor, Key::Named(NamedKey::Tab), None).handled);
            assert_eq!(
                editor.document().paragraph(0).unwrap().kind(),
                ParagraphKind::Bullet { indent: 0 }
            );
        }
        adapter.set_modifiers(ModifiersState::empty());
        editor
            .set_selection(Selection::new(Position::new(0, 0), Position::new(1, 0)))
            .unwrap();
        assert!(key(&mut adapter, &mut editor, Key::Named(NamedKey::Tab), None).handled);
        assert_eq!(
            editor.document().paragraph(0).unwrap().kind(),
            ParagraphKind::Bullet { indent: 1 }
        );
        assert_eq!(
            editor.document().paragraph(1).unwrap().kind(),
            ParagraphKind::Body
        );
    }

    #[test]
    fn empty_or_filtered_paste_preserves_selection_and_history() {
        let mut editor = Editor::from_text("original");
        editor.insert_text("typed ").unwrap();
        editor.select_all();
        let selection = editor.selection();
        let revision = editor.document().revision();
        let mut adapter = focused(&mut editor);
        for clipboard in ["", "\u{0}\u{1}\u{7}"] {
            let outcome = adapter.paste(&mut editor, clipboard).unwrap();
            assert!(outcome.handled);
            assert!(!outcome.changed);
            assert_eq!(editor.document().plain_text(), "typed original");
            assert_eq!(editor.document().revision(), revision);
            assert_eq!(editor.selection(), selection);
        }
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "original");
        assert!(!editor.can_undo());

        key(
            &mut adapter,
            &mut editor,
            Key::Character("a".into()),
            Some("a"),
        );
        adapter.paste(&mut editor, "").unwrap();
        key(
            &mut adapter,
            &mut editor,
            Key::Character("b".into()),
            Some("b"),
        );
        assert_eq!(editor.document().plain_text(), "aboriginal");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "original");
        assert!(!editor.can_undo());

        for preedit in ["", "候"] {
            editor.update_composition(preedit, None).unwrap();
            let composition = editor.composition().cloned();
            let outcome = adapter.paste(&mut editor, "\u{0}").unwrap();
            assert!(outcome.handled);
            assert!(!outcome.changed);
            assert_eq!(editor.composition(), composition.as_ref());
            assert_eq!(editor.document().plain_text(), "original");
            assert!(!editor.can_undo());
        }
    }

    #[test]
    fn clipboard_shortcuts_are_explicit_and_undoable() {
        let mut editor = Editor::from_text("hello");
        let mut adapter = focused(&mut editor);
        adapter.set_modifiers(ModifiersState::CONTROL);
        key(
            &mut adapter,
            &mut editor,
            Key::Character("a".into()),
            Some("\u{1}"),
        );
        let cut = key(&mut adapter, &mut editor, Key::Character("x".into()), None);
        assert_eq!(cut.clipboard, Some(ClipboardEvent::Cut("hello".into())));
        assert_eq!(editor.document().plain_text(), "");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "hello");
        let paste = key(&mut adapter, &mut editor, Key::Character("v".into()), None);
        assert_eq!(paste.clipboard, Some(ClipboardEvent::PasteRequest));
        adapter.paste(&mut editor, "one\r\ntwo\u{0}").unwrap();
        assert_eq!(editor.document().plain_text(), "one\ntwo");
    }

    #[test]
    fn ime_empty_preedit_keeps_the_replacement_and_suppresses_keys() {
        let mut editor = Editor::from_text("ab");
        editor
            .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 1)))
            .unwrap();
        let mut adapter = focused(&mut editor);
        adapter.handle_ime(&Ime::Enabled, &mut editor).unwrap();
        adapter
            .handle_ime(&Ime::Preedit("え".into(), Some((3, 3))), &mut editor)
            .unwrap();
        key(
            &mut adapter,
            &mut editor,
            Key::Character("e".into()),
            Some("e"),
        );
        assert_eq!(editor.document().plain_text(), "ab");
        adapter
            .handle_ime(&Ime::Preedit(String::new(), None), &mut editor)
            .unwrap();
        adapter
            .handle_ime(&Ime::Commit("é".into()), &mut editor)
            .unwrap();
        assert_eq!(editor.document().plain_text(), "éb");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "ab");
    }

    #[test]
    fn invalid_preedit_and_focus_loss_do_not_modify_the_document() {
        let mut editor = Editor::from_text("a");
        let mut adapter = focused(&mut editor);
        assert_eq!(
            adapter.handle_ime(&Ime::Preedit("é".into(), Some((1, 1))), &mut editor),
            Err(Error::InvalidCompositionSelection)
        );
        assert!(editor.composition().is_none());
        adapter
            .handle_ime(&Ime::Preedit("候".into(), None), &mut editor)
            .unwrap();
        adapter
            .handle_window_event(&WindowEvent::Focused(false), &mut editor)
            .unwrap();
        assert!(editor.composition().is_none());
        assert!(!adapter.is_focused());
        assert!(
            !key(
                &mut adapter,
                &mut editor,
                Key::Character("b".into()),
                Some("b")
            )
            .handled
        );
        assert_eq!(editor.document().plain_text(), "a");
    }

    #[test]
    fn cleared_preedit_without_commit_does_not_block_future_typing() {
        let mut editor = Editor::from_text("ab");
        editor
            .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 1)))
            .unwrap();
        let mut adapter = focused(&mut editor);
        adapter.handle_ime(&Ime::Enabled, &mut editor).unwrap();
        adapter
            .handle_ime(&Ime::Preedit("候".into(), None), &mut editor)
            .unwrap();
        adapter
            .handle_ime(&Ime::Preedit(String::new(), None), &mut editor)
            .unwrap();
        key(
            &mut adapter,
            &mut editor,
            Key::Character("c".into()),
            Some("c"),
        );
        assert_eq!(editor.document().plain_text(), "cb");
        assert!(editor.composition().is_none());
        assert!(adapter.ime_enabled());
    }

    #[test]
    fn idle_empty_ime_events_preserve_selection_and_history() {
        let mut editor = Editor::from_text("selected");
        editor.select_all();
        let selection = editor.selection();
        let mut adapter = focused(&mut editor);
        for event in [
            Ime::Preedit(String::new(), None),
            Ime::Commit(String::new()),
        ] {
            let outcome = adapter.handle_ime(&event, &mut editor).unwrap();
            assert!(outcome.handled);
            assert!(!outcome.changed);
            assert_eq!(editor.document().plain_text(), "selected");
            assert_eq!(editor.selection(), selection);
            assert!(editor.composition().is_none());
            assert!(!editor.can_undo());
        }
    }

    #[test]
    fn empty_commit_after_preedit_deletes_the_captured_replacement() {
        let mut editor = Editor::from_text("selected");
        editor.select_all();
        let selection = editor.selection();
        let mut adapter = focused(&mut editor);
        adapter
            .handle_ime(&Ime::Preedit("candidate".into(), None), &mut editor)
            .unwrap();
        adapter
            .handle_ime(&Ime::Preedit(String::new(), None), &mut editor)
            .unwrap();
        let outcome = adapter
            .handle_ime(&Ime::Commit(String::new()), &mut editor)
            .unwrap();
        assert!(outcome.changed);
        assert_eq!(editor.document().plain_text(), "");
        assert!(editor.composition().is_none());
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "selected");
        assert_eq!(editor.selection(), selection);
        assert!(!editor.can_undo());
    }

    #[test]
    fn ime_outcomes_report_content_selection_and_enabled_changes() {
        let mut editor = Editor::from_text("original");
        let mut adapter = focused(&mut editor);
        let text = "候".repeat(1024);
        let preedit = Ime::Preedit(text.clone(), Some((text.len(), text.len())));
        assert!(adapter.handle_ime(&preedit, &mut editor).unwrap().changed);
        assert!(!adapter.handle_ime(&preedit, &mut editor).unwrap().changed);
        assert!(
            adapter
                .handle_ime(&Ime::Enabled, &mut editor)
                .unwrap()
                .changed
        );
        assert!(
            !adapter
                .handle_ime(&Ime::Enabled, &mut editor)
                .unwrap()
                .changed
        );

        let selection_changed = Ime::Preedit(text, Some((0, 3)));
        assert!(
            adapter
                .handle_ime(&selection_changed, &mut editor)
                .unwrap()
                .changed
        );
        assert!(
            !adapter
                .handle_ime(&selection_changed, &mut editor)
                .unwrap()
                .changed
        );
        assert!(
            adapter
                .handle_ime(&Ime::Preedit("different".into(), None), &mut editor)
                .unwrap()
                .changed
        );
        assert!(
            adapter
                .handle_ime(&Ime::Disabled, &mut editor)
                .unwrap()
                .changed
        );
        assert!(
            !adapter
                .handle_ime(&Ime::Disabled, &mut editor)
                .unwrap()
                .changed
        );
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "original");
        assert!(!editor.can_undo());
    }
}
