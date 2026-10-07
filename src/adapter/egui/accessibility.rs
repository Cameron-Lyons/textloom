//! Egui's stock text helper selects scalar values. These runs use the same
//! extended grapheme boundaries as Textloom, including paragraph separators.

use super::ParagraphLayout;
use crate::{Editor, InlineStyle, Paragraph, ParagraphKind, Position};
use egui::{Galley, Id, Pos2, Rect, Ui, accesskit, text::CCursor};
use std::{fmt, ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessibilityError {
    CharacterTooLong { position: Position, bytes: usize },
    InvalidLayout,
    StaleDocument,
    InvalidTextPosition,
}

impl fmt::Display for AccessibilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CharacterTooLong { position, bytes } => write!(
                f,
                "grapheme at {}:{} is {bytes} bytes; AccessKit supports at most 255",
                position.paragraph, position.byte
            ),
            Self::InvalidLayout => f.write_str("accessibility layout does not match the document"),
            Self::StaleDocument => f.write_str("accessibility action refers to an older document"),
            Self::InvalidTextPosition => f.write_str("invalid accessibility text position"),
        }
    }
}
impl std::error::Error for AccessibilityError {}

#[derive(Debug)]
struct Run {
    id: Id,
    range: Range<usize>,
    value: String,
    lengths: Vec<u8>,
    /// Paragraph-local byte boundaries, without the synthetic separator.
    offsets: Vec<usize>,
    /// Matching scalar indices for egui's layout API.
    scalar_offsets: Vec<usize>,
    style: InlineStyle,
    row: usize,
    line_break: bool,
}

#[derive(Debug)]
struct ParagraphSnapshot {
    source: Arc<Paragraph>,
    galley: Arc<Galley>,
    id: Id,
    runs: Vec<Run>,
}

#[derive(Debug)]
pub(super) struct Snapshot {
    paragraphs: Vec<ParagraphSnapshot>,
}

fn paragraph_id(widget_id: Id, paragraph: usize) -> Id {
    widget_id.with(("paragraph", paragraph))
}

pub(super) fn build(
    editor: &Editor,
    layouts: &[ParagraphLayout],
    widget_id: Id,
) -> Result<Snapshot, AccessibilityError> {
    if layouts.len() != editor.document().paragraphs().len() {
        return Err(AccessibilityError::InvalidLayout);
    }
    let mut paragraphs = Vec::with_capacity(layouts.len());
    for (index, (source, layout)) in editor
        .document()
        .paragraphs()
        .iter()
        .zip(layouts)
        .enumerate()
    {
        let text = source.text();
        if layout.galley.job.text != text || layout.galley.rows.is_empty() {
            return Err(AccessibilityError::InvalidLayout);
        }
        let id = paragraph_id(widget_id, index);
        let scalar_boundaries: Vec<_> = text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(text.len()))
            .collect();
        let mut row_ends = Vec::with_capacity(layout.galley.rows.len());
        let mut scalar_end = 0;
        for row in &layout.galley.rows {
            scalar_end += row.char_count_including_newline().0;
            row_ends.push(scalar_end);
        }
        let mut runs: Vec<Run> = Vec::new();
        let mut span_index = 0;
        for (byte, grapheme) in text.grapheme_indices(true) {
            let length =
                u8::try_from(grapheme.len()).map_err(|_| AccessibilityError::CharacterTooLong {
                    position: Position::new(index, byte),
                    bytes: grapheme.len(),
                })?;
            while source
                .spans()
                .get(span_index)
                .is_some_and(|span| span.range.end <= byte)
            {
                span_index += 1;
            }
            let style = source
                .spans()
                .get(span_index)
                .map_or(InlineStyle::default(), |span| span.style);
            let scalar_start = scalar_boundaries
                .binary_search(&byte)
                .map_err(|_| AccessibilityError::InvalidLayout)?;
            let scalar_end = scalar_boundaries
                .binary_search(&(byte + grapheme.len()))
                .map_err(|_| AccessibilityError::InvalidLayout)?;
            let row = row_ends
                .partition_point(|end| *end <= scalar_start)
                .min(row_ends.len() - 1);
            let new_run = runs
                .last()
                .is_none_or(|run| run.row != row || run.style != style || run.lengths.len() == 255);
            if new_run {
                runs.push(Run {
                    id: id.with(("grapheme_run", runs.len())),
                    range: byte..byte,
                    value: String::new(),
                    lengths: Vec::new(),
                    offsets: vec![byte],
                    scalar_offsets: vec![scalar_start],
                    style,
                    row,
                    line_break: false,
                });
            }
            let run = runs.last_mut().expect("a run was created");
            run.range.end = byte + grapheme.len();
            run.value.push_str(grapheme);
            run.lengths.push(length);
            run.offsets.push(run.range.end);
            run.scalar_offsets.push(scalar_end);
        }
        if runs.is_empty() {
            runs.push(Run {
                id: id.with(("grapheme_run", 0usize)),
                range: 0..0,
                value: String::new(),
                lengths: Vec::new(),
                offsets: vec![0],
                scalar_offsets: vec![0],
                style: InlineStyle::default(),
                row: 0,
                line_break: false,
            });
        }
        if index + 1 < layouts.len() {
            let last = runs.last_mut().expect("empty paragraphs also have a run");
            last.value.push('\n');
            last.lengths.push(1);
            last.line_break = true;
        }
        paragraphs.push(ParagraphSnapshot {
            source: Arc::clone(source),
            galley: Arc::clone(&layout.galley),
            id,
            runs,
        });
    }
    Ok(Snapshot { paragraphs })
}

impl Snapshot {
    pub(super) fn matches_document(&self, editor: &Editor) -> bool {
        self.paragraphs.len() == editor.document().paragraphs().len()
            && self
                .paragraphs
                .iter()
                .zip(editor.document().paragraphs())
                .all(|(cached, current)| Arc::ptr_eq(&cached.source, current))
    }

    pub(super) fn is_current(&self, editor: &Editor, layouts: &[ParagraphLayout]) -> bool {
        self.matches_document(editor)
            && self.paragraphs.len() == layouts.len()
            && self
                .paragraphs
                .iter()
                .zip(layouts)
                .all(|(cached, layout)| Arc::ptr_eq(&cached.galley, &layout.galley))
    }

    pub(super) fn position(&self, position: Position) -> Option<accesskit::TextPosition> {
        let paragraph = self.paragraphs.get(position.paragraph)?;
        // Prefer the next run at a shared boundary. In particular, paragraph
        // end must point before its synthetic newline, never after it.
        for run in paragraph.runs.iter().rev() {
            if let Ok(character_index) = run.offsets.binary_search(&position.byte) {
                return Some(accesskit::TextPosition {
                    node: run.id.accesskit_id(),
                    character_index,
                });
            }
        }
        None
    }

    pub(super) fn decode(&self, position: accesskit::TextPosition) -> Option<Position> {
        for (index, paragraph) in self.paragraphs.iter().enumerate() {
            for run in &paragraph.runs {
                if run.id.accesskit_id() != position.node {
                    continue;
                }
                if let Some(byte) = run.offsets.get(position.character_index) {
                    return Some(Position::new(index, *byte));
                }
                if run.line_break && position.character_index == run.offsets.len() {
                    return Some(Position::new(index + 1, 0));
                }
                return None;
            }
        }
        None
    }

    pub(super) fn publish(
        &self,
        ui: &Ui,
        widget_id: Id,
        editor: &Editor,
        layouts: &[ParagraphLayout],
        origin: Pos2,
    ) {
        let selection = editor.selection();
        if ui
            .ctx()
            .accesskit_node_builder(widget_id, |node| {
                node.set_role(accesskit::Role::MultilineTextInput);
                if let (Some(anchor), Some(focus)) = (
                    self.position(selection.anchor),
                    self.position(selection.focus),
                ) {
                    node.set_text_selection(accesskit::TextSelection { anchor, focus });
                }
                if ui.is_enabled() {
                    node.add_action(accesskit::Action::SetTextSelection);
                    node.add_action(accesskit::Action::SetValue);
                    node.add_action(accesskit::Action::ReplaceSelectedText);
                }
            })
            .is_none()
        {
            return;
        }
        let to_global = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .unwrap_or_default();
        for (paragraph, layout) in self.paragraphs.iter().zip(layouts) {
            let paragraph_rect = layout.rect.translate(origin.to_vec2());
            let _paragraph_ui = Ui::new(
                ui.ctx().clone(),
                paragraph.id,
                egui::UiBuilder::new()
                    .accessibility_parent(widget_id)
                    .layer_id(ui.layer_id())
                    .max_rect(paragraph_rect),
            );
            ui.ctx().accesskit_node_builder(paragraph.id, |node| {
                node.set_role(match paragraph.source.kind() {
                    ParagraphKind::Body => accesskit::Role::Paragraph,
                    ParagraphKind::Heading { .. } => accesskit::Role::Heading,
                    ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. } => {
                        accesskit::Role::ListItem
                    }
                });
                node.set_is_line_breaking_object();
                node.set_bounds(accesskit_rect(to_global * paragraph_rect));
                match paragraph.source.kind() {
                    ParagraphKind::Heading { level } => node.set_level(usize::from(level)),
                    ParagraphKind::Bullet { indent } => {
                        node.set_level(usize::from(indent) + 1);
                        node.set_list_style(accesskit::ListStyle::Disc);
                    }
                    ParagraphKind::Ordered { indent, .. } => {
                        node.set_level(usize::from(indent) + 1);
                        node.set_list_style(accesskit::ListStyle::Numeric);
                    }
                    ParagraphKind::Body => {}
                }
            });
            let from_galley = to_global
                * egui::emath::TSTransform::from_translation(paragraph_rect.min.to_vec2());
            for (run_index, run) in paragraph.runs.iter().enumerate() {
                let geometry = run_geometry(run, &paragraph.galley);
                let local_rect = geometry.bounds.translate(paragraph_rect.min.to_vec2());
                let _run_ui = Ui::new(
                    ui.ctx().clone(),
                    run.id,
                    egui::UiBuilder::new()
                        .accessibility_parent(paragraph.id)
                        .layer_id(ui.layer_id())
                        .max_rect(local_rect),
                );
                ui.ctx().accesskit_node_builder(run.id, |node| {
                    node.set_role(accesskit::Role::TextRun);
                    node.set_value(run.value.clone());
                    node.set_character_lengths(run.lengths.clone());
                    node.set_text_direction(accesskit::TextDirection::LeftToRight);
                    node.set_bounds(accesskit_rect(from_galley * geometry.bounds));
                    if let (Some(positions), Some(widths)) = (&geometry.positions, &geometry.widths)
                    {
                        node.set_character_positions(
                            positions
                                .iter()
                                .map(|position| position * from_galley.scaling)
                                .collect::<Vec<_>>(),
                        );
                        node.set_character_widths(
                            widths
                                .iter()
                                .map(|width| width * from_galley.scaling)
                                .collect::<Vec<_>>(),
                        );
                    }
                    if let Some(previous) = run_index
                        .checked_sub(1)
                        .and_then(|index| paragraph.runs.get(index))
                        && previous.row == run.row
                    {
                        node.set_previous_on_line(previous.id.accesskit_id());
                    }
                    if let Some(next) = paragraph.runs.get(run_index + 1)
                        && next.row == run.row
                    {
                        node.set_next_on_line(next.id.accesskit_id());
                    }
                    publish_style(node, run, &paragraph.galley);
                });
            }
        }
    }
}

struct Geometry {
    bounds: Rect,
    positions: Option<Vec<f32>>,
    widths: Option<Vec<f32>>,
}

fn run_geometry(run: &Run, galley: &Galley) -> Geometry {
    let mut bounds = Rect::NOTHING;
    let mut positions = Vec::with_capacity(run.lengths.len());
    let mut widths = Vec::with_capacity(run.lengths.len());
    let mut single_row = true;
    for pair in run.scalar_offsets.windows(2) {
        let mut start_cursor = CCursor::new(pair[0]);
        start_cursor.prefer_next_row = true;
        let end_cursor = CCursor::new(pair[1]);
        let start = galley.pos_from_cursor(start_cursor);
        let end = galley.pos_from_cursor(end_cursor);
        single_row &= galley.layout_from_cursor(start_cursor).row
            == galley.layout_from_cursor(end_cursor).row;
        bounds = bounds.union(start).union(end);
        positions.push(start.min.x);
        widths.push((end.min.x - start.min.x).max(0.0));
    }
    if run.line_break || run.lengths.is_empty() {
        let caret = galley.pos_from_cursor(CCursor::new(*run.scalar_offsets.last().unwrap_or(&0)));
        bounds = bounds.union(caret);
        if run.line_break {
            positions.push(caret.min.x);
            widths.push(0.0);
        }
    }
    if !bounds.is_finite() {
        bounds = galley.rect;
    }
    for position in &mut positions {
        *position -= bounds.min.x;
    }
    Geometry {
        bounds,
        positions: single_row.then_some(positions),
        widths: single_row.then_some(widths),
    }
}

fn accesskit_rect(rect: Rect) -> accesskit::Rect {
    accesskit::Rect {
        x0: rect.min.x.into(),
        y0: rect.min.y.into(),
        x1: rect.max.x.into(),
        y1: rect.max.y.into(),
    }
}

fn publish_style(node: &mut accesskit::Node, run: &Run, galley: &Galley) {
    node.set_font_weight(if run.style.bold { 700.0 } else { 400.0 });
    if run.style.italic {
        node.set_italic();
    }
    if run.style.code {
        node.set_font_family("monospace");
    }
    let format = galley
        .job
        .sections
        .iter()
        .find(|section| {
            section
                .byte_range
                .contains(&egui::text::ByteIndex(run.range.start))
        })
        .or_else(|| galley.job.sections.last())
        .map(|section| &section.format);
    let rgba = format.map_or([0, 0, 0, 255], |format| {
        format.color.to_srgba_unmultiplied()
    });
    let color = accesskit::Color {
        red: rgba[0],
        green: rgba[1],
        blue: rgba[2],
        alpha: rgba[3],
    };
    node.set_foreground_color(color);
    if let Some(format) = format {
        node.set_font_size(format.font_id.size);
    }
    let decoration = accesskit::TextDecoration {
        style: accesskit::TextDecorationStyle::Solid,
        color,
    };
    if run.style.underline {
        node.set_underline(decoration);
    }
    if run.style.strikethrough {
        node.set_strikethrough(decoration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot(text: &str, width: f32) -> (Editor, Snapshot) {
        let (editor, result) = try_make_snapshot(text, width);
        (editor, result.unwrap())
    }

    fn try_make_snapshot(text: &str, width: f32) -> (Editor, Result<Snapshot, AccessibilityError>) {
        let editor = Editor::from_text(text);
        let context = egui::Context::default();
        let mut result = None;
        context
            .run_ui(egui::RawInput::default(), |ui| {
                let mut y = 0.0;
                let layouts: Vec<_> = editor
                    .document()
                    .paragraphs()
                    .iter()
                    .map(|paragraph| {
                        let job = egui::text::LayoutJob::simple(
                            paragraph.text().to_owned(),
                            egui::FontId::proportional(16.0),
                            egui::Color32::WHITE,
                            width,
                        );
                        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                        let rect = Rect::from_min_size(Pos2::new(0.0, y), galley.size());
                        y += galley.size().y;
                        ParagraphLayout {
                            galley,
                            rect,
                            marker: None,
                            marker_position: Pos2::ZERO,
                        }
                    })
                    .collect();
                result = Some(build(&editor, &layouts, Id::new("editor")));
            })
            .drop_without_applying_deltas();
        (editor, result.unwrap())
    }

    #[test]
    fn emoji_and_combining_marks_expose_only_real_caret_stops() {
        let (_, snapshot) = make_snapshot("a👩🏽‍💻e\u{301}", 500.0);
        let runs = &snapshot.paragraphs[0].runs;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].lengths, vec![1, 15, 3]);
        assert!(snapshot.position(Position::new(0, 5)).is_none());
        for byte in [0, 1, 16, 19] {
            let position = Position::new(0, byte);
            assert_eq!(
                snapshot.decode(snapshot.position(position).unwrap()),
                Some(position)
            );
        }
    }

    #[test]
    fn newline_is_selectable_and_decodes_to_the_next_paragraph() {
        let (_, snapshot) = make_snapshot("a\n\nb", 500.0);
        for index in 0..2 {
            let paragraph = &snapshot.paragraphs[index];
            let last = paragraph.runs.last().unwrap();
            assert!(last.value.ends_with('\n'));
            let end = Position::new(index, paragraph.source.text().len());
            let before_break = snapshot.position(end).unwrap();
            assert_eq!(before_break.character_index, last.lengths.len() - 1);
            assert_eq!(snapshot.decode(before_break), Some(end));
            assert_eq!(
                snapshot.decode(accesskit::TextPosition {
                    node: last.id.accesskit_id(),
                    character_index: last.lengths.len()
                }),
                Some(Position::new(index + 1, 0))
            );
        }
    }

    #[test]
    fn wrapping_and_chunking_preserve_graphemes() {
        let text = "e\u{301}".repeat(300);
        let (_, snapshot) = make_snapshot(&text, 5000.0);
        let runs = &snapshot.paragraphs[0].runs;
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].lengths.len(), 255);
        assert_eq!(runs[1].lengths.len(), 45);
        assert!(
            runs.iter()
                .flat_map(|run| &run.lengths)
                .all(|length| *length == 3)
        );
        for byte in (0..=text.len()).step_by(3) {
            let position = Position::new(0, byte);
            assert_eq!(
                snapshot.decode(snapshot.position(position).unwrap()),
                Some(position)
            );
        }
        let (_, wrapped) = make_snapshot("first second third fourth", 65.0);
        assert!(wrapped.paragraphs[0].runs.len() > 1);
        for run in &wrapped.paragraphs[0].runs {
            let geometry = run_geometry(run, &wrapped.paragraphs[0].galley);
            assert_eq!(
                geometry.positions.as_ref().unwrap().len(),
                run.lengths.len()
            );
        }
    }

    #[test]
    fn oversized_clusters_fail_explicitly_and_stale_documents_are_detected() {
        let text = format!("x{}", "\u{0301}".repeat(200));
        let (_, result) = try_make_snapshot(&text, 500.0);
        assert!(matches!(
            result,
            Err(AccessibilityError::CharacterTooLong { bytes: 401, .. })
        ));
        let (mut editor, snapshot) = make_snapshot("valid", 500.0);
        assert!(snapshot.matches_document(&editor));
        editor.insert_text("new").unwrap();
        assert!(!snapshot.matches_document(&editor));
    }
}
