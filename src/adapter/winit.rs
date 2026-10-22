//! Keyboard, focus, and IME input from winit for an editor that the host renders.
//!
//! [`WinitAdapter`] turns winit window events into [`Editor`] commands: typed
//! text, navigation and deletion keys, formatting and clipboard shortcuts, and IME
//! composition. It does not lay out, paint, or hit-test text and never touches the
//! system clipboard. The host keeps those jobs, and each call returns an
//! [`InputOutcome`] that says what the host should do next.
//!
//! A host typically:
//!
//! 1. Calls [`WinitAdapter::set_focused`] when the editor widget gains or loses
//!    focus within its window.
//! 2. Passes window events to [`WinitAdapter::handle_window_event`] and offers
//!    events reported as unhandled to the rest of the application. Every adapter in
//!    a window should see `ModifiersChanged`, focused or not, because modifiers
//!    belong to the window.
//! 3. Services [`InputOutcome::clipboard`]: writes copied or cut content to the
//!    system clipboard, and answers [`ClipboardEvent::PasteRequest`] with
//!    [`WinitAdapter::paste`] or [`WinitAdapter::paste_fragment`].
//! 4. Redraws when [`InputOutcome::changed`] is set, painting the document and any
//!    IME preedit from [`Editor::composition`].
//! 5. Calls [`WinitAdapter::sync_ime`] with the caret's area every frame while the
//!    editor has focus, and at least once after it loses focus. When focus moves
//!    between editors, sync the one losing focus first.
//!
//! Do not route the same events through another adapter, such as the egui widget,
//! as well.
//!
//! # Example
//!
//! An event handler for a window that holds one editor, with a `String` standing
//! in for the system clipboard:
//!
//! ```
//! use textloom::Editor;
//! use textloom::adapter::winit::{ClipboardEvent, WinitAdapter};
//! use winit::dpi::{LogicalPosition, LogicalSize};
//! use winit::event::WindowEvent;
//! use winit::window::Window;
//!
//! fn on_window_event(
//!     input: &mut WinitAdapter,
//!     editor: &mut Editor,
//!     clipboard: &mut String,
//!     window: &Window,
//!     event: &WindowEvent,
//! ) -> Result<(), textloom::Error> {
//!     let outcome = input.handle_window_event(event, editor)?;
//!     let mut changed = outcome.changed;
//!     match outcome.clipboard {
//!         Some(ClipboardEvent::Copy(text) | ClipboardEvent::Cut(text)) => *clipboard = text,
//!         Some(ClipboardEvent::PasteRequest) => {
//!             changed |= input.paste(editor, clipboard)?.changed;
//!         }
//!         _ => {}
//!     }
//!     if !outcome.handled {
//!         // Offer `event` to the rest of the application.
//!     }
//!     if changed {
//!         window.request_redraw();
//!     }
//!     // Once the frame is laid out, report the caret's area in logical coordinates.
//!     let caret = LogicalPosition::new(24.0, 8.0);
//!     input.sync_ime(window, editor, caret, LogicalSize::new(1.0, 16.0));
//!     Ok(())
//! }
//! ```
//!
//! [`WinitAdapter`] shows the effect of individual key and IME events.

use super::Shortcut;
use crate::document::LINE_SEPARATOR;
use crate::{Editor, Error, Fragment, Movement, ParagraphKind, Position, Selection};
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::{ElementState, Ime, WindowEvent};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::Window;

// Windows reports AltGr as Control+Alt, so that chord types text there.
const CTRL_ALT_IS_ALTGR: bool = cfg!(windows);

/// Clipboard work that a key press asks the host to do.
///
/// The adapter never accesses the system clipboard. Copy and cut produce the
/// plain-text variants unless [`WinitAdapter::rich_clipboard`] is enabled, and
/// neither is produced when the selection is empty.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClipboardEvent {
    /// Write this plain text of the selection to the clipboard. Paragraphs and soft
    /// line breaks are separated by `\n`. In read-only mode, cut also produces
    /// this event and leaves the selection in place.
    Copy(String),
    /// Write this styled copy of the selection to the clipboard, encoded by the
    /// host, for example with [`Fragment::to_html`] or [`Fragment::to_bytes`].
    CopyRich(Fragment),
    /// Write this plain text to the clipboard. The adapter has already removed the
    /// selection from the document.
    Cut(String),
    /// Write this styled copy to the clipboard. It was taken before the adapter
    /// removed the selection from the document.
    CutRich(Fragment),
    /// Read the clipboard and pass its content to [`WinitAdapter::paste`] or
    /// [`WinitAdapter::paste_fragment`].
    PasteRequest,
}

/// The result of handing one input to [`WinitAdapter`], and what the host should do
/// about it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
#[must_use = "service clipboard requests and redraw after changes"]
pub struct InputOutcome {
    /// Whether the adapter consumed the input. The host should offer unhandled
    /// input, such as an unbound shortcut, Page Down, or Tab outside a list, to the
    /// rest of the application.
    pub handled: bool,
    /// Whether the input changed the document, the selection, the typing style, or
    /// the IME composition, or the adapter's focus, read-only, or IME state. The
    /// host should redraw.
    pub changed: bool,
    /// Clipboard work for the host after a copy, cut, or paste.
    pub clipboard: Option<ClipboardEvent>,
}

/// The modifier key that triggers command shortcuts such as copy and undo, along
/// with the platform's text-editing key conventions.
///
/// [`Default`] gives [`Super`](Self::Super) on macOS and
/// [`Control`](Self::Control) elsewhere. In both modes, Alt (Option) moves and
/// deletes by words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandModifier {
    /// Control, as on Windows and Linux. Control also moves and deletes by words,
    /// and Control+Insert, Shift+Insert, and Shift+Delete copy, paste, and cut.
    Control,
    /// Super (Command), as on macOS. Command+Left and Command+Right, and Control+Left
    /// and Control+Right, move to the edges of the line, Option+Up and Option+Down to
    /// the edges of the paragraph, and Command+Backspace deletes back to the start of
    /// the line. Control+A, E, B, F, P, N, H, and D act as Home, End, Left, Right,
    /// Up, Down, Backspace, and Delete.
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

/// Routes winit keyboard, focus, and IME events to an [`Editor`] that the host
/// renders.
///
/// The adapter keeps the input state that winit spreads over several events:
/// widget and window focus, modifiers, and the platform IME session. It edits only
/// while both the editor widget and its window have focus. Configure it with
/// [`new`](Self::new) and the builder methods, then connect it to the window as the
/// [module documentation](self) describes.
///
/// # Example
///
/// ```
/// use textloom::Editor;
/// use textloom::adapter::winit::{CommandModifier, WinitAdapter};
/// use winit::event::{ElementState, Ime};
/// use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};
///
/// let mut editor = Editor::from_text("Hello");
/// let mut input = WinitAdapter::new().command_modifier(CommandModifier::Control);
/// assert!(input.set_focused(true, &mut editor).changed);
///
/// // Control+A selects everything, also on a Russian layout, through the physical key.
/// input.set_modifiers(ModifiersState::CONTROL);
/// let select_all = input.handle_key(
///     &Key::Character("ф".into()),
///     PhysicalKey::Code(KeyCode::KeyA),
///     ElementState::Pressed,
///     None,
///     &mut editor,
/// )?;
/// assert!(select_all.handled && select_all.changed);
///
/// // The preedit leaves the document alone until the IME commits.
/// input.set_modifiers(ModifiersState::empty());
/// let preedit = input.handle_ime(&Ime::Preedit("日本".into(), Some((0, 6))), &mut editor)?;
/// assert!(preedit.changed);
/// assert_eq!(editor.document().plain_text(), "Hello");
/// let commit = input.handle_ime(&Ime::Commit("日本語".into()), &mut editor)?;
/// assert!(commit.changed);
/// assert_eq!(editor.document().plain_text(), "日本語");
/// # Ok::<(), textloom::Error>(())
/// ```
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

/// The caret area that places the platform's IME candidate window, in logical
/// window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ImeArea {
    position: LogicalPosition<f64>,
    size: LogicalSize<f64>,
}

/// What the window was last told about IME, so that [`WinitAdapter::sync_ime`] sends
/// only changes.
#[derive(Debug, Default)]
struct ImeSync {
    /// Whether IME is allowed on the window.
    allowed: bool,
    /// Whether the window's IME state must be sent again, because focus returned or
    /// the IME was disabled.
    stale: bool,
    /// The caret area the window has, if any.
    area: Option<ImeArea>,
    /// Whether the IME may still have a preedit session, which must be interrupted
    /// if the editor dropped its composition.
    preedit: bool,
}

/// The window calls that bring its IME state up to date.
#[derive(Debug, Default, PartialEq)]
struct ImeRequests {
    /// Disallow and allow IME again, so the input method drops its preedit.
    interrupt: bool,
    /// Whether to allow IME, if that has to be sent.
    allowed: Option<bool>,
    /// The caret area to send, if it changed.
    area: Option<ImeArea>,
}

/// Which edge of a soft line to move to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineEdge {
    Start,
    End,
}

impl WinitAdapter {
    /// Creates an unfocused, editable adapter that uses the platform's default
    /// [`CommandModifier`], plain-text clipboard events, and Up and Down movement
    /// between paragraphs.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the modifier that triggers command shortcuts, in place of the platform's
    /// default.
    #[must_use]
    pub fn command_modifier(mut self, modifier: CommandModifier) -> Self {
        self.command_modifier = modifier;
        self
    }

    /// Makes copy and cut produce [`ClipboardEvent::CopyRich`] and
    /// [`ClipboardEvent::CutRich`], which carry a styled [`Fragment`], instead of
    /// plain text. The host chooses the clipboard formats.
    #[must_use]
    pub fn rich_clipboard(mut self, enabled: bool) -> Self {
        self.rich_clipboard = enabled;
        self
    }

    /// Delegates Up and Down, and Control+P and Control+N with
    /// [`CommandModifier::Super`], to the host for movement by visual line, which
    /// needs the host's text layout. These keys are then returned unhandled, so the
    /// host still has the original key event, including its Shift modifier.
    ///
    /// Without this option, they move to the previous or next paragraph, keeping the
    /// caret's grapheme column. Command+Up and Command+Down move to the document's
    /// start and end either way.
    #[must_use]
    pub fn host_vertical_navigation(mut self, enabled: bool) -> Self {
        self.host_vertical_navigation = enabled;
        self
    }

    /// Whether read-only mode is on; see [`set_read_only`](Self::set_read_only).
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Turns read-only mode on or off.
    ///
    /// In read-only mode, navigation, selection, and copying keep working, and cut
    /// copies without removing the selection. Keys, shortcuts, IME events, and
    /// pastes that would edit the document are consumed without changing it or the
    /// undo history. Tab and Command+Enter still reach the host. Turning read-only
    /// mode on cancels any composition, and the next [`sync_ime`](Self::sync_ime)
    /// disallows IME on the window.
    ///
    /// The outcome's `changed` is set when the mode or the composition changed.
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

    /// Whether the editor has keyboard focus: the host marked the widget focused
    /// with [`set_focused`](Self::set_focused), and the window has not reported
    /// losing focus. Key, IME, and paste input is returned unhandled otherwise.
    #[must_use]
    pub fn is_focused(&self) -> bool {
        self.focused && !self.window_unfocused
    }

    /// Whether the platform IME reported [`Ime::Enabled`] and has not been disabled
    /// since, by [`Ime::Disabled`], by the editor losing focus, or by read-only mode.
    #[must_use]
    pub fn ime_enabled(&self) -> bool {
        self.ime_enabled
    }

    /// The modifier state used to interpret keys, from the last `ModifiersChanged`
    /// event or [`set_modifiers`](Self::set_modifiers) call.
    ///
    /// The window losing focus clears it. The widget losing focus does not, so
    /// modifiers that are still held apply when the widget regains focus.
    #[must_use]
    pub fn modifiers(&self) -> ModifiersState {
        self.modifiers
    }

    /// Sets the modifier state used to interpret keys, for hosts that track
    /// modifiers themselves instead of passing `ModifiersChanged` to
    /// [`handle_window_event`](Self::handle_window_event).
    pub fn set_modifiers(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }

    /// Tells the adapter whether the editor widget has focus within its window.
    ///
    /// When the editor loses focus, this ends the current typing undo group,
    /// cancels any composition, and forgets that IME was enabled. When it gains
    /// focus, the next [`sync_ime`](Self::sync_ime) allows IME again, unless the
    /// adapter is read-only. Window focus is tracked separately, through
    /// [`handle_window_event`](Self::handle_window_event).
    ///
    /// The outcome's `changed` is set when the widget's focus or the composition
    /// changed.
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

    /// Brings the window's IME state up to date with the editor and places the IME
    /// candidate window at the caret, whose area `position` and `size` give in
    /// logical window coordinates.
    ///
    /// Call this every frame while the editor has focus, and at least once after it
    /// loses focus. Only changes reach the window: IME is allowed when the editor
    /// gains focus and again after [`Ime::Disabled`], and disallowed once when the
    /// editor loses widget focus or becomes read-only. The caret area is sent when
    /// it moves, and again after [`Ime::Enabled`]. When the editor dropped a
    /// composition that the input method may still be showing, for example because
    /// the host moved the selection, IME is briefly disallowed so that the input
    /// method abandons it.
    ///
    /// IME stays allowed while only the window is in the background, because winit
    /// 0.30 on X11 recreates the input context, and resets the input method's state,
    /// whenever IME is allowed again. If the window has focus although winit reported
    /// otherwise, as with its spurious `Focused(false)` at startup on macOS, the
    /// adapter treats the window as focused again.
    pub fn sync_ime(
        &mut self,
        window: &Window,
        editor: &Editor,
        position: LogicalPosition<f64>,
        size: LogicalSize<f64>,
    ) {
        let window_focused = self.window_unfocused && window.has_focus();
        let requests = self.plan_ime(editor, window_focused, ImeArea { position, size });
        if requests.interrupt {
            window.set_ime_allowed(false);
            window.set_ime_allowed(true);
        }
        if let Some(allowed) = requests.allowed {
            window.set_ime_allowed(allowed);
        }
        if let Some(ImeArea { position, size }) = requests.area {
            window.set_ime_cursor_area(position, size);
        }
    }

    /// Handles one window event.
    ///
    /// The adapter uses these events and returns any other unhandled:
    ///
    /// - `ModifiersChanged`, to track the modifiers. It is never reported as
    ///   handled, so other parts of the application can see it too.
    /// - `Focused`, for window focus. Losing it clears the modifiers and otherwise
    ///   acts like losing widget focus in [`set_focused`](Self::set_focused).
    /// - `KeyboardInput`, through [`handle_key`](Self::handle_key) with the event's
    ///   logical key, physical key, and text. Synthetic key events, which winit sends
    ///   for keys already held when the window gains focus, are ignored. A key press
    ///   while the window is thought to lack focus corrects that, which repairs
    ///   winit's spurious `Focused(false)` at startup on macOS.
    /// - `Ime`, through [`handle_ime`](Self::handle_ime).
    ///
    /// # Errors
    ///
    /// Returns the editor's error if it rejects the command that a key or IME event
    /// maps to, such as [`Error::InvalidCompositionSelection`] for a preedit with an
    /// invalid cursor range.
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

    /// Handles a key event. [`handle_window_event`](Self::handle_window_event)
    /// calls this for keyboard input.
    ///
    /// `key` and `physical_key` are the event's logical and physical keys, and
    /// `text` is the text it produces (winit's `KeyEvent::text`). Typed text comes
    /// from `text` only, never from `key`, so a dead-key sequence is inserted once.
    /// Shortcuts fall back to the physical key when the layout's character is not
    /// Latin, so they also work on, for example, Cyrillic layouts.
    ///
    /// Key releases, and presses while the editor lacks focus, are returned
    /// unhandled. While a composition shows preedit text, presses are consumed
    /// without effect, and Escape cancels any composition. Otherwise, with Shift
    /// extending the selection where that applies:
    ///
    /// - With the command modifier, A selects all; C, X, and V copy, cut, and
    ///   request a paste; Z undoes, and Shift+Z or Y redoes; B, I, and U toggle bold,
    ///   italic, and underline; 0 and 1 to 6 make the selected paragraphs body text
    ///   or headings of that level; Shift+7 and Shift+8 make them a numbered or
    ///   bulleted list; and `]` and `[` indent and outdent list items. Other command
    ///   chords with a character key are returned unhandled. The dedicated Copy,
    ///   Cut, Paste, Undo, and Redo keys work too.
    /// - Left and Right move by grapheme, or by word with the word modifier that
    ///   [`CommandModifier`] describes. Up and Down move to the previous or next
    ///   paragraph, and Home and End to the start or end of the paragraph; with the
    ///   command modifier, all four move to the start or end of the document.
    ///   [`CommandModifier::Super`] adds
    ///   the macOS line and paragraph movements. Page Up and Page Down are returned
    ///   unhandled, as are Up and Down without the command modifier when
    ///   [`host_vertical_navigation`](Self::host_vertical_navigation) is on.
    /// - Enter inserts a paragraph break like [`Editor::insert_paragraph`], and
    ///   Shift+Enter a soft line break like [`Editor::insert_line_break`].
    ///   Command+Enter is returned unhandled.
    /// - Backspace and Delete delete like [`Editor::delete_backward`] and
    ///   [`Editor::delete_forward`], or by word with the word modifier.
    /// - Tab and Shift+Tab indent and outdent when the selection includes a list
    ///   item and neither Control, Alt, nor Super is held. Otherwise Tab is returned
    ///   unhandled, for focus navigation.
    /// - The Process key, which Windows sends for keys that its IME handles, is
    ///   consumed.
    /// - Any other key inserts its `text`, without control characters, unless
    ///   Control or Super is held. On Windows, Control+Alt is AltGr and types text.
    ///   A key without such text is returned unhandled.
    ///
    /// Apart from Tab, a key that would edit the document in read-only mode is
    /// consumed without effect.
    ///
    /// # Errors
    ///
    /// Returns the editor's error if it rejects the command that the key maps to.
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
        let control_key = self.macos_control_key(key, physical_key).map(Key::Named);
        let key = control_key.as_ref().unwrap_or(key);
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
            || (self.modifiers.control_key()
                && !self.modifiers.super_key()
                && self.command_modifier == CommandModifier::Control
                && control_key.is_none());
        let super_command = command && self.command_modifier == CommandModifier::Super;
        let control_line = self.command_modifier == CommandModifier::Super
            && self.modifiers.control_key()
            && !self.modifiers.alt_key()
            && !self.modifiers.super_key()
            && control_key.is_none();
        if (super_command || control_line)
            && matches!(key, Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight))
        {
            editor.cancel_composition();
            let edge = if matches!(key, Key::Named(NamedKey::ArrowRight)) {
                LineEdge::End
            } else {
                LineEdge::Start
            };
            let position = soft_line_boundary_position(editor, edge);
            let anchor = if shift {
                editor.selection().anchor
            } else {
                position
            };
            editor.set_selection(Selection::new(anchor, position))?;
            outcome.changed = before != editor.presentation();
            return Ok(outcome);
        }
        if self.command_modifier == CommandModifier::Super
            && self.modifiers.alt_key()
            && !self.modifiers.control_key()
            && !self.modifiers.super_key()
            && matches!(key, Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown))
        {
            editor.cancel_composition();
            move_paragraph_boundary(
                editor,
                matches!(key, Key::Named(NamedKey::ArrowDown)),
                shift,
            )?;
            outcome.changed = before != editor.presentation();
            return Ok(outcome);
        }
        let paragraph_vertical = !self.host_vertical_navigation;
        let movement = match key {
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
                    let range = if editor.selection().is_caret() {
                        soft_line_boundary_position(editor, LineEdge::Start)
                            ..editor.selection().focus
                    } else {
                        editor.selection().range()
                    };
                    if !range.is_empty() {
                        editor.replace_range(range, "")?;
                    }
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
                    let clean = super::clean_text(produced, false);
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

    /// Handles an IME event. [`handle_window_event`](Self::handle_window_event)
    /// calls this for `WindowEvent::Ime`.
    ///
    /// A preedit becomes the editor's [`composition`](Editor::composition), which
    /// leaves the document unchanged until the input method commits. An empty
    /// preedit keeps the composition and the selection that it will replace. A
    /// commit replaces that selection, or the current one without a composition,
    /// with the committed text, recorded in the undo history like
    /// [`Editor::insert_text`]. Control characters other than line breaks and tabs
    /// are removed from it first, and a commit that consists only of such
    /// characters cancels the composition instead, as [`Ime::Disabled`] does.
    ///
    /// After a composition is canceled outside the input method, for example by
    /// Escape, by the host moving the selection, or by losing focus, the input method
    /// may still send preedit and commit events from that session. They are consumed
    /// without effect until [`sync_ime`](Self::sync_ime) interrupts the session or
    /// [`Ime::Disabled`] arrives.
    ///
    /// Events are returned unhandled while the editor lacks focus. In read-only
    /// mode, they are consumed and cancel any composition.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCompositionSelection`] if a preedit's cursor range is
    /// reversed or not on character boundaries of its text, and the editor's error if
    /// it rejects the composition.
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
            Ime::Enabled => {
                self.ime_enabled = true;
                // Wayland's enable resets the cursor rectangle, and a new X11 input
                // context has no spot, so send the caret area again.
                self.ime.area = None;
            }
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
                let clean = super::clean_text(text, true);
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

    /// Inserts clipboard text that the host read after a
    /// [`ClipboardEvent::PasteRequest`], replacing the selection as one undo step,
    /// separate from typing.
    ///
    /// Control characters other than line breaks and tabs are removed first.
    /// Nothing changes when no text remains, in read-only mode, or while a
    /// composition shows preedit text; an empty composition is canceled. The paste
    /// is returned unhandled while the editor lacks focus.
    ///
    /// # Errors
    ///
    /// Returns the editor's error if it rejects the insertion.
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
        let clean = super::clean_text(text, true);
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

    /// Inserts a styled fragment that the host read from the clipboard after a
    /// [`ClipboardEvent::PasteRequest`], replacing the selection as one undo step.
    ///
    /// Nothing changes in read-only mode or while a composition shows preedit text;
    /// an empty composition is canceled. The paste is returned unhandled while the
    /// editor lacks focus.
    ///
    /// # Errors
    ///
    /// Returns the editor's error if it rejects the insertion.
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

    /// Whether exactly the command modifier, without Alt or the other command key,
    /// is held. Shift may be held too.
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

    /// The key that a macOS Control+letter binding stands for, such as Home for
    /// Control+A, with [`CommandModifier::Super`].
    fn macos_control_key(&self, key: &Key, physical_key: PhysicalKey) -> Option<NamedKey> {
        if self.command_modifier != CommandModifier::Super
            || !self.modifiers.control_key()
            || self.modifiers.alt_key()
            || self.modifiers.super_key()
        {
            return None;
        }
        Some(match shortcut_key(key, physical_key)? {
            'a' => NamedKey::Home,
            'e' => NamedKey::End,
            'b' => NamedKey::ArrowLeft,
            'f' => NamedKey::ArrowRight,
            'p' => NamedKey::ArrowUp,
            'n' => NamedKey::ArrowDown,
            'h' => NamedKey::Backspace,
            'd' => NamedKey::Delete,
            _ => return None,
        })
    }

    /// Applies a change of widget or window focus, given whether the editor had
    /// focus before. Returns whether a composition was canceled.
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

    /// Records that the window has focus, which input from it proves. Returns
    /// whether the editor regained focus this way.
    fn observe_window_focus(&mut self) -> bool {
        let regained = self.window_unfocused && self.focused;
        self.window_unfocused = false;
        if regained {
            self.ime.stale = true;
        }
        regained
    }

    /// Decides which window calls [`sync_ime`](Self::sync_ime) makes, and records
    /// their effect.
    fn plan_ime(&mut self, editor: &Editor, window_focused: bool, area: ImeArea) -> ImeRequests {
        if window_focused {
            self.observe_window_focus();
        }
        // Keep IME allowed while only the window is in the background: winit 0.30
        // on X11 recreates the input context on every change, unfocused and with
        // the input method's per-context state reset.
        let editable = self.focused && !self.read_only;
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

    /// The shortcut of a dedicated clipboard or history key, or of a PC clipboard
    /// alias such as Shift+Insert with [`CommandModifier::Control`].
    fn named_shortcut(&self, key: &Key, editor: &Editor) -> Option<Shortcut> {
        let Key::Named(named) = key else {
            return None;
        };
        let pc_aliases = self.command_modifier == CommandModifier::Control;
        let modifiers = self.modifiers;
        Some(match named {
            NamedKey::Copy => Shortcut::Copy,
            NamedKey::Cut => Shortcut::Cut,
            NamedKey::Paste => Shortcut::Paste,
            NamedKey::Undo => Shortcut::Undo,
            NamedKey::Redo => Shortcut::Redo,
            NamedKey::Insert if pc_aliases && modifiers == ModifiersState::CONTROL => {
                Shortcut::Copy
            }
            NamedKey::Insert if pc_aliases && modifiers == ModifiersState::SHIFT => Shortcut::Paste,
            NamedKey::Delete
                if pc_aliases
                    && modifiers == ModifiersState::SHIFT
                    && !editor.selection().is_caret() =>
            {
                Shortcut::Cut
            }
            _ => return None,
        })
    }

    /// Runs a shortcut, unless read-only mode blocks it, and returns the clipboard
    /// work it leaves for the host.
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

    /// The copy or cut event for the selection, or `None` when it is empty.
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

/// The shortcut key name of a key event; see [`super::shortcut_key_name`].
fn shortcut_key(key: &Key, physical_key: PhysicalKey) -> Option<char> {
    let logical = match key {
        Key::Character(text) => {
            let mut characters = text.chars();
            characters.next().filter(|_| characters.next().is_none())
        }
        Key::Named(_) => return None,
        Key::Dead(_) | Key::Unidentified(_) => None,
    };
    super::shortcut_key_name(logical, physical_shortcut_key(physical_key))
}

/// The character that a physical shortcut key types on a US layout.
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

/// Cancels a composition without preedit text before a paste. Returns `false`,
/// leaving the composition alone, when its preedit text is visible.
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

/// Moves the caret to the start of its paragraph, or to its end with `end`, or to
/// the previous or next paragraph's when it is already there. With `extend`, the
/// selection's anchor stays.
fn move_paragraph_boundary(editor: &mut Editor, end: bool, extend: bool) -> Result<(), Error> {
    let focus = editor.selection().focus;
    let document = editor.document();
    let length = document
        .paragraph(focus.paragraph)
        .expect("the editor keeps its selection inside the document")
        .text()
        .len();
    let paragraph = if end && focus.byte == length {
        (focus.paragraph + 1).min(document.paragraphs().len() - 1)
    } else if !end && focus.byte == 0 {
        focus.paragraph.saturating_sub(1)
    } else {
        focus.paragraph
    };
    let position = Position::new(
        paragraph,
        if end {
            document
                .paragraph(paragraph)
                .expect("the target paragraph index is clamped to the document")
                .text()
                .len()
        } else {
            0
        },
    );
    let anchor = if extend {
        editor.selection().anchor
    } else {
        position
    };
    editor.set_selection(Selection::new(anchor, position))
}

/// The position at the given edge of the soft line that holds the selection's
/// focus. Soft lines end at soft line breaks and at the paragraph's edges.
fn soft_line_boundary_position(editor: &Editor, edge: LineEdge) -> Position {
    let focus = editor.selection().focus;
    let text = editor
        .document()
        .paragraph(focus.paragraph)
        .expect("the editor keeps its selection inside the document")
        .text();
    let byte = match edge {
        LineEdge::End => text[focus.byte..]
            .find(LINE_SEPARATOR)
            .map_or(text.len(), |byte| focus.byte + byte),
        LineEdge::Start => text[..focus.byte]
            .rfind(LINE_SEPARATOR)
            .map_or(0, |byte| byte + LINE_SEPARATOR.len_utf8()),
    };
    Position::new(focus.paragraph, byte)
}

/// Whether any paragraph that the selection touches is a list item.
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
    use std::mem;
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
        ImeArea {
            position: LogicalPosition::new(x, 4.0),
            size: LogicalSize::new(1.0, 16.0),
        }
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
        let text = "prefix\ncafé 👩🏽\u{200d}💻 tail";
        let end = Position::new(1, "café 👩🏽\u{200d}💻".len());
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
        let mut editor = Editor::from_text("café 👩\u{200d}💻");
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
            adapter.plan_ime(&editor, false, area(1.0)).allowed,
            Some(true)
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
        let logical_j = chord(&mut adapter, &mut editor, control, "j", KeyCode::KeyC, None);
        assert!(!logical_j.handled && logical_j.clipboard.is_none());
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
                assert_eq!(mem::discriminant(&second), mem::discriminant(&kind));
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
    fn macos_control_letters_move_and_extend_without_splitting_graphemes() {
        let text = "first\ncafé e\u{301}👩🏽\u{200d}💻\nthird";
        let start = Position::new(1, "café e\u{301}".len());
        for (letter, expected) in [
            ("a", Position::new(1, 0)),
            ("e", Position::new(1, "café e\u{301}👩🏽\u{200d}💻".len())),
            ("b", Position::new(1, "café ".len())),
            ("f", Position::new(1, "café e\u{301}👩🏽\u{200d}💻".len())),
            ("p", Position::new(0, 5)),
            ("n", Position::new(2, 5)),
        ] {
            for extend in [false, true] {
                let mut editor = Editor::from_text(text);
                editor.set_selection(Selection::caret(start)).unwrap();
                let mut adapter = WinitAdapter::new().command_modifier(CommandModifier::Super);
                adapter.set_focused(true, &mut editor);
                adapter.set_modifiers(if extend {
                    ModifiersState::CONTROL | ModifiersState::SHIFT
                } else {
                    ModifiersState::CONTROL
                });
                let outcome = key(
                    &mut adapter,
                    &mut editor,
                    Key::Character(letter.into()),
                    None,
                );
                assert!(outcome.handled && outcome.changed, "Control-{letter}");
                assert_eq!(
                    editor.selection(),
                    Selection::new(if extend { start } else { expected }, expected)
                );
                assert_eq!(editor.document().plain_text(), text);
                assert!(!editor.can_undo());
            }
        }
    }

    #[test]
    fn macos_control_deletes_one_grapheme_and_respects_edit_guards() {
        let text = "a e\u{301}👩🏽\u{200d}💻 z";
        let caret = Selection::caret(Position::new(0, "a e\u{301}".len()));
        for (logical, expected) in [
            (Key::Character("h".into()), "a 👩🏽\u{200d}💻 z"),
            (Key::Character("d".into()), "a e\u{301} z"),
            (Key::Named(NamedKey::Backspace), "a 👩🏽\u{200d}💻 z"),
        ] {
            let mut editor = Editor::from_text(text);
            editor.set_selection(caret).unwrap();
            let mut adapter = WinitAdapter::new().command_modifier(CommandModifier::Super);
            adapter.set_focused(true, &mut editor);
            adapter.set_modifiers(ModifiersState::CONTROL);
            let outcome = key(&mut adapter, &mut editor, logical.clone(), None);
            assert!(outcome.handled && outcome.changed);
            assert_eq!(editor.document().plain_text(), expected);
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), text);
            assert_eq!(editor.selection(), caret);
            adapter.set_read_only(true, &mut editor);
            let outcome = key(&mut adapter, &mut editor, logical.clone(), None);
            assert!(outcome.handled && !outcome.changed);
            adapter.set_read_only(false, &mut editor);
            editor.update_composition("候", None).unwrap();
            let outcome = key(&mut adapter, &mut editor, logical, None);
            assert!(outcome.handled && !outcome.changed);
            assert_eq!(editor.composition().unwrap().text, "候");
            assert_eq!(editor.document().plain_text(), text);
        }
    }

    #[test]
    fn macos_option_vertical_navigation_visits_and_selects_paragraph_boundaries() {
        let text = "first\n\ncafé 👩🏽\u{200d}💻 tail\nlast";
        for (named, start, targets) in [
            (
                NamedKey::ArrowUp,
                Position::new(2, "café ".len()),
                vec![
                    Position::new(2, 0),
                    Position::new(1, 0),
                    Position::default(),
                    Position::default(),
                ],
            ),
            (
                NamedKey::ArrowDown,
                Position::new(0, 2),
                vec![
                    Position::new(0, 5),
                    Position::new(1, 0),
                    Position::new(2, "café 👩🏽\u{200d}💻 tail".len()),
                    Position::new(3, 4),
                    Position::new(3, 4),
                ],
            ),
        ] {
            for extend in [false, true] {
                let mut editor = Editor::from_text(text);
                editor.set_selection(Selection::caret(start)).unwrap();
                let mut adapter = WinitAdapter::new()
                    .command_modifier(CommandModifier::Super)
                    .host_vertical_navigation(true);
                adapter.set_focused(true, &mut editor);
                adapter.set_modifiers(if extend {
                    ModifiersState::ALT | ModifiersState::SHIFT
                } else {
                    ModifiersState::ALT
                });
                for target in &targets {
                    let outcome = key(&mut adapter, &mut editor, Key::Named(named), None);
                    assert!(outcome.handled);
                    assert_eq!(
                        editor.selection(),
                        Selection::new(if extend { start } else { *target }, *target)
                    );
                }
                assert!(!editor.can_undo());
            }
        }
    }

    #[test]
    fn macos_command_line_navigation_and_deletion_stop_at_soft_breaks() {
        let text = "keep\u{2028}café e\u{301}👩🏽\u{200d}💻 tail";
        let start = Position::new(0, "keep\u{2028}café e\u{301}".len());
        let line_start = Position::new(0, "keep\u{2028}".len());
        for modifiers in [ModifiersState::SUPER, ModifiersState::CONTROL] {
            for extend in [false, true] {
                let mut editor = Editor::from_text(text);
                editor.set_selection(Selection::caret(start)).unwrap();
                let mut adapter = WinitAdapter::new().command_modifier(CommandModifier::Super);
                adapter.set_focused(true, &mut editor);
                adapter.set_modifiers(if extend {
                    modifiers | ModifiersState::SHIFT
                } else {
                    modifiers
                });
                key(
                    &mut adapter,
                    &mut editor,
                    Key::Named(NamedKey::ArrowLeft),
                    None,
                );
                assert_eq!(
                    editor.selection(),
                    Selection::new(if extend { start } else { line_start }, line_start)
                );
                editor
                    .set_selection(Selection::caret(Position::new(0, 2)))
                    .unwrap();
                key(
                    &mut adapter,
                    &mut editor,
                    Key::Named(NamedKey::ArrowRight),
                    None,
                );
                assert_eq!(editor.selection().focus, Position::new(0, "keep".len()));
            }
        }
        let mut editor = Editor::from_text(text);
        editor.set_selection(Selection::caret(start)).unwrap();
        let mut adapter = WinitAdapter::new().command_modifier(CommandModifier::Super);
        adapter.set_focused(true, &mut editor);
        adapter.set_modifiers(ModifiersState::SUPER);
        key(
            &mut adapter,
            &mut editor,
            Key::Named(NamedKey::Backspace),
            None,
        );
        assert_eq!(
            editor.document().paragraph(0).unwrap().text(),
            "keep\u{2028}👩🏽\u{200d}💻 tail"
        );
        assert!(editor.undo());
        assert_eq!(editor.document().paragraph(0).unwrap().text(), text);
        assert_eq!(editor.selection(), Selection::caret(start));
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
        assert_eq!(adapter.plan_ime(&editor, false, area(2.0)), none);
        adapter
            .handle_window_event(&WindowEvent::Focused(true), &mut editor)
            .unwrap();
        assert_eq!(
            adapter.plan_ime(&editor, false, area(2.0)),
            allow(area(2.0))
        );

        adapter.handle_ime(&Ime::Enabled, &mut editor).unwrap();
        assert_eq!(
            adapter.plan_ime(&editor, false, area(2.0)),
            ImeRequests {
                area: Some(area(2.0)),
                ..ImeRequests::default()
            }
        );
        assert_eq!(adapter.plan_ime(&editor, false, area(2.0)), none);
    }

    #[test]
    fn window_blur_keeps_ime_allowed_and_interrupts_a_canceled_preedit() {
        let mut editor = Editor::from_text("ab");
        let mut adapter = focused(&mut editor);
        adapter.plan_ime(&editor, false, area(1.0));
        adapter
            .handle_ime(&Ime::Preedit("に".into(), Some((3, 3))), &mut editor)
            .unwrap();
        adapter
            .handle_window_event(&WindowEvent::Focused(false), &mut editor)
            .unwrap();
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "ab");
        assert_eq!(
            adapter.plan_ime(&editor, false, area(1.0)),
            ImeRequests {
                interrupt: true,
                area: Some(area(1.0)),
                ..ImeRequests::default()
            }
        );
        assert_eq!(
            adapter.plan_ime(&editor, false, area(1.0)),
            ImeRequests::default()
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
