use std::ops::Range;

use egui::text::{CCursor, CharIndex};
use egui::{Galley, Pos2, Rect, Vec2};

use super::layout::ParagraphLayouts;
use crate::{Document, Editor, Error, Link, Paragraph, Position, Selection};

#[derive(Clone)]
pub(super) struct VerticalNavigation {
    pub(super) selection: Selection,
    pub(super) preferred_x: f32,
    pub(super) prefer_next_row: bool,
}

// Where a visual caret movement lands, and the column that later vertical
// movements keep.
#[derive(Clone, Copy)]
pub(super) struct NavigationTarget {
    pub(super) position: Position,
    // The caret column in layout coordinates.
    pub(super) preferred_x: f32,
    // Places a caret at a soft-wrap seam at the start of the next row.
    pub(super) prefer_next_row: bool,
}

// A caret position, and which row shows it when it falls on a soft-wrap seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CaretPlacement {
    pub(super) position: Position,
    // Shows the caret at the start of the next row rather than the end of the
    // previous one.
    pub(super) prefer_next_row: bool,
}

pub(super) fn hit_test(editor: &Editor, layouts: &ParagraphLayouts, point: Vec2) -> CaretPlacement {
    let document_start = CaretPlacement {
        position: Position::default(),
        prefer_next_row: true,
    };
    let paragraph = layouts
        .paragraph_at_y(point.y)
        .min(layouts.len().saturating_sub(1));
    let Some(layout) = layouts.get(paragraph) else {
        return document_start;
    };
    let Some(source) = editor.document().paragraph(paragraph) else {
        return document_start;
    };
    let local = point - layout.rect.min.to_vec2();
    let cursor = layout.galley.cursor_from_pos(local);
    let (byte, prefer_next_row) =
        snap_vertical_cursor(source, &layout.galley, cursor, local.x, false)
            .unwrap_or((source.text().len(), cursor.prefer_next_row));
    CaretPlacement {
        position: Position::new(paragraph, byte),
        prefer_next_row,
    }
}

pub(super) fn grapheme_at_point(
    document: &Document,
    layouts: &ParagraphLayouts,
    point: Vec2,
) -> Option<Position> {
    let paragraph = layouts.paragraph_at_y(point.y);
    let layout = layouts.get(paragraph)?;
    let source = document.paragraph(paragraph)?;
    let local = point - layout.rect.min.to_vec2();
    let mut scalar = 0;
    for row in &layout.galley.rows {
        if row.min_y() <= local.y && local.y < row.max_y() {
            let x = local.x - row.pos.x;
            let column = row
                .glyphs
                .iter()
                .position(|glyph| glyph.pos.x <= x && x < glyph.max_x())?;
            let byte = source.byte_from_scalar(scalar + column)?;
            return Some(Position::new(paragraph, source.boundary_at_or_before(byte)));
        }
        scalar += row.char_count_including_newline().0;
    }
    None
}

pub(super) fn link_at_point(
    document: &Document,
    layouts: &ParagraphLayouts,
    point: Vec2,
) -> Option<Link> {
    let position = grapheme_at_point(document, layouts, point)?;
    document.link_at(position).cloned()
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
                .unwrap_or_else(|| paragraph.scalar_count())
        });
    layout
        .galley
        .pos_from_cursor(CCursor {
            index: CharIndex(index),
            prefer_next_row,
        })
        .translate(origin.to_vec2() + layout.rect.min.to_vec2())
}

pub(super) fn selection_bounds(
    document: &Document,
    layouts: &ParagraphLayouts,
    origin: Pos2,
    selection: Selection,
    content_width: f32,
) -> Option<Rect> {
    if selection.is_caret() {
        return None;
    }
    let range = selection.range();
    let start = document_caret_rect(document, layouts, origin, range.start, true);
    let end = document_caret_rect(document, layouts, origin, range.end, false);
    let mut bounds = start.union(end);
    if start.top() != end.top() {
        bounds.min.x = origin.x;
        bounds.max.x = bounds.max.x.max(origin.x + content_width);
    }
    Some(bounds)
}

pub(super) fn vertical_position(
    editor: &Editor,
    layouts: &ParagraphLayouts,
    navigation: Option<&VerticalNavigation>,
    down: bool,
) -> Option<NavigationTarget> {
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
    let (paragraph, target, cursor) = if down && row + 1 < layout.galley.rows.len() {
        let cursor = layout
            .galley
            .cursor_down_one_row(&cursor, Some(preferred_x - layout.rect.left()))
            .0;
        (focus.paragraph, layout, cursor)
    } else if !down && row > 0 {
        let cursor = layout
            .galley
            .cursor_up_one_row(&cursor, Some(preferred_x - layout.rect.left()))
            .0;
        (focus.paragraph, layout, cursor)
    } else if down && focus.paragraph + 1 < layouts.len() {
        let paragraph = focus.paragraph + 1;
        let target = layouts.get(paragraph)?;
        let y = target.galley.rows.first()?.rect().center().y;
        let cursor = target
            .galley
            .cursor_from_pos(Vec2::new(preferred_x - target.rect.left(), y));
        (paragraph, target, cursor)
    } else if !down && focus.paragraph > 0 {
        let paragraph = focus.paragraph - 1;
        let target = layouts.get(paragraph)?;
        let y = target.galley.rows.last()?.rect().center().y;
        let cursor = target
            .galley
            .cursor_from_pos(Vec2::new(preferred_x - target.rect.left(), y));
        (paragraph, target, cursor)
    } else {
        let cursor = if down {
            layout.galley.end()
        } else {
            layout.galley.begin()
        };
        (focus.paragraph, layout, cursor)
    };
    let source = editor.document().paragraph(paragraph)?;
    let x = preferred_x - target.rect.left();
    let (byte, prefer_next_row) = snap_vertical_cursor(source, &target.galley, cursor, x, down)?;
    Some(NavigationTarget {
        position: Position::new(paragraph, byte),
        preferred_x,
        prefer_next_row,
    })
}

pub(super) fn page_position(
    editor: &Editor,
    layouts: &ParagraphLayouts,
    navigation: Option<&VerticalNavigation>,
    down: bool,
    page: f32,
) -> Option<NavigationTarget> {
    let document = editor.document();
    let focus = editor.selection().focus;
    let prefer_next_row = navigation.is_none_or(|navigation| navigation.prefer_next_row);
    let caret = document_caret_rect(document, layouts, Pos2::ZERO, focus, prefer_next_row);
    let preferred_x = navigation.map_or(caret.center().x, |navigation| navigation.preferred_x);
    let y = caret.center().y + if down { page } else { -page };
    if y < 0.0 {
        return Some(NavigationTarget {
            position: Position::default(),
            preferred_x,
            prefer_next_row: true,
        });
    }
    if y > layouts.content_height()? {
        return Some(NavigationTarget {
            position: document.end(),
            preferred_x,
            prefer_next_row: true,
        });
    }
    let paragraph = layouts.paragraph_at_y(y).min(layouts.len().checked_sub(1)?);
    let target = layouts.get(paragraph)?;
    let x = preferred_x - target.rect.left();
    let cursor = target.galley.cursor_from_pos(Vec2::new(
        x,
        (y - target.rect.top()).clamp(0.0, target.galley.rect.bottom()),
    ));
    let source = document.paragraph(paragraph)?;
    let (byte, prefer_next_row) = snap_vertical_cursor(source, &target.galley, cursor, x, down)?;
    Some(NavigationTarget {
        position: Position::new(paragraph, byte),
        preferred_x,
        prefer_next_row,
    })
}

pub(super) fn row_boundary_position(
    editor: &Editor,
    layouts: &ParagraphLayouts,
    prefer_next_row: bool,
    end: bool,
) -> Option<NavigationTarget> {
    let focus = editor.selection().focus;
    let source = editor.document().paragraph(focus.paragraph)?;
    let layout = layouts.get(focus.paragraph)?;
    let cursor = CCursor {
        index: CharIndex(source.scalar_index(focus.byte)?),
        prefer_next_row,
    };
    let target = if end {
        layout.galley.cursor_end_of_row(&cursor)
    } else {
        layout.galley.cursor_begin_of_row(&cursor)
    };
    let target_x = layout.galley.pos_from_cursor(target).center().x;
    let (byte, prefer_next_row) =
        snap_vertical_cursor(source, &layout.galley, target, target_x, end)?;
    let preferred_x = layout.rect.left()
        + layout
            .galley
            .pos_from_cursor(CCursor {
                index: CharIndex(source.scalar_index(byte)?),
                prefer_next_row,
            })
            .center()
            .x;
    Some(NavigationTarget {
        position: Position::new(focus.paragraph, byte),
        preferred_x,
        prefer_next_row,
    })
}

pub(super) fn paragraph_boundary_position(editor: &Editor, end: bool) -> Position {
    let focus = editor.selection().focus;
    let document = editor.document();
    let length = document
        .paragraph(focus.paragraph)
        .expect("the editor keeps its selection inside the document")
        .text()
        .len();
    let paragraph = if end && focus.byte == length {
        (focus.paragraph + 1).min(document.paragraphs().len() - 1)
    } else if !end && focus.byte == 0 {
        focus.paragraph.saturating_sub(1)
    } else {
        focus.paragraph
    };
    Position::new(
        paragraph,
        if end {
            document
                .paragraph(paragraph)
                .expect("the target paragraph is clamped to the document")
                .text()
                .len()
        } else {
            0
        },
    )
}

pub(super) fn snap_vertical_cursor(
    source: &Paragraph,
    galley: &Galley,
    cursor: CCursor,
    x: f32,
    down: bool,
) -> Option<(usize, bool)> {
    let byte = source.byte_from_scalar(cursor.index.0)?;
    let before = source.boundary_at_or_before(byte);
    let after = source.boundary_at_or_after(byte);
    if before == after {
        return Some((byte, cursor.prefer_next_row));
    }
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
    let distance = |byte, prefer_next_row| {
        let index = CharIndex(source.scalar_index(byte)?);
        let caret = galley.pos_from_cursor(CCursor {
            index,
            prefer_next_row,
        });
        Some((caret.center().x - x).abs())
    };
    match (on_target_row(before), on_target_row(after)) {
        (Some(before_affinity), Some(after_affinity)) => Some(
            if distance(before, before_affinity)? <= distance(after, after_affinity)? {
                (before, before_affinity)
            } else {
                (after, after_affinity)
            },
        ),
        (Some(affinity), None) => Some((before, affinity)),
        (None, Some(affinity)) => Some((after, affinity)),
        (None, None) => Some(if down { (after, false) } else { (before, true) }),
    }
}
