//! Integrations that connect the editing core to GUI toolkits.
//!
//! Each adapter is behind the Cargo feature of the same name and translates one
//! toolkit's input into commands for an [editor](crate::Editor):
//!
//! - `egui` provides a rich-text editor widget for egui that lays out, paints, and
//!   hit-tests the document and publishes it to egui's accessibility tree.
//! - `winit` routes winit keyboard, modifier, focus, and IME events to an editor
//!   whose host does its own layout, painting, pointer handling, and clipboard
//!   access.
//!
//! Both adapters share the rules that map key chords to editing and formatting
//! shortcuts and that filter control characters out of typed and pasted text.
//!
//! A host for another toolkit can build its own adapter on the public core API
//! instead: paint [`Document::paragraphs`](crate::Document::paragraphs), map
//! pointer positions to a [`Position`](crate::Position), route commands through
//! the editor, and describe it to assistive technology with
//! [`accessibility`](crate::accessibility).

#[cfg(feature = "egui")]
pub mod egui;
#[cfg(feature = "winit")]
pub mod winit;

#[cfg(any(feature = "egui", feature = "winit"))]
use crate::{Editor, Error, InlineAttribute, ParagraphKind};
#[cfg(any(feature = "egui", feature = "winit"))]
use std::borrow::Cow;

/// Removes control characters from text that an adapter is about to insert.
///
/// With `multiline`, line feeds, carriage returns, and tabs are kept, as pasted and
/// IME-committed text may contain them. Returns `text` unchanged, without
/// allocating, when nothing is removed.
#[cfg(any(feature = "egui", feature = "winit"))]
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

/// Picks the character that identifies a command shortcut's key, for
/// [`Shortcut::new`].
///
/// `logical` is the character the keyboard layout assigns to the key, and
/// `physical` the character the same key types on a US layout, if known:
///
/// - An ASCII letter or digit from the layout wins, so remapped Latin layouts keep
///   their shortcuts.
/// - A bracket, or punctuation that egui names as a key, resolves to the physical
///   key only when that is a digit, as on layouts that type these characters on
///   the digit row. Otherwise a bracket stands for itself, and such punctuation
///   for no shortcut.
/// - Other punctuation and non-ASCII characters, such as Cyrillic or Greek
///   letters, resolve to the physical key. Punctuation stands for itself when the
///   physical key is unknown.
///
/// Letters are returned in lowercase.
#[cfg(any(feature = "egui", feature = "winit"))]
fn shortcut_key_name(logical: Option<char>, physical: Option<char>) -> Option<char> {
    let physical = physical.map(|key| key.to_ascii_lowercase());
    let physical_digit = physical.filter(char::is_ascii_digit);
    match logical.filter(char::is_ascii) {
        Some(letter) if letter.is_ascii_alphanumeric() => Some(letter.to_ascii_lowercase()),
        Some(bracket @ ('[' | ']')) => Some(physical_digit.unwrap_or(bracket)),
        Some(punctuation) if egui_names_key(punctuation) => physical_digit,
        Some(punctuation) => physical.or(Some(punctuation)),
        None => physical,
    }
}

/// Whether egui has a named `egui::Key` for this punctuation character.
#[cfg(any(feature = "egui", feature = "winit"))]
fn egui_names_key(character: char) -> bool {
    matches!(
        character,
        ' ' | '!'
            | '\''
            | '+'
            | ','
            | '-'
            | '.'
            | '/'
            | ':'
            | ';'
            | '='
            | '?'
            | '['
            | '\\'
            | ']'
            | '`'
            | '{'
            | '|'
            | '}'
    )
}

/// A command-modifier shortcut that both adapters recognize.
#[cfg(any(feature = "egui", feature = "winit"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shortcut {
    SelectAll,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
    Bold,
    Italic,
    Underline,
    Paragraph(ParagraphKind),
    NumberedList,
    Indent,
    Outdent,
}

#[cfg(any(feature = "egui", feature = "winit"))]
impl Shortcut {
    /// Resolves the shortcut for a key name from [`shortcut_key_name`] pressed with
    /// the command modifier, and with Shift when `shift` is set.
    fn new(name: char, shift: bool) -> Option<Self> {
        Some(match (name, shift) {
            ('a', false) => Self::SelectAll,
            ('c', false) => Self::Copy,
            ('x', false) => Self::Cut,
            ('v', false) => Self::Paste,
            ('z', false) => Self::Undo,
            ('z', true) | ('y', false) => Self::Redo,
            ('b', false) => Self::Bold,
            ('i', false) => Self::Italic,
            ('u', false) => Self::Underline,
            ('0', false) => Self::Paragraph(ParagraphKind::Body),
            ('1'..='6', false) => Self::Paragraph(ParagraphKind::Heading {
                level: name as u8 - b'0',
            }),
            ('7' | '&', true) => Self::NumberedList,
            ('8' | '*', true) => Self::Paragraph(ParagraphKind::Bullet { indent: 0 }),
            (']', false) => Self::Indent,
            ('[', false) => Self::Outdent,
            _ => return None,
        })
    }

    /// Whether the shortcut can change the document, so read-only mode blocks it.
    fn edits(self) -> bool {
        !matches!(self, Self::SelectAll | Self::Copy | Self::Cut)
    }

    /// Runs the shortcut's editor command. Clipboard shortcuts do nothing here,
    /// because the adapter exchanges their data with the host.
    fn apply(self, editor: &mut Editor) -> Result<(), Error> {
        match self {
            Self::SelectAll => editor.select_all(),
            Self::Undo => {
                editor.undo();
            }
            Self::Redo => {
                editor.redo();
            }
            Self::Bold => return editor.toggle_style(InlineAttribute::Bold),
            Self::Italic => return editor.toggle_style(InlineAttribute::Italic),
            Self::Underline => return editor.toggle_style(InlineAttribute::Underline),
            Self::Paragraph(kind) => return editor.set_paragraph_kind(kind),
            Self::NumberedList => return editor.apply_numbered_list(0),
            Self::Indent => return editor.indent_list(),
            Self::Outdent => return editor.outdent_list(),
            Self::Copy | Self::Cut | Self::Paste => {}
        }
        Ok(())
    }
}
