use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::{Document, LINE_SEPARATOR};
use crate::{Error, Position};

// Store only nonempty 64-byte blocks. Dense boundaries cost at most one
// block per 64 text bytes, while a single large word needs only its two ends.
#[derive(Debug)]
pub(super) struct WordSelectionIndex {
    blocks: Box<[BoundaryBlock]>,
}

#[derive(Debug)]
struct BoundaryBlock {
    index: usize,
    bits: u64,
}

impl WordSelectionIndex {
    pub(super) fn new(text: &str) -> Self {
        let mut blocks: Vec<BoundaryBlock> = Vec::new();
        for byte in text
            .split_word_bound_indices()
            .map(|(byte, _)| byte)
            .chain([text.len()])
        {
            let index = byte / 64;
            let bit = 1_u64 << (byte % 64);
            if let Some(last) = blocks.last_mut().filter(|last| last.index == index) {
                last.bits |= bit;
            } else {
                blocks.push(BoundaryBlock { index, bits: bit });
            }
        }
        Self {
            blocks: blocks.into(),
        }
    }

    pub(super) fn contains(&self, byte: usize) -> bool {
        self.blocks
            .binary_search_by_key(&(byte / 64), |block| block.index)
            .is_ok_and(|index| self.blocks[index].bits & (1_u64 << (byte % 64)) != 0)
    }

    // Callers select a character, so byte is strictly before the text's end.
    pub(super) fn segment(&self, byte: usize) -> Range<usize> {
        let block_index = byte / 64;
        let after = self
            .blocks
            .partition_point(|block| block.index <= block_index);
        let block = &self.blocks[after - 1];
        let mask = if block.index == block_index {
            u64::MAX >> (63 - byte % 64)
        } else {
            u64::MAX
        };
        let before = block.bits & mask;
        let start = if before != 0 {
            block.index * 64 + (63 - before.leading_zeros()) as usize
        } else {
            let previous = &self.blocks[after - 2];
            previous.index * 64 + (63 - previous.bits.leading_zeros()) as usize
        };
        let remaining = block.bits & !mask;
        let end = if remaining != 0 {
            block.index * 64 + remaining.trailing_zeros() as usize
        } else {
            let next = &self.blocks[after];
            next.index * 64 + next.bits.trailing_zeros() as usize
        };
        start..end
    }
}

impl Document {
    /// Returns the range that double-click word selection selects at
    /// `position`.
    ///
    /// The range is the Unicode word-boundary segment (UAX #29) that contains
    /// the grapheme after `position`, such as a word, a run of spaces, or a
    /// punctuation character, widened to grapheme boundaries. At the end of a
    /// paragraph or before a soft line break, the grapheme before `position`
    /// is used instead. The range is empty at `position` in an empty
    /// paragraph, at a paragraph start before a soft line break, and between
    /// two soft line breaks.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPosition`] if `position` is not a valid
    /// position in this document.
    pub fn word_range_at(&self, position: Position) -> Result<Range<Position>, Error> {
        self.validate_position(position)?;
        let paragraph = &self.paragraphs[position.paragraph];
        let text = paragraph.text();
        let byte =
            if position.byte == text.len() || text[position.byte..].starts_with(LINE_SEPARATOR) {
                let previous = paragraph.previous_grapheme(position.byte);
                if position.byte == 0 || text[previous..].starts_with(LINE_SEPARATOR) {
                    return Ok(position..position);
                }
                previous
            } else {
                position.byte
            };
        let bytes = if let Some(segment) = paragraph.cached_word_segment(byte) {
            segment
        } else {
            let restart = word_restart(text, byte);
            // Keep ordinary local selection allocation-free. Long scans build
            // a forward-segmented index shared with formatting-only copies.
            if byte - restart > 4_096 && byte - restart >= text.len() / 2 {
                paragraph.indexed_word_segment(byte)
            } else {
                let segment = text[restart..]
                    .split_word_bound_indices()
                    .map(|(start, word)| (restart + start, word))
                    .find(|(start, word)| *start <= byte && byte < start + word.len())
                    .map_or(byte..byte, |(start, word)| start..start + word.len());
                // A huge selected word is expensive even at its beginning.
                // Cache it only when this query already traversed most of the
                // paragraph; avoid scanning an unrelated large suffix.
                if segment.end - restart > 4_096 && segment.end - restart >= text.len() / 2 {
                    paragraph.indexed_word_segment(byte)
                } else {
                    segment
                }
            }
        };
        let bytes =
            paragraph.boundary_at_or_before(bytes.start)..paragraph.boundary_at_or_after(bytes.end);
        Ok(Position::new(position.paragraph, bytes.start)
            ..Position::new(position.paragraph, bytes.end))
    }
}

// A space is followed by a word boundary unless the next character is Extend,
// Format, ZWJ, or WSegSpace (WB3d, WB4), and no later rule looks back past that
// space, so segmentation restarted there matches segmentation from byte 0.
pub(crate) fn word_restart(text: &str, byte: usize) -> usize {
    let bytes = text.as_bytes();
    let last = text.floor_char_boundary(byte.min(bytes.len().saturating_sub(1)));
    text[..last]
        .rmatch_indices(' ')
        .map(|(space, _)| space + 1)
        .find(|&start| {
            if bytes[start].is_ascii() {
                bytes[start] != b' '
            } else {
                let pair = &text[start - 1..text.ceil_char_boundary(start + 1)];
                pair.split_word_bounds().next() == Some(" ")
            }
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_boundaries_match_forward_segmentation_in_sparse_and_dense_blocks() {
        for text in [
            String::new(),
            "a".repeat(1_025),
            "你好".repeat(100),
            "🇺🇸🇨🇦a\u{301}:β  \u{ad}👩\u{200d}💻!?".repeat(100),
        ] {
            let index = WordSelectionIndex::new(&text);
            let expected: Vec<_> = text
                .split_word_bound_indices()
                .map(|(byte, _)| byte)
                .chain([text.len()])
                .collect();
            for byte in 0..=text.len() + 64 {
                assert_eq!(
                    index.contains(byte),
                    expected.binary_search(&byte).is_ok(),
                    "byte {byte} of {text:?}"
                );
            }
        }
    }

    #[test]
    fn word_selection_matches_full_paragraph_segmentation() {
        for suffix in [
            "hello world",
            "слово λόγος",
            "你好 世界",
            "a  b",
            "a 'b'",
            "a \u{301}b",
            "a \u{200d}👩",
            "a \u{ad}b",
            "a \u{3000}b",
            "🇺🇸 🇨🇦",
        ] {
            let text = "prefix ".repeat(100) + suffix;
            let document = Document::from_text(&text);
            let paragraph = &document.paragraphs()[0];
            for (byte, _) in text.grapheme_indices(true) {
                let (start, word) = text
                    .split_word_bound_indices()
                    .find(|(start, word)| *start <= byte && byte < start + word.len())
                    .unwrap();
                let expected = Position::new(0, paragraph.boundary_at_or_before(start))
                    ..Position::new(0, paragraph.boundary_at_or_after(start + word.len()));
                assert_eq!(
                    document.word_range_at(Position::new(0, byte)).unwrap(),
                    expected,
                    "byte {byte}, suffix {suffix:?}"
                );
            }
        }
    }

    #[test]
    fn generated_word_selection_matches_unicode_reference_at_every_caret() {
        let tokens = [
            "a",
            " ",
            "  ",
            "\t",
            "'",
            ":",
            "_",
            "7",
            "é",
            "слово",
            "你好",
            "\u{301}",
            "\u{600}",
            "\u{200d}",
            "\u{ad}",
            "\u{3000}",
            "\u{a0}",
            "\u{2028}",
            "\u{2028}\u{2028}",
            "🇺",
            "🇸",
            "👩",
            "🏻",
            "\u{5d0}",
        ];
        let mut seed = 0x1f83_d9ab_fb41_bd6b_u64;
        let mut checked = 0;
        for length in 0..2_000 {
            let mut text = String::new();
            for _ in 0..length % 32 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                text.push_str(tokens[seed as usize % tokens.len()]);
            }
            let document = Document::from_text(&text);
            let graphemes: Vec<_> = text.grapheme_indices(true).collect();
            let boundaries: Vec<_> = graphemes
                .iter()
                .map(|(byte, _)| *byte)
                .chain([text.len()])
                .collect();
            for cached in [false, true] {
                if cached && !text.is_empty() {
                    document.paragraphs()[0].indexed_word_segment(0);
                }
                for (index, &byte) in boundaries.iter().enumerate() {
                    let position = Position::new(0, byte);
                    let target = if byte == text.len() || text[byte..].starts_with(LINE_SEPARATOR) {
                        index
                            .checked_sub(1)
                            .filter(|&previous| !graphemes[previous].1.starts_with(LINE_SEPARATOR))
                    } else {
                        Some(index)
                    };
                    let expected = target.map_or(position..position, |target| {
                        let target = boundaries[target];
                        let (start, word) = text
                            .split_word_bound_indices()
                            .find(|(start, word)| *start <= target && target < start + word.len())
                            .unwrap();
                        let end = start + word.len();
                        let start =
                            boundaries[boundaries.partition_point(|byte| *byte <= start) - 1];
                        let end = boundaries[boundaries.partition_point(|byte| *byte < end)];
                        Position::new(0, start)..Position::new(0, end)
                    });
                    let actual = document.word_range_at(position).unwrap();
                    assert_eq!(actual, expected, "{text:?} at {byte}");
                    assert!(document.validate_position(actual.start).is_ok());
                    assert!(document.validate_position(actual.end).is_ok());
                    checked += 1;
                }
            }
            for byte in (0..=text.len()).filter(|byte| boundaries.binary_search(byte).is_err()) {
                let position = Position::new(0, byte);
                assert_eq!(
                    document.word_range_at(position),
                    Err(crate::Error::InvalidPosition(position))
                );
            }
        }
        assert!(checked > 20_000, "{checked}");
    }

    #[test]
    fn indexed_selection_matches_forward_segmentation_and_survives_formatting() {
        for token in ["你好", "7'🏻\u{301}\u{200d}👩א'\u{301}", "🇺🇸🇨🇦", "a\u{301}"]
        {
            let text = token.repeat(2_000);
            let document = Document::from_text(&text);
            let paragraph = &document.paragraphs()[0];
            document.word_range_at(document.end()).unwrap();
            assert!(paragraph.cached_word_segment(0).is_some());
            let formatted = paragraph.with_kind(crate::ParagraphKind::Quote);
            assert!(formatted.cached_word_segment(0).is_some());
            for (byte, _) in text.grapheme_indices(true).step_by(37) {
                let (start, word) = text
                    .split_word_bound_indices()
                    .find(|(start, word)| *start <= byte && byte < start + word.len())
                    .unwrap();
                let expected = paragraph.boundary_at_or_before(start)
                    ..paragraph.boundary_at_or_after(start + word.len());
                let actual = document.word_range_at(Position::new(0, byte)).unwrap();
                assert_eq!(
                    actual,
                    Position::new(0, expected.start)..Position::new(0, expected.end)
                );
            }
        }
    }

    #[test]
    fn text_edits_discard_word_selection_index_and_undo_restores_it() {
        let document = Document::from_text(&"你好".repeat(2_000));
        document.word_range_at(document.end()).unwrap();
        let mut editor = crate::Editor::new(document);
        editor.insert_text("prefix ").unwrap();
        assert!(
            editor.document().paragraphs()[0]
                .cached_word_segment(0)
                .is_none()
        );
        assert_eq!(
            editor
                .document()
                .word_range_at(Position::new(0, 0))
                .unwrap(),
            Position::new(0, 0)..Position::new(0, 6)
        );
        assert!(editor.undo());
        assert!(
            editor.document().paragraphs()[0]
                .cached_word_segment(0)
                .is_some()
        );
        assert_eq!(
            editor
                .document()
                .word_range_at(Position::new(0, 0))
                .unwrap(),
            Position::new(0, 0)..Position::new(0, 3)
        );
    }

    #[test]
    fn early_selection_does_not_index_a_large_untouched_suffix() {
        let document = Document::from_text(&"!?".repeat(100_000));
        assert_eq!(
            document.word_range_at(Position::new(0, 4_097)).unwrap(),
            Position::new(0, 4_097)..Position::new(0, 4_098)
        );
        assert!(document.paragraphs()[0].cached_word_segment(0).is_none());
    }

    #[test]
    fn a_long_selected_word_is_cached_even_at_its_start() {
        let text = "a".repeat(10_000);
        let document = Document::from_text(&text);
        assert_eq!(
            document.word_range_at(Position::new(0, 0)).unwrap(),
            Position::new(0, 0)..Position::new(0, text.len())
        );
        assert!(document.paragraphs()[0].cached_word_segment(0).is_some());
    }

    #[test]
    fn boundary_blocks_match_forward_segments_across_bit_and_block_edges() {
        for text in [
            "!?".repeat(200),
            "a".repeat(256),
            "你好".repeat(200),
            format!("{} {}!{}", "a".repeat(62), "b".repeat(128), "c".repeat(65)),
        ] {
            let index = WordSelectionIndex::new(&text);
            for (start, segment) in text.split_word_bound_indices() {
                for (offset, _) in segment.char_indices() {
                    assert_eq!(
                        index.segment(start + offset),
                        start..start + segment.len(),
                        "{text:?}, byte {}",
                        start + offset
                    );
                }
            }
            assert!(index.blocks.len() <= text.len() / 64 + 1);
        }
        let dense = WordSelectionIndex::new(&"!?".repeat(100_000));
        assert!(std::mem::size_of_val(&*dense.blocks) <= 50_016);
        let sparse = WordSelectionIndex::new(&"a".repeat(200_000));
        assert_eq!(sparse.blocks.len(), 2);
    }

    #[test]
    fn a_long_local_word_does_not_index_a_large_unrelated_prefix() {
        let text = "x ".repeat(100_000) + &"a".repeat(5_000);
        let document = Document::from_text(&text);
        assert_eq!(
            document.word_range_at(document.end()).unwrap(),
            Position::new(0, 200_000)..document.end()
        );
        assert!(document.paragraphs()[0].cached_word_segment(0).is_none());
    }
}
