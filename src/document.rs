use std::{
    borrow::Cow,
    fmt,
    ops::Range,
    sync::{Arc, OnceLock},
};

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use crate::{Error, Fragment, InlineStyle, Link, ParagraphKind, Position, StylePatch};

mod change;
mod numbering;
mod paragraph;
mod words;

pub(crate) use change::Delta;
pub(crate) use numbering::{Levels, same_effect as same_numbering_effect};
pub use paragraph::{Paragraph, Span};
use paragraph::{append_slice, push_span};

#[cfg(any(feature = "egui", feature = "accesskit"))]
use change::LastChange;

#[derive(Clone)]
pub struct Document {
    paragraphs: Vec<Arc<Paragraph>>,
    revision: u64,
    identity: Arc<()>,
    numbers: OnceLock<Arc<[(usize, u32)]>>,
    #[cfg(any(feature = "egui", feature = "accesskit"))]
    last_change: Option<LastChange>,
}

impl PartialEq for Document {
    fn eq(&self, other: &Self) -> bool {
        self.paragraphs == other.paragraphs && self.revision == other.revision
    }
}

impl Eq for Document {}

impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("paragraphs", &self.paragraphs)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
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

    pub fn from_text(text: &str) -> Self {
        let text = normalize_newlines(text);
        Self::from_shared(
            text.split('\n')
                .map(|text| Arc::new(Paragraph::plain(text)))
                .collect(),
        )
    }

    pub fn from_fragment(fragment: &Fragment) -> Self {
        Self::from_shared(fragment.paragraphs().to_vec())
    }

    pub fn from_paragraphs<I>(paragraphs: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Arc<Paragraph>>,
    {
        let paragraphs: Vec<_> = paragraphs.into_iter().map(Into::into).collect();
        if paragraphs.is_empty() {
            Self::default()
        } else {
            Self::from_shared(paragraphs)
        }
    }

    fn from_shared(paragraphs: Vec<Arc<Paragraph>>) -> Self {
        Self {
            paragraphs,
            revision: 0,
            identity: Arc::new(()),
            numbers: OnceLock::new(),
            #[cfg(any(feature = "egui", feature = "accesskit"))]
            last_change: None,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        Fragment::encode_paragraphs(self.paragraphs())
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, crate::FragmentError> {
        Fragment::from_bytes(bytes).map(|fragment| Self::from_shared(fragment.into_paragraphs()))
    }

    pub fn to_html(&self) -> String {
        crate::export::html(self.paragraphs(), Levels::default())
    }

    pub fn fragment(&self, range: Range<Position>) -> Result<Fragment, Error> {
        self.validate_range(&range)?;
        let numbering = if range.start.paragraph != 0
            && self.paragraphs[range.start.paragraph..=range.end.paragraph]
                .iter()
                .any(|paragraph| matches!(paragraph.kind, ParagraphKind::Ordered { .. }))
        {
            Levels::before(&self.paragraphs, range.start.paragraph, |index| {
                self.list_number(index)
            })
        } else {
            Levels::default()
        };
        let mut levels = numbering.clone();
        let mut captured_levels: Vec<u8> = Vec::new();
        let paragraphs = self
            .slices(&range)
            .map(|(_, paragraph, slice)| {
                let mut kind = paragraph.kind;
                if let ParagraphKind::Ordered { indent, start } = kind
                    && !captured_levels.contains(&indent)
                {
                    captured_levels.push(indent);
                    if start.is_none() && levels.open_run(indent).is_none() {
                        kind = ParagraphKind::Ordered {
                            indent,
                            start: Some(1),
                        };
                    }
                }
                levels.push(paragraph.kind);
                if slice == (0..paragraph.text.len()) {
                    paragraph.with_kind(kind)
                } else {
                    let mut text = String::with_capacity(slice.len());
                    let mut spans = Vec::new();
                    append_slice(&mut text, &mut spans, paragraph, slice);
                    Arc::new(Paragraph::styled(text, spans, kind))
                }
            })
            .collect();
        Ok(Fragment::captured(paragraphs, numbering))
    }

    pub fn paragraphs(&self) -> &[Arc<Paragraph>] {
        &self.paragraphs
    }

    pub(crate) fn into_paragraphs(self) -> Vec<Arc<Paragraph>> {
        self.paragraphs
    }

    pub fn paragraph(&self, index: usize) -> Option<&Paragraph> {
        self.paragraphs.get(index).map(Arc::as_ref)
    }

    pub fn list_number(&self, index: usize) -> Option<u32> {
        match self.paragraphs.get(index)?.kind {
            ParagraphKind::Ordered {
                start: Some(start), ..
            } => Some(start),
            ParagraphKind::Ordered { start: None, .. } => {
                let numbers = self
                    .numbers
                    .get_or_init(|| numbering::numbers(&self.paragraphs));
                let position = numbers
                    .binary_search_by_key(&index, |&(paragraph, _)| paragraph)
                    .expect("ordered items are numbered");
                Some(numbers[position].1)
            }
            _ => None,
        }
    }

    #[cfg(feature = "egui")]
    pub(crate) fn numbered_items_from(&self, from: usize) -> &[(usize, u32)] {
        let numbers = self
            .numbers
            .get_or_init(|| numbering::numbers(&self.paragraphs));
        &numbers[numbers.partition_point(|&(index, _)| index < from)..]
    }

    pub fn end(&self) -> Position {
        let paragraph = self.paragraphs.len() - 1;
        Position::new(paragraph, self.paragraphs[paragraph].text.len())
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn content_identity(&self) -> Arc<()> {
        Arc::clone(&self.identity)
    }

    pub fn plain_text(&self) -> String {
        crate::export::plain_text(self.paragraphs())
    }

    pub fn text(&self, range: Range<Position>) -> Result<String, Error> {
        self.validate_range(&range)?;
        let mut result = String::new();
        for (index, paragraph, slice) in self.slices(&range) {
            if index != range.start.paragraph {
                result.push('\n');
            }
            crate::export::push_plain(&mut result, &paragraph.text[slice]);
        }
        Ok(result)
    }

    fn slices(
        &self,
        range: &Range<Position>,
    ) -> impl Iterator<Item = (usize, &Arc<Paragraph>, Range<usize>)> {
        let Range { start, end } = range.clone();
        (start.paragraph..=end.paragraph).map(move |index| {
            let paragraph = &self.paragraphs[index];
            let from = if index == start.paragraph {
                start.byte
            } else {
                0
            };
            let to = if index == end.paragraph {
                end.byte
            } else {
                paragraph.text.len()
            };
            (index, paragraph, from..to)
        })
    }

    pub(crate) fn spans_in(&self, range: &Range<Position>) -> impl Iterator<Item = &Span> {
        self.slices(range)
            .flat_map(|(_, paragraph, slice)| &paragraph.spans[paragraph.overlapping(slice)])
    }

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

    pub fn link_at(&self, position: Position) -> Option<&Link> {
        let paragraph = self.paragraph(position.paragraph)?;
        if !paragraph.is_grapheme_boundary(position.byte) {
            return None;
        }
        paragraph.link_for_byte(position.byte)
    }

    pub fn link_range_at(&self, position: Position) -> Option<Range<Position>> {
        let paragraph = self.paragraph(position.paragraph)?;
        if !paragraph.is_grapheme_boundary(position.byte) {
            return None;
        }
        let range = paragraph.link_range(position.byte)?;
        Some(
            Position::new(position.paragraph, range.start)
                ..Position::new(position.paragraph, range.end),
        )
    }

    fn inside_link(&self, range: &Range<Position>) -> Option<&Link> {
        if range.start.byte == 0 {
            return None;
        }
        let before = self.paragraphs[range.start.paragraph].link_for_byte(range.start.byte - 1)?;
        let after = self.paragraphs[range.end.paragraph].link_for_byte(range.end.byte)?;
        (before == after).then_some(before)
    }

    fn caret_link_location(&self, position: Position) -> Option<(&Paragraph, usize)> {
        let paragraph = self.paragraph(position.paragraph)?;
        let byte = [position.byte.checked_sub(1), Some(position.byte)]
            .into_iter()
            .flatten()
            .find(|byte| paragraph.link_for_byte(*byte).is_some())?;
        Some((paragraph, byte))
    }

    pub(crate) fn caret_link(&self, position: Position) -> Option<&Link> {
        let (paragraph, byte) = self.caret_link_location(position)?;
        paragraph.link_for_byte(byte)
    }

    pub(crate) fn caret_link_range(&self, position: Position) -> Option<Range<Position>> {
        let (paragraph, byte) = self.caret_link_location(position)?;
        let range = paragraph.link_range(byte)?;
        Some(
            Position::new(position.paragraph, range.start)
                ..Position::new(position.paragraph, range.end),
        )
    }

    pub(crate) fn replace(
        &mut self,
        range: Range<Position>,
        text: &str,
        style: InlineStyle,
    ) -> Result<(Delta, Position), Error> {
        self.validate_range(&range)?;
        let link = self.inside_link(&range).cloned();
        self.replace_with_link(range, text, style, link.as_ref())
    }

    pub(crate) fn replace_with_link(
        &mut self,
        range: Range<Position>,
        text: &str,
        style: InlineStyle,
        link: Option<&Link>,
    ) -> Result<(Delta, Position), Error> {
        self.replace_with_kind(range, text, style, link, None)
    }

    pub(crate) fn replace_with_kind(
        &mut self,
        range: Range<Position>,
        text: &str,
        style: InlineStyle,
        link: Option<&Link>,
        kind: Option<ParagraphKind>,
    ) -> Result<(Delta, Position), Error> {
        self.validate_range(&range)?;
        if range.is_empty() && text.is_empty() {
            return Ok((Delta::empty(range.start.paragraph), range.start));
        }
        let text = normalize_newlines(text);
        let first = &self.paragraphs[range.start.paragraph];
        let last = &self.paragraphs[range.end.paragraph];
        let chunks: Vec<&str> = text.split('\n').collect();
        let leading = removes_leading_paragraph(&range);
        let base = kind.unwrap_or_else(|| {
            if leading {
                self.surviving_kind(range.start.paragraph..range.end.paragraph, last.kind)
            } else {
                first.kind
            }
        });
        let mut after = Vec::with_capacity(chunks.len());
        let mut insertion_end = 0;
        for (index, chunk) in chunks.iter().enumerate() {
            let is_first = index == 0;
            let is_last = index + 1 == chunks.len();
            let kind = if is_first {
                base
            } else if is_last {
                suffix_kind(base, last, &range, leading, index)
            } else {
                continuation_kind(base, index)
            };
            if chunk.is_empty() && !is_last && is_first && range.start.byte == first.text.len() {
                after.push(first.with_kind(kind));
                continue;
            }
            if chunk.is_empty() && is_last && !is_first && range.end.byte == 0 {
                after.push(last.with_kind(kind));
                continue;
            }
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
            push_span(&mut spans, start..output.len(), style, link);
            if is_last {
                insertion_end = output.len();
                append_slice(
                    &mut output,
                    &mut spans,
                    last,
                    range.end.byte..last.text.len(),
                );
            }
            after.push(Arc::new(Paragraph::styled(output, spans, kind)));
        }
        let caret_paragraph = range.start.paragraph + after.len() - 1;
        let caret_source = after.last().expect("replacement has a paragraph");
        let caret = Position::new(
            caret_paragraph,
            caret_source.boundary_at_or_after(insertion_end),
        );
        let delta = self.change(range.start.paragraph..range.end.paragraph + 1, after);
        Ok((delta, caret))
    }

    pub(crate) fn replace_fragment(
        &mut self,
        range: Range<Position>,
        fragment: &Fragment,
    ) -> Result<(Delta, Position), Error> {
        self.validate_range(&range)?;
        if range.is_empty()
            && let [paragraph] = fragment.paragraphs()
            && paragraph.text.is_empty()
        {
            return Ok((Delta::empty(range.start.paragraph), range.start));
        }
        let first = &self.paragraphs[range.start.paragraph];
        let last = &self.paragraphs[range.end.paragraph];
        let count = fragment.paragraphs().len();
        let multiple = range.start.paragraph != range.end.paragraph;
        let prefix = range.start.byte > 0;
        let suffix = range.end.byte < last.text.len();
        let inline = count == 1 && (prefix || suffix || fragment.paragraphs()[0].text.is_empty());
        let leading = inline && removes_leading_paragraph(&range);
        let mut after: Vec<Arc<Paragraph>> = Vec::with_capacity(count);
        let mut insertion_end = 0;
        for (index, source) in fragment.paragraphs().iter().enumerate() {
            let is_first = index == 0;
            let is_last = index + 1 == count;
            let prefix_len = if is_first { range.start.byte } else { 0 };
            let suffix_len = if is_last {
                last.text.len() - range.end.byte
            } else {
                0
            };
            let kind = if leading {
                self.surviving_kind(range.start.paragraph..range.end.paragraph, last.kind)
            } else if inline || (is_first && prefix_len > 0) {
                first.kind
            } else if is_last && suffix_len > 0 {
                if multiple || range.start.byte == 0 {
                    last.kind
                } else {
                    numbered_kind(first.kind, 1)
                }
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
                paragraph.with_kind(kind)
            })
            .collect();
        self.change(range, after)
    }

    fn surviving_kind(&self, removed: Range<usize>, kind: ParagraphKind) -> ParagraphKind {
        numbering::surviving_kind(
            self.paragraphs[removed]
                .iter()
                .map(|paragraph| paragraph.kind),
            kind,
        )
    }

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
            let end = &self.paragraphs[range.end.paragraph];
            let style = if range.start.byte < source.text.len() {
                source.style_for_byte(range.start.byte)
            } else {
                builder.last_style()
            };
            let link = source.link_for_byte(range.start.byte);
            let multiple = range.start.paragraph != range.end.paragraph;
            let leading = builder.is_empty() && multiple;
            if leading {
                let removed = self.paragraphs[range.start.paragraph + 1..range.end.paragraph]
                    .iter()
                    .map(|paragraph| paragraph.kind);
                builder.kind = numbering::surviving_kind(
                    std::iter::once(builder.kind).chain(removed),
                    end.kind,
                );
            }
            builder.append_replacement(
                &replacement_chunks,
                style,
                link,
                range.end.byte < end.text.len(),
            );
            if multiple && multiline && !leading {
                builder.kind = end.kind;
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
        let after = builder.paragraphs;
        caret.byte =
            after[caret.paragraph - first.start.paragraph].boundary_at_or_after(caret.byte);
        let range = first.start.paragraph..last.end.paragraph + 1;
        (self.change(range, after), caret)
    }

    pub(crate) fn apply_style(
        &mut self,
        range: Range<Position>,
        patch: StylePatch,
    ) -> Result<Delta, Error> {
        self.validate_range(&range)?;
        if range.is_empty() || patch == StylePatch::default() {
            return Ok(Delta::empty(range.start.paragraph));
        }
        let after = self
            .slices(&range)
            .map(|(_, paragraph, slice)| {
                paragraph
                    .with_style(slice, patch)
                    .map_or_else(|| Arc::clone(paragraph), Arc::new)
            })
            .collect();
        Ok(self.change(range.start.paragraph..range.end.paragraph + 1, after))
    }

    pub(crate) fn set_link(
        &mut self,
        range: Range<Position>,
        link: Option<&Link>,
    ) -> Result<Delta, Error> {
        self.validate_range(&range)?;
        if range.is_empty() {
            return Ok(Delta::empty(range.start.paragraph));
        }
        let after = self
            .slices(&range)
            .map(|(_, paragraph, slice)| {
                paragraph
                    .with_link(slice, link)
                    .map_or_else(|| Arc::clone(paragraph), Arc::new)
            })
            .collect();
        Ok(self.change(range.start.paragraph..range.end.paragraph + 1, after))
    }

    pub(crate) fn set_kind(
        &mut self,
        range: Range<usize>,
        kind: ParagraphKind,
    ) -> Result<Delta, Error> {
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
            .map(|(offset, paragraph)| paragraph.with_kind(numbered_kind(kind, offset)))
            .collect();
        Ok(self.change(range, after))
    }

    pub(crate) fn apply_numbered_list(&mut self, range: Range<usize>, indent: u8) -> Delta {
        let is_item =
            |kind| matches!(kind, ParagraphKind::Ordered { indent: level, .. } if level == indent);
        let selected = &self.paragraphs[range.clone()];
        if selected.iter().all(|paragraph| is_item(paragraph.kind)) {
            return Delta::empty(range.start);
        }
        let extends = self.paragraphs[..range.start]
            .iter()
            .rev()
            .map(|paragraph| paragraph.kind)
            .find(|kind| match *kind {
                ParagraphKind::Ordered { indent: level, .. }
                | ParagraphKind::Bullet { indent: level } => level <= indent,
                _ => true,
            })
            .is_some_and(is_item);
        let after = selected
            .iter()
            .enumerate()
            .map(|(offset, paragraph)| {
                let start = match paragraph.kind {
                    _ if offset > 0 => None,
                    ParagraphKind::Ordered {
                        indent: level,
                        start,
                    } if level == indent => start,
                    _ if extends => None,
                    _ => Some(1),
                };
                paragraph.with_kind(ParagraphKind::Ordered { indent, start })
            })
            .collect();
        self.change(range, after)
    }
}

struct ReplacementBuilder {
    paragraphs: Vec<Arc<Paragraph>>,
    text: String,
    spans: Vec<Span>,
    kind: ParagraphKind,
    candidate: Option<Arc<Paragraph>>,
    // Start of the last grapheme cluster of `text[..scanned]`.
    tail: usize,
    scanned: usize,
}

impl ReplacementBuilder {
    fn new(kind: ParagraphKind) -> Self {
        Self {
            paragraphs: Vec::new(),
            text: String::new(),
            spans: Vec::new(),
            kind,
            candidate: None,
            tail: 0,
            scanned: 0,
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

    fn last_style(&mut self) -> InlineStyle {
        if let Some(paragraph) = &self.candidate {
            return paragraph.style_for_byte(paragraph.text.len().saturating_sub(1));
        }
        self.tail = last_grapheme_start(&self.text, self.tail, self.scanned);
        self.scanned = self.text.len();
        let index = self
            .spans
            .partition_point(|span| span.range.end <= self.tail);
        self.spans
            .get(index)
            .map_or_else(InlineStyle::default, |span| span.style)
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
        self.tail = 0;
        self.scanned = 0;
        if let Some(paragraph) = self.candidate.take() {
            self.paragraphs.push(paragraph.with_kind(self.kind));
        } else {
            self.paragraphs.push(Arc::new(Paragraph::styled(
                std::mem::take(&mut self.text),
                std::mem::take(&mut self.spans),
                self.kind,
            )));
        }
    }

    fn append_document(&mut self, document: &Document, range: Range<Position>) {
        for (index, paragraph, slice) in document.slices(&range) {
            if index > range.start.paragraph {
                self.finish_paragraph();
                self.kind = paragraph.kind;
            }
            if self.is_empty() && slice == (0..paragraph.text.len()) {
                self.candidate = Some(Arc::clone(paragraph));
            } else if !slice.is_empty() {
                self.materialize_candidate();
                append_slice(&mut self.text, &mut self.spans, paragraph, slice);
            }
        }
    }

    fn append_replacement(
        &mut self,
        chunks: &[&str],
        style: InlineStyle,
        link: Option<&Link>,
        suffix: bool,
    ) {
        let kind = self.kind;
        for (index, chunk) in chunks.iter().enumerate() {
            if index > 0 {
                self.finish_paragraph();
                self.kind = if suffix && index + 1 == chunks.len() {
                    numbered_kind(kind, index)
                } else {
                    continuation_kind(kind, index)
                };
            }
            if !chunk.is_empty() {
                self.materialize_candidate();
                let start = self.text.len();
                self.text.push_str(chunk);
                push_span(&mut self.spans, start..self.text.len(), style, link);
            }
        }
    }
}

// Start of the last grapheme cluster of `text`, given the start `tail` of the last
// cluster of `text[..scanned]`. Appending cannot move a boundary before `scanned`, so
// segmentation resumes at `tail` and walks forward over the appended bytes only.
// Searching back from the end would rescan a cluster that keeps growing.
fn last_grapheme_start(text: &str, tail: usize, scanned: usize) -> usize {
    let last_start = |text: &str| {
        text.grapheme_indices(true)
            .next_back()
            .map_or(0, |(start, _)| start)
    };
    if scanned == text.len() {
        return tail;
    }
    let first = match text[..scanned].chars().next_back() {
        None => 0,
        Some(previous) => {
            let rest = &text[tail..];
            let offset = scanned - previous.len_utf8() - tail;
            match GraphemeCursor::new(offset, rest.len(), true).next_boundary(rest, 0) {
                Ok(Some(boundary)) => tail + boundary,
                _ => return last_start(text),
            }
        }
    };
    if first == text.len() {
        tail
    } else {
        first + last_start(&text[first..])
    }
}

pub(crate) const LINE_SEPARATOR: char = '\u{2028}';
const PARAGRAPH_SEPARATOR: char = '\u{2029}';

pub(crate) fn normalize_newlines(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') && !text.contains(PARAGRAPH_SEPARATOR) {
        return Cow::Borrowed(text);
    }
    let mut normalized = String::with_capacity(text.len());
    let mut start = 0;
    for (index, separator) in text.match_indices(['\r', PARAGRAPH_SEPARATOR]) {
        normalized.push_str(&text[start..index]);
        normalized.push('\n');
        start = index
            + separator.len()
            + usize::from(separator == "\r" && text.as_bytes().get(index + 1) == Some(&b'\n'));
    }
    normalized.push_str(&text[start..]);
    Cow::Owned(normalized)
}

fn removes_leading_paragraph(range: &Range<Position>) -> bool {
    range.start.paragraph != range.end.paragraph && range.start.byte == 0
}

fn suffix_kind(
    base: ParagraphKind,
    last: &Paragraph,
    range: &Range<Position>,
    leading: bool,
    index: usize,
) -> ParagraphKind {
    if range.start.paragraph != range.end.paragraph && !leading {
        last.kind
    } else if range.end.byte < last.text.len() {
        numbered_kind(base, index)
    } else {
        continuation_kind(base, index)
    }
}

fn continuation_kind(kind: ParagraphKind, offset: usize) -> ParagraphKind {
    match kind {
        ParagraphKind::Heading { .. } => ParagraphKind::Body,
        other => numbered_kind(other, offset),
    }
}

fn numbered_kind(kind: ParagraphKind, offset: usize) -> ParagraphKind {
    match kind {
        ParagraphKind::Ordered { indent, .. } if offset > 0 => ParagraphKind::Ordered {
            indent,
            start: None,
        },
        other => other,
    }
}

#[cfg(test)]
mod tests;
