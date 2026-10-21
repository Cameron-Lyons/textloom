use super::{Shortcut, clean_text, shortcut_key_name};
use crate::{Editor, Error, Fragment, Movement, ParagraphKind};
use winit::{
    dpi::{LogicalPosition, LogicalSize},
    event::{ElementState, Ime, WindowEvent},
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
    window::Window,
};

const CTRL_ALT_IS_ALTGR: bool = cfg!(windows);

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClipboardEvent {
    Copy(String),
    CopyRich(Fragment),
    Cut(String),
    CutRich(Fragment),
    PasteRequest,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
#[must_use = "service clipboard requests and redraw after changes"]
pub struct InputOutcome {
    pub handled: bool,
    pub changed: bool,
    pub clipboard: Option<ClipboardEvent>,
}

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
    rich_clipboard: bool,
    read_only: bool,
    host_vertical_navigation: bool,
    ime: ImeSync,
}

type ImeArea = (LogicalPosition<f64>, LogicalSize<f64>);

#[derive(Debug, Default)]
struct ImeSync {
    allowed: bool,
    stale: bool,
    area: Option<ImeArea>,
    preedit: bool,
}

#[derive(Debug, Default, PartialEq)]
struct ImeRequests {
    interrupt: bool,
    allowed: Option<bool>,
    area: Option<ImeArea>,
}

impl WinitAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn command_modifier(mut self, modifier: CommandModifier) -> Self {
        self.command_modifier = modifier;
        self
    }

    pub fn rich_clipboard(mut self, enabled: bool) -> Self {
        self.rich_clipboard = enabled;
        self
    }

    pub fn host_vertical_navigation(mut self, enabled: bool) -> Self {
        self.host_vertical_navigation = enabled;
        self
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn set_read_only(&mut self, read_only: bool, editor: &mut Editor) -> InputOutcome {
        let changed = self.read_only != read_only;
        self.read_only = read_only;
        let canceled = read_only && editor.cancel_composition();
        if read_only {
            self.ime_enabled = false;
        }
        InputOutcome {
            changed: changed || canceled,
            ..InputOutcome::default()
        }
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

    pub fn set_modifiers(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }

    pub fn set_focused(&mut self, focused: bool, editor: &mut Editor) -> InputOutcome {
        let was_focused = self.is_focused();
        let changed = self.focused != focused;
        self.focused = focused;
        let canceled = self.focus_changed(was_focused, editor);
        InputOutcome {
            changed: changed || canceled,
            ..InputOutcome::default()
        }
    }

    pub fn sync_ime(
        &mut self,
        window: &Window,
        editor: &Editor,
        position: LogicalPosition<f64>,
        size: LogicalSize<f64>,
    ) {
        let window_focused = self.window_unfocused && window.has_focus();
        let requests = self.plan_ime(editor, window_focused, (position, size));
        if requests.interrupt {
            window.set_ime_allowed(false);
            window.set_ime_allowed(true);
        }
        if let Some(allowed) = requests.allowed {
            window.set_ime_allowed(allowed);
        }
        if let Some((position, size)) = requests.area {
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
                let was_focused = self.is_focused();
                let changed = self.window_unfocused == *focused;
                self.window_unfocused = !focused;
                if !focused {
                    self.modifiers = ModifiersState::empty();
                }
                let canceled = self.focus_changed(was_focused, editor);
                Ok(InputOutcome {
                    changed: changed || canceled,
                    ..InputOutcome::default()
                })
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => {
                let regained = event.state == ElementState::Pressed && self.observe_window_focus();
                let mut outcome = self.handle_key(
                    &event.logical_key,
                    event.physical_key,
                    event.state,
                    event.text.as_deref(),
                    editor,
                )?;
                outcome.changed |= regained;
                Ok(outcome)
            }
            WindowEvent::Ime(event) => self.handle_ime(event, editor),
            _ => Ok(InputOutcome::default()),
        }
    }

    pub fn handle_key(
        &mut self,
        key: &Key,
        physical_key: PhysicalKey,
        state: ElementState,
        text: Option<&str>,
        editor: &mut Editor,
    ) -> Result<InputOutcome, Error> {
        if !self.is_focused() || state != ElementState::Pressed {
            return Ok(InputOutcome::default());
        }
        let before = editor.presentation();
        if self.read_only {
            editor.cancel_composition();
        }
        let escape = matches!(key, Key::Named(NamedKey::Escape));
        if editor
            .composition()
            .is_some_and(|composition| !composition.text.is_empty() || escape)
        {
            return Ok(InputOutcome {
                handled: true,
                changed: escape && editor.cancel_composition(),
                clipboard: None,
            });
        }
        let mut outcome = InputOutcome {
            handled: true,
            ..InputOutcome::default()
        };
        let command = self.command_pressed();
        let shift = self.modifiers.shift_key();
        let mut shortcut = None;
        if command {
            shortcut = shortcut_key(key, physical_key).and_then(|name| Shortcut::new(name, shift));
            if shortcut.is_none() && matches!(key, Key::Character(_)) {
                outcome.handled = false;
                outcome.changed = before != editor.presentation();
                return Ok(outcome);
            }
        }
        if let Some(shortcut) = shortcut.or_else(|| self.named_shortcut(key, editor)) {
            if !(matches!(shortcut, Shortcut::Copy | Shortcut::Cut)
                && editor.selection().is_caret())
            {
                editor.cancel_composition();
            }
            outcome.clipboard = self.run_shortcut(shortcut, editor)?;
            outcome.changed = before != editor.presentation();
            return Ok(outcome);
        }

        let word = self.modifiers.alt_key()
            || (self.modifiers.control_key() && !self.modifiers.super_key());
        let super_command = command && self.command_modifier == CommandModifier::Super;
        let paragraph_vertical = !self.host_vertical_navigation;
        let movement = match key {
            Key::Named(NamedKey::ArrowLeft) if super_command => Some(Movement::ParagraphStart),
            Key::Named(NamedKey::ArrowRight) if super_command => Some(Movement::ParagraphEnd),
            Key::Named(NamedKey::ArrowLeft) if word => Some(Movement::WordBackward),
            Key::Named(NamedKey::ArrowRight) if word => Some(Movement::WordForward),
            Key::Named(NamedKey::ArrowLeft) => Some(Movement::GraphemeBackward),
            Key::Named(NamedKey::ArrowRight) => Some(Movement::GraphemeForward),
            Key::Named(NamedKey::ArrowUp) if command => Some(Movement::DocumentStart),
            Key::Named(NamedKey::ArrowDown) if command => Some(Movement::DocumentEnd),
            Key::Named(NamedKey::ArrowUp) if paragraph_vertical => Some(Movement::ParagraphUp),
            Key::Named(NamedKey::ArrowDown) if paragraph_vertical => Some(Movement::ParagraphDown),
            Key::Named(NamedKey::Home) if command => Some(Movement::DocumentStart),
            Key::Named(NamedKey::End) if command => Some(Movement::DocumentEnd),
            Key::Named(NamedKey::Home) => Some(Movement::ParagraphStart),
            Key::Named(NamedKey::End) => Some(Movement::ParagraphEnd),
            _ => None,
        };
        if let Some(movement) = movement {
            editor.cancel_composition();
            editor.move_cursor(movement, shift)?;
        } else {
            match key {
                Key::Named(
                    NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::PageUp | NamedKey::PageDown,
                ) => outcome.handled = false,
                Key::Named(NamedKey::Process) => {}
                Key::Named(NamedKey::Enter) if self.read_only && !command => {}
                Key::Named(NamedKey::Backspace | NamedKey::Delete) if self.read_only => {}
                Key::Named(NamedKey::Enter) if shift && !command => {
                    editor.cancel_composition();
                    editor.insert_line_break()?;
                }
                Key::Named(NamedKey::Enter) if !command => {
                    editor.cancel_composition();
                    editor.insert_paragraph()?;
                }
                Key::Named(NamedKey::Backspace) if super_command => {
                    editor.cancel_composition();
                    editor.delete_to_paragraph_start()?;
                }
                Key::Named(NamedKey::Backspace) if word => {
                    editor.cancel_composition();
                    editor.delete_word_backward()?;
                }
                Key::Named(NamedKey::Delete) if word => {
                    editor.cancel_composition();
                    editor.delete_word_forward()?;
                }
                Key::Named(NamedKey::Backspace) => {
                    editor.cancel_composition();
                    editor.delete_backward()?;
                }
                Key::Named(NamedKey::Delete) => {
                    editor.cancel_composition();
                    editor.delete_forward()?;
                }
                Key::Named(NamedKey::Tab)
                    if !self.read_only
                        && !self.modifiers.intersects(
                            ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SUPER,
                        )
                        && selection_contains_list(editor) =>
                {
                    editor.cancel_composition();
                    if shift {
                        editor.outdent_list()?;
                    } else {
                        editor.indent_list()?;
                    }
                }
                _ if !self.modifiers.super_key()
                    && (!self.modifiers.control_key()
                        || CTRL_ALT_IS_ALTGR && self.modifiers.alt_key()) =>
                {
                    let produced = text.unwrap_or("");
                    let clean = clean_text(produced, false);
                    if clean.is_empty() {
                        outcome.handled = false;
                    } else if !self.read_only {
                        editor.cancel_composition();
                        editor.insert_text(&clean)?;
                    }
                }
                _ => outcome.handled = false,
            }
        }
        outcome.changed = before != editor.presentation();
        Ok(outcome)
    }

    pub fn handle_ime(&mut self, event: &Ime, editor: &mut Editor) -> Result<InputOutcome, Error> {
        if !self.is_focused() {
            return Ok(InputOutcome::default());
        }
        if self.read_only {
            if matches!(event, Ime::Disabled) {
                // No preedit or commit follows Disabled, so the canceled session is over.
                self.ime.preedit = false;
                self.ime.stale = true;
            }
            let changed = editor.cancel_composition() || self.ime_enabled;
            self.ime_enabled = false;
            return Ok(InputOutcome {
                handled: true,
                changed,
                clipboard: None,
            });
        }
        if self.ime.preedit
            && editor.composition().is_none()
            && matches!(event, Ime::Preedit(..) | Ime::Commit(_))
        {
            // The host canceled this session. Keep its pending reset through the
            // empty preedit that winit sends before a queued commit.
            return Ok(InputOutcome {
                handled: true,
                ..InputOutcome::default()
            });
        }
        let before = editor.presentation();
        let was_enabled = self.ime_enabled;
        let mut preedit_changed = false;
        match event {
            Ime::Enabled => self.ime_enabled = true,
            Ime::Disabled => {
                self.ime_enabled = false;
                self.ime.stale = true;
                self.ime.preedit = false;
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
                let selection = selection.map(|(start, end)| start..end);
                if text.is_empty() {
                    self.ime.preedit = editor.composition().is_some();
                    if editor
                        .composition()
                        .is_some_and(|composition| !composition.text.is_empty())
                    {
                        editor.update_composition(text, selection)?;
                        preedit_changed = true;
                    }
                } else {
                    preedit_changed = editor.composition().is_none_or(|composition| {
                        composition.text != *text || composition.selection != selection
                    });
                    if preedit_changed {
                        editor.update_composition(text, selection)?;
                    }
                    self.ime.preedit = true;
                }
            }
            Ime::Commit(text) => {
                self.ime.preedit = false;
                let clean = clean_text(text, true);
                if clean.is_empty() && !text.is_empty() {
                    editor.cancel_composition();
                } else if !clean.is_empty() || editor.composition().is_some() {
                    editor.commit_composition(&clean)?;
                }
            }
        }
        Ok(InputOutcome {
            handled: true,
            changed: before != editor.presentation()
                || preedit_changed
                || was_enabled != self.ime_enabled,
            clipboard: None,
        })
    }

    pub fn paste(&mut self, editor: &mut Editor, text: &str) -> Result<InputOutcome, Error> {
        if !self.is_focused() {
            return Ok(InputOutcome::default());
        }
        let blocked = InputOutcome {
            handled: true,
            ..InputOutcome::default()
        };
        if self.read_only {
            return Ok(blocked);
        }
        let clean = clean_text(text, true);
        if clean.is_empty() {
            return Ok(blocked);
        }
        let before = editor.presentation();
        if !cancel_empty_composition(editor) {
            return Ok(blocked);
        }
        editor.isolated_edit(|editor| editor.insert_text(&clean))?;
        Ok(InputOutcome {
            handled: true,
            changed: before != editor.presentation(),
            clipboard: None,
        })
    }

    pub fn paste_fragment(
        &mut self,
        editor: &mut Editor,
        fragment: &Fragment,
    ) -> Result<InputOutcome, Error> {
        if !self.is_focused() {
            return Ok(InputOutcome::default());
        }
        let blocked = InputOutcome {
            handled: true,
            ..InputOutcome::default()
        };
        if self.read_only {
            return Ok(blocked);
        }
        let before = editor.presentation();
        if !cancel_empty_composition(editor) {
            return Ok(blocked);
        }
        editor.insert_fragment(fragment)?;
        Ok(InputOutcome {
            handled: true,
            changed: before != editor.presentation(),
            clipboard: None,
        })
    }

    fn command_pressed(&self) -> bool {
        match self.command_modifier {
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

    fn focus_changed(&mut self, was_focused: bool, editor: &mut Editor) -> bool {
        if self.is_focused() {
            if !was_focused {
                self.ime.stale = true;
            }
            return false;
        }
        if was_focused {
            editor.break_history_group();
        }
        self.ime_enabled = false;
        editor.cancel_composition()
    }

    fn observe_window_focus(&mut self) -> bool {
        let regained = self.window_unfocused && self.focused;
        self.window_unfocused = false;
        if regained {
            self.ime.stale = true;
        }
        regained
    }

    fn plan_ime(&mut self, editor: &Editor, window_focused: bool, area: ImeArea) -> ImeRequests {
        if window_focused {
            self.observe_window_focus();
        }
        let editable = self.is_focused() && !self.read_only;
        let ime = &mut self.ime;
        let mut requests = ImeRequests::default();
        if !editable {
            if ime.allowed {
                requests.allowed = Some(false);
            }
            *ime = ImeSync::default();
            return requests;
        }
        if ime.preedit && editor.composition().is_none() {
            requests.interrupt = true;
            ime.preedit = false;
        } else if !ime.allowed || ime.stale {
            requests.allowed = Some(true);
        }
        if requests.interrupt || requests.allowed.is_some() {
            ime.allowed = true;
            ime.stale = false;
            ime.area = None;
        }
        if ime.area != Some(area) {
            ime.area = Some(area);
            requests.area = Some(area);
        }
        requests
    }

    fn named_shortcut(&self, key: &Key, editor: &Editor) -> Option<Shortcut> {
        let Key::Named(named) = key else {
            return None;
        };
        let pc = self.command_modifier == CommandModifier::Control;
        let modifiers = self.modifiers;
        Some(match named {
            NamedKey::Copy => Shortcut::Copy,
            NamedKey::Cut => Shortcut::Cut,
            NamedKey::Paste => Shortcut::Paste,
            NamedKey::Undo => Shortcut::Undo,
            NamedKey::Redo => Shortcut::Redo,
            NamedKey::Insert if pc && modifiers == ModifiersState::CONTROL => Shortcut::Copy,
            NamedKey::Insert if pc && modifiers == ModifiersState::SHIFT => Shortcut::Paste,
            NamedKey::Delete
                if pc && modifiers == ModifiersState::SHIFT && !editor.selection().is_caret() =>
            {
                Shortcut::Cut
            }
            _ => return None,
        })
    }

    fn run_shortcut(
        &self,
        shortcut: Shortcut,
        editor: &mut Editor,
    ) -> Result<Option<ClipboardEvent>, Error> {
        if self.read_only && shortcut.edits() {
            return Ok(None);
        }
        match shortcut {
            Shortcut::Copy => return Ok(self.selection_event(editor, false)),
            Shortcut::Cut => {
                let cut = !self.read_only;
                let event = self.selection_event(editor, cut);
                if cut && event.is_some() {
                    editor.insert_text("")?;
                }
                return Ok(event);
            }
            Shortcut::Paste => return Ok(Some(ClipboardEvent::PasteRequest)),
            _ => shortcut.apply(editor)?,
        }
        Ok(None)
    }

    fn selection_event(&self, editor: &Editor, cut: bool) -> Option<ClipboardEvent> {
        if editor.selection().is_caret() {
            return None;
        }
        Some(match (cut, self.rich_clipboard) {
            (false, false) => ClipboardEvent::Copy(editor.selected_text()),
            (false, true) => ClipboardEvent::CopyRich(editor.selected_fragment()),
            (true, false) => ClipboardEvent::Cut(editor.selected_text()),
            (true, true) => ClipboardEvent::CutRich(editor.selected_fragment()),
        })
    }
}

fn shortcut_key(key: &Key, physical_key: PhysicalKey) -> Option<char> {
    let logical = match key {
        Key::Character(text) => {
            let mut characters = text.chars();
            characters.next().filter(|_| characters.next().is_none())
        }
        Key::Named(_) => return None,
        Key::Dead(_) | Key::Unidentified(_) => None,
    };
    shortcut_key_name(logical, physical_shortcut_key(physical_key))
}

fn physical_shortcut_key(physical_key: PhysicalKey) -> Option<char> {
    let PhysicalKey::Code(code) = physical_key else {
        return None;
    };
    Some(match code {
        KeyCode::KeyA => 'a',
        KeyCode::KeyB => 'b',
        KeyCode::KeyC => 'c',
        KeyCode::KeyD => 'd',
        KeyCode::KeyE => 'e',
        KeyCode::KeyF => 'f',
        KeyCode::KeyG => 'g',
        KeyCode::KeyH => 'h',
        KeyCode::KeyI => 'i',
        KeyCode::KeyJ => 'j',
        KeyCode::KeyK => 'k',
        KeyCode::KeyL => 'l',
        KeyCode::KeyM => 'm',
        KeyCode::KeyN => 'n',
        KeyCode::KeyO => 'o',
        KeyCode::KeyP => 'p',
        KeyCode::KeyQ => 'q',
        KeyCode::KeyR => 'r',
        KeyCode::KeyS => 's',
        KeyCode::KeyT => 't',
        KeyCode::KeyU => 'u',
        KeyCode::KeyV => 'v',
        KeyCode::KeyW => 'w',
        KeyCode::KeyX => 'x',
        KeyCode::KeyY => 'y',
        KeyCode::KeyZ => 'z',
        KeyCode::Digit0 => '0',
        KeyCode::Digit1 => '1',
        KeyCode::Digit2 => '2',
        KeyCode::Digit3 => '3',
        KeyCode::Digit4 => '4',
        KeyCode::Digit5 => '5',
        KeyCode::Digit6 => '6',
        KeyCode::Digit7 => '7',
        KeyCode::Digit8 => '8',
        KeyCode::Digit9 => '9',
        KeyCode::BracketLeft => '[',
        KeyCode::BracketRight => ']',
        _ => return None,
    })
}

fn cancel_empty_composition(editor: &mut Editor) -> bool {
    match editor.composition() {
        Some(composition) if !composition.text.is_empty() => false,
        Some(_) => {
            editor.cancel_composition();
            true
        }
        None => true,
    }
}

fn selection_contains_list(editor: &Editor) -> bool {
    editor.document().paragraphs()[editor.selected_paragraphs()]
        .iter()
        .any(|paragraph| {
            matches!(
                paragraph.kind(),
                ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. }
            )
        })
}

#[cfg(test)]
#[allow(unused_must_use)]
mod tests {
    use super::*;
    use crate::{Position, Selection, StylePatch};
    use std::mem::discriminant;
    use winit::keyboard::NativeKeyCode;

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
        let physical = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        adapter
            .handle_key(&key, physical, ElementState::Pressed, text, editor)
            .unwrap()
    }

    fn chord(
        adapter: &mut WinitAdapter,
        editor: &mut Editor,
        modifiers: ModifiersState,
        logical: &str,
        physical: KeyCode,
        text: Option<&str>,
    ) -> InputOutcome {
        adapter.set_modifiers(modifiers);
        adapter
            .handle_key(
                &Key::Character(logical.into()),
                PhysicalKey::Code(physical),
                ElementState::Pressed,
                text,
                editor,
            )
            .unwrap()
    }

    fn area(x: f64) -> ImeArea {
        (LogicalPosition::new(x, 4.0), LogicalSize::new(1.0, 16.0))
    }

    #[test]
    fn widget_and_window_blur_separate_typing_undo_groups() {
        for window_blur in [false, true] {
            let mut editor = Editor::default();
            let mut adapter = focused(&mut editor);
            for text in ["a", "b"] {
                key(
                    &mut adapter,
                    &mut editor,
                    Key::Character(text.into()),
                    Some(text),
                );
            }
            assert_eq!(editor.undo_len(), 1);

            if window_blur {
                adapter
                    .handle_window_event(&WindowEvent::Focused(false), &mut editor)
                    .unwrap();
            } else {
                adapter.set_focused(false, &mut editor);
            }
            assert!(!adapter.is_focused());
            assert!(
                !key(
                    &mut adapter,
                    &mut editor,
                    Key::Character("ignored".into()),
                    Some("ignored"),
                )
                .handled
            );
            if window_blur {
                adapter
                    .handle_window_event(&WindowEvent::Focused(true), &mut editor)
                    .unwrap();
            } else {
                adapter.set_focused(true, &mut editor);
            }
            for text in ["c", "d"] {
                key(
                    &mut adapter,
                    &mut editor,
                    Key::Character(text.into()),
                    Some(text),
                );
            }
            assert_eq!(editor.document().plain_text(), "abcd");
            assert_eq!(editor.undo_len(), 2);
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), "ab");
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), "");
        }
    }

    #[test]
    fn heading_shortcuts_follow_platform_command_modifiers_and_read_only_mode() {
        for (command_modifier, modifiers) in [
            (CommandModifier::Control, ModifiersState::CONTROL),
            (CommandModifier::Super, ModifiersState::SUPER),
        ] {
            let mut editor = Editor::from_text("first\nsecond");
            editor.select_all();
            let selection = editor.selection();
            let mut adapter = WinitAdapter::new().command_modifier(command_modifier);
            adapter.set_focused(true, &mut editor);
            adapter.set_modifiers(modifiers);
            for level in 1..=6 {
                let outcome = key(
                    &mut adapter,
                    &mut editor,
                    Key::Character(level.to_string().into()),
                    None,
                );
                assert!(outcome.handled && outcome.changed);
                assert!(
                    editor
                        .document()
                        .paragraphs()
                        .iter()
                        .all(|paragraph| { paragraph.kind() == ParagraphKind::Heading { level } })
                );
                assert_eq!(editor.selection(), selection);
            }
            assert!(key(&mut adapter, &mut editor, Key::Character("0".into()), None).changed);
            assert!(
                editor
                    .document()
                    .paragraphs()
                    .iter()
                    .all(|paragraph| { paragraph.kind() == ParagraphKind::Body })
            );
            assert!(editor.undo());
            assert!(
                editor
                    .document()
                    .paragraphs()
                    .iter()
                    .all(|paragraph| { paragraph.kind() == ParagraphKind::Heading { level: 6 } })
            );
            let history = editor.undo_len();
            let revision = editor.document().revision();
            adapter.set_read_only(true, &mut editor);
            for shortcut in ["0", "1", "2", "3", "4", "5", "6"] {
                let outcome = key(
                    &mut adapter,
                    &mut editor,
                    Key::Character(shortcut.into()),
                    None,
                );
                assert!(outcome.handled && !outcome.changed);
                assert_eq!(editor.document().revision(), revision);
                assert_eq!(editor.undo_len(), history);
                assert_eq!(editor.selection(), selection);
            }
        }
    }

    #[test]
    fn numeric_keys_and_altgr_text_do_not_trigger_heading_shortcuts() {
        let mut editor = Editor::default();
        let mut adapter = focused(&mut editor);
        key(
            &mut adapter,
            &mut editor,
            Key::Character("1".into()),
            Some("1"),
        );
        let ctrl_alt = chord(
            &mut adapter,
            &mut editor,
            ModifiersState::CONTROL | ModifiersState::ALT,
            "2",
            KeyCode::Digit2,
            Some("²"),
        );
        assert_eq!(ctrl_alt.handled, CTRL_ALT_IS_ALTGR);
        assert_eq!(
            editor.document().plain_text(),
            if CTRL_ALT_IS_ALTGR { "1²" } else { "1" }
        );
        assert_eq!(
            editor.document().paragraph(0).unwrap().kind(),
            ParagraphKind::Body
        );
        for modifiers in [
            ModifiersState::SUPER,
            ModifiersState::CONTROL | ModifiersState::SUPER,
        ] {
            let outcome = chord(
                &mut adapter,
                &mut editor,
                modifiers,
                "3",
                KeyCode::Digit3,
                Some("3"),
            );
            assert!(!outcome.handled && !outcome.changed);
        }
        let revision = editor.document().revision();
        adapter.set_read_only(true, &mut editor);
        let outcome = chord(
            &mut adapter,
            &mut editor,
            ModifiersState::CONTROL | ModifiersState::ALT,
            "4",
            KeyCode::Digit4,
            Some("⁴"),
        );
        assert_eq!(outcome.handled, CTRL_ALT_IS_ALTGR);
        assert!(!outcome.changed);
        assert_eq!(editor.document().revision(), revision);
    }

    #[test]
    fn super_backspace_deletes_to_paragraph_start_and_undo_restores_selection() {
        let text = "prefix\ncafé 👩🏽‍💻 tail";
        let end = Position::new(1, "café 👩🏽‍💻".len());
        let caret = Selection::caret(end);
        for selection in [
            caret,
            Selection::new(Position::new(0, 1), end),
            Selection::new(end, Position::new(0, 1)),
        ] {
            let mut editor = Editor::from_text(text);
            editor.set_selection(selection).unwrap();
            let mut adapter = WinitAdapter::new().command_modifier(CommandModifier::Super);
            adapter.set_focused(true, &mut editor);
            adapter.set_modifiers(ModifiersState::SUPER);
            let outcome = key(
                &mut adapter,
                &mut editor,
                Key::Named(NamedKey::Backspace),
                None,
            );
            assert!(outcome.handled && outcome.changed);
            assert_eq!(
                editor.document().plain_text(),
                if selection == caret {
                    "prefix\n tail"
                } else {
                    "p tail"
                }
            );
            assert_eq!(editor.undo_len(), 1);
            if selection == caret {
                let revision = editor.document().revision();
                let outcome = key(
                    &mut adapter,
                    &mut editor,
                    Key::Named(NamedKey::Backspace),
                    None,
                );
                assert!(outcome.handled && !outcome.changed);
                assert_eq!(editor.document().revision(), revision);
                assert_eq!(editor.undo_len(), 1);
            }
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), text);
            assert_eq!(editor.selection(), selection);
        }
    }

    #[test]
    fn super_backspace_respects_read_only_ime_and_modifier_guards() {
        let mut editor = Editor::from_text("one two");
        editor
            .set_selection(Selection::caret(editor.document().end()))
            .unwrap();
        let mut adapter = WinitAdapter::new().command_modifier(CommandModifier::Super);
        adapter.set_focused(true, &mut editor);
        adapter.set_modifiers(ModifiersState::SUPER);
        adapter.set_read_only(true, &mut editor);
        let outcome = key(
            &mut adapter,
            &mut editor,
            Key::Named(NamedKey::Backspace),
            None,
        );
        assert!(outcome.handled && !outcome.changed);
        adapter.set_read_only(false, &mut editor);
        editor.update_composition("候", None).unwrap();
        let outcome = key(
            &mut adapter,
            &mut editor,
            Key::Named(NamedKey::Backspace),
            None,
        );
        assert!(outcome.handled && !outcome.changed);
        assert!(editor.composition().is_some());
        editor.cancel_composition();
        assert_eq!(editor.document().plain_text(), "one two");
        assert!(!editor.can_undo());

        for (modifiers, expected) in [
            (ModifiersState::CONTROL | ModifiersState::ALT, "one "),
            (ModifiersState::CONTROL | ModifiersState::SUPER, "one tw"),
        ] {
            adapter.set_modifiers(modifiers);
            key(
                &mut adapter,
                &mut editor,
                Key::Named(NamedKey::Backspace),
                None,
            );
            assert_eq!(editor.document().plain_text(), expected);
            assert!(editor.undo());
        }
    }

    #[test]
    fn emphasis_shortcuts_normalize_mixed_selections_in_both_directions() {
        for (shortcut, patch) in [
            (
                "b",
                StylePatch {
                    bold: Some(true),
                    ..Default::default()
                },
            ),
            (
                "i",
                StylePatch {
                    italic: Some(true),
                    ..Default::default()
                },
            ),
            (
                "u",
                StylePatch {
                    underline: Some(true),
                    ..Default::default()
                },
            ),
        ] {
            for reverse in [false, true] {
                let mut editor = Editor::from_text("aé");
                editor
                    .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 1)))
                    .unwrap();
                editor.apply_style(patch).unwrap();
                let selection = if reverse {
                    Selection::new(editor.document().end(), Position::default())
                } else {
                    Selection::new(Position::default(), editor.document().end())
                };
                editor.set_selection(selection).unwrap();
                editor.clear_history();
                let original = editor.document().to_bytes();
                let mut adapter = focused(&mut editor);
                adapter.set_modifiers(ModifiersState::CONTROL);

                for enabled in [true, false] {
                    let outcome = key(
                        &mut adapter,
                        &mut editor,
                        Key::Character(shortcut.into()),
                        None,
                    );
                    assert!(outcome.handled && outcome.changed);
                    let style = editor.selection_style();
                    let actual = match shortcut {
                        "b" => style.bold,
                        "i" => style.italic,
                        _ => style.underline,
                    };
                    assert_eq!(actual, Some(enabled));
                    assert_eq!(editor.selection(), selection);
                }
                assert_eq!(editor.undo_len(), 2);
                assert!(editor.undo());
                assert!(editor.undo());
                assert_eq!(editor.document().to_bytes(), original);
                assert_eq!(editor.selection(), selection);
            }
        }
    }

    #[test]
    fn read_only_keeps_selection_and_copy_without_changing_history() {
        let mut editor = Editor::from_text("original");
        let mut adapter = focused(&mut editor);
        key(
            &mut adapter,
            &mut editor,
            Key::Character("a".into()),
            Some("a"),
        );
        assert!(editor.can_undo());
        assert!(adapter.set_read_only(true, &mut editor).changed);
        assert!(adapter.is_read_only());
        assert!(!adapter.set_read_only(true, &mut editor).changed);

        adapter.set_modifiers(ModifiersState::CONTROL);
        let select = key(&mut adapter, &mut editor, Key::Character("a".into()), None);
        assert!(select.handled && select.changed);
        let selection = editor.selection();
        for shortcut in ["c", "x"] {
            let outcome = key(
                &mut adapter,
                &mut editor,
                Key::Character(shortcut.into()),
                None,
            );
            assert!(outcome.handled && !outcome.changed);
            assert_eq!(
                outcome.clipboard,
                Some(ClipboardEvent::Copy("aoriginal".to_owned()))
            );
        }
        for shortcut in ["v", "z", "y", "b", "i", "u", "[", "]"] {
            let outcome = key(
                &mut adapter,
                &mut editor,
                Key::Character(shortcut.into()),
                None,
            );
            assert!(outcome.handled && !outcome.changed);
            assert_eq!(outcome.clipboard, None);
        }
        adapter.set_modifiers(ModifiersState::empty());
        for named in [NamedKey::Enter, NamedKey::Backspace, NamedKey::Delete] {
            let outcome = key(&mut adapter, &mut editor, Key::Named(named), None);
            assert!(outcome.handled && !outcome.changed);
        }
        let text = key(
            &mut adapter,
            &mut editor,
            Key::Character("x".into()),
            Some("x"),
        );
        assert!(text.handled && !text.changed);
        assert_eq!(editor.selection(), selection);
        assert!(adapter.paste(&mut editor, "replacement").unwrap().handled);
        let fragment = Fragment::from_text("replacement");
        assert!(
            adapter
                .paste_fragment(&mut editor, &fragment)
                .unwrap()
                .handled
        );
        assert_eq!(editor.document().plain_text(), "aoriginal");
        assert_eq!(editor.selection(), selection);

        let movement = key(
            &mut adapter,
            &mut editor,
            Key::Named(NamedKey::ArrowRight),
            None,
        );
        assert!(movement.handled && movement.changed);
        assert!(editor.selection().is_caret());
        assert_eq!(editor.selection().focus, editor.document().end());
        assert!(
            !key(
                &mut adapter,
                &mut editor,
                Key::Named(NamedKey::Tab),
                Some("\t")
            )
            .handled
        );
        assert!(adapter.set_read_only(false, &mut editor).changed);
        adapter.set_modifiers(ModifiersState::CONTROL);
        key(&mut adapter, &mut editor, Key::Character("z".into()), None);
        assert_eq!(editor.document().plain_text(), "original");
        assert!(!editor.can_undo());
    }

    #[test]
    fn read_only_cancels_preedit_and_rejects_ime_mutations() {
        let mut editor = Editor::from_text("original");
        let mut adapter = focused(&mut editor).rich_clipboard(true);
        editor.select_all();
        let selection = editor.selection();
        adapter.handle_ime(&Ime::Enabled, &mut editor).unwrap();
        adapter
            .handle_ime(&Ime::Preedit("candidate".to_owned(), None), &mut editor)
            .unwrap();
        assert!(adapter.set_read_only(true, &mut editor).changed);
        assert!(editor.composition().is_none());
        assert!(!adapter.ime_enabled());
        for event in [
            Ime::Enabled,
            Ime::Preedit("candidate".to_owned(), Some((0, 9))),
            Ime::Commit("replacement".to_owned()),
            Ime::Disabled,
        ] {
            let outcome = adapter.handle_ime(&event, &mut editor).unwrap();
            assert!(outcome.handled && !outcome.changed);
            assert!(!adapter.ime_enabled());
            assert!(editor.composition().is_none());
        }
        adapter.set_modifiers(ModifiersState::CONTROL);
        let outcome = key(&mut adapter, &mut editor, Key::Character("x".into()), None);
        let Some(ClipboardEvent::CopyRich(fragment)) = outcome.clipboard else {
            panic!("read-only cut should copy a rich fragment");
        };
        assert_eq!(fragment.plain_text(), "original");
        assert_eq!(editor.document().plain_text(), "original");
        assert_eq!(editor.selection(), selection);
        assert!(!editor.can_undo());
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
        key(
            &mut adapter,
            &mut editor,
            Key::Character("@".into()),
            Some("@"),
        );
        assert_eq!(editor.document().plain_text(), "´e\n@");
        adapter.set_modifiers(ModifiersState::CONTROL | ModifiersState::ALT);
        key(
            &mut adapter,
            &mut editor,
            Key::Character("€".into()),
            Some("€"),
        );
        let expected = if CTRL_ALT_IS_ALTGR {
            "´e\n@€"
        } else {
            "´e\n@"
        };
        assert_eq!(editor.document().plain_text(), expected);
        let outcome = key(
            &mut adapter,
            &mut editor,
            Key::Character("x".into()),
            if CTRL_ALT_IS_ALTGR { None } else { Some("x") },
        );
        assert!(!outcome.handled);
        assert_eq!(editor.document().plain_text(), expected);
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
            Some("a"),
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
    fn passive_keys_preserve_empty_preedit_and_its_following_commit() {
        for (logical, modifiers, text, host_vertical, handled) in [
            (
                Key::Named(NamedKey::Process),
                ModifiersState::empty(),
                None,
                false,
                true,
            ),
            (
                Key::Named(NamedKey::Shift),
                ModifiersState::SHIFT,
                None,
                false,
                false,
            ),
            (
                Key::Named(NamedKey::Control),
                ModifiersState::CONTROL,
                None,
                false,
                false,
            ),
            (
                Key::Named(NamedKey::Alt),
                ModifiersState::ALT,
                None,
                false,
                false,
            ),
            (
                Key::Named(NamedKey::Super),
                ModifiersState::SUPER,
                None,
                false,
                false,
            ),
            (
                Key::Named(NamedKey::F1),
                ModifiersState::empty(),
                None,
                false,
                false,
            ),
            (
                Key::Dead(Some('^')),
                ModifiersState::empty(),
                None,
                false,
                false,
            ),
            (
                Key::Character("k".into()),
                ModifiersState::CONTROL,
                None,
                false,
                false,
            ),
            (
                Key::Character("x".into()),
                ModifiersState::empty(),
                Some("\0\u{7}"),
                false,
                false,
            ),
            (
                Key::Character("x".into()),
                ModifiersState::empty(),
                None,
                false,
                false,
            ),
            (
                Key::Named(NamedKey::Tab),
                ModifiersState::empty(),
                None,
                false,
                false,
            ),
            (
                Key::Named(NamedKey::PageUp),
                ModifiersState::empty(),
                None,
                false,
                false,
            ),
            (
                Key::Named(NamedKey::PageDown),
                ModifiersState::empty(),
                None,
                false,
                false,
            ),
            (
                Key::Named(NamedKey::ArrowUp),
                ModifiersState::empty(),
                None,
                true,
                false,
            ),
            (
                Key::Named(NamedKey::ArrowDown),
                ModifiersState::empty(),
                None,
                true,
                false,
            ),
        ] {
            for sync_after_key in [false, true] {
                let mut editor = Editor::from_text("abc");
                editor
                    .set_selection(Selection::new(Position::new(0, 2), Position::new(0, 0)))
                    .unwrap();
                let selection = editor.selection();
                let mut adapter = focused(&mut editor).host_vertical_navigation(host_vertical);
                adapter.plan_ime(&editor, false, area(1.0));
                adapter
                    .handle_ime(&Ime::Preedit("candidate".into(), None), &mut editor)
                    .unwrap();
                adapter
                    .handle_ime(&Ime::Preedit(String::new(), None), &mut editor)
                    .unwrap();
                adapter.set_modifiers(modifiers);
                let outcome = key(&mut adapter, &mut editor, logical.clone(), text);
                assert_eq!(outcome.handled, handled, "{logical:?}");
                assert!(!outcome.changed, "{logical:?}");
                assert_eq!(editor.selection(), selection);
                assert!(editor.composition().is_some(), "{logical:?}");
                if sync_after_key {
                    assert_eq!(
                        adapter.plan_ime(&editor, false, area(1.0)),
                        ImeRequests::default(),
                        "{logical:?}"
                    );
                }
                let commit = adapter
                    .handle_ime(&Ime::Commit("é".into()), &mut editor)
                    .unwrap();
                assert!(commit.changed, "{logical:?}");
                assert_eq!(editor.document().plain_text(), "éc");
                assert!(editor.undo());
                assert_eq!(editor.document().plain_text(), "abc");
                assert_eq!(editor.selection(), selection);
            }
        }
    }

    #[test]
    fn caret_clipboard_shortcuts_preserve_empty_preedit_and_its_commit() {
        for (logical, modifiers) in [
            (Key::Character("c".into()), ModifiersState::CONTROL),
            (Key::Character("x".into()), ModifiersState::CONTROL),
            (Key::Named(NamedKey::Copy), ModifiersState::empty()),
            (Key::Named(NamedKey::Cut), ModifiersState::empty()),
            (Key::Named(NamedKey::Insert), ModifiersState::CONTROL),
        ] {
            for rich_clipboard in [false, true] {
                for sync_after_key in [false, true] {
                    let mut editor = Editor::from_text("ab");
                    let selection = Selection::caret(Position::new(0, 1));
                    editor.set_selection(selection).unwrap();
                    let revision = editor.document().revision();
                    let mut adapter = focused(&mut editor).rich_clipboard(rich_clipboard);
                    adapter.plan_ime(&editor, false, area(1.0));
                    adapter
                        .handle_ime(&Ime::Preedit("候".into(), None), &mut editor)
                        .unwrap();
                    adapter
                        .handle_ime(&Ime::Preedit(String::new(), None), &mut editor)
                        .unwrap();
                    let composition = editor.composition().cloned();
                    adapter.set_modifiers(modifiers);
                    let outcome = key(&mut adapter, &mut editor, logical.clone(), None);
                    assert!(outcome.handled && !outcome.changed, "{logical:?}");
                    assert!(outcome.clipboard.is_none(), "{logical:?}");
                    assert_eq!(editor.composition(), composition.as_ref(), "{logical:?}");
                    assert_eq!(editor.selection(), selection);
                    assert_eq!(editor.document().revision(), revision);
                    assert_eq!((editor.undo_len(), editor.redo_len()), (0, 0));
                    if sync_after_key {
                        assert_eq!(
                            adapter.plan_ime(&editor, false, area(1.0)),
                            ImeRequests::default(),
                            "{logical:?}"
                        );
                    }
                    let outcome = adapter
                        .handle_ime(&Ime::Commit("候".into()), &mut editor)
                        .unwrap();
                    assert!(outcome.handled && outcome.changed, "{logical:?}");
                    assert_eq!(editor.document().plain_text(), "a候b");
                    assert!(editor.composition().is_none());
                    assert!(editor.undo());
                    assert_eq!(editor.document().plain_text(), "ab");
                    assert_eq!(editor.selection(), selection);
                    assert!(!editor.can_undo());
                }
            }
        }
    }

    #[test]
    fn accepted_keys_cancel_empty_preedit_and_reject_its_queued_commit() {
        for (logical, modifiers, text) in [
            (
                Key::Named(NamedKey::ArrowLeft),
                ModifiersState::empty(),
                None,
            ),
            (Key::Named(NamedKey::Enter), ModifiersState::empty(), None),
            (Key::Named(NamedKey::Enter), ModifiersState::SHIFT, None),
            (
                Key::Named(NamedKey::Backspace),
                ModifiersState::empty(),
                None,
            ),
            (
                Key::Named(NamedKey::Backspace),
                ModifiersState::CONTROL,
                None,
            ),
            (Key::Named(NamedKey::Delete), ModifiersState::empty(), None),
            (Key::Named(NamedKey::Delete), ModifiersState::CONTROL, None),
            (Key::Named(NamedKey::Tab), ModifiersState::empty(), None),
            (Key::Named(NamedKey::Tab), ModifiersState::SHIFT, None),
            (Key::Named(NamedKey::Copy), ModifiersState::empty(), None),
            (Key::Named(NamedKey::Paste), ModifiersState::empty(), None),
            (Key::Character("b".into()), ModifiersState::CONTROL, None),
            (
                Key::Character("x".into()),
                ModifiersState::empty(),
                Some("x"),
            ),
        ] {
            let mut editor = Editor::from_text("abc");
            editor
                .set_paragraph_kind(ParagraphKind::Bullet { indent: 1 })
                .unwrap();
            editor
                .set_selection(Selection::new(Position::new(0, 2), Position::new(0, 0)))
                .unwrap();
            let mut adapter = focused(&mut editor);
            adapter.plan_ime(&editor, false, area(1.0));
            adapter
                .handle_ime(&Ime::Preedit("candidate".into(), None), &mut editor)
                .unwrap();
            adapter
                .handle_ime(&Ime::Preedit(String::new(), None), &mut editor)
                .unwrap();
            adapter.set_modifiers(modifiers);
            let outcome = key(&mut adapter, &mut editor, logical.clone(), text);
            assert!(outcome.handled && outcome.changed, "{logical:?}");
            assert!(editor.composition().is_none(), "{logical:?}");
            let before = editor.document().to_bytes();
            let selection = editor.selection();
            let history = (editor.undo_len(), editor.redo_len());
            let commit = adapter
                .handle_ime(&Ime::Commit("candidate".into()), &mut editor)
                .unwrap();
            assert!(commit.handled && !commit.changed, "{logical:?}");
            assert_eq!(editor.document().to_bytes(), before);
            assert_eq!(editor.selection(), selection);
            assert_eq!((editor.undo_len(), editor.redo_len()), history);
            assert!(adapter.plan_ime(&editor, false, area(1.0)).interrupt);
        }
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

    #[test]
    fn focused_window_input_corrects_a_spurious_startup_blur() {
        let mut editor = Editor::default();
        let mut adapter = WinitAdapter::new().command_modifier(CommandModifier::Super);
        adapter.set_focused(true, &mut editor);
        let blur = adapter
            .handle_window_event(&WindowEvent::Focused(false), &mut editor)
            .unwrap();
        assert!(blur.changed && !adapter.is_focused());
        assert!(adapter.observe_window_focus());
        assert!(adapter.is_focused());
        let typed = key(
            &mut adapter,
            &mut editor,
            Key::Character("a".into()),
            Some("a"),
        );
        assert!(typed.handled && typed.changed);
        assert_eq!(editor.document().plain_text(), "a");

        adapter
            .handle_window_event(&WindowEvent::Focused(false), &mut editor)
            .unwrap();
        assert_eq!(
            adapter.plan_ime(&editor, false, area(1.0)),
            ImeRequests::default()
        );
        assert!(!adapter.is_focused());
        let requests = adapter.plan_ime(&editor, true, area(1.0));
        assert!(adapter.is_focused());
        assert_eq!(requests.allowed, Some(true));

        adapter.set_focused(false, &mut editor);
        adapter
            .handle_window_event(&WindowEvent::Focused(false), &mut editor)
            .unwrap();
        assert!(!adapter.observe_window_focus());
        adapter.set_focused(true, &mut editor);
        assert!(adapter.is_focused());
    }

    #[test]
    fn ctrl_alt_letters_stay_with_the_host_outside_windows_altgr() {
        for command_modifier in [CommandModifier::Control, CommandModifier::Super] {
            for read_only in [false, true] {
                let mut editor = Editor::default();
                let mut adapter = WinitAdapter::new().command_modifier(command_modifier);
                adapter.set_focused(true, &mut editor);
                adapter.set_read_only(read_only, &mut editor);
                for (logical, physical) in [("t", KeyCode::KeyT), ("1", KeyCode::Digit1)] {
                    let text = (!CTRL_ALT_IS_ALTGR).then_some(logical);
                    let outcome = chord(
                        &mut adapter,
                        &mut editor,
                        ModifiersState::CONTROL | ModifiersState::ALT,
                        logical,
                        physical,
                        text,
                    );
                    assert!(!outcome.handled && !outcome.changed);
                    assert_eq!(editor.document().plain_text(), "");
                }
            }
        }
    }

    #[test]
    fn non_latin_layouts_use_physical_shortcut_keys() {
        let control = ModifiersState::CONTROL;
        let shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let mut editor = Editor::from_text("text");
        editor.select_all();
        let mut adapter = focused(&mut editor);
        for logical in ["с", "ψ"] {
            let copy = chord(
                &mut adapter,
                &mut editor,
                control,
                logical,
                KeyCode::KeyC,
                Some(logical),
            );
            assert!(copy.handled);
            assert_eq!(copy.clipboard, Some(ClipboardEvent::Copy("text".into())));
        }
        editor
            .set_selection(Selection::caret(Position::default()))
            .unwrap();
        chord(&mut adapter, &mut editor, control, "ф", KeyCode::KeyA, None);
        assert_eq!(editor.selected_text(), "text");
        chord(&mut adapter, &mut editor, control, "и", KeyCode::KeyB, None);
        assert_eq!(editor.selection_style().bold, Some(true));

        let kind = |editor: &Editor| editor.document().paragraph(0).unwrap().kind();
        chord(
            &mut adapter,
            &mut editor,
            control,
            "&",
            KeyCode::Digit1,
            None,
        );
        assert_eq!(kind(&editor), ParagraphKind::Heading { level: 1 });
        chord(
            &mut adapter,
            &mut editor,
            control,
            "é",
            KeyCode::Digit2,
            None,
        );
        assert_eq!(kind(&editor), ParagraphKind::Heading { level: 2 });
        chord(&mut adapter, &mut editor, shift, "8", KeyCode::Digit8, None);
        assert_eq!(kind(&editor), ParagraphKind::Bullet { indent: 0 });
        chord(&mut adapter, &mut editor, shift, "/", KeyCode::Digit7, None);
        assert_eq!(
            kind(&editor),
            ParagraphKind::Ordered {
                indent: 0,
                start: Some(1)
            }
        );
        chord(&mut adapter, &mut editor, shift, "(", KeyCode::Digit8, None);
        assert_eq!(kind(&editor), ParagraphKind::Bullet { indent: 0 });
        chord(
            &mut adapter,
            &mut editor,
            control,
            "ъ",
            KeyCode::BracketRight,
            None,
        );
        assert_eq!(kind(&editor), ParagraphKind::Bullet { indent: 1 });
        chord(
            &mut adapter,
            &mut editor,
            control,
            "х",
            KeyCode::BracketLeft,
            None,
        );
        assert_eq!(kind(&editor), ParagraphKind::Bullet { indent: 0 });
        let undo = chord(&mut adapter, &mut editor, control, "я", KeyCode::KeyZ, None);
        assert!(undo.handled && undo.changed);
        assert_eq!(kind(&editor), ParagraphKind::Bullet { indent: 1 });

        adapter.set_read_only(true, &mut editor);
        let paste = chord(&mut adapter, &mut editor, control, "м", KeyCode::KeyV, None);
        assert!(paste.handled && !paste.changed);
        assert_eq!(paste.clipboard, None);
    }

    #[test]
    fn punctuation_without_an_egui_key_uses_its_physical_key() {
        let control = ModifiersState::CONTROL;
        let shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let mut editor = Editor::from_text("text");
        editor.select_all();
        editor
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
            .unwrap();
        let mut adapter = focused(&mut editor);
        let kind = |editor: &Editor| editor.document().paragraph(0).unwrap().kind();
        let indent = chord(
            &mut adapter,
            &mut editor,
            control,
            "$",
            KeyCode::BracketRight,
            None,
        );
        assert!(indent.handled && indent.changed);
        assert_eq!(kind(&editor), ParagraphKind::Bullet { indent: 1 });
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: None,
            })
            .unwrap();
        let star = chord(
            &mut adapter,
            &mut editor,
            shift,
            "*",
            KeyCode::BracketRight,
            None,
        );
        assert!(!star.handled && !star.changed);
        assert!(matches!(kind(&editor), ParagraphKind::Ordered { .. }));
        adapter.set_modifiers(shift);
        let unidentified = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        for (logical, expected) in [
            ("*", ParagraphKind::Bullet { indent: 0 }),
            (
                "&",
                ParagraphKind::Ordered {
                    indent: 0,
                    start: Some(1),
                },
            ),
        ] {
            let outcome = adapter
                .handle_key(
                    &Key::Character(logical.into()),
                    unidentified,
                    ElementState::Pressed,
                    None,
                    &mut editor,
                )
                .unwrap();
            assert!(outcome.handled && outcome.changed, "{logical}");
            assert_eq!(kind(&editor), expected);
        }
    }

    #[test]
    fn latin_logical_keys_keep_their_shortcuts_on_remapped_layouts() {
        let control = ModifiersState::CONTROL;
        let mut editor = Editor::from_text("text");
        editor.select_all();
        let mut adapter = focused(&mut editor);
        let j = chord(&mut adapter, &mut editor, control, "j", KeyCode::KeyC, None);
        assert!(!j.handled && j.clipboard.is_none());
        let copy = chord(&mut adapter, &mut editor, control, "c", KeyCode::KeyI, None);
        assert_eq!(copy.clipboard, Some(ClipboardEvent::Copy("text".into())));
        assert_eq!(editor.selection_style().italic, Some(false));
        editor
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
            .unwrap();
        let plus = chord(
            &mut adapter,
            &mut editor,
            control,
            "+",
            KeyCode::BracketRight,
            None,
        );
        assert!(!plus.handled);
        assert_eq!(
            editor.document().paragraph(0).unwrap().kind(),
            ParagraphKind::Bullet { indent: 0 }
        );
    }

    #[test]
    fn command_shift_chords_without_shift_bindings_reach_the_host() {
        for read_only in [false, true] {
            let mut editor = Editor::from_text("text");
            editor.select_all();
            let mut adapter = focused(&mut editor);
            adapter.set_read_only(read_only, &mut editor);
            let revision = editor.document().revision();
            let shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
            for (logical, physical) in [
                ("A", KeyCode::KeyA),
                ("C", KeyCode::KeyC),
                ("X", KeyCode::KeyX),
                ("V", KeyCode::KeyV),
                ("Y", KeyCode::KeyY),
                ("B", KeyCode::KeyB),
                ("I", KeyCode::KeyI),
                ("U", KeyCode::KeyU),
                ("!", KeyCode::Digit1),
                ("}", KeyCode::BracketRight),
            ] {
                let outcome = chord(&mut adapter, &mut editor, shift, logical, physical, None);
                assert!(!outcome.handled && !outcome.changed, "{logical}");
                assert_eq!(outcome.clipboard, None);
                assert_eq!(editor.document().revision(), revision);
                assert_eq!(editor.selected_text(), "text");
            }
        }
    }

    #[test]
    fn command_shortcuts_run_in_both_modifier_modes() {
        for (command_modifier, command) in [
            (CommandModifier::Control, ModifiersState::CONTROL),
            (CommandModifier::Super, ModifiersState::SUPER),
        ] {
            let shift = command | ModifiersState::SHIFT;
            let mut editor = Editor::from_text("one\ntwo");
            let mut adapter = WinitAdapter::new().command_modifier(command_modifier);
            adapter.set_focused(true, &mut editor);
            let select = chord(
                &mut adapter,
                &mut editor,
                command,
                "a",
                KeyCode::KeyA,
                Some("a"),
            );
            assert!(select.handled && select.changed);
            let copy = chord(
                &mut adapter,
                &mut editor,
                command,
                "c",
                KeyCode::KeyC,
                Some("c"),
            );
            assert_eq!(
                copy.clipboard,
                Some(ClipboardEvent::Copy("one\ntwo".into()))
            );

            for (modifiers, logical, physical, kind) in [
                (
                    command,
                    "1",
                    KeyCode::Digit1,
                    ParagraphKind::Heading { level: 1 },
                ),
                (command, "0", KeyCode::Digit0, ParagraphKind::Body),
                (
                    shift,
                    "&",
                    KeyCode::Digit7,
                    ParagraphKind::Ordered {
                        indent: 0,
                        start: Some(1),
                    },
                ),
                (
                    shift,
                    "*",
                    KeyCode::Digit8,
                    ParagraphKind::Bullet { indent: 0 },
                ),
                (
                    command,
                    "]",
                    KeyCode::BracketRight,
                    ParagraphKind::Bullet { indent: 1 },
                ),
                (
                    command,
                    "[",
                    KeyCode::BracketLeft,
                    ParagraphKind::Bullet { indent: 0 },
                ),
            ] {
                let outcome = chord(
                    &mut adapter,
                    &mut editor,
                    modifiers,
                    logical,
                    physical,
                    None,
                );
                assert!(outcome.handled && outcome.changed, "{logical}");
                let second = editor.document().paragraph(1).unwrap().kind();
                assert_eq!(editor.document().paragraph(0).unwrap().kind(), kind);
                assert_eq!(discriminant(&second), discriminant(&kind));
            }
            let bold = chord(&mut adapter, &mut editor, command, "b", KeyCode::KeyB, None);
            assert!(bold.handled && bold.changed);
            assert_eq!(editor.selection_style().bold, Some(true));

            let cut = chord(
                &mut adapter,
                &mut editor,
                command,
                "x",
                KeyCode::KeyX,
                Some("x"),
            );
            assert_eq!(cut.clipboard, Some(ClipboardEvent::Cut("one\ntwo".into())));
            assert_eq!(editor.document().plain_text(), "");
            chord(
                &mut adapter,
                &mut editor,
                command,
                "z",
                KeyCode::KeyZ,
                Some("z"),
            );
            assert_eq!(editor.document().plain_text(), "one\ntwo");
            chord(
                &mut adapter,
                &mut editor,
                shift,
                "Z",
                KeyCode::KeyZ,
                Some("Z"),
            );
            assert_eq!(editor.document().plain_text(), "");
            chord(
                &mut adapter,
                &mut editor,
                command,
                "z",
                KeyCode::KeyZ,
                Some("z"),
            );
            chord(
                &mut adapter,
                &mut editor,
                command,
                "y",
                KeyCode::KeyY,
                Some("y"),
            );
            assert_eq!(editor.document().plain_text(), "");
            let paste = chord(
                &mut adapter,
                &mut editor,
                command,
                "v",
                KeyCode::KeyV,
                Some("v"),
            );
            assert_eq!(paste.clipboard, Some(ClipboardEvent::PasteRequest));
        }
    }

    #[test]
    fn navigation_keys_move_and_extend_in_both_modifier_modes() {
        use Movement::*;
        let text = "one two\nthree four\nfive";
        let start = Position::new(1, 6);
        for command_modifier in [CommandModifier::Control, CommandModifier::Super] {
            let (command, word) = match command_modifier {
                CommandModifier::Control => (ModifiersState::CONTROL, ModifiersState::CONTROL),
                CommandModifier::Super => (ModifiersState::SUPER, ModifiersState::ALT),
            };
            let none = ModifiersState::empty();
            let mut cases = vec![
                (none, NamedKey::ArrowLeft, GraphemeBackward),
                (none, NamedKey::ArrowRight, GraphemeForward),
                (word, NamedKey::ArrowLeft, WordBackward),
                (word, NamedKey::ArrowRight, WordForward),
                (none, NamedKey::ArrowUp, ParagraphUp),
                (none, NamedKey::ArrowDown, ParagraphDown),
                (none, NamedKey::Home, ParagraphStart),
                (none, NamedKey::End, ParagraphEnd),
                (command, NamedKey::ArrowUp, DocumentStart),
                (command, NamedKey::ArrowDown, DocumentEnd),
                (command, NamedKey::Home, DocumentStart),
                (command, NamedKey::End, DocumentEnd),
            ];
            if command_modifier == CommandModifier::Super {
                cases.push((command, NamedKey::ArrowLeft, ParagraphStart));
                cases.push((command, NamedKey::ArrowRight, ParagraphEnd));
            }
            for (modifiers, named, movement) in cases {
                for extend in [false, true] {
                    let mut expected = Editor::from_text(text);
                    expected.set_selection(Selection::caret(start)).unwrap();
                    expected.move_cursor(movement, extend).unwrap();
                    let mut editor = Editor::from_text(text);
                    editor.set_selection(Selection::caret(start)).unwrap();
                    let mut adapter = WinitAdapter::new().command_modifier(command_modifier);
                    adapter.set_focused(true, &mut editor);
                    adapter.set_modifiers(if extend {
                        modifiers | ModifiersState::SHIFT
                    } else {
                        modifiers
                    });
                    let outcome = key(&mut adapter, &mut editor, Key::Named(named), None);
                    assert!(
                        outcome.handled && outcome.changed,
                        "{named:?} {modifiers:?}"
                    );
                    assert_eq!(editor.selection(), expected.selection(), "{named:?}");
                    assert_eq!(editor.selection().anchor == start, extend);
                }
            }
        }
    }

    #[test]
    fn host_vertical_navigation_leaves_line_and_page_keys_to_the_host() {
        let caret = Selection::caret(Position::new(0, 1));
        for host_vertical in [false, true] {
            let mut editor = Editor::from_text("one\ntwo");
            editor.set_selection(caret).unwrap();
            let mut adapter = WinitAdapter::new()
                .command_modifier(CommandModifier::Control)
                .host_vertical_navigation(host_vertical);
            adapter.set_focused(true, &mut editor);
            for modifiers in [ModifiersState::empty(), ModifiersState::SHIFT] {
                adapter.set_modifiers(modifiers);
                for page in [NamedKey::PageDown, NamedKey::PageUp] {
                    assert!(!key(&mut adapter, &mut editor, Key::Named(page), None).handled);
                    assert_eq!(editor.selection(), caret);
                }
                let down = key(
                    &mut adapter,
                    &mut editor,
                    Key::Named(NamedKey::ArrowDown),
                    None,
                );
                assert_eq!(down.handled, !host_vertical);
                if host_vertical {
                    assert_eq!(editor.selection(), caret);
                } else {
                    assert_eq!(editor.selection().focus, Position::new(1, 1));
                    editor.set_selection(caret).unwrap();
                }
            }
            adapter.set_modifiers(ModifiersState::CONTROL);
            let end = key(
                &mut adapter,
                &mut editor,
                Key::Named(NamedKey::ArrowDown),
                None,
            );
            assert!(end.handled);
            assert_eq!(editor.selection().focus, editor.document().end());
        }
    }

    #[test]
    fn widget_refocus_keeps_held_modifiers_and_window_blur_clears_them() {
        let mut editor = Editor::from_text("doc");
        let mut adapter = WinitAdapter::new().command_modifier(CommandModifier::Super);
        adapter.set_focused(true, &mut editor);
        adapter.set_modifiers(ModifiersState::SUPER);
        adapter.set_focused(false, &mut editor);
        adapter.set_focused(true, &mut editor);
        let paste = key(
            &mut adapter,
            &mut editor,
            Key::Character("v".into()),
            Some("v"),
        );
        assert_eq!(paste.clipboard, Some(ClipboardEvent::PasteRequest));
        assert_eq!(editor.document().plain_text(), "doc");
        adapter
            .handle_window_event(&WindowEvent::Focused(false), &mut editor)
            .unwrap();
        assert_eq!(adapter.modifiers(), ModifiersState::empty());
    }

    #[test]
    fn paste_cancels_empty_preedit_and_is_blocked_by_visible_preedit() {
        for rich in [false, true] {
            let paste = |adapter: &mut WinitAdapter, editor: &mut Editor| {
                if rich {
                    adapter.paste_fragment(editor, &Fragment::from_text("x"))
                } else {
                    adapter.paste(editor, "x")
                }
            };
            let mut editor = Editor::from_text("ab");
            let replacement = Selection::new(Position::new(0, 0), Position::new(0, 1));
            editor.set_selection(replacement).unwrap();
            let mut adapter = focused(&mut editor);
            adapter
                .handle_ime(&Ime::Preedit("n".into(), Some((1, 1))), &mut editor)
                .unwrap();
            let blocked = paste(&mut adapter, &mut editor).unwrap();
            assert!(blocked.handled && !blocked.changed);
            assert_eq!(editor.composition().unwrap().text, "n");
            assert_eq!(editor.document().plain_text(), "ab");

            adapter
                .handle_ime(&Ime::Preedit(String::new(), None), &mut editor)
                .unwrap();
            let pasted = paste(&mut adapter, &mut editor).unwrap();
            assert!(pasted.handled && pasted.changed);
            assert!(editor.composition().is_none());
            assert_eq!(editor.document().plain_text(), "xb");
            assert_eq!(editor.undo_len(), 1);
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), "ab");
            assert_eq!(editor.selection(), replacement);
        }
    }

    #[test]
    fn repeated_empty_preedits_report_no_change() {
        let mut editor = Editor::from_text("ab");
        let mut adapter = focused(&mut editor);
        let ime = |adapter: &mut WinitAdapter, editor: &mut Editor, event| {
            adapter.handle_ime(&event, editor).unwrap().changed
        };
        assert!(ime(
            &mut adapter,
            &mut editor,
            Ime::Preedit("n".into(), Some((1, 1)))
        ));
        assert!(ime(
            &mut adapter,
            &mut editor,
            Ime::Preedit(String::new(), None)
        ));
        for _ in 0..3 {
            assert!(!ime(
                &mut adapter,
                &mut editor,
                Ime::Preedit(String::new(), Some((0, 0)))
            ));
            assert!(!ime(
                &mut adapter,
                &mut editor,
                Ime::Preedit(String::new(), None)
            ));
        }
        assert!(ime(&mut adapter, &mut editor, Ime::Commit("ん".into())));
        assert_eq!(editor.document().plain_text(), "んab");
    }

    #[test]
    fn ime_commit_filters_control_characters() {
        let replacement = Selection::new(Position::new(0, 0), Position::new(0, 1));
        let mut editor = Editor::from_text("ab");
        editor.set_selection(replacement).unwrap();
        let mut adapter = focused(&mut editor);
        adapter
            .handle_ime(&Ime::Preedit("x".into(), None), &mut editor)
            .unwrap();
        let outcome = adapter
            .handle_ime(&Ime::Commit("\u{8}\u{16}".into()), &mut editor)
            .unwrap();
        assert!(outcome.changed);
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "ab");
        assert_eq!(editor.selection(), replacement);
        assert!(!editor.can_undo());

        adapter
            .handle_ime(&Ime::Commit("a\u{8}b\nc".into()), &mut editor)
            .unwrap();
        assert_eq!(editor.document().plain_text(), "ab\ncb");
    }

    #[test]
    fn sync_ime_sends_only_changes_and_releases_once() {
        let mut editor = Editor::default();
        let mut adapter = WinitAdapter::new();
        let allow = |area| ImeRequests {
            allowed: Some(true),
            area: Some(area),
            ..ImeRequests::default()
        };
        let release = ImeRequests {
            allowed: Some(false),
            ..ImeRequests::default()
        };
        let none = ImeRequests::default();
        assert_eq!(adapter.plan_ime(&editor, false, area(1.0)), none);
        adapter.set_focused(true, &mut editor);
        assert_eq!(
            adapter.plan_ime(&editor, false, area(1.0)),
            allow(area(1.0))
        );
        assert!(!adapter.ime_enabled());
        assert_eq!(adapter.plan_ime(&editor, false, area(1.0)), none);
        assert_eq!(
            adapter.plan_ime(&editor, false, area(2.0)),
            ImeRequests {
                area: Some(area(2.0)),
                ..ImeRequests::default()
            }
        );
        adapter.handle_ime(&Ime::Disabled, &mut editor).unwrap();
        assert_eq!(
            adapter.plan_ime(&editor, false, area(2.0)),
            allow(area(2.0))
        );

        adapter.set_focused(false, &mut editor);
        assert_eq!(adapter.plan_ime(&editor, false, area(2.0)), release);
        assert_eq!(adapter.plan_ime(&editor, false, area(2.0)), none);
        adapter.set_focused(true, &mut editor);
        assert_eq!(
            adapter.plan_ime(&editor, false, area(2.0)),
            allow(area(2.0))
        );
        adapter.set_focused(false, &mut editor);
        adapter.set_focused(true, &mut editor);
        assert_eq!(
            adapter.plan_ime(&editor, false, area(2.0)),
            allow(area(2.0))
        );

        adapter.set_read_only(true, &mut editor);
        assert_eq!(adapter.plan_ime(&editor, false, area(2.0)), release);
        adapter.set_read_only(false, &mut editor);
        assert_eq!(
            adapter.plan_ime(&editor, false, area(2.0)),
            allow(area(2.0))
        );
        adapter
            .handle_window_event(&WindowEvent::Focused(false), &mut editor)
            .unwrap();
        assert_eq!(adapter.plan_ime(&editor, false, area(2.0)), release);
        adapter
            .handle_window_event(&WindowEvent::Focused(true), &mut editor)
            .unwrap();
        assert_eq!(
            adapter.plan_ime(&editor, false, area(2.0)),
            allow(area(2.0))
        );
    }

    #[test]
    fn dropped_preedit_is_interrupted_instead_of_restored() {
        let interrupt = |area| ImeRequests {
            interrupt: true,
            area: Some(area),
            ..ImeRequests::default()
        };
        for host_click in [false, true] {
            let mut editor = Editor::from_text("abc");
            let mut adapter = focused(&mut editor);
            adapter.plan_ime(&editor, false, area(1.0));
            adapter
                .handle_ime(&Ime::Preedit("ni".into(), Some((2, 2))), &mut editor)
                .unwrap();
            if host_click {
                editor
                    .set_selection(Selection::caret(Position::new(0, 3)))
                    .unwrap();
            } else {
                let escape = key(
                    &mut adapter,
                    &mut editor,
                    Key::Named(NamedKey::Escape),
                    None,
                );
                assert!(escape.handled && escape.changed);
            }
            assert!(editor.composition().is_none());
            let resent = adapter
                .handle_ime(&Ime::Preedit("nih".into(), Some((3, 3))), &mut editor)
                .unwrap();
            assert!(resent.handled && !resent.changed);
            assert!(editor.composition().is_none());
            assert_eq!(
                adapter.plan_ime(&editor, false, area(1.0)),
                interrupt(area(1.0))
            );
            assert_eq!(
                adapter.plan_ime(&editor, false, area(1.0)),
                ImeRequests::default()
            );
            adapter
                .handle_ime(&Ime::Preedit("k".into(), None), &mut editor)
                .unwrap();
            assert_eq!(editor.composition().unwrap().text, "k");
            assert_eq!(editor.document().plain_text(), "abc");
        }
    }

    #[test]
    fn canceled_preedit_ignores_queued_commits_until_ime_is_interrupted() {
        for host_click in [false, true] {
            for (clear_before_cancel, clear_after_cancel) in
                [(false, false), (false, true), (true, false)]
            {
                let mut editor = Editor::from_text("abc");
                let mut adapter = focused(&mut editor);
                adapter.plan_ime(&editor, false, area(1.0));
                adapter
                    .handle_ime(&Ime::Preedit("ni".into(), None), &mut editor)
                    .unwrap();
                if clear_before_cancel {
                    adapter
                        .handle_ime(&Ime::Preedit(String::new(), None), &mut editor)
                        .unwrap();
                }
                if host_click {
                    editor
                        .set_selection(Selection::caret(editor.document().end()))
                        .unwrap();
                } else {
                    key(
                        &mut adapter,
                        &mut editor,
                        Key::Named(NamedKey::Escape),
                        None,
                    );
                }
                let selection = editor.selection();
                if clear_after_cancel {
                    let cleared = adapter
                        .handle_ime(&Ime::Preedit(String::new(), None), &mut editor)
                        .unwrap();
                    assert!(cleared.handled && !cleared.changed);
                }
                let commit = adapter
                    .handle_ime(&Ime::Commit("に".into()), &mut editor)
                    .unwrap();
                assert!(commit.handled && !commit.changed);
                assert_eq!(editor.document().plain_text(), "abc");
                assert_eq!(editor.selection(), selection);
                assert!(!editor.can_undo());
                assert!(adapter.plan_ime(&editor, false, area(1.0)).interrupt);

                adapter
                    .handle_ime(&Ime::Preedit("ka".into(), None), &mut editor)
                    .unwrap();
                assert_eq!(editor.composition().unwrap().text, "ka");
                assert!(
                    adapter
                        .handle_ime(&Ime::Commit("か".into()), &mut editor)
                        .unwrap()
                        .changed
                );
                assert_eq!(
                    editor.document().plain_text(),
                    if host_click { "abcか" } else { "かabc" }
                );
            }
        }
    }

    #[test]
    fn ime_events_while_read_only_keep_the_canceled_preedit_guard() {
        for event in [
            None,
            Some(Ime::Preedit(String::new(), None)),
            Some(Ime::Preedit("nih".into(), Some((3, 3)))),
        ] {
            let mut editor = Editor::from_text("abc");
            editor
                .set_selection(Selection::caret(editor.document().end()))
                .unwrap();
            let mut adapter = focused(&mut editor);
            adapter.plan_ime(&editor, false, area(1.0));
            adapter
                .handle_ime(&Ime::Preedit("ni".into(), Some((2, 2))), &mut editor)
                .unwrap();
            assert!(adapter.set_read_only(true, &mut editor).changed);
            assert!(editor.composition().is_none());
            if let Some(event) = event {
                let outcome = adapter.handle_ime(&event, &mut editor).unwrap();
                assert!(outcome.handled && !outcome.changed);
            }
            adapter.set_read_only(false, &mut editor);

            let commit = adapter
                .handle_ime(&Ime::Commit("你".into()), &mut editor)
                .unwrap();
            assert!(commit.handled && !commit.changed);
            assert_eq!(editor.document().plain_text(), "abc");
            assert!(!editor.can_undo());
            assert!(adapter.plan_ime(&editor, false, area(1.0)).interrupt);
        }
    }

    #[test]
    fn ime_disabled_while_read_only_ends_the_canceled_preedit_session() {
        let mut editor = Editor::from_text("abc");
        editor
            .set_selection(Selection::caret(editor.document().end()))
            .unwrap();
        let mut adapter = focused(&mut editor);
        adapter.plan_ime(&editor, false, area(1.0));
        adapter
            .handle_ime(&Ime::Preedit("ni".into(), Some((2, 2))), &mut editor)
            .unwrap();
        assert!(adapter.set_read_only(true, &mut editor).changed);
        let disabled = adapter.handle_ime(&Ime::Disabled, &mut editor).unwrap();
        assert!(disabled.handled && !disabled.changed);
        adapter.set_read_only(false, &mut editor);

        assert!(
            adapter
                .handle_ime(&Ime::Enabled, &mut editor)
                .unwrap()
                .changed
        );
        adapter
            .handle_ime(&Ime::Preedit("a".into(), Some((1, 1))), &mut editor)
            .unwrap();
        assert_eq!(editor.composition().unwrap().text, "a");
        let commit = adapter
            .handle_ime(&Ime::Commit("a".into()), &mut editor)
            .unwrap();
        assert!(commit.handled && commit.changed);
        assert_eq!(editor.document().plain_text(), "abca");
        assert_eq!(
            adapter.plan_ime(&editor, false, area(1.0)),
            ImeRequests {
                interrupt: false,
                allowed: Some(true),
                area: Some(area(1.0)),
            }
        );
    }

    #[test]
    fn pc_clipboard_aliases_and_dedicated_keys_follow_read_only_rules() {
        let shift = ModifiersState::SHIFT;
        let control = ModifiersState::CONTROL;
        let none = ModifiersState::empty();
        let press = |adapter: &mut WinitAdapter, editor: &mut Editor, modifiers, named| {
            adapter.set_modifiers(modifiers);
            key(adapter, editor, Key::Named(named), None)
        };
        let mut editor = Editor::from_text("secret");
        editor.select_all();
        let mut adapter = focused(&mut editor);
        let cut = press(&mut adapter, &mut editor, shift, NamedKey::Delete);
        assert_eq!(cut.clipboard, Some(ClipboardEvent::Cut("secret".into())));
        assert_eq!(editor.document().plain_text(), "");
        press(&mut adapter, &mut editor, none, NamedKey::Undo);
        assert_eq!(editor.document().plain_text(), "secret");
        press(&mut adapter, &mut editor, none, NamedKey::Redo);
        assert_eq!(editor.document().plain_text(), "");
        press(&mut adapter, &mut editor, none, NamedKey::Undo);
        for (modifiers, named, expected) in [
            (
                control,
                NamedKey::Insert,
                ClipboardEvent::Copy("secret".into()),
            ),
            (none, NamedKey::Copy, ClipboardEvent::Copy("secret".into())),
            (shift, NamedKey::Insert, ClipboardEvent::PasteRequest),
            (none, NamedKey::Paste, ClipboardEvent::PasteRequest),
        ] {
            let outcome = press(&mut adapter, &mut editor, modifiers, named);
            assert!(outcome.handled && !outcome.changed);
            assert_eq!(outcome.clipboard, Some(expected));
        }
        let cut = press(&mut adapter, &mut editor, none, NamedKey::Cut);
        assert_eq!(cut.clipboard, Some(ClipboardEvent::Cut("secret".into())));
        press(&mut adapter, &mut editor, none, NamedKey::Undo);

        adapter.set_read_only(true, &mut editor);
        for (modifiers, named) in [(shift, NamedKey::Delete), (none, NamedKey::Cut)] {
            let copy = press(&mut adapter, &mut editor, modifiers, named);
            assert!(copy.handled && !copy.changed);
            assert_eq!(copy.clipboard, Some(ClipboardEvent::Copy("secret".into())));
        }
        for (modifiers, named) in [
            (shift, NamedKey::Insert),
            (none, NamedKey::Paste),
            (none, NamedKey::Undo),
            (none, NamedKey::Redo),
        ] {
            let outcome = press(&mut adapter, &mut editor, modifiers, named);
            assert!(outcome.handled && !outcome.changed);
            assert_eq!(outcome.clipboard, None);
        }
        assert_eq!(editor.document().plain_text(), "secret");
        adapter.set_read_only(false, &mut editor);

        editor
            .set_selection(Selection::caret(Position::default()))
            .unwrap();
        let delete = press(&mut adapter, &mut editor, shift, NamedKey::Delete);
        assert_eq!(delete.clipboard, None);
        assert_eq!(editor.document().plain_text(), "ecret");

        let mut editor = Editor::from_text("secret");
        editor.select_all();
        let mut adapter = WinitAdapter::new().command_modifier(CommandModifier::Super);
        adapter.set_focused(true, &mut editor);
        let delete = press(&mut adapter, &mut editor, shift, NamedKey::Delete);
        assert_eq!(delete.clipboard, None);
        assert_eq!(editor.document().plain_text(), "");
        let copy = press(&mut adapter, &mut editor, none, NamedKey::Copy);
        assert!(copy.handled && copy.clipboard.is_none());
    }

    #[test]
    fn cut_is_a_separate_undo_step_from_typing() {
        let mut editor = Editor::default();
        let mut adapter = focused(&mut editor);
        for text in ["a", "b", "c"] {
            key(
                &mut adapter,
                &mut editor,
                Key::Character(text.into()),
                Some(text),
            );
        }
        adapter.set_modifiers(ModifiersState::SHIFT);
        key(
            &mut adapter,
            &mut editor,
            Key::Named(NamedKey::ArrowLeft),
            None,
        );
        let cut = chord(
            &mut adapter,
            &mut editor,
            ModifiersState::CONTROL,
            "x",
            KeyCode::KeyX,
            Some("x"),
        );
        assert_eq!(cut.clipboard, Some(ClipboardEvent::Cut("c".into())));
        adapter.set_modifiers(ModifiersState::empty());
        key(
            &mut adapter,
            &mut editor,
            Key::Character("d".into()),
            Some("d"),
        );
        assert_eq!(editor.document().plain_text(), "abd");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "ab");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "abc");
    }

    #[test]
    fn read_only_passes_command_enter_to_the_host_like_editable_mode() {
        for read_only in [false, true] {
            let mut editor = Editor::from_text("text");
            let mut adapter = focused(&mut editor);
            adapter.set_read_only(read_only, &mut editor);
            adapter.set_modifiers(ModifiersState::CONTROL);
            let submit = key(
                &mut adapter,
                &mut editor,
                Key::Named(NamedKey::Enter),
                Some("\r"),
            );
            assert!(!submit.handled && !submit.changed);
            adapter.set_modifiers(ModifiersState::empty());
            let enter = key(
                &mut adapter,
                &mut editor,
                Key::Named(NamedKey::Enter),
                Some("\r"),
            );
            assert!(enter.handled);
            let paragraphs = if read_only { 1 } else { 2 };
            assert_eq!(editor.document().paragraphs().len(), paragraphs);
        }
    }

    #[test]
    fn shift_enter_inserts_a_soft_line_break_unless_read_only() {
        for read_only in [false, true] {
            let mut editor = Editor::from_text("text");
            let mut adapter = focused(&mut editor);
            adapter.set_read_only(read_only, &mut editor);
            adapter.set_modifiers(ModifiersState::SHIFT);
            let outcome = key(
                &mut adapter,
                &mut editor,
                Key::Named(NamedKey::Enter),
                Some("\r"),
            );
            assert!(outcome.handled);
            assert_eq!(outcome.changed, !read_only);
            assert_eq!(editor.document().paragraphs().len(), 1);
            let expected = if read_only { "text" } else { "\u{2028}text" };
            assert_eq!(editor.document().paragraph(0).unwrap().text(), expected);
            adapter.set_modifiers(ModifiersState::CONTROL | ModifiersState::SHIFT);
            let submit = key(
                &mut adapter,
                &mut editor,
                Key::Named(NamedKey::Enter),
                Some("\r"),
            );
            assert!(!submit.handled && !submit.changed);
        }
    }

    #[test]
    fn enter_and_backspace_leave_quotes_while_tab_stays_with_the_host() {
        let mut editor = Editor::from_text("quote");
        editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
        editor
            .set_selection(Selection::caret(editor.document().end()))
            .unwrap();
        let mut adapter = focused(&mut editor);
        let kinds = |editor: &Editor| -> Vec<_> {
            editor
                .document()
                .paragraphs()
                .iter()
                .map(|paragraph| paragraph.kind())
                .collect()
        };
        let enter = Key::Named(NamedKey::Enter);
        assert!(key(&mut adapter, &mut editor, enter.clone(), Some("\r")).changed);
        assert_eq!(kinds(&editor), [ParagraphKind::Quote; 2]);
        assert!(key(&mut adapter, &mut editor, enter, Some("\r")).changed);
        assert_eq!(kinds(&editor), [ParagraphKind::Quote, ParagraphKind::Body]);
        assert_eq!(editor.document().paragraphs().len(), 2);
        editor
            .set_selection(Selection::caret(Position::new(0, 0)))
            .unwrap();
        let backspace = Key::Named(NamedKey::Backspace);
        assert!(key(&mut adapter, &mut editor, backspace, None).changed);
        assert_eq!(kinds(&editor), [ParagraphKind::Body; 2]);
        assert_eq!(editor.document().plain_text(), "quote\n");
        editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
        let tab = key(&mut adapter, &mut editor, Key::Named(NamedKey::Tab), None);
        assert!(!tab.handled && !tab.changed);
        assert_eq!(kinds(&editor), [ParagraphKind::Quote, ParagraphKind::Body]);
    }

    #[test]
    fn windows_ime_process_keys_are_consumed() {
        let mut editor = Editor::from_text("text");
        let mut adapter = focused(&mut editor);
        let outcome = key(
            &mut adapter,
            &mut editor,
            Key::Named(NamedKey::Process),
            None,
        );
        assert!(outcome.handled && !outcome.changed);
        assert_eq!(editor.document().plain_text(), "text");
    }
}
