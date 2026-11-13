use super::layout::{ParagraphLayoutView, ParagraphLayouts};
use crate::{Document, Position, Selection};
use egui::epaint::FontsView;
use egui::epaint::text::Glyph;
use egui::text::{CharIndex, LayoutJob};
use egui::{Color32, FontFamily, FontId, Galley, Pos2, Rect};
use std::collections::HashMap;
use std::ops::{Range, RangeInclusive};
use std::slice;
use std::sync::Arc;

// Sorted, nonoverlapping document ranges and the fill painted behind them.
#[derive(Clone, Copy, Debug)]
pub(super) struct BackgroundRanges<'ranges> {
    pub(super) ranges: &'ranges [Range<Position>],
    pub(super) fill: Color32,
}

#[derive(Clone, Default)]
pub(super) struct PaintCache {
    probe: Option<Probe>,
    galleys: HashMap<usize, PaintedGalley>,
}

#[derive(Clone)]
struct Probe {
    galley: Arc<Galley>,
    fill: f32,
}

impl Probe {
    fn new(fonts: &mut FontsView<'_>, family: &FontFamily) -> Self {
        // No other text uses this size, so the glyph gets its own atlas slot.
        let font = FontId::new(7.0625, family.clone());
        let galley = fonts.layout_job(LayoutJob::simple_singleline(
            "g".to_owned(),
            font,
            Color32::WHITE,
        ));
        Self {
            galley,
            fill: fonts.font_atlas_fill_ratio(),
        }
    }

    fn glyph(&self) -> Option<&Glyph> {
        self.galley.rows.first()?.glyphs.first()
    }

    // Atlas recreation clears egui's galley cache, but so does any pass that skips
    // this widget, such as another viewport's pass. The fill ratio only grows until
    // recreation, and recreation also moves the probe's glyph.
    fn same_atlas(&self, earlier: &Self) -> bool {
        Arc::ptr_eq(&self.galley, &earlier.galley)
            || (self.fill >= earlier.fill
                && self
                    .glyph()
                    .zip(earlier.glyph())
                    .is_some_and(|(glyph, earlier)| {
                        !glyph.uv_rect.is_nothing() && glyph.uv_rect == earlier.uv_rect
                    }))
    }
}

#[derive(Clone)]
struct PaintedGalley {
    // Retaining the source prevents its address from being reused by another layout.
    source: Arc<Galley>,
    current: Arc<Galley>,
    backgrounds: Option<BackgroundGalley>,
    used: bool,
}

#[derive(Clone, PartialEq)]
struct BackgroundQuad {
    row: usize,
    rect: Rect,
    color: Color32,
}

#[derive(Clone)]
struct BackgroundGalley {
    quads: Vec<BackgroundQuad>,
    galley: Arc<Galley>,
}

impl PaintCache {
    fn begin(&mut self, fonts: &mut FontsView<'_>, family: &FontFamily) {
        // A tiny cached job detects atlas recreation without cloning and hashing
        // every visible paragraph on every idle frame.
        let probe = Probe::new(fonts, family);
        if self
            .probe
            .as_ref()
            .is_none_or(|earlier| !probe.same_atlas(earlier))
        {
            self.galleys.clear();
        }
        self.probe = Some(probe);
        for cached in self.galleys.values_mut() {
            cached.used = false;
        }
    }

    fn refresh(&mut self, fonts: &mut FontsView<'_>, source: &Arc<Galley>) -> Arc<Galley> {
        let cached = self
            .galleys
            .entry(Arc::as_ptr(source) as usize)
            .or_insert_with(|| PaintedGalley {
                source: source.clone(),
                current: fonts.layout_job((*source.job).clone()),
                backgrounds: None,
                used: true,
            });
        debug_assert!(
            Arc::ptr_eq(&cached.source, source),
            "painted galleys are keyed by source address, which the retained source keeps unique"
        );
        cached.used = true;
        cached.current.clone()
    }

    fn with_backgrounds(
        &mut self,
        source: &Arc<Galley>,
        quads: Vec<BackgroundQuad>,
    ) -> Arc<Galley> {
        let cached = self
            .galleys
            .get_mut(&(Arc::as_ptr(source) as usize))
            .expect("visible galleys were refreshed before their backgrounds");
        if let Some(backgrounds) = &cached.backgrounds
            && backgrounds.quads == quads
        {
            return backgrounds.galley.clone();
        }
        // Mutating a galley clones its entire row vector. Keep one prepared
        // variant per source so unchanged visible backgrounds do not copy
        // potentially hundreds of thousands of hidden rows on every frame.
        // Quad order captures the paint layers; coordinates stay galley-local.
        let mut galley = cached.current.clone();
        let mut first = usize::MAX;
        let mut end = 0;
        for quad in &quads {
            append_background(&mut galley, quad.row, quad.rect, quad.color);
            first = first.min(quad.row);
            end = end.max(quad.row + 1);
        }
        finish_backgrounds(&mut galley, &cached.current, first..end);
        cached.backgrounds = Some(BackgroundGalley {
            quads,
            galley: galley.clone(),
        });
        galley
    }

    fn end(&mut self) {
        self.galleys.retain(|_, cached| cached.used);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn paint_document(
    painter: &egui::Painter,
    cache: &mut PaintCache,
    document: &Document,
    layouts: &ParagraphLayouts,
    origin: Pos2,
    backgrounds: &[BackgroundRanges<'_>],
    selection: Option<Selection>,
    selection_fill: Color32,
    family: &FontFamily,
    text_color: Color32,
    quote_color: Color32,
) {
    let clip = painter.clip_rect();
    let top = clip.top() - origin.y;
    let bottom = clip.bottom() - origin.y;
    let first = layouts.first_bottom_at_least(top - 5.0);
    let last = layouts.first_top_after(bottom + 5.0).max(first);
    let mut visible = Vec::with_capacity(last - first);
    painter.fonts_mut(|fonts| {
        cache.begin(fonts, family);
        visible.extend(layouts.views_range(first..last).map(|layout| {
            let galley = cache.refresh(fonts, layout.galley);
            let marker = layout.marker.map(|marker| cache.refresh(fonts, marker));
            let offset = origin.y + layout.rect.min.y;
            let (top, bottom) = (clip.top() - offset - 5.0, clip.bottom() - offset + 5.0);
            let rows = &layout.galley.rows;
            let start = rows.partition_point(|row| row.rect().bottom() < top);
            let end = rows.partition_point(|row| row.rect().top() <= bottom);
            VisibleParagraph {
                layout,
                galley,
                marker,
                rows: start..end.max(start),
                backgrounds: Vec::new(),
            }
        }));
        cache.end();
    });
    // Consecutive quote paragraphs share one bar; the trailing `None` flushes the last one.
    let quote_bars = visible
        .iter()
        .map(|paragraph| paragraph.layout.quote_bar)
        .chain([None]);
    let mut bar: Option<Rect> = None;
    for next in quote_bars {
        match (bar.take(), next) {
            (Some(mut current), Some(next)) => {
                current.max.y = next.bottom();
                bar = Some(current);
            }
            (None, next) => bar = next,
            (Some(quote), None) => {
                painter.rect_filled(quote.translate(origin.to_vec2()), 0.0, quote_color);
            }
        }
    }
    let selection_range = selection.map(Selection::range);
    let selection_layer = selection_range.as_ref().map(|range| BackgroundRanges {
        ranges: slice::from_ref(range),
        fill: selection_fill,
    });
    let layers = backgrounds.iter().copied().chain(selection_layer);
    let window = visible_window(document, &visible, first);
    for BackgroundRanges { ranges, fill } in layers {
        for range in visible_ranges(ranges, window.as_ref()) {
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
                    fill,
                );
            }
        }
    }
    for paragraph in &mut visible {
        let rect = paragraph.layout.rect.translate(origin.to_vec2());
        if !painter.clip_rect().intersects(rect) {
            continue;
        }
        if !paragraph.backgrounds.is_empty() {
            paragraph.galley = cache.with_backgrounds(
                paragraph.layout.galley,
                std::mem::take(&mut paragraph.backgrounds),
            );
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

fn visible_window(
    document: &Document,
    visible: &[VisibleParagraph<'_>],
    first: usize,
) -> Option<RangeInclusive<Position>> {
    let (head, tail) = (visible.first()?, visible.last()?);
    let last = first + visible.len() - 1;
    let start = document
        .paragraph(first)
        .and_then(|paragraph| {
            paragraph.byte_from_scalar(
                head.layout
                    .row_index
                    .offset(head.layout.galley, head.rows.start),
            )
        })
        .unwrap_or(0);
    let end = match tail.rows.end.checked_sub(1) {
        None => Some(0),
        Some(row) => {
            let count = tail.layout.galley.rows[row]
                .char_count_excluding_newline()
                .0;
            document.paragraph(last).and_then(|paragraph| {
                paragraph
                    .byte_from_scalar(tail.layout.row_index.offset(tail.layout.galley, row) + count)
            })
        }
    }
    .unwrap_or(usize::MAX);
    Some(Position::new(first, start)..=Position::new(last, end))
}

fn visible_ranges<'a>(
    ranges: &'a [Range<Position>],
    window: Option<&RangeInclusive<Position>>,
) -> &'a [Range<Position>] {
    let Some(window) = window else {
        return &[];
    };
    let end = ranges.partition_point(|range| range.start <= *window.end());
    let start = ranges[..end].partition_point(|range| range.end <= *window.start());
    &ranges[start..end]
}

struct VisibleParagraph<'layout> {
    layout: ParagraphLayoutView<'layout>,
    galley: Arc<Galley>,
    marker: Option<Arc<Galley>>,
    rows: Range<usize>,
    backgrounds: Vec<BackgroundQuad>,
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
        let rows = &self.layout.galley.rows;
        let first = self
            .layout
            .row_index
            .row_at_scalar(chars.start)
            .max(self.rows.start);
        for row_index in first..self.rows.end {
            let row = &rows[row_index];
            let row_start = self.layout.row_index.offset(self.layout.galley, row_index);
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
            if !(local_start < local_end || row_break) {
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
                self.backgrounds.push(BackgroundQuad {
                    row: row_index,
                    rect: local,
                    color,
                });
            }
        }
    }
}

fn finish_backgrounds(galley: &mut Arc<Galley>, unpainted: &Galley, rows: Range<usize>) {
    // Appending each highlight is constant time. Move all added triangles
    // behind the glyphs once per row, keeping the layers in paint order.
    // Rotating after each match makes a heavily highlighted row quadratic.
    let Some(galley) = Arc::get_mut(galley) else {
        return;
    };
    for index in rows {
        let Some(row) = Arc::get_mut(&mut galley.rows[index].row) else {
            continue;
        };
        let visuals = &mut row.visuals;
        let added = visuals.mesh.indices.len() - unpainted.rows[index].visuals.mesh.indices.len();
        visuals.mesh.indices[visuals.glyph_index_start..].rotate_right(added);
        visuals.glyph_index_start += added;
    }
}

fn append_background(galley: &mut Arc<Galley>, row_index: usize, rect: Rect, color: Color32) {
    let galley = Arc::make_mut(galley);
    let placed = &mut galley.rows[row_index];
    let row_bounds = rect.translate(placed.pos.to_vec2());
    let row = Arc::make_mut(&mut placed.row);
    let mesh = &mut row.visuals.mesh;
    mesh.add_colored_rect(rect, color);
    row.visuals.mesh_bounds = row.visuals.mesh_bounds.union(rect);
    galley.mesh_bounds = galley.mesh_bounds.union(row_bounds);
    galley.num_vertices += 4;
    galley.num_indices += 6;
}
