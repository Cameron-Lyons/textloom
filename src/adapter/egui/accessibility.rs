//! Egui's stock text helper selects scalar values. These runs use the same
//! extended grapheme boundaries as Textloom, including paragraph separators.

use super::ParagraphLayout;
use crate::{Editor, InlineStyle, Paragraph, ParagraphKind, Position};
use egui::{Galley, Id, Pos2, Rect, Ui, accesskit, text::CCursor};
use std::{collections::HashMap, fmt, ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;

/// Failure to publish or apply the widget's native accessibility text model.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessibilityError {
    /// A grapheme exceeds AccessKit's maximum selectable UTF-8 character length.
    CharacterTooLong {
        /// Start of the unsupported grapheme in document coordinates.
        position: Position,
        /// UTF-8 byte length of the grapheme.
        bytes: usize,
    },
    /// The rendered paragraphs do not match the committed document.
    InvalidLayout,
    /// An action targets a text snapshot superseded by an edit.
    StaleDocument,
    /// A requested caret stop is absent from the published text runs.
    InvalidTextPosition,
    /// A recognized text action has missing or incompatible data.
    InvalidActionData,
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
            Self::InvalidActionData => {
                f.write_str("accessibility action has missing or incompatible data")
            }
        }
    }
}
impl std::error::Error for AccessibilityError {}

#[derive(Debug)]
struct Run {
    id: Id,
    range: Range<usize>,
    /// Paragraph-local byte boundaries, without the synthetic separator.
    offsets: Vec<usize>,
    line_break: bool,
    node: accesskit::Node,
    geometry: Geometry,
}

#[derive(Debug)]
struct RunBuilder {
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
    line_break: bool,
    runs: Vec<Run>,
}

#[derive(Debug)]
pub(super) struct Snapshot {
    paragraphs: Vec<Arc<ParagraphSnapshot>>,
    document_identity: Arc<()>,
    run_locations: HashMap<accesskit::NodeId, (usize, usize)>,
}

fn paragraph_id(widget_id: Id, paragraph: usize) -> Id {
    widget_id.with(("paragraph", paragraph))
}

#[cfg(test)]
fn build(
    editor: &Editor,
    layouts: &[ParagraphLayout],
    widget_id: Id,
) -> Result<Snapshot, AccessibilityError> {
    build_cached(editor, layouts, widget_id, None)
}

pub(super) fn build_cached(
    editor: &Editor,
    layouts: &[ParagraphLayout],
    widget_id: Id,
    previous: Option<&Snapshot>,
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
        let id = paragraph_id(widget_id, index);
        let line_break = index + 1 < layouts.len();
        if let Some(cached) = previous.and_then(|snapshot| snapshot.paragraphs.get(index))
            && cached.id == id
            && cached.line_break == line_break
            && Arc::ptr_eq(&cached.source, source)
            && Arc::ptr_eq(&cached.galley, &layout.galley)
        {
            // Both immutable inputs were validated when this paragraph was
            // built. Reuse its text runs and geometry after unrelated edits.
            paragraphs.push(Arc::clone(cached));
            continue;
        }
        let text = source.text();
        if layout.galley.job.text != text || layout.galley.rows.is_empty() {
            return Err(AccessibilityError::InvalidLayout);
        }
        let mut row_ends = Vec::with_capacity(layout.galley.rows.len());
        let mut scalar_end = 0;
        for row in &layout.galley.rows {
            scalar_end += row.char_count_including_newline().0;
            row_ends.push(scalar_end);
        }
        let mut runs: Vec<RunBuilder> = Vec::new();
        let mut span_index = 0;
        let mut scalar_start = 0;
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
            let scalar_end = scalar_start + grapheme.chars().count();
            let row = row_ends
                .partition_point(|end| *end <= scalar_start)
                .min(row_ends.len() - 1);
            let new_run = runs
                .last()
                .is_none_or(|run| run.row != row || run.style != style || run.lengths.len() == 255);
            if new_run {
                runs.push(RunBuilder {
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
            scalar_start = scalar_end;
        }
        if runs.is_empty() {
            runs.push(RunBuilder {
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
        if line_break {
            let last = runs.last_mut().expect("empty paragraphs also have a run");
            last.value.push('\n');
            last.lengths.push(1);
            last.line_break = true;
        }
        // Freeze text, style, line links, and geometry once. Publication only
        // clones the node properties and applies the current widget transform.
        let mut builders = runs.into_iter().peekable();
        let mut runs = Vec::with_capacity(builders.len());
        let mut previous = None;
        while let Some(builder) = builders.next() {
            let previous_on_line = previous
                .filter(|(_, row)| *row == builder.row)
                .map(|(id, _)| id);
            let next_on_line = builders
                .peek()
                .filter(|next| next.row == builder.row)
                .map(|next| next.id);
            previous = Some((builder.id, builder.row));
            runs.push(freeze_run(
                builder,
                &layout.galley,
                previous_on_line,
                next_on_line,
            ));
        }
        paragraphs.push(Arc::new(ParagraphSnapshot {
            source: Arc::clone(source),
            galley: Arc::clone(&layout.galley),
            id,
            line_break,
            runs,
        }));
    }
    let run_locations = paragraphs
        .iter()
        .enumerate()
        .flat_map(|(paragraph_index, paragraph)| {
            paragraph
                .runs
                .iter()
                .enumerate()
                .map(move |(run_index, run)| (run.id.accesskit_id(), (paragraph_index, run_index)))
        })
        .collect();
    Ok(Snapshot {
        paragraphs,
        document_identity: editor.document().content_identity(),
        run_locations,
    })
}

impl Snapshot {
    pub(super) fn matches_document(&self, editor: &Editor) -> bool {
        Arc::ptr_eq(
            &self.document_identity,
            &editor.document().content_identity(),
        )
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
        let index = paragraph
            .runs
            .partition_point(|run| run.range.start <= position.byte)
            .checked_sub(1)?;
        if let Some(run) = paragraph.runs.get(index)
            && let Ok(character_index) = run.offsets.binary_search(&position.byte)
        {
            return Some(accesskit::TextPosition {
                node: run.id.accesskit_id(),
                character_index,
            });
        }
        None
    }

    pub(super) fn decode(&self, position: accesskit::TextPosition) -> Option<Position> {
        let &(index, run_index) = self.run_locations.get(&position.node)?;
        let run = &self.paragraphs[index].runs[run_index];
        if let Some(byte) = run.offsets.get(position.character_index) {
            return Some(Position::new(index, *byte));
        }
        if run.line_break && position.character_index == run.offsets.len() {
            return Some(Position::new(index + 1, 0));
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
        read_only: bool,
    ) {
        let selection = editor.selection();
        if ui
            .ctx()
            .accesskit_node_builder(widget_id, |node| {
                node.set_role(accesskit::Role::MultilineTextInput);
                if read_only {
                    node.set_read_only();
                }
                if let (Some(anchor), Some(focus)) = (
                    self.position(selection.anchor),
                    self.position(selection.focus),
                ) {
                    node.set_text_selection(accesskit::TextSelection { anchor, focus });
                }
                if ui.is_enabled() {
                    node.add_action(accesskit::Action::SetTextSelection);
                    if !read_only {
                        node.add_action(accesskit::Action::SetValue);
                        node.add_action(accesskit::Action::ReplaceSelectedText);
                    }
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
            let paragraph_ui = Ui::new(
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
            for run in &paragraph.runs {
                let geometry = &run.geometry;
                let local_rect = geometry.bounds.translate(paragraph_rect.min.to_vec2());
                // Register the same empty hover widget and accessibility parent
                // as a standalone Ui, without allocating its layout and stack.
                paragraph_ui.interact(
                    Rect::from_min_size(local_rect.min, egui::Vec2::ZERO),
                    run.id,
                    egui::Sense::hover(),
                );
                ui.ctx().accesskit_node_builder(run.id, |node| {
                    *node = run.node.clone();
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
                });
            }
        }
    }
}

#[derive(Debug)]
struct Geometry {
    bounds: Rect,
    positions: Option<Vec<f32>>,
    widths: Option<Vec<f32>>,
}

fn freeze_run(run: RunBuilder, galley: &Galley, previous: Option<Id>, next: Option<Id>) -> Run {
    let geometry = run_geometry(&run, galley);
    let mut node = accesskit::Node::new(accesskit::Role::TextRun);
    node.set_value(run.value);
    node.set_character_lengths(run.lengths);
    node.set_text_direction(accesskit::TextDirection::LeftToRight);
    node.set_bounds(accesskit_rect(geometry.bounds));
    if geometry.positions.is_some() && geometry.widths.is_some() {
        // Reserve property slots without retaining duplicate coordinate arrays.
        // Publication fills them in the current global coordinate scale.
        node.set_character_positions(Vec::<f32>::new());
        node.set_character_widths(Vec::<f32>::new());
    }
    if let Some(previous) = previous {
        node.set_previous_on_line(previous.accesskit_id());
    }
    if let Some(next) = next {
        node.set_next_on_line(next.accesskit_id());
    }
    publish_style(&mut node, run.style, run.range.start, galley);
    Run {
        id: run.id,
        range: run.range,
        offsets: run.offsets,
        line_break: run.line_break,
        node,
        geometry,
    }
}

fn run_geometry(run: &RunBuilder, galley: &Galley) -> Geometry {
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

fn publish_style(node: &mut accesskit::Node, style: InlineStyle, byte: usize, galley: &Galley) {
    node.set_font_weight(if style.bold { 700.0 } else { 400.0 });
    if style.italic {
        node.set_italic();
    }
    if style.code {
        node.set_font_family("monospace");
    }
    let format = galley
        .job
        .sections
        .iter()
        .find(|section| section.byte_range.contains(&egui::text::ByteIndex(byte)))
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
    if style.underline {
        node.set_underline(decoration);
    }
    if style.strikethrough {
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
        let layouts = layout_frame(&context, &editor, width);
        let result = build(&editor, &layouts, Id::new("editor"));
        (editor, result)
    }

    fn layout_frame(context: &egui::Context, editor: &Editor, width: f32) -> Vec<ParagraphLayout> {
        let mut layouts = Vec::new();
        let appearance = super::super::Appearance {
            font: egui::FontId::proportional(16.0),
            bold_family: None,
            color: egui::Color32::WHITE,
            strong_color: egui::Color32::WHITE,
            code_background: egui::Color32::BLACK,
            width,
        };
        context
            .run_ui(egui::RawInput::default(), |ui| {
                let mut y = 0.0;
                layouts = editor
                    .document()
                    .paragraphs()
                    .iter()
                    .map(|paragraph| {
                        let job = super::super::paragraph_job(paragraph, &appearance, width);
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
            })
            .drop_without_applying_deltas();
        layouts
    }

    #[test]
    fn local_edits_reuse_unchanged_paragraph_runs_and_geometry() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("first\naé👩‍💻 second\nlast");
        let widget_id = Id::new("editor");
        let layouts = layout_frame(&context, &editor, 500.0);
        let first = build(&editor, &layouts, widget_id).unwrap();
        let idle = build_cached(&editor, &layouts, widget_id, Some(&first)).unwrap();
        assert!(
            first
                .paragraphs
                .iter()
                .zip(&idle.paragraphs)
                .all(|(before, after)| Arc::ptr_eq(before, after))
        );

        editor
            .set_selection(crate::Selection::caret(Position::new(1, 0)))
            .unwrap();
        editor.insert_text("changed ").unwrap();
        let layouts = layout_frame(&context, &editor, 500.0);
        let edited = build_cached(&editor, &layouts, widget_id, Some(&first)).unwrap();
        assert!(Arc::ptr_eq(&first.paragraphs[0], &edited.paragraphs[0]));
        assert!(!Arc::ptr_eq(&first.paragraphs[1], &edited.paragraphs[1]));
        assert!(Arc::ptr_eq(&first.paragraphs[2], &edited.paragraphs[2]));
        assert!(edited.matches_document(&editor));
        assert!(!first.matches_document(&editor));
        for (index, paragraph) in editor.document().paragraphs().iter().enumerate() {
            for grapheme in 0..=paragraph.grapheme_count() {
                let position =
                    Position::new(index, paragraph.byte_from_grapheme(grapheme).unwrap());
                assert_eq!(
                    edited.decode(edited.position(position).unwrap()),
                    Some(position)
                );
            }
        }
        let other_id = build_cached(&editor, &layouts, Id::new("other"), Some(&edited)).unwrap();
        assert!(
            edited
                .paragraphs
                .iter()
                .zip(&other_id.paragraphs)
                .all(|(before, after)| !Arc::ptr_eq(before, after) && before.id != after.id)
        );
    }

    #[test]
    fn cached_paragraphs_refresh_when_the_final_separator_changes() {
        let context = egui::Context::default();
        let editor = Editor::from_text("first\nlast");
        let widget_id = Id::new("editor");
        let layouts = layout_frame(&context, &editor, 500.0);
        let first = build(&editor, &layouts, widget_id).unwrap();
        let mut paragraphs = editor.document().paragraphs().to_vec();
        paragraphs.push(Arc::clone(
            &crate::Document::from_text("new").paragraphs()[0],
        ));
        let appended = Editor::new(crate::Document::from_fragment(
            &crate::Fragment::from_paragraphs(paragraphs),
        ));
        let layouts = layout_frame(&context, &appended, 500.0);
        let extended = build_cached(&appended, &layouts, widget_id, Some(&first)).unwrap();
        assert!(Arc::ptr_eq(&first.paragraphs[0], &extended.paragraphs[0]));
        assert!(!Arc::ptr_eq(&first.paragraphs[1], &extended.paragraphs[1]));
        let last_run = extended.paragraphs[1].runs.last().unwrap();
        assert_eq!(last_run.node.value(), Some("last\n"));
        let after_break = accesskit::TextPosition {
            node: last_run.id.accesskit_id(),
            character_index: last_run.node.character_lengths().len(),
        };
        assert_eq!(extended.decode(after_break), Some(Position::new(2, 0)));

        let layouts = layout_frame(&context, &editor, 500.0);
        let shortened = build_cached(&editor, &layouts, widget_id, Some(&extended)).unwrap();
        assert!(Arc::ptr_eq(
            &extended.paragraphs[0],
            &shortened.paragraphs[0]
        ));
        assert!(!Arc::ptr_eq(
            &extended.paragraphs[1],
            &shortened.paragraphs[1]
        ));
        assert_eq!(
            shortened.paragraphs[1].runs.last().unwrap().node.value(),
            Some("last")
        );
        assert_eq!(shortened.decode(after_break), None);
    }

    #[test]
    fn cached_paragraphs_refresh_for_width_and_font_changes() {
        let context = egui::Context::default();
        let editor = Editor::from_text("first second third fourth\nlast words here");
        let widget_id = Id::new("editor");
        let layouts = layout_frame(&context, &editor, 500.0);
        let wide = build(&editor, &layouts, widget_id).unwrap();
        let layouts = layout_frame(&context, &editor, 65.0);
        let narrow = build_cached(&editor, &layouts, widget_id, Some(&wide)).unwrap();
        assert!(
            wide.paragraphs
                .iter()
                .zip(&narrow.paragraphs)
                .all(|(before, after)| !Arc::ptr_eq(before, after))
        );
        assert!(narrow.paragraphs[0].runs.len() > wide.paragraphs[0].runs.len());

        let mut definitions = egui::FontDefinitions::default();
        definitions.families.insert(
            egui::FontFamily::Proportional,
            definitions.families[&egui::FontFamily::Monospace].clone(),
        );
        context.set_fonts(definitions);
        let layouts = layout_frame(&context, &editor, 65.0);
        let fonts = build_cached(&editor, &layouts, widget_id, Some(&narrow)).unwrap();
        assert!(
            narrow
                .paragraphs
                .iter()
                .zip(&fonts.paragraphs)
                .all(|(before, after)| !Arc::ptr_eq(before, after))
        );
    }

    #[test]
    fn failed_rebuilds_leave_the_previous_snapshot_intact() {
        let context = egui::Context::default();
        let editor = Editor::from_text("first\nvalid\nlast");
        let widget_id = Id::new("editor");
        let layouts = layout_frame(&context, &editor, 500.0);
        let snapshot = build(&editor, &layouts, widget_id).unwrap();
        let position = snapshot.position(Position::new(1, 2)).unwrap();
        let references = Arc::strong_count(&snapshot.paragraphs[0]);
        let mut changed = Editor::new(editor.document().clone());
        changed
            .set_selection(crate::Selection::caret(Position::new(1, 0)))
            .unwrap();
        changed
            .insert_text(&format!("x{}", "\u{0301}".repeat(200)))
            .unwrap();
        let mut layouts = layout_frame(&context, &changed, 500.0);
        assert!(matches!(
            build_cached(&changed, &layouts, widget_id, Some(&snapshot)),
            Err(AccessibilityError::CharacterTooLong { bytes: 401, .. })
        ));
        assert_eq!(Arc::strong_count(&snapshot.paragraphs[0]), references);
        assert!(snapshot.matches_document(&editor));
        assert_eq!(snapshot.decode(position), Some(Position::new(1, 2)));

        layouts[1].galley = Arc::clone(&layouts[0].galley);
        assert!(matches!(
            build_cached(&changed, &layouts, widget_id, Some(&snapshot)),
            Err(AccessibilityError::InvalidLayout)
        ));
        assert!(matches!(
            build_cached(&changed, &layouts[..2], widget_id, Some(&snapshot)),
            Err(AccessibilityError::InvalidLayout)
        ));
        assert_eq!(Arc::strong_count(&snapshot.paragraphs[0]), references);
        assert_eq!(snapshot.decode(position), Some(Position::new(1, 2)));
    }

    #[test]
    fn emoji_and_combining_marks_expose_only_real_caret_stops() {
        let (_, snapshot) = make_snapshot("a👩🏽‍💻e\u{301}", 500.0);
        let runs = &snapshot.paragraphs[0].runs;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].node.character_lengths(), [1, 15, 3]);
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
            assert!(last.node.value().unwrap().ends_with('\n'));
            let end = Position::new(index, paragraph.source.text().len());
            let before_break = snapshot.position(end).unwrap();
            assert_eq!(
                before_break.character_index,
                last.node.character_lengths().len() - 1
            );
            assert_eq!(snapshot.decode(before_break), Some(end));
            assert_eq!(
                snapshot.decode(accesskit::TextPosition {
                    node: last.id.accesskit_id(),
                    character_index: last.node.character_lengths().len()
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
        assert_eq!(runs[0].node.character_lengths().len(), 255);
        assert_eq!(runs[1].node.character_lengths().len(), 45);
        assert!(
            runs.iter()
                .flat_map(|run| run.node.character_lengths())
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
            assert_eq!(
                run.geometry.positions.as_ref().unwrap().len(),
                run.node.character_lengths().len()
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

    #[test]
    fn snapshots_validate_cloned_content_and_decode_wrapped_paragraphs() {
        let (editor, snapshot) = make_snapshot("first\naé👩‍💻 second third\n", 65.0);
        let cloned = Editor::new(editor.document().clone());
        assert!(snapshot.matches_document(&cloned));
        assert!(!snapshot.matches_document(&Editor::from_text("different\ntext\n")));

        for (index, paragraph) in editor.document().paragraphs().iter().enumerate() {
            for grapheme in 0..=paragraph.grapheme_count() {
                let position =
                    Position::new(index, paragraph.byte_from_grapheme(grapheme).unwrap());
                assert_eq!(
                    snapshot.decode(snapshot.position(position).unwrap()),
                    Some(position)
                );
            }
        }
        assert_eq!(
            snapshot.decode(accesskit::TextPosition {
                node: accesskit::NodeId(u64::MAX),
                character_index: 0,
            }),
            None
        );
        let run = snapshot.position(Position::new(1, 0)).unwrap();
        assert_eq!(
            snapshot.decode(accesskit::TextPosition {
                character_index: usize::MAX,
                ..run
            }),
            None
        );
    }

    #[test]
    fn cached_geometry_follows_the_widget_origin_when_published() {
        let (editor, snapshot) = make_snapshot("a👩🏽‍💻e\u{301}", 500.0);
        let context = egui::Context::default();
        context.enable_accesskit();
        let paragraph = &snapshot.paragraphs[0];
        let run_id = paragraph.runs[0].id.accesskit_id();
        let layouts = vec![ParagraphLayout {
            galley: Arc::clone(&paragraph.galley),
            rect: Rect::from_min_size(Pos2::ZERO, paragraph.galley.size()),
            marker: None,
            marker_position: Pos2::ZERO,
        }];
        let publish_bounds = |origin| {
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                snapshot.publish(ui, Id::new("editor"), &editor, &layouts, origin, false);
            });
            output.textures_delta.clear();
            output
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .into_iter()
                .find(|(id, _)| *id == run_id)
                .unwrap()
                .1
                .bounds()
                .unwrap()
        };
        let first = publish_bounds(Pos2::new(10.0, 20.0));
        let moved = publish_bounds(Pos2::new(110.0, 220.0));
        assert_eq!(moved.x0 - first.x0, 100.0);
        assert_eq!(moved.x1 - first.x1, 100.0);
        assert_eq!(moved.y0 - first.y0, 200.0);
        assert_eq!(moved.y1 - first.y1, 200.0);
    }

    fn registration_frame(
        editor: &Editor,
        snapshot: &Snapshot,
        layouts: &[ParagraphLayout],
        placement: (Pos2, egui::emath::TSTransform),
        enabled: bool,
        read_only: bool,
        legacy: bool,
    ) -> (accesskit::TreeUpdate, Vec<egui::Response>) {
        let (origin, transform) = placement;
        let context = egui::Context::default();
        context.enable_accesskit();
        let widget_id = Id::new("editor");
        context.memory_mut(|memory| memory.request_focus(widget_id));
        let mut responses = Vec::new();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            if !enabled {
                ui.disable();
            }
            context.set_transform_layer(ui.layer_id(), transform);
            ui.interact(
                Rect::from_min_size(origin, egui::vec2(500.0, 500.0)),
                widget_id,
                egui::Sense::click_and_drag(),
            );
            if legacy {
                // Reference the previous empty-Ui registration and rebuild run
                // nodes directly from source, without using cached templates.
                for (paragraph, layout) in snapshot.paragraphs.iter().zip(layouts) {
                    let paragraph_rect = layout.rect.translate(origin.to_vec2());
                    let _paragraph_ui = Ui::new(
                        context.clone(),
                        paragraph.id,
                        egui::UiBuilder::new()
                            .accessibility_parent(widget_id)
                            .layer_id(ui.layer_id())
                            .max_rect(paragraph_rect),
                    );
                    let from_galley = transform
                        * egui::emath::TSTransform::from_translation(paragraph_rect.min.to_vec2());
                    context.accesskit_node_builder(paragraph.id, |node| {
                        node.set_bounds(accesskit_rect(transform * paragraph_rect));
                    });
                    for (run_index, run) in paragraph.runs.iter().enumerate() {
                        let geometry = &run.geometry;
                        let _run_ui = Ui::new(
                            context.clone(),
                            run.id,
                            egui::UiBuilder::new()
                                .accessibility_parent(paragraph.id)
                                .layer_id(ui.layer_id())
                                .max_rect(geometry.bounds.translate(paragraph_rect.min.to_vec2())),
                        );
                        context.accesskit_node_builder(run.id, |node| {
                            node.set_role(accesskit::Role::TextRun);
                            let mut value = paragraph.source.text()[run.range.clone()].to_owned();
                            if run.line_break {
                                value.push('\n');
                            }
                            let lengths: Vec<_> = value
                                .graphemes(true)
                                .map(|grapheme| u8::try_from(grapheme.len()).unwrap())
                                .collect();
                            node.set_value(value);
                            node.set_character_lengths(lengths);
                            node.set_text_direction(accesskit::TextDirection::LeftToRight);
                            node.set_bounds(accesskit_rect(from_galley * geometry.bounds));
                            if let (Some(positions), Some(widths)) =
                                (&geometry.positions, &geometry.widths)
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
                            let row = run_row(paragraph, run);
                            if let Some(previous) = run_index
                                .checked_sub(1)
                                .and_then(|index| paragraph.runs.get(index))
                                && run_row(paragraph, previous) == row
                            {
                                node.set_previous_on_line(previous.id.accesskit_id());
                            }
                            if let Some(next) = paragraph.runs.get(run_index + 1)
                                && run_row(paragraph, next) == row
                            {
                                node.set_next_on_line(next.id.accesskit_id());
                            }
                            let style = paragraph
                                .source
                                .spans()
                                .iter()
                                .find(|span| span.range.contains(&run.range.start))
                                .map_or(InlineStyle::default(), |span| span.style);
                            publish_style(node, style, run.range.start, &paragraph.galley);
                        });
                    }
                }
            } else {
                snapshot.publish(ui, widget_id, editor, layouts, origin, read_only);
            }
            responses = snapshot
                .paragraphs
                .iter()
                .flat_map(|paragraph| &paragraph.runs)
                .map(|run| context.read_response(run.id).unwrap())
                .collect();
        });
        let update = output.platform_output.accesskit_update.take().unwrap();
        output.drop_without_applying_deltas();
        (update, responses)
    }

    fn run_row(paragraph: &ParagraphSnapshot, run: &Run) -> usize {
        let scalar_start = paragraph.source.scalar_index(run.range.start).unwrap();
        let mut end = 0;
        paragraph
            .galley
            .rows
            .iter()
            .position(|row| {
                end += row.char_count_including_newline().0;
                end > scalar_start
            })
            .unwrap_or(paragraph.galley.rows.len() - 1)
    }

    #[test]
    fn run_registration_preserves_tree_geometry_and_hover_behavior() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("first café e\u{301} 👩‍💻 second third fourth\n\nlast 🇺🇸");
        editor
            .set_selection(crate::Selection::new(
                Position::new(0, 0),
                Position::new(0, 2),
            ))
            .unwrap();
        editor
            .apply_style(crate::StylePatch {
                bold: Some(true),
                italic: Some(true),
                underline: Some(true),
                strikethrough: Some(true),
                code: Some(true),
                foreground: Some(Some(crate::Color([42, 100, 200, 255]))),
            })
            .unwrap();
        let layouts = layout_frame(&context, &editor, 65.0);
        let snapshot = build(&editor, &layouts, Id::new("editor")).unwrap();
        let layouts: Vec<_> = snapshot
            .paragraphs
            .iter()
            .enumerate()
            .map(|(index, paragraph)| ParagraphLayout {
                galley: Arc::clone(&paragraph.galley),
                rect: Rect::from_min_size(
                    Pos2::new(18.0, index as f32 * 150.0),
                    paragraph.galley.size(),
                ),
                marker: None,
                marker_position: Pos2::ZERO,
            })
            .collect();
        let origin = Pos2::new(30.0, 40.0);
        let transform = egui::emath::TSTransform {
            translation: egui::vec2(70.0, 90.0),
            scaling: 2.0,
        };
        for (enabled, read_only) in [(true, false), (true, true), (false, false), (false, true)] {
            let (reference, old_responses) = registration_frame(
                &editor,
                &snapshot,
                &layouts,
                (origin, transform),
                enabled,
                read_only,
                true,
            );
            let (published, responses) = registration_frame(
                &editor,
                &snapshot,
                &layouts,
                (origin, transform),
                enabled,
                read_only,
                false,
            );
            assert_eq!(published.focus, reference.focus);
            let old_nodes: HashMap<_, _> = reference.nodes.into_iter().collect();
            let nodes: HashMap<_, _> = published.nodes.into_iter().collect();
            let widget = &nodes[&Id::new("editor").accesskit_id()];
            let children: Vec<_> = snapshot
                .paragraphs
                .iter()
                .map(|paragraph| paragraph.id.accesskit_id())
                .collect();
            assert_eq!(widget.children(), children);
            assert_eq!(
                widget.children(),
                old_nodes[&Id::new("editor").accesskit_id()].children()
            );
            assert_eq!(
                widget.supports_action(accesskit::Action::SetTextSelection),
                enabled
            );
            assert_eq!(widget.is_read_only(), read_only);
            assert_eq!(
                widget.supports_action(accesskit::Action::SetValue),
                enabled && !read_only
            );
            assert_eq!(
                widget.supports_action(accesskit::Action::ReplaceSelectedText),
                enabled && !read_only
            );
            for (paragraph, layout) in snapshot.paragraphs.iter().zip(&layouts) {
                let id = paragraph.id.accesskit_id();
                let children: Vec<_> = paragraph
                    .runs
                    .iter()
                    .map(|run| run.id.accesskit_id())
                    .collect();
                assert_eq!(nodes[&id].children(), children);
                assert_eq!(nodes[&id].children(), old_nodes[&id].children());
                assert_eq!(nodes[&id].bounds(), old_nodes[&id].bounds());
                let from_galley = transform
                    * egui::emath::TSTransform::from_translation(
                        layout.rect.min.to_vec2() + origin.to_vec2(),
                    );
                for run in &paragraph.runs {
                    let geometry = &run.geometry;
                    let id = run.id.accesskit_id();
                    let node = &nodes[&id];
                    assert_eq!(node.role(), accesskit::Role::TextRun);
                    assert_eq!(node, &old_nodes[&id]);
                    assert_eq!(node.bounds(), old_nodes[&id].bounds());
                    assert_eq!(
                        node.bounds(),
                        Some(accesskit_rect(from_galley * geometry.bounds))
                    );
                    assert!(!node.supports_action(accesskit::Action::Focus));
                    if let (Some(positions), Some(widths)) = (&geometry.positions, &geometry.widths)
                    {
                        assert_eq!(
                            node.character_positions().unwrap(),
                            positions
                                .iter()
                                .map(|position| position * 2.0)
                                .collect::<Vec<_>>()
                        );
                        assert_eq!(
                            node.character_widths().unwrap(),
                            widths.iter().map(|width| width * 2.0).collect::<Vec<_>>()
                        );
                    }
                }
            }
            for (response, old_response) in responses.iter().zip(&old_responses) {
                assert_eq!(response.id, old_response.id);
                assert_eq!(response.rect, old_response.rect);
                assert_eq!(response.interact_rect, old_response.interact_rect);
                assert_eq!(response.layer_id, old_response.layer_id);
                assert_eq!(response.sense, egui::Sense::hover());
                assert_eq!(response.sense, old_response.sense);
                assert_eq!(response.enabled(), old_response.enabled());
                assert!(!response.has_focus());
                assert!(!old_response.has_focus());
            }
        }
    }
}
