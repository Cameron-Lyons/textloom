use super::PARAGRAPH_GAP;
use crate::{Document, InlineStyle, Paragraph, ParagraphKind, document::LINE_SEPARATOR};
use egui::{
    Color32, FontData, FontFamily, FontId, Galley, Pos2, Rect, Stroke, TextFormat, Ui,
    epaint::{FontsView, TextOptions},
    text::{ByteIndex, LayoutJob, LayoutSection},
};
use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
    sync::Arc,
};
mod index;
pub(super) use index::{LayoutIdentity, ParagraphLayouts};

const BULLET_GUTTER: f32 = 1.5;
const ORDERED_GUTTER: f32 = 2.0;
const LEVEL_STEP: f32 = 1.25;
const MARKER_GAP: f32 = 0.5;
const QUOTE_GUTTER: f32 = 1.25;
const QUOTE_BAR_X: f32 = 0.25;
const QUOTE_BAR_WIDTH: f32 = 0.2;
const RESERVED_ORDINAL: &str = "000.";

#[derive(Clone, PartialEq)]
pub(super) struct Appearance {
    pub(super) font: FontId,
    pub(super) bold_family: Option<FontFamily>,
    pub(super) color: Color32,
    pub(super) strong_color: Color32,
    pub(super) code_background: Color32,
    pub(super) link_color: Color32,
    pub(super) width: f32,
}

#[derive(Clone)]
pub(super) struct ParagraphCache {
    pub(super) paragraph: Arc<Paragraph>,
    pub(super) number: Option<u32>,
    pub(super) chars: usize,
    pub(super) layout: ParagraphLayout,
}

#[derive(Clone)]
pub(super) struct ParagraphLayout {
    pub(super) galley: Arc<Galley>,
    pub(super) rect: Rect,
    pub(super) marker: Option<Arc<Galley>>,
    pub(super) marker_position: Pos2,
    pub(super) quote_bar: Option<Rect>,
}

pub(crate) struct ParagraphLayoutView<'a> {
    pub(crate) galley: &'a Arc<Galley>,
    pub(crate) rect: Rect,
    pub(crate) marker: Option<&'a Arc<Galley>>,
    pub(crate) marker_position: Pos2,
    pub(crate) quote_bar: Option<Rect>,
}

#[derive(Clone)]
struct FontEnvironment {
    pixels_per_point: f32,
    options: TextOptions,
    families: BTreeMap<FontFamily, Vec<String>>,
    font_data: Vec<(String, Arc<FontData>)>,
}

impl FontEnvironment {
    fn capture(pixels_per_point: f32, fonts: &FontsView<'_>) -> Self {
        let definitions = fonts.definitions();
        Self {
            pixels_per_point,
            options: *fonts.options(),
            families: definitions.families.clone(),
            font_data: definitions
                .font_data
                .iter()
                .map(|(name, data)| (name.clone(), Arc::clone(data)))
                .collect(),
        }
    }

    fn matches(&self, pixels_per_point: f32, fonts: &FontsView<'_>) -> bool {
        let definitions = fonts.definitions();
        self.pixels_per_point == pixels_per_point
            && self.options == *fonts.options()
            && self.families == definitions.families
            && self.font_data.len() == definitions.font_data.len()
            && self.font_data.iter().zip(&definitions.font_data).all(
                |((name, data), (current_name, current))| {
                    name == current_name && Arc::ptr_eq(data, current)
                },
            )
    }
}

struct Families {
    bold: Option<FontFamily>,
    code: FontFamily,
}

impl Families {
    fn resolve(appearance: &Appearance, fonts: &FontsView<'_>) -> Self {
        let registered = &fonts.definitions().families;
        Self {
            bold: appearance
                .bold_family
                .clone()
                .filter(|family| registered.contains_key(family)),
            code: if registered.contains_key(&FontFamily::Monospace) {
                FontFamily::Monospace
            } else {
                appearance.font.family.clone()
            },
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct LayoutCache {
    pub(super) appearance: Option<Appearance>,
    font_environment: Option<FontEnvironment>,
    pub(super) document_identity: Option<Arc<()>>,
    pending: Option<PendingChange>,
    pub(super) layouts: ParagraphLayouts,
    #[cfg(test)]
    pub(super) scans: usize,
}

#[derive(Clone)]
struct PendingChange {
    identity: Arc<()>,
    paragraphs: Option<(Range<usize>, usize)>,
}

impl LayoutCache {
    // Call after each edit so that one later update knows which paragraphs all of them changed.
    pub(super) fn track(&mut self, document: &Document) {
        let Some(cached) = &self.document_identity else {
            return;
        };
        let identity = document.content_identity();
        let base = self
            .pending
            .as_ref()
            .map_or(cached, |pending| &pending.identity);
        if Arc::ptr_eq(base, &identity) {
            return;
        }
        let step = document
            .change_since(base)
            .map(|change| (change.range, change.new_len));
        let paragraphs = match self.pending.take() {
            None => step,
            Some(pending) => pending.paragraphs.zip(step).map(merge_changes),
        };
        self.pending = Some(PendingChange {
            identity,
            paragraphs,
        });
    }

    pub(super) fn layout_document(
        &mut self,
        ui: &Ui,
        document: &Document,
        appearance: &Appearance,
    ) -> ParagraphLayouts {
        self.update(ui, document, appearance).0
    }

    pub(super) fn update(
        &mut self,
        ui: &Ui,
        document: &Document,
        appearance: &Appearance,
    ) -> (ParagraphLayouts, bool, bool) {
        let mut layout_changed = false;
        let appearance_changed = self.appearance.as_ref() != Some(appearance);
        let document_identity = document.content_identity();
        let document_changed = self
            .document_identity
            .as_ref()
            .is_none_or(|cached| !Arc::ptr_eq(cached, &document_identity));
        self.track(document);
        let change = self.pending.take().and_then(|pending| pending.paragraphs);
        let pixels_per_point = ui.ctx().pixels_per_point();
        ui.fonts_mut(|fonts| {
            let environment_changed = self
                .font_environment
                .as_ref()
                .is_none_or(|environment| !environment.matches(pixels_per_point, fonts));
            if environment_changed {
                self.font_environment = Some(FontEnvironment::capture(pixels_per_point, fonts));
            }
            if !appearance_changed && !document_changed && !environment_changed {
                return;
            }
            layout_changed = true;
            let mut shaper = Shaper {
                families: Families::resolve(appearance, fonts),
                fonts,
                appearance,
                reserved_ordinal: None,
            };

            if appearance_changed || environment_changed {
                let caches: Vec<_> = document
                    .paragraphs()
                    .iter()
                    .enumerate()
                    .map(|(index, paragraph)| {
                        shaper.create(paragraph, document.list_number(index), None)
                    })
                    .collect();
                self.layouts.rebuild(&caches);
            } else {
                let source = document.paragraphs();
                let (old, new) = if let Some((old, new_len)) = change {
                    let new = old.start..old.start + new_len;
                    (old, new)
                } else {
                    #[cfg(test)]
                    {
                        self.scans += 1;
                    }
                    let (prefix, suffix) = self.layouts.shared_edges(source);
                    (
                        prefix..self.layouts.len() - suffix,
                        prefix..source.len() - suffix,
                    )
                };
                let previous: Vec<_> = self.layouts.caches(old.clone()).collect();
                let renumbered = if old.len() == new.len() {
                    previous
                        .iter()
                        .zip(&source[new.clone()])
                        .any(|(cache, paragraph)| {
                            !crate::document::same_numbering_effect(
                                cache.paragraph.kind(),
                                paragraph.kind(),
                            )
                        })
                } else {
                    previous
                        .iter()
                        .map(|cache| cache.paragraph.kind())
                        .chain(source[new.clone()].iter().map(|paragraph| paragraph.kind()))
                        .any(|kind| {
                            !crate::document::same_numbering_effect(kind, ParagraphKind::Body)
                        })
                };
                let replacement: Vec<_> = if old.len() == new.len() {
                    previous
                        .iter()
                        .zip(&source[new.clone()])
                        .zip(new.clone())
                        .map(|((cache, paragraph), index)| {
                            let number = document.list_number(index);
                            if Arc::ptr_eq(&cache.paragraph, paragraph) && cache.number == number {
                                (*cache).clone()
                            } else {
                                shaper.create(paragraph, number, Some(cache))
                            }
                        })
                        .collect()
                } else {
                    let by_paragraph: HashMap<_, _> = previous
                        .iter()
                        .map(|&cache| (Arc::as_ptr(&cache.paragraph), cache))
                        .collect();
                    let by_content: HashMap<_, _> = previous
                        .iter()
                        .map(|&cache| (content_key(&cache.paragraph), cache))
                        .collect();
                    source[new.clone()]
                        .iter()
                        .zip(new.clone())
                        .map(|(paragraph, index)| {
                            let number = document.list_number(index);
                            match by_paragraph.get(&Arc::as_ptr(paragraph)) {
                                Some(cache) if cache.number == number => (*cache).clone(),
                                Some(cache) => shaper.create(paragraph, number, Some(cache)),
                                None => shaper.create(
                                    paragraph,
                                    number,
                                    by_content.get(&content_key(paragraph)).copied(),
                                ),
                            }
                        })
                        .collect()
                };
                self.layouts.replace(old, &replacement);
                if renumbered {
                    let items = document.numbered_items_from(new.end);
                    let visited = items
                        .iter()
                        .position(|&(index, _)| {
                            matches!(
                                source[index].kind(),
                                ParagraphKind::Ordered {
                                    indent: 0,
                                    start: Some(_)
                                }
                            )
                        })
                        .map_or(items.len(), |position| position + 1);
                    self.layouts
                        .renumber(&items[..visited], |index, cache, number| {
                            shaper.create(&source[index], Some(number), Some(cache))
                        });
                }
            }
            self.appearance = Some(appearance.clone());
            self.document_identity = Some(document_identity);
        });
        (self.layouts.clone(), document_changed, layout_changed)
    }
}

// A change is a replaced range and its new length; `second` changes the result of `first`.
fn merge_changes(
    ((first, first_len), (second, second_len)): ((Range<usize>, usize), (Range<usize>, usize)),
) -> (Range<usize>, usize) {
    let start = first.start.min(second.start);
    let end = (first.start + first_len).max(second.end);
    let old_end = end - first_len + first.len();
    let new_end = end - second.len() + second_len;
    (start..old_end, new_end - start)
}

fn content_key(paragraph: &Paragraph) -> (*const u8, *const crate::Span) {
    (paragraph.text().as_ptr(), paragraph.spans().as_ptr())
}

struct Shaper<'a, 'f> {
    fonts: &'a mut FontsView<'f>,
    appearance: &'a Appearance,
    families: Families,
    reserved_ordinal: Option<f32>,
}

impl Shaper<'_, '_> {
    fn create(
        &mut self,
        paragraph: &Arc<Paragraph>,
        number: Option<u32>,
        previous: Option<&ParagraphCache>,
    ) -> ParagraphCache {
        let appearance = self.appearance;
        let em = appearance.font.size;
        let (level, gutter, marker) = match paragraph.kind() {
            ParagraphKind::Bullet { indent } => (indent, BULLET_GUTTER, Some("•".to_owned())),
            ParagraphKind::Ordered { indent, .. } => (
                indent,
                ORDERED_GUTTER,
                Some(format!("{}.", number.unwrap_or(1))),
            ),
            _ => (0, 0.0, None),
        };
        let marker = marker.map(|text| self.single_line(text));
        let max_indent = (appearance.width - em * 2.0).max(0.0);
        let (indent, marker_x) = if let Some(marker) = &marker {
            let width = marker.size().x;
            let reserved = if matches!(paragraph.kind(), ParagraphKind::Ordered { .. }) {
                width.max(self.reserved_ordinal())
            } else {
                width
            };
            let indent = (f32::from(level) * LEVEL_STEP * em
                + (gutter * em).max(reserved + MARKER_GAP * em))
            .ceil()
            .min(max_indent);
            (indent, indent - width - MARKER_GAP * em)
        } else if paragraph.kind() == ParagraphKind::Quote {
            ((QUOTE_GUTTER * em).ceil().min(max_indent), 0.0)
        } else {
            (0.0, 0.0)
        };
        let scale = size_scale(paragraph.kind());
        let (galley, chars) = if let Some(previous) = previous
            && previous.layout.rect.left() == indent
            && size_scale(previous.paragraph.kind()) == scale
            && std::ptr::eq(previous.paragraph.text(), paragraph.text())
            && std::ptr::eq(previous.paragraph.spans(), paragraph.spans())
        {
            (Arc::clone(&previous.layout.galley), previous.chars)
        } else {
            let job = paragraph_job_with(
                paragraph,
                appearance,
                &self.families,
                scale,
                (appearance.width - indent).max(8.0),
            );
            let chars = paragraph.text().chars().count();
            let mut galley = self.fonts.layout_job(job);
            if galley.end().index.0 != chars {
                galley = self.fonts.layout_job(scalar_layout_job(&galley.job));
            }
            (galley, chars)
        };
        ParagraphCache {
            paragraph: Arc::clone(paragraph),
            number,
            chars,
            layout: ParagraphLayout {
                rect: Rect::from_min_size(Pos2::new(indent, 0.0), galley.size()),
                marker_position: Pos2::new(marker_x, 0.0),
                quote_bar: quote_bar(paragraph.kind(), em, indent, &galley),
                galley,
                marker,
            },
        }
    }

    fn single_line(&mut self, text: String) -> Arc<Galley> {
        self.fonts.layout_job(LayoutJob::simple_singleline(
            text,
            self.appearance.font.clone(),
            self.appearance.color,
        ))
    }

    fn reserved_ordinal(&mut self) -> f32 {
        if let Some(width) = self.reserved_ordinal {
            return width;
        }
        let width = self.single_line(RESERVED_ORDINAL.to_owned()).size().x;
        *self.reserved_ordinal.insert(width)
    }
}

fn quote_bar(kind: ParagraphKind, em: f32, indent: f32, galley: &Galley) -> Option<Rect> {
    if kind != ParagraphKind::Quote {
        return None;
    }
    let scale = (indent / (QUOTE_GUTTER * em)).min(1.0);
    let left = (QUOTE_BAR_X * em * scale).round();
    let width = (QUOTE_BAR_WIDTH * em * scale).round().max(1.0);
    (left + width < indent)
        .then(|| Rect::from_min_size(Pos2::new(left, 0.0), egui::vec2(width, galley.size().y)))
}

#[cfg(test)]
pub(super) fn paragraph_job(
    paragraph: &Paragraph,
    appearance: &Appearance,
    width: f32,
) -> LayoutJob {
    let families = Families {
        bold: appearance.bold_family.clone(),
        code: FontFamily::Monospace,
    };
    paragraph_job_with(
        paragraph,
        appearance,
        &families,
        size_scale(paragraph.kind()),
        width,
    )
}

fn size_scale(kind: ParagraphKind) -> f32 {
    match kind {
        ParagraphKind::Heading { level } => match level {
            1 => 1.8,
            2 => 1.5,
            3 => 1.25,
            _ => 1.1,
        },
        _ => 1.0,
    }
}

fn paragraph_job_with(
    paragraph: &Paragraph,
    appearance: &Appearance,
    families: &Families,
    size_scale: f32,
    width: f32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    job.keep_trailing_whitespace = true;
    for span in paragraph.spans() {
        let text = &paragraph.text()[span.range.clone()];
        let format = text_format(
            span.style,
            span.link.is_some(),
            appearance,
            families,
            size_scale,
        );
        if text.contains(LINE_SEPARATOR) {
            job.append(&text.replace(LINE_SEPARATOR, "\n"), 0.0, format);
        } else {
            job.append(text, 0.0, format);
        }
    }
    if job.sections.is_empty() {
        job.append(
            "",
            0.0,
            text_format(
                InlineStyle::default(),
                false,
                appearance,
                families,
                size_scale,
            ),
        );
    }
    job
}

pub(super) fn is_layout_text(layout: &str, text: &str) -> bool {
    let mut lines = text.split(LINE_SEPARATOR);
    let Some(mut rest) = layout.strip_prefix(lines.next().unwrap_or_default()) else {
        return false;
    };
    for line in lines {
        match rest
            .strip_prefix('\n')
            .and_then(|rest| rest.strip_prefix(line))
        {
            Some(remaining) => rest = remaining,
            None => return false,
        }
    }
    rest.is_empty()
}

pub(super) fn scalar_layout_job(source: &LayoutJob) -> LayoutJob {
    let mut job = source.clone();
    job.sections.clear();
    for section in &source.sections {
        let start = section.byte_range.start.0;
        let end = section.byte_range.end.0;
        for (offset, scalar) in source.text[start..end].char_indices() {
            job.sections.push(LayoutSection {
                leading_space: if offset == 0 {
                    section.leading_space
                } else {
                    0.0
                },
                byte_range: ByteIndex(start + offset)
                    ..ByteIndex(start + offset + scalar.len_utf8()),
                format: section.format.clone(),
            });
        }
    }
    job
}

fn text_format(
    style: InlineStyle,
    linked: bool,
    appearance: &Appearance,
    families: &Families,
    size_scale: f32,
) -> TextFormat {
    let mut font = appearance.font.clone();
    font.size *= size_scale;
    if style.code {
        font.family = families.code.clone();
    } else if style.bold
        && let Some(family) = &families.bold
    {
        font.family = family.clone();
    }
    let color = style.foreground.map_or_else(
        || {
            if linked {
                appearance.link_color
            } else if style.bold {
                appearance.strong_color
            } else {
                appearance.color
            }
        },
        |color| Color32::from_rgba_unmultiplied(color.0[0], color.0[1], color.0[2], color.0[3]),
    );
    let mut format = TextFormat {
        font_id: font,
        color,
        italics: style.italic,
        underline: if style.underline || linked {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        strikethrough: if style.strikethrough {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        background: if style.code {
            appearance.code_background
        } else {
            Color32::TRANSPARENT
        },
        ..Default::default()
    };
    if let Some(crate::Color([red, green, blue, alpha])) = style.background {
        format.background = Color32::from_rgba_unmultiplied(red, green, blue, alpha);
        format.expand_bg = 0.0;
    }
    format
}

#[cfg(test)]
mod tests {
    use super::super::{Cache, RichTextEditor};
    use super::*;
    use crate::{Editor, Position, Selection, StylePatch};
    use egui::{Event, Key, Modifiers, Vec2, epaint::Shape};

    fn appearance(size: f32) -> Appearance {
        Appearance {
            font: FontId::proportional(size),
            bold_family: None,
            color: Color32::WHITE,
            strong_color: Color32::WHITE,
            code_background: Color32::BLACK,
            link_color: Color32::LIGHT_BLUE,
            width: 400.0,
        }
    }

    fn layout_frame(
        context: &egui::Context,
        cache: &mut LayoutCache,
        document: &Document,
        appearance: &Appearance,
    ) -> (ParagraphLayouts, bool) {
        let mut result = None;
        context
            .run_ui(egui::RawInput::default(), |ui| {
                let (layouts, _, changed) = cache.update(ui, document, appearance);
                result = Some((layouts, changed));
            })
            .drop_without_applying_deltas();
        result.unwrap()
    }

    fn unrelated_pass(
        context: &egui::Context,
        viewport: egui::ViewportId,
        add_contents: impl FnMut(&mut Ui),
    ) {
        let mut input = egui::RawInput {
            viewport_id: viewport,
            ..Default::default()
        };
        input
            .viewports
            .insert(viewport, egui::ViewportInfo::default());
        context
            .run_ui(input, add_contents)
            .drop_without_applying_deltas();
    }

    fn other_viewport_pass(context: &egui::Context) {
        unrelated_pass(context, egui::ViewportId::from_hash_of("other"), |ui| {
            ui.label("other viewport");
        });
    }

    fn hidden_editor_pass(context: &egui::Context) {
        unrelated_pass(context, egui::ViewportId::ROOT, |ui| {
            ui.label("editor hidden");
        });
    }

    fn widget_frame(
        context: &egui::Context,
        editor: &mut Editor,
        events: Vec<Event>,
        mut before_editor: impl FnMut(&mut Ui),
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
            events,
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                before_editor(ui);
                let id = ui.make_persistent_id("editor");
                ui.memory_mut(|memory| {
                    if !memory.has_focus(id) {
                        memory.request_focus(id);
                    }
                });
                let output = RichTextEditor::new(editor).id(id).show(ui);
                assert!(output.errors.is_empty(), "{:?}", output.errors);
            });
        });
        output.textures_delta.clear();
        output
    }

    fn select(editor: &mut Editor, anchor: (usize, usize), focus: (usize, usize)) {
        editor
            .set_selection(Selection::new(
                Position::new(anchor.0, anchor.1),
                Position::new(focus.0, focus.1),
            ))
            .unwrap();
    }

    fn cached_layouts(context: &egui::Context) -> ParagraphLayouts {
        let id = context.memory(|memory| memory.focused()).unwrap();
        let cache = context.data(|data| data.get_temp::<Cache>(id)).unwrap();
        cache.layout.layouts
    }

    #[test]
    fn unrelated_passes_keep_layouts_and_their_environment() {
        let context = egui::Context::default();
        let mut cache = LayoutCache::default();
        let document = Document::from_text("first\nsecond café\nthird");
        let appearance = appearance(16.0);
        let (initial, changed) = layout_frame(&context, &mut cache, &document, &appearance);
        assert!(changed);
        for pass in [other_viewport_pass, hidden_editor_pass] {
            pass(&context);
            let (after, changed) = layout_frame(&context, &mut cache, &document, &appearance);
            assert!(!changed);
            assert!(initial.identity().ptr_eq(&after.identity()));
            assert!(Arc::ptr_eq(
                &initial.environment_identity(),
                &after.environment_identity()
            ));
            assert!(Arc::ptr_eq(
                &initial.get(1).unwrap().galley,
                &after.get(1).unwrap().galley
            ));
        }
    }

    #[test]
    fn vertical_navigation_keeps_its_column_across_other_viewport_passes() {
        let arrow_down = || Event::Key {
            key: Key::ArrowDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let context = egui::Context::default();
        let mut editor =
            Editor::from_text("abcdefghijklmnopqrstuvwxyz\nx\nabcdefghijklmnopqrstuvwxyz");
        editor
            .set_selection(Selection::caret(Position::new(0, 20)))
            .unwrap();
        widget_frame(&context, &mut editor, vec![], |_| {});
        widget_frame(&context, &mut editor, vec![arrow_down()], |_| {});
        assert_eq!(editor.selection().focus, Position::new(1, 1));
        other_viewport_pass(&context);
        widget_frame(&context, &mut editor, vec![arrow_down()], |_| {});
        assert_eq!(editor.selection().focus, Position::new(2, 20));
    }

    #[test]
    fn font_definition_changes_rebuild_layouts_but_equal_definitions_do_not() {
        let context = egui::Context::default();
        let mut cache = LayoutCache::default();
        let document = Document::from_text("first\nsecond");
        let appearance = appearance(16.0);
        let (initial, _) = layout_frame(&context, &mut cache, &document, &appearance);
        context.set_fonts(egui::FontDefinitions::default());
        let (same, changed) = layout_frame(&context, &mut cache, &document, &appearance);
        assert!(!changed);
        assert!(initial.identity().ptr_eq(&same.identity()));

        let data = egui::FontDefinitions::default().font_data["Hack"].clone();
        context.add_font(egui::epaint::text::FontInsert::new(
            "extra",
            (*data).clone(),
            vec![egui::epaint::text::InsertFontFamily {
                family: FontFamily::Proportional,
                priority: egui::epaint::text::FontPriority::Highest,
            }],
        ));
        let (added, changed) = layout_frame(&context, &mut cache, &document, &appearance);
        assert!(changed);
        assert!(!Arc::ptr_eq(
            &same.environment_identity(),
            &added.environment_identity()
        ));
        assert!(!Arc::ptr_eq(
            &same.get(0).unwrap().galley,
            &added.get(0).unwrap().galley
        ));
    }

    fn painted_first(output: &egui::FullOutput) -> Arc<Galley> {
        output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                Shape::Text(text) if text.galley.job.text == "first" => Some(text.galley.clone()),
                _ => None,
            })
            .unwrap()
    }

    fn uvs(galley: &Galley) -> Vec<([u16; 2], [u16; 2])> {
        galley
            .rows
            .iter()
            .flat_map(|row| {
                row.glyphs
                    .iter()
                    .map(|glyph| (glyph.uv_rect.min, glyph.uv_rect.max))
            })
            .collect()
    }

    fn fresh_uvs(
        context: &egui::Context,
        editor: &mut Editor,
        painted: &Galley,
    ) -> Vec<([u16; 2], [u16; 2])> {
        let mut job = (*painted.job).clone();
        for section in &mut job.sections {
            section.format.color = Color32::RED;
        }
        let mut reference = None;
        widget_frame(context, editor, vec![], |ui| {
            reference = Some(ui.fonts_mut(|fonts| fonts.layout_job(job.clone())));
        });
        uvs(&reference.unwrap())
    }

    fn fill_atlas(ui: &mut Ui) -> f32 {
        let letters: String = ('A'..='Z').chain('a'..='z').collect();
        for size in [150.0, 170.0, 190.0, 210.0, 230.0, 250.0] {
            ui.label(egui::RichText::new(&letters).font(FontId::proportional(size)));
        }
        ui.fonts(|fonts| fonts.font_atlas_fill_ratio())
    }

    #[test]
    fn atlas_recreation_repaints_current_glyphs_without_relayout() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("first\nsecond");
        widget_frame(&context, &mut editor, vec![], |_| {});
        let before = cached_layouts(&context);
        let mut fill = 0.0;
        widget_frame(&context, &mut editor, vec![], |ui| fill = fill_atlas(ui));
        assert!(fill > 0.8);
        let output = widget_frame(&context, &mut editor, vec![], |ui| {
            ui.label(egui::RichText::new("WXYZ").font(FontId::proportional(64.0)));
        });
        let after = cached_layouts(&context);
        assert!(before.identity().ptr_eq(&after.identity()));
        let painted = painted_first(&output);
        let reference = fresh_uvs(&context, &mut editor, &painted);
        assert_ne!(uvs(&after.get(0).unwrap().galley), reference);
        assert_eq!(uvs(&painted), reference);
    }

    #[test]
    fn unrelated_passes_keep_painted_galleys() {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("first\nsecond");
        widget_frame(&context, &mut editor, vec![], |_| {});
        let before = painted_first(&widget_frame(&context, &mut editor, vec![], |_| {}));
        for pass in [other_viewport_pass, hidden_editor_pass] {
            pass(&context);
            let after = painted_first(&widget_frame(&context, &mut editor, vec![], |_| {}));
            assert!(Arc::ptr_eq(&before, &after));
        }
    }

    #[test]
    fn painting_needs_only_the_editor_font_family() {
        let context = egui::Context::default();
        let mut definitions = egui::FontDefinitions::default();
        let fonts = definitions
            .families
            .remove(&FontFamily::Proportional)
            .unwrap();
        let family = FontFamily::Name("body".into());
        definitions.families.insert(family.clone(), fonts);
        context.set_fonts(definitions);
        let mut editor = Editor::from_text("first\nsecond");
        for _ in 0..2 {
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                RichTextEditor::new(&mut editor)
                    .font(FontId::new(16.0, family.clone()))
                    .show(ui);
            });
            output.textures_delta.clear();
            assert_eq!(
                painted_first(&output).job.sections[0].format.font_id.family,
                family
            );
        }
    }

    #[test]
    fn atlas_recreation_during_unrelated_passes_refreshes_painted_glyphs() {
        for viewport in [
            egui::ViewportId::from_hash_of("other"),
            egui::ViewportId::ROOT,
        ] {
            let context = egui::Context::default();
            let mut editor = Editor::from_text("first\nsecond");
            widget_frame(&context, &mut editor, vec![], |_| {});
            let stale = painted_first(&widget_frame(&context, &mut editor, vec![], |_| {}));
            let recorded = context.fonts(|fonts| fonts.font_atlas_fill_ratio());
            let mut fill = 0.0;
            unrelated_pass(&context, viewport, |ui| fill = fill_atlas(ui));
            assert!(fill > 0.8);
            unrelated_pass(&context, viewport, |ui| {
                ui.label(egui::RichText::new("ABCDEFGHIJKLM").font(FontId::proportional(120.0)));
                fill = ui.fonts(|fonts| fonts.font_atlas_fill_ratio());
            });
            assert!((recorded..0.8).contains(&fill), "{fill} vs {recorded}");
            let painted = painted_first(&widget_frame(&context, &mut editor, vec![], |_| {}));
            assert!(!Arc::ptr_eq(&stale, &painted));
            let reference = fresh_uvs(&context, &mut editor, &painted);
            assert_ne!(uvs(&stale), reference);
            assert_eq!(uvs(&painted), reference);
        }
    }

    #[test]
    fn layout_counts_scalars_without_indexing_paragraphs() {
        let context = egui::Context::default();
        let mut cache = LayoutCache::default();
        let document = Document::from_text("café one\nnaïve 👩‍💻 two\nascii three");
        let (layouts, _) = layout_frame(&context, &mut cache, &document, &appearance(16.0));
        assert!(
            document
                .paragraphs()
                .iter()
                .all(|paragraph| !paragraph.has_scalar_index())
        );
        let mut offset = 0;
        for (index, paragraph) in document.paragraphs().iter().enumerate() {
            assert_eq!(layouts.char_offset(index), offset);
            offset += paragraph.text().chars().count() + 1;
        }
        assert_eq!(layouts.char_offset(document.paragraphs().len()), offset);
    }

    #[test]
    fn list_gutters_scale_with_the_font_and_align_three_digit_ordinals() {
        let mut editor = Editor::from_text(&vec!["item"; 120].join("\n"));
        editor.select_all();
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: None,
            })
            .unwrap();
        let mut bullets = Editor::from_text("top\nnested");
        bullets.select_all();
        bullets
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
            .unwrap();
        bullets
            .set_selection(Selection::caret(Position::new(1, 0)))
            .unwrap();
        bullets.indent_list().unwrap();
        let context = egui::Context::default();
        for size in [12.5, 16.0, 24.0, 40.0] {
            let appearance = appearance(size);
            let mut cache = LayoutCache::default();
            let (layouts, _) = layout_frame(&context, &mut cache, editor.document(), &appearance);
            let left = layouts.get(0).unwrap().rect.left();
            assert_eq!(left, left.round(), "size={size}");
            assert!(left >= ORDERED_GUTTER * size, "size={size}");
            for index in [8, 9, 98, 99, 119] {
                let layout = layouts.get(index).unwrap();
                assert_eq!(layout.rect.left(), left, "size={size}, item={}", index + 1);
                let marker = layout.marker.unwrap();
                assert_eq!(marker.job.text, format!("{}.", index + 1));
                let gap = layout.rect.left() - layout.marker_position.x - marker.size().x;
                assert!((gap - MARKER_GAP * size).abs() < 1e-3, "size={size}");
            }

            let mut cache = LayoutCache::default();
            let (layouts, _) = layout_frame(&context, &mut cache, bullets.document(), &appearance);
            let top = layouts.get(0).unwrap().rect.left();
            let nested = layouts.get(1).unwrap().rect.left();
            assert_eq!(top, (BULLET_GUTTER * size).ceil(), "size={size}");
            assert_eq!(nested, (LEVEL_STEP * size + BULLET_GUTTER * size).ceil());
        }
    }

    #[test]
    fn quote_paragraphs_indent_with_the_font_and_carry_a_bar_in_their_gutter() {
        let mut editor = Editor::from_text("before\nquoted text\n\nafter");
        select(&mut editor, (1, 0), (2, 0));
        editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
        select(&mut editor, (2, 0), (2, 0));
        editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
        let context = egui::Context::default();
        for size in [12.5, 16.0, 24.0, 40.0] {
            let appearance = appearance(size);
            let mut cache = LayoutCache::default();
            let (layouts, _) = layout_frame(&context, &mut cache, editor.document(), &appearance);
            for index in [0, 3] {
                let layout = layouts.get(index).unwrap();
                assert_eq!(layout.rect.left(), 0.0);
                assert!(layout.quote_bar.is_none() && layout.marker.is_none());
            }
            for index in [1, 2] {
                let layout = layouts.get(index).unwrap();
                let indent = (QUOTE_GUTTER * size).ceil();
                assert_eq!(layout.rect.left(), indent, "size={size}");
                assert_eq!(layout.galley.job.wrap.max_width, appearance.width - indent);
                assert!(layout.marker.is_none());
                let bar = layout.quote_bar.unwrap();
                assert!(bar.left() > 0.0 && bar.right() < layout.rect.left());
                assert!(bar.width() >= 1.0);
                assert_eq!(bar.y_range(), layout.rect.y_range());
            }
        }

        let mut narrow = appearance(16.0);
        narrow.width = 40.0;
        let mut cache = LayoutCache::default();
        let (layouts, _) = layout_frame(&context, &mut cache, editor.document(), &narrow);
        let layout = layouts.get(1).unwrap();
        assert_eq!(layout.rect.left(), 8.0);
        assert!(layout.quote_bar.unwrap().right() < 8.0);
        narrow.width = 20.0;
        let (layouts, _) = layout_frame(&context, &mut cache, editor.document(), &narrow);
        let layout = layouts.get(1).unwrap();
        assert_eq!(layout.rect.left(), 0.0);
        assert!(layout.quote_bar.is_none());
    }

    #[test]
    fn incremental_quote_layouts_match_a_fresh_layout() {
        let context = egui::Context::default();
        let appearance = appearance(16.0);
        let mut cache = LayoutCache::default();
        let mut editor = Editor::from_text("a\nb\nc\nd");
        type Edit = fn(&mut Editor);
        let edits: [Edit; 6] = [
            |editor| {
                select(editor, (1, 0), (2, 1));
                editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
            },
            |editor| {
                select(editor, (2, 1), (2, 1));
                editor.insert_text(" more quoted text").unwrap();
            },
            |editor| {
                editor.insert_paragraph().unwrap();
            },
            |editor| {
                editor.insert_paragraph().unwrap();
            },
            |editor| {
                select(editor, (1, 0), (1, 0));
                editor.delete_backward().unwrap();
            },
            |editor| while editor.undo() {},
        ];
        for (step, edit) in edits.iter().enumerate() {
            layout_frame(&context, &mut cache, editor.document(), &appearance);
            edit(&mut editor);
            let (actual, _) = layout_frame(&context, &mut cache, editor.document(), &appearance);
            let mut fresh = LayoutCache::default();
            let (expected, _) = layout_frame(&context, &mut fresh, editor.document(), &appearance);
            assert_eq!(actual.len(), expected.len());
            for index in 0..expected.len() {
                let (actual, expected) = (actual.get(index).unwrap(), expected.get(index).unwrap());
                assert_eq!(actual.rect, expected.rect, "step {step}, {index}");
                assert_eq!(actual.quote_bar, expected.quote_bar, "step {step}, {index}");
                assert_eq!(
                    actual.galley.job, expected.galley.job,
                    "step {step}, {index}"
                );
                let quote = editor.document().paragraphs()[index].kind() == ParagraphKind::Quote;
                assert_eq!(actual.quote_bar.is_some(), quote, "step {step}, {index}");
            }
        }
    }

    #[test]
    fn unregistered_bold_and_code_families_fall_back_instead_of_panicking() {
        let context = egui::Context::default();
        let mut definitions = egui::FontDefinitions::default();
        definitions.families.remove(&FontFamily::Monospace);
        context.set_fonts(definitions);
        let mut editor = Editor::from_text("bold code plain");
        for (range, patch) in [
            (
                0..4,
                StylePatch {
                    bold: Some(true),
                    ..StylePatch::default()
                },
            ),
            (
                5..9,
                StylePatch {
                    code: Some(true),
                    ..StylePatch::default()
                },
            ),
        ] {
            editor
                .set_selection(Selection::new(
                    Position::new(0, range.start),
                    Position::new(0, range.end),
                ))
                .unwrap();
            editor.apply_style(patch).unwrap();
        }
        let mut appearance = appearance(16.0);
        appearance.bold_family = Some(FontFamily::Name("missing-bold".into()));
        appearance.strong_color = Color32::RED;
        let mut cache = LayoutCache::default();
        let (layouts, _) = layout_frame(&context, &mut cache, editor.document(), &appearance);
        let sections = &layouts.get(0).unwrap().galley.job.sections;
        assert_eq!(sections.len(), 4);
        assert_eq!(sections[0].format.font_id, FontId::proportional(16.0));
        assert_eq!(sections[0].format.color, Color32::RED);
        assert_eq!(sections[2].format.font_id, FontId::proportional(16.0));
        assert_eq!(sections[2].format.background, Color32::BLACK);

        widget_frame(&context, &mut editor, vec![], |ui| {
            RichTextEditor::new(&mut Editor::from_text("x"))
                .bold_font_family(FontFamily::Name("missing-bold".into()))
                .show(ui);
        });
    }

    #[test]
    fn renumbered_list_items_keep_their_shaped_text() {
        let context = egui::Context::default();
        let mut cache = LayoutCache::default();
        let mut editor = Editor::from_text("one\ntwo\nthree\nfour");
        editor.select_all();
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: Some(8),
            })
            .unwrap();
        let appearance = appearance(16.0);
        let (before, _) = layout_frame(&context, &mut cache, editor.document(), &appearance);

        let paragraphs = editor.document().paragraphs().to_vec();
        editor
            .set_selection(Selection::caret(Position::new(0, 3)))
            .unwrap();
        editor.insert_paragraph().unwrap();
        let (split, _) = layout_frame(&context, &mut cache, editor.document(), &appearance);
        for (old, new) in [(1, 2), (2, 3), (3, 4)] {
            assert!(Arc::ptr_eq(
                &paragraphs[old],
                &editor.document().paragraphs()[new]
            ));
            assert!(Arc::ptr_eq(
                &before.get(old).unwrap().galley,
                &split.get(new).unwrap().galley
            ));
            let marker = split.get(new).unwrap().marker.unwrap();
            assert_eq!(marker.job.text, format!("{}.", new + 8));
        }

        editor
            .set_selection(Selection::caret(Position::new(0, 0)))
            .unwrap();
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: Some(1),
            })
            .unwrap();
        let (restarted, _) = layout_frame(&context, &mut cache, editor.document(), &appearance);
        let mut fresh = LayoutCache::default();
        let (expected, _) = layout_frame(&context, &mut fresh, editor.document(), &appearance);
        for index in 0..5 {
            let actual = restarted.get(index).unwrap();
            assert!(Arc::ptr_eq(
                &split.get(index).unwrap().galley,
                &actual.galley
            ));
            let expected = expected.get(index).unwrap();
            assert_eq!(actual.galley.job, expected.galley.job);
            assert_eq!(actual.rect, expected.rect);
            assert_eq!(actual.marker_position, expected.marker_position);
            assert_eq!(
                actual.marker.map(|marker| marker.job.text.clone()),
                Some(format!("{}.", index + 1))
            );
        }
    }

    #[test]
    fn incremental_markers_match_a_fresh_layout_after_list_edits() {
        let context = egui::Context::default();
        let appearance = appearance(16.0);
        let mut cache = LayoutCache::default();
        let mut editor = Editor::from_text("a\nb\nc\nd\ne\nf\ng\nbody\nh\nquote\ni\nTitle\nj\nk");
        select(&mut editor, (0, 0), (6, 1));
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: None,
            })
            .unwrap();
        for (index, kind) in [
            (
                8,
                ParagraphKind::Ordered {
                    indent: 0,
                    start: None,
                },
            ),
            (9, ParagraphKind::Quote),
            (
                10,
                ParagraphKind::Ordered {
                    indent: 0,
                    start: None,
                },
            ),
            (11, ParagraphKind::Heading { level: 2 }),
            (
                12,
                ParagraphKind::Ordered {
                    indent: 0,
                    start: None,
                },
            ),
            (
                13,
                ParagraphKind::Ordered {
                    indent: 0,
                    start: Some(98),
                },
            ),
        ] {
            select(&mut editor, (index, 0), (index, 0));
            editor.set_paragraph_kind(kind).unwrap();
        }
        editor.clear_history();
        type Edit = fn(&mut Editor);
        let edits: [Edit; 9] = [
            |editor| {
                select(editor, (0, 1), (0, 1));
                editor.insert_paragraph().unwrap();
            },
            |editor| {
                select(editor, (2, 0), (2, 0));
                editor.indent_list().unwrap();
            },
            |editor| {
                select(editor, (3, 0), (3, 0));
                editor.set_paragraph_kind(ParagraphKind::Body).unwrap();
            },
            |editor| {
                editor
                    .set_paragraph_kind(ParagraphKind::Ordered {
                        indent: 0,
                        start: None,
                    })
                    .unwrap();
            },
            |editor| {
                select(editor, (0, 0), (1, 0));
                editor.delete_backward().unwrap();
            },
            |editor| {
                select(editor, (4, 1), (4, 1));
                editor.insert_text("text").unwrap();
            },
            |editor| {
                select(editor, (7, 0), (8, 0));
                editor.insert_text("").unwrap();
            },
            |editor| {
                select(editor, (7, 0), (7, 0));
                editor
                    .set_paragraph_kind(ParagraphKind::Ordered {
                        indent: 0,
                        start: Some(999),
                    })
                    .unwrap();
                select(editor, (7, 0), (7, 0));
                editor.insert_paragraph().unwrap();
            },
            |editor| while editor.undo() {},
        ];
        for (step, edit) in edits.iter().enumerate() {
            let (before, _) = layout_frame(&context, &mut cache, editor.document(), &appearance);
            let paragraphs = editor.document().paragraphs().to_vec();
            edit(&mut editor);
            let (actual, _) = layout_frame(&context, &mut cache, editor.document(), &appearance);
            let mut fresh = LayoutCache::default();
            let (expected, _) = layout_frame(&context, &mut fresh, editor.document(), &appearance);
            assert_eq!(actual.len(), expected.len());
            for index in 0..expected.len() {
                let (actual, expected) = (actual.get(index).unwrap(), expected.get(index).unwrap());
                let marker = |layout: &ParagraphLayout| {
                    layout.marker.as_ref().map(|marker| marker.job.text.clone())
                };
                assert_eq!(marker(&actual), marker(&expected), "step {step}, {index}");
                assert_eq!(actual.rect, expected.rect, "step {step}, {index}");
                assert_eq!(actual.marker_position, expected.marker_position);
                let document = editor.document();
                let shown = match document.paragraphs()[index].kind() {
                    ParagraphKind::Ordered { .. } => document
                        .list_number(index)
                        .map(|number| format!("{number}.")),
                    _ => None,
                };
                assert_eq!(marker(&actual), shown, "step {step}, {index}");
                if let Some(old) = paragraphs
                    .iter()
                    .position(|old| Arc::ptr_eq(old, &document.paragraphs()[index]))
                    && before.get(old).unwrap().rect.left() == actual.rect.left()
                {
                    assert!(Arc::ptr_eq(
                        &before.get(old).unwrap().galley,
                        &actual.galley
                    ));
                }
            }
        }
        assert_eq!(editor.document().list_number(0), Some(1));
    }

    #[test]
    fn tracked_edit_batches_around_lists_match_a_fresh_layout() {
        let context = egui::Context::default();
        let appearance = appearance(16.0);
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        let kind = |choice: usize, indent: u8| match choice {
            0 | 1 => ParagraphKind::Body,
            2 => ParagraphKind::Quote,
            3 => ParagraphKind::Heading { level: 1 },
            4 => ParagraphKind::Bullet { indent },
            5 => ParagraphKind::Ordered {
                indent,
                start: Some(5),
            },
            _ => ParagraphKind::Ordered {
                indent,
                start: None,
            },
        };
        let (mut merged, mut scanned) = (0, 0);
        for _ in 0..10 {
            let text: Vec<_> = (0..40).map(|index| format!("p{index}")).collect();
            let mut editor = Editor::from_text(&text.join("\n"));
            for index in 0..40 {
                select(&mut editor, (index, 0), (index, 0));
                editor
                    .set_paragraph_kind(kind(next(8), next(2) as u8))
                    .unwrap();
            }
            let mut cache = LayoutCache::default();
            layout_frame(&context, &mut cache, editor.document(), &appearance);
            for step in 0..20 {
                let scans = cache.scans;
                let (mut changes, mut exact, mut untracked) = (0, true, false);
                for _ in 0..1 + next(3) {
                    let before = editor.document().content_identity();
                    let count = editor.document().paragraphs().len();
                    let paragraph = next(count);
                    let len = editor.document().paragraph(paragraph).unwrap().text().len();
                    let byte = if len == 0 { 0 } else { next(len + 1) };
                    select(&mut editor, (paragraph, byte), (paragraph, byte));
                    match next(9) {
                        0 | 1 => editor.insert_paragraph().unwrap(),
                        2 => {
                            select(&mut editor, (paragraph, 0), (paragraph, 0));
                            editor.delete_backward().unwrap();
                        }
                        3 => {
                            let end = (paragraph + 1 + next(3)).min(count - 1);
                            select(&mut editor, (paragraph, 0), (end, 0));
                            editor
                                .insert_text(["", "x\ny", "a\nb\nc"][next(3)])
                                .unwrap();
                        }
                        4 => {
                            editor.undo();
                        }
                        5 => {
                            editor.redo();
                        }
                        6 => editor
                            .set_paragraph_kind(kind(next(8), next(2) as u8))
                            .unwrap(),
                        _ => editor.insert_text("q").unwrap(),
                    }
                    let document = editor.document();
                    if !Arc::ptr_eq(&before, &document.content_identity()) {
                        // An untracked edit is recovered only when it is the last of the batch.
                        exact &= !untracked && document.change_since(&before).is_some();
                        untracked = next(6) == 0;
                        if !untracked {
                            cache.track(document);
                        }
                        changes += 1;
                    }
                }
                let (actual, _) =
                    layout_frame(&context, &mut cache, editor.document(), &appearance);
                assert_eq!(cache.scans, scans + usize::from(!exact), "step {step}");
                merged += usize::from(exact && changes > 1);
                scanned += usize::from(!exact);
                let mut fresh = LayoutCache::default();
                let (expected, _) =
                    layout_frame(&context, &mut fresh, editor.document(), &appearance);
                let document = editor.document();
                assert_eq!(actual.len(), document.paragraphs().len());
                assert_eq!(actual.content_height(), expected.content_height());
                let cached = cache.layouts.caches(0..actual.len());
                for (index, cached) in cached.enumerate() {
                    assert!(Arc::ptr_eq(
                        &cached.paragraph,
                        &document.paragraphs()[index]
                    ));
                    assert_eq!(cached.number, document.list_number(index), "step {step}");
                    let (actual, expected) =
                        (actual.get(index).unwrap(), expected.get(index).unwrap());
                    let marker = |layout: &ParagraphLayout| {
                        layout.marker.as_ref().map(|marker| marker.job.text.clone())
                    };
                    assert_eq!(marker(&actual), marker(&expected), "step {step}, {index}");
                    assert_eq!(actual.rect, expected.rect, "step {step}, {index}");
                    assert_eq!(actual.marker_position, expected.marker_position);
                    assert_eq!(actual.quote_bar, expected.quote_bar);
                    assert_eq!(actual.galley.job, expected.galley.job);
                }
            }
        }
        assert!(
            merged > 50 && scanned > 5,
            "{merged} merged, {scanned} scanned"
        );
    }

    #[test]
    fn merged_changes_cover_both_changes_and_nothing_before_them() {
        fn replace(items: &[usize], range: Range<usize>, len: usize, id: usize) -> Vec<usize> {
            let mut items = items.to_vec();
            items.splice(range, (0..len).map(|index| id + index));
            items
        }
        fn ranges(len: usize) -> impl Iterator<Item = (Range<usize>, usize)> {
            (0..=len).flat_map(move |start| {
                (start..=len).flat_map(move |end| (0..3).map(move |new| (start..end, new)))
            })
        }
        let original: Vec<_> = (0..6).collect();
        for (first, first_len) in ranges(original.len()) {
            let middle = replace(&original, first.clone(), first_len, 100);
            for (second, second_len) in ranges(middle.len()) {
                let result = replace(&middle, second.clone(), second_len, 200);
                let (old, new_len) =
                    merge_changes(((first.clone(), first_len), (second.clone(), second_len)));
                assert_eq!(old.start, first.start.min(second.start));
                assert_eq!(result.len(), original.len() - old.len() + new_len);
                assert_eq!(result[..old.start], original[..old.start]);
                assert_eq!(result[old.start + new_len..], original[old.end..]);
            }
        }
    }

    #[test]
    fn random_edits_across_nested_and_long_lists_match_a_fresh_layout() {
        fn position(editor: &Editor, next: &mut impl FnMut(usize) -> usize) -> (usize, usize) {
            let paragraph = next(editor.document().paragraphs().len());
            let text = editor.document().paragraph(paragraph).unwrap().text();
            let mut bytes: Vec<_> = text.char_indices().map(|(byte, _)| byte).collect();
            bytes.push(text.len());
            (paragraph, bytes[next(bytes.len())])
        }
        fn kind(next: &mut impl FnMut(usize) -> usize) -> ParagraphKind {
            let indent = [0, 0, 0, 1, 1, 2, 3, 64][next(8)];
            match next(8) {
                0 => ParagraphKind::Body,
                1 => ParagraphKind::Quote,
                2 => ParagraphKind::Heading { level: 2 },
                3 => ParagraphKind::Bullet { indent },
                _ => ParagraphKind::Ordered {
                    indent,
                    start: [None, None, None, Some(1), Some(8), Some(98), Some(998)][next(7)],
                },
            }
        }

        let context = egui::Context::default();
        let mut appearance = appearance(16.0);
        appearance.width = 160.0;
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        let mut pasted = Editor::from_text("pasted one\npasted two\npasted three");
        pasted.select_all();
        pasted.apply_numbered_list(0).unwrap();
        select(&mut pasted, (1, 0), (1, 0));
        pasted.indent_list().unwrap();
        let fragment = crate::Fragment::from_document(pasted.document());
        let mut widths = std::collections::BTreeSet::new();
        let mut crossings = std::collections::BTreeSet::new();
        let mut wrapped = false;
        for _ in 0..8 {
            let text: Vec<_> = (0..16)
                .map(|index| match index % 4 {
                    0 => format!("p{index} wraps onto a second row"),
                    _ => format!("p{index}"),
                })
                .collect();
            let mut editor = Editor::from_text(&text.join("\n"));
            for index in 0..16 {
                select(&mut editor, (index, 0), (index, 0));
                let kind = if next(3) == 0 {
                    kind(&mut next)
                } else {
                    ParagraphKind::Ordered {
                        indent: [0, 0, 1, 2][next(4)],
                        start: None,
                    }
                };
                editor.set_paragraph_kind(kind).unwrap();
            }
            let mut cache = LayoutCache::default();
            let mut previous: Vec<(Arc<Paragraph>, usize)> = Vec::new();
            for step in 0..40 {
                let (paragraph, byte) = position(&editor, &mut next);
                select(&mut editor, (paragraph, byte), (paragraph, byte));
                match next(12) {
                    0 | 1 => editor
                        .insert_text(["x", "word ", "typed text that wraps"][next(3)])
                        .unwrap(),
                    2 | 3 => editor.insert_paragraph().unwrap(),
                    4 => {
                        select(&mut editor, (paragraph, 0), (paragraph, 0));
                        editor.delete_backward().unwrap();
                    }
                    5 => {
                        let focus = position(&editor, &mut next);
                        select(&mut editor, (paragraph, byte), focus);
                        editor.delete_backward().unwrap();
                    }
                    6 => {
                        let focus = position(&editor, &mut next);
                        select(&mut editor, (paragraph, byte), focus);
                        editor.set_paragraph_kind(kind(&mut next)).unwrap();
                    }
                    7 if next(2) == 0 => editor.indent_list().unwrap(),
                    7 => editor.outdent_list().unwrap(),
                    8 => {
                        let focus = position(&editor, &mut next);
                        select(&mut editor, (paragraph, byte), focus);
                        if next(2) == 0 {
                            editor.insert_text("x\ny\nz").unwrap();
                        } else {
                            editor.insert_fragment(&fragment).unwrap();
                        }
                    }
                    9 => editor.insert_line_break().unwrap(),
                    10 => {
                        editor.undo();
                    }
                    _ => {
                        editor.redo();
                    }
                }
                let (actual, _) =
                    layout_frame(&context, &mut cache, editor.document(), &appearance);
                let mut fresh = LayoutCache::default();
                let (expected, _) =
                    layout_frame(&context, &mut fresh, editor.document(), &appearance);
                let document = editor.document();
                assert_eq!(actual.len(), document.paragraphs().len());
                assert_eq!(actual.content_height(), expected.content_height());
                let mut current = Vec::new();
                let cached = cache.layouts.caches(0..actual.len());
                for (index, cached) in cached.enumerate() {
                    let paragraph = &document.paragraphs()[index];
                    assert!(Arc::ptr_eq(&cached.paragraph, paragraph));
                    assert_eq!(cached.number, document.list_number(index), "step {step}");
                    let (actual, expected) =
                        (actual.get(index).unwrap(), expected.get(index).unwrap());
                    let marker = |layout: &ParagraphLayout| {
                        let marker = layout.marker.as_ref()?;
                        Some((marker.job.text.clone(), marker.size()))
                    };
                    assert_eq!(marker(&actual), marker(&expected), "step {step}, {index}");
                    assert_eq!(actual.marker_position, expected.marker_position);
                    assert_eq!(actual.rect, expected.rect, "step {step}, {index}");
                    assert_eq!(actual.quote_bar, expected.quote_bar);
                    assert_eq!(actual.galley.job, expected.galley.job);
                    let rows = |layout: &ParagraphLayout| {
                        let rows = layout.galley.rows.iter();
                        rows.map(|row| (row.rect(), row.char_count_including_newline()))
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(rows(&actual), rows(&expected), "step {step}, {index}");
                    wrapped |= actual.galley.rows.len() > 1;
                    if let Some(number) = cached.number {
                        let width = number.to_string().len();
                        widths.insert(width);
                        if let Some(&(_, old)) =
                            previous.iter().find(|(old, _)| Arc::ptr_eq(old, paragraph))
                            && old != width
                        {
                            crossings.insert((old.min(width), old.max(width)));
                        }
                        current.push((Arc::clone(paragraph), width));
                    }
                }
                previous = current;
            }
        }
        assert!(wrapped);
        assert!((1..=4).all(|width| widths.contains(&width)), "{widths:?}");
        assert!(crossings.contains(&(1, 2)) && crossings.contains(&(2, 3)));
    }

    #[test]
    fn links_use_the_hyperlink_color_and_underline_unless_colored_explicitly() {
        let mut editor = Editor::from_text("plain link bold red filled");
        let link = crate::Link::new("https://example.com").unwrap();
        select(&mut editor, (0, 6), (0, 26));
        editor.set_link(Some(link)).unwrap();
        for (range, patch) in [
            (
                11..15,
                StylePatch {
                    bold: Some(true),
                    ..StylePatch::default()
                },
            ),
            (
                16..19,
                StylePatch {
                    foreground: Some(Some(crate::Color::RED)),
                    ..StylePatch::default()
                },
            ),
            (
                20..26,
                StylePatch {
                    background: Some(Some(crate::Color::YELLOW)),
                    ..StylePatch::default()
                },
            ),
        ] {
            select(&mut editor, (0, range.start), (0, range.end));
            editor.apply_style(patch).unwrap();
        }
        let mut appearance = appearance(16.0);
        appearance.strong_color = Color32::GREEN;
        let job = paragraph_job(editor.document().paragraph(0).unwrap(), &appearance, 400.0);
        let formats: Vec<_> = job
            .sections
            .iter()
            .map(|section| {
                let format = &section.format;
                (
                    &job.text[section.byte_range.start.0..section.byte_range.end.0],
                    format.color,
                    format.underline,
                    format.background,
                )
            })
            .collect();
        let red = Color32::from_rgb(255, 0, 0);
        let yellow = Color32::from_rgb(255, 255, 0);
        let link = Color32::LIGHT_BLUE;
        assert_eq!(
            formats,
            [
                ("plain ", Color32::WHITE, Stroke::NONE, Color32::TRANSPARENT),
                (
                    "link bold ",
                    link,
                    Stroke::new(1.0, link),
                    Color32::TRANSPARENT,
                ),
                ("red", red, Stroke::new(1.0, red), Color32::TRANSPARENT),
                (" ", link, Stroke::new(1.0, link), Color32::TRANSPARENT),
                ("filled", link, Stroke::new(1.0, link), yellow),
            ]
        );
    }
}
