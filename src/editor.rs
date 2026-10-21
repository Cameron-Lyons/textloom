use std::{
    fmt,
    ops::Range,
    sync::{Arc, Mutex, OnceLock, PoisonError},
};

mod composition;
mod history;

pub use composition::Composition;
pub use history::HistoryLimits;
use history::{EditState, Group, History, TransactionLevel};

use crate::document::Delta;
use crate::{
    Document, Error, Fragment, InlineAttribute, InlineStyle, Link, Movement, Paragraph,
    ParagraphKind, Position, SearchOptions, Selection, SelectionStyle, StylePatch,
};

pub struct Editor {
    document: Document,
    selection: Selection,
    typing_style: InlineStyle,
    history: History,
    preferred_column: Option<usize>,
    composition: Option<Arc<Composition>>,
    saved: SavePoint,
    style_cache: Mutex<Option<StyleCache>>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new(Document::new())
    }
}

impl Clone for Editor {
    fn clone(&self) -> Self {
        let mut history = self.history.clone();
        history.close_transactions(self.state());
        Self {
            document: self.document.clone(),
            selection: self.selection,
            typing_style: self.typing_style,
            history,
            preferred_column: self.preferred_column,
            composition: self.composition.clone(),
            saved: self.saved.clone(),
            style_cache: Mutex::new(
                self.style_cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
        }
    }
}

impl fmt::Debug for Editor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Editor")
            .field("paragraphs", &self.document.paragraphs().len())
            .field("revision", &self.document.revision())
            .field("selection", &self.selection)
            .field("typing_style", &self.typing_style)
            .field("composition", &self.composition())
            .field("undo_len", &self.undo_len())
            .field("redo_len", &self.redo_len())
            .field("history_bytes", &self.history_bytes())
            .field("history_limits", &self.history_limits())
            .field("modified", &self.is_modified())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct SavePoint {
    revision: u64,
    paragraphs: Vec<Arc<Paragraph>>,
    // Edits and history/transaction restores invalidate this content comparison.
    cached_match: OnceLock<bool>,
}

impl SavePoint {
    fn new(document: &Document) -> Self {
        Self {
            revision: document.revision(),
            paragraphs: document.paragraphs().to_vec(),
            cached_match: OnceLock::new(),
        }
    }

    fn matches(&self, document: &Document) -> bool {
        (self.revision == document.revision() && self.revision != u64::MAX)
            || *self
                .cached_match
                .get_or_init(|| self.paragraphs == document.paragraphs())
    }
}

#[derive(Clone)]
struct StyleCache {
    identity: Arc<()>,
    selection: Selection,
    style: Option<SelectionStyle>,
}

impl Editor {
    pub fn new(document: Document) -> Self {
        Self {
            typing_style: document.style_at(Position::default()),
            saved: SavePoint::new(&document),
            document,
            selection: Selection::default(),
            history: History::default(),
            preferred_column: None,
            composition: None,
            style_cache: Mutex::new(None),
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
    #[must_use]
    pub fn selection_style(&self) -> SelectionStyle {
        if self.selection.is_caret() {
            return self.typing_style.into();
        }
        let identity = self.document.content_identity();
        let mut cache = self
            .style_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let style = match cache.as_ref() {
            Some(cached)
                if Arc::ptr_eq(&cached.identity, &identity)
                    && cached.selection == self.selection =>
            {
                cached.style
            }
            _ => {
                let style = self.scan_selection_style();
                *cache = Some(StyleCache {
                    identity,
                    selection: self.selection,
                    style,
                });
                style
            }
        };
        style.unwrap_or_else(|| self.typing_style.into())
    }
    fn scan_selection_style(&self) -> Option<SelectionStyle> {
        let mut result: Option<SelectionStyle> = None;
        for span in self.document.spans_in(&self.selection.range()) {
            if let Some(style) = &mut result {
                style.include(span.style);
                if style.is_fully_mixed() {
                    return result;
                }
            } else {
                result = Some(span.style.into());
            }
        }
        result
    }
    #[must_use]
    pub fn selected_link(&self) -> Option<Link> {
        if self.selection.is_caret() {
            return self.document.caret_link(self.selection.focus).cloned();
        }
        let mut shared: Option<&Link> = None;
        for span in self.document.spans_in(&self.selection.range()) {
            let link = span.link.as_ref()?;
            if shared.is_some_and(|shared| shared != link) {
                return None;
            }
            shared = Some(link);
        }
        shared.cloned()
    }
    #[must_use]
    pub fn selected_paragraphs(&self) -> Range<usize> {
        let range = self.selection.range();
        let end = if range.end.paragraph > range.start.paragraph && range.end.byte == 0 {
            range.end.paragraph
        } else {
            range.end.paragraph + 1
        };
        range.start.paragraph..end
    }
    #[must_use]
    pub fn selection_paragraph_kind(&self) -> Option<ParagraphKind> {
        let mut kinds = self.document.paragraphs()[self.selected_paragraphs()]
            .iter()
            .map(|paragraph| paragraph.kind());
        let first = kinds.next().expect("a selection covers a paragraph");
        kinds.all(|kind| same_block(first, kind)).then_some(first)
    }
    pub fn can_undo(&self) -> bool {
        self.undo_len() != 0
    }
    pub fn can_redo(&self) -> bool {
        self.redo_len() != 0
    }
    pub fn history_limits(&self) -> HistoryLimits {
        self.history.limits()
    }
    pub fn history_bytes(&self) -> usize {
        self.history.resolved(self.state()).bytes()
    }
    pub fn undo_len(&self) -> usize {
        self.history.resolved(self.state()).undo_len()
    }
    pub fn redo_len(&self) -> usize {
        self.history.resolved(self.state()).redo_len()
    }

    pub fn set_history_limits(&mut self, limits: HistoryLimits) {
        self.normalize_history();
        self.history.set_limits(limits);
    }

    pub fn clear_history(&mut self) {
        self.normalize_history();
        self.history.clear();
    }

    pub fn break_history_group(&mut self) {
        self.normalize_history();
        self.history.break_group();
    }

    pub fn transaction<R>(
        &mut self,
        edit: impl FnOnce(&mut Self) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let transaction = TransactionGuard::begin(self);
        let result = edit(&mut *transaction.editor);
        if result.is_ok() {
            transaction.commit();
        }
        result
    }

    pub fn mark_saved(&mut self) {
        self.normalize_history();
        self.saved = SavePoint::new(&self.document);
    }

    #[must_use]
    pub fn is_modified(&self) -> bool {
        !self.saved.matches(&self.document)
    }

    pub fn set_selection(&mut self, selection: Selection) -> Result<(), Error> {
        self.document.validate_position(selection.anchor)?;
        self.document.validate_position(selection.focus)?;
        self.normalize_history();
        self.composition = None;
        if selection != self.selection {
            self.selection = selection;
            self.typing_style = self.selection_typing_style(selection);
        }
        self.preferred_column = None;
        self.break_history_group();
        Ok(())
    }

    pub fn select_all(&mut self) {
        let _ = self.set_selection(Selection::new(Position::default(), self.document.end()));
    }

    pub fn select_word_at(&mut self, position: Position) -> Result<(), Error> {
        let range = self.document.word_range_at(position)?;
        self.set_selection(Selection::new(range.start, range.end))
    }

    pub fn select_paragraph(&mut self, index: usize) -> Result<(), Error> {
        let end = self
            .document
            .paragraph(index)
            .ok_or(Error::InvalidPosition(Position::new(index, 0)))?
            .text()
            .len();
        self.set_selection(Selection::new(
            Position::new(index, 0),
            Position::new(index, end),
        ))
    }

    #[must_use]
    pub fn selected_text(&self) -> String {
        self.document
            .text(self.selection.range())
            .expect("editor maintains a valid selection")
    }

    #[must_use]
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
        self.record(delta, before, Group::None);
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

    pub fn replace_selected_match(
        &mut self,
        query: &str,
        replacement: &str,
        options: SearchOptions,
    ) -> Result<bool, Error> {
        self.ensure_no_composition()?;
        let range = self.selection.range();
        if !self.document.is_match(query, options, &range) {
            return Ok(false);
        }
        self.replace_matches(&[range], replacement);
        Ok(true)
    }

    pub fn replace_all(
        &mut self,
        query: &str,
        replacement: &str,
        options: SearchOptions,
    ) -> Result<usize, Error> {
        self.ensure_no_composition()?;
        let matches = self.document.find(query, options);
        if !matches.is_empty() {
            self.replace_matches(&matches, replacement);
        }
        Ok(matches.len())
    }

    fn replace_matches(&mut self, matches: &[Range<Position>], replacement: &str) {
        let before = self.state();
        let (delta, caret) = self.document.replace_matches(matches, replacement);
        self.selection = Selection::caret(caret);
        self.typing_style = self.document.style_at(caret);
        self.preferred_column = None;
        self.break_history_group();
        self.record(delta, before, Group::None);
    }

    pub fn insert_text(&mut self, text: &str) -> Result<(), Error> {
        self.ensure_no_composition()?;
        self.replace_selection(text, typing_group(text))
    }

    pub fn replace_range(&mut self, range: Range<Position>, text: &str) -> Result<(), Error> {
        self.replace_range_with_style(range, text, self.typing_style)
    }

    pub(crate) fn replace_range_with_style(
        &mut self,
        range: Range<Position>,
        text: &str,
        style: InlineStyle,
    ) -> Result<(), Error> {
        self.ensure_no_composition()?;
        self.replace_range_inner(range, text, style, Group::None)
    }

    pub(crate) fn replace_all_text(&mut self, text: &str) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let before = self.state();
        let start = Position::default();
        let style = self.document.style_at(start);
        let kind = self.document.paragraphs()[0].kind();
        let (delta, caret) = self.document.replace_with_kind(
            start..self.document.end(),
            text,
            style,
            None,
            Some(kind),
        )?;
        self.typing_style = style;
        self.finish_replacement(delta, caret, Group::None, before);
        Ok(())
    }

    pub fn insert_paragraph(&mut self) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if self
            .caret_block_item()
            .is_some_and(|paragraph| paragraph.text().is_empty())
        {
            return self.leave_block_item();
        }
        self.replace_selection("\n", Group::None)
    }

    pub fn insert_line_break(&mut self) -> Result<(), Error> {
        self.ensure_no_composition()?;
        self.replace_selection("\u{2028}", Group::Typing)
    }

    pub fn delete_backward(&mut self) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if !self.selection.is_caret() {
            return self.replace_selection("", Group::None);
        }
        if self.selection.focus.byte == 0 && self.caret_block_item().is_some() {
            return self.leave_block_item();
        }
        self.delete_to(
            self.previous_grapheme(self.selection.focus),
            Group::DeleteBackward,
        )
    }

    pub fn delete_forward(&mut self) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if !self.selection.is_caret() {
            return self.replace_selection("", Group::None);
        }
        self.delete_to(
            self.next_grapheme(self.selection.focus),
            Group::DeleteForward,
        )
    }

    pub fn delete_word_backward(&mut self) -> Result<(), Error> {
        self.delete_word(false)
    }

    pub fn delete_word_forward(&mut self) -> Result<(), Error> {
        self.delete_word(true)
    }

    pub fn delete_to_paragraph_start(&mut self) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if !self.selection.is_caret() {
            return self.replace_selection("", Group::None);
        }
        let focus = self.selection.focus;
        self.delete_to(Position::new(focus.paragraph, 0), Group::None)
    }

    fn delete_word(&mut self, forward: bool) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if !self.selection.is_caret() {
            return self.replace_selection("", Group::None);
        }
        let focus = self.selection.focus;
        let target = if forward {
            self.next_word(focus)
        } else {
            self.previous_word(focus)
        };
        self.delete_to(target, Group::None)
    }

    fn delete_to(&mut self, target: Position, group: Group) -> Result<(), Error> {
        let focus = self.selection.focus;
        if target == focus {
            return Ok(());
        }
        let group = if target.paragraph == focus.paragraph {
            group
        } else {
            Group::None
        };
        let range = focus.min(target)..focus.max(target);
        self.replace_range_inner(range, "", self.typing_style, group)
    }

    pub fn apply_style(&mut self, patch: StylePatch) -> Result<(), Error> {
        self.ensure_no_composition()?;
        self.break_history_group();
        let before = self.state();
        let delta = self.document.apply_style(self.selection.range(), patch)?;
        patch.apply(&mut self.typing_style);
        self.record(delta, before, Group::None);
        Ok(())
    }

    pub fn toggle_style(&mut self, attribute: InlineAttribute) -> Result<(), Error> {
        let style = self.selection_style();
        let current = match attribute {
            InlineAttribute::Bold => style.bold,
            InlineAttribute::Italic => style.italic,
            InlineAttribute::Underline => style.underline,
            InlineAttribute::Strikethrough => style.strikethrough,
            InlineAttribute::Code => style.code,
        };
        let value = Some(current != Some(true));
        let mut patch = StylePatch::default();
        match attribute {
            InlineAttribute::Bold => patch.bold = value,
            InlineAttribute::Italic => patch.italic = value,
            InlineAttribute::Underline => patch.underline = value,
            InlineAttribute::Strikethrough => patch.strikethrough = value,
            InlineAttribute::Code => patch.code = value,
        }
        self.apply_style(patch)
    }

    pub fn clear_formatting(&mut self) -> Result<(), Error> {
        self.apply_style(StylePatch {
            bold: Some(false),
            italic: Some(false),
            underline: Some(false),
            strikethrough: Some(false),
            code: Some(false),
            foreground: Some(None),
            background: Some(None),
        })
    }

    pub fn set_link(&mut self, link: Option<Link>) -> Result<(), Error> {
        self.ensure_no_composition()?;
        self.break_history_group();
        let range = if self.selection.is_caret() {
            match self.document.caret_link_range(self.selection.focus) {
                Some(range) => range,
                None => return Ok(()),
            }
        } else {
            self.selection.range()
        };
        let before = self.state();
        let delta = self.document.set_link(range, link.as_ref())?;
        self.record(delta, before, Group::None);
        Ok(())
    }

    pub fn insert_link(&mut self, text: &str, link: Link) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let text = if text.is_empty() { link.as_str() } else { text };
        self.break_history_group();
        let before = self.state();
        let (delta, caret) = self.document.replace_with_link(
            self.selection.range(),
            text,
            self.typing_style,
            Some(&link),
        )?;
        self.finish_replacement(delta, caret, Group::None, before);
        self.break_history_group();
        Ok(())
    }

    pub fn set_paragraph_kind(&mut self, kind: ParagraphKind) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let before = self.state();
        let delta = self.document.set_kind(self.selected_paragraphs(), kind)?;
        self.break_history_group();
        self.record(delta, before, Group::None);
        Ok(())
    }

    pub fn apply_numbered_list(&mut self, indent: u8) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let before = self.state();
        let delta = self
            .document
            .apply_numbered_list(self.selected_paragraphs(), indent);
        self.break_history_group();
        self.record(delta, before, Group::None);
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
            .change_list_indent(self.selected_paragraphs(), increase);
        self.break_history_group();
        self.record(delta, before, Group::None);
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
        let previous = self.selection;
        let focus = previous.focus;
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
        if self.selection != previous {
            self.typing_style = self.selection_typing_style(self.selection);
        }
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        self.normalize_history();
        if self.history.in_transaction() {
            return false;
        }
        self.cancel_composition();
        self.break_history_group();
        let Some(state) = self.history.undo(&mut self.document) else {
            return false;
        };
        self.restore(state);
        true
    }

    pub fn redo(&mut self) -> bool {
        self.normalize_history();
        if self.history.in_transaction() {
            return false;
        }
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
    fn normalize_history(&mut self) {
        self.history.normalize(self.state());
    }

    fn restore(&mut self, state: EditState) {
        self.saved.cached_match.take();
        self.selection = state.selection;
        self.typing_style = state.style;
        self.preferred_column = None;
    }

    fn selection_typing_style(&self, selection: Selection) -> InlineStyle {
        if !selection.is_caret()
            && let Some(span) = self.document.spans_in(&selection.range()).next()
        {
            return span.style;
        }
        self.document.style_at(selection.focus)
    }

    fn replace_selection(&mut self, text: &str, group: Group) -> Result<(), Error> {
        self.replace_range_inner(self.selection.range(), text, self.typing_style, group)
    }

    fn replace_range_inner(
        &mut self,
        range: Range<Position>,
        text: &str,
        style: InlineStyle,
        group: Group,
    ) -> Result<(), Error> {
        let before = self.state();
        let (delta, caret) = self.document.replace(range, text, style)?;
        self.typing_style = style;
        self.finish_replacement(delta, caret, group, before);
        Ok(())
    }

    fn finish_replacement(
        &mut self,
        delta: Delta,
        caret: Position,
        group: Group,
        before: EditState,
    ) {
        self.selection = Selection::caret(caret);
        self.preferred_column = None;
        self.record(delta, before, group);
    }

    fn record(&mut self, delta: Delta, before: EditState, group: Group) {
        if delta.is_empty() {
            return;
        }
        self.saved.cached_match.take();
        self.history.record(delta, before, self.state(), group);
    }

    fn paragraph_text(&self, paragraph: usize) -> &str {
        self.document
            .paragraph(paragraph)
            .expect("valid paragraph")
            .text()
    }

    fn caret_block_item(&self) -> Option<&Paragraph> {
        if !self.selection.is_caret() {
            return None;
        }
        let paragraph = self
            .document
            .paragraph(self.selection.focus.paragraph)
            .expect("valid selection");
        matches!(
            paragraph.kind(),
            ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. } | ParagraphKind::Quote
        )
        .then_some(paragraph)
    }

    fn leave_block_item(&mut self) -> Result<(), Error> {
        if self.caret_block_item().map(Paragraph::kind) == Some(ParagraphKind::Quote) {
            self.set_paragraph_kind(ParagraphKind::Body)
        } else {
            self.outdent_list()
        }
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

fn typing_group(text: &str) -> Group {
    if text.is_empty() || text.contains(['\n', '\r', '\u{2029}']) {
        Group::None
    } else {
        Group::Typing
    }
}

fn same_block(first: ParagraphKind, other: ParagraphKind) -> bool {
    match (first, other) {
        (ParagraphKind::Bullet { .. }, ParagraphKind::Bullet { .. })
        | (ParagraphKind::Ordered { .. }, ParagraphKind::Ordered { .. }) => true,
        _ => first == other,
    }
}

struct TransactionGuard<'a> {
    editor: &'a mut Editor,
    level: TransactionLevel,
    _scope: Arc<()>,
    rollback: Option<Rollback>,
}

struct Rollback {
    state: EditState,
    preferred_column: Option<usize>,
    composition: Option<Arc<Composition>>,
}

impl<'a> TransactionGuard<'a> {
    fn begin(editor: &'a mut Editor) -> Self {
        let state = editor.state();
        let (level, scope) = editor.history.begin_transaction(state);
        Self {
            level,
            _scope: scope,
            rollback: Some(Rollback {
                state,
                preferred_column: editor.preferred_column,
                composition: editor.composition.clone(),
            }),
            editor,
        }
    }

    fn commit(mut self) {
        self.rollback = None;
        self.editor.normalize_history();
        let after = self.editor.state();
        self.editor.history.commit_transaction(self.level, after);
    }
}

impl Drop for TransactionGuard<'_> {
    fn drop(&mut self) {
        if let Some(rollback) = self.rollback.take() {
            let editor = &mut *self.editor;
            editor.normalize_history();
            if !editor
                .history
                .rollback_transaction(self.level, &mut editor.document)
            {
                return;
            }
            editor.restore(rollback.state);
            editor.preferred_column = rollback.preferred_column;
            editor.composition = rollback.composition;
        }
    }
}
