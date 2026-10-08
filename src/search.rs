use std::{borrow::Cow, ops::Range};

use unicode_segmentation::UnicodeSegmentation;

use crate::{Document, Position};

/// Options for explicit document searches.
///
/// Case-insensitive search uses Unicode lowercase mappings, including
/// contextual final sigma. It does not perform Unicode normalization or full
/// case folding, so `é` differs from `e\u{301}` and `ß` differs from `ss`.
/// The default searches case-sensitive literal text without requiring whole words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchOptions {
    /// Compare literal text when true; compare Unicode lowercase mappings when false.
    pub case_sensitive: bool,
    /// Require both match endpoints to be Unicode word boundaries.
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
    /// Find nonoverlapping matches in document order, preserving original
    /// paragraph and UTF-8 byte coordinates. Only complete graphemes match.
    ///
    /// Query CRLF and CR are normalized to paragraph breaks. Empty queries
    /// return no matches. Case-sensitive queries without paragraph breaks or
    /// whole-word constraints search each paragraph without copying its text.
    /// Other multi-paragraph searches temporarily flatten the document.
    pub fn find(&self, query: &str, options: SearchOptions) -> Vec<Range<Position>> {
        if query.is_empty() {
            return Vec::new();
        }
        let normalized_query = if query.contains('\r') {
            Cow::Owned(query.replace("\r\n", "\n").replace('\r', "\n"))
        } else {
            Cow::Borrowed(query)
        };
        if options.case_sensitive && !options.whole_word && !normalized_query.contains('\n') {
            let mut matches = Vec::new();
            for (index, paragraph) in self.paragraphs().iter().enumerate() {
                if normalized_query.len() > paragraph.text().len() {
                    continue;
                }
                visit_matches(paragraph.text(), &normalized_query, |bytes| {
                    let range = Position::new(index, bytes.start)..Position::new(index, bytes.end);
                    if self.validate_position(range.start).is_ok()
                        && self.validate_position(range.end).is_ok()
                    {
                        matches.push(range);
                        true
                    } else {
                        false
                    }
                });
            }
            return matches;
        }
        let source = if let [paragraph] = self.paragraphs() {
            Cow::Borrowed(paragraph.text())
        } else {
            Cow::Owned(self.plain_text())
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
        let word_boundaries: Vec<_> = if options.whole_word {
            source
                .split_word_bound_indices()
                .map(|(byte, _)| byte)
                .chain([source.len()])
                .collect()
        } else {
            Vec::new()
        };

        let (searched, needle, lowercase_changes) = if options.case_sensitive {
            (Cow::Borrowed(source.as_ref()), normalized_query, Vec::new())
        } else {
            let lowered = source.to_lowercase();
            let mut changes = Vec::new();
            let mut lower_byte = 0;
            for (byte, character) in source.char_indices() {
                // String lowercase additionally handles contextual final
                // sigma, whose UTF-8 length equals its scalar lowercase.
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
            debug_assert_eq!(lower_byte, lowered.len());
            (
                Cow::Owned(lowered),
                Cow::Owned(normalized_query.to_lowercase()),
                changes,
            )
        };

        let original_byte = |byte: usize| -> Option<usize> {
            let index = lowercase_changes.partition_point(|(lowered, _)| lowered.end <= byte);
            if let Some((lowered, original)) = lowercase_changes.get(index)
                && byte >= lowered.start
            {
                // A width-changing scalar may expand into several lowercase
                // scalars. Only its original outer boundaries are valid.
                return (byte == lowered.start).then_some(original.start);
            }
            let original = index.checked_sub(1).map_or(byte, |index| {
                let (lowered, original) = &lowercase_changes[index];
                byte - lowered.end + original.end
            });
            // This also rejects partial expansions whose total UTF-8 width
            // equals the original scalar width, without storing all offsets.
            source.is_char_boundary(original).then_some(original)
        };
        let mut matches = Vec::new();
        visit_matches(&searched, &needle, |bytes| {
            let range = original_byte(bytes.start)
                .zip(original_byte(bytes.end))
                .and_then(|(start, end)| {
                    if options.whole_word
                        && (word_boundaries.binary_search(&start).is_err()
                            || word_boundaries.binary_search(&end).is_err())
                    {
                        return None;
                    }
                    let range = position(start)..position(end);
                    (self.validate_position(range.start).is_ok()
                        && self.validate_position(range.end).is_ok())
                    .then_some(range)
                });
            if let Some(range) = range {
                matches.push(range);
                true
            } else {
                false
            }
        });
        matches
    }
}

/// A rejected candidate may overlap a valid later match, so only accepted
/// matches advance past the full needle. Retain the literal searcher's needle
/// preprocessing between accepted candidates.
fn visit_matches(source: &str, needle: &str, mut accept: impl FnMut(Range<usize>) -> bool) {
    let mut offset = 0;
    'search: loop {
        for (relative_start, _) in source[offset..].match_indices(needle) {
            let start = offset + relative_start;
            if !accept(start..start + needle.len()) {
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
}
