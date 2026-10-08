use std::{fmt, ops::Range};

/// A caret position. `byte` is a UTF-8 offset at an extended grapheme boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    /// Zero-based paragraph index.
    pub paragraph: usize,
    /// UTF-8 byte offset within that paragraph, including its end.
    pub byte: usize,
}

impl Position {
    /// Construct coordinates without validation; the receiving document validates them.
    pub const fn new(paragraph: usize, byte: usize) -> Self {
        Self { paragraph, byte }
    }
}

/// A directional selection: the anchor stays fixed while the focus moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// Fixed endpoint when extending a selection.
    pub anchor: Position,
    /// Moving endpoint and the location used to determine typing style.
    pub focus: Position,
}

impl Selection {
    /// Construct an empty selection at a position without validating it.
    pub const fn caret(position: Position) -> Self {
        Self {
            anchor: position,
            focus: position,
        }
    }
    /// Construct a directional selection without validating its endpoints.
    pub const fn new(anchor: Position, focus: Position) -> Self {
        Self { anchor, focus }
    }
    /// Whether both endpoints identify the same position.
    pub fn is_caret(self) -> bool {
        self.anchor == self.focus
    }
    /// The selected half-open range, with endpoints sorted in document order.
    pub fn range(self) -> Range<Position> {
        self.anchor.min(self.focus)..self.anchor.max(self.focus)
    }
}

/// Unpremultiplied sRGBA color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(
    /// Red, green, blue, and alpha channels, each in `0..=255`.
    pub [u8; 4],
);

/// Character formatting; the default has no emphasis or explicit foreground color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InlineStyle {
    /// Bold emphasis.
    pub bold: bool,
    /// Italic emphasis.
    pub italic: bool,
    /// Underline decoration.
    pub underline: bool,
    /// Strikethrough decoration.
    pub strikethrough: bool,
    /// Inline code formatting, typically rendered with a monospace font.
    pub code: bool,
    /// Explicit foreground color, or the renderer's default when absent.
    pub foreground: Option<Color>,
}

/// Only supplied attributes change. `Some(None)` clears a foreground color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StylePatch {
    /// Replace bold emphasis when supplied.
    pub bold: Option<bool>,
    /// Replace italic emphasis when supplied.
    pub italic: Option<bool>,
    /// Replace underline decoration when supplied.
    pub underline: Option<bool>,
    /// Replace strikethrough decoration when supplied.
    pub strikethrough: Option<bool>,
    /// Replace inline code formatting when supplied.
    pub code: Option<bool>,
    /// Replace the foreground color; `Some(None)` restores the renderer's default.
    pub foreground: Option<Option<Color>>,
}

impl StylePatch {
    /// Update only attributes supplied by this patch.
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

/// Semantic block formatting. List markers are presentation, separate from paragraph text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ParagraphKind {
    /// Ordinary body text.
    #[default]
    Body,
    /// A heading that continues as body text when split.
    Heading {
        /// Heading level in `1..=6`; invalid values are rejected by editing and import.
        level: u8,
    },
    /// A bullet list item that retains its indentation when split.
    Bullet {
        /// Zero-based indentation level.
        indent: u8,
    },
    /// An ordered list item whose numbering continues when split.
    Ordered {
        /// Zero-based indentation level.
        indent: u8,
        /// This item's displayed ordinal. Continuations saturate at `u32::MAX`.
        start: u32,
    },
}

/// Logical caret movement, independent of visual wrapping or bidirectional layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Movement {
    /// Preceding extended grapheme, crossing paragraph breaks when needed.
    GraphemeBackward,
    /// Following extended grapheme, crossing paragraph breaks when needed.
    GraphemeForward,
    /// Preceding Unicode word start; at paragraph start, move to the preceding paragraph's end.
    WordBackward,
    /// Following Unicode word end; at paragraph end, move to the following paragraph's start.
    WordForward,
    /// Start of the current logical paragraph.
    ParagraphStart,
    /// End of the current logical paragraph.
    ParagraphEnd,
    /// Start of the document.
    DocumentStart,
    /// End of the document.
    DocumentEnd,
    /// Previous paragraph, retaining the preferred grapheme column.
    ParagraphUp,
    /// Next paragraph, retaining the preferred grapheme column.
    ParagraphDown,
}

/// Validation and editing failures. Failed validation preserves document content.
/// Additional validation errors may be added in future releases.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A coordinate is outside the document or inside an extended grapheme.
    InvalidPosition(Position),
    /// A forward range has reversed endpoints or invalid paragraph indices.
    InvalidRange,
    /// Rich text has invalid separators, formatting runs, or paragraph metadata.
    InvalidFragment,
    /// Preedit selection offsets are reversed, out of bounds, or inside UTF-8 encodings.
    InvalidCompositionSelection,
    /// Heading level is outside `1..=6`.
    InvalidHeadingLevel(u8),
    /// Commit or cancel IME preedit before performing this operation.
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
