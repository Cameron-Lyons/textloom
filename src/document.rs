use std::{
    borrow::Cow,
    ops::Range,
    sync::{Arc, OnceLock},
};

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use crate::{Error, Fragment, InlineStyle, ParagraphKind, Position, StylePatch};

/// A run of equally formatted text, measured in UTF-8 bytes within a paragraph.
///
/// Runs cover the complete paragraph, never overlap, and always end at extended
/// grapheme boundaries. Empty paragraphs have no runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// Half-open UTF-8 byte range within its paragraph.
    pub range: Range<usize>,
    /// Formatting applied to every grapheme in the range.
    pub style: InlineStyle,
}

/// An immutable paragraph. Its text and formatting can be shared by history.
#[derive(Clone, Debug)]
pub struct Paragraph {
    text: Arc<str>,
    spans: Arc<[Span]>,
    kind: ParagraphKind,
    ascii: bool,
    indices: OnceLock<Arc<TextIndices>>,
}

/// Immutable text indexes are built only when requested, and remain shared
/// when formatting or paragraph metadata changes. ASCII grapheme and scalar
/// boundaries are implicit; only word navigation needs an allocated index.
#[derive(Debug, Default)]
struct TextIndices {
    graphemes: OnceLock<Box<[usize]>>,
    scalars: OnceLock<Box<[usize]>>,
    words: OnceLock<Box<[Range<usize>]>>,
}

impl PartialEq for Paragraph {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.spans == other.spans && self.kind == other.kind
    }
}

impl Eq for Paragraph {}

impl Paragraph {
    /// Text without paragraph separators.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Complete, contiguous formatting runs, merged where adjacent styles agree.
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// Semantic paragraph formatting used by renderers and exporters.
    pub fn kind(&self) -> ParagraphKind {
        self.kind
    }

    /// Number of extended grapheme clusters in this paragraph.
    /// Unicode boundaries are indexed lazily and reused by later queries.
    pub fn grapheme_count(&self) -> usize {
        if self.ascii {
            self.text.len()
        } else {
            self.grapheme_boundaries().len() - 1
        }
    }

    /// Map a grapheme index to a UTF-8 byte offset. The index immediately
    /// after the last grapheme maps to the end of the paragraph.
    pub fn byte_from_grapheme(&self, index: usize) -> Option<usize> {
        if self.ascii {
            (index <= self.text.len()).then_some(index)
        } else {
            self.grapheme_boundaries().get(index).copied()
        }
    }

    /// Map a UTF-8 byte offset to a grapheme index, rejecting offsets within
    /// a cluster or outside the paragraph.
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

    /// Return the preceding grapheme boundary, clamped at paragraph start.
    /// Arbitrary byte offsets are accepted; offsets beyond the end return the end.
    pub fn previous_grapheme(&self, byte: usize) -> usize {
        if byte == 0 {
            0
        } else if self.ascii {
            byte.saturating_sub(1).min(self.text.len())
        } else {
            let boundaries = self.grapheme_boundaries();
            boundaries[boundaries
                .partition_point(|offset| *offset < byte)
                .saturating_sub(1)]
        }
    }

    /// Return the following grapheme boundary, clamped at paragraph end.
    /// Arbitrary byte offsets are accepted, including offsets inside UTF-8 encodings.
    pub fn next_grapheme(&self, byte: usize) -> usize {
        if byte >= self.text.len() {
            self.text.len()
        } else if self.ascii {
            byte.saturating_add(1).min(self.text.len())
        } else {
            let boundaries = self.grapheme_boundaries();
            boundaries[boundaries
                .partition_point(|offset| *offset <= byte)
                .min(boundaries.len() - 1)]
        }
    }

    /// Snap a byte offset backward to a grapheme boundary.
    /// Offsets beyond the paragraph are clamped to its end.
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

    /// Snap a byte offset forward to a grapheme boundary.
    /// Offsets beyond the paragraph are clamped to its end.
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

    fn is_grapheme_boundary(&self, byte: usize) -> bool {
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
        // Limit each fresh cursor's work. Long contextual sequences fall back
        // to the shared index instead of repeatedly rescanning their prefix.
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

    /// Number of Unicode scalar values, for GUI APIs that use character
    /// rather than grapheme offsets.
    pub fn scalar_count(&self) -> usize {
        if self.ascii {
            self.text.len()
        } else {
            self.scalar_boundaries().len() - 1
        }
    }

    /// Map a Unicode scalar index to a byte offset, including paragraph end.
    pub fn byte_from_scalar(&self, index: usize) -> Option<usize> {
        if self.ascii {
            (index <= self.text.len()).then_some(index)
        } else {
            self.scalar_boundaries().get(index).copied()
        }
    }

    /// Map a byte offset to a Unicode scalar index. Grapheme-internal scalar
    /// boundaries are accepted, while offsets inside UTF-8 encodings are not.
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

    /// Move backward to a Unicode word start, snapped to a grapheme boundary.
    /// Arbitrary byte offsets are accepted; no preceding word returns zero.
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

    /// Move forward to a Unicode word end, snapped to a grapheme boundary.
    /// Arbitrary byte offsets are accepted; no following word returns the end.
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

    fn cached_grapheme_boundaries(&self) -> Option<&[usize]> {
        // Local boundary queries need no allocated index. Reuse one already
        // built by grapheme counting, offset conversion, or navigation.
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

    fn word_ranges(&self) -> &[Range<usize>] {
        self.text_indices().words.get_or_init(|| {
            self.text
                .unicode_word_indices()
                .map(|(byte, word)| byte..byte + word.len())
                .collect()
        })
    }

    fn with_kind(&self, kind: ParagraphKind) -> Self {
        Self {
            text: Arc::clone(&self.text),
            spans: Arc::clone(&self.spans),
            kind,
            ascii: self.ascii,
            indices: self.indices.clone(),
        }
    }

    fn plain(text: &str) -> Self {
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

    fn styled(text: String, raw_spans: Vec<Span>, kind: ParagraphKind) -> Self {
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

    /// Construct validated rich-text data without accepting malformed run or
    /// paragraph boundaries. Adjacent runs with equal styles are merged.
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
        // Run endpoints are sorted, so validate them with one forward scan.
        // Importing nonuniform Unicode text must not eagerly allocate a full
        // grapheme index, which can be much larger than the serialized text.
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
        // Validate every original endpoint before merging adjacent styles so
        // an invalid grapheme split cannot disappear during normalization.
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

    fn style_for_byte(&self, byte: usize) -> InlineStyle {
        let index = self.spans.partition_point(|span| span.range.end <= byte);
        self.spans
            .get(index)
            .map_or_else(InlineStyle::default, |span| span.style)
    }
}

/// A document always contains at least one paragraph.
///
/// Paragraphs are shared with undo history. An edit rebuilds only its affected
/// paragraphs; all other paragraph allocations remain intact.
/// Equality compares rich content and the revision counter. To compare content
/// independently of revision, compare [`Fragment::from_document`] values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    paragraphs: Vec<Arc<Paragraph>>,
    revision: u64,
    identity: Arc<()>,
}

impl Default for Document {
    fn default() -> Self {
        Self::from_text("")
    }
}

impl Document {
    /// Create a document containing one empty body paragraph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Import plain text, normalizing CRLF and lone CR to paragraph breaks.
    pub fn from_text(text: &str) -> Self {
        let text = normalize_newlines(text);
        Self::from_paragraphs(
            text.split('\n')
                .map(|text| Arc::new(Paragraph::plain(text)))
                .collect(),
        )
    }

    /// Restore an immutable rich fragment, sharing its paragraph allocations.
    pub fn from_fragment(fragment: &Fragment) -> Self {
        Self::from_paragraphs(fragment.paragraphs().to_vec())
    }

    fn from_paragraphs(paragraphs: Vec<Arc<Paragraph>>) -> Self {
        Self {
            paragraphs,
            revision: 0,
            identity: Arc::new(()),
        }
    }

    /// Encode the complete document in Textloom's versioned native rich-text format.
    /// See [`Fragment::to_bytes`] for compatibility and decoder resource limits.
    pub fn to_bytes(&self) -> Vec<u8> {
        Fragment::encode_paragraphs(self.paragraphs())
    }

    /// Decode validated native rich text. See [`Fragment::from_bytes`] for resource limits.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, crate::FragmentError> {
        Fragment::from_bytes(bytes)
            .map(|fragment| Self::from_paragraphs(fragment.into_paragraphs()))
    }

    /// Export semantic HTML with escaped text and preserved formatting.
    /// NUL characters become U+FFFD because HTML cannot preserve literal NULs.
    pub fn to_html(&self) -> String {
        crate::export::html(self.paragraphs())
    }

    /// Capture a forward range with clipped inline runs and original paragraph kinds.
    /// Complete paragraphs share their original allocations.
    ///
    /// Returns [`Error::InvalidRange`] for reversed endpoints or
    /// [`Error::InvalidPosition`] for endpoints outside grapheme boundaries.
    pub fn fragment(&self, range: Range<Position>) -> Result<Fragment, Error> {
        self.validate_range(&range)?;
        let mut paragraphs = Vec::with_capacity(range.end.paragraph - range.start.paragraph + 1);
        for index in range.start.paragraph..=range.end.paragraph {
            let paragraph = &self.paragraphs[index];
            let start = if index == range.start.paragraph {
                range.start.byte
            } else {
                0
            };
            let end = if index == range.end.paragraph {
                range.end.byte
            } else {
                paragraph.text.len()
            };
            if start == 0 && end == paragraph.text.len() {
                paragraphs.push(Arc::clone(paragraph));
            } else {
                let mut text = String::with_capacity(end - start);
                let mut spans = Vec::new();
                append_slice(&mut text, &mut spans, paragraph, start..end);
                paragraphs.push(Arc::new(Paragraph::styled(text, spans, paragraph.kind)));
            }
        }
        Ok(Fragment::from_paragraphs(paragraphs))
    }

    /// Paragraphs in document order, always containing at least one element.
    /// Shared allocations may be retained by fragments or undo history.
    pub fn paragraphs(&self) -> &[Arc<Paragraph>] {
        &self.paragraphs
    }

    pub(crate) fn into_paragraphs(self) -> Vec<Arc<Paragraph>> {
        self.paragraphs
    }

    /// Look up a paragraph by zero-based index; out-of-range indices return `None`.
    pub fn paragraph(&self, index: usize) -> Option<&Paragraph> {
        self.paragraphs.get(index).map(Arc::as_ref)
    }

    /// The valid caret position immediately after the final paragraph's text.
    pub fn end(&self) -> Position {
        let paragraph = self.paragraphs.len() - 1;
        Position::new(paragraph, self.paragraphs[paragraph].text.len())
    }

    /// Increases on every actual edit, undo, or redo; it is never restored by undo.
    /// Starts at zero on import and saturates at `u64::MAX`.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// A stable token for this content state, shared by document clones and
    /// replaced only after actual changes. Renderers may compare its pointers
    /// without retaining or comparing all paragraphs.
    #[cfg(any(feature = "egui", feature = "accesskit"))]
    pub(crate) fn content_identity(&self) -> Arc<()> {
        Arc::clone(&self.identity)
    }

    /// Export text with LF between paragraphs. List markers are presentation.
    pub fn plain_text(&self) -> String {
        let capacity = self
            .paragraphs
            .iter()
            .map(|paragraph| paragraph.text.len())
            .sum::<usize>()
            .saturating_add(self.paragraphs.len() - 1);
        let mut result = String::with_capacity(capacity);
        for (index, paragraph) in self.paragraphs.iter().enumerate() {
            if index != 0 {
                result.push('\n');
            }
            result.push_str(paragraph.text());
        }
        result
    }

    /// Read a forward range; paragraph breaks are returned as LF.
    ///
    /// Returns [`Error::InvalidRange`] for reversed endpoints or
    /// [`Error::InvalidPosition`] for endpoints outside grapheme boundaries.
    pub fn text(&self, range: Range<Position>) -> Result<String, Error> {
        self.validate_range(&range)?;
        let mut result = String::new();
        for index in range.start.paragraph..=range.end.paragraph {
            if index != range.start.paragraph {
                result.push('\n');
            }
            let paragraph = &self.paragraphs[index];
            let start = if index == range.start.paragraph {
                range.start.byte
            } else {
                0
            };
            let end = if index == range.end.paragraph {
                range.end.byte
            } else {
                paragraph.text.len()
            };
            result.push_str(&paragraph.text[start..end]);
        }
        Ok(result)
    }

    /// Check that a position references an existing paragraph and a grapheme boundary.
    /// Paragraph start and end are valid even for empty paragraphs.
    /// Returns [`Error::InvalidPosition`] without changing the document on failure.
    pub fn validate_position(&self, position: Position) -> Result<(), Error> {
        let Some(paragraph) = self.paragraph(position.paragraph) else {
            return Err(Error::InvalidPosition(position));
        };
        if paragraph.is_grapheme_boundary(position.byte) {
            Ok(())
        } else {
            Err(Error::InvalidPosition(position))
        }
    }

    /// Return the preceding run's style at a boundary, or the first style at zero.
    /// Invalid positions and empty paragraphs return the default style.
    pub fn style_at(&self, position: Position) -> InlineStyle {
        let Some(paragraph) = self.paragraph(position.paragraph) else {
            return InlineStyle::default();
        };
        if !paragraph.is_grapheme_boundary(position.byte) {
            return InlineStyle::default();
        }
        paragraph.style_for_byte(position.byte.saturating_sub(1))
    }

    fn validate_range(&self, range: &Range<Position>) -> Result<(), Error> {
        if range.start > range.end {
            return Err(Error::InvalidRange);
        }
        self.validate_position(range.start)?;
        self.validate_position(range.end)
    }

    /// Replace text while preserving the formatting of both surviving edges.
    /// Heading continuations become body paragraphs; list continuations retain
    /// their kind. The returned caret is snapped forward if insertion joins a
    /// surrounding grapheme, such as a combining mark or emoji sequence.
    pub(crate) fn replace(
        &mut self,
        range: Range<Position>,
        text: &str,
        style: InlineStyle,
    ) -> Result<(Delta, Position), Error> {
        self.validate_range(&range)?;
        let text = normalize_newlines(text);
        let first = &self.paragraphs[range.start.paragraph];
        let last = &self.paragraphs[range.end.paragraph];
        let chunks: Vec<&str> = text.split('\n').collect();
        let mut after = Vec::with_capacity(chunks.len());
        let mut insertion_end = 0;
        for (index, chunk) in chunks.iter().enumerate() {
            let is_first = index == 0;
            let is_last = index + 1 == chunks.len();
            let capacity = chunk
                .len()
                .saturating_add(if is_first { range.start.byte } else { 0 })
                .saturating_add(if is_last {
                    last.text.len() - range.end.byte
                } else {
                    0
                });
            let mut output = String::with_capacity(capacity);
            let mut spans = Vec::new();
            if is_first {
                append_slice(&mut output, &mut spans, first, 0..range.start.byte);
            }
            let start = output.len();
            output.push_str(chunk);
            push_span(&mut spans, start..output.len(), style);
            if is_last {
                insertion_end = output.len();
                append_slice(
                    &mut output,
                    &mut spans,
                    last,
                    range.end.byte..last.text.len(),
                );
            }
            let mut kind = if is_first {
                // Deleting a complete leading paragraph preserves the suffix's
                // paragraph metadata when no replacement text takes its place.
                if is_last
                    && range.start.paragraph != range.end.paragraph
                    && range.start.byte == 0
                    && text.is_empty()
                {
                    last.kind
                } else {
                    first.kind
                }
            } else if is_last && range.start.paragraph != range.end.paragraph {
                last.kind
            } else {
                continuation_kind(first.kind, index)
            };
            if is_first
                && let (
                    ParagraphKind::Ordered {
                        indent: first_indent,
                        start,
                    },
                    ParagraphKind::Ordered { indent, .. },
                ) = (first.kind, kind)
                && first_indent == indent
            {
                // Removing a complete list item moves the surviving suffix
                // into its ordinal, rather than leaving a hole in the list.
                kind = ParagraphKind::Ordered { indent, start };
            }
            after.push(Arc::new(Paragraph::styled(output, spans, kind)));
        }
        let caret_paragraph = range.start.paragraph + after.len() - 1;
        let caret_source = after.last().expect("replacement has a paragraph");
        let caret = Position::new(
            caret_paragraph,
            caret_source.boundary_at_or_after(insertion_end),
        );
        let paragraph_range = range.start.paragraph..range.end.paragraph + 1;
        let (paragraph_range, after) =
            if range.start.paragraph != range.end.paragraph || chunks.len() > 1 {
                self.with_numbered_continuations(paragraph_range, after)
            } else {
                (paragraph_range, after)
            };
        let delta = self.change(paragraph_range, after);
        Ok((delta, caret))
    }

    pub(crate) fn replace_fragment(
        &mut self,
        range: Range<Position>,
        fragment: &Fragment,
    ) -> Result<(Delta, Position), Error> {
        self.validate_range(&range)?;
        let first = &self.paragraphs[range.start.paragraph];
        let last = &self.paragraphs[range.end.paragraph];
        let mut after = Vec::with_capacity(fragment.paragraphs().len());
        let mut insertion_end = 0;
        for (index, source) in fragment.paragraphs().iter().enumerate() {
            let is_first = index == 0;
            let is_last = index + 1 == fragment.paragraphs().len();
            let prefix_len = if is_first { range.start.byte } else { 0 };
            let suffix_len = if is_last {
                last.text.len() - range.end.byte
            } else {
                0
            };
            let kind = if is_first && prefix_len > 0 {
                first.kind
            } else {
                source.kind
            };
            if prefix_len == 0 && suffix_len == 0 && kind == source.kind {
                if is_last {
                    insertion_end = source.text.len();
                }
                after.push(Arc::clone(source));
                continue;
            }
            let mut text = String::with_capacity(prefix_len + source.text.len() + suffix_len);
            let mut spans = Vec::new();
            if is_first {
                append_slice(&mut text, &mut spans, first, 0..range.start.byte);
            }
            append_slice(&mut text, &mut spans, source, 0..source.text.len());
            if is_last {
                insertion_end = text.len();
                append_slice(&mut text, &mut spans, last, range.end.byte..last.text.len());
            }
            after.push(Arc::new(Paragraph::styled(text, spans, kind)));
        }
        let caret = Position::new(
            range.start.paragraph + after.len() - 1,
            after
                .last()
                .expect("fragment has a paragraph")
                .boundary_at_or_after(insertion_end),
        );
        // Rich paste preserves explicit source numbering, including list restarts.
        let delta = self.change(range.start.paragraph..range.end.paragraph + 1, after);
        Ok((delta, caret))
    }

    pub(crate) fn change_list_indent(&mut self, range: Range<usize>, increase: bool) -> Delta {
        let after = self.paragraphs[range.clone()]
            .iter()
            .map(|paragraph| {
                let kind = match paragraph.kind {
                    ParagraphKind::Bullet { indent } => {
                        if increase {
                            ParagraphKind::Bullet {
                                indent: indent.saturating_add(1),
                            }
                        } else if indent == 0 {
                            ParagraphKind::Body
                        } else {
                            ParagraphKind::Bullet { indent: indent - 1 }
                        }
                    }
                    ParagraphKind::Ordered { indent, start } => {
                        if increase {
                            ParagraphKind::Ordered {
                                indent: indent.saturating_add(1),
                                start,
                            }
                        } else if indent == 0 {
                            ParagraphKind::Body
                        } else {
                            ParagraphKind::Ordered {
                                indent: indent - 1,
                                start,
                            }
                        }
                    }
                    kind => kind,
                };
                if kind == paragraph.kind {
                    Arc::clone(paragraph)
                } else {
                    Arc::new(paragraph.with_kind(kind))
                }
            })
            .collect();
        self.change(range, after)
    }

    /// Build a batch replacement against the original text, normalizing joined
    /// graphemes only after every match is applied. This avoids shifting ranges
    /// and rebuilding the same paragraph once per match.
    /// Matches must be a nonempty `find` result for the current document:
    /// ordered, nonoverlapping ranges with valid grapheme-boundary endpoints.
    pub(crate) fn replace_matches(
        &mut self,
        matches: &[Range<Position>],
        replacement: &str,
    ) -> (Delta, Position) {
        let first = matches.first().expect("nonempty matches");
        let last = matches.last().expect("nonempty matches");
        debug_assert!(
            matches
                .iter()
                .all(|range| self.validate_range(range).is_ok() && !range.is_empty())
        );
        debug_assert!(matches.windows(2).all(|pair| pair[0].end <= pair[1].start));
        let replacement = normalize_newlines(replacement);
        let single_chunk = [replacement.as_ref()];
        let replacement_chunks: Cow<'_, [&str]> = if replacement.contains('\n') {
            Cow::Owned(replacement.split('\n').collect())
        } else {
            Cow::Borrowed(&single_chunk)
        };
        let multiline = replacement_chunks.len() > 1;
        let mut builder = ReplacementBuilder::new(self.paragraphs[first.start.paragraph].kind);
        let mut cursor = Position::new(first.start.paragraph, 0);
        let mut caret = Position::default();
        for range in matches {
            builder.append_document(self, cursor..range.start);
            let source = &self.paragraphs[range.start.paragraph];
            let style =
                source.style_for_byte(range.start.byte.min(source.text.len().saturating_sub(1)));
            if range.start.paragraph != range.end.paragraph
                && range.start.byte == 0
                && replacement.is_empty()
                && builder.is_empty()
            {
                builder.kind = self.paragraphs[range.end.paragraph].kind;
                if let (
                    ParagraphKind::Ordered {
                        indent: first_indent,
                        start,
                    },
                    ParagraphKind::Ordered { indent, .. },
                ) = (source.kind, builder.kind)
                    && first_indent == indent
                {
                    builder.kind = ParagraphKind::Ordered { indent, start };
                }
            }
            builder.append_replacement(&replacement_chunks, style);
            if range.start.paragraph != range.end.paragraph && multiline {
                builder.kind = self.paragraphs[range.end.paragraph].kind;
            }
            caret = Position::new(
                first.start.paragraph + builder.paragraphs.len(),
                builder.text_len(),
            );
            cursor = range.end;
        }
        builder.append_document(
            self,
            cursor
                ..Position::new(
                    last.end.paragraph,
                    self.paragraphs[last.end.paragraph].text.len(),
                ),
        );
        builder.finish_paragraph();
        let mut after = builder.paragraphs;
        caret.byte =
            after[caret.paragraph - first.start.paragraph].boundary_at_or_after(caret.byte);
        let range = first.start.paragraph..last.end.paragraph + 1;
        let structural = multiline
            || matches
                .iter()
                .any(|range| range.start.paragraph != range.end.paragraph);
        let range = if structural {
            let (range, numbered) = self.with_numbered_continuations(range, after);
            after = numbered;
            range
        } else {
            range
        };
        (self.change(range, after), caret)
    }

    pub(crate) fn apply_style(
        &mut self,
        range: Range<Position>,
        patch: StylePatch,
    ) -> Result<Option<Delta>, Error> {
        self.validate_range(&range)?;
        if range.is_empty() {
            return Ok(None);
        }
        let mut after = Vec::with_capacity(range.end.paragraph - range.start.paragraph + 1);
        for index in range.start.paragraph..=range.end.paragraph {
            let paragraph = &self.paragraphs[index];
            let start = if index == range.start.paragraph {
                range.start.byte
            } else {
                0
            };
            let end = if index == range.end.paragraph {
                range.end.byte
            } else {
                paragraph.text.len()
            };
            let mut spans = Vec::new();
            for span in paragraph.spans.iter() {
                let overlap = span.range.start.max(start)..span.range.end.min(end);
                if overlap.start >= overlap.end {
                    push_span(&mut spans, span.range.clone(), span.style);
                    continue;
                }
                push_span(&mut spans, span.range.start..overlap.start, span.style);
                let mut style = span.style;
                patch.apply(&mut style);
                push_span(&mut spans, overlap.clone(), style);
                push_span(&mut spans, overlap.end..span.range.end, span.style);
            }
            if spans.as_slice() == paragraph.spans() {
                after.push(Arc::clone(paragraph));
            } else {
                after.push(Arc::new(Paragraph {
                    text: Arc::clone(&paragraph.text),
                    spans: spans.into(),
                    kind: paragraph.kind,
                    ascii: paragraph.ascii,
                    indices: paragraph.indices.clone(),
                }));
            }
        }
        let delta = self.change(range.start.paragraph..range.end.paragraph + 1, after);
        Ok((!delta.is_empty()).then_some(delta))
    }

    pub(crate) fn set_kind(
        &mut self,
        range: Range<usize>,
        kind: ParagraphKind,
    ) -> Result<Option<Delta>, Error> {
        if range.start > range.end || range.end > self.paragraphs.len() {
            return Err(Error::InvalidRange);
        }
        if let ParagraphKind::Heading { level } = kind
            && !(1..=6).contains(&level)
        {
            return Err(Error::InvalidHeadingLevel(level));
        }
        let after = self.paragraphs[range.clone()]
            .iter()
            .enumerate()
            .map(|(offset, paragraph)| {
                let kind = numbered_kind(kind, offset);
                if paragraph.kind == kind {
                    Arc::clone(paragraph)
                } else {
                    Arc::new(paragraph.with_kind(kind))
                }
            })
            .collect();
        let (range, after) = self.with_numbered_continuations(range, after);
        let delta = self.change(range, after);
        Ok((!delta.is_empty()).then_some(delta))
    }

    pub(crate) fn replay(&mut self, delta: &Delta, forward: bool) {
        if delta.is_empty() {
            return;
        }
        let (remove, insert) = if forward {
            (&delta.before, &delta.after)
        } else {
            (&delta.after, &delta.before)
        };
        debug_assert_eq!(
            self.paragraphs.get(delta.start..delta.start + remove.len()),
            Some(remove.as_slice()),
            "history must be replayed against its corresponding document state"
        );
        self.paragraphs.splice(
            delta.start..delta.start + remove.len(),
            insert.iter().cloned(),
        );
        self.revision = self.revision.saturating_add(1);
        self.identity = Arc::new(());
    }

    fn with_numbered_continuations(
        &self,
        mut range: Range<usize>,
        mut after: Vec<Arc<Paragraph>>,
    ) -> (Range<usize>, Vec<Arc<Paragraph>>) {
        for index in 1..after.len() {
            if let Some(kind) = next_numbered_kind(after[index - 1].kind, after[index].kind)
                && after[index].kind != kind
            {
                after[index] = Arc::new(after[index].with_kind(kind));
            }
        }
        let Some(last) = after.last() else {
            return (range, after);
        };
        let mut previous = last.kind;
        for paragraph in &self.paragraphs[range.end..] {
            let Some(kind) = next_numbered_kind(previous, paragraph.kind) else {
                break;
            };
            after.push(if paragraph.kind == kind {
                Arc::clone(paragraph)
            } else {
                Arc::new(paragraph.with_kind(kind))
            });
            range.end += 1;
            previous = kind;
        }
        (range, after)
    }

    fn change(&mut self, range: Range<usize>, mut after: Vec<Arc<Paragraph>>) -> Delta {
        let mut start = range.start;
        let mut end = range.end;
        // Preserve existing allocations at unchanged edges, and keep them out of
        // the undo delta even when a caller supplied a wide range.
        let mut prefix = 0;
        while start < end
            && prefix < after.len()
            && self.paragraphs[start].as_ref() == after[prefix].as_ref()
        {
            start += 1;
            prefix += 1;
        }
        after.drain(..prefix);
        while start < end
            && after
                .last()
                .is_some_and(|paragraph| self.paragraphs[end - 1].as_ref() == paragraph.as_ref())
        {
            end -= 1;
            after.pop();
        }
        let before = self.paragraphs[start..end].to_vec();
        let delta = Delta {
            start,
            before,
            after,
        };
        if !delta.is_empty() {
            self.paragraphs
                .splice(start..end, delta.after.iter().cloned());
            self.revision = self.revision.saturating_add(1);
            self.identity = Arc::new(());
        }
        delta
    }
}

struct ReplacementBuilder {
    paragraphs: Vec<Arc<Paragraph>>,
    text: String,
    spans: Vec<Span>,
    kind: ParagraphKind,
    // A complete source paragraph stays shared until more text is joined to it.
    // While a candidate is present, the owned text and span buffers are empty.
    candidate: Option<Arc<Paragraph>>,
}

impl ReplacementBuilder {
    fn new(kind: ParagraphKind) -> Self {
        Self {
            paragraphs: Vec::new(),
            text: String::new(),
            spans: Vec::new(),
            kind,
            candidate: None,
        }
    }

    fn text_len(&self) -> usize {
        self.candidate
            .as_ref()
            .map_or(self.text.len(), |paragraph| paragraph.text.len())
    }

    fn is_empty(&self) -> bool {
        self.text_len() == 0
    }

    fn materialize_candidate(&mut self) {
        if let Some(paragraph) = self.candidate.take() {
            append_slice(
                &mut self.text,
                &mut self.spans,
                &paragraph,
                0..paragraph.text.len(),
            );
        }
    }

    fn finish_paragraph(&mut self) {
        if let Some(paragraph) = self.candidate.take() {
            self.paragraphs.push(if paragraph.kind == self.kind {
                paragraph
            } else {
                Arc::new(paragraph.with_kind(self.kind))
            });
        } else {
            self.paragraphs.push(Arc::new(Paragraph::styled(
                std::mem::take(&mut self.text),
                std::mem::take(&mut self.spans),
                self.kind,
            )));
        }
    }

    fn append_document(&mut self, document: &Document, range: Range<Position>) {
        for index in range.start.paragraph..=range.end.paragraph {
            let paragraph = &document.paragraphs[index];
            if index > range.start.paragraph {
                self.finish_paragraph();
                self.kind = paragraph.kind;
            }
            let start = if index == range.start.paragraph {
                range.start.byte
            } else {
                0
            };
            let end = if index == range.end.paragraph {
                range.end.byte
            } else {
                paragraph.text.len()
            };
            if self.is_empty() && start == 0 && end == paragraph.text.len() {
                self.candidate = Some(Arc::clone(paragraph));
            } else if start < end {
                self.materialize_candidate();
                append_slice(&mut self.text, &mut self.spans, paragraph, start..end);
            }
        }
    }

    fn append_replacement(&mut self, chunks: &[&str], style: InlineStyle) {
        for (index, chunk) in chunks.iter().enumerate() {
            if index > 0 {
                self.finish_paragraph();
                self.kind = continuation_kind(self.kind, 1);
            }
            if !chunk.is_empty() {
                self.materialize_candidate();
                let start = self.text.len();
                self.text.push_str(chunk);
                push_span(&mut self.spans, start..self.text.len(), style);
            }
        }
    }
}

/// History stores only the contiguous paragraphs changed by an operation.
#[derive(Clone, Debug)]
pub(crate) struct Delta {
    pub start: usize,
    pub before: Vec<Arc<Paragraph>>,
    pub after: Vec<Arc<Paragraph>>,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.before.is_empty() && self.after.is_empty()
    }

    /// A conservative estimate; shared text may be counted more than once.
    pub fn retained_bytes(&self) -> usize {
        self.before
            .iter()
            .chain(&self.after)
            .map(|paragraph| {
                std::mem::size_of::<Paragraph>()
                    .saturating_add(paragraph.text.len())
                    .saturating_add(std::mem::size_of_val(paragraph.spans()))
            })
            .fold(0, usize::saturating_add)
    }
}

pub(crate) fn normalize_newlines(text: &str) -> Cow<'_, str> {
    let mut breaks = text.match_indices('\r').map(|(index, _)| index);
    let Some(first) = breaks.next() else {
        return Cow::Borrowed(text);
    };
    let mut normalized = String::with_capacity(text.len());
    let mut start = 0;
    for index in std::iter::once(first).chain(breaks) {
        normalized.push_str(&text[start..index]);
        normalized.push('\n');
        start = index + 1 + usize::from(text.as_bytes().get(index + 1) == Some(&b'\n'));
    }
    normalized.push_str(&text[start..]);
    Cow::Owned(normalized)
}

fn continuation_kind(kind: ParagraphKind, offset: usize) -> ParagraphKind {
    match kind {
        ParagraphKind::Heading { .. } => ParagraphKind::Body,
        other => numbered_kind(other, offset),
    }
}

fn numbered_kind(kind: ParagraphKind, offset: usize) -> ParagraphKind {
    match kind {
        ParagraphKind::Ordered { indent, start } => ParagraphKind::Ordered {
            indent,
            start: start.saturating_add(u32::try_from(offset).unwrap_or(u32::MAX)),
        },
        other => other,
    }
}

fn next_numbered_kind(previous: ParagraphKind, current: ParagraphKind) -> Option<ParagraphKind> {
    if let (
        ParagraphKind::Ordered {
            indent: previous_indent,
            start,
        },
        ParagraphKind::Ordered { indent, .. },
    ) = (previous, current)
        && previous_indent == indent
    {
        Some(ParagraphKind::Ordered {
            indent,
            start: start.saturating_add(1),
        })
    } else {
        None
    }
}

fn append_slice(
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

fn push_span(spans: &mut Vec<Span>, range: Range<usize>, style: InlineStyle) {
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
    // Paragraphs contain no CR/LF, so every ASCII byte is a whole grapheme.
    // The common case can retain the owned runs without copying or segmentation.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newline_normalization_handles_all_adjacent_breaks_and_keeps_lf_borrowed() {
        let tokens = ["a", "é", "\r", "\n"];
        for length in 0..=5 {
            for mut case in 0..4usize.pow(length) {
                let mut text = String::new();
                for _ in 0..length {
                    text.push_str(tokens[case % tokens.len()]);
                    case /= tokens.len();
                }
                let expected = text.replace("\r\n", "\n").replace('\r', "\n");
                let normalized = normalize_newlines(&text);
                assert_eq!(normalized, expected, "input {text:?}");
                assert_eq!(matches!(normalized, Cow::Borrowed(_)), !text.contains('\r'));
            }
        }
    }

    #[test]
    fn indexed_unicode_offsets_match_extended_graphemes_and_scalars() {
        for text in [
            "",
            "plain ASCII",
            "café e\u{301}",
            "👨‍👩‍👧‍👦🇺🇸🇨🇦🇯🇵",
            "क्‍ष नमस्ते",
            "a\u{301}\u{302}👩🏽‍💻z",
        ] {
            let paragraph = Paragraph::plain(text);
            let graphemes: Vec<_> = text
                .grapheme_indices(true)
                .map(|(byte, _)| byte)
                .chain([text.len()])
                .collect();
            let scalars: Vec<_> = text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain([text.len()])
                .collect();
            assert_eq!(paragraph.grapheme_count(), graphemes.len() - 1);
            assert_eq!(paragraph.scalar_count(), scalars.len() - 1);
            for (index, byte) in graphemes.iter().enumerate() {
                assert_eq!(paragraph.byte_from_grapheme(index), Some(*byte));
                assert_eq!(paragraph.grapheme_index(*byte), Some(index));
            }
            for (index, byte) in scalars.iter().enumerate() {
                assert_eq!(paragraph.byte_from_scalar(index), Some(*byte));
                assert_eq!(paragraph.scalar_index(*byte), Some(index));
            }
            assert_eq!(paragraph.byte_from_grapheme(graphemes.len()), None);
            assert_eq!(paragraph.byte_from_scalar(scalars.len()), None);
            for byte in 0..=text.len() + 2 {
                assert_eq!(
                    paragraph.grapheme_index(byte),
                    graphemes.iter().position(|offset| *offset == byte)
                );
                assert_eq!(
                    paragraph.scalar_index(byte),
                    scalars.iter().position(|offset| *offset == byte)
                );
                assert_eq!(
                    paragraph.previous_grapheme(byte),
                    graphemes
                        .iter()
                        .copied()
                        .rev()
                        .find(|offset| *offset < byte)
                        .unwrap_or(0)
                );
                assert_eq!(
                    paragraph.next_grapheme(byte),
                    graphemes
                        .iter()
                        .copied()
                        .find(|offset| *offset > byte)
                        .unwrap_or(text.len())
                );
                assert_eq!(
                    paragraph.boundary_at_or_before(byte),
                    graphemes
                        .iter()
                        .copied()
                        .rev()
                        .find(|offset| *offset <= byte)
                        .unwrap_or(0)
                );
                assert_eq!(
                    paragraph.boundary_at_or_after(byte),
                    graphemes
                        .iter()
                        .copied()
                        .find(|offset| *offset >= byte)
                        .unwrap_or(text.len())
                );
            }
        }
    }

    #[test]
    fn local_grapheme_checks_and_snapping_match_complete_segmentation() {
        let mut texts: Vec<String> = [
            "",
            "plain ASCII",
            "é",
            "café e\u{301}",
            "\u{600}e\u{301}z",
            "👨‍👩‍👧‍👦🇺🇸🇨🇦🇯🇵",
            "क्‍ष नमस्ते",
            "각나",
            "a\t\u{301}\u{0000}👩🏽‍💻z",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        texts.push(format!("a👩{}‍💻z", "\u{301}".repeat(64)));
        texts.push(format!("a{}z", "🇦".repeat(65)));
        texts.push(format!("aक{}षz", "\u{94d}\u{301}".repeat(33)));

        for text in texts {
            let boundaries: Vec<_> = text
                .grapheme_indices(true)
                .map(|(byte, _)| byte)
                .chain([text.len()])
                .collect();
            let check = |paragraph: &Paragraph, byte: usize| {
                assert_eq!(
                    paragraph.is_grapheme_boundary(byte),
                    boundaries.contains(&byte),
                    "boundary at {byte} in {text:?}"
                );
                assert_eq!(
                    paragraph.boundary_at_or_before(byte),
                    boundaries
                        .iter()
                        .rev()
                        .copied()
                        .find(|offset| *offset <= byte)
                        .unwrap_or(0),
                    "backward snap at {byte} in {text:?}"
                );
                assert_eq!(
                    paragraph.boundary_at_or_after(byte),
                    boundaries
                        .iter()
                        .copied()
                        .find(|offset| *offset >= byte)
                        .unwrap_or(text.len()),
                    "forward snap at {byte} in {text:?}"
                );
            };
            for byte in (0..=text.len() + 2).chain([usize::MAX]) {
                check(&Paragraph::plain(&text), byte);
            }
            let paragraph = Paragraph::plain(&text);
            paragraph.grapheme_count();
            let cached = paragraph.cached_grapheme_boundaries().map(<[_]>::as_ptr);
            for byte in (0..=text.len() + 2).chain([usize::MAX]) {
                check(&paragraph, byte);
            }
            assert_eq!(
                paragraph.cached_grapheme_boundaries().map(<[_]>::as_ptr),
                cached
            );
        }
    }

    #[test]
    fn position_validation_and_style_lookup_leave_grapheme_indexes_lazy() {
        let text = "café e\u{301} 👩🏽‍💻 🇦🇧🇨 क्‍ष ".repeat(16);
        let mut document = Document::from_text(&text);
        document
            .apply_style(
                Position::default()..document.end(),
                StylePatch {
                    bold: Some(true),
                    ..StylePatch::default()
                },
            )
            .unwrap();
        let boundaries: Vec<_> = text
            .grapheme_indices(true)
            .map(|(byte, _)| byte)
            .chain([text.len()])
            .collect();
        for byte in (0..=text.len() + 1).chain([usize::MAX]) {
            let position = Position::new(0, byte);
            let valid = boundaries.contains(&byte);
            assert_eq!(document.validate_position(position).is_ok(), valid);
            assert_eq!(document.style_at(position).bold, valid);
        }
        assert!(
            document
                .paragraph(0)
                .unwrap()
                .cached_grapheme_boundaries()
                .is_none()
        );
    }

    #[test]
    fn long_grapheme_contexts_initialize_and_reuse_one_index() {
        for (text, byte) in [
            (format!("a{}z", "🇦".repeat(65)), 1 + 4 * 33),
            (
                format!("a👩{}‍💻z", "\u{301}".repeat(64)),
                "a👩‍".len() + "\u{301}".len() * 64,
            ),
            (
                format!("aक{}षz", "\u{94d}\u{301}".repeat(33)),
                "aक".len() + "\u{94d}\u{301}".len() * 33,
            ),
        ] {
            let paragraph = Paragraph::plain(&text);
            let boundaries: Vec<_> = text
                .grapheme_indices(true)
                .map(|(byte, _)| byte)
                .chain([text.len()])
                .collect();
            assert_eq!(
                paragraph.is_grapheme_boundary(byte),
                boundaries.contains(&byte)
            );
            let cached = paragraph.cached_grapheme_boundaries().unwrap().as_ptr();
            for byte in 0..=text.len() {
                assert_eq!(
                    paragraph.is_grapheme_boundary(byte),
                    boundaries.contains(&byte)
                );
                assert_eq!(
                    paragraph.cached_grapheme_boundaries().unwrap().as_ptr(),
                    cached
                );
            }
        }

        let text = format!("a{}z", "\u{301}".repeat(64));
        let before = Paragraph::plain(&text);
        assert_eq!(before.boundary_at_or_before(text.len() - 3), 0);
        assert!(before.cached_grapheme_boundaries().is_some());
        let after = Paragraph::plain(&text);
        assert_eq!(after.boundary_at_or_after(1), text.len() - 1);
        assert!(after.cached_grapheme_boundaries().is_some());
    }

    #[test]
    fn all_match_search_in_a_long_regional_run_reuses_grapheme_index() {
        let document = Document::from_text(&format!("a{}z", "🇦".repeat(257)));
        let expected: Vec<_> = (0..128)
            .map(|index| Position::new(0, 1 + index * 8)..Position::new(0, 1 + (index + 1) * 8))
            .collect();
        assert_eq!(
            document.find("🇦🇦", crate::SearchOptions::default()),
            expected
        );
        let paragraph = document.paragraph(0).unwrap();
        let cached = paragraph.cached_grapheme_boundaries().unwrap().as_ptr();
        assert_eq!(
            document.find("🇦🇦", crate::SearchOptions::default()),
            expected
        );
        assert_eq!(
            paragraph.cached_grapheme_boundaries().unwrap().as_ptr(),
            cached
        );
    }

    #[test]
    fn unicode_editing_snaps_carets_and_indexes_only_long_contexts() {
        for (text, byte, insertion, expected_byte, indexed) in [
            (
                "café 👩💻 e\u{301}".to_owned(),
                "café 👩".len(),
                "‍",
                "café 👩‍💻".len(),
                false,
            ),
            ("a🇦🇧🇨🇩z".to_owned(), "a🇦🇧".len(), "🇪", "a🇦🇧🇪🇨".len(), false),
            (
                format!("a👩{}💻z", "\u{301}".repeat(64)),
                "a👩".len() + "\u{301}".len() * 64,
                "‍",
                "a👩‍💻".len() + "\u{301}".len() * 64,
                true,
            ),
        ] {
            let mut editor = crate::Editor::from_text(&text);
            editor
                .set_selection(crate::Selection::caret(Position::new(0, byte)))
                .unwrap();
            editor.insert_text(insertion).unwrap();
            let paragraph = editor.document().paragraph(0).unwrap();
            assert_eq!(editor.selection().focus, Position::new(0, expected_byte));
            assert_eq!(paragraph.cached_grapheme_boundaries().is_some(), indexed);
            assert!(
                editor
                    .document()
                    .validate_position(editor.selection().focus)
                    .is_ok()
            );
            assert_eq!(paragraph.cached_grapheme_boundaries().is_some(), indexed);
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), text);
            assert!(editor.redo());
            assert_eq!(editor.selection().focus, Position::new(0, expected_byte));
        }
    }

    #[test]
    fn word_navigation_matches_unicode_segmentation() {
        for text in [
            "",
            "one, two's!",
            "can't 123,456 12.34 under_score a:b; c.d 'word'",
            "\t  one\u{000b}two\u{000c}three \u{0000}four",
            "café 👩‍💻 नमस्ते 🇺🇸",
            "你好世界 e\u{301}",
            "!!!",
        ] {
            let paragraph = Paragraph::plain(text);
            for byte in 0..=text.len() + 1 {
                let previous = text
                    .unicode_word_indices()
                    .rev()
                    .find(|(start, _)| *start < byte)
                    .map_or(0, |(start, _)| start);
                let next = text
                    .unicode_word_indices()
                    .find(|(start, word)| start + word.len() > byte)
                    .map_or(text.len(), |(start, word)| start + word.len());
                assert_eq!(
                    paragraph.previous_word(byte),
                    paragraph.boundary_at_or_before(previous)
                );
                assert_eq!(
                    paragraph.next_word(byte),
                    paragraph.boundary_at_or_after(next)
                );
            }
        }
    }

    #[test]
    fn unicode_indexes_are_independently_lazy_and_shared_across_metadata_changes() {
        let mut document = Document::from_text("café 👩‍💻");
        let original = Arc::clone(&document.paragraphs()[0]);
        let indices = original.indices.get().unwrap();
        assert!(indices.graphemes.get().is_none());
        assert!(indices.scalars.get().is_none());
        assert!(indices.words.get().is_none());
        let equal = Paragraph::plain(original.text());
        assert_eq!(*original, equal);
        original.grapheme_count();
        assert_eq!(*original, equal);
        assert!(indices.graphemes.get().is_some());
        assert!(indices.scalars.get().is_none());
        assert!(indices.words.get().is_none());
        document
            .set_kind(0..1, ParagraphKind::Heading { level: 2 })
            .unwrap();
        assert!(Arc::ptr_eq(
            indices,
            document.paragraph(0).unwrap().indices.get().unwrap()
        ));
        document
            .apply_style(
                Position::default()..document.end(),
                StylePatch {
                    bold: Some(true),
                    ..StylePatch::default()
                },
            )
            .unwrap();
        assert!(Arc::ptr_eq(
            indices,
            document.paragraph(0).unwrap().indices.get().unwrap()
        ));
        document
            .replace(document.end()..document.end(), "!", InlineStyle::default())
            .unwrap();
        assert!(!Arc::ptr_eq(
            indices,
            document.paragraph(0).unwrap().indices.get().unwrap()
        ));
        assert!(Paragraph::plain("ASCII").indices.get().is_none());
    }

    #[test]
    fn ascii_word_indexes_are_lazy_shared_and_invalidated_by_text_edits() {
        let mut document = Document::from_text("one two's 12.34");
        let original = Arc::clone(&document.paragraphs()[0]);
        assert_eq!(original.grapheme_count(), original.text().len());
        assert_eq!(original.scalar_count(), original.text().len());
        assert_eq!(original.next_grapheme(3), 4);
        assert_eq!(original.previous_grapheme(3), 2);
        assert!(original.indices.get().is_none());

        assert_eq!(original.next_word(3), 9);
        assert_eq!(original.previous_word(14), 10);
        let indices = original.indices.get().unwrap();
        let words = indices.words.get().unwrap().as_ptr();
        assert!(indices.graphemes.get().is_none());
        assert!(indices.scalars.get().is_none());

        document
            .set_kind(0..1, ParagraphKind::Heading { level: 2 })
            .unwrap();
        document
            .apply_style(
                Position::default()..document.end(),
                StylePatch {
                    bold: Some(true),
                    ..StylePatch::default()
                },
            )
            .unwrap();
        let formatted = document.paragraph(0).unwrap();
        assert!(Arc::ptr_eq(indices, formatted.indices.get().unwrap()));
        assert_eq!(formatted.next_word(3), 9);
        assert_eq!(formatted.word_ranges().as_ptr(), words);

        let (delta, _) = document
            .replace(
                document.end()..document.end(),
                " six",
                InlineStyle::default(),
            )
            .unwrap();
        let edited = document.paragraph(0).unwrap();
        assert!(edited.indices.get().is_none());
        assert_eq!(edited.next_word(15), 19);
        assert!(!Arc::ptr_eq(indices, edited.indices.get().unwrap()));
        document.replay(&delta, false);
        assert!(Arc::ptr_eq(
            indices,
            document.paragraph(0).unwrap().indices.get().unwrap()
        ));
    }

    #[test]
    fn uniform_unicode_end_typing_does_not_build_text_indexes() {
        let mut document = Document::from_text("café 👩‍💻");
        for _ in 0..3 {
            document
                .replace(document.end()..document.end(), "é", InlineStyle::default())
                .unwrap();
            let paragraph = document.paragraph(0).unwrap();
            assert_eq!(paragraph.spans().len(), 1);
            let indices = paragraph.indices.get().unwrap();
            assert!(indices.graphemes.get().is_none());
            assert!(indices.scalars.get().is_none());
            assert!(indices.words.get().is_none());
        }
        assert_normalized(&document);
    }

    #[test]
    fn rich_parts_validate_and_merge_runs() {
        let paragraph = Paragraph::from_parts(
            "aéébc",
            vec![
                Span {
                    range: 0..1,
                    style: bold(),
                },
                Span {
                    range: 1..3,
                    style: bold(),
                },
                Span {
                    range: 3..5,
                    style: bold(),
                },
                Span {
                    range: 5..6,
                    style: InlineStyle::default(),
                },
                Span {
                    range: 6..7,
                    style: InlineStyle::default(),
                },
            ],
            ParagraphKind::Heading { level: 1 },
        )
        .unwrap();
        assert_eq!(
            paragraph.spans(),
            &[
                Span {
                    range: 0..5,
                    style: bold()
                },
                Span {
                    range: 5..7,
                    style: InlineStyle::default()
                }
            ]
        );
        assert!(Paragraph::from_parts("", vec![], ParagraphKind::Body).is_ok());
        for (text, spans, kind) in [
            (
                "a\nb",
                vec![Span {
                    range: 0..3,
                    style: bold(),
                }],
                ParagraphKind::Body,
            ),
            (
                "a\rb",
                vec![Span {
                    range: 0..3,
                    style: bold(),
                }],
                ParagraphKind::Body,
            ),
            (
                "a",
                vec![Span {
                    range: 0..1,
                    style: bold(),
                }],
                ParagraphKind::Heading { level: 0 },
            ),
            ("a", vec![], ParagraphKind::Body),
            (
                "",
                vec![Span {
                    range: 0..0,
                    style: bold(),
                }],
                ParagraphKind::Body,
            ),
            (
                "ab",
                vec![Span {
                    range: 0..1,
                    style: bold(),
                }],
                ParagraphKind::Body,
            ),
            (
                "ab",
                vec![Span {
                    range: 1..2,
                    style: bold(),
                }],
                ParagraphKind::Body,
            ),
            (
                "ab",
                vec![Span {
                    range: 0..3,
                    style: bold(),
                }],
                ParagraphKind::Body,
            ),
            (
                "e\u{301}",
                vec![
                    Span {
                        range: 0..1,
                        style: bold(),
                    },
                    Span {
                        range: 1..3,
                        style: bold(),
                    },
                ],
                ParagraphKind::Body,
            ),
            (
                "é",
                vec![
                    Span {
                        range: 0..1,
                        style: bold(),
                    },
                    Span {
                        range: 1..2,
                        style: bold(),
                    },
                ],
                ParagraphKind::Body,
            ),
        ] {
            assert_eq!(
                Paragraph::from_parts(text, spans, kind),
                Err(Error::InvalidFragment)
            );
        }
    }

    #[test]
    fn rich_unicode_run_validation_keeps_navigation_indexes_lazy() {
        let paragraph = Paragraph::from_parts(
            "ééé🦀",
            vec![
                Span {
                    range: 0..2,
                    style: bold(),
                },
                Span {
                    range: 2..6,
                    style: InlineStyle::default(),
                },
                Span {
                    range: 6..10,
                    style: bold(),
                },
            ],
            ParagraphKind::Body,
        )
        .unwrap();
        let indices = paragraph.indices.get().unwrap();
        assert!(indices.graphemes.get().is_none());
        assert!(indices.scalars.get().is_none());
        assert!(indices.words.get().is_none());
        assert_eq!(paragraph.spans().len(), 3);
        assert_eq!(paragraph.grapheme_count(), 4);
    }

    #[test]
    fn document_content_identity_tracks_changes_and_preserves_noops() {
        let mut document = Document::from_text("text");
        let cloned = document.clone();
        assert!(Arc::ptr_eq(&document.identity, &cloned.identity));
        let (noop, _) = document
            .replace(
                Position::default()..document.end(),
                "text",
                InlineStyle::default(),
            )
            .unwrap();
        assert!(noop.is_empty());
        assert!(Arc::ptr_eq(&document.identity, &cloned.identity));
        let (delta, _) = document
            .replace(document.end()..document.end(), "!", InlineStyle::default())
            .unwrap();
        assert!(!Arc::ptr_eq(&document.identity, &cloned.identity));
        let changed = Arc::clone(&document.identity);
        document.replay(&delta, false);
        assert!(!Arc::ptr_eq(&document.identity, &changed));
        assert!(!Arc::ptr_eq(&document.identity, &cloned.identity));
        assert_eq!(document.paragraphs(), cloned.paragraphs());
        let undone = Arc::clone(&document.identity);
        document.replay(&delta, true);
        assert!(!Arc::ptr_eq(&document.identity, &undone));

        let before_kind = Arc::clone(&document.identity);
        let kind = ParagraphKind::Heading { level: 2 };
        assert!(document.set_kind(0..1, kind).unwrap().is_some());
        assert!(!Arc::ptr_eq(&document.identity, &before_kind));
        let before_style = Arc::clone(&document.identity);
        assert!(document.set_kind(0..1, kind).unwrap().is_none());
        assert!(Arc::ptr_eq(&document.identity, &before_style));
        let patch = StylePatch {
            bold: Some(true),
            ..StylePatch::default()
        };
        assert!(
            document
                .apply_style(Position::default()..document.end(), patch)
                .unwrap()
                .is_some()
        );
        assert!(!Arc::ptr_eq(&document.identity, &before_style));
        let styled = Arc::clone(&document.identity);
        assert!(
            document
                .apply_style(Position::default()..document.end(), patch)
                .unwrap()
                .is_none()
        );
        assert!(Arc::ptr_eq(&document.identity, &styled));
        document.replay(&noop, true);
        assert!(Arc::ptr_eq(&document.identity, &styled));
    }

    fn bold() -> InlineStyle {
        InlineStyle {
            bold: true,
            ..InlineStyle::default()
        }
    }

    fn assert_normalized(document: &Document) {
        assert!(!document.paragraphs.is_empty());
        for paragraph in document.paragraphs() {
            let mut previous_end = 0;
            let mut previous_style = None;
            for span in paragraph.spans() {
                assert_eq!(span.range.start, previous_end);
                assert!(span.range.start < span.range.end);
                assert!(paragraph.text().is_char_boundary(span.range.start));
                assert!(paragraph.text().is_char_boundary(span.range.end));
                assert_ne!(Some(span.style), previous_style);
                let boundaries: Vec<_> = paragraph
                    .text()
                    .grapheme_indices(true)
                    .map(|(index, _)| index)
                    .chain([paragraph.text().len()])
                    .collect();
                assert!(boundaries.contains(&span.range.start));
                assert!(boundaries.contains(&span.range.end));
                previous_end = span.range.end;
                previous_style = Some(span.style);
            }
            assert_eq!(previous_end, paragraph.text().len());
        }
    }

    #[test]
    fn plain_text_normalizes_newlines_and_preserves_empty_paragraphs() {
        let document = Document::from_text("a\r\nb\rc\n");
        assert_eq!(document.plain_text(), "a\nb\nc\n");
        assert_eq!(document.paragraphs().len(), 4);
        assert_eq!(document.end(), Position::new(3, 0));
        assert_eq!(
            document
                .text(Position::new(0, 1)..Position::new(2, 1))
                .unwrap(),
            "\nb\nc"
        );
        assert_normalized(&document);
        assert_eq!(Document::new().paragraphs().len(), 1);
    }

    #[test]
    fn positions_require_extended_grapheme_boundaries() {
        let document = Document::from_text("e\u{301}👨‍👩‍👧‍👦🇺🇸");
        for (byte, _) in document.paragraph(0).unwrap().text().grapheme_indices(true) {
            assert!(document.validate_position(Position::new(0, byte)).is_ok());
        }
        assert!(document.validate_position(document.end()).is_ok());
        assert!(document.validate_position(Position::new(0, 1)).is_err());
        assert!(document.validate_position(Position::new(0, 2)).is_err());
        assert!(document.validate_position(Position::new(0, 7)).is_err());
        assert!(document.validate_position(Position::new(1, 0)).is_err());
    }

    #[test]
    fn replacing_preserves_styles_on_surviving_edges() {
        let mut document = Document::from_text("before\nafter");
        document
            .apply_style(
                Position::new(0, 0)..Position::new(0, 6),
                StylePatch {
                    bold: Some(true),
                    ..StylePatch::default()
                },
            )
            .unwrap();
        document
            .apply_style(
                Position::new(1, 0)..Position::new(1, 5),
                StylePatch {
                    italic: Some(true),
                    ..StylePatch::default()
                },
            )
            .unwrap();
        let (delta, caret) = document
            .replace(
                Position::new(0, 3)..Position::new(1, 2),
                "X",
                InlineStyle::default(),
            )
            .unwrap();
        assert_eq!(document.plain_text(), "befXter");
        assert_eq!(caret, Position::new(0, 4));
        assert_eq!(document.paragraph(0).unwrap().spans().len(), 3);
        assert!(document.style_at(Position::new(0, 3)).bold);
        assert!(document.style_at(Position::new(0, 5)).italic);
        document.replay(&delta, false);
        assert_eq!(document.plain_text(), "before\nafter");
        document.replay(&delta, true);
        assert_eq!(document.plain_text(), "befXter");
        assert_normalized(&document);
    }

    #[test]
    fn formatting_merges_runs_and_is_a_noop_when_unchanged() {
        let mut document = Document::from_text("abcdef");
        let patch = StylePatch {
            bold: Some(true),
            ..StylePatch::default()
        };
        document
            .apply_style(Position::new(0, 2)..Position::new(0, 4), patch)
            .unwrap();
        assert_eq!(document.paragraph(0).unwrap().spans().len(), 3);
        let revision = document.revision();
        assert!(
            document
                .apply_style(Position::new(0, 2)..Position::new(0, 4), patch)
                .unwrap()
                .is_none()
        );
        assert_eq!(document.revision(), revision);
        document
            .apply_style(Position::new(0, 0)..Position::new(0, 6), patch)
            .unwrap();
        assert_eq!(
            document.paragraph(0).unwrap().spans(),
            &[Span {
                range: 0..6,
                style: bold()
            }]
        );
        assert_eq!(document.style_at(Position::new(0, 0)), bold());
        assert_eq!(document.style_at(document.end()), bold());
        assert_normalized(&document);
    }

    #[test]
    fn inserted_combining_marks_cannot_split_formatting_runs() {
        let mut document = Document::from_text("ex");
        document
            .apply_style(
                Position::new(0, 0)..Position::new(0, 1),
                StylePatch {
                    bold: Some(true),
                    ..StylePatch::default()
                },
            )
            .unwrap();
        let (_, caret) = document
            .replace(
                Position::new(0, 1)..Position::new(0, 1),
                "\u{301}",
                InlineStyle {
                    italic: true,
                    ..InlineStyle::default()
                },
            )
            .unwrap();
        assert_eq!(document.plain_text(), "e\u{301}x");
        assert_eq!(caret, Position::new(0, 3));
        assert_eq!(document.paragraph(0).unwrap().spans()[0].range, 0..3);
        assert_eq!(document.paragraph(0).unwrap().spans()[0].style, bold());
        assert_normalized(&document);
    }

    #[test]
    fn emoji_joiner_insertion_snaps_caret_past_new_cluster() {
        let mut document = Document::from_text("👩👧");
        let (_, caret) = document
            .replace(Position::new(0, 4)..Position::new(0, 4), "\u{200d}", bold())
            .unwrap();
        assert_eq!(document.plain_text(), "👩‍👧");
        assert_eq!(caret, document.end());
        assert_eq!(document.paragraph(0).unwrap().spans().len(), 1);
        assert_normalized(&document);
    }

    #[test]
    fn joining_paragraphs_also_normalizes_clusters() {
        let mut document = Document::from_text("a\n\u{301}b");
        document
            .apply_style(
                Position::new(1, 0)..Position::new(1, 2),
                StylePatch {
                    italic: Some(true),
                    ..StylePatch::default()
                },
            )
            .unwrap();
        let (_, caret) = document
            .replace(
                Position::new(0, 1)..Position::new(1, 0),
                "",
                InlineStyle::default(),
            )
            .unwrap();
        assert_eq!(document.plain_text(), "a\u{301}b");
        assert_eq!(caret, Position::new(0, 3));
        assert_normalized(&document);
    }

    #[test]
    fn history_retains_only_changed_paragraphs_and_shares_others() {
        let mut document = Document::from_text("zero\none\ntwo\nthree");
        let unaffected = Arc::clone(&document.paragraphs()[3]);
        let (delta, _) = document
            .replace(Position::new(1, 1)..Position::new(1, 2), "N", bold())
            .unwrap();
        assert_eq!(delta.start, 1);
        assert_eq!(delta.before.len(), 1);
        assert_eq!(delta.after.len(), 1);
        assert!(delta.retained_bytes() > 0);
        assert!(Arc::ptr_eq(&unaffected, &document.paragraphs()[3]));
        let revision = document.revision();
        document.replay(&delta, false);
        assert_eq!(document.revision(), revision + 1);
        assert!(Arc::ptr_eq(&unaffected, &document.paragraphs()[3]));
        assert_eq!(document.plain_text(), "zero\none\ntwo\nthree");
    }

    #[test]
    fn heading_and_list_splits_preserve_paragraph_semantics() {
        let mut document = Document::from_text("heading");
        document
            .set_kind(0..1, ParagraphKind::Heading { level: 2 })
            .unwrap();
        document
            .replace(
                document.end()..document.end(),
                "\nbody",
                InlineStyle::default(),
            )
            .unwrap();
        assert_eq!(
            document.paragraph(0).unwrap().kind(),
            ParagraphKind::Heading { level: 2 }
        );
        assert_eq!(document.paragraph(1).unwrap().kind(), ParagraphKind::Body);
        document
            .set_kind(
                1..2,
                ParagraphKind::Ordered {
                    indent: 1,
                    start: 7,
                },
            )
            .unwrap();
        document
            .replace(
                document.end()..document.end(),
                "\nnext\nlast",
                InlineStyle::default(),
            )
            .unwrap();
        assert_eq!(
            document.paragraph(2).unwrap().kind(),
            ParagraphKind::Ordered {
                indent: 1,
                start: 8
            }
        );
        assert_eq!(
            document.paragraph(3).unwrap().kind(),
            ParagraphKind::Ordered {
                indent: 1,
                start: 9
            }
        );
        assert_normalized(&document);
    }

    #[test]
    fn deletion_of_a_complete_leading_paragraph_preserves_suffix_kind() {
        let mut document = Document::from_text("first\nheading");
        document
            .set_kind(1..2, ParagraphKind::Heading { level: 3 })
            .unwrap();
        document
            .replace(
                Position::new(0, 0)..Position::new(1, 0),
                "",
                InlineStyle::default(),
            )
            .unwrap();
        assert_eq!(document.plain_text(), "heading");
        assert_eq!(
            document.paragraph(0).unwrap().kind(),
            ParagraphKind::Heading { level: 3 }
        );
    }

    #[test]
    fn noops_do_not_change_revision_or_allocations() {
        let mut document = Document::from_text("unchanged");
        let original = Arc::clone(&document.paragraphs()[0]);
        let (delta, caret) = document
            .replace(
                Position::default()..document.end(),
                "unchanged",
                InlineStyle::default(),
            )
            .unwrap();
        assert!(delta.is_empty());
        assert_eq!(caret, document.end());
        assert_eq!(document.revision(), 0);
        assert!(Arc::ptr_eq(&original, &document.paragraphs()[0]));
        assert!(
            document
                .set_kind(0..1, ParagraphKind::Body)
                .unwrap()
                .is_none()
        );
        assert!(
            document
                .apply_style(Position::default()..document.end(), StylePatch::default())
                .unwrap()
                .is_none()
        );
        document.replay(&delta, false);
        assert_eq!(document.revision(), 0);
    }

    #[test]
    fn invalid_operations_leave_document_unchanged() {
        let mut document = Document::from_text("é");
        let original = document.clone();
        assert!(
            document
                .replace(Position::new(0, 1)..document.end(), "x", bold())
                .is_err()
        );
        assert!(matches!(
            document.set_kind(0..1, ParagraphKind::Heading { level: 0 }),
            Err(Error::InvalidHeadingLevel(0))
        ));
        assert!(matches!(
            document.set_kind(0..2, ParagraphKind::Body),
            Err(Error::InvalidRange)
        ));
        assert_eq!(
            document.text(document.end()..Position::default()),
            Err(Error::InvalidRange)
        );
        assert_eq!(document, original);
    }

    #[test]
    fn unicode_replacements_preserve_text_invariants_and_replay_exactly() {
        let inputs = [
            "",
            "abc",
            "e\u{301}x",
            "👩👧",
            "🇺🇸🇨🇦",
            "a\n\u{301}b",
            "क्\nष",
            "\u{600}a",
        ];
        let insertions = ["", "X", "\u{301}", "\u{200d}", "\n", "\r\n", "\n\u{301}"];
        for input in inputs {
            let mut original = Document::from_text(input);
            let mut positions = Vec::new();
            for (index, paragraph) in original.paragraphs().iter().enumerate() {
                positions.extend(
                    paragraph
                        .text()
                        .grapheme_indices(true)
                        .map(|(byte, _)| Position::new(index, byte)),
                );
                positions.push(Position::new(index, paragraph.text().len()));
            }
            // Give surviving edges different styles, so replay verifies both
            // content and formatting when replacement changes segmentation.
            for (index, position) in positions.iter().enumerate() {
                if index % 2 == 0
                    && let Some(end) = positions.get(index + 1)
                {
                    original
                        .apply_style(
                            *position..*end,
                            StylePatch {
                                bold: Some(true),
                                ..StylePatch::default()
                            },
                        )
                        .unwrap();
                }
            }
            let plain = original.plain_text();
            for (start_index, start) in positions.iter().enumerate() {
                for end in &positions[start_index..] {
                    let global_offset = |position: Position| {
                        original.paragraphs()[..position.paragraph]
                            .iter()
                            .map(|paragraph| paragraph.text().len() + 1)
                            .sum::<usize>()
                            + position.byte
                    };
                    for insertion in insertions {
                        let mut expected = plain.clone();
                        expected.replace_range(
                            global_offset(*start)..global_offset(*end),
                            &normalize_newlines(insertion),
                        );
                        let mut edited = original.clone();
                        let (delta, caret) = edited
                            .replace(
                                *start..*end,
                                insertion,
                                InlineStyle {
                                    italic: true,
                                    ..InlineStyle::default()
                                },
                            )
                            .unwrap();
                        assert_eq!(edited.plain_text(), expected);
                        assert!(edited.validate_position(caret).is_ok());
                        assert_normalized(&edited);
                        let result = edited.paragraphs().to_vec();
                        edited.replay(&delta, false);
                        assert_eq!(edited.paragraphs(), original.paragraphs());
                        edited.replay(&delta, true);
                        assert_eq!(edited.paragraphs(), result);
                    }
                }
            }
        }
    }
}
