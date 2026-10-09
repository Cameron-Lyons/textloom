//! Immutable rich-text clipboard fragments and dependency-free interchange.

use std::{fmt, sync::Arc};

use crate::{Color, Document, InlineStyle, Paragraph, ParagraphKind, Span};

const MAGIC: &[u8; 4] = b"TLFR";
const VERSION: u8 = 1;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_PARAGRAPHS: usize = 1_000_000;
const MAX_SPANS: usize = 1_000_000;

/// An immutable piece of a rich-text document, suitable for a clipboard.
///
/// Paragraphs are shared with the source document until edited. A fragment
/// always contains at least one paragraph, including when its text is empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fragment {
    paragraphs: Vec<Arc<Paragraph>>,
}

impl Default for Fragment {
    fn default() -> Self {
        Self::from_text("")
    }
}

impl Fragment {
    /// Import plain text, normalizing CRLF and lone CR to paragraph breaks.
    pub fn from_text(text: &str) -> Self {
        Self {
            paragraphs: Document::from_text(text).into_paragraphs(),
        }
    }

    /// Capture an entire document, sharing its immutable paragraph allocations.
    pub fn from_document(document: &Document) -> Self {
        Self {
            paragraphs: document.paragraphs().to_vec(),
        }
    }

    pub(crate) fn from_paragraphs(paragraphs: Vec<Arc<Paragraph>>) -> Self {
        if paragraphs.is_empty() {
            Self::default()
        } else {
            Self { paragraphs }
        }
    }

    pub(crate) fn into_paragraphs(self) -> Vec<Arc<Paragraph>> {
        self.paragraphs
    }

    /// Return the ordered, immutable paragraphs, including an empty paragraph
    /// when the fragment contains no text.
    pub fn paragraphs(&self) -> &[Arc<Paragraph>] {
        &self.paragraphs
    }

    /// Export text with LF between paragraphs, omitting visual list markers.
    pub fn plain_text(&self) -> String {
        crate::export::plain_text(self.paragraphs())
    }

    /// Encode text, paragraph kinds and inline formatting without losing data.
    ///
    /// The native format begins with `TLFR`, version byte `1`, then a
    /// little-endian `u64` paragraph count. Each paragraph contains a kind tag
    /// (`0` body, `1` heading + `u8` level, `2` bullet + `u8` indent, or `3`
    /// ordered + `u8` indent + little-endian `u32` start), a `u64` UTF-8 byte
    /// length and its text, then a `u64` span count. Each span contains its
    /// `u64` end offset, a style byte and optional four-byte sRGBA color. Span
    /// starts follow the preceding end, initially zero. Style bits 0 through
    /// 5 are bold, italic, underline, strike, code and color-present.
    ///
    /// The version and fixed integer widths make files independent of target
    /// architecture. [`Self::from_bytes`] checks the complete format and
    /// applies explicit resource limits to untrusted data. Encoding has no
    /// size limit; fragments above those decoding limits cannot be imported
    /// with [`Self::from_bytes`].
    pub fn to_bytes(&self) -> Vec<u8> {
        Self::encode_paragraphs(&self.paragraphs)
    }

    pub(crate) fn encode_paragraphs(paragraphs: &[Arc<Paragraph>]) -> Vec<u8> {
        let capacity = paragraphs.iter().fold(13usize, |capacity, paragraph| {
            let kind_bytes = match paragraph.kind() {
                ParagraphKind::Body => 1,
                ParagraphKind::Heading { .. } | ParagraphKind::Bullet { .. } => 2,
                ParagraphKind::Ordered { .. } => 6,
            };
            paragraph.spans().iter().fold(
                capacity
                    .saturating_add(kind_bytes + 16)
                    .saturating_add(paragraph.text().len()),
                |capacity, span| {
                    capacity.saturating_add(9 + usize::from(span.style.foreground.is_some()) * 4)
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
                ParagraphKind::Ordered { indent, start } => {
                    output.extend_from_slice(&[3, indent]);
                    output.extend_from_slice(&start.to_le_bytes());
                }
            }
            write_usize(&mut output, paragraph.text().len());
            output.extend_from_slice(paragraph.text().as_bytes());
            write_usize(&mut output, paragraph.spans().len());
            for span in paragraph.spans() {
                write_usize(&mut output, span.range.end);
                output.push(style_flags(span.style));
                if let Some(Color(color)) = span.style.foreground {
                    output.extend_from_slice(&color);
                }
            }
        }
        output
    }

    /// Decode native rich text, rejecting invalid or noncanonical data.
    ///
    /// Inputs are limited to 64 MiB, one million paragraphs and one million
    /// total spans. Declared lengths are checked against the remaining input
    /// before allocation. UTF-8, complete span coverage, grapheme boundaries,
    /// heading levels, flags and trailing bytes are validated.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FragmentError> {
        if bytes.len() > MAX_BYTES {
            return Err(FragmentError::LimitExceeded);
        }
        let mut input = Reader::new(bytes);
        if input.take(MAGIC.len())? != MAGIC {
            return Err(FragmentError::InvalidMagic);
        }
        let version = input.byte()?;
        if version != VERSION {
            return Err(FragmentError::UnsupportedVersion(version));
        }
        let count = input.length()?;
        if count == 0 {
            return Err(FragmentError::InvalidParagraphCount);
        }
        if count > MAX_PARAGRAPHS {
            return Err(FragmentError::LimitExceeded);
        }
        // Even an empty body paragraph needs one tag and two u64 lengths.
        if count > input.remaining() / 17 {
            return Err(FragmentError::Truncated);
        }
        let mut paragraphs = Vec::with_capacity(count);
        let mut total_spans = 0usize;
        for _ in 0..count {
            let kind = match input.byte()? {
                0 => ParagraphKind::Body,
                1 => {
                    let level = input.byte()?;
                    if !(1..=6).contains(&level) {
                        return Err(FragmentError::InvalidParagraphKind);
                    }
                    ParagraphKind::Heading { level }
                }
                2 => ParagraphKind::Bullet {
                    indent: input.byte()?,
                },
                3 => ParagraphKind::Ordered {
                    indent: input.byte()?,
                    start: u32::from_le_bytes(input.take(4)?.try_into().unwrap()),
                },
                _ => return Err(FragmentError::InvalidParagraphKind),
            };
            let length = input.length()?;
            let text =
                std::str::from_utf8(input.take(length)?).map_err(|_| FragmentError::InvalidUtf8)?;
            if text.as_bytes().contains(&b'\r') || text.as_bytes().contains(&b'\n') {
                return Err(FragmentError::InvalidParagraphText);
            }
            let span_count = input.length()?;
            total_spans = total_spans
                .checked_add(span_count)
                .ok_or(FragmentError::LimitExceeded)?;
            if total_spans > MAX_SPANS {
                return Err(FragmentError::LimitExceeded);
            }
            // Every span needs at least its end offset and style byte.
            if span_count > input.remaining() / 9 {
                return Err(FragmentError::Truncated);
            }
            let mut spans = Vec::with_capacity(span_count);
            let mut start = 0;
            let mut previous_style = None;
            for _ in 0..span_count {
                let end = input.length()?;
                if end <= start || end > text.len() {
                    return Err(FragmentError::InvalidSpans);
                }
                let style = read_style(&mut input)?;
                if previous_style == Some(style) {
                    return Err(FragmentError::InvalidSpans);
                }
                spans.push(Span {
                    range: start..end,
                    style,
                });
                start = end;
                previous_style = Some(style);
            }
            if start != text.len() {
                return Err(FragmentError::InvalidSpans);
            }
            // The paragraph constructor validates grapheme boundaries in one
            // forward scan; repeating segmentation here doubles import work.
            let paragraph = Paragraph::from_parts(text, spans, kind)
                .map_err(|_| FragmentError::InvalidSpans)?;
            paragraphs.push(Arc::new(paragraph));
        }
        if input.remaining() != 0 {
            return Err(FragmentError::TrailingData);
        }
        Ok(Self { paragraphs })
    }

    /// Export escaped HTML with semantic formatting, headings and nested lists.
    ///
    /// Text is escaped and the generated attributes contain only validated
    /// numeric values. The wrapper preserves whitespace. Indentation increases
    /// nest lists beneath the preceding item; gaps in indentation do not create
    /// artificial empty list items. Explicit ordered numbering is preserved.
    /// NUL characters become U+FFFD because HTML cannot represent them as text.
    pub fn to_html(&self) -> String {
        crate::export::html(self.paragraphs())
    }
}

/// A malformed or unsupported native rich-text fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FragmentError {
    /// The input ends before a declared field or payload is complete.
    Truncated,
    /// The input does not begin with the `TLFR` signature.
    InvalidMagic,
    /// The version byte is not supported by this decoder.
    UnsupportedVersion(u8),
    /// The input exceeds a size, paragraph count, span count, or platform limit.
    LimitExceeded,
    /// The input declares no paragraphs.
    InvalidParagraphCount,
    /// A paragraph kind tag or heading level is invalid.
    InvalidParagraphKind,
    /// Paragraph text contains a CR or LF rather than a paragraph boundary.
    InvalidParagraphText,
    /// Paragraph text is not valid UTF-8.
    InvalidUtf8,
    /// Spans do not canonically cover the text at extended grapheme boundaries.
    InvalidSpans,
    /// A style byte sets reserved bits.
    InvalidStyle,
    /// Bytes remain after the last declared paragraph.
    TrailingData,
}

impl fmt::Display for FragmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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
}

fn read_style(input: &mut Reader<'_>) -> Result<InlineStyle, FragmentError> {
    let flags = input.byte()?;
    if flags & !0x3f != 0 {
        return Err(FragmentError::InvalidStyle);
    }
    Ok(InlineStyle {
        bold: flags & 1 != 0,
        italic: flags & 2 != 0,
        underline: flags & 4 != 0,
        strikethrough: flags & 8 != 0,
        code: flags & 16 != 0,
        foreground: if flags & 32 != 0 {
            Some(Color(input.take(4)?.try_into().unwrap()))
        } else {
            None
        },
    })
}

struct Reader<'a> {
    remaining: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn remaining(&self) -> usize {
        self.remaining.len()
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], FragmentError> {
        if length > self.remaining.len() {
            return Err(FragmentError::Truncated);
        }
        let (bytes, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, FragmentError> {
        Ok(self.take(1)?[0])
    }

    fn length(&mut self) -> Result<usize, FragmentError> {
        let value = u64::from_le_bytes(self.take(8)?.try_into().unwrap());
        usize::try_from(value).map_err(|_| FragmentError::LimitExceeded)
    }
}
