use std::fmt::{self, Debug, Formatter};
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use crate::types::Direction;
use crate::{Error, InlineStyle, Link, ParagraphKind, StylePatch};

/// A run of a [`Paragraph`]'s text with one style and link.
///
/// A paragraph's spans cover its text in order, without gaps or overlaps.
/// Their ranges are nonempty and end on extended grapheme cluster boundaries,
/// and neighboring spans differ in style or link, because paragraphs merge
/// runs that agree.
///
/// Fields may be added in minor releases, so build a span with
/// [`Span::new`] rather than a struct expression.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Span {
    /// The run's byte range in the paragraph's text.
    pub range: Range<usize>,
    /// The run's character formatting.
    pub style: InlineStyle,
    /// The run's hyperlink target, or `None` if it is not linked.
    pub link: Option<Link>,
}

impl Span {
    /// Creates a span that gives the UTF-8 byte `range` of a paragraph's text
    /// `style` and, if `link` is `Some`, a hyperlink.
    ///
    /// [`Paragraph::try_new`] checks that its spans fit the text.
    #[must_use]
    pub fn new(range: Range<usize>, style: InlineStyle, link: Option<Link>) -> Self {
        Self { range, style, link }
    }

    fn continues(&self, style: InlineStyle, link: Option<&Link>) -> bool {
        self.style == style && self.link.as_ref() == link
    }
}

pub(crate) struct SpanCursor<'spans> {
    spans: &'spans [Span],
    index: usize,
}

impl<'spans> SpanCursor<'spans> {
    pub(crate) fn new(spans: &'spans [Span]) -> Self {
        Self { spans, index: 0 }
    }

    #[cfg(feature = "accesskit")]
    pub(crate) fn index(&self) -> usize {
        self.index
    }

    pub(crate) fn at(&mut self, byte: usize) -> (InlineStyle, Option<&'spans Link>) {
        while self
            .spans
            .get(self.index)
            .is_some_and(|span| span.range.end <= byte)
        {
            self.index += 1;
        }
        self.spans
            .get(self.index)
            .map_or((InlineStyle::default(), None), |span| {
                (span.style, span.link.as_ref())
            })
    }
}

/// One paragraph of a [`Document`](crate::Document): immutable text with
/// formatting runs and a block kind.
///
/// The text never contains CR, LF, or U+2029 PARAGRAPH SEPARATOR, because
/// paragraph breaks separate paragraphs instead; it may contain U+2028 LINE
/// SEPARATOR as a soft line break. The [`spans`](Paragraph::spans) cover the
/// text. Documents, fragments, and undo history share paragraphs through
/// [`Arc`]. A paragraph builds its grapheme, scalar, and word indexes when
/// they are first needed and shares them with copies of it that differ only
/// in kind or formatting.
///
/// Byte offsets are UTF-8 offsets into [`text`](Paragraph::text). The
/// conversion methods map them to and from grapheme indexes (extended
/// grapheme clusters, what a reader sees as one character) and scalar indexes
/// (Unicode scalar values, Rust `char`s) for GUI and accessibility
/// integrations. ASCII text needs no indexes.
#[derive(Clone)]
pub struct Paragraph {
    pub(super) text: Arc<str>,
    pub(super) spans: Arc<[Span]>,
    pub(super) kind: ParagraphKind,
    pub(super) ascii: bool,
    pub(super) linked: bool,
    pub(super) indices: OnceLock<Arc<TextIndices>>,
}

#[derive(Debug, Default)]
pub(super) struct TextIndices {
    pub(super) graphemes: OnceLock<Box<[usize]>>,
    pub(super) scalars: OnceLock<Box<[usize]>>,
    pub(super) words: OnceLock<Box<[Range<usize>]>>,
}

impl PartialEq for Paragraph {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.spans == other.spans && self.kind == other.kind
    }
}

impl Eq for Paragraph {}

impl Debug for Paragraph {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Paragraph")
            .field("text", &&*self.text)
            .field("spans", &&*self.spans)
            .field("kind", &self.kind)
            .finish()
    }
}

impl Paragraph {
    /// Creates a validated paragraph from text, formatting runs, and a block
    /// kind, for importers that assemble documents with
    /// [`Document::from_paragraphs`](crate::Document::from_paragraphs).
    ///
    /// `spans` must cover `text` contiguously from byte 0 to its end with
    /// nonempty ranges whose ends fall on extended grapheme cluster
    /// boundaries, so empty text takes no spans. Neighboring spans with an
    /// equal style and link are merged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidHeadingLevel`] for a heading level outside
    /// `1..=6`, and [`Error::InvalidFragment`] if the spans do not cover the
    /// text as described or the text contains CR, LF, or U+2029.
    pub fn try_new(text: &str, spans: Vec<Span>, kind: ParagraphKind) -> Result<Self, Error> {
        let paragraph = Self::try_from_parts(text, spans, kind)?;
        if text.contains(['\r', '\n', '\u{2029}']) {
            return Err(Error::InvalidFragment);
        }
        Ok(paragraph)
    }

    /// Returns the paragraph's text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the formatting runs, which cover the text in order; empty text
    /// has none.
    #[must_use]
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// Returns the paragraph's block kind.
    #[must_use]
    pub fn kind(&self) -> ParagraphKind {
        self.kind
    }

    pub(crate) fn is_ascii(&self) -> bool {
        self.ascii
    }

    /// Returns the number of extended grapheme clusters in the text.
    ///
    /// For non-ASCII text, this builds the grapheme index if it is not built
    /// yet.
    #[must_use]
    pub fn grapheme_count(&self) -> usize {
        if self.ascii {
            self.text.len()
        } else {
            self.grapheme_boundaries().len() - 1
        }
    }

    /// Returns the byte offset where grapheme `index` starts, or the text's
    /// length when `index` equals the [grapheme count](Paragraph::grapheme_count).
    ///
    /// Returns `None` for larger indexes.
    #[must_use]
    pub fn byte_from_grapheme(&self, index: usize) -> Option<usize> {
        if index == 0 || self.ascii {
            (index <= self.text.len()).then_some(index)
        } else {
            self.grapheme_boundaries().get(index).copied()
        }
    }

    /// Returns the index of the grapheme that starts at `byte`, or the
    /// [grapheme count](Paragraph::grapheme_count) at the end of the text.
    ///
    /// Returns `None` if `byte` is past the end or not a grapheme boundary.
    #[must_use]
    pub fn grapheme_index(&self, byte: usize) -> Option<usize> {
        if byte == 0 {
            Some(0)
        } else if byte > self.text.len() || !self.text.is_char_boundary(byte) {
            None
        } else if self.ascii {
            (byte <= self.text.len()).then_some(byte)
        } else {
            self.grapheme_boundaries().binary_search(&byte).ok()
        }
    }

    /// Returns the nearest grapheme boundary before `byte`, or 0 if `byte` is
    /// 0.
    ///
    /// `byte` need not be a boundary itself, and offsets past the end give
    /// the text's length.
    #[must_use]
    pub fn previous_grapheme(&self, byte: usize) -> usize {
        self.boundary_at_or_before(byte.saturating_sub(1))
    }

    /// Returns the nearest grapheme boundary after `byte`, or the text's
    /// length if there is none.
    ///
    /// `byte` need not be a boundary itself.
    #[must_use]
    pub fn next_grapheme(&self, byte: usize) -> usize {
        self.boundary_at_or_after(byte.saturating_add(1))
    }

    /// Returns `byte` if it is a grapheme boundary, and otherwise the nearest
    /// boundary before it.
    ///
    /// Offsets past the end give the text's length.
    #[must_use]
    pub fn boundary_at_or_before(&self, byte: usize) -> usize {
        if byte == 0 || byte >= self.text.len() || self.ascii {
            byte.min(self.text.len())
        } else if let Some(boundaries) = self.cached_grapheme_boundaries() {
            boundaries[boundaries
                .partition_point(|offset| *offset <= byte)
                .saturating_sub(1)]
        } else {
            let mut byte = byte;
            while !self.text.is_char_boundary(byte) {
                byte -= 1;
            }
            self.local_grapheme_boundary(byte, Direction::Backward)
                .unwrap_or_else(|| {
                    let boundaries = self.grapheme_boundaries();
                    boundaries[boundaries
                        .partition_point(|offset| *offset <= byte)
                        .saturating_sub(1)]
                })
        }
    }

    /// Returns `byte` if it is a grapheme boundary, and otherwise the nearest
    /// boundary after it.
    ///
    /// Offsets past the end give the text's length.
    #[must_use]
    pub fn boundary_at_or_after(&self, byte: usize) -> usize {
        if byte == 0 || byte >= self.text.len() || self.ascii {
            byte.min(self.text.len())
        } else if let Some(boundaries) = self.cached_grapheme_boundaries() {
            boundaries[boundaries
                .partition_point(|offset| *offset < byte)
                .min(boundaries.len() - 1)]
        } else {
            let mut byte = byte;
            while !self.text.is_char_boundary(byte) {
                byte += 1;
            }
            self.local_grapheme_boundary(byte, Direction::Forward)
                .unwrap_or_else(|| {
                    let boundaries = self.grapheme_boundaries();
                    boundaries[boundaries
                        .partition_point(|offset| *offset < byte)
                        .min(boundaries.len() - 1)]
                })
        }
    }

    pub(super) fn is_grapheme_boundary(&self, byte: usize) -> bool {
        if byte > self.text.len() || !self.text.is_char_boundary(byte) {
            false
        } else if byte == 0 || byte == self.text.len() || self.ascii {
            true
        } else if let Some(boundaries) = self.cached_grapheme_boundaries() {
            boundaries.binary_search(&byte).is_ok()
        } else {
            let context = self.grapheme_context(byte);
            GraphemeCursor::new(byte, self.text.len(), true)
                .is_boundary(context.chunk, context.start)
                .unwrap_or_else(|_| self.grapheme_boundaries().binary_search(&byte).is_ok())
        }
    }

    fn local_grapheme_boundary(&self, byte: usize, direction: Direction) -> Option<usize> {
        let context = self.grapheme_context(byte);
        let mut cursor = GraphemeCursor::new(byte, self.text.len(), true);
        if cursor.is_boundary(context.chunk, context.start).ok()? {
            return Some(byte);
        }
        Some(match direction {
            Direction::Forward => cursor
                .next_boundary(context.chunk, context.start)
                .ok()?
                .unwrap_or(self.text.len()),
            Direction::Backward => cursor
                .prev_boundary(context.chunk, context.start)
                .ok()?
                .unwrap_or(0),
        })
    }

    fn grapheme_context(&self, byte: usize) -> GraphemeContext<'_> {
        grapheme_context(&self.text, byte)
    }

    /// Returns the number of Unicode scalar values (`char`s) in the text.
    #[must_use]
    pub fn scalar_count(&self) -> usize {
        if self.ascii {
            self.text.len()
        } else if let Some(scalars) = self.indices.get().and_then(|indices| indices.scalars.get()) {
            scalars.len() - 1
        } else {
            self.text.chars().count()
        }
    }

    /// Returns the byte offset of scalar `index`, or the text's length when
    /// `index` equals the [scalar count](Paragraph::scalar_count).
    ///
    /// Returns `None` for larger indexes.
    #[must_use]
    pub fn byte_from_scalar(&self, index: usize) -> Option<usize> {
        if index == 0 || self.ascii {
            (index <= self.text.len()).then_some(index)
        } else {
            self.scalar_boundaries().get(index).copied()
        }
    }

    /// Returns the index of the scalar that starts at `byte`, or the
    /// [scalar count](Paragraph::scalar_count) at the end of the text.
    ///
    /// Returns `None` if `byte` is past the end or inside a character.
    #[must_use]
    pub fn scalar_index(&self, byte: usize) -> Option<usize> {
        if byte == 0 {
            Some(0)
        } else if byte > self.text.len() || !self.text.is_char_boundary(byte) {
            None
        } else if self.ascii {
            (byte <= self.text.len()).then_some(byte)
        } else {
            self.scalar_boundaries().binary_search(&byte).ok()
        }
    }

    /// Returns the nearest start of a word before `byte`, or 0 if there is
    /// none.
    ///
    /// Words follow Unicode word segmentation (UAX #29), a run of
    /// punctuation, symbols, or emoji also counts as a word, and whitespace is
    /// skipped. The result is a grapheme boundary.
    #[must_use]
    pub fn previous_word(&self, byte: usize) -> usize {
        if byte == 0 {
            return 0;
        }
        let words = self.word_ranges();
        let byte = words
            .get(
                words
                    .partition_point(|word| word.start < byte)
                    .wrapping_sub(1),
            )
            .map_or(0, |word| word.start);
        self.boundary_at_or_before(byte)
    }

    /// Returns the nearest end of a word after `byte`, or the text's length if
    /// there is none.
    ///
    /// Words are as for [`previous_word`](Paragraph::previous_word). The
    /// result is a grapheme boundary.
    #[must_use]
    pub fn next_word(&self, byte: usize) -> usize {
        if byte >= self.text.len() {
            return self.text.len();
        }
        let words = self.word_ranges();
        let byte = words
            .get(words.partition_point(|word| word.end <= byte))
            .map_or(self.text.len(), |word| word.end);
        self.boundary_at_or_after(byte)
    }

    fn text_indices(&self) -> &TextIndices {
        self.indices
            .get_or_init(|| Arc::new(TextIndices::default()))
    }

    fn grapheme_boundaries(&self) -> &[usize] {
        self.text_indices().graphemes.get_or_init(|| {
            self.text
                .grapheme_indices(true)
                .map(|(byte, _)| byte)
                .chain([self.text.len()])
                .collect()
        })
    }

    pub(super) fn cached_grapheme_boundaries(&self) -> Option<&[usize]> {
        self.indices.get()?.graphemes.get().map(Box::as_ref)
    }

    #[cfg(all(test, feature = "egui"))]
    pub(crate) fn has_scalar_index(&self) -> bool {
        self.indices
            .get()
            .is_some_and(|indices| indices.scalars.get().is_some())
    }

    fn scalar_boundaries(&self) -> &[usize] {
        self.text_indices().scalars.get_or_init(|| {
            self.text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain([self.text.len()])
                .collect()
        })
    }

    pub(super) fn word_ranges(&self) -> &[Range<usize>] {
        self.text_indices()
            .words
            .get_or_init(|| word_stops(&self.text).into())
    }

    #[cfg(any(feature = "egui", feature = "accesskit"))]
    pub(crate) fn word_starts(&self) -> Vec<usize> {
        match self.indices.get().and_then(|indices| indices.words.get()) {
            Some(words) => words.iter().map(|word| word.start).collect(),
            None => word_stops(&self.text)
                .into_iter()
                .map(|word| word.start)
                .collect(),
        }
    }

    pub(super) fn with_kind(self: &Arc<Self>, kind: ParagraphKind) -> Arc<Self> {
        if self.kind == kind {
            return Arc::clone(self);
        }
        Arc::new(Self {
            text: Arc::clone(&self.text),
            spans: Arc::clone(&self.spans),
            kind,
            ascii: self.ascii,
            linked: self.linked,
            indices: self.shared_indices(),
        })
    }

    fn shared_indices(&self) -> OnceLock<Arc<TextIndices>> {
        if !self.ascii {
            self.text_indices();
        }
        self.indices.clone()
    }

    pub(super) fn with_style(&self, range: Range<usize>, patch: StylePatch) -> Option<Self> {
        self.with_runs(range, patch, None)
    }

    pub(super) fn with_link(&self, range: Range<usize>, link: Option<&Link>) -> Option<Self> {
        self.with_runs(range, StylePatch::default(), Some(link))
    }

    fn with_runs(
        &self,
        range: Range<usize>,
        patch: StylePatch,
        link: Option<Option<&Link>>,
    ) -> Option<Self> {
        if range.is_empty() {
            return None;
        }
        let Range {
            start: first,
            end: last,
        } = self.overlapping(range.clone());
        fn patched<'a>(
            span: &'a Span,
            patch: StylePatch,
            link: Option<Option<&'a Link>>,
        ) -> (InlineStyle, Option<&'a Link>) {
            let mut style = span.style;
            patch.apply(&mut style);
            (style, link.unwrap_or(span.link.as_ref()))
        }
        let patched = |span| patched(span, patch, link);
        let first = first
            + self.spans[first..last].iter().position(|span| {
                let (style, link) = patched(span);
                !span.continues(style, link)
            })?;
        let mut spans = Vec::with_capacity(self.spans.len() + 2);
        spans.extend_from_slice(&self.spans[..first]);
        for span in &self.spans[first..last] {
            let overlap = span.range.start.max(range.start)..span.range.end.min(range.end);
            let (style, link) = patched(span);
            push_span(
                &mut spans,
                span.range.start..overlap.start,
                span.style,
                span.link.as_ref(),
            );
            push_span(&mut spans, overlap.clone(), style, link);
            push_span(
                &mut spans,
                overlap.end..span.range.end,
                span.style,
                span.link.as_ref(),
            );
        }
        if let Some(span) = self.spans.get(last) {
            push_span(
                &mut spans,
                span.range.clone(),
                span.style,
                span.link.as_ref(),
            );
            spans.extend_from_slice(&self.spans[last + 1..]);
        }
        let linked = match link {
            None => self.linked,
            Some(_) => has_links(&spans),
        };
        Some(Self {
            text: Arc::clone(&self.text),
            spans: spans.into(),
            kind: self.kind,
            ascii: self.ascii,
            linked,
            indices: self.shared_indices(),
        })
    }

    pub(super) fn plain(text: &str) -> Self {
        let ascii = text.is_ascii();
        let spans = if text.is_empty() {
            Vec::new()
        } else {
            vec![Span {
                range: 0..text.len(),
                style: InlineStyle::default(),
                link: None,
            }]
        };
        Self {
            text: Arc::from(text),
            spans: spans.into(),
            kind: ParagraphKind::Body,
            ascii,
            linked: false,
            indices: OnceLock::new(),
        }
    }

    pub(super) fn styled(text: String, raw_spans: Vec<Span>, kind: ParagraphKind) -> Self {
        let ascii = text.is_ascii();
        let spans = normalize_spans(&text, raw_spans, ascii);
        Self {
            text: text.into(),
            linked: has_links(&spans),
            spans: spans.into(),
            kind,
            ascii,
            indices: OnceLock::new(),
        }
    }

    pub(crate) fn try_from_parts(
        text: &str,
        mut spans: Vec<Span>,
        kind: ParagraphKind,
    ) -> Result<Self, Error> {
        if let ParagraphKind::Heading { level } = kind
            && !(1..=6).contains(&level)
        {
            return Err(Error::InvalidHeadingLevel(level));
        }
        let ascii = text.is_ascii();
        let mut end = 0;
        let mut boundaries = None;
        let mut is_boundary = |byte: usize| {
            if !text.is_char_boundary(byte) {
                return false;
            }
            let context = grapheme_context(text, byte);
            GraphemeCursor::new(byte, text.len(), true)
                .is_boundary(context.chunk, context.start)
                .unwrap_or_else(|_| {
                    boundaries
                        .get_or_insert_with(|| text.grapheme_indices(true).map(|(byte, _)| byte))
                        .find(|boundary| *boundary >= byte)
                        == Some(byte)
                })
        };
        for span in &spans {
            if span.range.start != end
                || span.range.start >= span.range.end
                || span.range.end > text.len()
                || (!ascii && span.range.end < text.len() && !is_boundary(span.range.end))
            {
                return Err(Error::InvalidFragment);
            }
            end = span.range.end;
        }
        if end != text.len() {
            return Err(Error::InvalidFragment);
        }
        spans.dedup_by(|next, previous| {
            if previous.continues(next.style, next.link.as_ref()) {
                previous.range.end = next.range.end;
                true
            } else {
                false
            }
        });
        Ok(Self {
            text: Arc::from(text),
            linked: has_links(&spans),
            spans: spans.into(),
            kind,
            ascii,
            indices: OnceLock::new(),
        })
    }

    pub(super) fn overlapping(&self, range: Range<usize>) -> Range<usize> {
        if range.is_empty() {
            return 0..0;
        }
        let first = self
            .spans
            .partition_point(|span| span.range.end <= range.start);
        first..first + self.spans[first..].partition_point(|span| span.range.start < range.end)
    }

    pub(super) fn style_for_byte(&self, byte: usize) -> InlineStyle {
        let index = self.spans.partition_point(|span| span.range.end <= byte);
        self.spans
            .get(index)
            .map_or_else(InlineStyle::default, |span| span.style)
    }

    pub(super) fn link_for_byte(&self, byte: usize) -> Option<&Link> {
        let index = self.spans.partition_point(|span| span.range.end <= byte);
        self.spans.get(index)?.link.as_ref()
    }

    pub(super) fn link_range(&self, byte: usize) -> Option<Range<usize>> {
        let index = self.spans.partition_point(|span| span.range.end <= byte);
        let link = self.spans.get(index)?.link.as_ref()?;
        let start = self.spans[..index]
            .iter()
            .rev()
            .take_while(|span| span.link.as_ref() == Some(link))
            .last()
            .map_or(self.spans[index].range.start, |span| span.range.start);
        let end = self.spans[index + 1..]
            .iter()
            .take_while(|span| span.link.as_ref() == Some(link))
            .last()
            .map_or(self.spans[index].range.end, |span| span.range.end);
        Some(start..end)
    }
}

pub(super) fn append_slice(
    text: &mut String,
    spans: &mut Vec<Span>,
    paragraph: &Paragraph,
    range: Range<usize>,
) {
    let offset = text.len();
    text.push_str(&paragraph.text[range.clone()]);
    let [head, rest @ ..] = &paragraph.spans[paragraph.overlapping(range.clone())] else {
        return;
    };
    let clip = |span: &Span| {
        span.range.start.max(range.start) - range.start + offset
            ..span.range.end.min(range.end) - range.start + offset
    };
    push_span(spans, clip(head), head.style, head.link.as_ref());
    spans.extend(rest.iter().map(|span| Span {
        range: clip(span),
        style: span.style,
        link: span.link.clone(),
    }));
}

#[inline]
pub(super) fn push_span(
    spans: &mut Vec<Span>,
    range: Range<usize>,
    style: InlineStyle,
    link: Option<&Link>,
) {
    if range.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.range.end == range.start
        && last.continues(style, link)
    {
        last.range.end = range.end;
        return;
    }
    spans.push(Span {
        range,
        style,
        link: link.cloned(),
    });
}

fn has_links(spans: &[Span]) -> bool {
    spans.iter().any(|span| span.link.is_some())
}

fn normalize_spans(text: &str, raw: Vec<Span>, ascii: bool) -> Vec<Span> {
    if ascii || raw.len() <= 1 {
        return raw;
    }
    if raw[..raw.len() - 1].iter().all(|span| {
        let context = grapheme_context(text, span.range.end);
        GraphemeCursor::new(span.range.end, text.len(), true)
            .is_boundary(context.chunk, context.start)
            .unwrap_or(false)
    }) {
        return raw;
    }
    let mut result = Vec::with_capacity(raw.len());
    let mut spans = SpanCursor::new(&raw);
    for (start, grapheme) in text.grapheme_indices(true) {
        let (style, link) = spans.at(start);
        push_span(&mut result, start..start + grapheme.len(), style, link);
    }
    result
}

// A window of text around a byte offset, for `GraphemeCursor` queries that would otherwise
// need the whole text. Callers fall back to the full grapheme index when the cursor needs
// context beyond the window.
struct GraphemeContext<'text> {
    chunk: &'text str,
    // Byte offset of `chunk` in the whole text.
    start: usize,
}

fn grapheme_context(text: &str, byte: usize) -> GraphemeContext<'_> {
    const CONTEXT_BYTES: usize = 64;
    let mut start = byte.saturating_sub(CONTEXT_BYTES);
    let mut end = byte.saturating_add(CONTEXT_BYTES).min(text.len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    GraphemeContext {
        chunk: &text[start..end],
        start,
    }
}

fn word_stops(text: &str) -> Vec<Range<usize>> {
    if text.is_ascii() {
        return ascii_word_stops(text);
    }
    let mut stops: Vec<Range<usize>> = Vec::new();
    let mut symbols = false;
    for (start, segment) in text.split_word_bound_indices() {
        let range = start..start + segment.len();
        if is_word_segment(segment) {
            stops.push(range);
            symbols = false;
        } else if segment.chars().all(char::is_whitespace) {
            symbols = false;
        } else if symbols && let Some(run) = stops.last_mut() {
            run.end = range.end;
        } else {
            stops.push(range);
            symbols = true;
        }
    }
    stops
}

// The std tables follow the compiler's Unicode version; the segmenter's stay pinned.
fn is_word_segment(segment: &str) -> bool {
    let (major, minor, update) = char::UNICODE_VERSION;
    if (u64::from(major), u64::from(minor), u64::from(update))
        == unicode_segmentation::UNICODE_VERSION
    {
        segment.chars().any(char::is_alphanumeric)
    } else {
        segment.unicode_words().next().is_some()
    }
}

fn ascii_word_stops(text: &str) -> Vec<Range<usize>> {
    let mut stops = Vec::new();
    let mut end = 0;
    for (start, word) in text.unicode_word_indices() {
        push_symbol_runs(&mut stops, text.as_bytes(), end..start);
        end = start + word.len();
        stops.push(start..end);
    }
    push_symbol_runs(&mut stops, text.as_bytes(), end..text.len());
    stops
}

fn push_symbol_runs(stops: &mut Vec<Range<usize>>, bytes: &[u8], gap: Range<usize>) {
    let mut start = gap.start;
    for index in gap.clone() {
        if char::from(bytes[index]).is_whitespace() {
            if start < index {
                stops.push(start..index);
            }
            start = index + 1;
        }
    }
    if start < gap.end {
        stops.push(start..gap.end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_cursor_scans_forward_for_bridges_and_grapheme_normalization() {
        let bold = InlineStyle {
            bold: true,
            ..InlineStyle::default()
        };
        let italic = InlineStyle {
            italic: true,
            ..InlineStyle::default()
        };
        let link = Link::try_new("https://example.com").unwrap();
        let spans = [
            Span {
                range: 0..2,
                style: bold,
                link: None,
            },
            Span {
                range: 2..4,
                style: italic,
                link: Some(link.clone()),
            },
            Span {
                range: 6..8,
                style: bold,
                link: None,
            },
        ];
        let mut cursor = SpanCursor::new(&spans);
        for byte in 0..10 {
            let current = spans.iter().position(|span| span.range.end > byte);
            let expected = current.map_or((InlineStyle::default(), None), |index| {
                (spans[index].style, spans[index].link.as_ref())
            });
            assert_eq!(cursor.at(byte), expected, "byte {byte}");
            #[cfg(feature = "accesskit")]
            assert_eq!(
                cursor.index(),
                current.unwrap_or(spans.len()),
                "byte {byte}"
            );
        }

        let paragraph = Paragraph::styled(
            "ae\u{301}x".to_owned(),
            vec![
                Span {
                    range: 0..2,
                    style: bold,
                    link: None,
                },
                Span {
                    range: 2..4,
                    style: italic,
                    link: Some(link),
                },
                Span {
                    range: 4..5,
                    style: italic,
                    link: None,
                },
            ],
            ParagraphKind::Body,
        );
        assert_eq!(
            paragraph.spans(),
            [
                Span {
                    range: 0..4,
                    style: bold,
                    link: None,
                },
                Span {
                    range: 4..5,
                    style: italic,
                    link: None,
                },
            ]
        );
    }

    #[test]
    fn word_stops_classify_words_with_the_pinned_segmentation_tables() {
        for character in '\u{80}'..=char::MAX {
            let segment = character.to_string();
            assert_eq!(
                is_word_segment(&segment),
                segment.unicode_words().next().is_some(),
                "U+{:04X} with std Unicode {:?}",
                u32::from(character),
                char::UNICODE_VERSION
            );
        }
        assert_eq!(
            word_stops("ab \u{558}\u{558}\u{558} cd"),
            [0..2, 3..9, 10..12],
            "U+0558 is unassigned in the pinned Unicode 17 tables and a letter in Unicode 18"
        );
    }
}
