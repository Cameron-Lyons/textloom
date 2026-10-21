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
        if let Some(lines) = paragraph_lines(&needle) {
            return self
                .find_lines_match(
                    range.start.paragraph,
                    &lines,
                    options,
                    Some(range.start),
                    false,
                )
                .is_some_and(|found| found == *range);
        }
        let mut found = false;
        let _ = self.visit_flattened_matches(&needle, options, |candidate| {
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
        let lines = paragraph_lines(&needle);
        if lines.is_none() && needle.contains('\n') {
            let mut found = None;
            let mut wrapped = None;
            let _ = self.visit_flattened_matches(&needle, options, |range| {
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
            let boundary = bounded.then_some(boundary);
            if let Some(lines) = &lines {
                self.find_lines_match(index, lines, options, boundary, backward)
            } else {
                self.find_paragraph_match(index, &needle, options, boundary, backward, strategy)
            }
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

    fn find_lines_match(
        &self,
        index: usize,
        lines: &[&str],
        options: SearchOptions,
        boundary: Option<Position>,
        backward: bool,
    ) -> Option<Range<Position>> {
        let [first_line, middle_lines @ .., last_line] = lines else {
            return None;
        };
        let breaks = middle_lines.len() + 1;
        let first_index = if backward {
            index.checked_sub(breaks)?
        } else {
            index
        };
        let [first, middle @ .., last] = self
            .paragraphs()
            .get(first_index..=first_index.checked_add(breaks)?)?
        else {
            return None;
        };
        let (first, last) = (first.text(), last.text());
        let mut middle = middle
            .iter()
            .map(|paragraph| paragraph.text())
            .zip(middle_lines);
        let (start, end) = if options.case_sensitive {
            (first.ends_with(first_line)
                && last.starts_with(last_line)
                && middle.all(|(text, line)| text == *line))
            .then(|| (first.len() - first_line.len(), last_line.len()))?
        } else {
            let start = lowered_suffix_start(first, first_line)?;
            let end = lowered_prefix_end(last, last_line)?;
            middle
                .all(|(text, line)| lowered_prefix_end(text, line) == Some(text.len()))
                .then_some((start, end))?
        };
        let range = Position::new(first_index, start)..Position::new(first_index + breaks, end);
        (boundary.is_none_or(|boundary| {
            if backward {
                range.end <= boundary
            } else {
                range.start >= boundary
            }
        }) && self.is_grapheme_range(&range)
            && (!options.whole_word
                || (is_word_boundary(first, start) && is_word_boundary(last, end))))
        .then_some(range)
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
        if !needle.contains('\n') {
            for index in 0..self.paragraphs().len() {
                self.visit_paragraph_matches(index, needle, options, &mut accept)?;
            }
            return ControlFlow::Continue(());
        }
        if let Some(lines) = paragraph_lines(needle) {
            for index in 0..self.paragraphs().len() {
                if let Some(range) = self.find_lines_match(index, &lines, options, None, false) {
                    accept(range)?;
                }
            }
            return ControlFlow::Continue(());
        }
        self.visit_flattened_matches(needle, options, accept)
    }

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
        visit_text_matches(&source, false, needle, options, |bytes| {
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
    has_unique_start(needle) || literal_prefixes(needle.as_bytes()).last() == Some(&0)
}

// A line of such a query must end, fill, or start the paragraph it matches.
fn paragraph_lines(needle: &str) -> Option<Vec<&str>> {
    (needle.contains('\n') && never_overlaps(needle)).then(|| needle.split('\n').collect())
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

fn is_word_boundary(text: &str, byte: usize) -> bool {
    WordBoundaries::new(text, word_restart(text, byte)).contains(byte)
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

struct WordBoundaries<'a> {
    text: &'a str,
    start: usize,
    boundaries: Vec<usize>,
    remaining: UWordBoundIndices<'a>,
}

impl<'a> WordBoundaries<'a> {
    fn new(text: &'a str, start: usize) -> Self {
        Self {
            text,
            start,
            boundaries: Vec::new(),
            remaining: text[start..].split_word_bound_indices(),
        }
    }

    fn contains(&mut self, byte: usize) -> bool {
        debug_assert!(byte >= self.start);
        if let Some(boundary) = ascii_word_boundary(self.text.as_bytes(), byte) {
            return boundary;
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
    let mut words = WordBoundaries::new(source, 0);
    let mut offsets = LowercaseOffsets::new(source, !options.case_sensitive && !ascii);
    visit_literals(&searched, needle, |bytes| {
        let range = offsets
            .original(bytes.start)
            .zip(offsets.original(bytes.end))
            .and_then(|(start, end)| {
                (!options.whole_word || (words.contains(end) && words.contains(start)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;

    fn assert_matches_flattened_stream(
        document: &Document,
        query: &str,
        options: SearchOptions,
    ) -> Option<usize> {
        let needle = search_needle(query, options);
        paragraph_lines(&needle)?;
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
            for wrap in [false, true] {
                for backward in [false, true] {
                    let expected = if backward {
                        matches
                            .iter()
                            .rev()
                            .find(|item| item.end <= range.start)
                            .or_else(|| wrap.then(|| matches.last()).flatten())
                    } else {
                        matches
                            .iter()
                            .find(|item| item.start >= range.end)
                            .or_else(|| wrap.then(|| matches.first()).flatten())
                    };
                    assert_eq!(
                        document.find_relative(query, options, range.clone(), wrap, backward),
                        expected.cloned(),
                        "{range:?} wrap={wrap} backward={backward} {context}"
                    );
                }
            }
        }
        Some(matches.len())
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
            "👩‍💻",
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
            for byte in (0..=text.len()).filter(|byte| text.is_char_boundary(*byte)) {
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
}
