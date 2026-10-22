use std::fmt::{self, Debug, Display, Formatter};
use std::ops::Range;
use std::sync::Arc;

/// A location in a [`Document`](crate::Document): a paragraph index and a
/// UTF-8 byte offset into that paragraph's text.
///
/// Positions order by paragraph, then by byte. A position is valid in a
/// document when `paragraph` names an existing paragraph and `byte` is an
/// extended grapheme cluster boundary of its text; the start (`0`) and end
/// (the text's length in bytes) are always boundaries. Methods that take a
/// position reject other values with [`Error::InvalidPosition`], so a position
/// never splits a character, a combining sequence, an emoji sequence, or a
/// flag.
///
/// Paragraph breaks are not part of any paragraph's text: the end of one
/// paragraph and the start of the next are different positions, one step
/// apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    /// Zero-based index of the paragraph in the document.
    pub paragraph: usize,
    /// Offset into the paragraph's text in UTF-8 bytes, not characters or
    /// graphemes.
    pub byte: usize,
}

impl Position {
    /// Creates a position `byte` bytes into paragraph `paragraph`.
    ///
    /// The position is not checked against any document;
    /// [`Document::validate_position`](crate::Document::validate_position)
    /// does that.
    #[must_use]
    pub const fn new(paragraph: usize, byte: usize) -> Self {
        Self { paragraph, byte }
    }
}

/// A directional selection between two positions.
///
/// `anchor` is where the selection started and stays fixed while the
/// selection is extended; `focus` is the end that moves, where the caret is
/// drawn. The focus may come before the anchor. When both are equal, the
/// selection is a caret. [`Selection::range`] returns the selected positions in
/// document order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Selection {
    /// The fixed end, where the selection started.
    pub anchor: Position,
    /// The moving end, where the caret is.
    pub focus: Position,
}

impl Selection {
    /// Creates an empty selection, a caret, at `position`.
    #[must_use]
    pub const fn caret(position: Position) -> Self {
        Self {
            anchor: position,
            focus: position,
        }
    }

    /// Creates a selection from `anchor` to `focus`, which may come in either
    /// order.
    #[must_use]
    pub const fn new(anchor: Position, focus: Position) -> Self {
        Self { anchor, focus }
    }

    /// Returns whether the selection is empty, that is, whether `anchor` equals
    /// `focus`.
    #[must_use]
    pub fn is_caret(self) -> bool {
        self.anchor == self.focus
    }

    /// Returns the selected range in document order, from the earlier to the
    /// later of `anchor` and `focus`.
    #[must_use]
    pub fn range(self) -> Range<Position> {
        self.anchor.min(self.focus)..self.anchor.max(self.focus)
    }
}

/// A text or fill color as red, green, blue, and alpha bytes.
///
/// Alpha is straight (not premultiplied): 255 is opaque and 0 is fully
/// transparent. The associated constants are Excel's ten standard colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(
    /// The `[red, green, blue, alpha]` channels.
    pub [u8; 4],
);

impl Color {
    /// Excel's standard Dark Red, `#c00000`.
    pub const DARK_RED: Self = Self([0xc0, 0x00, 0x00, 0xff]);
    /// Excel's standard Red, `#ff0000`.
    pub const RED: Self = Self([0xff, 0x00, 0x00, 0xff]);
    /// Excel's standard Orange, `#ffc000`.
    pub const ORANGE: Self = Self([0xff, 0xc0, 0x00, 0xff]);
    /// Excel's standard Yellow, `#ffff00`.
    pub const YELLOW: Self = Self([0xff, 0xff, 0x00, 0xff]);
    /// Excel's standard Light Green, `#92d050`.
    pub const LIGHT_GREEN: Self = Self([0x92, 0xd0, 0x50, 0xff]);
    /// Excel's standard Green, `#00b050`.
    pub const GREEN: Self = Self([0x00, 0xb0, 0x50, 0xff]);
    /// Excel's standard Light Blue, `#00b0f0`.
    pub const LIGHT_BLUE: Self = Self([0x00, 0xb0, 0xf0, 0xff]);
    /// Excel's standard Blue, `#0070c0`.
    pub const BLUE: Self = Self([0x00, 0x70, 0xc0, 0xff]);
    /// Excel's standard Dark Blue, `#002060`.
    pub const DARK_BLUE: Self = Self([0x00, 0x20, 0x60, 0xff]);
    /// Excel's standard Purple, `#7030a0`.
    pub const PURPLE: Self = Self([0x70, 0x30, 0xa0, 0xff]);

    /// Excel's ten standard colors with their English display names, in
    /// Excel's palette order, for font and fill color pickers.
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

/// A validated hyperlink target, carried by [`Span::link`](crate::Span::link).
///
/// A target is nonempty, at most [`Link::MAX_LEN`] bytes long, and contains no
/// control characters, U+2028 LINE SEPARATOR, or U+2029 PARAGRAPH SEPARATOR.
/// Textloom stores targets verbatim and never parses or opens them; HTML
/// export writes text linked to a `javascript:`, `vbscript:`, or `data:`
/// target without its link. Links compare and hash by target text, and clones
/// share one allocation.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Link(Arc<str>);

impl Link {
    /// The longest accepted target, in UTF-8 bytes (64 KiB).
    pub const MAX_LEN: usize = 64 * 1024;

    /// Validates `target` and creates a link to it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLink`] if `target` is empty, is longer than
    /// [`Link::MAX_LEN`] bytes, or contains a control character (Unicode
    /// category Cc), U+2028, or U+2029.
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

    /// Returns the target exactly as it was passed to [`Link::new`].
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn address(&self) -> usize {
        self.0.as_ptr().addr()
    }
}

impl Debug for Link {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Link").field(&&*self.0).finish()
    }
}

impl Display for Link {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The character formatting of a run of text.
///
/// The attributes are independent of each other. The default is plain text in
/// the renderer's default color with no fill. HTML export maps the attributes
/// to `<strong>`, `<em>`, `<u>`, `<s>`, `<code>`, and a `<span>` carrying
/// `color` and `background-color`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InlineStyle {
    /// Whether the text has a bold (heavy) weight.
    pub bold: bool,
    /// Whether the text is slanted.
    pub italic: bool,
    /// Whether the text has a line under it.
    pub underline: bool,
    /// Whether the text has a line through it.
    pub strikethrough: bool,
    /// Whether the text is inline code, which renderers draw in a monospace
    /// font.
    pub code: bool,
    /// The text color, or `None` for the renderer's default text color.
    pub foreground: Option<Color>,
    /// The fill color behind the text, or `None` for no fill, like Excel's
    /// Fill Color.
    pub background: Option<Color>,
}

/// The formatting that all selected text agrees on, as reported by
/// [`Editor::selection_style`](crate::Editor::selection_style) for
/// formatting toolbars.
///
/// Each attribute is `Some` when every selected run has the same value and
/// `None` when the selection mixes values. For the colors, `Some(None)` means
/// the default color (or no fill) throughout and `Some(Some(color))` means one
/// explicit color throughout. Converting an [`InlineStyle`] gives a uniform
/// summary with every attribute `Some`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionStyle {
    /// `Some(true)` if all selected text is bold, `Some(false)` if none is,
    /// and `None` if it is mixed.
    pub bold: Option<bool>,
    /// `Some(true)` if all selected text is italic, `Some(false)` if none is,
    /// and `None` if it is mixed.
    pub italic: Option<bool>,
    /// `Some(true)` if all selected text is underlined, `Some(false)` if none
    /// is, and `None` if it is mixed.
    pub underline: Option<bool>,
    /// `Some(true)` if all selected text is struck through, `Some(false)` if
    /// none is, and `None` if it is mixed.
    pub strikethrough: Option<bool>,
    /// `Some(true)` if all selected text is inline code, `Some(false)` if none
    /// is, and `None` if it is mixed.
    pub code: Option<bool>,
    /// The text color shared by all selected text (`Some(None)` for the
    /// default color), or `None` if colors are mixed.
    pub foreground: Option<Option<Color>>,
    /// The fill shared by all selected text (`Some(None)` for no fill), or
    /// `None` if fills are mixed.
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

/// A partial formatting change: each `Some` attribute replaces that attribute,
/// and each `None` attribute leaves it unchanged.
///
/// [`Editor::apply_style`](crate::Editor::apply_style) applies a patch to the
/// selection. The default patch changes nothing. Converting an
/// [`InlineStyle`] sets every attribute, and converting a [`SelectionStyle`]
/// copies only its uniform attributes, as a format painter does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StylePatch {
    /// The new bold state, or `None` to keep each run's own.
    pub bold: Option<bool>,
    /// The new italic state, or `None` to keep each run's own.
    pub italic: Option<bool>,
    /// The new underline state, or `None` to keep each run's own.
    pub underline: Option<bool>,
    /// The new strikethrough state, or `None` to keep each run's own.
    pub strikethrough: Option<bool>,
    /// The new inline code state, or `None` to keep each run's own.
    pub code: Option<bool>,
    /// The new text color (`Some(None)` for the default color), or `None` to
    /// keep each run's own.
    pub foreground: Option<Option<Color>>,
    /// The new fill (`Some(None)` for no fill), or `None` to keep each run's
    /// own.
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
    /// Overwrites the attributes of `style` that this patch sets and leaves
    /// the others unchanged.
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

/// The block type of a paragraph.
///
/// New kinds may be added in minor releases, so a `match` needs a wildcard
/// arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ParagraphKind {
    /// Ordinary body text.
    #[default]
    Body,
    /// A section heading.
    Heading {
        /// The heading level, from 1 (most prominent) to 6, as in HTML's `<h1>`
        /// to `<h6>`. Commands and constructors reject other levels with
        /// [`Error::InvalidHeadingLevel`].
        level: u8,
    },
    /// An item of a bulleted (unordered) list.
    Bullet {
        /// The nesting depth, where 0 is the outermost list.
        indent: u8,
    },
    /// An item of a numbered list; see
    /// [`Document::list_number`](crate::Document::list_number) for the number
    /// it displays.
    Ordered {
        /// The nesting depth, where 0 is the outermost list.
        indent: u8,
        /// An explicit number that restarts the list at this item, or `None`
        /// to continue the list from the nearest earlier item at this depth.
        start: Option<u32>,
    },
    /// A block quote paragraph. Consecutive quote paragraphs form one quote.
    Quote,
}

/// A caret movement for [`Editor::move_cursor`](crate::Editor::move_cursor).
///
/// Movements are logical: they follow the order of the text, not its layout,
/// so a renderer provides movement across wrapped rows and bidirectional text
/// itself. Word movements stop at Unicode words and at runs of punctuation,
/// symbols, or emoji, skipping whitespace.
///
/// New movements may be added in minor releases, so a `match` needs a
/// wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Movement {
    /// To the previous grapheme boundary, or from a paragraph start to the end
    /// of the previous paragraph.
    GraphemeBackward,
    /// To the next grapheme boundary, or from a paragraph end to the start of
    /// the next paragraph.
    GraphemeForward,
    /// To the nearest start of a word or punctuation run before the caret, or
    /// from a paragraph start to the end of the previous paragraph.
    WordBackward,
    /// To the nearest end of a word or punctuation run after the caret, or
    /// from a paragraph end to the start of the next paragraph.
    WordForward,
    /// To the start of the caret's paragraph, passing any soft line breaks.
    ParagraphStart,
    /// To the end of the caret's paragraph, passing any soft line breaks.
    ParagraphEnd,
    /// To the start of the first paragraph.
    DocumentStart,
    /// To the end of the last paragraph.
    DocumentEnd,
    /// To the same grapheme column in the previous paragraph, or to its end if
    /// it is shorter. A sequence of vertical moves keeps the column it started
    /// from. In the first paragraph, the caret stays in that paragraph.
    ParagraphUp,
    /// To the same grapheme column in the next paragraph, or to its end if it
    /// is shorter. A sequence of vertical moves keeps the column it started
    /// from. In the last paragraph, the caret stays in that paragraph.
    ParagraphDown,
}

/// What [`Editor::move_cursor`](crate::Editor::move_cursor) does with the
/// selection's anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[expect(
    clippy::exhaustive_enums,
    reason = "a movement either keeps the anchor or moves it; there is no third mode"
)]
pub enum SelectionMode {
    /// Moves the caret and collapses any range selection, as an arrow key does.
    /// A grapheme or word movement from a range selection collapses it to its
    /// start (backward movements) or end (forward movements) instead of moving.
    Move,
    /// Moves only the selection's focus and keeps its anchor, as Shift with an
    /// arrow key does.
    Extend,
}

/// An on/off character attribute for
/// [`Editor::toggle_style`](crate::Editor::toggle_style).
///
/// New attributes may be added in minor releases, so a `match` needs a
/// wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum InlineAttribute {
    /// Bold weight, [`InlineStyle::bold`].
    Bold,
    /// Slanted text, [`InlineStyle::italic`].
    Italic,
    /// A line under the text, [`InlineStyle::underline`].
    Underline,
    /// A line through the text, [`InlineStyle::strikethrough`].
    Strikethrough,
    /// Inline code, [`InlineStyle::code`].
    Code,
}

/// The direction in which an internal search or boundary scan moves through
/// text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    /// Toward the start of the text.
    Backward,
    /// Toward the end of the text.
    Forward,
}

/// An error from an editing or document operation.
///
/// Operations validate their input before changing anything, so an operation
/// that returns an error leaves the document, selection, and history as they
/// were. New variants may be added in minor releases, so a `match` needs a
/// wildcard arm.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The position is past the last paragraph, past the end of its paragraph,
    /// or not on an extended grapheme cluster boundary of its text. Holds the
    /// rejected position.
    InvalidPosition(Position),
    /// The range's start comes after its end.
    InvalidRange,
    /// The paragraph passed to [`Paragraph::new`](crate::Paragraph::new) is
    /// malformed: its text contains CR, LF, or U+2029, or its spans do not
    /// cover the text contiguously with nonempty ranges whose ends fall on
    /// grapheme boundaries.
    InvalidFragment,
    /// The IME preedit selection is reversed, extends past the preedit text,
    /// or does not fall on UTF-8 character boundaries.
    InvalidCompositionSelection,
    /// The heading level is outside `1..=6`. Holds the rejected level.
    InvalidHeadingLevel(u8),
    /// The command cannot run during an IME composition; commit or cancel the
    /// composition first.
    CompositionActive,
    /// An [`AccessiblePosition`](crate::accessibility::AccessiblePosition)
    /// names a paragraph or grapheme that does not exist.
    InvalidGraphemePosition {
        /// The requested paragraph index.
        paragraph: usize,
        /// The requested grapheme index within that paragraph.
        grapheme: usize,
    },
    /// The link target is empty, longer than [`Link::MAX_LEN`] bytes, or
    /// contains a control character, U+2028, or U+2029.
    InvalidLink,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPosition(position) => {
                write!(
                    f,
                    "invalid grapheme position {}:{}",
                    position.paragraph, position.byte
                )
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
