use std::{
    fmt,
    ops::Range,
    sync::{Arc, OnceLock},
};

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use crate::{Error, InlineStyle, Link, ParagraphKind, StylePatch};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: InlineStyle,
    pub link: Option<Link>,
}

impl Span {
    fn continues(&self, style: InlineStyle, link: Option<&Link>) -> bool {
        self.style == style && self.link.as_ref() == link
    }
}

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

impl fmt::Debug for Paragraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Paragraph")
            .field("text", &&*self.text)
            .field("spans", &&*self.spans)
            .field("kind", &self.kind)
            .finish()
    }
}

impl Paragraph {
    pub fn new(text: &str, spans: Vec<Span>, kind: ParagraphKind) -> Result<Self, Error> {
        Self::from_parts(text, spans, kind)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    pub fn kind(&self) -> ParagraphKind {
        self.kind
    }

    pub fn grapheme_count(&self) -> usize {
        if self.ascii {
            self.text.len()
        } else {
            self.grapheme_boundaries().len() - 1
        }
    }

    pub fn byte_from_grapheme(&self, index: usize) -> Option<usize> {
        if index == 0 || self.ascii {
            (index <= self.text.len()).then_some(index)
        } else {
            self.grapheme_boundaries().get(index).copied()
        }
    }

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

    pub fn previous_grapheme(&self, byte: usize) -> usize {
        self.boundary_at_or_before(byte.saturating_sub(1))
    }

    pub fn next_grapheme(&self, byte: usize) -> usize {
        self.boundary_at_or_after(byte.saturating_add(1))
    }

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
            self.local_grapheme_boundary(byte, false)
                .unwrap_or_else(|| {
                    let boundaries = self.grapheme_boundaries();
                    boundaries[boundaries
                        .partition_point(|offset| *offset <= byte)
                        .saturating_sub(1)]
                })
        }
    }

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
            self.local_grapheme_boundary(byte, true).unwrap_or_else(|| {
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
            let (chunk, start) = self.grapheme_context(byte);
            GraphemeCursor::new(byte, self.text.len(), true)
                .is_boundary(chunk, start)
                .unwrap_or_else(|_| self.grapheme_boundaries().binary_search(&byte).is_ok())
        }
    }

    fn local_grapheme_boundary(&self, byte: usize, forward: bool) -> Option<usize> {
        let (chunk, start) = self.grapheme_context(byte);
        let mut cursor = GraphemeCursor::new(byte, self.text.len(), true);
        if cursor.is_boundary(chunk, start).ok()? {
            return Some(byte);
        }
        Some(if forward {
            cursor
                .next_boundary(chunk, start)
                .ok()?
                .unwrap_or(self.text.len())
        } else {
            cursor.prev_boundary(chunk, start).ok()?.unwrap_or(0)
        })
    }

    fn grapheme_context(&self, byte: usize) -> (&str, usize) {
        grapheme_context(&self.text, byte)
    }

    pub fn scalar_count(&self) -> usize {
        if self.ascii {
            self.text.len()
        } else if let Some(scalars) = self.indices.get().and_then(|indices| indices.scalars.get()) {
            scalars.len() - 1
        } else {
            self.text.chars().count()
        }
    }

    pub fn byte_from_scalar(&self, index: usize) -> Option<usize> {
        if index == 0 || self.ascii {
            (index <= self.text.len()).then_some(index)
        } else {
            self.scalar_boundaries().get(index).copied()
        }
    }

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

    pub(super) fn with_kind(&self, kind: ParagraphKind) -> Self {
        Self {
            text: Arc::clone(&self.text),
            spans: Arc::clone(&self.spans),
            kind,
            ascii: self.ascii,
            linked: self.linked,
            indices: self.shared_indices(),
        }
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
        let first = self
            .spans
            .partition_point(|span| span.range.end <= range.start);
        let last = self
            .spans
            .partition_point(|span| span.range.start < range.end);
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

    pub(crate) fn from_parts(
        text: &str,
        mut spans: Vec<Span>,
        kind: ParagraphKind,
    ) -> Result<Self, Error> {
        if let ParagraphKind::Heading { level } = kind
            && !(1..=6).contains(&level)
        {
            return Err(Error::InvalidHeadingLevel(level));
        }
        if text.contains(['\r', '\n', '\u{2029}']) {
            return Err(Error::InvalidFragment);
        }
        let ascii = text.is_ascii();
        let mut end = 0;
        let mut boundaries = text.grapheme_indices(true).map(|(byte, _)| byte);
        for span in &spans {
            if span.range.start != end
                || span.range.start >= span.range.end
                || span.range.end > text.len()
                || (!ascii
                    && span.range.end < text.len()
                    && boundaries.find(|byte| *byte >= span.range.end) != Some(span.range.end))
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
    let first = paragraph
        .spans
        .partition_point(|span| span.range.end <= range.start);
    let last = paragraph
        .spans
        .partition_point(|span| span.range.start < range.end);
    if first >= last {
        return;
    }
    let clip = |span: &Span| {
        span.range.start.max(range.start) - range.start + offset
            ..span.range.end.min(range.end) - range.start + offset
    };
    let head = &paragraph.spans[first];
    push_span(spans, clip(head), head.style, head.link.as_ref());
    spans.extend(paragraph.spans[first + 1..last].iter().map(|span| Span {
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
        let (chunk, start) = grapheme_context(text, span.range.end);
        GraphemeCursor::new(span.range.end, text.len(), true)
            .is_boundary(chunk, start)
            .unwrap_or(false)
    }) {
        return raw;
    }
    let mut result = Vec::with_capacity(raw.len());
    let mut index = 0;
    for (start, grapheme) in text.grapheme_indices(true) {
        while raw.get(index).is_some_and(|span| span.range.end <= start) {
            index += 1;
        }
        let (style, link) = raw
            .get(index)
            .map_or((InlineStyle::default(), None), |span| {
                (span.style, span.link.as_ref())
            });
        push_span(&mut result, start..start + grapheme.len(), style, link);
    }
    result
}

fn grapheme_context(text: &str, byte: usize) -> (&str, usize) {
    const CONTEXT_BYTES: usize = 64;
    let mut start = byte.saturating_sub(CONTEXT_BYTES);
    let mut end = byte.saturating_add(CONTEXT_BYTES).min(text.len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[start..end], start)
}

fn word_stops(text: &str) -> Vec<Range<usize>> {
    let mut words = text
        .unicode_word_indices()
        .map(|(start, _)| start)
        .peekable();
    let mut stops: Vec<Range<usize>> = Vec::new();
    let mut symbols = false;
    for (start, segment) in text.split_word_bound_indices() {
        let range = start..start + segment.len();
        if words.next_if_eq(&start).is_some() {
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
