use std::{
    borrow::Cow,
    ops::{ControlFlow, Range},
};

use unicode_segmentation::{UWordBoundIndices, UnicodeSegmentation};

use crate::document::normalize_newlines;
use crate::{Document, Position};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_sensitive: bool,
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

impl Document {
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
        if query.is_empty() || range.is_empty() {
            return false;
        }
        let needle = search_needle(query, options);
        if !needle.contains('\n') {
            return range.start.paragraph == range.end.paragraph
                && self
                    .find_paragraph_match(
                        range.start.paragraph,
                        &needle,
                        options,
                        Some(range.start),
                        false,
                        ParagraphSearch::new(&needle, options),
                    )
                    .is_some_and(|found| found == *range);
        }
        let mut found = false;
        let _ = self.visit_normalized_matches(&needle, options, |candidate| {
            if candidate.start < range.start {
                return ControlFlow::Continue(());
            }
            found = candidate == *range;
            ControlFlow::Break(())
        });
        found
    }

    pub(crate) fn find_relative(
        &self,
        query: &str,
        options: SearchOptions,
        selection: Range<Position>,
        wrap: bool,
        backward: bool,
    ) -> Option<Range<Position>> {
        if query.is_empty() {
            return None;
        }
        let needle = search_needle(query, options);
        if needle.contains('\n') {
            let mut found = None;
            let mut wrapped = None;
            let _ = self.visit_normalized_matches(&needle, options, |range| {
                if backward {
                    if range.end <= selection.start {
                        found = Some(range);
                    } else if found.is_some() || !wrap {
                        return ControlFlow::Break(());
                    } else {
                        wrapped = Some(range);
                    }
                } else {
                    if range.start >= selection.end {
                        found = Some(range);
                        return ControlFlow::Break(());
                    }
                    if wrap && wrapped.is_none() {
                        wrapped = Some(range);
                    }
                }
                ControlFlow::Continue(())
            });
            return found.or(wrapped);
        }

        let strategy = ParagraphSearch::new(&needle, options);
        let boundary = if backward {
            selection.start
        } else {
            selection.end
        };
        let len = self.paragraphs().len();
        let start = boundary.paragraph;
        let search = |index: usize, bounded: bool| {
            self.find_paragraph_match(
                index,
                &needle,
                options,
                bounded.then_some(boundary),
                backward,
                strategy,
            )
        };
        if backward {
            (0..=start)
                .rev()
                .find_map(|index| search(index, index == start))
                .or_else(|| {
                    wrap.then(|| (start..len).rev().find_map(|index| search(index, false)))
                        .flatten()
                })
        } else {
            (start..len)
                .find_map(|index| search(index, index == start))
                .or_else(|| {
                    wrap.then(|| (0..=start).find_map(|index| search(index, false)))
                        .flatten()
                })
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
        backward: bool,
        strategy: ParagraphSearch,
    ) -> Option<Range<Position>> {
        let text = self.paragraphs()[index].text();
        if boundary.is_some_and(|position| {
            if backward {
                position.byte == 0
            } else {
                position.byte == text.len()
            }
        }) {
            return None;
        }
        let edge = boundary.map_or(if backward { text.len() } else { 0 }, |position| {
            position.byte
        });
        match strategy {
            ParagraphSearch::Literal => {
                let window = if backward { 0..edge } else { edge..text.len() };
                find_literal(&text[window.clone()], needle, backward, |bytes| {
                    let range = Position::new(index, window.start + bytes.start)
                        ..Position::new(index, window.start + bytes.end);
                    self.is_grapheme_range(&range).then_some(range)
                })
            }
            ParagraphSearch::Nearest => {
                self.find_nearest_match(index, needle, options, edge, backward)
            }
            ParagraphSearch::Scan => {
                let mut found = None;
                let _ = self.visit_paragraph_matches(index, needle, options, |range| {
                    if backward {
                        if boundary.is_some_and(|position| range.end > position) {
                            return ControlFlow::Break(());
                        }
                        found = Some(range);
                    } else if boundary.is_none_or(|position| range.start >= position) {
                        found = Some(range);
                        return ControlFlow::Break(());
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
        backward: bool,
    ) -> Option<Range<Position>> {
        let text = self.paragraphs()[index].text();
        // A character spans at most four bytes and lowercases to at least one,
        // so windows sharing this many bytes keep candidates crossing an edge.
        let overlap = needle.len().saturating_mul(4);
        let mut width = overlap.saturating_mul(2).max(NEAREST_WINDOW);
        let mut window = edge..edge;
        let mut words: Option<WordBoundaries<'_>> = None;
        loop {
            window = if backward {
                let end = text
                    .ceil_char_boundary(window.start.saturating_add(overlap))
                    .min(edge);
                text.floor_char_boundary(end.saturating_sub(width))..end
            } else {
                let start = text
                    .floor_char_boundary(window.end.saturating_sub(overlap))
                    .max(edge);
                start..text.ceil_char_boundary(start.saturating_add(width))
            };
            let source = &text[window.clone()];
            let searched = if options.case_sensitive {
                Cow::Borrowed(source)
            } else {
                Cow::Owned(lowercase(source))
            };
            let mut offsets = LowercaseOffsets::new(source, !options.case_sensitive);
            let found = find_literal(&searched, needle, backward, |bytes| {
                let start = window.start + offsets.original(bytes.start)?;
                let end = window.start + offsets.original(bytes.end)?;
                if options.whole_word {
                    let words = match &mut words {
                        Some(words) if words.start <= start => words,
                        words => words
                            .insert(WordBoundaries::new(text, word_restart(text, window.start))),
                    };
                    if !words.contains(end) || !words.contains(start) {
                        return None;
                    }
                }
                let range = Position::new(index, start)..Position::new(index, end);
                self.is_grapheme_range(&range).then_some(range)
            });
            let exhausted = if backward {
                window.start == 0
            } else {
                window.end == text.len()
            };
            if found.is_some() || exhausted {
                return found;
            }
            width = width.saturating_mul(2);
        }
    }

    fn visit_paragraph_matches(
        &self,
        index: usize,
        needle: &str,
        options: SearchOptions,
        mut accept: impl FnMut(Range<Position>) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let paragraph = &self.paragraphs()[index];
        if options.case_sensitive && needle.len() > paragraph.text().len() {
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
            visit_text_matches(paragraph.text(), needle, options, visit)
        }
    }

    fn visit_normalized_matches(
        &self,
        needle: &str,
        options: SearchOptions,
        mut accept: impl FnMut(Range<Position>) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        if !needle.contains('\n') {
            for index in 0..self.paragraphs().len() {
                self.visit_paragraph_matches(index, needle, options, &mut accept)?;
            }
            return ControlFlow::Continue(());
        }
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
        visit_text_matches(&source, needle, options, |bytes| {
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
    let query = normalize_newlines(query);
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
    // Occurrences of a query without a nonempty border never overlap, so every
    // occurrence belongs to the forward match stream and can be found locally.
    fn new(needle: &str, options: SearchOptions) -> Self {
        if !has_unique_start(needle) && literal_prefixes(needle.as_bytes()).last() != Some(&0) {
            Self::Scan
        } else if options.case_sensitive && !options.whole_word {
            Self::Literal
        } else {
            Self::Nearest
        }
    }
}

// Word segmentation restarted after a space that precedes an ASCII character
// other than a space reports the same boundaries as segmentation from byte 0.
fn word_restart(text: &str, byte: usize) -> usize {
    let bytes = text.as_bytes();
    (1..=byte.min(bytes.len().saturating_sub(1)))
        .rev()
        .find(|&start| bytes[start - 1] == b' ' && bytes[start] != b' ' && bytes[start].is_ascii())
        .unwrap_or(0)
}

struct WordBoundaries<'a> {
    start: usize,
    end: usize,
    boundaries: Vec<usize>,
    remaining: UWordBoundIndices<'a>,
}

impl<'a> WordBoundaries<'a> {
    fn new(text: &'a str, start: usize) -> Self {
        Self {
            start,
            end: text.len(),
            boundaries: Vec::new(),
            remaining: text[start..].split_word_bound_indices(),
        }
    }

    fn contains(&mut self, byte: usize) -> bool {
        debug_assert!(byte >= self.start);
        if byte == self.end {
            return true;
        }
        while self
            .boundaries
            .last()
            .is_none_or(|boundary| *boundary < byte)
        {
            let Some((boundary, _)) = self.remaining.next() else {
                return false;
            };
            self.boundaries.push(self.start + boundary);
        }
        self.boundaries.binary_search(&byte).is_ok()
    }
}

struct LowercaseOffsets<'a> {
    source: &'a str,
    folded: bool,
    changes: Option<Vec<(Range<usize>, Range<usize>)>>,
}

impl<'a> LowercaseOffsets<'a> {
    fn new(source: &'a str, folded: bool) -> Self {
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
                        changes.push((
                            lower_byte..lower_byte + lowered_len,
                            byte..byte + character.len_utf8(),
                        ));
                    }
                    lower_byte += lowered_len;
                }
            }
            changes
        });
        let index = changes.partition_point(|(lowered, _)| lowered.end <= byte);
        if let Some((lowered, original)) = changes.get(index)
            && byte >= lowered.start
        {
            return (byte == lowered.start).then_some(original.start);
        }
        let original = index.checked_sub(1).map_or(byte, |index| {
            let (lowered, original) = &changes[index];
            byte - lowered.end + original.end
        });
        source.is_char_boundary(original).then_some(original)
    }
}

fn find_literal<T>(
    source: &str,
    needle: &str,
    backward: bool,
    mut accept: impl FnMut(Range<usize>) -> Option<T>,
) -> Option<T> {
    if backward {
        source
            .rmatch_indices(needle)
            .find_map(|(start, _)| accept(start..start + needle.len()))
    } else {
        source
            .match_indices(needle)
            .find_map(|(start, _)| accept(start..start + needle.len()))
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

fn visit_text_matches(
    source: &str,
    needle: &str,
    options: SearchOptions,
    mut accept: impl FnMut(Range<usize>) -> ControlFlow<(), bool>,
) -> ControlFlow<()> {
    if options.case_sensitive && !options.whole_word {
        return visit_literals(source, needle, accept);
    }
    let searched = if options.case_sensitive {
        Cow::Borrowed(source)
    } else {
        Cow::Owned(lowercase(source))
    };
    if needle.len() > searched.len() {
        return ControlFlow::Continue(());
    }
    let mut word_boundaries = None;
    let mut offsets = LowercaseOffsets::new(source, !options.case_sensitive);
    visit_literals(&searched, needle, |bytes| {
        let range = offsets
            .original(bytes.start)
            .zip(offsets.original(bytes.end))
            .and_then(|(start, end)| {
                if options.whole_word {
                    let (boundaries, remaining) = word_boundaries.get_or_insert_with(|| {
                        (
                            Vec::new(),
                            source
                                .split_word_bound_indices()
                                .map(|(byte, _)| byte)
                                .chain([source.len()]),
                        )
                    });
                    while boundaries.last().is_none_or(|byte| *byte < end) {
                        let Some(byte) = remaining.next() else {
                            break;
                        };
                        boundaries.push(byte);
                    }
                    if boundaries.binary_search(&start).is_err()
                        || boundaries.binary_search(&end).is_err()
                    {
                        return None;
                    }
                }
                Some(start..end)
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
            let prefixes = literal_prefixes(needle.as_bytes());
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

fn literal_prefixes(needle: &[u8]) -> Vec<usize> {
    // Build a KMP failure table only after a rejected match might hide overlaps.
    // Restarting a fresh str matcher for each rejection can be quadratic.
    let mut prefixes = vec![0; needle.len()];
    let mut matched = 0;
    for index in 1..needle.len() {
        while matched > 0 && needle[index] != needle[matched] {
            matched = prefixes[matched - 1];
        }
        if needle[index] == needle[matched] {
            matched += 1;
        }
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
        while matched > 0 && byte != needle[matched] {
            matched = prefixes[matched - 1];
        }
        if byte == needle[matched] {
            matched += 1;
        }
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
