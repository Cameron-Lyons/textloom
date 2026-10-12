use std::{
    borrow::Cow,
    ops::{ControlFlow, Range},
};

use unicode_segmentation::UnicodeSegmentation;

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
                        is_direct_literal(&needle, options),
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

        let direct_literal = is_direct_literal(&needle, options);
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
                direct_literal,
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
        direct_literal: bool,
    ) -> Option<Range<Position>> {
        if boundary.is_some_and(|position| {
            if backward {
                position.byte == 0
            } else {
                position.byte == self.paragraphs()[index].text().len()
            }
        }) {
            return None;
        }
        if direct_literal {
            let text = self.paragraphs()[index].text();
            let edge = boundary.map_or(if backward { text.len() } else { 0 }, |position| {
                position.byte
            });
            let valid_match = |start| {
                let range = Position::new(index, start)..Position::new(index, start + needle.len());
                self.is_grapheme_range(&range).then_some(range)
            };
            return if backward {
                text[..edge]
                    .rmatch_indices(needle)
                    .find_map(|(start, _)| valid_match(start))
            } else {
                text[edge..]
                    .match_indices(needle)
                    .find_map(|(start, _)| valid_match(edge + start))
            };
        }
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
        visit_text_matches(paragraph.text(), needle, options, |bytes| {
            let range = Position::new(index, bytes.start)..Position::new(index, bytes.end);
            if self.is_grapheme_range(&range) {
                accept(range).map_continue(|()| true)
            } else {
                ControlFlow::Continue(false)
            }
        })
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

fn is_direct_literal(needle: &str, options: SearchOptions) -> bool {
    options.case_sensitive
        && !options.whole_word
        && needle
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
    let mut lowercase_changes = None;
    let mut original_byte = |byte: usize| -> Option<usize> {
        let lowercase_changes = lowercase_changes.get_or_insert_with(|| {
            let mut changes = Vec::new();
            if !options.case_sensitive && !source.is_ascii() {
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
                debug_assert_eq!(lower_byte, searched.len());
            }
            changes
        });
        let index = lowercase_changes.partition_point(|(lowered, _)| lowered.end <= byte);
        if let Some((lowered, original)) = lowercase_changes.get(index)
            && byte >= lowered.start
        {
            return (byte == lowered.start).then_some(original.start);
        }
        let original = index.checked_sub(1).map_or(byte, |index| {
            let (lowered, original) = &lowercase_changes[index];
            byte - lowered.end + original.end
        });
        source.is_char_boundary(original).then_some(original)
    };
    visit_literals(&searched, needle, |bytes| {
        let range = original_byte(bytes.start)
            .zip(original_byte(bytes.end))
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
    mut accept: impl FnMut(Range<usize>) -> ControlFlow<(), bool>,
) -> ControlFlow<()> {
    let mut offset = 0;
    'search: loop {
        for (relative_start, _) in source[offset..].match_indices(needle) {
            let start = offset + relative_start;
            if !accept(start..start + needle.len())? {
                offset = start
                    + source[start..]
                        .chars()
                        .next()
                        .expect("a nonempty query matched a character")
                        .len_utf8();
                continue 'search;
            }
        }
        break;
    }
    ControlFlow::Continue(())
}
