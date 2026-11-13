//! Scalar offsets for visual rows, shared by navigation and painting.

use egui::epaint::text::cursor::LayoutCursor;
use egui::text::{CCursor, CharIndex};
use egui::{Galley, Rect, Vec2};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub(crate) struct RowIndex {
    // Single-row paragraphs need no allocation. Longer paragraphs include a
    // final offset after the last row, like the index painting previously kept.
    starts: Option<Arc<[usize]>>,
    ordered_y: bool,
}

impl RowIndex {
    pub(crate) fn new(galley: &Galley) -> Self {
        let mut starts = (galley.rows.len() > 1).then(|| {
            let mut starts = Vec::with_capacity(galley.rows.len() + 1);
            starts.push(0);
            starts
        });
        let mut offset = 0;
        let mut ordered_y = true;
        let (mut previous_min, mut previous_max) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        for row in &galley.rows {
            if let Some(starts) = &mut starts {
                offset += row.char_count_including_newline().0;
                starts.push(offset);
            }
            let (min, max) = (row.min_y(), row.max_y());
            ordered_y &= min.is_finite()
                && max.is_finite()
                && min <= max
                && previous_min <= min
                && previous_max <= max;
            (previous_min, previous_max) = (min, max);
        }
        Self {
            starts: starts.map(Into::into),
            ordered_y,
        }
    }

    /// Matches egui's inclusive row bounds, five-point outer margin, and
    /// earliest-row preference when distances tie. Geometry is normally
    /// ordered, but unusual fonts or coordinates can require egui's scan.
    pub(crate) fn cursor_from_pos(&self, galley: &Galley, pos: Vec2) -> CCursor {
        if !self.ordered_y {
            return galley.cursor_from_pos(pos);
        }
        let Some(first) = galley.rows.first() else {
            return CCursor::default();
        };
        let last = galley.rows.last().expect("there is a first row");
        if pos.y < first.min_y() - 5.0 {
            return CCursor::default();
        }
        if last.max_y() + 5.0 < pos.y {
            return self.end(galley);
        }
        if pos.y.is_nan() {
            return CCursor::default();
        }
        let next = galley.rows.partition_point(|row| row.max_y() < pos.y);
        let row = if next < galley.rows.len() && galley.rows[next].min_y() <= pos.y {
            // At a shared edge or overlap, this is the first containing row.
            next
        } else if next == 0 {
            0
        } else {
            let previous_distance = (galley.rows[next - 1].max_y() - pos.y).abs();
            if next < galley.rows.len()
                && (galley.rows[next].min_y() - pos.y).abs() < previous_distance
            {
                next
            } else {
                // Equal bottom edges and f32 subtraction can both produce
                // distance plateaus. Preserve the earliest equal candidate.
                galley.rows[..next]
                    .partition_point(|row| (row.max_y() - pos.y).abs() > previous_distance)
            }
        };
        let placed = &galley.rows[row];
        self.cursor_from_layout(
            galley,
            LayoutCursor {
                row,
                column: placed.char_at(pos.x - placed.pos.x),
            },
        )
    }

    /// Hover hits use an exclusive bottom edge and never select a nearby row.
    pub(crate) fn containing_row(&self, galley: &Galley, y: f32) -> Option<usize> {
        if !self.ordered_y {
            return galley
                .rows
                .iter()
                .position(|row| row.min_y() <= y && y < row.max_y());
        }
        let row = galley.rows.partition_point(|row| row.max_y() <= y);
        galley
            .rows
            .get(row)
            .filter(|row| row.min_y() <= y && y < row.max_y())
            .map(|_| row)
    }

    pub(crate) fn offset(&self, galley: &Galley, row: usize) -> usize {
        self.starts.as_ref().map_or_else(
            || {
                if row == 0 {
                    0
                } else {
                    galley
                        .rows
                        .first()
                        .map_or(0, |row| row.char_count_including_newline().0)
                }
            },
            |starts| starts[row],
        )
    }

    pub(crate) fn row_at_scalar(&self, scalar: usize) -> usize {
        self.starts.as_ref().map_or(0, |starts| {
            starts[..starts.len() - 1]
                .partition_point(|&start| start <= scalar)
                .saturating_sub(1)
        })
    }

    pub(crate) fn end(&self, galley: &Galley) -> CCursor {
        if galley.rows.is_empty() {
            return CCursor::default();
        }
        CCursor {
            index: CharIndex(self.offset(galley, galley.rows.len())),
            prefer_next_row: true,
        }
    }

    pub(crate) fn layout_from_cursor(&self, galley: &Galley, cursor: CCursor) -> LayoutCursor {
        // Find the first row whose text end reaches the cursor. Unlike the
        // next row's start, that end excludes an explicit newline.
        let mut low = 0;
        let mut high = galley.rows.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let end =
                self.offset(galley, middle) + galley.rows[middle].char_count_excluding_newline().0;
            if end < cursor.index.0 {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let mut row = low;
        while let Some(placed) = galley.rows.get(row) {
            let start = self.offset(galley, row);
            if start > cursor.index.0 {
                break;
            }
            let column = cursor.index.0 - start;
            if !(cursor.prefer_next_row
                && !placed.ends_with_newline
                && column >= placed.char_count_excluding_newline().0)
            {
                return LayoutCursor {
                    row,
                    column: CharIndex(column),
                };
            }
            row += 1;
        }
        galley
            .rows
            .last()
            .map_or_else(LayoutCursor::default, |row| LayoutCursor {
                row: galley.rows.len() - 1,
                column: row.char_count_including_newline(),
            })
    }

    pub(crate) fn cursor_from_layout(&self, galley: &Galley, cursor: LayoutCursor) -> CCursor {
        let Some(row) = galley.rows.get(cursor.row) else {
            return self.end(galley);
        };
        let count = row.char_count_excluding_newline();
        CCursor {
            index: CharIndex(self.offset(galley, cursor.row)) + cursor.column.min(count),
            prefer_next_row: cursor.column < count,
        }
    }

    pub(crate) fn pos_from_cursor(&self, galley: &Galley, cursor: CCursor) -> Rect {
        galley.pos_from_layout_cursor(&self.layout_from_cursor(galley, cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(context: &egui::Context, text: &str, width: f32) -> Arc<Galley> {
        let mut galley = None;
        context
            .run_ui(egui::RawInput::default(), |ui| {
                galley = Some(ui.fonts_mut(|fonts| {
                    fonts.layout_job(egui::text::LayoutJob::simple(
                        text.to_owned(),
                        egui::FontId::proportional(16.0),
                        egui::Color32::WHITE,
                        width,
                    ))
                }));
            })
            .drop_without_applying_deltas();
        galley.unwrap()
    }

    #[test]
    fn indexed_cursors_match_egui_at_wraps_newlines_and_out_of_bounds() {
        let context = egui::Context::default();
        for text in [
            "",
            "\n",
            "a\n\n",
            "office affine words",
            "a\u{301}👩🏽\u{200d}💻\nshort\nlonger line\n",
            "many words wrap onto short lines",
        ] {
            for width in [1.0, 13.0, 47.0, 500.0] {
                let galley = layout(&context, text, width);
                let rows = RowIndex::new(&galley);
                assert_eq!(rows.starts.is_none(), galley.rows.len() <= 1);
                assert_eq!(rows.end(&galley).index, galley.end().index);
                assert_eq!(
                    rows.end(&galley).prefer_next_row,
                    galley.end().prefer_next_row
                );
                for index in 0..=galley.end().index.0 + 2 {
                    for prefer_next_row in [false, true] {
                        let cursor = CCursor {
                            index: CharIndex(index),
                            prefer_next_row,
                        };
                        let expected = galley.layout_from_cursor(cursor);
                        assert_eq!(
                            rows.layout_from_cursor(&galley, cursor),
                            expected,
                            "text={text:?}, width={width}, cursor={cursor:?}"
                        );
                        assert_eq!(
                            rows.pos_from_cursor(&galley, cursor),
                            galley.pos_from_cursor(cursor)
                        );
                        for end in [false, true] {
                            let column = if end {
                                galley.rows[expected.row].char_count_excluding_newline()
                            } else {
                                CharIndex(0)
                            };
                            let actual = rows.cursor_from_layout(
                                &galley,
                                LayoutCursor {
                                    row: expected.row,
                                    column,
                                },
                            );
                            let expected = if end {
                                galley.cursor_end_of_row(&cursor)
                            } else {
                                galley.cursor_begin_of_row(&cursor)
                            };
                            assert_eq!(actual.index, expected.index);
                            assert_eq!(actual.prefer_next_row, expected.prefer_next_row);
                        }
                        for x in [-10.0, 0.0, 5.0, 25.0, 500.0] {
                            for down in [false, true] {
                                let target = if down {
                                    expected
                                        .row
                                        .checked_add(1)
                                        .filter(|&row| row < galley.rows.len())
                                } else {
                                    expected.row.checked_sub(1)
                                };
                                if let Some(row) = target {
                                    let placed = &galley.rows[row];
                                    let actual = rows.cursor_from_layout(
                                        &galley,
                                        LayoutCursor {
                                            row,
                                            column: placed.char_at(x - placed.pos.x),
                                        },
                                    );
                                    let expected = if down {
                                        galley.cursor_down_one_row(&cursor, Some(x)).0
                                    } else {
                                        galley.cursor_up_one_row(&cursor, Some(x)).0
                                    };
                                    assert_eq!(
                                        (actual.index, actual.prefer_next_row),
                                        (expected.index, expected.prefer_next_row),
                                        "text={text:?}, width={width}, cursor={cursor:?}, x={x}, down={down}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn assert_position_parity(galley: &Galley, extra_y: &[f32]) {
        let index = RowIndex::new(galley);
        let mut ys = vec![
            f32::NAN,
            f32::NEG_INFINITY,
            f32::INFINITY,
            -f32::MAX,
            f32::MAX,
            -10.0,
            0.0,
            10.0,
        ];
        ys.extend_from_slice(extra_y);
        for row in &galley.rows {
            for y in [
                row.min_y(),
                row.max_y(),
                row.rect().center().y,
                row.min_y() - 5.0,
                row.max_y() + 5.0,
            ] {
                ys.extend([y.next_down(), y, y.next_up()]);
            }
        }
        for y in ys {
            let expected_row = galley
                .rows
                .iter()
                .position(|row| row.min_y() <= y && y < row.max_y());
            assert_eq!(
                index.containing_row(galley, y),
                expected_row,
                "y={y:?}, ordered={}",
                index.ordered_y
            );
            for x in [
                f32::NEG_INFINITY,
                f32::INFINITY,
                f32::NAN,
                -10.0,
                0.0,
                4.0,
                25.0,
                100.0,
            ] {
                let expected = galley.cursor_from_pos(Vec2::new(x, y));
                let actual = index.cursor_from_pos(galley, Vec2::new(x, y));
                assert_eq!(
                    (actual.index, actual.prefer_next_row),
                    (expected.index, expected.prefer_next_row),
                    "x={x:?}, y={y:?}, ordered={}",
                    index.ordered_y
                );
            }
        }
    }

    #[test]
    fn indexed_pointer_rows_match_egui_and_exclusive_hover_bounds() {
        let context = egui::Context::default();
        for scale in [0.75, 1.0, 1.25, 2.0] {
            context.set_pixels_per_point(scale);
            for text in [
                "",
                "\n",
                "a\n\n",
                "office affine wrapped words",
                "a\u{301}👩🏽\u{200d}💻\nshort\nlonger line\n",
            ] {
                for width in [1.0, 27.0, 67.0, 500.0] {
                    assert_position_parity(&layout(&context, text, width), &[]);
                }
            }
        }
    }

    #[test]
    fn indexed_pointer_rows_preserve_gaps_overlaps_zero_heights_and_float_ties() {
        let context = egui::Context::default();
        let original = layout(&context, "first\nsecond\nthird", 500.0);
        assert_eq!(original.rows.len(), 3);
        for (bounds, ordered) in [
            ([(0.0, 10.0), (10.0, 10.0), (20.0, 30.0)], true),
            ([(0.0, 0.0), (0.0, 0.0), (0.0, 0.0)], true),
            ([(0.0, 20.0), (10.0, 30.0), (20.0, 40.0)], true),
            ([(0.0, 30.0), (10.0, 20.0), (20.0, 40.0)], false),
            ([(10.0, 20.0), (0.0, 30.0), (20.0, 40.0)], false),
            (
                [(0.0, 0.0), (1.0, 1.0), (100_000_000.0, 100_000_000.0)],
                true,
            ),
            (
                [(-f32::MAX, -f32::MAX), (-10.0, 10.0), (f32::MAX, f32::MAX)],
                true,
            ),
            ([(0.0, 10.0), (f32::NAN, 20.0), (20.0, 30.0)], false),
            ([(0.0, 10.0), (10.0, f32::INFINITY), (20.0, 30.0)], false),
            ([(0.0, 10.0), (20.0, 10.0), (20.0, 30.0)], false),
        ] {
            let mut galley = (*original).clone();
            for (row, (min, max)) in galley.rows.iter_mut().zip(bounds) {
                row.pos.y = min;
                Arc::make_mut(&mut row.row).size.y = max - min;
            }
            assert_eq!(
                RowIndex::new(&galley).ordered_y,
                ordered,
                "bounds={bounds:?}"
            );
            assert_position_parity(&galley, &[5.0, 10.0, 15.0, 20.0, 50_000_000.0]);
        }
        let mut empty = (*original).clone();
        empty.rows.clear();
        assert_position_parity(&empty, &[]);
    }

    #[test]
    fn multirow_indices_share_offsets_when_cloned() {
        let context = egui::Context::default();
        context
            .run_ui(egui::RawInput::default(), |ui| {
                let galley = ui.fonts_mut(|fonts| {
                    fonts.layout_job(egui::text::LayoutJob::simple(
                        "first\nsecond\n".to_owned(),
                        egui::FontId::proportional(16.0),
                        egui::Color32::WHITE,
                        40.0,
                    ))
                });
                let rows = RowIndex::new(&galley);
                let clone = rows.clone();
                assert!(Arc::ptr_eq(
                    rows.starts.as_ref().unwrap(),
                    clone.starts.as_ref().unwrap()
                ));
            })
            .drop_without_applying_deltas();
    }
}
