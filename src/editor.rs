use std::{collections::VecDeque, ops::Range};
use unicode_segmentation::UnicodeSegmentation;

use crate::document::Delta;
use crate::{
    Document, Error, InlineStyle, Movement, ParagraphKind, Position, Selection, StylePatch,
};

/// Native IME preedit. It is rendered over `replacement` and never stored in the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Composition {
    pub text: String,
    /// UTF-8 byte offsets supplied by the input method; `None` means a hidden cursor.
    pub selection: Option<Range<usize>>,
    pub replacement: Selection,
}

/// Limits apply to retained undo and redo entries together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryLimits {
    pub max_entries: usize,
    pub max_bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_entries: 256,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy)]
struct EditState {
    selection: Selection,
    style: InlineStyle,
}

struct HistoryEntry {
    deltas: Vec<Delta>,
    before: EditState,
    after: EditState,
    typing: bool,
    bytes: usize,
}

/// A document, directional selection, bounded history, and transient IME state.
///
/// Adjacent single-paragraph insertions with the same style form one undo step.
/// Call [`Self::break_history_group`] between independently undoable operations.
pub struct Editor {
    document: Document,
    selection: Selection,
    typing_style: InlineStyle,
    undo: VecDeque<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    history_bytes: usize,
    limits: HistoryLimits,
    coalesce_typing: bool,
    preferred_column: Option<usize>,
    composition: Option<Composition>,
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
            undo: VecDeque::new(),
            redo: Vec::new(),
            history_bytes: 0,
            limits: HistoryLimits::default(),
            coalesce_typing: false,
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
    pub fn composition(&self) -> Option<&Composition> {
        self.composition.as_ref()
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn history_limits(&self) -> HistoryLimits {
        self.limits
    }
    /// Conservative estimate of paragraph content retained by undo and redo.
    pub fn history_bytes(&self) -> usize {
        self.history_bytes
    }
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    pub fn set_history_limits(&mut self, limits: HistoryLimits) {
        self.limits = limits;
        self.break_history_group();
        self.enforce_limits();
    }

    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.history_bytes = 0;
        self.break_history_group();
    }

    pub fn break_history_group(&mut self) {
        self.coalesce_typing = false;
    }

    /// Validate both endpoints before changing selection. Explicit selection cancels preedit.
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
        // Both endpoints are always valid for this document.
        let _ = self.set_selection(Selection::new(Position::default(), self.document.end()));
    }

    pub fn selected_text(&self) -> String {
        self.document
            .text(self.selection.range())
            .expect("editor maintains a valid selection")
    }

    pub fn insert_text(&mut self, text: &str) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let typing = self.selection.is_caret() && !text.is_empty() && !text.contains(['\n', '\r']);
        self.replace_selection(text, typing)
    }

    /// Replace a forward document range and move the caret after inserted text.
    /// Undo restores the selection that existed before this programmatic edit.
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

    /// Enter continues lists and returns to body text after a heading.
    /// Enter on an empty list item exits the list without inserting a blank paragraph.
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

    pub fn apply_style(&mut self, patch: StylePatch) -> Result<(), Error> {
        self.ensure_no_composition()?;
        self.break_history_group();
        let before = self.state();
        let delta = self.document.apply_style(self.selection.range(), patch)?;
        patch.apply(&mut self.typing_style);
        if let Some(delta) = delta {
            self.record(vec![delta], before, false);
        }
        Ok(())
    }

    /// Applies paragraph formatting to every selected paragraph. An endpoint at
    /// the start of the next paragraph excludes that paragraph.
    /// Ordered numbering starts at `start` in the first selected item and
    /// continues through following consecutive items with the same indent.
    pub fn set_paragraph_kind(&mut self, kind: ParagraphKind) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let range = self.selection.range();
        let end = if range.end.paragraph > range.start.paragraph && range.end.byte == 0 {
            range.end.paragraph
        } else {
            range.end.paragraph + 1
        };
        let before = self.state();
        let delta = self.document.set_kind(range.start.paragraph..end, kind)?;
        self.break_history_group();
        if let Some(delta) = delta {
            self.record(vec![delta], before, false);
        }
        Ok(())
    }

    /// Moves by logical graphemes/words/paragraphs; visual wrapped-line movement
    /// is supplied by a GUI adapter's layout engine through `set_selection`.
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
                    self.paragraph_text(focus.paragraph)[..focus.byte]
                        .graphemes(true)
                        .count()
                });
                self.preferred_column = Some(column);
                let paragraph = if movement == Movement::ParagraphUp {
                    focus.paragraph.saturating_sub(1)
                } else {
                    (focus.paragraph + 1).min(self.document.paragraphs().len() - 1)
                };
                let text = self.paragraph_text(paragraph);
                Position::new(
                    paragraph,
                    text.grapheme_indices(true)
                        .nth(column)
                        .map_or(text.len(), |(byte, _)| byte),
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

    pub fn begin_composition(&mut self) {
        if self.composition.is_none() {
            self.break_history_group();
            self.composition = Some(Composition {
                text: String::new(),
                selection: None,
                replacement: self.selection,
            });
        }
    }

    /// Preedit offsets follow native IME UTF-8 byte coordinates, which may be
    /// Unicode scalar boundaries within an unfinished grapheme.
    pub fn update_composition(
        &mut self,
        text: &str,
        selection: Option<Range<usize>>,
    ) -> Result<(), Error> {
        if let Some(range) = &selection
            && (range.start > range.end
                || range.end > text.len()
                || !text.is_char_boundary(range.start)
                || !text.is_char_boundary(range.end))
        {
            return Err(Error::InvalidCompositionSelection);
        }
        self.begin_composition();
        let composition = self
            .composition
            .as_mut()
            .expect("composition just initialized");
        composition.text.clear();
        composition.text.push_str(text);
        composition.selection = selection;
        Ok(())
    }

    /// A complete composition is a single undo step, including replacement of a selection.
    pub fn commit_composition(&mut self, text: &str) -> Result<(), Error> {
        self.break_history_group();
        if let Some(composition) = self.composition.take() {
            self.selection = composition.replacement;
        }
        self.replace_selection(text, false)
    }

    pub fn cancel_composition(&mut self) -> bool {
        if let Some(composition) = self.composition.take() {
            self.selection = composition.replacement;
            self.break_history_group();
            true
        } else {
            false
        }
    }

    pub fn undo(&mut self) -> bool {
        self.cancel_composition();
        self.break_history_group();
        let Some(entry) = self.undo.pop_back() else {
            return false;
        };
        for delta in entry.deltas.iter().rev() {
            self.document.replay(delta, false);
        }
        self.restore(entry.before);
        self.redo.push(entry);
        true
    }

    pub fn redo(&mut self) -> bool {
        self.cancel_composition();
        self.break_history_group();
        let Some(entry) = self.redo.pop() else {
            return false;
        };
        for delta in &entry.deltas {
            self.document.replay(delta, true);
        }
        self.restore(entry.after);
        self.undo.push_back(entry);
        true
    }

    fn ensure_no_composition(&self) -> Result<(), Error> {
        if self.composition.is_some() {
            Err(Error::CompositionActive)
        } else {
            Ok(())
        }
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
            self.record(vec![delta], before, typing);
        }
        Ok(())
    }

    fn record(&mut self, deltas: Vec<Delta>, before: EditState, typing: bool) {
        for entry in self.redo.drain(..) {
            self.history_bytes = self.history_bytes.saturating_sub(entry.bytes);
        }
        let after = self.state();
        let mut entry = HistoryEntry {
            bytes: deltas.iter().map(Delta::retained_bytes).sum(),
            deltas,
            before,
            after,
            typing,
        };
        let merge = self.coalesce_typing
            && typing
            && self.undo.back().is_some_and(|previous| {
                previous.typing
                    && previous.after.selection == before.selection
                    && previous.after.style == before.style
                    && previous.deltas.len() == 1
                    && entry.deltas.len() == 1
                    && previous.deltas[0].start == entry.deltas[0].start
                    && previous.deltas[0].before.len() == 1
                    && previous.deltas[0].after.len() == 1
                    && entry.deltas[0].before.len() == 1
                    && entry.deltas[0].after.len() == 1
            });
        if merge {
            let previous = self.undo.back_mut().expect("checked entry");
            self.history_bytes = self.history_bytes.saturating_sub(previous.bytes);
            previous.deltas[0].after = std::mem::take(&mut entry.deltas[0].after);
            previous.after = after;
            previous.bytes = previous.deltas[0].retained_bytes();
            self.history_bytes = self.history_bytes.saturating_add(previous.bytes);
        } else {
            self.history_bytes = self.history_bytes.saturating_add(entry.bytes);
            self.undo.push_back(entry);
        }
        self.coalesce_typing = typing;
        self.enforce_limits();
    }

    fn enforce_limits(&mut self) {
        while self.undo.len() + self.redo.len() > self.limits.max_entries
            || self.history_bytes > self.limits.max_bytes
        {
            // Dropping oldest undo or farthest redo preserves the applicable chain.
            let entry = self.undo.pop_front().or_else(|| {
                if self.redo.is_empty() {
                    None
                } else {
                    Some(self.redo.remove(0))
                }
            });
            let Some(entry) = entry else {
                break;
            };
            self.history_bytes = self.history_bytes.saturating_sub(entry.bytes);
        }
    }

    fn paragraph_text(&self, paragraph: usize) -> &str {
        self.document
            .paragraph(paragraph)
            .expect("valid paragraph")
            .text()
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
            self.paragraph_text(position.paragraph)[..position.byte]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(byte, _)| byte),
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
        let length = text[position.byte..]
            .graphemes(true)
            .next()
            .expect("nonempty suffix")
            .len();
        Position::new(position.paragraph, position.byte + length)
    }

    fn previous_word(&self, position: Position) -> Position {
        if position.byte == 0 {
            return self.previous_grapheme(position);
        }
        let text = self.paragraph_text(position.paragraph);
        let byte = text
            .unicode_word_indices()
            .rev()
            .find(|(start, _)| *start < position.byte)
            .map_or(0, |(start, _)| start);
        self.snap_boundary(Position::new(position.paragraph, byte), false)
    }

    fn next_word(&self, position: Position) -> Position {
        let text = self.paragraph_text(position.paragraph);
        if position.byte == text.len() {
            return self.next_grapheme(position);
        }
        let byte = text
            .unicode_word_indices()
            .find(|(start, word)| start + word.len() > position.byte)
            .map_or(text.len(), |(start, word)| start + word.len());
        self.snap_boundary(Position::new(position.paragraph, byte), true)
    }

    fn snap_boundary(&self, position: Position, forward: bool) -> Position {
        let text = self.paragraph_text(position.paragraph);
        let mut previous = 0;
        for (byte, _) in text.grapheme_indices(true) {
            if byte >= position.byte {
                return Position::new(
                    position.paragraph,
                    if forward || byte == position.byte {
                        byte
                    } else {
                        previous
                    },
                );
            }
            previous = byte;
        }
        Position::new(
            position.paragraph,
            if forward || position.byte == text.len() {
                text.len()
            } else {
                previous
            },
        )
    }
}
