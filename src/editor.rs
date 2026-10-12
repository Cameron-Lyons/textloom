use std::{ops::Range, sync::Arc};

mod composition;
mod history;

pub use composition::Composition;
pub use history::HistoryLimits;
use history::{EditState, History};

use crate::document::Delta;
use crate::{
    Document, Error, Fragment, InlineStyle, Movement, ParagraphKind, Position, SearchOptions,
    Selection, SelectionStyle, StylePatch,
};

pub struct Editor {
    document: Document,
    selection: Selection,
    typing_style: InlineStyle,
    history: History,
    preferred_column: Option<usize>,
    composition: Option<Arc<Composition>>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new(Document::new())
    }
}

impl Editor {
    pub fn new(document: Document) -> Self {
        Self {
            typing_style: document.style_at(Position::default()),
            document,
            selection: Selection::default(),
            history: History::default(),
            preferred_column: None,
            composition: None,
        }
    }

    pub fn from_text(text: &str) -> Self {
        Self::new(Document::from_text(text))
    }
    pub fn document(&self) -> &Document {
        &self.document
    }
    pub fn selection(&self) -> Selection {
        self.selection
    }
    pub fn typing_style(&self) -> InlineStyle {
        self.typing_style
    }
    pub fn selection_style(&self) -> SelectionStyle {
        if self.selection.is_caret() {
            return self.typing_style.into();
        }
        let range = self.selection.range();
        let mut result: Option<SelectionStyle> = None;
        for index in range.start.paragraph..=range.end.paragraph {
            let paragraph = &self.document.paragraphs()[index];
            let start = if index == range.start.paragraph {
                range.start.byte
            } else {
                0
            };
            let end = if index == range.end.paragraph {
                range.end.byte
            } else {
                paragraph.text().len()
            };
            if start == end {
                continue;
            }
            let spans = paragraph.spans();
            let first = spans.partition_point(|span| span.range.end <= start);
            for span in &spans[first..] {
                if span.range.start >= end {
                    break;
                }
                if let Some(style) = &mut result {
                    style.include(span.style);
                    if style.is_fully_mixed() {
                        return *style;
                    }
                } else {
                    result = Some(span.style.into());
                }
            }
        }
        result.unwrap_or_else(|| self.typing_style.into())
    }
    pub fn can_undo(&self) -> bool {
        self.history.undo_len() != 0
    }
    pub fn can_redo(&self) -> bool {
        self.history.redo_len() != 0
    }
    pub fn history_limits(&self) -> HistoryLimits {
        self.history.limits()
    }
    pub fn history_bytes(&self) -> usize {
        self.history.bytes()
    }
    pub fn undo_len(&self) -> usize {
        self.history.undo_len()
    }
    pub fn redo_len(&self) -> usize {
        self.history.redo_len()
    }

    pub fn set_history_limits(&mut self, limits: HistoryLimits) {
        self.history.set_limits(limits);
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    pub fn break_history_group(&mut self) {
        self.history.break_group();
    }

    pub fn set_selection(&mut self, selection: Selection) -> Result<(), Error> {
        self.document.validate_position(selection.anchor)?;
        self.document.validate_position(selection.focus)?;
        self.composition = None;
        self.selection = selection;
        self.typing_style = self.document.style_at(selection.focus);
        self.preferred_column = None;
        self.break_history_group();
        Ok(())
    }

    pub fn select_all(&mut self) {
        let _ = self.set_selection(Selection::new(Position::default(), self.document.end()));
    }

    pub fn selected_text(&self) -> String {
        self.document
            .text(self.selection.range())
            .expect("editor maintains a valid selection")
    }

    pub fn selected_fragment(&self) -> Fragment {
        self.document
            .fragment(self.selection.range())
            .expect("editor maintains a valid selection")
    }

    pub fn insert_fragment(&mut self, fragment: &Fragment) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let before = self.state();
        let (delta, caret) = self
            .document
            .replace_fragment(self.selection.range(), fragment)?;
        self.selection = Selection::caret(caret);
        self.typing_style = self.document.style_at(caret);
        self.preferred_column = None;
        self.break_history_group();
        if !delta.is_empty() {
            self.record(delta, before, false);
        }
        Ok(())
    }

    pub fn find_next(
        &mut self,
        query: &str,
        options: SearchOptions,
        wrap: bool,
    ) -> Result<bool, Error> {
        self.find_match(query, options, wrap, false)
    }

    pub fn find_previous(
        &mut self,
        query: &str,
        options: SearchOptions,
        wrap: bool,
    ) -> Result<bool, Error> {
        self.find_match(query, options, wrap, true)
    }

    fn find_match(
        &mut self,
        query: &str,
        options: SearchOptions,
        wrap: bool,
        backward: bool,
    ) -> Result<bool, Error> {
        self.ensure_no_composition()?;
        let found =
            self.document
                .find_relative(query, options, self.selection.range(), wrap, backward);
        if let Some(range) = found {
            self.set_selection(Selection::new(range.start, range.end))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn replace_all(
        &mut self,
        query: &str,
        replacement: &str,
        options: SearchOptions,
    ) -> Result<usize, Error> {
        self.ensure_no_composition()?;
        let matches = self.document.find(query, options);
        if matches.is_empty() {
            return Ok(0);
        }
        let before = self.state();
        let (delta, caret) = self.document.replace_matches(&matches, replacement);
        self.selection = Selection::caret(caret);
        self.typing_style = self.document.style_at(caret);
        self.preferred_column = None;
        self.break_history_group();
        if !delta.is_empty() {
            self.record(delta, before, false);
        }
        Ok(matches.len())
    }

    pub fn insert_text(&mut self, text: &str) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let typing = self.selection.is_caret() && !text.is_empty() && !text.contains(['\n', '\r']);
        self.replace_selection(text, typing)
    }

    pub fn replace_range(&mut self, range: Range<Position>, text: &str) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if range.start > range.end {
            return Err(Error::InvalidRange);
        }
        self.document.validate_position(range.start)?;
        self.document.validate_position(range.end)?;
        let before = self.state();
        self.selection = Selection::new(range.start, range.end);
        self.replace_selection_with_before(text, false, before)
    }

    pub fn insert_paragraph(&mut self) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if self.selection.is_caret() {
            let paragraph = self
                .document
                .paragraph(self.selection.focus.paragraph)
                .expect("valid selection");
            if paragraph.text().is_empty()
                && matches!(
                    paragraph.kind(),
                    ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. }
                )
            {
                return self.set_paragraph_kind(ParagraphKind::Body);
            }
        }
        self.replace_selection("\n", false)
    }

    pub fn delete_backward(&mut self) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if self.selection.is_caret() {
            let focus = self.selection.focus;
            let previous = self.previous_grapheme(focus);
            if previous == focus {
                return Ok(());
            }
            let before = self.state();
            self.selection = Selection::new(previous, focus);
            return self.replace_selection_with_before("", false, before);
        }
        self.replace_selection("", false)
    }

    pub fn delete_forward(&mut self) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if self.selection.is_caret() {
            let focus = self.selection.focus;
            let next = self.next_grapheme(focus);
            if next == focus {
                return Ok(());
            }
            let before = self.state();
            self.selection = Selection::new(focus, next);
            return self.replace_selection_with_before("", false, before);
        }
        self.replace_selection("", false)
    }

    pub fn delete_word_backward(&mut self) -> Result<(), Error> {
        self.delete_word(false)
    }

    pub fn delete_word_forward(&mut self) -> Result<(), Error> {
        self.delete_word(true)
    }

    fn delete_word(&mut self, forward: bool) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if !self.selection.is_caret() {
            return self.replace_selection("", false);
        }
        let focus = self.selection.focus;
        let target = if forward {
            self.next_word(focus)
        } else {
            self.previous_word(focus)
        };
        if target == focus {
            return Ok(());
        }
        let before = self.state();
        self.selection = Selection::new(focus, target);
        self.replace_selection_with_before("", false, before)
    }

    pub fn apply_style(&mut self, patch: StylePatch) -> Result<(), Error> {
        self.ensure_no_composition()?;
        self.break_history_group();
        let before = self.state();
        let delta = self.document.apply_style(self.selection.range(), patch)?;
        patch.apply(&mut self.typing_style);
        if let Some(delta) = delta {
            self.record(delta, before, false);
        }
        Ok(())
    }

    pub fn clear_formatting(&mut self) -> Result<(), Error> {
        self.apply_style(StylePatch {
            bold: Some(false),
            italic: Some(false),
            underline: Some(false),
            strikethrough: Some(false),
            code: Some(false),
            foreground: Some(None),
        })
    }

    pub fn set_paragraph_kind(&mut self, kind: ParagraphKind) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let before = self.state();
        let delta = self
            .document
            .set_kind(self.selected_paragraph_range(), kind)?;
        self.break_history_group();
        if let Some(delta) = delta {
            self.record(delta, before, false);
        }
        Ok(())
    }

    pub fn indent_list(&mut self) -> Result<(), Error> {
        self.change_list_indent(true)
    }

    pub fn outdent_list(&mut self) -> Result<(), Error> {
        self.change_list_indent(false)
    }

    fn change_list_indent(&mut self, increase: bool) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let before = self.state();
        let delta = self
            .document
            .change_list_indent(self.selected_paragraph_range(), increase);
        self.break_history_group();
        if !delta.is_empty() {
            self.record(delta, before, false);
        }
        Ok(())
    }

    pub fn move_cursor(&mut self, movement: Movement, extend: bool) -> Result<(), Error> {
        self.ensure_no_composition()?;
        self.break_history_group();
        let vertical = matches!(movement, Movement::ParagraphUp | Movement::ParagraphDown);
        if !extend
            && !self.selection.is_caret()
            && matches!(
                movement,
                Movement::GraphemeBackward
                    | Movement::WordBackward
                    | Movement::GraphemeForward
                    | Movement::WordForward
            )
        {
            let range = self.selection.range();
            let position = if matches!(
                movement,
                Movement::GraphemeBackward | Movement::WordBackward
            ) {
                range.start
            } else {
                range.end
            };
            return self.set_selection(Selection::caret(position));
        }
        let focus = self.selection.focus;
        let position = match movement {
            Movement::GraphemeBackward => self.previous_grapheme(focus),
            Movement::GraphemeForward => self.next_grapheme(focus),
            Movement::WordBackward => self.previous_word(focus),
            Movement::WordForward => self.next_word(focus),
            Movement::ParagraphStart => Position::new(focus.paragraph, 0),
            Movement::ParagraphEnd => {
                Position::new(focus.paragraph, self.paragraph_text(focus.paragraph).len())
            }
            Movement::DocumentStart => Position::default(),
            Movement::DocumentEnd => self.document.end(),
            Movement::ParagraphUp | Movement::ParagraphDown => {
                let column = self.preferred_column.unwrap_or_else(|| {
                    self.document
                        .paragraph(focus.paragraph)
                        .expect("valid selection")
                        .grapheme_index(focus.byte)
                        .expect("valid caret")
                });
                self.preferred_column = Some(column);
                let paragraph = if movement == Movement::ParagraphUp {
                    focus.paragraph.saturating_sub(1)
                } else {
                    (focus.paragraph + 1).min(self.document.paragraphs().len() - 1)
                };
                let text = self.document.paragraph(paragraph).expect("valid paragraph");
                Position::new(
                    paragraph,
                    text.byte_from_grapheme(column).unwrap_or(text.text().len()),
                )
            }
        };
        if !vertical {
            self.preferred_column = None;
        }
        if extend {
            self.selection.focus = position;
        } else {
            self.selection = Selection::caret(position);
        }
        self.typing_style = self.document.style_at(position);
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        self.cancel_composition();
        self.break_history_group();
        let Some(state) = self.history.undo(&mut self.document) else {
            return false;
        };
        self.restore(state);
        true
    }

    pub fn redo(&mut self) -> bool {
        self.cancel_composition();
        self.break_history_group();
        let Some(state) = self.history.redo(&mut self.document) else {
            return false;
        };
        self.restore(state);
        true
    }

    fn state(&self) -> EditState {
        EditState {
            selection: self.selection,
            style: self.typing_style,
        }
    }
    fn restore(&mut self, state: EditState) {
        self.selection = state.selection;
        self.typing_style = state.style;
        self.preferred_column = None;
    }

    fn replace_selection(&mut self, text: &str, typing: bool) -> Result<(), Error> {
        self.replace_selection_with_before(text, typing, self.state())
    }

    fn replace_selection_with_before(
        &mut self,
        text: &str,
        typing: bool,
        before: EditState,
    ) -> Result<(), Error> {
        let result = self
            .document
            .replace(self.selection.range(), text, self.typing_style);
        let (delta, caret) = match result {
            Ok(result) => result,
            Err(error) => {
                self.restore(before);
                return Err(error);
            }
        };
        self.selection = Selection::caret(caret);
        self.preferred_column = None;
        if !delta.is_empty() {
            self.record(delta, before, typing);
        }
        Ok(())
    }

    fn record(&mut self, delta: Delta, before: EditState, typing: bool) {
        self.history.record(delta, before, self.state(), typing);
    }

    fn paragraph_text(&self, paragraph: usize) -> &str {
        self.document
            .paragraph(paragraph)
            .expect("valid paragraph")
            .text()
    }

    fn selected_paragraph_range(&self) -> Range<usize> {
        let range = self.selection.range();
        let end = if range.end.paragraph > range.start.paragraph && range.end.byte == 0 {
            range.end.paragraph
        } else {
            range.end.paragraph + 1
        };
        range.start.paragraph..end
    }

    fn previous_grapheme(&self, position: Position) -> Position {
        if position.byte == 0 {
            if position.paragraph == 0 {
                return position;
            }
            return Position::new(
                position.paragraph - 1,
                self.paragraph_text(position.paragraph - 1).len(),
            );
        }
        Position::new(
            position.paragraph,
            self.document
                .paragraph(position.paragraph)
                .expect("valid paragraph")
                .previous_grapheme(position.byte),
        )
    }

    fn next_grapheme(&self, position: Position) -> Position {
        let text = self.paragraph_text(position.paragraph);
        if position.byte == text.len() {
            if position.paragraph + 1 == self.document.paragraphs().len() {
                return position;
            }
            return Position::new(position.paragraph + 1, 0);
        }
        Position::new(
            position.paragraph,
            self.document
                .paragraph(position.paragraph)
                .expect("valid paragraph")
                .next_grapheme(position.byte),
        )
    }

    fn previous_word(&self, position: Position) -> Position {
        if position.byte == 0 {
            return self.previous_grapheme(position);
        }
        Position::new(
            position.paragraph,
            self.document
                .paragraph(position.paragraph)
                .expect("valid paragraph")
                .previous_word(position.byte),
        )
    }

    fn next_word(&self, position: Position) -> Position {
        let text = self.paragraph_text(position.paragraph);
        if position.byte == text.len() {
            return self.next_grapheme(position);
        }
        Position::new(
            position.paragraph,
            self.document
                .paragraph(position.paragraph)
                .expect("valid paragraph")
                .next_word(position.byte),
        )
    }
}
