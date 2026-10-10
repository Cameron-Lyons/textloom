use std::{borrow::Cow, ops::Range, sync::Arc};

use crate::{Error, Fragment, InlineStyle, ParagraphKind, Position, StylePatch};

mod change;
mod paragraph;

pub(crate) use change::Delta;
pub use paragraph::{Paragraph, Span};
use paragraph::{append_slice, push_span};

#[cfg(any(feature = "egui", feature = "accesskit"))]
use change::LastChange;

/// A document always contains at least one paragraph.
///
/// Paragraphs are shared with undo history. An edit rebuilds only its affected
/// paragraphs; all other paragraph allocations remain intact.
/// Equality compares rich content and the revision counter. To compare content
/// independently of revision, compare [`Fragment::from_document`] values.
#[derive(Clone, Debug)]
pub struct Document {
    paragraphs: Vec<Arc<Paragraph>>,
    revision: u64,
    identity: Arc<()>,
    #[cfg(any(feature = "egui", feature = "accesskit"))]
    last_change: Option<LastChange>,
}

impl PartialEq for Document {
    fn eq(&self, other: &Self) -> bool {
        self.paragraphs == other.paragraphs && self.revision == other.revision
    }
}

impl Eq for Document {}

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
            #[cfg(any(feature = "egui", feature = "accesskit"))]
            last_change: None,
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
        crate::export::plain_text(self.paragraphs())
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
        if range.is_empty() || patch == StylePatch::default() {
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
            after.push(
                paragraph
                    .with_style(start..end, patch)
                    .map_or_else(|| Arc::clone(paragraph), Arc::new),
            );
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

#[cfg(test)]
mod tests;
