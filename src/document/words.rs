use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::{Document, LINE_SEPARATOR};
use crate::{Error, Position};

impl Document {
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
        let bytes = text
            .split_word_bound_indices()
            .find(|(start, word)| *start <= byte && byte < start + word.len())
            .map_or(position.byte..position.byte, |(start, word)| {
                paragraph.boundary_at_or_before(start)
                    ..paragraph.boundary_at_or_after(start + word.len())
            });
        Ok(Position::new(position.paragraph, bytes.start)
            ..Position::new(position.paragraph, bytes.end))
    }
}
