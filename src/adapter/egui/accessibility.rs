use super::ParagraphLayouts;
use super::layout::{LayoutIdentity, is_layout_text};
use crate::accessibility::node_semantics::{apply_text_style, set_paragraph_semantics};
use crate::document::{LINE_SEPARATOR, SpanCursor, same_numbering_effect};
use crate::{Editor, InlineStyle, Link, Paragraph, ParagraphKind, Position, Span};
use egui::epaint::text::PlacedRow;
use egui::text::CharIndex;
use egui::{Galley, Id, Pos2, Rect, Ui, accesskit};
use std::collections::HashMap;
use std::fmt;
use std::ops::Range;
use std::sync::{Arc, OnceLock};
use unicode_segmentation::UnicodeSegmentation;

const MAX_RUN_CHARACTERS: usize = 255;
const MAX_CHARACTER_BYTES: usize = 255;

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessibilityError {
    InvalidLayout,
    StaleDocument,
    InvalidTextPosition,
    InvalidActionData,
}

impl fmt::Display for AccessibilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
    offsets: Vec<usize>,
    row: usize,
    line_break: bool,
    soft_break: bool,
    node: accesskit::Node,
    geometry: Geometry,
}

#[derive(Debug)]
struct RunBuilder {
    id: Id,
    range: Range<usize>,
    value: String,
    lengths: Vec<u8>,
    word_starts: Vec<u8>,
    offsets: Vec<usize>,
    scalar_offsets: Vec<usize>,
    layout_start: usize,
    style: InlineStyle,
    link: Option<Link>,
    row: usize,
    line_break: bool,
    soft_break: bool,
}

#[derive(Debug)]
struct LinkNode {
    id: Id,
    link: Link,
    runs: Range<usize>,
    bounds: Rect,
    label: String,
}

#[derive(Debug)]
struct ParagraphSnapshot {
    source: Arc<Paragraph>,
    galley: Arc<Galley>,
    serial: u64,
    id: Id,
    line_break: bool,
    runs: Arc<[Run]>,
    links: Arc<[LinkNode]>,
}

type RunsKey = (*const str, *const [Span], *const Galley, bool);

fn runs_key(source: &Paragraph, galley: &Arc<Galley>, line_break: bool) -> RunsKey {
    (
        source.text(),
        source.spans(),
        Arc::as_ptr(galley),
        line_break,
    )
}

#[derive(Debug)]
pub(super) struct Snapshot {
    paragraphs: Vec<Arc<ParagraphSnapshot>>,
    document_identity: Arc<()>,
    layout_identity: LayoutIdentity,
    environment_identity: Arc<()>,
    widget_id: Id,
    run_locations: OnceLock<HashMap<accesskit::NodeId, (usize, usize)>>,
    next_serial: u64,
}

#[derive(Debug)]
struct Splice {
    replaced: Range<usize>,
    paragraphs: Vec<Arc<ParagraphSnapshot>>,
    next_serial: u64,
}

fn paragraph_id(widget_id: Id, serial: u64) -> Id {
    widget_id.with(("paragraph", serial))
}

#[cfg(test)]
fn build(
    editor: &Editor,
    layouts: &ParagraphLayouts,
    widget_id: Id,
) -> Result<Snapshot, AccessibilityError> {
    build_cached(editor, layouts, widget_id, None)
}

pub(super) fn build_cached(
    editor: &Editor,
    layouts: &ParagraphLayouts,
    widget_id: Id,
    previous: Option<&Snapshot>,
) -> Result<Snapshot, AccessibilityError> {
    let previous = previous.filter(|snapshot| snapshot.widget_id == widget_id);
    let old = previous.map_or(&[][..], |snapshot| snapshot.paragraphs.as_slice());
    let splice = changes_since(editor, layouts, widget_id, previous)?;
    let mut paragraphs = Vec::with_capacity(layouts.len());
    paragraphs.extend(old[..splice.replaced.start].iter().cloned());
    paragraphs.extend(splice.paragraphs);
    paragraphs.extend(old[splice.replaced.end..].iter().cloned());
    Ok(Snapshot {
        paragraphs,
        document_identity: editor.document().content_identity(),
        layout_identity: layouts.identity(),
        environment_identity: layouts.environment_identity(),
        widget_id,
        run_locations: OnceLock::new(),
        next_serial: splice.next_serial,
    })
}

fn changes_since(
    editor: &Editor,
    layouts: &ParagraphLayouts,
    widget_id: Id,
    previous: Option<&Snapshot>,
) -> Result<Splice, AccessibilityError> {
    let sources = editor.document().paragraphs();
    if layouts.len() != sources.len() {
        return Err(AccessibilityError::InvalidLayout);
    }
    let old = previous.map_or(&[][..], |snapshot| snapshot.paragraphs.as_slice());
    let next_serial = previous.map_or(0, |snapshot| snapshot.next_serial);
    if let Some(index) = previous.and_then(|snapshot| snapshot.edited_paragraph(editor, layouts)) {
        let layout = layouts
            .views_range(index..index + 1)
            .next()
            .ok_or(AccessibilityError::InvalidLayout)?;
        let edited = index..index + 1;
        return rebuild_window(
            sources,
            &[layout.galley],
            widget_id,
            old,
            edited.clone(),
            edited,
            next_serial,
        );
    }
    let galleys: Vec<_> = layouts
        .views_range(0..layouts.len())
        .map(|layout| layout.galley)
        .collect();
    let unchanged = |old_index: usize, index: usize| {
        let paragraph = &old[old_index];
        Arc::ptr_eq(&paragraph.source, &sources[index])
            && Arc::ptr_eq(&paragraph.galley, galleys[index])
            && paragraph.line_break == (index + 1 < sources.len())
    };
    let shared = old.len().min(sources.len());
    let prefix = (0..shared)
        .take_while(|&index| unchanged(index, index))
        .count();
    let suffix = (0..shared - prefix)
        .take_while(|&offset| unchanged(old.len() - 1 - offset, sources.len() - 1 - offset))
        .count();
    let window = prefix..sources.len() - suffix;
    rebuild_window(
        sources,
        &galleys[window.clone()],
        widget_id,
        old,
        prefix..old.len() - suffix,
        window,
        next_serial,
    )
}

fn rebuild_window(
    sources: &[Arc<Paragraph>],
    galleys: &[&Arc<Galley>],
    widget_id: Id,
    old: &[Arc<ParagraphSnapshot>],
    replaced: Range<usize>,
    window: Range<usize>,
    mut next_serial: u64,
) -> Result<Splice, AccessibilityError> {
    let prefix = window.start;
    let mut cached: HashMap<RunsKey, Vec<usize>> = HashMap::new();
    for index in replaced.clone().rev() {
        let paragraph = &old[index];
        let key = runs_key(&paragraph.source, &paragraph.galley, paragraph.line_break);
        cached.entry(key).or_default().push(index);
    }
    let mut claimed = vec![false; replaced.len()];
    let reused: Vec<_> = window
        .clone()
        .map(|index| {
            let key = runs_key(
                &sources[index],
                galleys[index - prefix],
                index + 1 < sources.len(),
            );
            let reused = cached.get_mut(&key)?.pop()?;
            claimed[reused - prefix] = true;
            Some(reused)
        })
        .collect();
    let mut paragraphs = Vec::with_capacity(window.len());
    for (index, reused) in window.zip(reused) {
        if let Some(reused) = reused {
            let cached = &old[reused];
            paragraphs.push(if Arc::ptr_eq(&cached.source, &sources[index]) {
                Arc::clone(cached)
            } else {
                Arc::new(ParagraphSnapshot {
                    source: Arc::clone(&sources[index]),
                    galley: Arc::clone(&cached.galley),
                    serial: cached.serial,
                    id: cached.id,
                    line_break: cached.line_break,
                    runs: Arc::clone(&cached.runs),
                    links: Arc::clone(&cached.links),
                })
            });
            continue;
        }
        let serial = if replaced.contains(&index) && !claimed[index - prefix] {
            old[index].serial
        } else {
            next_serial += 1;
            next_serial - 1
        };
        paragraphs.push(build_paragraph_snapshot(
            &sources[index],
            galleys[index - prefix],
            serial,
            widget_id,
            index + 1 < sources.len(),
        )?);
    }
    Ok(Splice {
        replaced,
        paragraphs,
        next_serial,
    })
}

fn build_paragraph_snapshot(
    source: &Arc<Paragraph>,
    galley: &Arc<Galley>,
    serial: u64,
    widget_id: Id,
    line_break: bool,
) -> Result<Arc<ParagraphSnapshot>, AccessibilityError> {
    let id = paragraph_id(widget_id, serial);
    let text = source.text();
    if !is_layout_text(&galley.job.text, text) || galley.rows.is_empty() {
        return Err(AccessibilityError::InvalidLayout);
    }
    let rows = Rows::new(galley);
    let limit = MAX_RUN_CHARACTERS - usize::from(line_break);
    let mut words = source.word_starts().into_iter().peekable();
    let mut runs: Vec<RunBuilder> = Vec::new();
    let mut spans = SpanCursor::new(source.spans());
    let mut scalar_start = 0;
    let mut shortened = 0;
    for (byte, grapheme) in text.grapheme_indices(true) {
        let end = byte + grapheme.len();
        let soft_break = grapheme.starts_with(LINE_SEPARATOR);
        let mut starts_word = false;
        while words.next_if(|word| *word < end).is_some() {
            starts_word = true;
        }
        let (style, link) = spans.at(byte);
        let mut piece_start = byte;
        while piece_start < end {
            let piece_end =
                piece_start + text[piece_start..end].floor_char_boundary(MAX_CHARACTER_BYTES);
            let piece = &text[piece_start..piece_end];
            let scalar_end = scalar_start + piece.chars().count();
            let row = rows.containing(scalar_start);
            let new_run = runs.last().is_none_or(|run| {
                run.row != row
                    || run.style != style
                    || run.link.as_ref() != link
                    || run.lengths.len() == limit
            });
            if new_run {
                runs.push(RunBuilder {
                    id: id.with(("grapheme_run", runs.len())),
                    range: piece_start..piece_start,
                    value: String::new(),
                    lengths: Vec::new(),
                    word_starts: Vec::new(),
                    offsets: vec![byte],
                    scalar_offsets: vec![scalar_start],
                    layout_start: piece_start - shortened,
                    style,
                    link: link.cloned(),
                    row,
                    line_break: false,
                    soft_break: false,
                });
            }
            let run = runs.last_mut().expect("a run was created");
            if starts_word && piece_start == byte {
                run.word_starts.push(
                    u8::try_from(run.lengths.len()).expect("runs hold at most 255 characters"),
                );
            }
            let piece = if soft_break { "\n" } else { piece };
            run.range.end = piece_end;
            run.soft_break = soft_break;
            run.value.push_str(piece);
            run.lengths
                .push(u8::try_from(piece.len()).expect("characters hold at most 255 bytes"));
            run.offsets.push(if piece_end == end { end } else { byte });
            run.scalar_offsets.push(scalar_end);
            scalar_start = scalar_end;
            piece_start = piece_end;
        }
        if soft_break {
            shortened += grapheme.len() - 1;
        }
    }
    if runs.last().is_none_or(|run| run.soft_break) {
        runs.push(RunBuilder {
            id: id.with(("grapheme_run", runs.len())),
            range: text.len()..text.len(),
            value: String::new(),
            lengths: Vec::new(),
            word_starts: Vec::new(),
            offsets: vec![text.len()],
            scalar_offsets: vec![scalar_start],
            layout_start: galley.job.text.len(),
            style: source
                .spans()
                .last()
                .map_or(InlineStyle::default(), |span| span.style),
            link: None,
            row: rows.containing(scalar_start),
            line_break: false,
            soft_break: false,
        });
    }
    if line_break {
        let last = runs.last_mut().expect("empty paragraphs also have a run");
        last.value.push('\n');
        last.lengths.push(1);
        last.line_break = true;
    }
    let mut builders = runs.into_iter().peekable();
    let mut runs = Vec::with_capacity(builders.len());
    let mut links: Vec<LinkNode> = Vec::new();
    let mut previous = None;
    while let Some(builder) = builders.next() {
        let index = runs.len();
        if let Some(link) = &builder.link {
            match links.last_mut() {
                Some(node) if node.runs.end == index && node.link == *link => {
                    node.runs.end = index + 1;
                    node.label.push_str(&text[builder.range.clone()]);
                }
                _ => links.push(LinkNode {
                    id: id.with(("link", links.len())),
                    link: link.clone(),
                    runs: index..index + 1,
                    bounds: Rect::NOTHING,
                    label: text[builder.range.clone()].to_owned(),
                }),
            }
        }
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
            galley,
            &rows,
            previous_on_line,
            next_on_line,
        ));
    }
    for link in &mut links {
        link.bounds = runs[link.runs.clone()]
            .iter()
            .fold(Rect::NOTHING, |bounds, run: &Run| {
                bounds.union(run.geometry.bounds)
            });
        link.label = link.label.replace(LINE_SEPARATOR, " ");
    }
    Ok(Arc::new(ParagraphSnapshot {
        source: Arc::clone(source),
        galley: Arc::clone(galley),
        serial,
        id,
        line_break,
        runs: runs.into(),
        links: links.into(),
    }))
}

pub(super) fn update_cached(
    editor: &Editor,
    layouts: &ParagraphLayouts,
    widget_id: Id,
    snapshot: &mut Option<Arc<Snapshot>>,
) -> Result<(), AccessibilityError> {
    if layouts.len() != editor.document().paragraphs().len() {
        return Err(AccessibilityError::InvalidLayout);
    }
    if let Some(previous) = snapshot
        .as_mut()
        .filter(|previous| previous.widget_id == widget_id)
    {
        if previous.is_current(editor, layouts) {
            return Ok(());
        }
        if let Some(previous) = Arc::get_mut(previous) {
            let splice = changes_since(editor, layouts, widget_id, Some(previous))?;
            previous.splice(splice);
            previous.document_identity = editor.document().content_identity();
            previous.layout_identity = layouts.identity();
            previous.environment_identity = layouts.environment_identity();
            return Ok(());
        }
    }
    let next = build_cached(editor, layouts, widget_id, snapshot.as_deref())?;
    *snapshot = Some(Arc::new(next));
    Ok(())
}

impl Run {
    fn text_position(&self, byte: usize) -> Option<accesskit::TextPosition> {
        let character_index = self.offsets.partition_point(|offset| *offset < byte);
        (self.offsets.get(character_index) == Some(&byte)).then(|| accesskit::TextPosition {
            node: self.id.accesskit_id(),
            character_index,
        })
    }
}

fn list_positions(kinds: impl Iterator<Item = ParagraphKind>) -> Vec<Option<(usize, usize)>> {
    let mut open: Vec<(bool, u8, usize)> = Vec::new();
    let mut sizes: Vec<usize> = Vec::new();
    let members: Vec<_> = kinds
        .map(|kind| {
            let (ordered, indent, restart) = match kind {
                ParagraphKind::Bullet { indent } => (false, indent, false),
                ParagraphKind::Ordered { indent, start } => (true, indent, start.is_some()),
                ParagraphKind::Heading { .. } => {
                    open.clear();
                    return None;
                }
                _ => return None,
            };
            open.retain(|&(_, level, _)| level <= indent);
            let existing = open
                .iter()
                .position(|&(kind, level, _)| level == indent && kind == ordered);
            let set = match existing {
                Some(index) if !restart => open[index].2,
                _ => {
                    if let Some(index) = existing {
                        open.remove(index);
                    }
                    sizes.push(0);
                    open.push((ordered, indent, sizes.len() - 1));
                    sizes.len() - 1
                }
            };
            sizes[set] += 1;
            Some((set, sizes[set]))
        })
        .collect();
    members
        .into_iter()
        .map(|member| member.map(|(set, position)| (position, sizes[set])))
        .collect()
}

impl Snapshot {
    fn run_locations(&self) -> &HashMap<accesskit::NodeId, (usize, usize)> {
        self.run_locations.get_or_init(|| {
            self.paragraphs
                .iter()
                .enumerate()
                .flat_map(|(paragraph_index, paragraph)| {
                    paragraph
                        .runs
                        .iter()
                        .enumerate()
                        .map(move |(run_index, run)| {
                            (run.id.accesskit_id(), (paragraph_index, run_index))
                        })
                })
                .collect()
        })
    }

    fn edited_paragraph(&self, editor: &Editor, layouts: &ParagraphLayouts) -> Option<usize> {
        let change = editor.document().change_since(&self.document_identity)?;
        if change.range.len() != 1 || change.new_len != 1 {
            return None;
        }
        let index = change.range.start;
        let before = self.paragraphs.get(index)?.source.kind();
        let after = editor.document().paragraphs().get(index)?.kind();
        (self.paragraphs.len() == layouts.len()
            && Arc::ptr_eq(&self.environment_identity, &layouts.environment_identity())
            && same_numbering_effect(before, after))
        .then_some(index)
    }

    fn splice(&mut self, splice: Splice) {
        let replaced = splice.replaced;
        if replaced.len() != splice.paragraphs.len() {
            self.run_locations.take();
        } else if let Some(locations) = self.run_locations.get_mut() {
            for paragraph in &self.paragraphs[replaced.clone()] {
                for run in paragraph.runs.iter() {
                    locations.remove(&run.id.accesskit_id());
                }
            }
            for (index, paragraph) in replaced.clone().zip(&splice.paragraphs) {
                for (run_index, run) in paragraph.runs.iter().enumerate() {
                    locations.insert(run.id.accesskit_id(), (index, run_index));
                }
            }
        }
        self.paragraphs.splice(replaced, splice.paragraphs);
        self.next_serial = splice.next_serial;
    }

    pub(super) fn matches_document(&self, editor: &Editor) -> bool {
        Arc::ptr_eq(
            &self.document_identity,
            &editor.document().content_identity(),
        )
    }

    pub(super) fn is_current(&self, editor: &Editor, layouts: &ParagraphLayouts) -> bool {
        self.matches_document(editor) && self.layout_identity.matches(layouts)
    }

    pub(super) fn position(&self, position: Position) -> Option<accesskit::TextPosition> {
        let paragraph = self.paragraphs.get(position.paragraph)?;
        let index = paragraph
            .runs
            .partition_point(|run| run.range.start <= position.byte)
            .checked_sub(1)?;
        paragraph.runs[index].text_position(position.byte)
    }

    fn focus_position(
        &self,
        position: Position,
        prefer_next_row: bool,
    ) -> Option<accesskit::TextPosition> {
        if !prefer_next_row && let Some(paragraph) = self.paragraphs.get(position.paragraph) {
            let next = paragraph
                .runs
                .partition_point(|run| run.range.start < position.byte);
            if let Some(before) = next
                .checked_sub(1)
                .and_then(|index| paragraph.runs.get(index))
                && let Some(after) = paragraph.runs.get(next)
                && before.range.end == position.byte
                && after.range.start == position.byte
                && before.row != after.row
                && !before.soft_break
            {
                return Some(accesskit::TextPosition {
                    node: before.id.accesskit_id(),
                    character_index: before.offsets.len() - 1,
                });
            }
        }
        self.position(position)
    }

    pub(super) fn link(&self, node: accesskit::NodeId) -> Option<&Link> {
        self.paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.links.iter())
            .find(|link| link.id.accesskit_id() == node)
            .map(|link| &link.link)
    }

    pub(super) fn decode(&self, position: accesskit::TextPosition) -> Option<Position> {
        let &(index, run_index) = self.run_locations().get(&position.node)?;
        let run = &self.paragraphs[index].runs[run_index];
        if let Some(byte) = run.offsets.get(position.character_index) {
            return Some(Position::new(index, *byte));
        }
        if run.line_break && position.character_index == run.offsets.len() {
            return Some(Position::new(index + 1, 0));
        }
        None
    }

    pub(super) fn decode_focus(
        &self,
        position: accesskit::TextPosition,
    ) -> Option<(Position, bool)> {
        let decoded = self.decode(position)?;
        // Equivalent run boundaries use the usual downstream caret, except
        // when the request explicitly names the end of a soft-wrapped row.
        let prefer_next_row = self.position(decoded) == Some(position)
            || self.focus_position(decoded, false) != Some(position);
        Some((decoded, prefer_next_row))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn publish(
        &self,
        ui: &Ui,
        widget_id: Id,
        editor: &Editor,
        layouts: &ParagraphLayouts,
        origin: Pos2,
        read_only: bool,
        focus_prefer_next_row: bool,
    ) {
        let selection = editor.selection();
        if ui
            .ctx()
            .accesskit_node_builder(widget_id, |node| {
                node.set_role(accesskit::Role::MultilineTextInput);
                if read_only {
                    node.set_read_only();
                }
                let focus = self.focus_position(selection.focus, focus_prefer_next_row);
                let anchor = if selection.is_caret() {
                    focus
                } else {
                    self.position(selection.anchor)
                };
                if let (Some(anchor), Some(focus)) = (anchor, focus) {
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
        let list_items = list_positions(
            self.paragraphs
                .iter()
                .map(|paragraph| paragraph.source.kind()),
        );
        let is_quote =
            |paragraph: &Arc<ParagraphSnapshot>| paragraph.source.kind() == ParagraphKind::Quote;
        let mut quote = None;
        for (index, ((paragraph, layout), list_item)) in self
            .paragraphs
            .iter()
            .zip(layouts.views_range(0..layouts.len()))
            .zip(list_items)
            .enumerate()
        {
            let kind = paragraph.source.kind();
            if kind != ParagraphKind::Quote {
                quote = None;
            } else if quote.is_none() {
                let id = widget_id.with(("blockquote", paragraph.serial));
                let end = index
                    + self.paragraphs[index..]
                        .iter()
                        .take_while(|paragraph| is_quote(paragraph))
                        .count();
                let bounds = layouts
                    .views_range(index..end)
                    .map(|member| {
                        member
                            .quote_bar
                            .map_or(member.rect, |bar| bar.union(member.rect))
                    })
                    .fold(Rect::NOTHING, Rect::union)
                    .translate(origin.to_vec2());
                Ui::new(
                    ui.ctx().clone(),
                    id,
                    egui::UiBuilder::new()
                        .accessibility_parent(widget_id)
                        .layer_id(ui.layer_id())
                        .max_rect(bounds),
                );
                ui.ctx().accesskit_node_builder(id, |node| {
                    node.set_role(accesskit::Role::Blockquote);
                    node.set_bounds(accesskit_rect(to_global * bounds));
                });
                quote = Some(id);
            }
            let paragraph_rect = layout.rect.translate(origin.to_vec2());
            let paragraph_ui = Ui::new(
                ui.ctx().clone(),
                paragraph.id,
                egui::UiBuilder::new()
                    .accessibility_parent(quote.unwrap_or(widget_id))
                    .layer_id(ui.layer_id())
                    .max_rect(paragraph_rect),
            );
            ui.ctx().accesskit_node_builder(paragraph.id, |node| {
                set_paragraph_semantics(node, kind);
                node.set_is_line_breaking_object();
                node.set_bounds(accesskit_rect(to_global * paragraph_rect));
                if let Some((position, size)) = list_item {
                    node.set_position_in_set(position);
                    node.set_size_of_set(size);
                }
            });
            let from_galley = to_global
                * egui::emath::TSTransform::from_translation(paragraph_rect.min.to_vec2());
            let mut links = paragraph.links.iter().peekable();
            let mut link_ui: Option<(Range<usize>, Ui)> = None;
            for (index, run) in paragraph.runs.iter().enumerate() {
                if link_ui.as_ref().is_some_and(|(runs, _)| runs.end <= index) {
                    link_ui = None;
                }
                if let Some(link) = links.next_if(|link| link.runs.start == index) {
                    let link_rect = link.bounds.translate(paragraph_rect.min.to_vec2());
                    let child = Ui::new(
                        ui.ctx().clone(),
                        link.id,
                        egui::UiBuilder::new()
                            .accessibility_parent(paragraph.id)
                            .layer_id(ui.layer_id())
                            .max_rect(link_rect),
                    );
                    ui.ctx().accesskit_node_builder(link.id, |node| {
                        node.set_role(accesskit::Role::Link);
                        node.set_url(link.link.as_str());
                        node.set_label(link.label.as_str());
                        node.set_bounds(accesskit_rect(from_galley * link.bounds));
                        if ui.is_enabled() {
                            node.add_action(accesskit::Action::Click);
                        }
                    });
                    link_ui = Some((link.runs.clone(), child));
                }
                let parent_ui = link_ui.as_ref().map_or(&paragraph_ui, |(_, ui)| ui);
                let geometry = &run.geometry;
                let local_rect = geometry.bounds.translate(paragraph_rect.min.to_vec2());
                parent_ui.interact(
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

struct Rows<'a> {
    galley: &'a Galley,
    ends: Vec<usize>,
    text_ends: Vec<usize>,
}

impl<'a> Rows<'a> {
    fn new(galley: &'a Galley) -> Self {
        let mut ends = Vec::with_capacity(galley.rows.len());
        let mut text_ends = Vec::with_capacity(galley.rows.len());
        let mut start = 0;
        for row in &galley.rows {
            text_ends.push(start + row.char_count_excluding_newline().0);
            start += row.char_count_including_newline().0;
            ends.push(start);
        }
        Self {
            galley,
            ends,
            text_ends,
        }
    }

    fn containing(&self, scalar: usize) -> usize {
        self.ends
            .partition_point(|end| *end <= scalar)
            .min(self.ends.len() - 1)
    }

    fn caret(&self, index: usize, prefer_next_row: bool) -> (usize, Rect) {
        let rows = &self.galley.rows;
        let mut row = self.text_ends.partition_point(|end| *end < index);
        while let Some(placed) = rows.get(row) {
            let start = row.checked_sub(1).map_or(0, |previous| self.ends[previous]);
            if start > index {
                break;
            }
            let column = index - start;
            let next_row_instead = prefer_next_row
                && !placed.ends_with_newline
                && column >= placed.char_count_excluding_newline().0;
            if !next_row_instead {
                return (row, caret_rect(placed, column));
            }
            row += 1;
        }
        let last = rows.len() - 1;
        let column = rows[last].char_count_including_newline().0;
        (last, caret_rect(&rows[last], column))
    }
}

fn caret_rect(row: &PlacedRow, column: usize) -> Rect {
    let x = row.pos.x + row.x_offset(CharIndex(column));
    Rect::from_min_max(Pos2::new(x, row.min_y()), Pos2::new(x, row.max_y()))
}

fn freeze_run(
    run: RunBuilder,
    galley: &Galley,
    rows: &Rows<'_>,
    previous: Option<Id>,
    next: Option<Id>,
) -> Run {
    let geometry = run_geometry(&run, rows);
    let mut node = accesskit::Node::new(accesskit::Role::TextRun);
    node.set_value(run.value);
    node.set_character_lengths(run.lengths);
    if !run.word_starts.is_empty() {
        node.set_word_starts(run.word_starts);
    }
    node.set_text_direction(accesskit::TextDirection::LeftToRight);
    node.set_bounds(accesskit_rect(geometry.bounds));
    if geometry.positions.is_some() && geometry.widths.is_some() {
        node.set_character_positions(Vec::<f32>::new());
        node.set_character_widths(Vec::<f32>::new());
    }
    if let Some(previous) = previous {
        node.set_previous_on_line(previous.accesskit_id());
    }
    if let Some(next) = next {
        node.set_next_on_line(next.accesskit_id());
    }
    publish_style(
        &mut node,
        run.style,
        run.link.is_some(),
        run.layout_start,
        galley,
    );
    Run {
        id: run.id,
        range: run.range,
        offsets: run.offsets,
        row: run.row,
        line_break: run.line_break,
        soft_break: run.soft_break,
        node,
        geometry,
    }
}

fn run_geometry(run: &RunBuilder, rows: &Rows<'_>) -> Geometry {
    let mut bounds = Rect::NOTHING;
    let mut positions = Vec::with_capacity(run.lengths.len());
    let mut widths = Vec::with_capacity(run.lengths.len());
    let mut single_row = true;
    let soft_break = run.soft_break.then(|| run.scalar_offsets.len() - 2);
    for (index, pair) in run.scalar_offsets.windows(2).enumerate() {
        if soft_break == Some(index) {
            let (_, caret) = rows.caret(pair[0], false);
            bounds = bounds.union(caret);
            positions.push(caret.min.x);
            widths.push(0.0);
            continue;
        }
        let (start_row, start) = rows.caret(pair[0], true);
        let (end_row, end) = rows.caret(pair[1], false);
        single_row &= start_row == end_row;
        bounds = bounds.union(start).union(end);
        positions.push(start.min.x);
        widths.push((end.min.x - start.min.x).max(0.0));
    }
    if run.line_break || run.lengths.is_empty() {
        let (_, caret) = rows.caret(*run.scalar_offsets.last().unwrap_or(&0), false);
        bounds = bounds.union(caret);
        if run.line_break {
            positions.push(caret.min.x);
            widths.push(0.0);
        }
    }
    if !bounds.is_finite() {
        bounds = rows.galley.rect;
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

fn publish_style(
    node: &mut accesskit::Node,
    style: InlineStyle,
    linked: bool,
    byte: usize,
    galley: &Galley,
) {
    let byte = egui::text::ByteIndex(byte);
    let sections = &galley.job.sections;
    let index = sections.partition_point(|section| section.byte_range.end <= byte);
    let format = sections
        .get(index)
        .filter(|section| section.byte_range.contains(&byte))
        .or_else(|| sections.last())
        .map(|section| &section.format);
    let rgba = format.map_or([0, 0, 0, 255], |format| {
        format.color.to_srgba_unmultiplied()
    });
    apply_text_style(
        node,
        InlineStyle {
            underline: style.underline || linked,
            foreground: Some(crate::Color(rgba)),
            ..style
        },
    );
    if let Some(format) = format {
        node.set_font_size(format.font_id.size);
    }
}

#[cfg(test)]
mod tests {
    use super::super::layout::ParagraphLayout;
    use super::*;
    use crate::accessibility::random_edits;
    use std::collections::HashSet;

    fn make_snapshot(text: &str, width: f32) -> (Editor, Snapshot) {
        let editor = Editor::from_text(text);
        let context = egui::Context::default();
        let layouts = layout_frame(&context, &editor, width);
        let snapshot = build(&editor, &layouts, Id::unique("editor")).unwrap();
        (editor, snapshot)
    }

    fn layout_frame(context: &egui::Context, editor: &Editor, width: f32) -> ParagraphLayouts {
        let mut layouts = Vec::new();
        let appearance = super::super::Appearance {
            font: egui::FontId::proportional(16.0),
            bold_family: None,
            color: egui::Color32::WHITE,
            strong_color: egui::Color32::WHITE,
            code_background: egui::Color32::BLACK,
            link_color: egui::Color32::LIGHT_BLUE,
            width,
        };
        context
            .run_ui(egui::RawInput::default(), |ui| {
                layouts = editor
                    .document()
                    .paragraphs()
                    .iter()
                    .map(|paragraph| {
                        let job = super::super::paragraph_job(paragraph, &appearance, width);
                        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                        let rect = Rect::from_min_size(Pos2::ZERO, galley.size());
                        ParagraphLayout {
                            galley,
                            rect,
                            marker: None,
                            marker_position: Pos2::ZERO,
                            quote_bar: None,
                        }
                    })
                    .collect();
            })
            .drop_without_applying_deltas();
        ParagraphLayouts::from_test_layouts(layouts)
    }

    fn cached_layout_frame(
        context: &egui::Context,
        cache: &mut super::super::Cache,
        editor: &Editor,
        width: f32,
    ) -> ParagraphLayouts {
        let appearance = super::super::Appearance {
            font: egui::FontId::proportional(16.0),
            bold_family: None,
            color: egui::Color32::WHITE,
            strong_color: egui::Color32::WHITE,
            code_background: egui::Color32::BLACK,
            link_color: egui::Color32::LIGHT_BLUE,
            width,
        };
        let mut layouts = None;
        context
            .run_ui(egui::RawInput::default(), |ui| {
                layouts = Some(cache.layout(ui, editor, &appearance));
            })
            .drop_without_applying_deltas();
        layouts.unwrap()
    }

    fn normalized_node(
        paragraph: &ParagraphSnapshot,
        run: &Run,
    ) -> (accesskit::Node, Option<usize>, Option<usize>) {
        let index = |id: accesskit::NodeId| {
            paragraph
                .runs
                .iter()
                .position(|run| run.id.accesskit_id() == id)
                .unwrap()
        };
        let mut node = run.node.clone();
        let links = (
            node.previous_on_line().map(index),
            node.next_on_line().map(index),
        );
        node.clear_previous_on_line();
        node.clear_next_on_line();
        (node, links.0, links.1)
    }

    fn assert_matches_fresh(
        snapshot: &Snapshot,
        editor: &Editor,
        layouts: &ParagraphLayouts,
        widget_id: Id,
    ) {
        let fresh = build(editor, layouts, widget_id).unwrap();
        assert!(snapshot.is_current(editor, layouts));
        assert_eq!(snapshot.run_locations().len(), fresh.run_locations().len());
        assert_eq!(snapshot.paragraphs.len(), fresh.paragraphs.len());
        let mut serials = HashSet::new();
        for (index, (actual, expected)) in snapshot
            .paragraphs
            .iter()
            .zip(&fresh.paragraphs)
            .enumerate()
        {
            assert!(serials.insert(actual.serial));
            assert!(actual.serial < snapshot.next_serial);
            assert_eq!(actual.id, paragraph_id(widget_id, actual.serial));
            assert!(Arc::ptr_eq(&actual.source, &expected.source));
            assert!(Arc::ptr_eq(&actual.galley, &expected.galley));
            assert_eq!(actual.line_break, expected.line_break);
            assert_eq!(actual.runs.len(), expected.runs.len());
            for (run_index, (actual_run, expected_run)) in
                actual.runs.iter().zip(expected.runs.iter()).enumerate()
            {
                assert_eq!(actual_run.id, actual.id.with(("grapheme_run", run_index)));
                assert_eq!(
                    snapshot.run_locations()[&actual_run.id.accesskit_id()],
                    (index, run_index)
                );
                assert_eq!(
                    normalized_node(actual, actual_run),
                    normalized_node(expected, expected_run)
                );
                assert_eq!(actual_run.range, expected_run.range);
                assert_eq!(actual_run.line_break, expected_run.line_break);
                assert_eq!(actual_run.offsets, expected_run.offsets);
                assert_eq!(actual_run.row, expected_run.row);
                assert_eq!(actual_run.soft_break, expected_run.soft_break);
                assert_eq!(actual_run.geometry.bounds, expected_run.geometry.bounds);
                assert_eq!(
                    actual_run.geometry.positions,
                    expected_run.geometry.positions
                );
                assert_eq!(actual_run.geometry.widths, expected_run.geometry.widths);
            }
            assert_eq!(actual.links.len(), expected.links.len());
            for (link_index, (actual_link, expected_link)) in
                actual.links.iter().zip(expected.links.iter()).enumerate()
            {
                assert_eq!(actual_link.id, actual.id.with(("link", link_index)));
                assert_eq!(actual_link.link, expected_link.link);
                assert_eq!(actual_link.runs, expected_link.runs);
                assert_eq!(actual_link.bounds, expected_link.bounds);
                assert_eq!(actual_link.label, expected_link.label);
            }
        }
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
    }

    #[test]
    fn incremental_updates_match_fresh_snapshots_and_preserve_retained_values() {
        let context = egui::Context::default();
        let mut cache = super::super::Cache::default();
        let mut editor = Editor::from_text("first\naé👩\u{200d}💻 second\nlast");
        let widget_id = Id::unique("editor");
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        let mut snapshot = None;
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let identity = Arc::as_ptr(snapshot.as_ref().unwrap());
        let storage = snapshot.as_ref().unwrap().paragraphs.as_ptr();
        let unchanged = Arc::clone(&snapshot.as_ref().unwrap().paragraphs[0]);
        editor
            .set_selection(crate::Selection::caret(Position::new(1, 0)))
            .unwrap();
        editor.insert_text("changed ").unwrap();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        assert_eq!(identity, Arc::as_ptr(snapshot.as_ref().unwrap()));
        assert_eq!(storage, snapshot.as_ref().unwrap().paragraphs.as_ptr());
        assert!(Arc::ptr_eq(
            &unchanged,
            &snapshot.as_ref().unwrap().paragraphs[0]
        ));
        assert_matches_fresh(snapshot.as_deref().unwrap(), &editor, &layouts, widget_id);

        let retained = Arc::clone(snapshot.as_ref().unwrap());
        let retained_editor = Editor::new(editor.document().clone());
        let retained_layouts = layouts.clone();
        editor.insert_text("é ").unwrap();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        assert!(!Arc::ptr_eq(&retained, snapshot.as_ref().unwrap()));
        assert_matches_fresh(&retained, &retained_editor, &retained_layouts, widget_id);
        assert_matches_fresh(snapshot.as_deref().unwrap(), &editor, &layouts, widget_id);
    }

    #[test]
    fn missed_updates_and_simultaneous_width_changes_match_fresh_snapshots() {
        let context = egui::Context::default();
        let mut cache = super::super::Cache::default();
        let mut editor =
            Editor::from_text("first second third fourth\naé👩\u{200d}💻 second\nlast words here");
        let widget_id = Id::unique("editor");
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        let mut snapshot = None;
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        for paragraph in [0, 2] {
            editor
                .set_selection(crate::Selection::caret(Position::new(paragraph, 0)))
                .unwrap();
            editor.insert_text("changed ").unwrap();
        }
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        assert_matches_fresh(snapshot.as_deref().unwrap(), &editor, &layouts, widget_id);
        let before = Arc::clone(&snapshot.as_ref().unwrap().paragraphs[1]);
        editor.insert_text("another ").unwrap();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 65.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        assert!(!Arc::ptr_eq(
            &before,
            &snapshot.as_ref().unwrap().paragraphs[1]
        ));
        assert_matches_fresh(snapshot.as_deref().unwrap(), &editor, &layouts, widget_id);
    }

    #[test]
    fn failed_incremental_updates_keep_the_published_snapshot_intact() {
        let context = egui::Context::default();
        let mut cache = super::super::Cache::default();
        let mut editor = Editor::from_text("first\nvalid\nlast");
        let widget_id = Id::unique("editor");
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        let mut snapshot = None;
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let identity = Arc::as_ptr(snapshot.as_ref().unwrap());
        let published_editor = Editor::new(editor.document().clone());
        let published_layouts = layouts.clone();
        editor
            .set_selection(crate::Selection::caret(Position::new(1, 0)))
            .unwrap();
        editor.insert_text("changed ").unwrap();
        assert_eq!(
            update_cached(&editor, &layouts, widget_id, &mut snapshot),
            Err(AccessibilityError::InvalidLayout)
        );
        assert_eq!(identity, Arc::as_ptr(snapshot.as_ref().unwrap()));
        assert_matches_fresh(
            snapshot.as_deref().unwrap(),
            &published_editor,
            &published_layouts,
            widget_id,
        );
    }

    #[test]
    fn local_edits_reuse_unchanged_paragraph_runs_and_geometry() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("first\naé👩\u{200d}💻 second\nlast");
        let widget_id = Id::unique("editor");
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
        let other_id = build_cached(&editor, &layouts, Id::unique("other"), Some(&edited)).unwrap();
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
        let widget_id = Id::unique("editor");
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
        let widget_id = Id::unique("editor");
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
        let widget_id = Id::unique("editor");
        let layouts = layout_frame(&context, &editor, 500.0);
        let snapshot = build(&editor, &layouts, widget_id).unwrap();
        let position = snapshot.position(Position::new(1, 2)).unwrap();
        let references = Arc::strong_count(&snapshot.paragraphs[0]);
        let mut changed = Editor::new(editor.document().clone());
        changed
            .set_selection(crate::Selection::caret(Position::new(1, 0)))
            .unwrap();
        changed.insert_text("changed ").unwrap();
        let layouts = layout_frame(&context, &changed, 500.0);
        let mut invalid: Vec<_> = layouts.iter().collect();
        invalid[1].galley = Arc::clone(&invalid[0].galley);
        let invalid = ParagraphLayouts::from_test_layouts(invalid);
        assert!(matches!(
            build_cached(&changed, &invalid, widget_id, Some(&snapshot)),
            Err(AccessibilityError::InvalidLayout)
        ));
        let truncated = ParagraphLayouts::from_test_layouts(layouts.iter().take(2).collect());
        assert!(matches!(
            build_cached(&changed, &truncated, widget_id, Some(&snapshot)),
            Err(AccessibilityError::InvalidLayout)
        ));
        assert_eq!(Arc::strong_count(&snapshot.paragraphs[0]), references);
        assert_eq!(snapshot.decode(position), Some(Position::new(1, 2)));
    }

    #[test]
    fn emoji_and_combining_marks_expose_only_real_caret_stops() {
        let (_, snapshot) = make_snapshot("a👩🏽\u{200d}💻e\u{301}", 500.0);
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
        for run in wrapped.paragraphs[0].runs.iter() {
            assert_eq!(
                run.geometry.positions.as_ref().unwrap().len(),
                run.node.character_lengths().len()
            );
        }
    }

    #[test]
    fn runs_hold_at_most_255_characters_including_the_line_break() {
        let text = format!("{}\nnext", "ab ".repeat(85));
        let (_, snapshot) = make_snapshot(&text, 5000.0);
        let runs = &snapshot.paragraphs[0].runs;
        let lengths: Vec<_> = runs
            .iter()
            .map(|run| run.node.character_lengths().len())
            .collect();
        assert_eq!(lengths, [254, 2]);
        assert_eq!(runs[1].node.value(), Some(" \n"));

        let context = egui::Context::default();
        context.enable_accesskit();
        let id = Id::unique("editor");
        let mut editor = Editor::from_text(&text);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(5000.0, 400.0))),
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| {
            super::super::RichTextEditor::new(&mut editor)
                .id(id)
                .show(ui);
        });
        let update = output.platform_output.accesskit_update.take().unwrap();
        output.drop_without_applying_deltas();
        let tree = accesskit_consumer::Tree::new(update, true);
        let node = tree
            .state()
            .node_by_tree_local_id(id.accesskit_id(), accesskit::TreeId::ROOT)
            .unwrap();
        let last_word = node.text_position_from_global_usv_index(252).unwrap();
        let paragraph_end = last_word.forward_to_word_end();
        assert_eq!(paragraph_end.to_global_usv_index(), 256);
        assert_eq!(
            paragraph_end.backward_to_word_start().to_global_usv_index(),
            252
        );
    }

    #[test]
    fn stale_documents_are_detected() {
        let (mut editor, snapshot) = make_snapshot("valid", 500.0);
        assert!(snapshot.matches_document(&editor));
        editor.insert_text("new").unwrap();
        assert!(!snapshot.matches_document(&editor));
    }

    #[test]
    fn snapshots_validate_cloned_content_and_decode_wrapped_paragraphs() {
        let (editor, snapshot) = make_snapshot("first\naé👩\u{200d}💻 second third\n", 65.0);
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
        let (editor, snapshot) = make_snapshot("a👩🏽\u{200d}💻e\u{301}", 500.0);
        let context = egui::Context::default();
        context.enable_accesskit();
        let paragraph = &snapshot.paragraphs[0];
        let run_id = paragraph.runs[0].id.accesskit_id();
        let layouts = ParagraphLayouts::from_test_layouts(vec![ParagraphLayout {
            galley: Arc::clone(&paragraph.galley),
            rect: Rect::from_min_size(Pos2::ZERO, paragraph.galley.size()),
            marker: None,
            marker_position: Pos2::ZERO,
            quote_bar: None,
        }]);
        let publish_bounds = |origin| {
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                snapshot.publish(
                    ui,
                    Id::unique("editor"),
                    &editor,
                    &layouts,
                    origin,
                    false,
                    true,
                );
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
        layouts: &ParagraphLayouts,
        placement: (Pos2, egui::emath::TSTransform),
        enabled: bool,
        read_only: bool,
        legacy: bool,
    ) -> (accesskit::TreeUpdate, Vec<egui::Response>) {
        let (origin, transform) = placement;
        let context = egui::Context::default();
        context.enable_accesskit();
        let widget_id = Id::unique("editor");
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
                for (paragraph, layout) in snapshot.paragraphs.iter().zip(layouts.iter()) {
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
                            let text = paragraph.source.text();
                            let word_starts: Vec<_> = paragraph
                                .source
                                .word_starts()
                                .into_iter()
                                .filter(|byte| run.range.contains(byte))
                                .map(|byte| {
                                    let graphemes = text[run.range.start..byte].graphemes(true);
                                    u8::try_from(graphemes.count()).unwrap()
                                })
                                .collect();
                            node.set_value(value);
                            node.set_character_lengths(lengths);
                            if !word_starts.is_empty() {
                                node.set_word_starts(word_starts);
                            }
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
                            let (style, linked) = paragraph
                                .source
                                .spans()
                                .iter()
                                .find(|span| span.range.contains(&run.range.start))
                                .map_or((InlineStyle::default(), false), |span| {
                                    (span.style, span.link.is_some())
                                });
                            publish_style(node, style, linked, run.range.start, &paragraph.galley);
                        });
                    }
                }
            } else {
                snapshot.publish(ui, widget_id, editor, layouts, origin, read_only, true);
            }
            responses = snapshot
                .paragraphs
                .iter()
                .flat_map(|paragraph| paragraph.runs.iter())
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
        let mut editor =
            Editor::from_text("first café e\u{301} 👩\u{200d}💻 second third fourth\n\nlast 🇺🇸");
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
                background: Some(Some(crate::Color::YELLOW)),
            })
            .unwrap();
        let layouts = layout_frame(&context, &editor, 65.0);
        let snapshot = build(&editor, &layouts, Id::unique("editor")).unwrap();
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
                quote_bar: None,
            })
            .collect();
        let layouts = ParagraphLayouts::from_test_layouts(layouts);
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
            let widget = &nodes[&Id::unique("editor").accesskit_id()];
            let children: Vec<_> = snapshot
                .paragraphs
                .iter()
                .map(|paragraph| paragraph.id.accesskit_id())
                .collect();
            assert_eq!(widget.children(), children);
            assert_eq!(
                widget.children(),
                old_nodes[&Id::unique("editor").accesskit_id()].children()
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
            for (paragraph, layout) in snapshot.paragraphs.iter().zip(layouts.iter()) {
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
                for run in paragraph.runs.iter() {
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

    fn snapshot_for(editor: &Editor, width: f32) -> (ParagraphLayouts, Snapshot) {
        let layouts = layout_frame(&egui::Context::default(), editor, width);
        let snapshot = build(editor, &layouts, Id::unique("editor")).unwrap();
        (layouts, snapshot)
    }

    fn published_nodes(
        editor: &Editor,
        snapshot: &Snapshot,
        layouts: &ParagraphLayouts,
        focus_prefer_next_row: bool,
    ) -> HashMap<accesskit::NodeId, accesskit::Node> {
        let context = egui::Context::default();
        context.enable_accesskit();
        let widget_id = Id::unique("editor");
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            ui.interact(
                Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 500.0)),
                widget_id,
                egui::Sense::click_and_drag(),
            );
            let origin = Pos2::ZERO;
            snapshot.publish(
                ui,
                widget_id,
                editor,
                layouts,
                origin,
                false,
                focus_prefer_next_row,
            );
        });
        let update = output.platform_output.accesskit_update.take().unwrap();
        output.drop_without_applying_deltas();
        update.nodes.into_iter().collect()
    }

    fn select(editor: &mut Editor, start: (usize, usize), end: (usize, usize)) {
        editor
            .set_selection(crate::Selection::new(
                Position::new(start.0, start.1),
                Position::new(end.0, end.1),
            ))
            .unwrap();
    }

    #[test]
    fn structural_edits_reuse_shifted_paragraphs_and_their_node_ids() {
        let context = egui::Context::default();
        let mut cache = super::super::Cache::default();
        let mut editor = Editor::from_text("zero zero\none\ntwo\nthree\nfour");
        let widget_id = Id::unique("editor");
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        let mut snapshot = None;
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let before = snapshot.as_ref().unwrap().paragraphs.clone();
        let in_two = accesskit::TextPosition {
            node: before[2].runs[0].id.accesskit_id(),
            character_index: 1,
        };

        select(&mut editor, (0, 4), (0, 4));
        editor.insert_paragraph().unwrap();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let split = Arc::clone(snapshot.as_ref().unwrap());
        assert_eq!(split.paragraphs[0].id, before[0].id);
        assert!(before.iter().all(|old| old.id != split.paragraphs[1].id));
        for (old, new) in before[1..].iter().zip(&split.paragraphs[2..]) {
            assert!(Arc::ptr_eq(old, new));
        }
        assert_eq!(split.decode(in_two), Some(Position::new(3, 1)));
        assert_matches_fresh(&split, &editor, &layouts, widget_id);

        let in_removed = accesskit::TextPosition {
            node: split.paragraphs[3].runs[0].id.accesskit_id(),
            character_index: 1,
        };
        select(&mut editor, (3, 0), (3, 0));
        editor.delete_backward().unwrap();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let joined = snapshot.as_ref().unwrap();
        assert_eq!(editor.document().paragraphs()[2].text(), "onetwo");
        assert_eq!(joined.paragraphs[2].id, split.paragraphs[2].id);
        assert_eq!(joined.decode(in_removed), None);
        for index in 3..5 {
            assert!(Arc::ptr_eq(
                &split.paragraphs[index + 1],
                &joined.paragraphs[index]
            ));
        }
        assert_matches_fresh(joined, &editor, &layouts, widget_id);
    }

    #[test]
    fn several_changes_before_a_frame_rebuild_only_the_changed_window() {
        let context = egui::Context::default();
        let mut cache = super::super::Cache::default();
        let mut editor = Editor::from_text("zero\none\ntwo\nthree\nfour\nfive\nsix");
        let widget_id = Id::unique("editor");
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        let mut snapshot = None;
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let before = Arc::clone(snapshot.as_ref().unwrap());
        let in_five = accesskit::TextPosition {
            node: before.paragraphs[5].runs[0].id.accesskit_id(),
            character_index: 2,
        };
        assert_eq!(before.decode(in_five), Some(Position::new(5, 2)));

        select(&mut editor, (1, 3), (1, 3));
        editor.insert_paragraph().unwrap();
        select(&mut editor, (4, 0), (4, 0));
        editor.insert_text("3: ").unwrap();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let after = snapshot.as_ref().unwrap();
        assert!(!Arc::ptr_eq(&before, after));
        for index in 0..2 {
            assert!(Arc::ptr_eq(
                &before.paragraphs[index],
                &after.paragraphs[index]
            ));
        }
        assert!(Arc::ptr_eq(&before.paragraphs[2], &after.paragraphs[3]));
        for index in 4..7 {
            assert!(Arc::ptr_eq(
                &before.paragraphs[index],
                &after.paragraphs[index + 1]
            ));
        }
        assert_eq!(after.decode(in_five), Some(Position::new(6, 2)));
        assert_matches_fresh(after, &editor, &layouts, widget_id);

        editor.select_all();
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: None,
            })
            .unwrap();
        editor.undo();
        assert!(editor.redo());
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        assert_matches_fresh(snapshot.as_deref().unwrap(), &editor, &layouts, widget_id);
    }

    #[test]
    fn random_edit_frames_and_rewraps_match_fresh_snapshots() {
        random_frames_match_fresh_snapshots(
            random_edits::seeded(0x0dd5_eed5_ba5e_ba11),
            12,
            false,
            random_edits::editor,
            random_edits::apply,
        );
    }

    #[test]
    fn sparse_edit_frames_match_fresh_snapshots_and_layouts() {
        crate::document::take_sparse_coverage();
        random_frames_match_fresh_snapshots(
            random_edits::seeded(0x5851_f42d_4c95_7f2d),
            3,
            true,
            random_edits::sparse_editor,
            random_edits::apply_sparse,
        );
        random_edits::assert_sparse_coverage([20, 10, 10]);
    }

    fn random_frames_match_fresh_snapshots<R: FnMut(usize) -> usize>(
        mut random: R,
        editors: usize,
        fresh_layouts: bool,
        editor: fn() -> Editor,
        apply: fn(&mut Editor, &mut R),
    ) {
        let context = egui::Context::default();
        let widget_id = Id::unique("editor");
        for _ in 0..editors {
            let mut cache = super::super::Cache::default();
            let mut editor = editor();
            let mut width = 400.0;
            let mut snapshot = None;
            for _ in 0..50 {
                for _ in 0..random(3) {
                    apply(&mut editor, &mut random);
                }
                if random(4) == 0 {
                    width = [400.0, 160.0, 70.0][random(3)];
                }
                let layouts = cached_layout_frame(&context, &mut cache, &editor, width);
                let _retained = (random(4) == 0).then(|| snapshot.clone());
                update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
                assert_matches_fresh(snapshot.as_deref().unwrap(), &editor, &layouts, widget_id);
                if fresh_layouts {
                    assert_layouts_match_fresh(&context, &editor, &layouts, width);
                }
            }
        }
    }

    fn assert_layouts_match_fresh(
        context: &egui::Context,
        editor: &Editor,
        layouts: &ParagraphLayouts,
        width: f32,
    ) {
        let mut cache = super::super::Cache::default();
        let fresh = cached_layout_frame(context, &mut cache, editor, width);
        assert_eq!(layouts.len(), fresh.len());
        assert_eq!(layouts.content_height(), fresh.content_height());
        let marker =
            |layout: &ParagraphLayout| layout.marker.as_ref().map(|marker| marker.job.text.clone());
        for (index, (actual, expected)) in layouts.iter().zip(fresh.iter()).enumerate() {
            assert_eq!(actual.galley.job, expected.galley.job);
            assert_eq!(actual.rect, expected.rect);
            assert_eq!(marker(&actual), marker(&expected));
            assert_eq!(actual.marker_position, expected.marker_position);
            assert_eq!(actual.quote_bar, expected.quote_bar);
            assert_eq!(layouts.char_offset(index), fresh.char_offset(index));
            assert_eq!(
                layouts.contains_list(index..layouts.len()),
                fresh.contains_list(index..fresh.len())
            );
        }
    }

    #[test]
    fn unshared_snapshots_splice_structural_edits_in_place() {
        let context = egui::Context::default();
        let mut cache = super::super::Cache::default();
        let long = "zero ".repeat(30);
        let mut editor = Editor::from_text(&format!("{long}\none\ntwo\nthree\nfour"));
        let widget_id = Id::unique("editor");
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        let mut snapshot = None;
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let first = snapshot.as_ref().unwrap();
        let identity = Arc::as_ptr(first);
        assert!(first.paragraphs[0].runs.len() > 1);
        let three = first.paragraphs[3].id;
        let in_three = accesskit::TextPosition {
            node: first.paragraphs[3].runs[0].id.accesskit_id(),
            character_index: 2,
        };
        assert_eq!(first.decode(in_three), Some(Position::new(3, 2)));

        select(&mut editor, (1, 1), (1, 1));
        editor.insert_paragraph().unwrap();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let split = snapshot.as_ref().unwrap();
        assert_eq!(identity, Arc::as_ptr(split));
        assert_eq!(split.paragraphs[4].id, three);
        assert_eq!(split.decode(in_three), Some(Position::new(4, 2)));
        assert_matches_fresh(split, &editor, &layouts, widget_id);

        select(&mut editor, (0, 4), (0, long.len()));
        editor.delete_backward().unwrap();
        select(&mut editor, (5, 0), (5, 0));
        editor.insert_text("edited ").unwrap();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let edited = snapshot.as_ref().unwrap();
        assert_eq!(identity, Arc::as_ptr(edited));
        assert_eq!(edited.paragraphs[0].runs.len(), 1);
        assert_eq!(edited.decode(in_three), Some(Position::new(4, 2)));
        assert_matches_fresh(edited, &editor, &layouts, widget_id);

        select(&mut editor, (1, 0), (3, 0));
        editor.delete_backward().unwrap();
        select(&mut editor, (3, 0), (3, 0));
        editor.insert_text("last ").unwrap();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let joined = snapshot.as_ref().unwrap();
        assert_eq!(identity, Arc::as_ptr(joined));
        assert_eq!(joined.decode(in_three), Some(Position::new(2, 2)));
        assert_matches_fresh(joined, &editor, &layouts, widget_id);

        assert!(editor.undo());
        assert!(editor.undo());
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let undone = snapshot.as_ref().unwrap();
        assert_eq!(identity, Arc::as_ptr(undone));
        assert_eq!(undone.decode(in_three), Some(Position::new(4, 2)));
        assert_matches_fresh(undone, &editor, &layouts, widget_id);
    }

    #[test]
    fn repeated_paragraphs_keep_distinct_node_ids() {
        let paragraph = Arc::clone(&crate::Document::from_text("same").paragraphs()[0]);
        let editor = Editor::new(crate::Document::from_fragment(
            &crate::Fragment::from_paragraphs(vec![Arc::clone(&paragraph); 3]),
        ));
        let context = egui::Context::default();
        let mut cache = super::super::Cache::default();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        let widget_id = Id::unique("editor");
        let first = build(&editor, &layouts, widget_id).unwrap();
        assert_matches_fresh(&first, &editor, &layouts, widget_id);
        let rebuilt = build_cached(&editor, &layouts, widget_id, Some(&first)).unwrap();
        for (before, after) in first.paragraphs.iter().zip(&rebuilt.paragraphs) {
            assert!(Arc::ptr_eq(before, after));
        }
        let ids: HashSet<_> = rebuilt.paragraphs.iter().map(|p| p.id).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn renumbered_list_items_keep_their_runs_and_node_ids() {
        let context = egui::Context::default();
        let mut cache = super::super::Cache::default();
        let mut editor = Editor::from_text("one\ntwo\nthree\nfour");
        editor.select_all();
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: None,
            })
            .unwrap();
        let widget_id = Id::unique("editor");
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        let mut snapshot = None;
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let before = Arc::clone(snapshot.as_ref().unwrap());
        select(&mut editor, (0, 3), (0, 3));
        editor.insert_paragraph().unwrap();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let after = Arc::clone(snapshot.as_ref().unwrap());
        for (old, new) in before.paragraphs[1..].iter().zip(&after.paragraphs[2..]) {
            assert!(Arc::ptr_eq(old, new));
        }
        assert_matches_fresh(&after, &editor, &layouts, widget_id);

        select(&mut editor, (2, 0), (2, 0));
        editor.set_paragraph_kind(ParagraphKind::Body).unwrap();
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let converted = snapshot.as_ref().unwrap();
        for index in [0, 1, 3, 4] {
            assert!(Arc::ptr_eq(
                &after.paragraphs[index],
                &converted.paragraphs[index]
            ));
        }
        assert_eq!(editor.document().list_number(3), Some(3));
        assert_matches_fresh(converted, &editor, &layouts, widget_id);
    }

    #[test]
    fn renumbered_list_items_follow_rewraps_from_wider_markers() {
        let context = egui::Context::default();
        let mut cache = super::super::Cache::default();
        let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi \
                    omicron pi rho sigma tau upsilon phi chi psi omega alpha beta gamma delta";
        let mut editor = Editor::from_text(&format!("a\n{long}"));
        editor.select_all();
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: None,
            })
            .unwrap();
        let widget_id = Id::unique("editor");
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 400.0);
        let mut snapshot = None;
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let before = Arc::clone(&snapshot.as_ref().unwrap().paragraphs[1]);

        select(&mut editor, (0, 0), (0, 0));
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: Some(1_000_000),
            })
            .unwrap();
        assert_eq!(editor.document().list_number(1), Some(1_000_001));
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 400.0);
        assert!(!Arc::ptr_eq(
            &before.galley,
            &layouts.get(1).unwrap().galley
        ));
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let renumbered = snapshot.as_ref().unwrap();
        assert_eq!(renumbered.paragraphs[1].id, before.id);
        assert_matches_fresh(renumbered, &editor, &layouts, widget_id);

        let layouts = cached_layout_frame(&context, &mut cache, &editor, 400.0);
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        assert_matches_fresh(snapshot.as_deref().unwrap(), &editor, &layouts, widget_id);
    }

    #[test]
    fn single_paragraph_edits_skip_the_scan_unless_other_paragraphs_can_rewrap() {
        type Edit = fn(&mut Editor);
        let context = egui::Context::default();
        let mut cache = super::super::Cache::default();
        let mut editor = Editor::from_text("title\none\ntwo\nquoted\nlast words");
        select(&mut editor, (0, 0), (0, 0));
        editor
            .set_paragraph_kind(ParagraphKind::Heading { level: 4 })
            .unwrap();
        select(&mut editor, (1, 0), (2, 3));
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: None,
            })
            .unwrap();
        let widget_id = Id::unique("editor");
        let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
        let mut snapshot = None;
        update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
        let identity = Arc::as_ptr(snapshot.as_ref().unwrap());
        let edits: [(Edit, f32, Option<usize>); 8] = [
            (
                |editor| {
                    select(editor, (1, 3), (1, 3));
                    editor.insert_text(" more").unwrap();
                },
                500.0,
                Some(1),
            ),
            (
                |editor| {
                    select(editor, (0, 0), (0, 0));
                    editor
                        .set_paragraph_kind(ParagraphKind::Heading { level: 5 })
                        .unwrap();
                },
                500.0,
                Some(0),
            ),
            (
                |editor| {
                    select(editor, (3, 0), (3, 0));
                    editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
                },
                500.0,
                Some(3),
            ),
            (
                |editor| {
                    select(editor, (1, 0), (1, 0));
                    editor
                        .set_paragraph_kind(ParagraphKind::Ordered {
                            indent: 0,
                            start: Some(1_000_000),
                        })
                        .unwrap();
                },
                500.0,
                None,
            ),
            (
                |editor| {
                    select(editor, (2, 0), (2, 0));
                    editor.set_paragraph_kind(ParagraphKind::Body).unwrap();
                },
                500.0,
                None,
            ),
            (
                |editor| {
                    select(editor, (4, 4), (4, 4));
                    editor.insert_paragraph().unwrap();
                },
                500.0,
                None,
            ),
            (
                |editor| {
                    select(editor, (0, 0), (0, 0));
                    editor.insert_text("a").unwrap();
                    select(editor, (2, 0), (2, 0));
                    editor.insert_text("b").unwrap();
                },
                500.0,
                None,
            ),
            (
                |editor| {
                    select(editor, (2, 0), (2, 0));
                    editor.insert_text("c").unwrap();
                },
                65.0,
                None,
            ),
        ];
        for (edit, width, expected) in edits {
            let before = snapshot.as_ref().unwrap().paragraphs.clone();
            edit(&mut editor);
            let layouts = cached_layout_frame(&context, &mut cache, &editor, width);
            let previous = snapshot.as_ref().unwrap();
            assert_eq!(previous.edited_paragraph(&editor, &layouts), expected);
            update_cached(&editor, &layouts, widget_id, &mut snapshot).unwrap();
            let after = snapshot.as_ref().unwrap();
            assert_eq!(identity, Arc::as_ptr(after));
            if let Some(index) = expected {
                assert_eq!(after.paragraphs[index].id, before[index].id);
                for (other, (old, new)) in before.iter().zip(&after.paragraphs).enumerate() {
                    assert_eq!(Arc::ptr_eq(old, new), other != index);
                }
            }
            assert_matches_fresh(after, &editor, &layouts, widget_id);
        }
    }

    #[test]
    fn row_index_carets_match_galley_cursor_queries() {
        let texts = [
            String::new(),
            "first second third fourth fifth".to_owned(),
            "e\u{301}".repeat(40),
            "a 👨\u{200d}👩\u{200d}👧\u{200d}👦 b 🇺🇸🇺🇸 c 👩🏽\u{200d}💻".to_owned(),
            "line\nbreaks\n\nend\n".to_owned(),
            "w".repeat(30),
        ];
        let context = egui::Context::default();
        context
            .run_ui(egui::RawInput::default(), |ui| {
                for text in &texts {
                    for width in [5.0, 65.0, 500.0] {
                        let galley = ui.fonts_mut(|fonts| {
                            fonts.layout_job(egui::text::LayoutJob::simple(
                                text.clone(),
                                egui::FontId::proportional(16.0),
                                egui::Color32::WHITE,
                                width,
                            ))
                        });
                        let rows = Rows::new(&galley);
                        for index in 0..=galley.end().index.0 {
                            for prefer_next_row in [false, true] {
                                let cursor = egui::text::CCursor {
                                    index: CharIndex(index),
                                    prefer_next_row,
                                };
                                assert_eq!(
                                    rows.caret(index, prefer_next_row),
                                    (
                                        galley.layout_from_cursor(cursor).row,
                                        galley.pos_from_cursor(cursor)
                                    ),
                                    "text={text:?}, width={width}, cursor={cursor:?}"
                                );
                            }
                        }
                    }
                }
            })
            .drop_without_applying_deltas();
    }

    #[test]
    fn oversized_graphemes_publish_scalar_aligned_pieces() {
        let cluster = format!("x{}", "\u{301}".repeat(200));
        let text = format!("ab {cluster} cd");
        let (_, snapshot) = make_snapshot(&text, 5000.0);
        let runs = &snapshot.paragraphs[0].runs;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].node.value(), Some(text.as_str()));
        assert_eq!(
            runs[0].node.character_lengths(),
            [1, 1, 1, 255, 146, 1, 1, 1]
        );
        assert_eq!(runs[0].node.word_starts(), [0, 3, 6]);
        let node = runs[0].id.accesskit_id();
        let end = 3 + cluster.len();
        for (character_index, byte) in [(3, 3), (4, 3), (5, end), (8, text.len())] {
            let position = accesskit::TextPosition {
                node,
                character_index,
            };
            assert_eq!(snapshot.decode(position), Some(Position::new(0, byte)));
        }
        for (byte, character_index) in [(3, 3), (end, 5)] {
            let position = snapshot.position(Position::new(0, byte)).unwrap();
            assert_eq!(position.character_index, character_index);
        }
        assert!(snapshot.position(Position::new(0, 4)).is_none());

        let text = format!("{}{cluster}", "a".repeat(254));
        let (_, snapshot) = make_snapshot(&text, 5000.0);
        let runs = &snapshot.paragraphs[0].runs;
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].node.character_lengths().len(), 255);
        assert_eq!(runs[1].node.character_lengths(), [146]);
        assert!(runs[1].node.word_starts().is_empty());
        let start = Position::new(0, 254);
        let end = Position::new(0, text.len());
        assert_eq!(
            snapshot.position(start),
            Some(accesskit::TextPosition {
                node: runs[0].id.accesskit_id(),
                character_index: 254,
            })
        );
        let continued = accesskit::TextPosition {
            node: runs[1].id.accesskit_id(),
            character_index: 0,
        };
        assert_eq!(snapshot.decode(continued), Some(start));
        assert_eq!(snapshot.decode(snapshot.position(end).unwrap()), Some(end));
    }

    #[test]
    fn oversized_graphemes_keep_idle_frames_cached_and_accept_set_value() {
        use accesskit::{Action, ActionData, ActionRequest, TreeId};
        let context = egui::Context::default();
        context.enable_accesskit();
        let id = Id::unique("editor");
        let mut editor = Editor::from_text(&format!("valid\nx{}", "\u{301}".repeat(200)));
        let frame = |editor: &mut Editor, events| {
            let mut errors = Vec::new();
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            context
                .run_ui(input, |ui| {
                    errors = super::super::RichTextEditor::new(editor)
                        .id(id)
                        .show(ui)
                        .accessibility_errors;
                })
                .drop_without_applying_deltas();
            let cache = context.data(|data| data.get_temp::<super::super::Cache>(id));
            (errors, cache.unwrap().accessibility.unwrap())
        };
        let (errors, first) = frame(&mut editor, vec![]);
        assert!(errors.is_empty());
        let (errors, idle) = frame(&mut editor, vec![]);
        assert!(errors.is_empty());
        assert!(Arc::ptr_eq(&first, &idle));
        assert_eq!(
            idle.paragraphs[1].runs[0].node.character_lengths(),
            [255, 146]
        );

        let request = ActionRequest {
            action: Action::SetValue,
            target_tree: TreeId::ROOT,
            target_node: id.accesskit_id(),
            data: Some(ActionData::Value("recovered".into())),
        };
        let event = egui::Event::AccessKitActionRequest(request);
        let (errors, _) = frame(&mut editor, vec![event]);
        assert!(errors.is_empty());
        assert_eq!(editor.document().plain_text(), "recovered");
    }

    #[test]
    fn text_runs_publish_word_starts_matching_word_navigation() {
        let (editor, snapshot) = make_snapshot("hello, world  foo", 500.0);
        let run = &snapshot.paragraphs[0].runs[0];
        assert_eq!(run.node.word_starts(), [0, 5, 7, 14]);
        let paragraph = &editor.document().paragraphs()[0];
        let mut byte = paragraph.text().len();
        let mut stops = Vec::new();
        while byte > 0 {
            byte = paragraph.previous_word(byte);
            stops.push(u8::try_from(byte).unwrap());
        }
        stops.reverse();
        assert_eq!(run.node.word_starts(), stops);

        let (_, snapshot) = make_snapshot("  lead", 500.0);
        assert_eq!(snapshot.paragraphs[0].runs[0].node.word_starts(), [2]);

        let (_, snapshot) = make_snapshot(&format!("{} tail", "a".repeat(300)), 5000.0);
        let runs = &snapshot.paragraphs[0].runs;
        assert_eq!(runs[0].node.word_starts(), [0]);
        assert_eq!(runs[1].node.word_starts(), [46]);
        let mut editor = Editor::from_text("word play");
        select(&mut editor, (0, 0), (0, 2));
        editor
            .apply_style(crate::StylePatch {
                bold: Some(true),
                ..crate::StylePatch::default()
            })
            .unwrap();
        let (_, snapshot) = snapshot_for(&editor, 500.0);
        let runs = &snapshot.paragraphs[0].runs;
        assert_eq!(runs[0].node.word_starts(), [0]);
        assert_eq!(runs[1].node.word_starts(), [3]);

        let text = "first second third fourth fifth";
        let (_, snapshot) = make_snapshot(text, 65.0);
        let runs = &snapshot.paragraphs[0].runs;
        assert!(runs.len() > 1);
        let published: Vec<_> = runs
            .iter()
            .flat_map(|run| {
                let offsets = &run.offsets;
                run.node
                    .word_starts()
                    .iter()
                    .map(move |index| offsets[usize::from(*index)])
            })
            .collect();
        let expected: Vec<_> = text.unicode_word_indices().map(|(byte, _)| byte).collect();
        assert_eq!(published, expected);
    }

    #[test]
    fn soft_line_breaks_end_rows_and_publish_newlines() {
        let mut editor = Editor::from_text("a\u{2028}bc\u{2028}d\u{2028}\nnext");
        let red = crate::Color([255, 0, 0, 255]);
        select(&mut editor, (0, 4), (0, 5));
        editor
            .apply_style(crate::StylePatch {
                foreground: Some(Some(red)),
                ..crate::StylePatch::default()
            })
            .unwrap();
        let (layouts, snapshot) = snapshot_for(&editor, 500.0);
        let galley = &layouts.get(0).unwrap().galley;
        assert_eq!(galley.job.text, "a\nbc\nd\n");
        let paragraph = &snapshot.paragraphs[0];
        let runs = &paragraph.runs;
        let values: Vec<_> = runs.iter().map(|run| run.node.value().unwrap()).collect();
        assert_eq!(values, ["a\n", "b", "c\n", "d\n", "\n"]);
        let rows: Vec<_> = runs.iter().map(|run| run.row).collect();
        assert_eq!(rows, [0, 1, 1, 2, 3]);
        let soft_breaks: Vec<_> = runs.iter().map(|run| run.soft_break).collect();
        assert_eq!(soft_breaks, [true, false, true, true, false]);
        let links: Vec<_> = runs
            .iter()
            .map(|run| {
                let (_, previous, next) = normalized_node(paragraph, run);
                (previous, next)
            })
            .collect();
        assert_eq!(
            links,
            [
                (None, None),
                (None, Some(2)),
                (Some(1), None),
                (None, None),
                (None, None)
            ]
        );
        let color = |run: &Run| run.node.foreground_color().unwrap();
        assert_eq!(
            color(&runs[1]),
            accesskit::Color {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 255
            }
        );
        assert_eq!(color(&runs[2]), color(&runs[0]));
        assert_ne!(color(&runs[2]), color(&runs[1]));

        for byte in [0, 1, 4, 5, 6, 9, 10, 13] {
            let position = Position::new(0, byte);
            assert_eq!(
                snapshot.decode(snapshot.position(position).unwrap()),
                Some(position)
            );
        }
        let after = Position::new(0, 4);
        assert_eq!(
            snapshot.decode(accesskit::TextPosition {
                node: runs[0].id.accesskit_id(),
                character_index: 2,
            }),
            Some(after)
        );
        assert_eq!(
            snapshot.position(after).unwrap().node,
            runs[1].id.accesskit_id()
        );
        assert_eq!(
            snapshot.decode_focus(accesskit::TextPosition {
                node: runs[0].id.accesskit_id(),
                character_index: 2,
            }),
            Some((after, true))
        );
        for prefer_next_row in [false, true] {
            assert_eq!(
                snapshot.focus_position(after, prefer_next_row),
                snapshot.position(after)
            );
        }
        let geometry = &runs[0].geometry;
        let widths = geometry.widths.as_ref().unwrap();
        assert_eq!(geometry.positions.as_ref().unwrap().len(), 2);
        assert_eq!(widths[1], 0.0);
        let first_row = &galley.rows[0];
        assert!(geometry.bounds.max.y <= first_row.max_y());
        assert!(
            (geometry.positions.as_ref().unwrap()[1] + geometry.bounds.min.x
                - first_row.rect().right())
            .abs()
                < 0.01
        );
        assert_matches_fresh(&snapshot, &editor, &layouts, Id::unique("editor"));

        let context = egui::Context::default();
        let mut cache = super::super::Cache::default();
        let widget_id = Id::unique("editor");
        let mut published = None;
        for edit in 0..3 {
            match edit {
                1 => {
                    select(&mut editor, (0, 5), (0, 5));
                    editor.insert_line_break().unwrap();
                }
                2 => editor.delete_backward().unwrap(),
                _ => {}
            }
            let layouts = cached_layout_frame(&context, &mut cache, &editor, 500.0);
            update_cached(&editor, &layouts, widget_id, &mut published).unwrap();
            assert_matches_fresh(published.as_deref().unwrap(), &editor, &layouts, widget_id);
        }
    }

    #[test]
    fn soft_line_breaks_are_lines_for_platform_adapters() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let id = Id::unique("editor");
        let mut editor = Editor::from_text("one\u{2028}two\u{2028}\nthree");
        select(&mut editor, (0, 6), (0, 6));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 400.0))),
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| {
            super::super::RichTextEditor::new(&mut editor)
                .id(id)
                .show(ui);
        });
        let update = output.platform_output.accesskit_update.take().unwrap();
        output.drop_without_applying_deltas();
        let tree = accesskit_consumer::Tree::new(update, true);
        let node = tree
            .state()
            .node_by_tree_local_id(id.accesskit_id(), accesskit::TreeId::ROOT)
            .unwrap();
        assert_eq!(node.document_range().text(), editor.document().plain_text());
        let lines: Vec<_> = (0..4)
            .map(|line| node.line_range_from_index(line).unwrap().text())
            .collect();
        assert_eq!(lines, ["one\n", "two\n", "\n", "three"]);
        let caret = node.text_selection_focus().unwrap();
        assert_eq!(caret.to_global_usv_index(), 4);
        assert_eq!(caret.to_line_index(), 1);
        assert_eq!(caret.forward_to_line_end().to_global_usv_index(), 8);
    }

    #[test]
    fn consecutive_quotes_publish_one_blockquote_that_navigation_passes_through() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let id = Id::unique("editor");
        let mut editor = Editor::from_text("intro\none two\nthree\nbody\nlast");
        for (start, end) in [((1, 0), (2, 1)), ((4, 0), (4, 0))] {
            select(&mut editor, start, end);
            editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
        }
        select(&mut editor, (2, 2), (2, 2));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 400.0))),
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| {
            super::super::RichTextEditor::new(&mut editor)
                .id(id)
                .show(ui);
        });
        let update = output.platform_output.accesskit_update.take().unwrap();
        output.drop_without_applying_deltas();
        let nodes: HashMap<_, _> = update.nodes.iter().cloned().collect();
        let cache = context
            .data(|data| data.get_temp::<super::super::Cache>(id))
            .unwrap();
        let snapshot = cache.accessibility.unwrap();
        let paragraph = |index: usize| snapshot.paragraphs[index].id.accesskit_id();
        let children = nodes[&id.accesskit_id()].children();
        assert_eq!(children.len(), 4);
        assert_eq!([children[0], children[2]], [paragraph(0), paragraph(3)]);
        for (container, members) in [(children[1], vec![1, 2]), (children[3], vec![4])] {
            let node = &nodes[&container];
            assert_eq!(node.role(), accesskit::Role::Blockquote);
            let members: Vec<_> = members.into_iter().map(paragraph).collect();
            assert_eq!(node.children(), members);
            let bounds = node.bounds().unwrap();
            for member in members {
                let member = &nodes[&member];
                assert_eq!(member.role(), accesskit::Role::Paragraph);
                let inner = member.bounds().unwrap();
                assert!(bounds.x0 < inner.x0 && bounds.y0 <= inner.y0);
                assert!(bounds.x1 >= inner.x1 && bounds.y1 >= inner.y1);
            }
        }

        let tree = accesskit_consumer::Tree::new(update, true);
        let node = tree
            .state()
            .node_by_tree_local_id(id.accesskit_id(), accesskit::TreeId::ROOT)
            .unwrap();
        assert_eq!(node.document_range().text(), editor.document().plain_text());
        let lines: Vec<_> = (0..5)
            .map(|line| node.line_range_from_index(line).unwrap().text())
            .collect();
        assert_eq!(lines, ["intro\n", "one two\n", "three\n", "body\n", "last"]);
        let caret = node.text_selection_focus().unwrap();
        assert_eq!(caret.to_global_usv_index(), 16);
        let next_word = caret.forward_to_word_start();
        assert_eq!(next_word.to_global_usv_index(), 20);
        assert_eq!(
            snapshot.decode(next_word.to_raw()),
            Some(Position::new(3, 0))
        );
        let start = caret.backward_to_paragraph_start();
        assert_eq!(snapshot.decode(start.to_raw()), Some(Position::new(2, 0)));
    }

    #[test]
    fn soft_wrap_caret_affinity_selects_the_upper_row_run() {
        let text = "first second third fourth fifth sixth";
        let mut editor = Editor::from_text(text);
        let (layouts, snapshot) = snapshot_for(&editor, 65.0);
        let runs = &snapshot.paragraphs[0].runs;
        assert_ne!(runs[0].row, runs[1].row);
        let seam = Position::new(0, runs[1].range.start);
        let upper = accesskit::TextPosition {
            node: runs[0].id.accesskit_id(),
            character_index: runs[0].node.character_lengths().len(),
        };
        let lower = accesskit::TextPosition {
            node: runs[1].id.accesskit_id(),
            character_index: 0,
        };
        assert_eq!(snapshot.position(seam), Some(lower));
        assert_eq!(snapshot.focus_position(seam, true), Some(lower));
        assert_eq!(snapshot.focus_position(seam, false), Some(upper));
        assert_eq!(snapshot.decode(upper), Some(seam));
        assert_eq!(snapshot.decode_focus(upper), Some((seam, false)));
        assert_eq!(snapshot.decode_focus(lower), Some((seam, true)));
        for position in [Position::new(0, 1), Position::new(0, text.len())] {
            assert_eq!(
                snapshot.focus_position(position, false),
                snapshot.position(position)
            );
        }

        editor
            .set_selection(crate::Selection::new(Position::default(), seam))
            .unwrap();
        for (prefer_next_row, focus) in [(true, lower), (false, upper)] {
            let nodes = published_nodes(&editor, &snapshot, &layouts, prefer_next_row);
            let selection = nodes[&Id::unique("editor").accesskit_id()]
                .text_selection()
                .unwrap();
            assert_eq!(selection.focus, focus);
            assert_eq!(
                selection.anchor,
                snapshot.position(Position::default()).unwrap()
            );
        }
    }

    #[test]
    fn collapsed_caret_at_a_wrap_seam_publishes_one_position() {
        let mut editor = Editor::from_text("first second third fourth fifth sixth");
        let (layouts, snapshot) = snapshot_for(&editor, 65.0);
        let runs = &snapshot.paragraphs[0].runs;
        assert_ne!(runs[0].row, runs[1].row);
        let seam = Position::new(0, runs[1].range.start);
        editor.set_selection(crate::Selection::caret(seam)).unwrap();
        for prefer_next_row in [true, false] {
            let nodes = published_nodes(&editor, &snapshot, &layouts, prefer_next_row);
            let selection = nodes[&Id::unique("editor").accesskit_id()]
                .text_selection()
                .unwrap();
            let caret = snapshot.focus_position(seam, prefer_next_row).unwrap();
            assert_eq!((selection.anchor, selection.focus), (caret, caret));
        }
    }

    #[test]
    fn clicking_a_wrap_seam_publishes_the_caret_on_the_clicked_row() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let id = Id::unique("wrap seam");
        let mut editor = Editor::from_text("first second third fourth fifth sixth seventh");
        let mut run_frame = |events| {
            let mut rect = Rect::NOTHING;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::Vec2::splat(400.0))),
                events,
                ..Default::default()
            };
            let mut output = context.run_ui(input, |ui| {
                let output = super::super::RichTextEditor::new(&mut editor)
                    .id(id)
                    .desired_width(120.0)
                    .min_rows(1)
                    .show(ui);
                assert!(output.errors.is_empty());
                assert!(output.accessibility_errors.is_empty());
                rect = output.response.rect;
            });
            let update = output.platform_output.accesskit_update.take();
            output.drop_without_applying_deltas();
            (rect, update)
        };
        let (rect, _) = run_frame(vec![]);
        let cache = context.data(|data| data.get_temp::<super::super::Cache>(id));
        let layout = cache.unwrap().layout.layouts.get(0).unwrap();
        let row = &layout.galley.rows[0];
        let pointer = rect.min
            + egui::Vec2::splat(super::super::PADDING)
            + layout.rect.min.to_vec2()
            + egui::vec2(row.rect().right() + 1.0, row.rect().center().y);
        let click = |pressed| egui::Event::PointerButton {
            pos: pointer,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        run_frame(vec![egui::Event::PointerMoved(pointer), click(true)]);
        let (_, update) = run_frame(vec![click(false)]);
        let nodes: HashMap<_, _> = update.unwrap().nodes.into_iter().collect();
        let focus = nodes[&id.accesskit_id()].text_selection().unwrap().focus;
        let cache = context.data(|data| data.get_temp::<super::super::Cache>(id));
        let snapshot = cache.unwrap().accessibility.unwrap();
        let first_row = &snapshot.paragraphs[0].runs[0];
        assert_eq!(focus.node, first_row.id.accesskit_id());
        assert_eq!(
            focus.character_index,
            first_row.node.character_lengths().len()
        );
        assert_eq!(snapshot.decode(focus), Some(editor.selection().focus));
    }

    #[test]
    fn list_items_publish_their_position_and_set_size() {
        let mut editor = Editor::from_text("a\nb\nc\nbody\nd\ne\nf\ng");
        for (range, kind) in [
            ((0, 2), ParagraphKind::Bullet { indent: 0 }),
            ((4, 5), ParagraphKind::Bullet { indent: 0 }),
            (
                (6, 6),
                ParagraphKind::Ordered {
                    indent: 0,
                    start: Some(4),
                },
            ),
            (
                (7, 7),
                ParagraphKind::Ordered {
                    indent: 1,
                    start: None,
                },
            ),
        ] {
            select(&mut editor, (range.0, 0), (range.1, 1));
            editor.set_paragraph_kind(kind).unwrap();
        }
        let (layouts, snapshot) = snapshot_for(&editor, 500.0);
        let nodes = published_nodes(&editor, &snapshot, &layouts, true);
        let sets: Vec<_> = snapshot
            .paragraphs
            .iter()
            .map(|paragraph| {
                let node = &nodes[&paragraph.id.accesskit_id()];
                (node.position_in_set(), node.size_of_set())
            })
            .collect();
        assert_eq!(
            sets,
            [
                (Some(1), Some(5)),
                (Some(2), Some(5)),
                (Some(3), Some(5)),
                (None, None),
                (Some(4), Some(5)),
                (Some(5), Some(5)),
                (Some(1), Some(1)),
                (Some(1), Some(1)),
            ]
        );
    }

    #[test]
    fn nested_items_do_not_split_their_parent_list_set() {
        let ordered = |indent| ParagraphKind::Ordered {
            indent,
            start: None,
        };
        let bullet = |indent| ParagraphKind::Bullet { indent };
        let positions = |kinds: &[ParagraphKind]| list_positions(kinds.iter().copied());
        assert_eq!(
            positions(&[ordered(0), ordered(1), ordered(0)]),
            [Some((1, 2)), Some((1, 1)), Some((2, 2))]
        );
        assert_eq!(
            positions(&[
                ordered(0),
                ordered(2),
                ordered(1),
                ordered(1),
                ordered(0),
                ordered(1)
            ]),
            [
                Some((1, 2)),
                Some((1, 1)),
                Some((1, 2)),
                Some((2, 2)),
                Some((2, 2)),
                Some((1, 1)),
            ]
        );
        assert_eq!(
            positions(&[bullet(0), ordered(1), bullet(0), ordered(0), bullet(0)]),
            [
                Some((1, 3)),
                Some((1, 1)),
                Some((2, 3)),
                Some((1, 1)),
                Some((3, 3)),
            ]
        );
        let restart = ParagraphKind::Ordered {
            indent: 0,
            start: Some(5),
        };
        assert_eq!(
            positions(&[
                ordered(0),
                ParagraphKind::Body,
                ordered(0),
                restart,
                ordered(0)
            ]),
            [Some((1, 2)), None, Some((2, 2)), Some((1, 2)), Some((2, 2))]
        );
        assert_eq!(
            positions(&[
                bullet(0),
                bullet(1),
                ParagraphKind::Heading { level: 2 },
                bullet(1),
                ParagraphKind::Body,
                bullet(0),
            ]),
            [
                Some((1, 1)),
                Some((1, 1)),
                None,
                Some((1, 1)),
                None,
                Some((1, 1)),
            ]
        );

        let mut editor = Editor::from_text("a\nb\nc");
        select(&mut editor, (0, 0), (2, 1));
        editor.set_paragraph_kind(ordered(0)).unwrap();
        select(&mut editor, (1, 0), (1, 1));
        editor.indent_list().unwrap();
        let (layouts, snapshot) = snapshot_for(&editor, 500.0);
        let nodes = published_nodes(&editor, &snapshot, &layouts, true);
        let sets: Vec<_> = snapshot
            .paragraphs
            .iter()
            .map(|paragraph| {
                let node = &nodes[&paragraph.id.accesskit_id()];
                (node.position_in_set(), node.size_of_set())
            })
            .collect();
        assert_eq!(
            sets,
            [(Some(1), Some(2)), (Some(1), Some(1)), (Some(2), Some(2))]
        );
    }

    #[test]
    fn filled_runs_publish_their_background_and_split_where_the_fill_changes() {
        let mut editor = Editor::from_text("no fill yellow code");
        select(&mut editor, (0, 8), (0, 19));
        editor
            .apply_style(crate::StylePatch {
                background: Some(Some(crate::Color::YELLOW)),
                ..Default::default()
            })
            .unwrap();
        let fill = crate::Color([0, 112, 192, 128]);
        select(&mut editor, (0, 15), (0, 19));
        editor
            .apply_style(crate::StylePatch {
                code: Some(true),
                background: Some(Some(fill)),
                ..Default::default()
            })
            .unwrap();
        let (layouts, snapshot) = snapshot_for(&editor, 500.0);
        let nodes = published_nodes(&editor, &snapshot, &layouts, true);
        let runs: Vec<_> = snapshot.paragraphs[0]
            .runs
            .iter()
            .map(|run| {
                let node = &nodes[&run.id.accesskit_id()];
                (run.range.clone(), node.background_color())
            })
            .collect();
        let color = |crate::Color([red, green, blue, alpha])| accesskit::Color {
            red,
            green,
            blue,
            alpha,
        };
        assert_eq!(
            runs,
            [
                (0..8, None),
                (8..15, Some(color(crate::Color::YELLOW))),
                (15..19, Some(color(fill))),
            ]
        );
    }

    #[test]
    fn formatted_runs_keep_their_colors_and_fonts_across_wraps_and_soft_breaks() {
        let colors = [
            Some(crate::Color([255, 0, 0, 255])),
            None,
            Some(crate::Color([0, 0, 255, 255])),
        ];
        let text = "e\u{301}猫\u{2028}".repeat(64);
        let spans = text
            .grapheme_indices(true)
            .enumerate()
            .map(|(index, (byte, grapheme))| Span {
                range: byte..byte + grapheme.len(),
                style: InlineStyle {
                    foreground: colors[index % colors.len()],
                    code: index % 2 == 0,
                    ..Default::default()
                },
                link: None,
            })
            .collect();
        let paragraph = Paragraph::new(&text, spans, ParagraphKind::Heading { level: 2 }).unwrap();
        let editor = Editor::new(crate::Document::from_paragraphs([paragraph]));
        for width in [8.0, 500.0] {
            let (_, snapshot) = snapshot_for(&editor, width);
            let paragraph = &snapshot.paragraphs[0];
            assert!(paragraph.runs.last().unwrap().range.is_empty());
            for run in paragraph.runs.iter() {
                let style = paragraph
                    .source
                    .spans()
                    .iter()
                    .find(|span| span.range.contains(&run.range.start))
                    .unwrap_or_else(|| paragraph.source.spans().last().unwrap())
                    .style;
                let crate::Color([red, green, blue, alpha]) =
                    style.foreground.unwrap_or(crate::Color([255; 4]));
                assert_eq!(
                    run.node.foreground_color(),
                    Some(accesskit::Color {
                        red,
                        green,
                        blue,
                        alpha,
                    }),
                    "width {width}, range {:?}",
                    run.range,
                );
                assert_eq!(run.node.font_size(), Some(24.0));
                assert_eq!(run.node.font_family(), style.code.then_some("monospace"));
            }
        }
    }

    #[test]
    fn links_publish_link_nodes_around_their_runs_for_platform_adapters() {
        let docs = Link::new("https://example.com/docs").unwrap();
        let mut editor = Editor::from_text("Read the docs\u{2028}now, then stop\nnext line");
        select(&mut editor, (0, 5), (0, 19));
        editor.set_link(Some(docs.clone())).unwrap();
        select(&mut editor, (0, 9), (0, 13));
        editor
            .apply_style(crate::StylePatch {
                bold: Some(true),
                ..Default::default()
            })
            .unwrap();
        select(&mut editor, (1, 5), (1, 9));
        editor
            .set_link(Some(Link::new("https://b.example").unwrap()))
            .unwrap();
        select(&mut editor, (0, 11), (0, 11));
        let (layouts, snapshot) = snapshot_for(&editor, 500.0);
        let nodes = published_nodes(&editor, &snapshot, &layouts, true);
        let paragraph = &snapshot.paragraphs[0];
        let link = &paragraph.links[0];
        assert_eq!(
            paragraph.runs[link.runs.clone()]
                .iter()
                .map(|run| run.range.clone())
                .collect::<Vec<_>>(),
            [5..9, 9..13, 13..16, 16..19]
        );
        let node = &nodes[&link.id.accesskit_id()];
        assert_eq!(node.role(), accesskit::Role::Link);
        assert_eq!(node.url(), Some(docs.as_str()));
        assert_eq!(node.label(), Some("the docs now"));
        assert!(node.supports_action(accesskit::Action::Click));
        assert_eq!(
            node.children(),
            paragraph.runs[link.runs.clone()]
                .iter()
                .map(|run| run.id.accesskit_id())
                .collect::<Vec<_>>()
        );
        let bounds = node.bounds().unwrap();
        for run in &paragraph.runs[link.runs.clone()] {
            let run = nodes[&run.id.accesskit_id()].bounds().unwrap();
            assert!(bounds.x0 <= run.x0 && run.x1 <= bounds.x1);
            assert!(bounds.y0 <= run.y0 && run.y1 <= bounds.y1);
        }
        assert_eq!(
            nodes[&paragraph.id.accesskit_id()].children(),
            [
                paragraph.runs[0].id.accesskit_id(),
                link.id.accesskit_id(),
                paragraph.runs[5].id.accesskit_id(),
            ]
        );
        let linked = &nodes[&paragraph.runs[1].id.accesskit_id()];
        assert!(linked.underline().is_some());
        assert!(
            nodes[&paragraph.runs[0].id.accesskit_id()]
                .underline()
                .is_none()
        );
        assert_eq!(snapshot.link(link.id.accesskit_id()), Some(&docs));
        assert_eq!(snapshot.link(paragraph.runs[1].id.accesskit_id()), None);
        assert_eq!(
            snapshot.paragraphs[1].links[0].label, "line",
            "a link ending its paragraph excludes the separator"
        );

        let context = egui::Context::default();
        context.enable_accesskit();
        let id = Id::unique("editor");
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 400.0))),
                ..Default::default()
            },
            |ui| {
                super::super::RichTextEditor::new(&mut editor)
                    .id(id)
                    .show(ui);
            },
        );
        let update = output.platform_output.accesskit_update.take().unwrap();
        output.drop_without_applying_deltas();
        let tree = accesskit_consumer::Tree::new(update, true);
        let node = tree
            .state()
            .node_by_tree_local_id(id.accesskit_id(), accesskit::TreeId::ROOT)
            .unwrap();
        assert_eq!(node.document_range().text(), editor.document().plain_text());
        let lines: Vec<_> = (0..3)
            .map(|line| node.line_range_from_index(line).unwrap().text())
            .collect();
        assert_eq!(lines, ["Read the docs\n", "now, then stop\n", "next line"]);
        let caret = node.text_selection_focus().unwrap();
        assert_eq!(caret.to_global_usv_index(), 11);
        assert_eq!(caret.backward_to_word_start().to_global_usv_index(), 9);
        assert_eq!(caret.backward_to_line_start().to_global_usv_index(), 0);
        let link = node
            .children()
            .flat_map(|paragraph| paragraph.children())
            .find(|child| child.role() == accesskit::Role::Link)
            .unwrap();
        assert_eq!(link.url(), Some(docs.as_str()));
        assert!(link.is_clickable(&accesskit_consumer::common_filter));
    }
}
