use std::{collections::VecDeque, ops::Range};

use crate::document::Delta;
use crate::{
    Document, Error, Fragment, InlineStyle, Movement, ParagraphKind, Position, SearchOptions,
    Selection, StylePatch,
};

/// Native IME preedit. It is rendered over `replacement` and never stored in the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Composition {
    /// Uncommitted text supplied by the input method, without newline normalization.
    pub text: String,
    /// UTF-8 byte offsets supplied by the input method; `None` means a hidden cursor.
    pub selection: Option<Range<usize>>,
    /// Document selection that will be replaced when the composition is committed.
    pub replacement: Selection,
}

/// Limits apply to retained undo and redo entries together.
/// The default retains at most 256 entries and an estimated 8 MiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryLimits {
    /// Maximum total entries across both history directions; zero disables history.
    pub max_entries: usize,
    /// Maximum estimated retained bytes; zero disables history.
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
///
/// Text edits, formatting, search navigation, and cursor movement return
/// [`Error::CompositionActive`] during IME preedit. Explicit selection cancels
/// preedit, and undo/redo cancel it before replaying history. Invalid inputs leave
/// document content unchanged. Text and range edits preserve surviving formatting;
/// a newly joined grapheme inherits its first scalar's style.
pub struct Editor {
    document: Document,
    selection: Selection,
    typing_style: InlineStyle,
    undo: VecDeque<HistoryEntry>,
    redo: VecDeque<HistoryEntry>,
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
    /// Start at document start with no history and the document's initial typing style.
    pub fn new(document: Document) -> Self {
        Self {
            typing_style: document.style_at(Position::default()),
            document,
            selection: Selection::default(),
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            history_bytes: 0,
            limits: HistoryLimits::default(),
            coalesce_typing: false,
            preferred_column: None,
            composition: None,
        }
    }

    /// Import plain text with normalized paragraph breaks and initialize a new editor.
    pub fn from_text(text: &str) -> Self {
        Self::new(Document::from_text(text))
    }
    /// Current document; edits go through the editor to maintain selection and history.
    pub fn document(&self) -> &Document {
        &self.document
    }
    /// Current directional selection, always at valid document grapheme boundaries.
    pub fn selection(&self) -> Selection {
        self.selection
    }
    /// Formatting applied to plain text insertions at the current selection.
    pub fn typing_style(&self) -> InlineStyle {
        self.typing_style
    }
    /// Current transient IME preedit, or `None` when no composition is active.
    pub fn composition(&self) -> Option<&Composition> {
        self.composition.as_ref()
    }
    /// Whether retained history contains an undo step.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    /// Whether retained history contains a redo step.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    /// Current limits shared by undo and redo history.
    pub fn history_limits(&self) -> HistoryLimits {
        self.limits
    }
    /// Conservative estimate of text and formatting retained by undo and redo.
    /// Shared navigation indexes and allocator overhead are excluded.
    pub fn history_bytes(&self) -> usize {
        self.history_bytes
    }
    /// Number of retained undo steps; grouped typing counts as one step.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }
    /// Number of retained redo steps.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Change limits, break the current typing group, and immediately evict excess history.
    /// Oldest undo entries are discarded first, then farthest redo entries.
    pub fn set_history_limits(&mut self, limits: HistoryLimits) {
        self.limits = limits;
        self.break_history_group();
        self.enforce_limits();
    }

    /// Discard undo and redo history while retaining document, selection, and IME preedit.
    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.history_bytes = 0;
        self.break_history_group();
    }

    /// Make the next text insertion a separate undo step from preceding typing.
    pub fn break_history_group(&mut self) {
        self.coalesce_typing = false;
    }

    /// Validate both endpoints before changing selection. Explicit selection cancels preedit.
    /// Updates typing style from the focus and breaks the current typing group.
    /// Returns [`Error::InvalidPosition`] without changing editor state for invalid endpoints.
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

    /// Select the entire document and cancel any active IME preedit.
    pub fn select_all(&mut self) {
        // Both endpoints are always valid for this document.
        let _ = self.set_selection(Selection::new(Position::default(), self.document.end()));
    }

    /// Selected plain text, with LF between paragraphs; a caret returns an empty string.
    pub fn selected_text(&self) -> String {
        self.document
            .text(self.selection.range())
            .expect("editor maintains a valid selection")
    }

    /// Capture selected text, inline styles, and paragraph metadata for a rich clipboard.
    pub fn selected_fragment(&self) -> Fragment {
        self.document
            .fragment(self.selection.range())
            .expect("editor maintains a valid selection")
    }

    /// Paste rich text as one undo step. Source paragraph kinds are preserved
    /// at paragraph starts; insertion into existing text retains its first kind.
    /// Moves the caret after the fragment, snapping forward across any joined grapheme.
    /// Returns [`Error::CompositionActive`] during IME preedit.
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
            self.record(vec![delta], before, false);
        }
        Ok(())
    }

    /// Select the next match after the current selection, optionally wrapping.
    /// Returns false without changing selection when no match is available.
    /// Returns [`Error::CompositionActive`] during IME preedit.
    pub fn find_next(
        &mut self,
        query: &str,
        options: SearchOptions,
        wrap: bool,
    ) -> Result<bool, Error> {
        self.find_match(query, options, wrap, false)
    }

    /// Select the preceding match, optionally wrapping to the last match.
    /// Returns false without changing selection when no match is available, or
    /// [`Error::CompositionActive`] during IME preedit.
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
        let matches = self.document.find(query, options);
        let selection = self.selection.range();
        let found = if backward {
            matches
                .iter()
                .rev()
                .find(|range| range.end <= selection.start)
                .or_else(|| if wrap { matches.last() } else { None })
        } else {
            matches
                .iter()
                .find(|range| range.start >= selection.end)
                .or_else(|| if wrap { matches.first() } else { None })
        };
        if let Some(range) = found {
            self.set_selection(Selection::new(range.start, range.end))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Replace every nonoverlapping match as one undo step, preserving each
    /// match's initial style. Ranges refer to the original document, so joined
    /// combining marks and inserted paragraph breaks cannot invalidate later matches.
    /// Returns the number of matches, including replacements that leave content unchanged.
    /// With matches, the caret moves after the final replacement; with none it stays put.
    /// Returns [`Error::CompositionActive`] during IME preedit.
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
        let (delta, caret) = self.document.replace_matches(&matches, replacement)?;
        self.selection = Selection::caret(caret);
        self.typing_style = self.document.style_at(caret);
        self.preferred_column = None;
        self.break_history_group();
        if !delta.is_empty() {
            self.record(vec![delta], before, false);
        }
        Ok(matches.len())
    }

    /// Replace the selection with plain text in the typing style and move the caret after it.
    /// CRLF and CR normalize to paragraph breaks. Adjacent caret insertions without
    /// paragraph breaks may share an undo step. Returns [`Error::CompositionActive`]
    /// during IME preedit.
    pub fn insert_text(&mut self, text: &str) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let typing = self.selection.is_caret() && !text.is_empty() && !text.contains(['\n', '\r']);
        self.replace_selection(text, typing)
    }

    /// Replace a forward document range and move the caret after inserted text.
    /// Undo restores the selection that existed before this programmatic edit.
    /// Uses the current typing style and normalizes CRLF/CR as paragraph breaks.
    /// Returns [`Error::InvalidRange`] for reversed endpoints,
    /// [`Error::InvalidPosition`] for invalid endpoints, or
    /// [`Error::CompositionActive`] during IME preedit.
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
    /// Replaces selected text when the selection is nonempty.
    /// Returns [`Error::CompositionActive`] during IME preedit.
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

    /// Delete the selection, or the preceding whole grapheme or paragraph break.
    /// Document start is a no-op. Returns [`Error::CompositionActive`] during IME preedit.
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

    /// Delete the selection, or the following whole grapheme or paragraph break.
    /// Document end is a no-op. Returns [`Error::CompositionActive`] during IME preedit.
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

    /// Delete the selection or the preceding Unicode word, including intervening whitespace.
    /// At paragraph start, deletes the preceding paragraph break; document start is a no-op.
    /// Returns [`Error::CompositionActive`] during IME preedit.
    pub fn delete_word_backward(&mut self) -> Result<(), Error> {
        self.delete_word(false)
    }

    /// Delete the selection or the following Unicode word, including intervening whitespace.
    /// At paragraph end, deletes the following paragraph break; document end is a no-op.
    /// Returns [`Error::CompositionActive`] during IME preedit.
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

    /// Patch selected text and the typing style, leaving unspecified attributes unchanged.
    /// At a caret, only typing style changes and no history entry is created.
    /// Breaks the current typing group. Returns [`Error::CompositionActive`] during IME preedit.
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
    /// Returns [`Error::InvalidHeadingLevel`] for heading levels outside `1..=6`,
    /// or [`Error::CompositionActive`] during IME preedit.
    pub fn set_paragraph_kind(&mut self, kind: ParagraphKind) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let before = self.state();
        let delta = self
            .document
            .set_kind(self.selected_paragraph_range(), kind)?;
        self.break_history_group();
        if let Some(delta) = delta {
            self.record(vec![delta], before, false);
        }
        Ok(())
    }

    /// Increase selected list item indentation, clamped at 255; body text is unchanged.
    /// Excludes a selection endpoint at the start of the following paragraph.
    /// Returns [`Error::CompositionActive`] during IME preedit.
    pub fn indent_list(&mut self) -> Result<(), Error> {
        self.change_list_indent(true)
    }

    /// Decrease selected list item indentation. Level-zero items become body text.
    /// Excludes a selection endpoint at the start of the following paragraph.
    /// Returns [`Error::CompositionActive`] during IME preedit.
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
            self.record(vec![delta], before, false);
        }
        Ok(())
    }

    /// Moves by logical graphemes/words/paragraphs; visual wrapped-line movement
    /// is supplied by a GUI adapter's layout engine through `set_selection`.
    /// With `extend`, preserves the anchor and moves the focus. Otherwise, backward
    /// and forward grapheme/word commands collapse a selection to its respective edge.
    /// Vertical movement preserves the preferred grapheme column across short paragraphs.
    /// Returns [`Error::CompositionActive`] during IME preedit.
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

    /// Begin transient IME preedit over the current selection and break the typing group.
    /// Repeated calls preserve the existing composition and its replacement selection.
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
    /// Starts a composition if necessary. Invalid or reversed offsets return
    /// [`Error::InvalidCompositionSelection`] without changing existing preedit.
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
    /// Normalizes paragraph breaks and clears preedit. Without an active composition,
    /// replaces the current selection. An empty commit deletes the replacement selection.
    pub fn commit_composition(&mut self, text: &str) -> Result<(), Error> {
        self.break_history_group();
        if let Some(composition) = self.composition.take() {
            self.selection = composition.replacement;
        }
        self.replace_selection(text, false)
    }

    /// Discard IME preedit and restore its replacement selection.
    /// Returns whether a composition was active; document content and history are unchanged.
    pub fn cancel_composition(&mut self) -> bool {
        if let Some(composition) = self.composition.take() {
            self.selection = composition.replacement;
            self.break_history_group();
            true
        } else {
            false
        }
    }

    /// Cancel IME preedit and restore the previous document, selection, and typing style.
    /// Returns whether a retained undo step was applied. The document revision increases
    /// when history changes content, and the step becomes available to redo.
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
        self.redo.push_back(entry);
        true
    }

    /// Cancel IME preedit and replay the next retained undo step.
    /// Returns whether a redo step was applied, restoring its selection and typing style.
    /// The document revision increases when history changes content.
    pub fn redo(&mut self) -> bool {
        self.cancel_composition();
        self.break_history_group();
        let Some(entry) = self.redo.pop_back() else {
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
            let entry = self.undo.pop_front().or_else(|| self.redo.pop_front());
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
