//! Winit keyboard and IME bridge, independent of the renderer and OS clipboard.
//!
//! Call [`WinitAdapter::set_focused`] when the editor gains or loses widget focus,
//! then pass window events to [`WinitAdapter::handle_window_event`]. Do not pass
//! those same text events to another editor adapter. The host renders the
//! document and `Editor::composition()`, performs pointer hit testing, services
//! clipboard requests, and calls [`WinitAdapter::sync_ime`] with the caret area.

use crate::{Editor, Error, Movement, ParagraphKind, StylePatch};
use winit::{
    dpi::{LogicalPosition, LogicalSize},
    event::{ElementState, Ime, WindowEvent},
    keyboard::{Key, ModifiersState, NamedKey},
    window::Window,
};

/// Clipboard operations are owned by the host, avoiding platform dependencies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardEvent {
    Copy(String),
    /// The selection has already been removed from the document.
    Cut(String),
    /// Read the clipboard, then call [`WinitAdapter::paste`].
    PasteRequest,
}

/// A host should stop dispatching handled input and redraw after a change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputOutcome {
    pub handled: bool,
    /// Includes changes to the caret, typing style, and preedit text.
    pub changed: bool,
    pub clipboard: Option<ClipboardEvent>,
}

/// The command modifier can be overridden for a host's shortcut preferences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandModifier {
    Control,
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

#[derive(Debug, Default)]
pub struct WinitAdapter {
    focused: bool,
    window_unfocused: bool,
    modifiers: ModifiersState,
    ime_enabled: bool,
    command_modifier: CommandModifier,
}

impl WinitAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn command_modifier(mut self, modifier: CommandModifier) -> Self {
        self.command_modifier = modifier;
        self
    }

    pub fn is_focused(&self) -> bool {
        self.focused && !self.window_unfocused
    }

    pub fn ime_enabled(&self) -> bool {
        self.ime_enabled
    }

    pub fn modifiers(&self) -> ModifiersState {
        self.modifiers
    }

    /// Also useful when the host routes modifier state through its own input API.
    pub fn set_modifiers(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }

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
                        outcome.clipboard = Some(ClipboardEvent::Copy(editor.selected_text()));
                    }
                }
                "x" => {
                    if !editor.selection().is_caret() {
                        let selected = editor.selected_text();
                        editor.insert_text("")?;
                        outcome.clipboard = Some(ClipboardEvent::Cut(selected));
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
                _ => outcome.handled = false,
            }
            outcome.changed = before != EditorState::capture(editor);
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
                Key::Named(NamedKey::Backspace) => editor.delete_backward()?,
                Key::Named(NamedKey::Delete) => editor.delete_forward()?,
                _ if !self.modifiers.super_key()
                    && (!self.modifiers.control_key() || self.modifiers.alt_key()) =>
                {
                    let produced = text.unwrap_or("");
                    let clean: String = produced
                        .chars()
                        .filter(|character| !character.is_control())
                        .collect();
                    if clean.is_empty() {
                        outcome.handled = false;
                    } else {
                        editor.insert_text(&clean)?;
                    }
                }
                _ => outcome.handled = false,
            }
        }
        outcome.changed = before != EditorState::capture(editor);
        Ok(outcome)
    }

    pub fn handle_ime(&mut self, event: &Ime, editor: &mut Editor) -> Result<InputOutcome, Error> {
        if !self.is_focused() {
            return Ok(InputOutcome::default());
        }
        let before = EditorState::capture(editor);
        let was_enabled = self.ime_enabled;
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
                    if editor.composition().is_none() {
                        editor.begin_composition();
                    }
                    editor.update_composition(text, selection.map(|(start, end)| start..end))?;
                }
            }
            Ime::Commit(text) => {
                if editor.composition().is_none() {
                    editor.begin_composition();
                }
                editor.commit_composition(text)?;
            }
        }
        Ok(InputOutcome {
            handled: true,
            changed: before != EditorState::capture(editor) || was_enabled != self.ime_enabled,
            clipboard: None,
        })
    }

    /// Deliver a host clipboard response. Paste is a single undoable edit.
    pub fn paste(&mut self, editor: &mut Editor, text: &str) -> Result<InputOutcome, Error> {
        if !self.is_focused() {
            return Ok(InputOutcome::default());
        }
        let before = EditorState::capture(editor);
        let clean: String = text
            .chars()
            .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
            .collect();
        editor.break_history_group();
        editor.insert_text(&clean)?;
        editor.break_history_group();
        Ok(InputOutcome {
            handled: true,
            changed: before != EditorState::capture(editor),
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

#[derive(PartialEq, Eq)]
struct EditorState {
    revision: u64,
    selection: crate::Selection,
    style: crate::InlineStyle,
    preedit: Option<(String, Option<std::ops::Range<usize>>)>,
}

impl EditorState {
    fn capture(editor: &Editor) -> Self {
        Self {
            revision: editor.document().revision(),
            selection: editor.selection(),
            style: editor.typing_style(),
            preedit: editor
                .composition()
                .map(|composition| (composition.text.clone(), composition.selection.clone())),
        }
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
}
