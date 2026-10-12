use std::{
    ops::Range,
    sync::{Arc, OnceLock},
};

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use crate::{Error, InlineStyle, ParagraphKind, StylePatch};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: InlineStyle,
}

#[derive(Clone, Debug)]
pub struct Paragraph {
    pub(super) text: Arc<str>,
    pub(super) spans: Arc<[Span]>,
    pub(super) kind: ParagraphKind,
    pub(super) ascii: bool,
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

impl Paragraph {
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
        if self.ascii {
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
        const CONTEXT_BYTES: usize = 64;
        let mut start = byte.saturating_sub(CONTEXT_BYTES);
        let mut end = byte.saturating_add(CONTEXT_BYTES).min(self.text.len());
        while !self.text.is_char_boundary(start) {
            start += 1;
        }
        while !self.text.is_char_boundary(end) {
            end -= 1;
        }
        (&self.text[start..end], start)
    }

    pub fn scalar_count(&self) -> usize {
        if self.ascii {
            self.text.len()
        } else {
            self.scalar_boundaries().len() - 1
        }
    }

    pub fn byte_from_scalar(&self, index: usize) -> Option<usize> {
        if self.ascii {
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
        self.text_indices().words.get_or_init(|| {
            self.text
                .unicode_word_indices()
                .map(|(byte, word)| byte..byte + word.len())
                .collect()
        })
    }

    pub(super) fn with_kind(&self, kind: ParagraphKind) -> Self {
        Self {
            text: Arc::clone(&self.text),
            spans: Arc::clone(&self.spans),
            kind,
            ascii: self.ascii,
            indices: self.indices.clone(),
        }
    }

    pub(super) fn with_style(&self, range: Range<usize>, patch: StylePatch) -> Option<Self> {
        if range.is_empty() {
            return None;
        }
        let first = self
            .spans
            .partition_point(|span| span.range.end <= range.start);
        let last = self
            .spans
            .partition_point(|span| span.range.start < range.end);
        let first = first
            + self.spans[first..last].iter().position(|span| {
                let mut style = span.style;
                patch.apply(&mut style);
                style != span.style
            })?;
        let mut spans = Vec::with_capacity(self.spans.len() + 2);
        spans.extend_from_slice(&self.spans[..first]);
        for span in &self.spans[first..last] {
            let overlap = span.range.start.max(range.start)..span.range.end.min(range.end);
            push_span(&mut spans, span.range.start..overlap.start, span.style);
            let mut style = span.style;
            patch.apply(&mut style);
            push_span(&mut spans, overlap.clone(), style);
            push_span(&mut spans, overlap.end..span.range.end, span.style);
        }
        if let Some(span) = self.spans.get(last) {
            push_span(&mut spans, span.range.clone(), span.style);
            spans.extend_from_slice(&self.spans[last + 1..]);
        }
        Some(Self {
            text: Arc::clone(&self.text),
            spans: spans.into(),
            kind: self.kind,
            ascii: self.ascii,
            indices: self.indices.clone(),
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
            }]
        };
        Self {
            text: Arc::from(text),
            spans: spans.into(),
            kind: ParagraphKind::Body,
            ascii,
            indices: new_text_indices(ascii),
        }
    }

    pub(super) fn styled(text: String, raw_spans: Vec<Span>, kind: ParagraphKind) -> Self {
        let ascii = text.is_ascii();
        let spans = normalize_spans(&text, raw_spans, ascii);
        Self {
            text: text.into(),
            spans: spans.into(),
            kind,
            ascii,
            indices: new_text_indices(ascii),
        }
    }

    pub(crate) fn from_parts(
        text: &str,
        mut spans: Vec<Span>,
        kind: ParagraphKind,
    ) -> Result<Self, Error> {
        if text.as_bytes().contains(&b'\r')
            || text.as_bytes().contains(&b'\n')
            || matches!(kind, ParagraphKind::Heading { level } if !(1..=6).contains(&level))
        {
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
            if next.style == previous.style {
                previous.range.end = next.range.end;
                true
            } else {
                false
            }
        });
        Ok(Self {
            text: Arc::from(text),
            spans: spans.into(),
            kind,
            ascii,
            indices: new_text_indices(ascii),
        })
    }

    pub(super) fn style_for_byte(&self, byte: usize) -> InlineStyle {
        let index = self.spans.partition_point(|span| span.range.end <= byte);
        self.spans
            .get(index)
            .map_or_else(InlineStyle::default, |span| span.style)
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
    for span in &paragraph.spans[first..] {
        if span.range.start >= range.end {
            break;
        }
        let start = span.range.start.max(range.start) - range.start + offset;
        let end = span.range.end.min(range.end) - range.start + offset;
        push_span(spans, start..end, span.style);
    }
}

pub(super) fn push_span(spans: &mut Vec<Span>, range: Range<usize>, style: InlineStyle) {
    if range.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.range.end == range.start
        && last.style == style
    {
        last.range.end = range.end;
        return;
    }
    spans.push(Span { range, style });
}

fn normalize_spans(text: &str, raw: Vec<Span>, ascii: bool) -> Vec<Span> {
    if ascii || raw.len() <= 1 {
        return raw;
    }
    let mut result = Vec::with_capacity(raw.len());
    let mut index = 0;
    for (start, grapheme) in text.grapheme_indices(true) {
        while raw.get(index).is_some_and(|span| span.range.end <= start) {
            index += 1;
        }
        let style = raw
            .get(index)
            .map_or_else(InlineStyle::default, |span| span.style);
        push_span(&mut result, start..start + grapheme.len(), style);
    }
    result
}

fn new_text_indices(ascii: bool) -> OnceLock<Arc<TextIndices>> {
    if ascii {
        OnceLock::new()
    } else {
        OnceLock::from(Arc::new(TextIndices::default()))
    }
}
