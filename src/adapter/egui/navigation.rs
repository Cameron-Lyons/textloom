//! Pointer hit testing, grapheme snapping, and visual caret navigation.
use std::ops::Range;

use egui::{
    Galley, Pos2, Rect, Vec2,
    text::{CCursor, CharIndex},
};
use unicode_segmentation::UnicodeSegmentation;

use super::layout::ParagraphLayouts;
use crate::{Document, Editor, Error, Paragraph, Position, Selection};

#[derive(Clone)]
pub(super) struct VerticalNavigation {
    pub(super) selection: Selection,
    pub(super) preferred_x: f32,
    pub(super) prefer_next_row: bool,
}

pub(super) fn hit_test(
    editor: &Editor,
    layouts: &ParagraphLayouts,
    point: Vec2,
) -> (Position, bool) {
    let paragraph = layouts
        .paragraph_at_y(point.y)
        .min(layouts.len().saturating_sub(1));
    let Some(layout) = layouts.get(paragraph) else {
        return (Position::default(), true);
    };
    let Some(source) = editor.document().paragraph(paragraph) else {
        return (Position::default(), true);
    };
    let cursor = layout
        .galley
        .cursor_from_pos(point - layout.rect.min.to_vec2());
    let byte = source
        .byte_from_scalar(cursor.index.0)
        .unwrap_or(source.text().len());
    let before = source.boundary_at_or_before(byte);
    let after = source.boundary_at_or_after(byte);
    let byte = if byte - before <= after - byte {
        before
    } else {
        after
    };
    let index = CharIndex(source.scalar_index(byte).expect("snapped scalar boundary"));
    let clicked_row = layout.galley.layout_from_cursor(cursor).row;
    // A wrap seam has two visual caret positions. Preserve the row that was
    // clicked even when a scalar hit was snapped to a grapheme boundary.
    let prefer_next_row = [cursor.prefer_next_row, !cursor.prefer_next_row]
        .into_iter()
        .find(|&prefer_next_row| {
            layout
                .galley
                .layout_from_cursor(CCursor {
                    index,
                    prefer_next_row,
                })
                .row
                == clicked_row
        })
        .unwrap_or(cursor.prefer_next_row);
    (Position::new(paragraph, byte), prefer_next_row)
}

pub(super) fn pointer_navigation(
    editor: &Editor,
    layouts: &ParagraphLayouts,
    position: Position,
    prefer_next_row: bool,
) -> Option<VerticalNavigation> {
    let selection = editor.selection();
    if selection.focus != position {
        return None;
    }
    let caret = document_caret_rect(
        editor.document(),
        layouts,
        Pos2::ZERO,
        position,
        prefer_next_row,
    );
    Some(VerticalNavigation {
        selection,
        preferred_x: caret.center().x,
        prefer_next_row,
    })
}

pub(super) fn preedit_byte_range(text: &str, range: &Range<usize>) -> Result<Range<usize>, Error> {
    if range.start > range.end {
        return Err(Error::InvalidCompositionSelection);
    }
    // Chars skips scalar prefixes in chunks; CharIndices uses a scalar-by-scalar
    // fallback. Derive byte offsets from the remaining suffix without allocating.
    let mut chars = text.chars();
    if range.start != 0 && chars.nth(range.start - 1).is_none() {
        return Err(Error::InvalidCompositionSelection);
    }
    let start = text.len() - chars.as_str().len();
    if range.end != range.start && chars.nth(range.end - range.start - 1).is_none() {
        return Err(Error::InvalidCompositionSelection);
    }
    let end = text.len() - chars.as_str().len();
    Ok(start..end)
}

pub(super) fn snap_grapheme(paragraph: &Paragraph, byte: usize) -> usize {
    let before = paragraph.boundary_at_or_before(byte);
    let after = paragraph.boundary_at_or_after(byte);
    if byte - before <= after - byte {
        before
    } else {
        after
    }
}

pub(super) fn word_selection(editor: &Editor, position: Position) -> Selection {
    let Some(paragraph) = editor.document().paragraph(position.paragraph) else {
        return Selection::caret(position);
    };
    let text = paragraph.text();
    // Hit testing snaps the right half of the final grapheme to paragraph end.
    // Double clicking there should still select the trailing word or symbol.
    let byte = if position.byte == text.len() {
        paragraph.boundary_at_or_before(text.len().saturating_sub(1))
    } else {
        position.byte
    };
    for (start, word) in text.split_word_bound_indices() {
        if start <= byte && byte < start + word.len() {
            return Selection::new(
                Position::new(position.paragraph, snap_grapheme(paragraph, start)),
                Position::new(
                    position.paragraph,
                    snap_grapheme(paragraph, start + word.len()),
                ),
            );
        }
    }
    Selection::caret(position)
}

pub(super) fn document_caret_rect(
    document: &Document,
    layouts: &ParagraphLayouts,
    origin: Pos2,
    position: Position,
    prefer_next_row: bool,
) -> Rect {
    let Some(layout) = layouts.get(position.paragraph) else {
        return Rect::from_min_size(origin, Vec2::new(0.0, 16.0));
    };
    let index = document
        .paragraph(position.paragraph)
        .map_or(0, |paragraph| {
            paragraph
                .scalar_index(position.byte)
                .unwrap_or(paragraph.scalar_count())
        });
    layout
        .galley
        .pos_from_cursor(CCursor {
            index: CharIndex(index),
            prefer_next_row,
        })
        .translate(origin.to_vec2() + layout.rect.min.to_vec2())
}

pub(super) fn vertical_position(
    editor: &Editor,
    layouts: &ParagraphLayouts,
    navigation: Option<&VerticalNavigation>,
    down: bool,
) -> Option<(Position, f32, bool)> {
    let focus = editor.selection().focus;
    let source = editor.document().paragraph(focus.paragraph)?;
    let layout = layouts.get(focus.paragraph)?;
    let cursor = CCursor {
        index: CharIndex(source.scalar_index(focus.byte)?),
        prefer_next_row: navigation.is_none_or(|navigation| navigation.prefer_next_row),
    };
    let row = layout.galley.layout_from_cursor(cursor).row;
    let preferred_x = navigation.map_or_else(
        || layout.rect.left() + layout.galley.pos_from_cursor(cursor).center().x,
        |navigation| navigation.preferred_x,
    );
    let (paragraph, cursor) = if down && row + 1 < layout.galley.rows.len() {
        (
            focus.paragraph,
            layout
                .galley
                .cursor_down_one_row(&cursor, Some(preferred_x - layout.rect.left()))
                .0,
        )
    } else if !down && row > 0 {
        (
            focus.paragraph,
            layout
                .galley
                .cursor_up_one_row(&cursor, Some(preferred_x - layout.rect.left()))
                .0,
        )
    } else if down && focus.paragraph + 1 < layouts.len() {
        let paragraph = focus.paragraph + 1;
        let target = &layouts.get(paragraph).unwrap();
        let row = target.galley.rows.first()?;
        (
            paragraph,
            target.galley.cursor_from_pos(Vec2::new(
                preferred_x - target.rect.left(),
                row.rect().center().y,
            )),
        )
    } else if !down && focus.paragraph > 0 {
        let paragraph = focus.paragraph - 1;
        let target = &layouts.get(paragraph).unwrap();
        let row = target.galley.rows.last()?;
        (
            paragraph,
            target.galley.cursor_from_pos(Vec2::new(
                preferred_x - target.rect.left(),
                row.rect().center().y,
            )),
        )
    } else {
        (
            focus.paragraph,
            if down {
                layout.galley.end()
            } else {
                layout.galley.begin()
            },
        )
    };
    let source = editor.document().paragraph(paragraph)?;
    let (byte, prefer_next_row) = snap_vertical_cursor(
        source,
        &layouts.get(paragraph).unwrap().galley,
        cursor,
        down,
    )?;
    Some((Position::new(paragraph, byte), preferred_x, prefer_next_row))
}

pub(super) fn snap_vertical_cursor(
    source: &Paragraph,
    galley: &Galley,
    cursor: CCursor,
    down: bool,
) -> Option<(usize, bool)> {
    let byte = source.byte_from_scalar(cursor.index.0)?;
    let before = source.boundary_at_or_before(byte);
    let after = source.boundary_at_or_after(byte);
    if before == after {
        return Some((byte, cursor.prefer_next_row));
    }
    // A space followed by combining marks can wrap between its scalar glyphs.
    // Prefer a real caret boundary on the target row rather than snapping back
    // onto the row we just left. Recompute affinity for the snapped boundary.
    let target_row = galley.layout_from_cursor(cursor).row;
    let on_target_row = |byte| {
        let index = CharIndex(source.scalar_index(byte)?);
        [cursor.prefer_next_row, !cursor.prefer_next_row]
            .into_iter()
            .find(|&prefer_next_row| {
                galley
                    .layout_from_cursor(CCursor {
                        index,
                        prefer_next_row,
                    })
                    .row
                    == target_row
            })
    };
    match (on_target_row(before), on_target_row(after)) {
        (Some(before_affinity), Some(after_affinity)) => Some(if byte - before <= after - byte {
            (before, before_affinity)
        } else {
            (after, after_affinity)
        }),
        (Some(affinity), None) => Some((before, affinity)),
        (None, Some(affinity)) => Some((after, affinity)),
        // A grapheme spanning the entire target row has no legal caret there.
        // Cross the cluster in the requested direction, using the closest row
        // representation of that boundary when it falls on another wrap seam.
        (None, None) => Some(if down { (after, false) } else { (before, true) }),
    }
}
