use std::borrow::Cow;
use std::fmt::{self, Debug, Display, Formatter};
use std::sync::Arc;

use crate::document::Levels;
use crate::{Color, Document, InlineStyle, Link, Paragraph, ParagraphKind, Span, export};

const MAGIC: &[u8; 4] = b"TLFR";
const VERSION: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    V1,
    V2,
}

impl Format {
    fn try_new(version: u8) -> Result<Self, FragmentError> {
        match version {
            1 => Ok(Self::V1),
            2 => Ok(Self::V2),
            _ => Err(FragmentError::UnsupportedVersion(version)),
        }
    }

    fn style_bits(self) -> u8 {
        match self {
            Self::V1 => 0x3f,
            Self::V2 => 0xff,
        }
    }
}

/// Bounds on the TLFR input that
/// [`Fragment::from_bytes_with_limits`] accepts, which protect against
/// oversized untrusted input.
///
/// Fields may be added in minor releases, so start from
/// [`DecodeLimits::DEFAULT`] (also the [`Default`]) or
/// [`DecodeLimits::UNLIMITED`] and assign the fields to change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DecodeLimits {
    /// The longest accepted input, in bytes; 64 MiB by default.
    pub max_bytes: usize,
    /// The most paragraphs accepted; one million by default.
    pub max_paragraphs: usize,
    /// The most formatting spans accepted across all paragraphs; one million
    /// by default.
    pub max_spans: usize,
}

impl DecodeLimits {
    /// Limits for untrusted input: 64 MiB, one million paragraphs, and one
    /// million spans. In-memory documents can exceed them, so reopening such a
    /// document's own snapshot needs larger limits.
    pub const DEFAULT: Self = Self {
        max_bytes: 64 * 1024 * 1024,
        max_paragraphs: 1_000_000,
        max_spans: 1_000_000,
    };

    /// No limits, for trusted input such as the application's own snapshots.
    pub const UNLIMITED: Self = Self {
        max_bytes: usize::MAX,
        max_paragraphs: usize::MAX,
        max_spans: usize::MAX,
    };
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Rich text for the clipboard and interchange: paragraphs with their
/// formatting, links, and kinds.
///
/// A fragment always has at least one paragraph. Paragraphs captured whole
/// are shared with the document they came from rather than copied.
/// [`Document::fragment`] and [`Editor::selected_fragment`](crate::Editor::selected_fragment)
/// capture one from a range, and
/// [`Editor::insert_fragment`](crate::Editor::insert_fragment) pastes one.
/// [`to_bytes`](Fragment::to_bytes) and [`from_bytes`](Fragment::from_bytes)
/// convert it to and from Textloom's native format (TLFR), and
/// [`to_html`](Fragment::to_html) and [`plain_text`](Fragment::plain_text)
/// export the other clipboard formats.
///
/// Two fragments are equal when their paragraphs are equal.
#[derive(Clone)]
pub struct Fragment {
    paragraphs: Vec<Arc<Paragraph>>,
    numbering: Levels,
}

impl PartialEq for Fragment {
    fn eq(&self, other: &Self) -> bool {
        self.paragraphs == other.paragraphs
    }
}

impl Eq for Fragment {}

impl Debug for Fragment {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Fragment")
            .field("paragraphs", &self.paragraphs)
            .finish_non_exhaustive()
    }
}

impl Default for Fragment {
    fn default() -> Self {
        Self::from_text("")
    }
}

impl Fragment {
    /// Creates a fragment of unformatted body paragraphs from plain text,
    /// split into paragraphs as by [`Document::from_text`].
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self::from_paragraphs(Document::from_text(text).into_paragraphs())
    }

    /// Creates a fragment of a whole document, sharing its paragraphs.
    ///
    /// `Fragment::from(&document)` does the same.
    #[must_use]
    pub fn from_document(document: &Document) -> Self {
        Self::from_paragraphs(document.paragraphs().to_vec())
    }

    pub(crate) fn from_paragraphs(paragraphs: Vec<Arc<Paragraph>>) -> Self {
        Self::captured(paragraphs, Levels::default())
    }

    pub(crate) fn captured(paragraphs: Vec<Arc<Paragraph>>, numbering: Levels) -> Self {
        if paragraphs.is_empty() {
            Self::from_text("")
        } else {
            Self {
                paragraphs,
                numbering,
            }
        }
    }

    pub(crate) fn into_paragraphs(self) -> Vec<Arc<Paragraph>> {
        self.paragraphs
    }

    /// Returns the paragraphs in order; there is always at least one.
    #[must_use]
    pub fn paragraphs(&self) -> &[Arc<Paragraph>] {
        &self.paragraphs
    }

    /// Returns the plain text, written as by [`Document::plain_text`].
    #[must_use]
    pub fn plain_text(&self) -> String {
        export::plain_text(self.paragraphs())
    }

    /// Encodes the fragment losslessly in TLFR version 2, the format that
    /// [`Fragment::from_bytes`] decodes.
    ///
    /// The encoding is the same on every architecture. The numbers that
    /// captured list items displayed in their source document are not
    /// encoded; only [`to_html`](Fragment::to_html) uses them.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        Self::encode_paragraphs(&self.paragraphs)
    }

    pub(crate) fn encode_paragraphs(paragraphs: &[Arc<Paragraph>]) -> Vec<u8> {
        let capacity = paragraphs.iter().fold(13usize, |capacity, paragraph| {
            let kind_bytes = match paragraph.kind() {
                ParagraphKind::Body | ParagraphKind::Quote => 1,
                ParagraphKind::Heading { .. }
                | ParagraphKind::Bullet { .. }
                | ParagraphKind::Ordered { start: None, .. } => 2,
                ParagraphKind::Ordered { start: Some(_), .. } => 6,
            };
            paragraph.spans().iter().fold(
                capacity
                    .saturating_add(kind_bytes + 16)
                    .saturating_add(paragraph.text().len()),
                |capacity, span| {
                    let colors = usize::from(span.style.foreground.is_some())
                        + usize::from(span.style.background.is_some());
                    let link = span.link.as_ref().map_or(0, |link| 8 + link.as_str().len());
                    capacity.saturating_add(9 + colors * 4 + link)
                },
            )
        });
        let mut output = Vec::with_capacity(capacity);
        output.extend_from_slice(MAGIC);
        output.push(VERSION);
        write_usize(&mut output, paragraphs.len());
        for paragraph in paragraphs {
            match paragraph.kind() {
                ParagraphKind::Body => output.push(0),
                ParagraphKind::Heading { level } => output.extend_from_slice(&[1, level]),
                ParagraphKind::Bullet { indent } => output.extend_from_slice(&[2, indent]),
                ParagraphKind::Ordered {
                    indent,
                    start: None,
                } => output.extend_from_slice(&[4, indent]),
                ParagraphKind::Ordered {
                    indent,
                    start: Some(start),
                } => {
                    output.extend_from_slice(&[5, indent]);
                    output.extend_from_slice(&start.to_le_bytes());
                }
                ParagraphKind::Quote => output.push(6),
            }
            write_usize(&mut output, paragraph.text().len());
            output.extend_from_slice(paragraph.text().as_bytes());
            write_usize(&mut output, paragraph.spans().len());
            for span in paragraph.spans() {
                write_usize(&mut output, span.range.end);
                output.push(style_flags(span.style) | (u8::from(span.link.is_some()) << 7));
                if let Some(Color(color)) = span.style.foreground {
                    output.extend_from_slice(&color);
                }
                if let Some(Color(fill)) = span.style.background {
                    output.extend_from_slice(&fill);
                }
                if let Some(link) = &span.link {
                    write_usize(&mut output, link.as_str().len());
                    output.extend_from_slice(link.as_str().as_bytes());
                }
            }
        }
        output
    }

    /// Decodes a fragment from TLFR bytes, version 1 or 2, within
    /// [`DecodeLimits::DEFAULT`].
    ///
    /// Decoding validates the whole input, so untrusted bytes are safe to
    /// pass. Version 1 stored each list item's displayed number; decoding
    /// keeps a number as an explicit start only where it differs from the
    /// number the item would compute, so the items display the same numbers.
    ///
    /// # Errors
    ///
    /// Returns a [`FragmentError`] naming the first problem found: input
    /// beyond the default limits, a wrong signature or unsupported version,
    /// truncated or trailing data, or an invalid paragraph kind, text, span,
    /// style, or link.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FragmentError> {
        Self::from_bytes_with_limits(bytes, DecodeLimits::DEFAULT)
    }

    /// Decodes a fragment from TLFR bytes as
    /// [`from_bytes`](Fragment::from_bytes) does, within `limits`.
    ///
    /// # Errors
    ///
    /// Returns [`FragmentError::LimitExceeded`] if the input is longer than
    /// `limits.max_bytes` or declares more paragraphs or spans than `limits`
    /// allow, and otherwise the errors of
    /// [`from_bytes`](Fragment::from_bytes).
    pub fn from_bytes_with_limits(
        bytes: &[u8],
        limits: DecodeLimits,
    ) -> Result<Self, FragmentError> {
        if bytes.len() > limits.max_bytes {
            return Err(FragmentError::LimitExceeded);
        }
        let mut input = Reader::new(bytes);
        if input.take(MAGIC.len())? != MAGIC {
            return Err(FragmentError::InvalidMagic);
        }
        let format = Format::try_new(input.byte()?)?;
        let count = input.length()?;
        if count == 0 {
            return Err(FragmentError::InvalidParagraphCount);
        }
        if count > limits.max_paragraphs {
            return Err(FragmentError::LimitExceeded);
        }
        if count > input.remaining() / 17 {
            return Err(FragmentError::Truncated);
        }
        let mut paragraphs = Vec::with_capacity(count);
        let mut total_spans = 0usize;
        let mut last_link = None;
        let mut levels = Levels::default();
        for _ in 0..count {
            let kind = match (format, input.byte()?) {
                (_, 0) => ParagraphKind::Body,
                (_, 1) => {
                    let level = input.byte()?;
                    if !(1..=6).contains(&level) {
                        return Err(FragmentError::InvalidParagraphKind);
                    }
                    ParagraphKind::Heading { level }
                }
                (_, 2) => ParagraphKind::Bullet {
                    indent: input.byte()?,
                },
                (Format::V1, 3) => {
                    let indent = input.byte()?;
                    let ordinal = input.u32()?;
                    ParagraphKind::Ordered {
                        indent,
                        start: (ordinal != levels.continuation(indent)).then_some(ordinal),
                    }
                }
                (Format::V2, 4) => ParagraphKind::Ordered {
                    indent: input.byte()?,
                    start: None,
                },
                (Format::V2, 5) => ParagraphKind::Ordered {
                    indent: input.byte()?,
                    start: Some(input.u32()?),
                },
                (Format::V2, 6) => ParagraphKind::Quote,
                _ => return Err(FragmentError::InvalidParagraphKind),
            };
            if format == Format::V1 {
                levels.push(kind);
            }
            let length = input.length()?;
            let text =
                str::from_utf8(input.take(length)?).map_err(|_| FragmentError::InvalidUtf8)?;
            if text.contains(['\r', '\n']) {
                return Err(FragmentError::InvalidParagraphText);
            }
            let text = match (format, text.contains('\u{2029}')) {
                (_, false) => Cow::Borrowed(text),
                (Format::V1, true) => Cow::Owned(text.replace('\u{2029}', "\u{2028}")),
                (Format::V2, true) => return Err(FragmentError::InvalidParagraphText),
            };
            let span_count = input.length()?;
            total_spans = total_spans
                .checked_add(span_count)
                .ok_or(FragmentError::LimitExceeded)?;
            if total_spans > limits.max_spans {
                return Err(FragmentError::LimitExceeded);
            }
            if span_count > input.remaining() / 9 {
                return Err(FragmentError::Truncated);
            }
            let mut spans: Vec<Span> = Vec::with_capacity(span_count);
            let mut start = 0;
            for _ in 0..span_count {
                let end = input.length()?;
                if end <= start || end > text.len() {
                    return Err(FragmentError::InvalidSpans);
                }
                let (style, link) = read_format(&mut input, format, &mut last_link)?;
                if spans
                    .last()
                    .is_some_and(|previous| previous.style == style && previous.link == link)
                {
                    return Err(FragmentError::InvalidSpans);
                }
                spans.push(Span {
                    range: start..end,
                    style,
                    link,
                });
                start = end;
            }
            if start != text.len() {
                return Err(FragmentError::InvalidSpans);
            }
            let paragraph = Paragraph::try_from_parts(&text, spans, kind)
                .map_err(|_| FragmentError::InvalidSpans)?;
            paragraphs.push(Arc::new(paragraph));
        }
        if input.remaining() != 0 {
            return Err(FragmentError::TrailingData);
        }
        Ok(Self::from_paragraphs(paragraphs))
    }

    /// Exports the fragment as HTML, as [`Document::to_html`] does.
    ///
    /// List items show the numbers they displayed in the document they were
    /// captured from, so a copy of the third to fifth items of a list exports
    /// `<ol start="3">`.
    #[must_use]
    pub fn to_html(&self) -> String {
        export::html(self.paragraphs(), self.numbering.clone())
    }
}

impl From<&Document> for Fragment {
    fn from(document: &Document) -> Self {
        Self::from_document(document)
    }
}

impl From<Fragment> for Document {
    fn from(fragment: Fragment) -> Self {
        Self::from_paragraphs(fragment.into_paragraphs())
    }
}

impl Document {
    /// Decodes a document from TLFR bytes as
    /// [`Document::from_bytes`] does, within `limits`.
    ///
    /// Use [`DecodeLimits::UNLIMITED`] or raised limits to reopen trusted
    /// snapshots of documents larger than the default limits.
    ///
    /// # Errors
    ///
    /// Returns the [`FragmentError`] that
    /// [`Fragment::from_bytes_with_limits`] reports.
    pub fn from_bytes_with_limits(
        bytes: &[u8],
        limits: DecodeLimits,
    ) -> Result<Self, FragmentError> {
        Fragment::from_bytes_with_limits(bytes, limits).map(Self::from)
    }
}

/// An error decoding TLFR bytes, from [`Fragment::from_bytes`] or
/// [`Document::from_bytes`] and their `_with_limits` variants.
///
/// New variants may be added in minor releases, so a `match` needs a wildcard
/// arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FragmentError {
    /// The input ends before the data it declares, or declares more
    /// paragraphs or spans than its remaining bytes can hold.
    Truncated,
    /// The input does not start with the `TLFR` signature.
    InvalidMagic,
    /// The format version is not 1 or 2. Holds the version byte.
    UnsupportedVersion(u8),
    /// The input exceeds a [`DecodeLimits`] bound, or declares a length that
    /// does not fit in `usize`.
    LimitExceeded,
    /// The input declares zero paragraphs.
    InvalidParagraphCount,
    /// A paragraph kind tag is unknown or not defined in the input's format
    /// version, or a heading level is outside `1..=6`.
    InvalidParagraphKind,
    /// Paragraph text contains CR or LF, or in version 2, U+2029.
    InvalidParagraphText,
    /// Paragraph text is not valid UTF-8.
    InvalidUtf8,
    /// Formatting spans do not cover the paragraph text contiguously with
    /// nonempty ranges that end on grapheme boundaries, or a span repeats the
    /// style and link of the span before it, which the encoder would have
    /// merged.
    InvalidSpans,
    /// A link target is not valid UTF-8 or not a valid [`Link`].
    InvalidLink,
    /// A span's style flags set bits that the input's format version does not
    /// define.
    InvalidStyle,
    /// Bytes follow the last paragraph.
    TrailingData,
}

impl Display for FragmentError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("rich-text fragment is truncated"),
            Self::InvalidMagic => formatter.write_str("invalid rich-text fragment signature"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported rich-text fragment version {version}"
                )
            }
            Self::LimitExceeded => {
                formatter.write_str("rich-text fragment exceeds decoding limits")
            }
            Self::InvalidParagraphCount => {
                formatter.write_str("rich-text fragment must contain a paragraph")
            }
            Self::InvalidParagraphKind => {
                formatter.write_str("invalid paragraph kind or heading level")
            }
            Self::InvalidParagraphText => {
                formatter.write_str("paragraph text contains a line break")
            }
            Self::InvalidUtf8 => formatter.write_str("paragraph text is invalid UTF-8"),
            Self::InvalidSpans => {
                formatter.write_str("formatting spans do not cover grapheme boundaries")
            }
            Self::InvalidStyle => formatter.write_str("formatting contains unknown style flags"),
            Self::InvalidLink => formatter.write_str("formatting contains an invalid link target"),
            Self::TrailingData => formatter.write_str("rich-text fragment contains trailing data"),
        }
    }
}

impl std::error::Error for FragmentError {}

fn write_usize(output: &mut Vec<u8>, value: usize) {
    output.extend_from_slice(&(value as u64).to_le_bytes());
}

fn style_flags(style: InlineStyle) -> u8 {
    u8::from(style.bold)
        | (u8::from(style.italic) << 1)
        | (u8::from(style.underline) << 2)
        | (u8::from(style.strikethrough) << 3)
        | (u8::from(style.code) << 4)
        | (u8::from(style.foreground.is_some()) << 5)
        | (u8::from(style.background.is_some()) << 6)
}

fn read_format(
    input: &mut Reader<'_>,
    format: Format,
    last_link: &mut Option<Link>,
) -> Result<(InlineStyle, Option<Link>), FragmentError> {
    let flags = input.byte()?;
    if flags & !format.style_bits() != 0 {
        return Err(FragmentError::InvalidStyle);
    }
    let style = InlineStyle {
        bold: flags & 1 != 0,
        italic: flags & 2 != 0,
        underline: flags & 4 != 0,
        strikethrough: flags & 8 != 0,
        code: flags & 16 != 0,
        foreground: if flags & 32 != 0 {
            Some(Color(input.array()?))
        } else {
            None
        },
        background: if flags & 64 != 0 {
            Some(Color(input.array()?))
        } else {
            None
        },
    };
    if flags & 128 == 0 {
        return Ok((style, None));
    }
    let length = input.length()?;
    if length > Link::MAX_LEN {
        return Err(FragmentError::InvalidLink);
    }
    let target = str::from_utf8(input.take(length)?).map_err(|_| FragmentError::InvalidLink)?;
    let link = match last_link {
        Some(link) if link.as_str() == target => link.clone(),
        _ => Link::try_new(target).map_err(|_| FragmentError::InvalidLink)?,
    };
    *last_link = Some(link.clone());
    Ok((style, Some(link)))
}

struct Reader<'input> {
    remaining: &'input [u8],
}

impl<'input> Reader<'input> {
    fn new(bytes: &'input [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn remaining(&self) -> usize {
        self.remaining.len()
    }

    fn take(&mut self, length: usize) -> Result<&'input [u8], FragmentError> {
        if length > self.remaining.len() {
            return Err(FragmentError::Truncated);
        }
        let (bytes, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], FragmentError> {
        let (bytes, remaining) = self
            .remaining
            .split_first_chunk()
            .ok_or(FragmentError::Truncated)?;
        self.remaining = remaining;
        Ok(*bytes)
    }

    fn byte(&mut self) -> Result<u8, FragmentError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, FragmentError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn length(&mut self) -> Result<usize, FragmentError> {
        let value = u64::from_le_bytes(self.array()?);
        usize::try_from(value).map_err(|_| FragmentError::LimitExceeded)
    }
}
