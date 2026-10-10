//! Selection painting in visible paragraph layouts.
use super::layout::ParagraphLayouts;
use crate::{Document, Selection};
use egui::{Color32, Pos2, Rect, text::CharIndex};

pub(super) fn paint_document_selection(
    document: &Document,
    selection: Selection,
    layouts: &ParagraphLayouts,
    origin: Pos2,
    painter: &egui::Painter,
    color: Color32,
) {
    let selection = selection.range();
    if selection.start == selection.end {
        return;
    }
    let top = painter.clip_rect().top() - origin.y - 5.0;
    let bottom = painter.clip_rect().bottom() - origin.y + 5.0;
    let first = layouts
        .first_bottom_at_least(top)
        .max(selection.start.paragraph);
    let last = layouts
        .first_top_after(bottom)
        .min(selection.end.paragraph.saturating_add(1));
    for (offset, layout) in layouts.views_range(first..last).enumerate() {
        let index = first + offset;
        if !painter
            .clip_rect()
            .intersects(layout.rect.translate(origin.to_vec2()).expand(5.0))
        {
            continue;
        }
        let Some(source) = document.paragraph(index) else {
            continue;
        };
        let start = if index == selection.start.paragraph {
            selection.start.byte
        } else {
            0
        };
        let end = if index == selection.end.paragraph {
            selection.end.byte
        } else {
            source.text().len()
        };
        let char_start = source.scalar_index(start).unwrap_or(0);
        let char_end = source.scalar_index(end).unwrap_or(source.scalar_count());
        let mut row_start = 0;
        for (row_index, row) in layout.galley.rows.iter().enumerate() {
            let count = row.char_count_excluding_newline().0;
            let local_start = char_start.saturating_sub(row_start).min(count);
            let local_end = char_end.saturating_sub(row_start).min(count);
            let includes_break =
                index < selection.end.paragraph && row_index + 1 == layout.galley.rows.len();
            if local_start < local_end || includes_break {
                let left = row.pos.x + row.x_offset(CharIndex(local_start));
                let mut right = row.pos.x + row.x_offset(CharIndex(local_end));
                if includes_break {
                    right += 5.0;
                }
                let selected = Rect::from_min_max(
                    Pos2::new(left, row.rect().top()),
                    Pos2::new(right, row.rect().bottom()),
                );
                painter.rect_filled(
                    selected.translate(origin.to_vec2() + layout.rect.min.to_vec2()),
                    0.0,
                    color,
                );
            }
            row_start += row.char_count_including_newline().0;
        }
    }
}
