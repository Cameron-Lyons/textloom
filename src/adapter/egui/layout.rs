//! Paragraph shaping and incremental layout invalidation.
use super::PARAGRAPH_GAP;
use crate::{Document, InlineStyle, Paragraph, ParagraphKind};
use egui::{
    Color32, FontFamily, FontId, Galley, Pos2, Rect, Stroke, TextFormat, Ui,
    text::{ByteIndex, LayoutJob, LayoutSection},
};
use std::{collections::HashMap, sync::Arc};
mod index;
pub(super) use index::{LayoutIdentity, ParagraphLayouts};
#[derive(Clone, PartialEq)]
pub(super) struct Appearance {
    pub(super) font: FontId,
    pub(super) bold_family: Option<FontFamily>,
    pub(super) color: Color32,
    pub(super) strong_color: Color32,
    pub(super) code_background: Color32,
    pub(super) width: f32,
}

#[derive(Clone)]
pub(super) struct ParagraphCache {
    pub(super) paragraph: Arc<Paragraph>,
    pub(super) galley: Arc<Galley>,
    pub(super) marker: Option<Arc<Galley>>,
    pub(super) chars: usize,
    pub(super) indent: f32,
}

#[derive(Clone)]
pub(super) struct ParagraphLayout {
    pub(super) galley: Arc<Galley>,
    pub(super) rect: Rect,
    pub(super) marker: Option<Arc<Galley>>,
    pub(super) marker_position: Pos2,
}

/// Borrow immutable shaped text while materializing only its current rectangle.
pub(crate) struct ParagraphLayoutView<'a> {
    pub(crate) galley: &'a Arc<Galley>,
    pub(crate) rect: Rect,
    pub(crate) marker: Option<&'a Arc<Galley>>,
    pub(crate) marker_position: Pos2,
}

#[derive(Clone, Default)]
pub(super) struct LayoutCache {
    pub(super) appearance: Option<Appearance>,
    // Egui invalidates this probe when its font cache/atlas changes.
    pub(super) font_probe: Option<Arc<Galley>>,
    pub(super) document_identity: Option<Arc<()>>,
    pub(super) paragraphs: Vec<ParagraphCache>,
    pub(super) layouts: ParagraphLayouts,
}

impl LayoutCache {
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
        ui.fonts_mut(|fonts| {
            let probe = fonts.layout_job(LayoutJob::simple_singleline(
                " ".to_owned(),
                appearance.font.clone(),
                Color32::WHITE,
            ));
            let fonts_changed = self
                .font_probe
                .as_ref()
                .is_none_or(|previous| !Arc::ptr_eq(previous, &probe));
            self.font_probe = Some(probe);
            if !appearance_changed && !document_changed && !fonts_changed {
                return;
            }
            layout_changed = true;
            let mut create = |paragraph: &Arc<Paragraph>| {
                let (indent, marker) = match paragraph.kind() {
                    ParagraphKind::Bullet { indent } => {
                        (24.0 + f32::from(indent) * 20.0, Some("•".to_owned()))
                    }
                    ParagraphKind::Ordered { indent, start } => {
                        (32.0 + f32::from(indent) * 20.0, Some(format!("{start}.")))
                    }
                    _ => (0.0, None),
                };
                let marker = marker.map(|text| {
                    fonts.layout_job(LayoutJob::simple_singleline(
                        text,
                        appearance.font.clone(),
                        appearance.color,
                    ))
                });
                let marker_width = marker.as_ref().map_or(0.0, |galley| galley.size().x);
                let indent = if marker.is_some() {
                    indent.max(marker_width + 8.0)
                } else {
                    indent
                };
                let indent = indent.min((appearance.width - appearance.font.size * 2.0).max(0.0));
                let job =
                    paragraph_job(paragraph, appearance, (appearance.width - indent).max(8.0));
                let chars = paragraph.scalar_count();
                let mut galley = fonts.layout_job(job);
                if galley.end().index.0 != chars {
                    // egui's shaper can duplicate scalar slots for descending
                    // clusters. Retry independent scalars to retain valid text
                    // coordinates and every original section's appearance.
                    galley = fonts.layout_job(scalar_layout_job(&galley.job));
                }
                ParagraphCache {
                    paragraph: paragraph.clone(),
                    galley,
                    marker,
                    chars,
                    indent,
                }
            };

            if appearance_changed || fonts_changed {
                self.paragraphs.clear();
                self.paragraphs
                    .extend(document.paragraphs().iter().map(create));
                self.layouts.rebuild(&self.paragraphs, true);
            } else {
                let source = document.paragraphs();
                // An exact edit descriptor avoids discovering changed paragraphs.
                // Missing/skipped edits or unrelated clones retain the identity
                // comparison fallback, preserving untouched prefix/suffix layouts.
                let (old, new) = if let Some(change) = self
                    .document_identity
                    .as_ref()
                    .and_then(|id| document.change_since(id))
                {
                    let new = change.range.start..change.range.start + change.new_len;
                    (change.range, new)
                } else {
                    let prefix = self
                        .paragraphs
                        .iter()
                        .zip(source)
                        .take_while(|(cache, paragraph)| Arc::ptr_eq(&cache.paragraph, paragraph))
                        .count();
                    let suffix = self.paragraphs[prefix..]
                        .iter()
                        .rev()
                        .zip(source[prefix..].iter().rev())
                        .take_while(|(cache, paragraph)| Arc::ptr_eq(&cache.paragraph, paragraph))
                        .count();
                    (
                        prefix..self.paragraphs.len() - suffix,
                        prefix..source.len() - suffix,
                    )
                };
                if old.len() == new.len() {
                    for (cache, paragraph) in self.paragraphs[old.clone()]
                        .iter_mut()
                        .zip(&source[new.clone()])
                    {
                        if !Arc::ptr_eq(&cache.paragraph, paragraph) {
                            *cache = create(paragraph);
                        }
                    }
                } else {
                    let mut previous: HashMap<_, _> = self
                        .paragraphs
                        .drain(old.clone())
                        .map(|cache| (Arc::as_ptr(&cache.paragraph), cache))
                        .collect();
                    let replacement = source[new.clone()].iter().map(|paragraph| {
                        previous
                            .remove(&Arc::as_ptr(paragraph))
                            .unwrap_or_else(|| create(paragraph))
                    });
                    self.paragraphs.splice(old.start..old.start, replacement);
                }
                self.layouts.replace(old, &self.paragraphs[new]);
            }
            self.appearance = Some(appearance.clone());
            self.document_identity = Some(document_identity);
        });
        (self.layouts.clone(), document_changed, layout_changed)
    }
}
pub(super) fn paragraph_job(
    paragraph: &Paragraph,
    appearance: &Appearance,
    width: f32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    job.keep_trailing_whitespace = true;
    let size_scale = match paragraph.kind() {
        ParagraphKind::Heading { level } => match level {
            1 => 1.8,
            2 => 1.5,
            3 => 1.25,
            _ => 1.1,
        },
        _ => 1.0,
    };
    for span in paragraph.spans() {
        job.append(
            &paragraph.text()[span.range.clone()],
            0.0,
            text_format(span.style, appearance, size_scale),
        );
    }
    if job.sections.is_empty() {
        // An empty paragraph still has a caret row with the right font height.
        job.append(
            "",
            0.0,
            text_format(InlineStyle::default(), appearance, size_scale),
        );
    }
    job
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

pub(super) fn text_format(
    style: InlineStyle,
    appearance: &Appearance,
    size_scale: f32,
) -> TextFormat {
    let mut font = appearance.font.clone();
    font.size *= size_scale;
    if style.code {
        font.family = FontFamily::Monospace;
    } else if style.bold
        && let Some(family) = &appearance.bold_family
    {
        font.family = family.clone();
    }
    let color = style.foreground.map_or_else(
        || {
            if style.bold {
                appearance.strong_color
            } else {
                appearance.color
            }
        },
        |color| Color32::from_rgba_unmultiplied(color.0[0], color.0[1], color.0[2], color.0[3]),
    );
    TextFormat {
        font_id: font,
        color,
        italics: style.italic,
        underline: if style.underline {
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
    }
}
