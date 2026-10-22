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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DecodeLimits {
    pub max_bytes: usize,
    pub max_paragraphs: usize,
    pub max_spans: usize,
}

impl DecodeLimits {
    pub const DEFAULT: Self = Self {
        max_bytes: 64 * 1024 * 1024,
        max_paragraphs: 1_000_000,
        max_spans: 1_000_000,
    };

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
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self::from_paragraphs(Document::from_text(text).into_paragraphs())
    }

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

    #[must_use]
    pub fn paragraphs(&self) -> &[Arc<Paragraph>] {
        &self.paragraphs
    }

    #[must_use]
    pub fn plain_text(&self) -> String {
        export::plain_text(self.paragraphs())
    }

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

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FragmentError> {
        Self::from_bytes_with_limits(bytes, DecodeLimits::DEFAULT)
    }

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
            let paragraph = Paragraph::from_parts(&text, spans, kind)
                .map_err(|_| FragmentError::InvalidSpans)?;
            paragraphs.push(Arc::new(paragraph));
        }
        if input.remaining() != 0 {
            return Err(FragmentError::TrailingData);
        }
        Ok(Self::from_paragraphs(paragraphs))
    }

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
    pub fn from_bytes_with_limits(
        bytes: &[u8],
        limits: DecodeLimits,
    ) -> Result<Self, FragmentError> {
        Fragment::from_bytes_with_limits(bytes, limits).map(Self::from)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FragmentError {
    Truncated,
    InvalidMagic,
    UnsupportedVersion(u8),
    LimitExceeded,
    InvalidParagraphCount,
    InvalidParagraphKind,
    InvalidParagraphText,
    InvalidUtf8,
    InvalidSpans,
    InvalidLink,
    InvalidStyle,
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
        _ => Link::new(target).map_err(|_| FragmentError::InvalidLink)?,
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
