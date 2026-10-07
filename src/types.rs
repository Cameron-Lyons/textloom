use std::{fmt, ops::Range};

/// A caret position. `byte` is a UTF-8 offset at an extended grapheme boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    pub paragraph: usize,
    pub byte: usize,
}

impl Position {
    pub const fn new(paragraph: usize, byte: usize) -> Self {
        Self { paragraph, byte }
    }
}

/// A directional selection: the anchor stays fixed while the focus moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Position,
    pub focus: Position,
}

impl Selection {
    pub const fn caret(position: Position) -> Self {
        Self {
            anchor: position,
            focus: position,
        }
    }
    pub const fn new(anchor: Position, focus: Position) -> Self {
        Self { anchor, focus }
    }
    pub fn is_caret(self) -> bool {
        self.anchor == self.focus
    }
    pub fn range(self) -> Range<Position> {
        self.anchor.min(self.focus)..self.anchor.max(self.focus)
    }
}

/// Unpremultiplied sRGBA color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(pub [u8; 4]);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub foreground: Option<Color>,
}

/// Only supplied attributes change. `Some(None)` clears a foreground color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StylePatch {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strikethrough: Option<bool>,
    pub code: Option<bool>,
    pub foreground: Option<Option<Color>>,
}

impl StylePatch {
    pub fn apply(self, style: &mut InlineStyle) {
        if let Some(value) = self.bold {
            style.bold = value;
        }
        if let Some(value) = self.italic {
            style.italic = value;
        }
        if let Some(value) = self.underline {
            style.underline = value;
        }
        if let Some(value) = self.strikethrough {
            style.strikethrough = value;
        }
        if let Some(value) = self.code {
            style.code = value;
        }
        if let Some(value) = self.foreground {
            style.foreground = value;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ParagraphKind {
    #[default]
    Body,
    Heading {
        level: u8,
    },
    Bullet {
        indent: u8,
    },
    Ordered {
        indent: u8,
        start: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Movement {
    GraphemeBackward,
    GraphemeForward,
    WordBackward,
    WordForward,
    ParagraphStart,
    ParagraphEnd,
    DocumentStart,
    DocumentEnd,
    ParagraphUp,
    ParagraphDown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidPosition(Position),
    InvalidRange,
    InvalidCompositionSelection,
    InvalidHeadingLevel(u8),
    CompositionActive,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPosition(p) => {
                write!(f, "invalid grapheme position {}:{}", p.paragraph, p.byte)
            }
            Self::InvalidRange => f.write_str("range endpoints are reversed"),
            Self::InvalidCompositionSelection => {
                f.write_str("preedit selection must be valid UTF-8 byte offsets")
            }
            Self::InvalidHeadingLevel(level) => write!(f, "heading level {level} is outside 1..=6"),
            Self::CompositionActive => {
                f.write_str("commit or cancel the active IME composition first")
            }
        }
    }
}
impl std::error::Error for Error {}
