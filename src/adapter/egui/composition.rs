use super::{
    Cache,
    layout::{Appearance, LayoutCache, ParagraphLayouts},
};
use crate::{Composition, Document, Editor, Error, InlineStyle, Position, Selection};
use egui::Ui;
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct CompositionPreview {
    pub(super) composition: Arc<Composition>,
    pub(super) style: InlineStyle,
    pub(super) document: Document,
    pub(super) render_cache: LayoutCache,
    pub(super) caret: Position,
    pub(super) selection: Option<Selection>,
}

impl Cache {
    pub(super) fn layout_preview(
        &mut self,
        ui: &Ui,
        editor: &Editor,
        appearance: &Appearance,
    ) -> Result<Option<ParagraphLayouts>, Error> {
        let Some(composition) = editor
            .composition_snapshot()
            .filter(|composition| !composition.text.is_empty())
        else {
            self.preview = None;
            return Ok(None);
        };
        let style = editor.typing_style();
        if !self.preview.as_ref().is_some_and(|preview| {
            preview.style == style
                && (Arc::ptr_eq(&preview.composition, &composition)
                    || preview.composition.text == composition.text
                        && preview.composition.replacement == composition.replacement)
        }) {
            let mut document = editor.document().clone();
            let range = composition.replacement.range();
            let mut preedit_style = style;
            preedit_style.underline = true;
            document.replace(range, &composition.text, preedit_style)?;
            let (caret, selection) = preedit_selection(&document, &composition);
            let render_cache = self
                .preview
                .take()
                .map_or_else(LayoutCache::default, |preview| preview.render_cache);
            self.preview = Some(Box::new(CompositionPreview {
                composition: Arc::clone(&composition),
                style,
                document,
                render_cache,
                caret,
                selection,
            }));
        }
        let preview = self.preview.as_mut().expect("preview initialized above");
        if preview.composition.selection != composition.selection {
            (preview.caret, preview.selection) = preedit_selection(&preview.document, &composition);
        }
        preview.composition = composition;
        Ok(Some(preview.render_cache.layout_document(
            ui,
            &preview.document,
            appearance,
        )))
    }
}

fn preedit_selection(
    document: &Document,
    composition: &Composition,
) -> (Position, Option<Selection>) {
    let start = composition.replacement.range().start;
    let caret_byte = composition
        .selection
        .as_ref()
        .map_or(composition.text.len(), |range| range.end);
    let caret = position_after_prefix(document, start, &composition.text[..caret_byte]);
    let selection = composition.selection.as_ref().map(|selection| {
        if selection.start == selection.end {
            Selection::caret(caret)
        } else {
            Selection::new(
                position_after_prefix(document, start, &composition.text[..selection.start]),
                caret,
            )
        }
    });
    (caret, selection)
}

fn position_after_prefix(document: &Document, start: Position, prefix: &str) -> Position {
    let normalized = crate::document::normalize_newlines(prefix);
    let paragraphs = normalized.bytes().filter(|byte| *byte == b'\n').count();
    let byte = if paragraphs == 0 {
        start.byte + normalized.len()
    } else {
        normalized.rsplit('\n').next().map_or(0, str::len)
    };
    let paragraph = start.paragraph + paragraphs;
    let Some(source) = document.paragraph(paragraph) else {
        return document.end();
    };
    let byte = source.boundary_at_or_after(byte);
    Position::new(paragraph, byte)
}
