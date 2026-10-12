use super::layout::{ParagraphLayoutView, ParagraphLayouts};
use crate::{Document, Position, Selection};
use egui::{Color32, Galley, Pos2, Rect, text::CharIndex};
use std::{ops::Range, sync::Arc};

pub(super) type BackgroundRanges<'a> = (&'a [Range<Position>], Color32);

#[allow(clippy::too_many_arguments)]
pub(super) fn paint_document(
    painter: &egui::Painter,
    document: &Document,
    layouts: &ParagraphLayouts,
    origin: Pos2,
    backgrounds: &[BackgroundRanges<'_>],
    selection: Option<(Selection, Color32)>,
    text_color: Color32,
    quote_color: Color32,
) {
    let top = painter.clip_rect().top() - origin.y;
    let bottom = painter.clip_rect().bottom() - origin.y;
    let first = layouts.first_bottom_at_least(top - 5.0);
    let last = layouts.first_top_after(bottom + 5.0).max(first);
    let mut visible: Vec<_> = layouts
        .views_range(first..last)
        .map(|layout| VisibleParagraph {
            galley: layout.galley.clone(),
            marker: layout.marker.cloned(),
            layout,
            row_starts: None,
        })
        .collect();
    painter.fonts_mut(|fonts| {
        for paragraph in &mut visible {
            paragraph.galley = fonts.layout_job((*paragraph.layout.galley.job).clone());
            if let Some(marker) = paragraph.layout.marker {
                paragraph.marker = Some(fonts.layout_job((*marker.job).clone()));
            }
        }
    });
    let mut bar: Option<Rect> = None;
    for next in visible
        .iter()
        .map(|paragraph| paragraph.layout.quote_bar)
        .chain([None])
    {
        match (&mut bar, next) {
            (Some(bar), Some(next)) => bar.max.y = next.bottom(),
            (None, next) => bar = next,
            (Some(_), None) => {
                let quote = bar.take().expect("matched a bar");
                painter.rect_filled(quote.translate(origin.to_vec2()), 0.0, quote_color);
            }
        }
    }
    let selection_range = selection.map(|(selection, color)| (selection.range(), color));
    let layers = backgrounds.iter().copied().chain(
        selection_range
            .as_ref()
            .map(|(range, color)| (std::slice::from_ref(range), *color)),
    );
    for (ranges, color) in layers {
        let window = visible_ranges(ranges, first..last);
        for range in window {
            if range.start >= range.end
                || !fits(document, range.start)
                || !fits(document, range.end)
            {
                continue;
            }
            let paragraphs =
                range.start.paragraph.max(first)..range.end.paragraph.saturating_add(1).min(last);
            for index in paragraphs {
                let Some(source) = document.paragraph(index) else {
                    break;
                };
                let start = if index == range.start.paragraph {
                    range.start.byte
                } else {
                    0
                };
                let (end, includes_break) = if index == range.end.paragraph {
                    (range.end.byte, false)
                } else {
                    (source.text().len(), true)
                };
                let (Some(char_start), Some(char_end)) =
                    (source.scalar_index(start), source.scalar_index(end))
                else {
                    continue;
                };
                visible[index - first].paint_range(
                    painter,
                    origin,
                    char_start..char_end,
                    includes_break,
                    color,
                );
            }
        }
    }
    for paragraph in &visible {
        let rect = paragraph.layout.rect.translate(origin.to_vec2());
        if !painter.clip_rect().intersects(rect) {
            continue;
        }
        if let Some(marker) = &paragraph.marker {
            painter.galley(
                origin + paragraph.layout.marker_position.to_vec2(),
                marker.clone(),
                text_color,
            );
        }
        painter.galley(rect.min, paragraph.galley.clone(), text_color);
    }
}

fn fits(document: &Document, position: Position) -> bool {
    document
        .paragraph(position.paragraph)
        .is_some_and(|paragraph| paragraph.text().is_char_boundary(position.byte))
}

fn visible_ranges(ranges: &[Range<Position>], visible: Range<usize>) -> &[Range<Position>] {
    let first = Position::new(visible.start, 0);
    let after = Position::new(visible.end, 0);
    let end = ranges.partition_point(|range| range.start < after);
    let start = ranges[..end].partition_point(|range| range.end <= first);
    &ranges[start..end]
}

struct VisibleParagraph<'a> {
    layout: ParagraphLayoutView<'a>,
    galley: Arc<Galley>,
    marker: Option<Arc<Galley>>,
    row_starts: Option<Vec<usize>>,
}

impl VisibleParagraph<'_> {
    fn paint_range(
        &mut self,
        painter: &egui::Painter,
        origin: Pos2,
        chars: Range<usize>,
        includes_break: bool,
        color: Color32,
    ) {
        let offset = origin.to_vec2() + self.layout.rect.min.to_vec2();
        let top = painter.clip_rect().top() - offset.y - 5.0;
        let bottom = painter.clip_rect().bottom() - offset.y + 5.0;
        let rows = &self.layout.galley.rows;
        let starts = self.row_starts.get_or_insert_with(|| {
            rows.iter()
                .scan(0, |start, row| {
                    let row_start = *start;
                    *start += row.char_count_including_newline().0;
                    Some(row_start)
                })
                .collect()
        });
        let first = starts
            .partition_point(|&start| start <= chars.start)
            .saturating_sub(1);
        for (row_index, row) in rows.iter().enumerate().skip(first) {
            let row_start = starts[row_index];
            if row_start > chars.end {
                break;
            }
            let count = row.char_count_excluding_newline().0;
            let local_start = chars.start.saturating_sub(row_start).min(count);
            let local_end = chars.end.saturating_sub(row_start).min(count);
            let row_break = if row.ends_with_newline {
                chars.contains(&(row_start + count))
            } else {
                includes_break && row_index + 1 == rows.len()
            };
            if !(local_start < local_end || row_break)
                || row.rect().bottom() < top
                || row.rect().top() > bottom
            {
                continue;
            }
            let left = row.x_offset(CharIndex(local_start));
            let mut right = row.x_offset(CharIndex(local_end));
            if row_break {
                right += 5.0;
            }
            let local = Rect::from_min_max(Pos2::new(left, 0.0), Pos2::new(right, row.size.y));
            if row.visuals.glyph_index_start == 0 {
                painter.rect_filled(local.translate(offset + row.pos.to_vec2()), 0.0, color);
            } else {
                splice_background(&mut self.galley, row_index, local, color);
            }
        }
    }
}

fn splice_background(galley: &mut Arc<Galley>, row_index: usize, rect: Rect, color: Color32) {
    let galley = Arc::make_mut(galley);
    let placed = &mut galley.rows[row_index];
    let row_bounds = rect.translate(placed.pos.to_vec2());
    let row = Arc::make_mut(&mut placed.row);
    let mesh = &mut row.visuals.mesh;
    mesh.add_colored_rect(rect, color);
    mesh.indices[row.visuals.glyph_index_start..].rotate_right(6);
    row.visuals.glyph_index_start += 6;
    row.visuals.mesh_bounds = row.visuals.mesh_bounds.union(rect);
    galley.mesh_bounds = galley.mesh_bounds.union(row_bounds);
    galley.num_vertices += 4;
    galley.num_indices += 6;
}
