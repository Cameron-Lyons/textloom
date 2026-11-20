use std::borrow::Cow;
use std::ops::{ControlFlow, Range};
use std::sync::Arc;

use unicode_segmentation::{UWordBoundIndices, UWordBounds, UnicodeSegmentation};

use crate::model::word_restart;
use crate::types::Direction;
use crate::{Document, Paragraph, Position, model};

/// Options for [`Document::find`] and the editor's search commands.
///
/// The default is a case-sensitive search that matches anywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "built with struct update syntax from `SearchOptions::default()`; a new field is a major release"
)]
pub struct SearchOptions {
    /// Whether matching distinguishes letter case. When `false`, the query
    /// and the text are compared by their Unicode lowercase mappings, with
    /// final sigma `ς` treated as `σ`, without normalization or full case
    /// folding.
    pub case_sensitive: bool,
    /// Whether a match must start and end at Unicode word boundaries
    /// (UAX #29).
    pub whole_word: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: true,
            whole_word: false,
        }
    }
}

/// What [`Editor::find_next`](crate::Editor::find_next) and
/// [`find_previous`](crate::Editor::find_previous) do when no match remains
/// before the end (or start) of the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[expect(
    clippy::exhaustive_enums,
    reason = "a search either wraps or stops; there is no third behavior"
)]
pub enum SearchWrap {
    /// Continues the search from the other end of the document.
    Around,
    /// Stops at the end of the document (or its start, for `find_previous`)
    /// and reports no match.
    Stop,
}

impl Document {
    /// Returns the matches of `query` in document order.
    ///
    /// Matching is literal. LF, CRLF, a lone CR, or U+2029 in `query` matches
    /// a paragraph break, and U+2028 matches a soft line break. A match always
    /// covers complete graphemes, so an occurrence that would split one does
    /// not match. Matches do not overlap: each is the first acceptable
    /// occurrence that starts at or after the end of the previous match. An
    /// empty query matches nothing.
    #[must_use]
    pub fn find(&self, query: &str, options: SearchOptions) -> Vec<Range<Position>> {
        let mut matches = Vec::new();
        if !query.is_empty() {
            let needle = search_needle(query, options);
            let _ = self.visit_normalized_matches(&needle, options, |range| {
                matches.push(range);
                ControlFlow::Continue(())
            });
        }
        matches
    }

    pub(crate) fn is_match(
        &self,
        query: &str,
        options: SearchOptions,
        range: &Range<Position>,
    ) -> bool {
        // The editor supplies its validated selection, so its endpoints are
        // already grapheme boundaries inside this document.
        if query.is_empty() || range.is_empty() {
            return false;
        }
        let needle = search_needle(query, options);
        if !self.range_matches_text(range, &needle, options.case_sensitive)
            || (options.whole_word && !self.has_word_edges(range))
        {
            return false;
        }
        // A matching selection belongs to the forward stream unless an earlier
        // accepted occurrence could overlap it. Only that case needs a search.
        // Normalization can add a border ("Aa" lowers to "aa"), so check the
        // needle rather than the query.
        let multiline = self.multiline_query(&needle, Direction::Forward);
        if multiline.as_ref().map_or_else(
            || never_overlaps(&needle),
            |pattern| !pattern.can_overlap(&needle),
        ) {
            return true;
        }
        let mut found = false;
        let check = |candidate: Range<Position>| {
            if candidate.end <= range.start {
                return ControlFlow::Continue(());
            }
            found = candidate == *range;
            ControlFlow::Break(())
        };
        if let Some(lines) = multiline {
            let _ = self.visit_multiline_matches(lines, options, check);
        } else {
            let _ = self.visit_paragraph_matches(range.start.paragraph, &needle, options, check);
        }
        found
    }

    fn range_matches_text(
        &self,
        range: &Range<Position>,
        needle: &str,
        case_sensitive: bool,
    ) -> bool {
        let mut lines = needle.split('\n');
        for index in range.start.paragraph..=range.end.paragraph {
            let Some(line) = lines.next() else {
                return false;
            };
            let paragraph = &self.paragraphs()[index];
            let text = paragraph.text();
            let start = if index == range.start.paragraph {
                range.start.byte
            } else {
                0
            };
            let end = if index == range.end.paragraph {
                range.end.byte
            } else {
                text.len()
            };
            let selected = &text[start..end];
            let matches = if case_sensitive {
                selected == line
            } else {
                selected.eq_ignore_ascii_case(line)
                    || (!paragraph.is_ascii()
                        && lowered_prefix_end(selected, line) == Some(selected.len()))
            };
            if !matches {
                return false;
            }
        }
        lines.next().is_none()
    }

    // Edges in one paragraph share a segmenter, so the stream that decides the
    // start continues to the end instead of restarting.
    fn has_word_edges(&self, range: &Range<Position>) -> bool {
        let first = &self.paragraphs()[range.start.paragraph];
        let mut words = WordBoundaries::for_paragraph(first, range.start.byte);
        if !words.contains(range.start.byte) {
            return false;
        }
        if range.start.paragraph == range.end.paragraph {
            words.contains(range.end.byte)
        } else {
            is_word_boundary(&self.paragraphs()[range.end.paragraph], range.end.byte)
        }
    }

    pub(crate) fn find_relative(
        &self,
        query: &str,
        options: SearchOptions,
        selection: Range<Position>,
        wrap: SearchWrap,
        direction: Direction,
    ) -> Option<Range<Position>> {
        if query.is_empty() {
            return None;
        }
        let wrap = wrap == SearchWrap::Around;
        let needle = search_needle(query, options);
        let boundary = match direction {
            Direction::Backward => selection.start,
            Direction::Forward => selection.end,
        };
        let lines = match self.multiline_query(&needle, direction) {
            Some(pattern) if pattern.lines.len() > self.paragraphs().len() => return None,
            Some(mut pattern) if pattern.can_overlap(&needle) => {
                pattern.restore_forward_order();
                let mut found = None;
                let mut wrapped = None;
                let _ = self.visit_multiline_matches(pattern, options, |range| {
                    match direction {
                        Direction::Backward => {
                            if range.end <= selection.start {
                                found = Some(range);
                            } else if found.is_some() || !wrap {
                                return ControlFlow::Break(());
                            } else {
                                wrapped = Some(range);
                            }
                        }
                        Direction::Forward => {
                            if range.start >= selection.end {
                                found = Some(range);
                                return ControlFlow::Break(());
                            }
                            if wrap && wrapped.is_none() {
                                wrapped = Some(range);
                            }
                        }
                    }
                    ControlFlow::Continue(())
                });
                return found.or(wrapped);
            }
            Some(pattern) if pattern.uses_kmp() => {
                return pattern.find_near(self, options, boundary, wrap);
            }
            pattern => pattern.map(|pattern| pattern.lines),
        };

        let strategy = ParagraphSearch::new(&needle, options);
        let len = self.paragraphs().len();
        let start = boundary.paragraph;
        let search = |index: usize, bounded: bool| {
            let boundary = bounded.then_some(boundary);
            if let Some(lines) = &lines {
                self.find_lines_match(index, lines, options, boundary, direction)
            } else {
                self.find_paragraph_match(index, &needle, options, boundary, direction, strategy)
            }
        };
        match direction {
            Direction::Backward => (0..=start)
                .rev()
                .find_map(|index| search(index, index == start))
                .or_else(|| {
                    wrap.then(|| (start..len).rev().find_map(|index| search(index, false)))
                        .flatten()
                }),
            Direction::Forward => (start..len)
                .find_map(|index| search(index, index == start))
                .or_else(|| {
                    wrap.then(|| (0..=start).find_map(|index| search(index, false)))
                        .flatten()
                }),
        }
    }

    fn is_grapheme_range(&self, range: &Range<Position>) -> bool {
        self.validate_position(range.start).is_ok() && self.validate_position(range.end).is_ok()
    }

    fn find_paragraph_match(
        &self,
        index: usize,
        needle: &str,
        options: SearchOptions,
        boundary: Option<Position>,
        direction: Direction,
        strategy: ParagraphSearch,
    ) -> Option<Range<Position>> {
        let text = self.paragraphs()[index].text();
        let (unbounded_edge, exhausted_edge) = match direction {
            Direction::Backward => (text.len(), 0),
            Direction::Forward => (0, text.len()),
        };
        if boundary.is_some_and(|position| position.byte == exhausted_edge) {
            return None;
        }
        let edge = boundary.map_or(unbounded_edge, |position| position.byte);
        match strategy {
            ParagraphSearch::Literal => {
                let window = match direction {
                    Direction::Backward => 0..edge,
                    Direction::Forward => edge..text.len(),
                };
                find_literal(&text[window.clone()], needle, direction, |bytes| {
                    let range = Position::new(index, window.start + bytes.start)
                        ..Position::new(index, window.start + bytes.end);
                    self.is_grapheme_range(&range).then_some(range)
                })
            }
            ParagraphSearch::Nearest => {
                self.find_nearest_match(index, needle, options, edge, direction)
            }
            ParagraphSearch::Scan => {
                let mut found = None;
                let _ = self.visit_paragraph_matches(index, needle, options, |range| {
                    match direction {
                        Direction::Backward => {
                            if boundary.is_some_and(|position| range.end > position) {
                                return ControlFlow::Break(());
                            }
                            found = Some(range);
                        }
                        Direction::Forward => {
                            if boundary.is_none_or(|position| range.start >= position) {
                                found = Some(range);
                                return ControlFlow::Break(());
                            }
                        }
                    }
                    ControlFlow::Continue(())
                });
                found
            }
        }
    }

    fn find_nearest_match(
        &self,
        index: usize,
        needle: &str,
        options: SearchOptions,
        edge: usize,
        direction: Direction,
    ) -> Option<Range<Position>> {
        let paragraph = &self.paragraphs()[index];
        let text = paragraph.text();
        // A character spans at most four bytes and lowercases to at least one,
        // so windows sharing this many bytes keep candidates crossing an edge.
        let overlap = needle.len().saturating_mul(4);
        let mut width = overlap.saturating_mul(2).max(NEAREST_WINDOW);
        let mut window = edge..edge;
        let mut words: Option<WordBoundaries<'_>> = None;
        loop {
            window = match direction {
                Direction::Backward => {
                    let end = text
                        .ceil_char_boundary(window.start.saturating_add(overlap))
                        .min(edge);
                    text.floor_char_boundary(end.saturating_sub(width))..end
                }
                Direction::Forward => {
                    let start = text
                        .floor_char_boundary(window.end.saturating_sub(overlap))
                        .max(edge);
                    start..text.ceil_char_boundary(start.saturating_add(width))
                }
            };
            let source = &text[window.clone()];
            let searched = if options.case_sensitive {
                Cow::Borrowed(source)
            } else {
                Cow::Owned(lowercase(source))
            };
            let mut offsets = LowercaseOffsets::new(source, !options.case_sensitive);
            let found = find_literal(&searched, needle, direction, |bytes| {
                let start = window.start + offsets.original(bytes.start)?;
                let end = window.start + offsets.original(bytes.end)?;
                if options.whole_word {
                    let words = match &mut words {
                        Some(words) if words.start <= start => words,
                        words => {
                            words.insert(WordBoundaries::for_paragraph(paragraph, window.start))
                        }
                    };
                    if !words.contains(end) || !words.contains(start) {
                        return None;
                    }
                }
                let range = Position::new(index, start)..Position::new(index, end);
                self.is_grapheme_range(&range).then_some(range)
            });
            let exhausted = match direction {
                Direction::Backward => window.start == 0,
                Direction::Forward => window.end == text.len(),
            };
            if found.is_some() || exhausted {
                return found;
            }
            width = width.saturating_mul(2);
        }
    }

    fn find_lines_match(
        &self,
        index: usize,
        lines: &[&str],
        options: SearchOptions,
        boundary: Option<Position>,
        direction: Direction,
    ) -> Option<Range<Position>> {
        let [first_line, middle_lines @ .., last_line] = lines else {
            return None;
        };
        let breaks = middle_lines.len() + 1;
        let first_index = match direction {
            Direction::Backward => index.checked_sub(breaks)?,
            Direction::Forward => index,
        };
        let last_index = first_index.checked_add(breaks)?;
        let range =
            self.multiline_edges(first_index, last_index, first_line, last_line, options)?;
        let within_boundary = boundary.is_none_or(|boundary| match direction {
            Direction::Backward => range.end <= boundary,
            Direction::Forward => range.start >= boundary,
        });
        (within_boundary
            && self.paragraphs()[first_index + 1..last_index]
                .iter()
                .zip(middle_lines)
                .all(|(paragraph, line)| matches_line(paragraph, line, options.case_sensitive))
            && self.valid_search_edges(&range, options))
        .then_some(range)
    }

    fn multiline_edges(
        &self,
        first: usize,
        last: usize,
        first_line: &str,
        last_line: &str,
        options: SearchOptions,
    ) -> Option<Range<Position>> {
        let first_text = self.paragraph(first)?.text();
        let last_text = self.paragraph(last)?.text();
        let (start, end) = if options.case_sensitive {
            let start = first_text.strip_suffix(first_line)?.len();
            if !last_text.starts_with(last_line) {
                return None;
            }
            (start, last_line.len())
        } else {
            (
                lowered_suffix_start(first_text, first_line)?,
                lowered_prefix_end(last_text, last_line)?,
            )
        };
        Some(Position::new(first, start)..Position::new(last, end))
    }

    fn valid_search_edges(&self, range: &Range<Position>, options: SearchOptions) -> bool {
        self.is_grapheme_range(range) && (!options.whole_word || self.has_word_edges(range))
    }

    fn visit_paragraph_matches(
        &self,
        index: usize,
        needle: &str,
        options: SearchOptions,
        mut accept: impl FnMut(Range<Position>) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let paragraph = &self.paragraphs()[index];
        let ascii = paragraph.is_ascii();
        // ASCII text lowercases in place and cannot contain non-ASCII needles.
        if (options.case_sensitive || ascii) && needle.len() > paragraph.text().len()
            || ascii && !needle.is_ascii()
        {
            return ControlFlow::Continue(());
        }
        let visit = |bytes: Range<usize>| {
            let range = Position::new(index, bytes.start)..Position::new(index, bytes.end);
            if self.is_grapheme_range(&range) {
                accept(range).map_continue(|()| true)
            } else {
                ControlFlow::Continue(false)
            }
        };
        if options.case_sensitive && !options.whole_word {
            visit_literals_with_retry(paragraph.text(), needle, visit, |start| {
                paragraph.next_grapheme(start)
            })
        } else {
            visit_text_matches(paragraph.text(), ascii, needle, options, visit)
        }
    }

    fn visit_normalized_matches(
        &self,
        needle: &str,
        options: SearchOptions,
        mut accept: impl FnMut(Range<Position>) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        if let Some(lines) = self.multiline_query(needle, Direction::Forward) {
            return self.visit_multiline_matches(lines, options, accept);
        }
        for index in 0..self.paragraphs().len() {
            self.visit_paragraph_matches(index, needle, options, &mut accept)?;
        }
        ControlFlow::Continue(())
    }

    fn multiline_query<'needle>(
        &self,
        needle: &'needle str,
        direction: Direction,
    ) -> Option<MultilineQuery<'needle>> {
        needle.contains('\n').then(|| {
            let count = self.paragraphs().len();
            let mut lines: Vec<_> = needle.split('\n').take(count + 1).collect();
            // For a short query the direct suffix/whole lines/prefix checks
            // have a bounded cost per candidate. Longer feasible queries use KMP.
            let prefixes = if lines.len() > 16 && lines.len() <= count {
                let last = lines.len() - 1;
                let middle = &mut lines[1..last];
                if direction == Direction::Backward {
                    middle.reverse();
                }
                prefix_lengths(middle)
            } else {
                Vec::new()
            };
            MultilineQuery {
                lines,
                prefixes,
                direction,
            }
        })
    }

    fn visit_multiline_matches(
        &self,
        query: MultilineQuery<'_>,
        options: SearchOptions,
        mut accept: impl FnMut(Range<Position>) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let Some(last_start) = self.paragraphs().len().checked_sub(query.lines.len()) else {
            return ControlFlow::Continue(());
        };
        if query.uses_kmp() {
            return query.visit(
                self,
                options,
                0..last_start + 1,
                Position::default(),
                accept,
            );
        }
        let lines = &query.lines;
        // The first line must be a suffix, so each paragraph has at most one
        // candidate. Resume in the ending paragraph: a match may start there
        // if its first line comes after the previous match's last line.
        let mut end = Position::default();
        for (index, _) in self.paragraphs().windows(lines.len()).enumerate() {
            if index >= end.paragraph
                && let Some(range) =
                    self.find_lines_match(index, lines, options, Some(end), Direction::Forward)
            {
                end = range.end;
                accept(range)?;
            }
        }
        ControlFlow::Continue(())
    }

    #[cfg(test)]
    fn visit_flattened_matches(
        &self,
        needle: &str,
        options: SearchOptions,
        mut accept: impl FnMut(Range<Position>) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let source = if let [paragraph] = self.paragraphs() {
            Cow::Borrowed(paragraph.text())
        } else {
            let mut source = String::with_capacity(
                self.paragraphs()
                    .iter()
                    .map(|paragraph| paragraph.text().len() + 1)
                    .sum(),
            );
            for (index, paragraph) in self.paragraphs().iter().enumerate() {
                if index != 0 {
                    source.push('\n');
                }
                source.push_str(paragraph.text());
            }
            Cow::Owned(source)
        };
        let mut paragraph_starts = Vec::with_capacity(self.paragraphs().len());
        let mut paragraph_start = 0;
        for paragraph in self.paragraphs() {
            paragraph_starts.push(paragraph_start);
            paragraph_start += paragraph.text().len() + 1;
        }
        let position = |byte: usize| {
            let paragraph = paragraph_starts.partition_point(|start| *start <= byte) - 1;
            Position::new(paragraph, byte - paragraph_starts[paragraph])
        };
        visit_text_matches(&source, /* ascii */ false, needle, options, |bytes| {
            let range = position(bytes.start)..position(bytes.end);
            if self.is_grapheme_range(&range) {
                accept(range).map_continue(|()| true)
            } else {
                ControlFlow::Continue(false)
            }
        })
    }
}

fn search_needle(query: &str, options: SearchOptions) -> Cow<'_, str> {
    let query = model::normalize_newlines(query);
    if options.case_sensitive {
        query
    } else {
        Cow::Owned(lowercase(&query))
    }
}

const NEAREST_WINDOW: usize = 1024;

#[derive(Clone, Copy)]
enum ParagraphSearch {
    Literal,
    Nearest,
    Scan,
}

impl ParagraphSearch {
    fn new(needle: &str, options: SearchOptions) -> Self {
        if !never_overlaps(needle) {
            Self::Scan
        } else if options.case_sensitive && !options.whole_word {
            Self::Literal
        } else {
            Self::Nearest
        }
    }
}

// Occurrences of a query without a nonempty border never overlap, so every
// occurrence belongs to the forward match stream and can be found locally.
fn never_overlaps(needle: &str) -> bool {
    has_unique_start(needle) || prefix_lengths(needle.as_bytes()).last() == Some(&0)
}

// KMP matches the whole middle lines. Only the edges of a complete match
// can be partial paragraphs. Backward navigation traverses the reversed
// pattern and document so it can stop at the nearest acceptable match.
struct MultilineQuery<'needle> {
    lines: Vec<&'needle str>,
    prefixes: Vec<usize>,
    direction: Direction,
}

impl MultilineQuery<'_> {
    fn uses_kmp(&self) -> bool {
        !self.prefixes.is_empty()
    }

    fn restore_forward_order(&mut self) {
        if self.direction == Direction::Forward || !self.uses_kmp() {
            return;
        }
        let last = self.lines.len() - 1;
        let middle = &mut self.lines[1..last];
        middle.reverse();
        self.prefixes = prefix_lengths(middle);
        self.direction = Direction::Forward;
    }

    fn can_overlap(&self, needle: &str) -> bool {
        if !self.uses_kmp() {
            return !never_overlaps(needle);
        }
        let [first, middle @ .., last] = self.lines.as_slice() else {
            unreachable!("KMP queries contain middle lines");
        };
        if edges_overlap(first.as_bytes(), last.as_bytes()) {
            return true;
        }
        // An overlap spanning a break aligns a suffix of the complete middle
        // lines with their prefix. Check the partial lines on either side for
        // each such alignment, including zero shared complete lines.
        let mut shared = self.prefixes[middle.len() - 1];
        loop {
            let opposite = middle.len() - 1 - shared;
            let (left, right) = match self.direction {
                Direction::Forward => (opposite, shared),
                Direction::Backward => (shared, opposite),
            };
            if middle[left].ends_with(first) && middle[right].starts_with(last) {
                return true;
            }
            if shared == 0 {
                return false;
            }
            shared = self.prefixes[shared - 1];
        }
    }

    // Candidate ranges index the starting paragraph, even when scanning backward.
    fn find_near(
        &self,
        document: &Document,
        options: SearchOptions,
        boundary: Position,
        wrap: bool,
    ) -> Option<Range<Position>> {
        let breaks = self.lines.len() - 1;
        let count = document.paragraphs().len() - breaks;
        let paragraph = boundary.paragraph;
        let (near, wrapped, wrap_boundary) = match self.direction {
            Direction::Forward => (
                paragraph..count,
                0..(paragraph + 1).min(count),
                Position::default(),
            ),
            Direction::Backward => (
                0..(paragraph + 1).saturating_sub(breaks),
                paragraph.saturating_sub(breaks)..count,
                document.end(),
            ),
        };
        let find = |candidates, boundary| {
            self.visit(document, options, candidates, boundary, ControlFlow::Break)
                .break_value()
        };
        find(near, boundary).or_else(|| wrap.then(|| find(wrapped, wrap_boundary)).flatten())
    }

    fn visit<B>(
        &self,
        document: &Document,
        options: SearchOptions,
        candidates: Range<usize>,
        mut boundary: Position,
        mut accept: impl FnMut(Range<Position>) -> ControlFlow<B>,
    ) -> ControlFlow<B> {
        if candidates.is_empty() {
            return ControlFlow::Continue(());
        }
        let [first, middle @ .., last] = self.lines.as_slice() else {
            unreachable!("long multiline queries contain at least three lines");
        };
        let mut matched = 0;
        let step = |(index, paragraph): (usize, &Arc<Paragraph>)| {
            matched = advance_prefix(matched, &self.prefixes, |index| {
                matches_line(paragraph, middle[index], options.case_sensitive)
            });
            if matched == middle.len() {
                let start = match self.direction {
                    Direction::Forward => index - matched,
                    Direction::Backward => index - 1,
                };
                let end = start + matched + 1;
                matched = self.prefixes[matched - 1];
                // In the forward match stream, skip candidates already
                // covered by the last result before inspecting partial lines.
                if self.direction == Direction::Forward && start < boundary.paragraph {
                    return ControlFlow::Continue(());
                }
                if let Some(range) = document.multiline_edges(start, end, first, last, options) {
                    let next = match self.direction {
                        Direction::Forward if range.start >= boundary => range.end,
                        Direction::Backward if range.end <= boundary => range.start,
                        _ => return ControlFlow::Continue(()),
                    };
                    if document.valid_search_edges(&range, options) {
                        boundary = next;
                        accept(range)?;
                    }
                }
            }
            ControlFlow::Continue(())
        };
        let mut paragraphs = document
            .paragraphs()
            .iter()
            .enumerate()
            .take(candidates.end + middle.len())
            .skip(candidates.start + 1);
        match self.direction {
            Direction::Forward => paragraphs.try_for_each(step),
            Direction::Backward => paragraphs.rev().try_for_each(step),
        }
    }
}

// A border without a hard break is a prefix of the first line and suffix of
// the last. Full middle paragraphs never require a byte-level failure table.
fn edges_overlap(first: &[u8], last: &[u8]) -> bool {
    let limit = first.len().min(last.len());
    let head = &first[..limit];
    let tail = &last[last.len() - limit..];
    if head.is_empty() || !tail.contains(&head[0]) {
        return false;
    }
    if head[0] == tail[limit - 1] || head == tail {
        return true;
    }
    let prefixes = prefix_lengths(head);
    tail.iter().fold(0, |matched, byte| {
        advance_prefix(matched, &prefixes, |index| head[index] == *byte)
    }) > 0
}

fn advance_prefix(
    mut matched: usize,
    prefixes: &[usize],
    mut matches: impl FnMut(usize) -> bool,
) -> usize {
    loop {
        if matches(matched) {
            return matched + 1;
        }
        if matched == 0 {
            return 0;
        }
        matched = prefixes[matched - 1];
    }
}

fn matches_line(paragraph: &Paragraph, line: &str, case_sensitive: bool) -> bool {
    let text = paragraph.text();
    if case_sensitive {
        text == line
    } else if paragraph.is_ascii() {
        text.eq_ignore_ascii_case(line)
    } else {
        lowered_prefix_end(text, line) == Some(text.len())
    }
}

fn lowercase_char(character: char) -> impl DoubleEndedIterator<Item = char> {
    character
        .to_lowercase()
        .map(|lowered| if lowered == 'ς' { 'σ' } else { lowered })
}

fn lowered_prefix_end(text: &str, prefix: &str) -> Option<usize> {
    let mut remaining = prefix.chars();
    let mut end = 0;
    for character in text.chars() {
        if remaining.as_str().is_empty() {
            break;
        }
        for lowered in lowercase_char(character) {
            if remaining.next() != Some(lowered) {
                return None;
            }
        }
        end += character.len_utf8();
    }
    remaining.as_str().is_empty().then_some(end)
}

fn lowered_suffix_start(text: &str, suffix: &str) -> Option<usize> {
    let mut remaining = suffix.chars();
    let mut start = text.len();
    for character in text.chars().rev() {
        if remaining.as_str().is_empty() {
            break;
        }
        for lowered in lowercase_char(character).rev() {
            if remaining.next_back() != Some(lowered) {
                return None;
            }
        }
        start -= character.len_utf8();
    }
    remaining.as_str().is_empty().then_some(start)
}

fn is_word_boundary(paragraph: &Paragraph, byte: usize) -> bool {
    WordBoundaries::for_paragraph(paragraph, byte).contains(byte)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AsciiWordClass {
    Letter,
    Numeric,
    MidLetter,
    MidNumLetQ,
    MidNum,
    ExtendNumLet,
    Other,
}

fn ascii_word_class(byte: u8) -> AsciiWordClass {
    match byte {
        b'A'..=b'Z' | b'a'..=b'z' => AsciiWordClass::Letter,
        b'0'..=b'9' => AsciiWordClass::Numeric,
        b':' => AsciiWordClass::MidLetter,
        b'.' | b'\'' => AsciiWordClass::MidNumLetQ,
        b',' | b';' => AsciiWordClass::MidNum,
        b'_' => AsciiWordClass::ExtendNumLet,
        _ => AsciiWordClass::Other,
    }
}

// The UAX #29 word rules inspect at most two characters on each side, and an
// ASCII character is never Extend, Format, or ZWJ. Non-ASCII context is left
// to the segmenter.
fn ascii_word_boundary(text: &[u8], byte: usize) -> Option<bool> {
    use AsciiWordClass::{ExtendNumLet, Letter, MidLetter, MidNum, MidNumLetQ, Numeric};

    let (Some(&before), Some(&after)) = (
        byte.checked_sub(1).and_then(|index| text.get(index)),
        text.get(byte),
    ) else {
        return Some(true);
    };
    if !before.is_ascii() || !after.is_ascii() {
        return None;
    }
    let newline = |byte: u8| matches!(byte, b'\n' | b'\x0b' | b'\x0c' | b'\r');
    if before == b'\r' && after == b'\n' {
        return Some(false);
    }
    if newline(before) || newline(after) {
        return Some(true);
    }
    if before == b' ' && after == b' ' {
        return Some(false);
    }
    let neighbor = |index: Option<usize>| match index.and_then(|index| text.get(index)) {
        None => Some(None),
        Some(&byte) if byte.is_ascii() => Some(Some(ascii_word_class(byte))),
        Some(_) => None,
    };
    let joined = match (ascii_word_class(before), ascii_word_class(after)) {
        (Letter | Numeric, Letter | Numeric)
        | (Letter | Numeric | ExtendNumLet, ExtendNumLet)
        | (ExtendNumLet, Letter | Numeric) => true,
        (Letter, MidLetter | MidNumLetQ) => neighbor(Some(byte + 1))? == Some(Letter),
        (MidLetter | MidNumLetQ, Letter) => neighbor(byte.checked_sub(2))? == Some(Letter),
        (Numeric, MidNum | MidNumLetQ) => neighbor(Some(byte + 1))? == Some(Numeric),
        (MidNum | MidNumLetQ, Numeric) => neighbor(byte.checked_sub(2))? == Some(Numeric),
        _ => false,
    };
    Some(!joined)
}

struct WordBoundaries<'text> {
    paragraph: &'text Paragraph,
    start: usize,
    boundaries: Vec<usize>,
    remaining: Option<UWordBoundIndices<'text>>,
}

impl<'text> WordBoundaries<'text> {
    // Nearby navigation can revisit a long unspaced prefix on every press.
    // Cache expensive scans in the paragraph's compact word-boundary index.
    fn for_paragraph(paragraph: &'text Paragraph, start: usize) -> Self {
        Self {
            paragraph,
            start,
            boundaries: Vec::new(),
            remaining: None,
        }
    }

    fn index_after_scan(&mut self, byte: usize, scanned: usize) -> Option<bool> {
        if scanned > 4_096 && scanned >= self.paragraph.text().len() / 2 {
            let boundary = self.paragraph.indexed_word_boundary(byte);
            self.boundaries = Vec::new();
            self.remaining = None;
            Some(boundary)
        } else {
            None
        }
    }

    fn contains(&mut self, byte: usize) -> bool {
        debug_assert!(
            byte >= self.start,
            "word checks start at or before every byte they query, but byte {byte} \
             precedes {}",
            self.start
        );
        let text = self.paragraph.text();
        if let Some(boundary) = ascii_word_boundary(text.as_bytes(), byte) {
            return boundary;
        }
        if let Some(boundary) = self.paragraph.cached_word_boundary(byte) {
            return boundary;
        }
        // ASCII edges can be decided locally. Defer even finding a safe
        // restart until Unicode context is needed: searching backwards for a
        // space would otherwise scan an unrelated, possibly huge prefix.
        if self.remaining.is_none() {
            self.start = word_restart(text, self.start);
            if let Some(boundary) = self.index_after_scan(byte, byte - self.start) {
                return boundary;
            }
            self.remaining = Some(text[self.start..].split_word_bound_indices());
        }
        while self
            .boundaries
            .last()
            .is_none_or(|boundary| *boundary < byte)
        {
            let Some((boundary, segment)) = self.remaining.as_mut().and_then(Iterator::next) else {
                return false;
            };
            if let Some(boundary) = self.index_after_scan(byte, boundary + segment.len()) {
                return boundary;
            }
            self.boundaries.push(self.start + boundary);
        }
        self.boundaries.binary_search(&byte).is_ok()
    }
}

struct LowercaseOffsets<'source> {
    source: &'source str,
    folded: bool,
    changes: Option<Vec<LengthChange>>,
}

// A character whose lowercase mapping has a different UTF-8 length, with its byte range in
// the lowercased text and in the source.
struct LengthChange {
    lowered: Range<usize>,
    original: Range<usize>,
}

impl<'source> LowercaseOffsets<'source> {
    fn new(source: &'source str, folded: bool) -> Self {
        Self {
            source,
            folded,
            changes: None,
        }
    }

    fn original(&mut self, byte: usize) -> Option<usize> {
        let source = self.source;
        let folded = self.folded;
        let changes = self.changes.get_or_insert_with(|| {
            let mut changes = Vec::new();
            if folded && !source.is_ascii() {
                let mut lower_byte = 0;
                for (byte, character) in source.char_indices() {
                    let lowered_len = if character.is_ascii() {
                        1
                    } else {
                        character.to_lowercase().map(char::len_utf8).sum::<usize>()
                    };
                    if lowered_len != character.len_utf8() {
                        changes.push(LengthChange {
                            lowered: lower_byte..lower_byte + lowered_len,
                            original: byte..byte + character.len_utf8(),
                        });
                    }
                    lower_byte += lowered_len;
                }
            }
            changes
        });
        let index = changes.partition_point(|change| change.lowered.end <= byte);
        if let Some(change) = changes.get(index)
            && byte >= change.lowered.start
        {
            return (byte == change.lowered.start).then_some(change.original.start);
        }
        let original = index.checked_sub(1).map_or(byte, |index| {
            let change = &changes[index];
            byte - change.lowered.end + change.original.end
        });
        source.is_char_boundary(original).then_some(original)
    }
}

fn find_literal<T>(
    source: &str,
    needle: &str,
    direction: Direction,
    mut accept: impl FnMut(Range<usize>) -> Option<T>,
) -> Option<T> {
    match direction {
        Direction::Backward => source
            .rmatch_indices(needle)
            .find_map(|(start, _)| accept(start..start + needle.len())),
        Direction::Forward => source
            .match_indices(needle)
            .find_map(|(start, _)| accept(start..start + needle.len())),
    }
}

fn has_unique_start(needle: &str) -> bool {
    needle
        .chars()
        .next()
        .is_some_and(|first| !needle[first.len_utf8()..].contains(first))
}

fn lowercase(text: &str) -> String {
    let lowered = text.to_lowercase();
    if lowered.contains('ς') {
        lowered.replace('ς', "σ")
    } else {
        lowered
    }
}

// Candidate starts advance monotonically, as do candidate ends, even when a
// rejected literal overlaps the next one. Keep a separate segmenter for each
// edge so neither needs to retain a table of previously visited boundaries.
fn forward_word_boundary<'text>(
    text: &'text str,
    words: &mut UWordBounds<'text>,
    other: &UWordBounds<'text>,
    byte: usize,
) -> bool {
    if let Some(boundary) = ascii_word_boundary(text.as_bytes(), byte) {
        return boundary;
    }
    let tail_len = text.len() - byte;
    // Once this edge catches up to the other cursor, resume its segmentation.
    if (tail_len..words.as_str().len()).contains(&other.as_str().len()) {
        *words = other.clone();
    }
    let unscanned = words.as_str();
    let gap = unscanned.len().saturating_sub(tail_len);
    if gap > 4_096 {
        // Search only the unvisited gap for a safe Unicode restart. Without a
        // suitable space the current segmenter retains the full word context.
        let restart = word_restart(unscanned, gap);
        if restart != 0 {
            *words = unscanned[restart..].split_word_bounds();
        }
    }
    while words.as_str().len() > tail_len {
        words.next();
    }
    words.as_str().len() == tail_len
}

fn visit_text_matches(
    source: &str,
    ascii: bool,
    needle: &str,
    options: SearchOptions,
    mut accept: impl FnMut(Range<usize>) -> ControlFlow<(), bool>,
) -> ControlFlow<()> {
    if options.case_sensitive && !options.whole_word {
        return visit_literals(source, needle, accept);
    }
    let searched = if options.case_sensitive {
        Cow::Borrowed(source)
    } else if ascii {
        Cow::Owned(source.to_ascii_lowercase())
    } else {
        Cow::Owned(lowercase(source))
    };
    if needle.len() > searched.len() {
        return ControlFlow::Continue(());
    }
    let mut starts = source.split_word_bounds();
    let mut ends = starts.clone();
    let mut offsets = LowercaseOffsets::new(source, !options.case_sensitive && !ascii);
    visit_literals(&searched, needle, |bytes| {
        let range = offsets
            .original(bytes.start)
            .zip(offsets.original(bytes.end))
            .and_then(|(start, end)| {
                (!options.whole_word
                    || (forward_word_boundary(source, &mut starts, &ends, start)
                        && forward_word_boundary(source, &mut ends, &starts, end)))
                .then_some(start..end)
            });
        if let Some(range) = range {
            accept(range)
        } else {
            ControlFlow::Continue(false)
        }
    })
}

fn visit_literals(
    source: &str,
    needle: &str,
    accept: impl FnMut(Range<usize>) -> ControlFlow<(), bool>,
) -> ControlFlow<()> {
    visit_literals_with_retry(source, needle, accept, |start| {
        start
            + source[start..]
                .chars()
                .next()
                .expect("a nonempty query matched a character")
                .len_utf8()
    })
}

fn visit_literals_with_retry(
    source: &str,
    needle: &str,
    mut accept: impl FnMut(Range<usize>) -> ControlFlow<(), bool>,
    mut retry: impl FnMut(usize) -> usize,
) -> ControlFlow<()> {
    // Keep sparse rejection cases on the standard matcher, but bound both the
    // number of restarts and repeated needle preprocessing before using KMP.
    let mut retries = 2;
    let mut retry_bytes = 64;
    let mut may_overlap = None;
    let mut offset = 0;
    'search: loop {
        for (relative, _) in source[offset..].match_indices(needle) {
            let start = offset + relative;
            if accept(start..start + needle.len())?
                || !*may_overlap.get_or_insert_with(|| !has_unique_start(needle))
            {
                continue;
            }
            let next = retry(start);
            if needle.len() > source.len() - next {
                return ControlFlow::Continue(());
            }
            if retries > 0 && needle.len() <= retry_bytes {
                retries -= 1;
                retry_bytes -= needle.len();
                offset = next;
                continue 'search;
            }
            let prefixes = prefix_lengths(needle.as_bytes());
            if prefixes[needle.len() - 1] == 0 {
                // Without a shared prefix and suffix, occurrences cannot overlap.
                may_overlap = Some(false);
                continue;
            }
            return visit_overlapping_literals(source, needle, next, &prefixes, accept);
        }
        return ControlFlow::Continue(());
    }
}

fn prefix_lengths<T: PartialEq>(needle: &[T]) -> Vec<usize> {
    // KMP failure table: each entry is the longest proper prefix ending here.
    let mut prefixes = vec![0; needle.len()];
    let mut matched = 0;
    for index in 1..needle.len() {
        matched = advance_prefix(matched, &prefixes, |previous| {
            needle[index] == needle[previous]
        });
        prefixes[index] = matched;
    }
    prefixes
}

fn visit_overlapping_literals(
    source: &str,
    needle: &str,
    offset: usize,
    prefixes: &[usize],
    mut accept: impl FnMut(Range<usize>) -> ControlFlow<(), bool>,
) -> ControlFlow<()> {
    let needle = needle.as_bytes();
    let mut matched = 0;
    for (index, &byte) in source.as_bytes()[offset..].iter().enumerate() {
        matched = advance_prefix(matched, prefixes, |previous| byte == needle[previous]);
        if matched == needle.len() {
            let end = offset + index + 1;
            // Accepted matches consume their bytes; rejected matches may overlap.
            matched = if accept(end - needle.len()..end)? {
                0
            } else {
                prefixes[matched - 1]
            };
        }
    }
    ControlFlow::Continue(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;

    #[test]
    fn selected_match_membership_agrees_with_the_forward_stream() {
        let long_query = format!("!{}?", "ab\n".repeat(20));
        let overlapping_query = "ab\n".repeat(18);
        let texts = [
            String::new(),
            "banana ababa ana aaa".to_owned(),
            "Foo cat category foo".to_owned(),
            "İi\u{307} KȺ ΟΣΑ ΣΣ".to_owned(),
            "a e\u{301} 👩\u{200d}💻 \u{600}a".to_owned(),
            "ab\u{2028}cd\nEF\n\nİ".to_owned(),
            "🇺🇸🇨🇦🇺🇸".to_owned(),
            format!("prefix\n{long_query}\nsuffix"),
            "ab\n".repeat(21),
            "\n\n\n".to_owned(),
        ];
        let queries = [
            "",
            "a",
            "aa",
            "ana",
            "aba",
            "foo",
            "cat",
            "i",
            "i\u{307}",
            "İ",
            "kⱥ",
            "οσ",
            "Σ",
            "e\u{301}",
            "💻",
            "🇺🇸",
            "\n",
            "\r\n",
            "\u{2029}",
            "\u{2028}",
            "cd\nEF",
            "cd\r\nEF",
            "EF\n\nİ",
            &long_query,
            &overlapping_query,
            // Only the normalized needles ("aa" and "\n\n") overlap themselves.
            "Aa",
            "\r\n\n",
        ];
        for text in texts {
            let document = Document::from_text(&text);
            let positions: Vec<_> = document
                .paragraphs()
                .iter()
                .enumerate()
                .flat_map(|(index, paragraph)| {
                    paragraph
                        .text()
                        .grapheme_indices(true)
                        .map(move |(byte, _)| Position::new(index, byte))
                        .chain([Position::new(index, paragraph.text().len())])
                })
                .collect();
            for query in queries {
                for case_sensitive in [true, false] {
                    for whole_word in [true, false] {
                        let options = SearchOptions {
                            case_sensitive,
                            whole_word,
                        };
                        let matches = document.find(query, options);
                        for (index, &start) in positions.iter().enumerate() {
                            for &end in &positions[index..] {
                                let range = start..end;
                                assert_eq!(
                                    document.is_match(query, options, &range),
                                    matches.contains(&range),
                                    "{text:?}, {query:?}, {options:?}, {range:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn ascii_word_checks_defer_prefix_scanning_until_unicode_context_is_needed() {
        let prefix = "!?".repeat(4_096);
        let text = format!("{prefix}a:β γ");
        let document = Document::from_text(&text);
        let paragraph = &document.paragraphs()[0];
        let start = prefix.len() - 2;
        let mut words = WordBoundaries::for_paragraph(paragraph, start);
        assert!(words.contains(start + 1));
        assert!(words.contains(text.len()));
        assert!(words.remaining.is_none());
        assert!(paragraph.cached_word_boundary(start).is_none());
        assert_eq!(words.start, start);

        // The colon and Greek letter belong to the same word as the preceding
        // ASCII letter. Restarting at the queried edge would invent a boundary.
        assert!(!words.contains(prefix.len() + 2));
        assert!(paragraph.cached_word_boundary(start).is_some());
        assert_eq!(words.start, 0);
        let boundaries: Vec<_> = text
            .split_word_bound_indices()
            .map(|(byte, _)| byte)
            .chain([text.len()])
            .collect();
        for byte in text[start..]
            .char_indices()
            .map(|(byte, _)| start + byte)
            .chain([text.len()])
        {
            assert_eq!(
                words.contains(byte),
                boundaries.binary_search(&byte).is_ok()
            );
        }
    }

    #[test]
    fn nearby_word_search_indexes_only_expensive_unicode_scans() {
        let options = SearchOptions {
            whole_word: true,
            ..SearchOptions::default()
        };
        for token in ["你好", "😀!?"] {
            let document = Document::from_text(&token.repeat(2_000));
            let paragraph = &document.paragraphs()[0];
            let query = &token[..token.chars().next().unwrap().len_utf8()];
            let mut editor = Editor::new(document.clone());
            assert!(editor.find_next(query, options, SearchWrap::Stop).unwrap());
            assert!(paragraph.cached_word_boundary(0).is_none());

            editor
                .set_selection(crate::Selection::caret(document.end()))
                .unwrap();
            assert!(
                editor
                    .find_previous(query, options, SearchWrap::Stop)
                    .unwrap()
            );
            assert_eq!(paragraph.cached_word_boundary(0), Some(true));
            let expected: Vec<_> = paragraph
                .text()
                .split_word_bound_indices()
                .map(|(byte, _)| byte)
                .chain([paragraph.text().len()])
                .collect();
            let mut words = WordBoundaries::for_paragraph(paragraph, 0);
            for byte in paragraph.text().char_indices().map(|(byte, _)| byte) {
                assert_eq!(words.contains(byte), expected.binary_search(&byte).is_ok());
            }
            assert!(words.remaining.is_none());
            assert!(words.boundaries.is_empty());
        }

        let ascii = Document::from_text(&"!?".repeat(10_000));
        let paragraph = &ascii.paragraphs()[0];
        let mut words = WordBoundaries::for_paragraph(paragraph, paragraph.text().len() - 3);
        assert!(words.contains(paragraph.text().len() - 2));
        assert!(paragraph.cached_word_boundary(0).is_none());

        // A short query near the start can still scan a huge, unbroken word.
        // Cache that work too, without indexing an unrelated large suffix.
        for text in ["é".repeat(10_000), format!("é {}", "你".repeat(10_000))] {
            let document = Document::from_text(&text);
            let paragraph = &document.paragraphs()[0];
            let mut words = WordBoundaries::for_paragraph(paragraph, 0);
            words.contains(2);
            assert_eq!(
                paragraph.cached_word_boundary(0).is_some(),
                !text.contains(' ')
            );
        }

        let text = "你好".repeat(2_000);
        let document = Document::from_text(&format!("{text}\n{text}"));
        let mut editor = Editor::new(document.clone());
        assert!(
            editor
                .find_next("好\n你", options, SearchWrap::Stop)
                .unwrap()
        );
        assert_eq!(document.paragraphs()[0].cached_word_boundary(0), Some(true));
        assert!(document.paragraphs()[1].cached_word_boundary(0).is_none());
    }

    fn assert_matches_flattened_stream(
        document: &Document,
        query: &str,
        options: SearchOptions,
    ) -> Option<usize> {
        let needle = search_needle(query, options);
        if !needle.contains('\n') {
            return None;
        }
        let mut matches = Vec::new();
        let _ = document.visit_flattened_matches(&needle, options, |range| {
            matches.push(range);
            ControlFlow::Continue(())
        });
        let context = format!("{query:?} {options:?} {:?}", document.plain_text());
        assert_eq!(document.find(query, options), matches, "{context}");
        let positions: Vec<_> = document
            .paragraphs()
            .iter()
            .enumerate()
            .flat_map(|(index, paragraph)| {
                (0..=paragraph.grapheme_count()).map(move |grapheme| {
                    Position::new(index, paragraph.byte_from_grapheme(grapheme).unwrap())
                })
            })
            .collect();
        let mut ranges: Vec<_> = positions
            .iter()
            .map(|&position| position..position)
            .collect();
        for (index, &position) in positions.iter().enumerate() {
            let other = positions[(index * 7 + 3) % positions.len()];
            ranges.push(position.min(other)..position.max(other));
        }
        ranges.extend(matches.iter().cloned());
        for range in ranges {
            assert_eq!(
                document.is_match(query, options, &range),
                matches.iter().find(|item| item.start >= range.start) == Some(&range),
                "{range:?} {context}"
            );
            for wrap in [SearchWrap::Stop, SearchWrap::Around] {
                for direction in [Direction::Forward, Direction::Backward] {
                    let expected = match direction {
                        Direction::Backward => matches
                            .iter()
                            .rev()
                            .find(|item| item.end <= range.start)
                            .or_else(|| {
                                (wrap == SearchWrap::Around)
                                    .then(|| matches.last())
                                    .flatten()
                            }),
                        Direction::Forward => matches
                            .iter()
                            .find(|item| item.start >= range.end)
                            .or_else(|| {
                                (wrap == SearchWrap::Around)
                                    .then(|| matches.first())
                                    .flatten()
                            }),
                    };
                    assert_eq!(
                        document.find_relative(query, options, range.clone(), wrap, direction),
                        expected.cloned(),
                        "{range:?} wrap={wrap:?} {direction:?} {context}"
                    );
                }
            }
        }
        Some(matches.len())
    }

    #[test]
    fn overlapping_multiline_matches_resume_at_the_previous_end() {
        for (text, query) in [
            ("a\na\na\na", "a\na"),
            ("É\néé\né", "É\né"),
            ("\n\n\n\n\n", "\r\n\n"),
            ("i\u{307}\nİ İ\ni\u{307}", "İ\nİ"),
        ] {
            let document = Document::from_text(text);
            for whole_word in [false, true] {
                let options = SearchOptions {
                    case_sensitive: false,
                    whole_word,
                };
                let count = assert_matches_flattened_stream(&document, query, options);
                if !whole_word {
                    assert_eq!(count, Some(2), "{text:?}, {query:?}");
                }
            }
        }
    }

    #[test]
    fn long_query_line_borders_agree_with_literal_borders_in_both_directions() {
        let document = Document::from_text(&"\n".repeat(50));
        let parts = ["", "a", "ab", "ba", "abc", "β", "aβ", "βa", "İ"];
        for first in parts {
            for last in parts {
                for unit in ["\na", "a\na", "a\nab\na", "aβ\nβa\n", "ab\nβ\nba"] {
                    for middle in [
                        unit.repeat(17),
                        format!("{unit}\n{}\n{unit}", "q\n".repeat(16)),
                    ] {
                        let query = format!("{first}\n{middle}\n{last}");
                        let bordered = (1..query.len())
                            .any(|start| query.as_bytes().starts_with(&query.as_bytes()[start..]));
                        for direction in [Direction::Backward, Direction::Forward] {
                            let pattern = document.multiline_query(&query, direction).unwrap();
                            assert_eq!(
                                pattern.can_overlap(&query),
                                bordered,
                                "{query:?} {direction:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn long_directional_search_leaves_word_boundaries_before_the_caret_lazy() {
        let middle = "K\n".repeat(17);
        let first = "你好".repeat(5_000);
        let text = format!("{first}\n{middle}END\n好\n{middle}END·好\n{middle}END");
        let doc = Document::from_text(&text);
        let query = format!("好\n{}end", "k\n".repeat(17));
        let options = SearchOptions {
            case_sensitive: false,
            whole_word: true,
        };
        // Both nearby matches share a paragraph and respect its byte boundary.
        let caret = Position::new(37, "END·".len());
        for (direction, start) in [(Direction::Backward, 19), (Direction::Forward, 37)] {
            let found = doc
                .find_relative(&query, options, caret..caret, SearchWrap::Stop, direction)
                .unwrap();
            assert_eq!(found.start.paragraph, start);
            assert!(doc.paragraphs()[0].cached_word_boundary(0).is_none());
        }
    }

    #[test]
    fn long_multiline_queries_match_the_flattened_unicode_oracle() {
        let middle = "İ\nΣ\n\nK\n".repeat(4);
        let lowered = "i\u{307}\nσ\n\nk\n".repeat(4);
        let query = format!("a\n{lowered}a");
        let text = format!("ba\n{middle}aa\n{lowered}a\u{301}\n{middle}a");
        let empty_lines = "\n".repeat(17);
        let unbordered = format!("é\n{lowered}β");
        let shared_end = format!(" é\n{middle}βé\n{lowered}β·é\n{middle}β");
        for (source, query) in [
            (text.as_str(), query.as_str()),
            (shared_end.as_str(), unbordered.as_str()),
            (text.as_str(), empty_lines.as_str()),
            (&"\n".repeat(39), empty_lines.as_str()),
            ("one paragraph", query.as_str()),
        ] {
            let document = Document::from_text(source);
            for case_sensitive in [true, false] {
                for whole_word in [true, false] {
                    assert_matches_flattened_stream(
                        &document,
                        query,
                        SearchOptions {
                            case_sensitive,
                            whole_word,
                        },
                    )
                    .unwrap();
                }
            }
        }
    }

    #[test]
    fn paragraph_wise_cross_paragraph_queries_agree_with_the_flattened_match_stream() {
        let tokens = [
            "a",
            "A",
            "aa",
            " ",
            "-",
            "'",
            ".",
            "\n",
            "\n\n",
            "İ",
            "i\u{307}",
            "\u{307}",
            "\u{212A}",
            "K",
            "k",
            "Σ",
            "σ",
            "ς",
            "ΟΣ",
            "e\u{301}",
            "\u{301}",
            "é",
            "🇨🇺",
            "🇺",
            "👩\u{200d}💻",
            "\u{200d}",
            "\u{2028}",
            "word",
            "Word",
            "你好",
            "\u{0600}",
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize
        };
        let (mut checked, mut matched) = (0, 0);
        for _ in 0..40 {
            let mut source = String::new();
            for _ in 0..next() % 40 {
                source.push_str(tokens[next() % tokens.len()]);
            }
            let mut editor = Editor::from_text(&source);
            for round in 0..4 {
                let positions: Vec<_> = editor
                    .document()
                    .paragraphs()
                    .iter()
                    .enumerate()
                    .flat_map(|(index, paragraph)| {
                        (0..=paragraph.grapheme_count()).map(move |grapheme| {
                            Position::new(index, paragraph.byte_from_grapheme(grapheme).unwrap())
                        })
                    })
                    .collect();
                match round {
                    0 => {}
                    3 => {
                        editor.undo();
                    }
                    _ => {
                        let start = positions[next() % positions.len()];
                        let end = positions[next() % positions.len()];
                        editor
                            .replace_range(
                                start.min(end)..start.max(end),
                                tokens[next() % tokens.len()],
                            )
                            .unwrap();
                    }
                }
                let document = editor.document();
                let text = document
                    .paragraphs()
                    .iter()
                    .map(|paragraph| paragraph.text())
                    .collect::<Vec<_>>()
                    .join("\n");
                let chars: Vec<_> = text
                    .char_indices()
                    .map(|(byte, _)| byte)
                    .chain([text.len()])
                    .collect();
                let mut queries: Vec<String> = [
                    "\n",
                    "a\n",
                    "\na",
                    "\n\na",
                    "\r\n",
                    "a\u{2029}",
                    "ς\n",
                    "Σ\n\nk",
                ]
                .map(String::from)
                .into();
                for _ in 0..8 {
                    let start = chars[next() % chars.len()];
                    let end = chars[next() % chars.len()];
                    let slice = &text[start.min(end)..start.max(end)];
                    queries.extend([slice.to_string(), slice.to_uppercase()]);
                }
                for _ in 0..4 {
                    queries.push(
                        (0..=next() % 4)
                            .map(|_| tokens[next() % tokens.len()])
                            .collect(),
                    );
                }
                for query in &queries {
                    for case_sensitive in [true, false] {
                        for whole_word in [false, true] {
                            let options = SearchOptions {
                                case_sensitive,
                                whole_word,
                            };
                            if let Some(count) =
                                assert_matches_flattened_stream(document, query, options)
                            {
                                checked += 1;
                                matched += usize::from(count > 0);
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 5_000 && matched > 1_000, "{checked} {matched}");
    }

    #[test]
    fn case_insensitive_middle_lines_must_fill_their_paragraphs() {
        for (text, query) in [
            ("Σ\nfoo\nk", "σ\n\nk"),
            ("Σ\nfoo\nk", "Σ\n\nK"),
            ("x\nABC\ny", "x\nab\ny"),
            ("x\nABC\ny", "X\nAB\nY"),
            ("x\nABC\n\ny", "x\nab\n\ny"),
            ("x\nABC\nDEF\ny", "x\nabc\nde\ny"),
            ("x\nΟΣΑ\ny", "x\nοσ\ny"),
            ("x\n\u{212A}\u{212A}\ny", "x\nk\ny"),
        ] {
            let document = Document::from_text(text);
            for whole_word in [false, true] {
                for case_sensitive in [false, true] {
                    let options = SearchOptions {
                        case_sensitive,
                        whole_word,
                    };
                    assert_eq!(
                        assert_matches_flattened_stream(&document, query, options),
                        Some(0),
                        "{text:?} {query:?} {options:?}"
                    );
                }
            }
        }
        let insensitive = SearchOptions {
            case_sensitive: false,
            whole_word: false,
        };
        for (text, query) in [
            ("Σ\n\nk", "σ\n\nk"),
            ("x\nABC\ny", "x\nabc\ny"),
            ("x\nΟΣ\ny", "X\nος\nY"),
        ] {
            let document = Document::from_text(text);
            assert_eq!(
                assert_matches_flattened_stream(&document, query, insensitive),
                Some(1),
                "{text:?} {query:?}"
            );
        }
    }

    #[test]
    fn ascii_word_boundaries_agree_with_unicode_segmentation() {
        let tokens = [
            "a", "Z", "q", "0", "7", ":", ".", "'", ",", ";", "_", " ", "  ", "\t", "\"", "-", "!",
            "/", "\n", "\r", "\r\n", "\x0b", "\x0c", "\u{301}", "\u{200d}", "\u{ad}", "é",
            "\u{5d0}", "\u{3000}", "\u{30a2}", "🇺", "👩", "’", "·", "\u{85}", "\u{2028}", "١",
        ];
        let ascii = tokens.iter().take_while(|token| token.is_ascii()).count();
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut decided = 0;
        for length in 0..4_000 {
            let mut text = String::new();
            for _ in 0..length % 24 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let index = if seed.is_multiple_of(4) {
                    (seed >> 8) as usize % tokens.len()
                } else {
                    (seed >> 8) as usize % ascii
                };
                text.push_str(tokens[index]);
            }
            let boundaries: Vec<_> = text
                .split_word_bound_indices()
                .map(|(byte, _)| byte)
                .chain([text.len()])
                .collect();
            let char_boundaries = (0..=text.len()).filter(|byte| text.is_char_boundary(*byte));
            for byte in char_boundaries {
                if let Some(boundary) = ascii_word_boundary(text.as_bytes(), byte) {
                    decided += 1;
                    assert_eq!(
                        boundary,
                        boundaries.binary_search(&byte).is_ok(),
                        "byte {byte} in {text:?}"
                    );
                }
            }
        }
        assert!(decided > 10_000, "{decided}");
    }

    #[test]
    fn word_restarts_agree_with_segmentation_from_the_paragraph_start() {
        let tokens = [
            " ", " ", " ", "  ", "a", "Z", "7", ":", ".", "'", ",", "\"", "_", "-", "\t", "é",
            "\u{301}", "\u{200d}", "\u{ad}", "\u{2060}", "\u{3000}", "\u{a0}", "\u{2028}",
            "\u{85}", "Мир", "λόγ", "你好", "\u{5d0}", "\u{5f3}", "\u{30a2}", "\u{ff70}", "🇺",
            "👩", "١", "’", "·", "🏻",
        ];
        let mut seed = 0x3c6e_f372_fe94_f82b_u64;
        let (mut checked, mut unicode_restarts) = (0, 0);
        for length in 0..3_000 {
            let mut text = String::new();
            for _ in 0..length % 32 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                text.push_str(tokens[(seed >> 8) as usize % tokens.len()]);
            }
            let boundaries: Vec<_> = text
                .split_word_bound_indices()
                .map(|(byte, _)| byte)
                .chain([text.len()])
                .collect();
            let char_boundaries = (0..=text.len()).filter(|byte| text.is_char_boundary(*byte));
            let document = Document::from_text(&text);
            for byte in char_boundaries {
                let restart = word_restart(&text, byte);
                assert!(restart <= byte, "byte {byte} in {text:?}");
                let restarted: Vec<_> = text[restart..]
                    .split_word_bound_indices()
                    .map(|(offset, _)| restart + offset)
                    .chain([text.len()])
                    .collect();
                assert_eq!(
                    restarted,
                    boundaries[boundaries.partition_point(|boundary| *boundary < restart)..],
                    "restart {restart} for byte {byte} in {text:?}"
                );
                assert_eq!(
                    WordBoundaries::for_paragraph(&document.paragraphs()[0], byte).contains(byte),
                    boundaries.binary_search(&byte).is_ok(),
                    "byte {byte} in {text:?}"
                );
                checked += 1;
                unicode_restarts +=
                    usize::from(restart > 0 && !text.as_bytes()[restart].is_ascii());
            }
        }
        assert!(
            checked > 50_000 && unicode_restarts > 10_000,
            "{checked} {unicode_restarts}"
        );
    }

    #[test]
    fn word_restarts_stay_near_the_caret_in_spaced_non_latin_prose() {
        for word in [
            "слово",
            "Λόγος",
            "你好",
            "\u{5d0}\u{5d1}",
            "\u{30a2}\u{30a4}",
            "١٢",
        ] {
            let text = format!("{word} ").repeat(1_000);
            let byte = text.len() / 2;
            let restart = word_restart(&text, byte);
            assert!(restart > 0 && byte - restart <= word.len() + 1, "{word:?}");
            assert_eq!(&text[restart - 1..restart], " ", "{word:?}");
        }
        for (joined, restart) in [
            ("\u{301}", 0),
            ("\u{200d}", 0),
            ("\u{ad}", 0),
            ("\u{3000}", 0),
            (" ", "слово  ".len()),
            ("\u{a0}", "слово ".len()),
        ] {
            let text = format!("слово {joined}слово");
            assert_eq!(word_restart(&text, text.len()), restart, "{joined:?}");
        }
    }
}
