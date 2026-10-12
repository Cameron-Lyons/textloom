use std::{fmt, ops::Range, sync::Arc};

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
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
    #[must_use]
    pub fn range(self) -> Range<Position> {
        self.anchor.min(self.focus)..self.anchor.max(self.focus)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(pub [u8; 4]);

impl Color {
    pub const DARK_RED: Self = Self([0xc0, 0x00, 0x00, 0xff]);
    pub const RED: Self = Self([0xff, 0x00, 0x00, 0xff]);
    pub const ORANGE: Self = Self([0xff, 0xc0, 0x00, 0xff]);
    pub const YELLOW: Self = Self([0xff, 0xff, 0x00, 0xff]);
    pub const LIGHT_GREEN: Self = Self([0x92, 0xd0, 0x50, 0xff]);
    pub const GREEN: Self = Self([0x00, 0xb0, 0x50, 0xff]);
    pub const LIGHT_BLUE: Self = Self([0x00, 0xb0, 0xf0, 0xff]);
    pub const BLUE: Self = Self([0x00, 0x70, 0xc0, 0xff]);
    pub const DARK_BLUE: Self = Self([0x00, 0x20, 0x60, 0xff]);
    pub const PURPLE: Self = Self([0x70, 0x30, 0xa0, 0xff]);

    pub const STANDARD: [(&'static str, Self); 10] = [
        ("Dark Red", Self::DARK_RED),
        ("Red", Self::RED),
        ("Orange", Self::ORANGE),
        ("Yellow", Self::YELLOW),
        ("Light Green", Self::LIGHT_GREEN),
        ("Green", Self::GREEN),
        ("Light Blue", Self::LIGHT_BLUE),
        ("Blue", Self::BLUE),
        ("Dark Blue", Self::DARK_BLUE),
        ("Purple", Self::PURPLE),
    ];
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Link(Arc<str>);

impl Link {
    pub const MAX_LEN: usize = 64 * 1024;

    pub fn new(target: &str) -> Result<Self, Error> {
        if target.is_empty()
            || target.len() > Self::MAX_LEN
            || target.chars().any(|character| {
                character.is_control() || matches!(character, '\u{2028}' | '\u{2029}')
            })
        {
            return Err(Error::InvalidLink);
        }
        Ok(Self(Arc::from(target)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn address(&self) -> usize {
        self.0.as_ptr().addr()
    }
}

impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Link").field(&&*self.0).finish()
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub foreground: Option<Color>,
    pub background: Option<Color>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionStyle {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strikethrough: Option<bool>,
    pub code: Option<bool>,
    pub foreground: Option<Option<Color>>,
    pub background: Option<Option<Color>>,
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
            background: Some(style.background),
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
        self.background = self.background.filter(|value| *value == style.background);
    }

    pub(crate) fn is_fully_mixed(self) -> bool {
        self.bold.is_none()
            && self.italic.is_none()
            && self.underline.is_none()
            && self.strikethrough.is_none()
            && self.code.is_none()
            && self.foreground.is_none()
            && self.background.is_none()
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
    pub background: Option<Option<Color>>,
}

impl From<InlineStyle> for StylePatch {
    fn from(style: InlineStyle) -> Self {
        SelectionStyle::from(style).into()
    }
}

impl From<SelectionStyle> for StylePatch {
    fn from(style: SelectionStyle) -> Self {
        Self {
            bold: style.bold,
            italic: style.italic,
            underline: style.underline,
            strikethrough: style.strikethrough,
            code: style.code,
            foreground: style.foreground,
            background: style.background,
        }
    }
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
        if let Some(value) = self.background {
            style.background = value;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
        start: Option<u32>,
    },
    Quote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum InlineAttribute {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Code,
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
    InvalidGraphemePosition { paragraph: usize, grapheme: usize },
    InvalidLink,
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
            Self::InvalidGraphemePosition {
                paragraph,
                grapheme,
            } => write!(
                f,
                "invalid grapheme index {grapheme} in paragraph {paragraph}"
            ),
            Self::InvalidLink => f.write_str(
                "link target must be nonempty, at most 64 KiB, and free of control characters",
            ),
        }
    }
}
impl std::error::Error for Error {}
