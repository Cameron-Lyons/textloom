use std::{fmt, ops::Range};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionStyle {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strikethrough: Option<bool>,
    pub code: Option<bool>,
    pub foreground: Option<Option<Color>>,
}

impl From<InlineStyle> for SelectionStyle {
    fn from(style: InlineStyle) -> Self {
        Self {
            bold: Some(style.bold),
            italic: Some(style.italic),
            underline: Some(style.underline),
            strikethrough: Some(style.strikethrough),
            code: Some(style.code),
            foreground: Some(style.foreground),
        }
    }
}

impl SelectionStyle {
    pub(crate) fn include(&mut self, style: InlineStyle) {
        self.bold = self.bold.filter(|value| *value == style.bold);
        self.italic = self.italic.filter(|value| *value == style.italic);
        self.underline = self.underline.filter(|value| *value == style.underline);
        self.strikethrough = self
            .strikethrough
            .filter(|value| *value == style.strikethrough);
        self.code = self.code.filter(|value| *value == style.code);
        self.foreground = self.foreground.filter(|value| *value == style.foreground);
    }

    pub(crate) fn is_fully_mixed(self) -> bool {
        self.bold.is_none()
            && self.italic.is_none()
            && self.underline.is_none()
            && self.strikethrough.is_none()
            && self.code.is_none()
            && self.foreground.is_none()
    }
}

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
#[non_exhaustive]
pub enum Error {
    InvalidPosition(Position),
    InvalidRange,
    InvalidFragment,
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
            Self::InvalidFragment => {
                f.write_str("fragment has invalid text, formatting, or paragraph structure")
            }
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
