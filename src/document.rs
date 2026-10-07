use std::{borrow::Cow, ops::Range, sync::Arc};

use unicode_segmentation::UnicodeSegmentation;

use crate::{Error, InlineStyle, ParagraphKind, Position, StylePatch};

/// A run of equally formatted text, measured in UTF-8 bytes within a paragraph.
///
/// Runs cover the complete paragraph, never overlap, and always end at extended
/// grapheme boundaries. Empty paragraphs have no runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: InlineStyle,
}

/// An immutable paragraph. Its text and formatting can be shared by history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
    text: Arc<str>,
    spans: Arc<[Span]>,
    kind: ParagraphKind,
    ascii: bool,
}

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

    fn with_kind(&self, kind: ParagraphKind) -> Self {
        Self {
            text: Arc::clone(&self.text),
            spans: Arc::clone(&self.spans),
            kind,
            ascii: self.ascii,
        }
    }

    fn plain(text: &str) -> Self {
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
            ascii: text.is_ascii(),
        }
    }

    fn styled(text: String, raw_spans: Vec<Span>, kind: ParagraphKind) -> Self {
        let ascii = text.is_ascii();
        let spans = normalize_spans(&text, &raw_spans, ascii);
        Self {
            text: text.into(),
            spans: spans.into(),
            kind,
            ascii,
        }
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    paragraphs: Vec<Arc<Paragraph>>,
    revision: u64,
}

impl Default for Document {
    fn default() -> Self {
        Self::from_text("")
    }
}

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    /// Import plain text, normalizing CRLF and lone CR to paragraph breaks.
    pub fn from_text(text: &str) -> Self {
        let text = normalize_newlines(text);
        Self {
            paragraphs: text
                .split('\n')
                .map(|text| Arc::new(Paragraph::plain(text)))
                .collect(),
            revision: 0,
        }
    }

    pub fn paragraphs(&self) -> &[Arc<Paragraph>] {
        &self.paragraphs
    }

    pub fn paragraph(&self, index: usize) -> Option<&Paragraph> {
        self.paragraphs.get(index).map(Arc::as_ref)
    }

    pub fn end(&self) -> Position {
        let paragraph = self.paragraphs.len() - 1;
        Position::new(paragraph, self.paragraphs[paragraph].text.len())
    }

    /// Increases on every actual edit, undo, or redo; it is never restored by undo.
    pub fn revision(&self) -> u64 {
        self.revision
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

    pub fn validate_position(&self, position: Position) -> Result<(), Error> {
        let Some(paragraph) = self.paragraph(position.paragraph) else {
            return Err(Error::InvalidPosition(position));
        };
        if position.byte == paragraph.text.len()
            || (paragraph.ascii && position.byte < paragraph.text.len())
            || paragraph
                .text()
                .grapheme_indices(true)
                .any(|(byte, _)| byte == position.byte)
        {
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
        if position.byte > paragraph.text.len() {
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
            boundary_at_or_after(caret_source, insertion_end),
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
        }
        delta
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

fn normalize_newlines(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
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

fn normalize_spans(text: &str, raw: &[Span], ascii: bool) -> Vec<Span> {
    // Paragraphs contain no CR/LF, so every ASCII byte is a whole grapheme.
    // The common case can retain already normalized runs without segmentation.
    if ascii {
        return raw.to_vec();
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

fn boundary_at_or_after(paragraph: &Paragraph, byte: usize) -> usize {
    let text = paragraph.text();
    if paragraph.ascii {
        return byte.min(text.len());
    }
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .find(|index| *index >= byte)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

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
