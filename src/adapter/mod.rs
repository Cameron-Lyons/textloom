#[cfg(feature = "egui")]
pub mod egui;
#[cfg(feature = "winit")]
pub mod winit;

#[cfg(any(feature = "egui", feature = "winit"))]
use crate::{Editor, Error, InlineAttribute, ParagraphKind};
#[cfg(any(feature = "egui", feature = "winit"))]
use std::borrow::Cow;

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

    fn edits(self) -> bool {
        !matches!(self, Self::SelectAll | Self::Copy | Self::Cut)
    }

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
