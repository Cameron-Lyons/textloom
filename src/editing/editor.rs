use std::fmt::{self, Debug, Formatter};
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

use super::composition::Composition;
use super::history::{EditState, Group, History, HistoryLimits, OpenTransaction, TransactionLevel};

use crate::model::{Delta, Indent, Replacement};
use crate::types::Direction;
use crate::{
    Document, Error, Fragment, InlineAttribute, InlineStyle, Link, Movement, Paragraph,
    ParagraphKind, Position, SearchOptions, SearchWrap, Selection, SelectionMode, SelectionStyle,
    StylePatch,
};

/// An editing session over a [`Document`]: the document with a selection, a
/// typing style, undo history, a save point, and an optional IME composition.
///
/// Commands correspond to the actions of a rich-text editor: typing, deleting,
/// formatting, links, lists, search and replace, rich copy and paste, and
/// undo. They validate their input first, so a command that returns an error
/// changes nothing. While an IME composition is active, commands that change
/// the document fail with [`Error::CompositionActive`] until the composition
/// is committed or canceled.
///
/// Each command that changes the document records one undo step, except that
/// consecutive typing, Backspaces, or Deletes within one paragraph merge into
/// one step. [`Editor::break_history_group`] ends such a group, and
/// [`Editor::transaction`] records several commands as one step. Undo and redo
/// restore the selection and typing style along with the content.
///
/// The editor works without a window or renderer; the optional adapters
/// connect it to egui, winit, and AccessKit. A clone shares the unchanged
/// paragraphs and has its own history.
///
/// # Examples
///
/// ```
/// use textloom::{Editor, InlineAttribute, Movement, Position, Selection, SelectionMode};
///
/// let mut editor = Editor::from_text("Hello world");
/// editor.set_selection(Selection::new(Position::new(0, 6), Position::new(0, 11)))?;
/// editor.toggle_style(InlineAttribute::Bold)?;
/// assert_eq!(editor.selection_style().bold, Some(true));
///
/// editor.move_cursor(Movement::DocumentEnd, SelectionMode::Move)?;
/// editor.insert_text("!")?;
/// assert_eq!(editor.document().plain_text(), "Hello world!");
/// assert!(editor.undo());
/// assert_eq!(editor.document().plain_text(), "Hello world");
/// # Ok::<(), textloom::Error>(())
/// ```
pub struct Editor {
    document: Document,
    pub(super) selection: Selection,
    typing_style: InlineStyle,
    history: History,
    preferred_column: Option<usize>,
    pub(super) composition: Option<Arc<Composition>>,
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
                    .expect(STYLE_CACHE_LOCK_HEALTHY)
                    .clone(),
            ),
        }
    }
}

// The style cache stays locked only while a selection's spans are scanned and the result is
// stored, which does not panic, so no thread poisons the lock.
const STYLE_CACHE_LOCK_HEALTHY: &str = "no panic occurs while the style cache is locked";

impl Debug for Editor {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Presentation {
    revision: u64,
    selection: Selection,
    typing_style: InlineStyle,
    pub(crate) composing: bool,
}

impl Editor {
    /// Creates an editor for `document` with a caret at its start, empty
    /// history, and the document marked as saved.
    ///
    /// The typing style is that of the document's first grapheme, or the
    /// default style if the first paragraph is empty.
    #[must_use]
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

    /// Creates an editor for plain text, split into paragraphs as by
    /// [`Document::from_text`].
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self::new(Document::from_text(text))
    }

    /// Returns the document being edited.
    #[must_use]
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Returns the selection, whose positions are always valid in the
    /// [document](Editor::document).
    #[must_use]
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// Returns the style that inserted text receives, whether typed, pasted
    /// as plain text, or committed by an IME.
    ///
    /// Selecting sets it to the style of the first selected grapheme, or at a
    /// caret, to [`Document::style_at`] the caret. Formatting commands at a
    /// caret change only this pending style, and moves that leave the
    /// selection unchanged keep it.
    #[must_use]
    pub fn typing_style(&self) -> InlineStyle {
        self.typing_style
    }

    /// Summarizes the formatting of the selection, for a formatting toolbar.
    ///
    /// At a caret, the summary is the [typing style](Editor::typing_style).
    /// For a range, each attribute is uniform only if all selected text
    /// agrees on it; paragraph breaks and empty paragraphs are ignored, and a
    /// selection of only paragraph breaks also reports the typing style. The
    /// scan of a range is cached until the document or selection changes.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "the expect checks an internal invariant: nothing panics while the cache is locked"
    )]
    pub fn selection_style(&self) -> SelectionStyle {
        if self.selection.is_caret() {
            return self.typing_style.into();
        }
        let identity = self.document.content_identity();
        let mut cache = self.style_cache.lock().expect(STYLE_CACHE_LOCK_HEALTHY);
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

    /// Returns the link that a link editor should start from.
    ///
    /// At a caret, this is the link of the grapheme before the caret, or
    /// failing that, after it. For a range, it is the link that all selected
    /// text shares, or `None` if any selected text is unlinked or the links
    /// differ.
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

    /// Returns the indexes of the paragraphs that block commands, such as
    /// [`set_paragraph_kind`](Editor::set_paragraph_kind), change.
    ///
    /// These are the paragraphs the selection touches, except a last
    /// paragraph that a multi-paragraph selection only enters at its start.
    /// The range is never empty.
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

    /// Returns the kind of the first [selected
    /// paragraph](Editor::selected_paragraphs) if all of them share it, for a
    /// block format menu, or `None` if they differ.
    ///
    /// Bullets count as one kind whatever their indents, and so do ordered
    /// items whatever their indents and starts.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "the expect checks an internal invariant: a selection covers a paragraph"
    )]
    pub fn selection_paragraph_kind(&self) -> Option<ParagraphKind> {
        let mut kinds = self.document.paragraphs()[self.selected_paragraphs()]
            .iter()
            .map(|paragraph| paragraph.kind());
        let first = kinds.next().expect("a selection covers a paragraph");
        kinds.all(|kind| same_block(first, kind)).then_some(first)
    }

    /// Returns whether there is a step to undo.
    ///
    /// [`undo`](Editor::undo) still does nothing inside a
    /// [transaction](Editor::transaction).
    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.undo_len() != 0
    }

    /// Returns whether there is an undone step to redo.
    ///
    /// [`redo`](Editor::redo) still does nothing inside a
    /// [transaction](Editor::transaction).
    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.redo_len() != 0
    }

    /// Returns the bounds on retained history.
    #[must_use]
    pub fn history_limits(&self) -> HistoryLimits {
        self.history.limits()
    }

    /// Returns an estimate of the memory that undo and redo steps retain, in
    /// bytes, which [`HistoryLimits::max_bytes`] bounds.
    ///
    /// Each step is charged for the text, formatting runs, and link targets on
    /// the larger side of its change that the other side does not share, plus
    /// the paragraph pointers of both sides. Navigation indexes and allocator
    /// overhead are not counted.
    #[must_use]
    pub fn history_bytes(&self) -> usize {
        self.history.resolved(self.state()).bytes()
    }

    /// Returns the number of steps that [`undo`](Editor::undo) can undo.
    #[must_use]
    pub fn undo_len(&self) -> usize {
        self.history.resolved(self.state()).undo_len()
    }

    /// Returns the number of undone steps that [`redo`](Editor::redo) can
    /// redo.
    #[must_use]
    pub fn redo_len(&self) -> usize {
        self.history.resolved(self.state()).redo_len()
    }

    /// Sets the bounds on retained history and discards steps beyond them.
    ///
    /// The oldest undo steps go first, then the redo steps farthest from the
    /// current state. A zero bound disables history, so later commands record
    /// no steps. Ends the current undo group.
    pub fn set_history_limits(&mut self, limits: HistoryLimits) {
        self.normalize_history();
        self.history.set_limits(limits);
    }

    /// Discards all undo and redo steps and ends the current undo group.
    pub fn clear_history(&mut self) {
        self.normalize_history();
        self.history.clear();
    }

    /// Ends the current undo group, so that the next typing, Backspace, or
    /// Delete starts a new undo step instead of joining the previous one.
    ///
    /// Selection changes, formatting, and other commands end groups
    /// themselves; hosts call this when, for example, the editor loses focus.
    pub fn break_history_group(&mut self) {
        self.normalize_history();
        self.history.break_group();
    }

    pub(crate) fn isolated_edit(
        &mut self,
        edit: impl FnOnce(&mut Self) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.break_history_group();
        let result = edit(self);
        self.break_history_group();
        result
    }

    /// Runs `edit` on this editor as one undo step and returns its result.
    ///
    /// When `edit` returns `Ok`, the document changes of the commands it ran
    /// form a single undo step, in which successive edits to one paragraph
    /// merge. The step is recorded whenever the document changed, even if the
    /// changes cancel out. When `edit` returns `Err` or panics, the document,
    /// selection, typing style, and IME composition return to their state
    /// before the call, and history is unchanged. A transaction inside another
    /// joins the outer step, and its failure reverts only its own edits. While
    /// a transaction is open, [`undo`](Editor::undo) and
    /// [`redo`](Editor::redo) do nothing.
    ///
    /// If `edit` puts another editor in this one's place, commit and rollback
    /// leave that editor as it is. An editor moved out of the callback becomes
    /// an independent snapshot once its transactions end, keeping their edits
    /// as one undo step, like a clone.
    ///
    /// # Errors
    ///
    /// Returns the error that `edit` returns.
    ///
    /// # Examples
    ///
    /// ```
    /// use textloom::{Editor, StylePatch};
    ///
    /// let mut editor = Editor::from_text("draft");
    /// editor.select_all();
    /// editor.transaction(|editor| {
    ///     editor.insert_text("final")?;
    ///     editor.select_all();
    ///     editor.apply_style(StylePatch { bold: Some(true), ..StylePatch::default() })
    /// })?;
    /// assert_eq!(editor.undo_len(), 1);
    /// assert!(editor.undo());
    /// assert_eq!(editor.document().plain_text(), "draft");
    /// # Ok::<(), textloom::Error>(())
    /// ```
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

    /// Records the current content as saved, for
    /// [`is_modified`](Editor::is_modified).
    ///
    /// The save point keeps the saved paragraphs alive, sharing them with the
    /// document.
    pub fn mark_saved(&mut self) {
        self.normalize_history();
        self.saved = SavePoint::new(&self.document);
    }

    /// Returns whether the content differs from the content at the last
    /// [`mark_saved`](Editor::mark_saved), or at creation if it was never
    /// called.
    ///
    /// Content is compared rather than history, so undoing back to the saved
    /// text reads as unmodified. The comparison is cached until the next
    /// change.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        !self.saved.matches(&self.document)
    }

    /// Selects `selection`, keeping its direction.
    ///
    /// Cancels any IME composition and ends the current undo group. A changed
    /// selection takes its typing style from the first selected grapheme, or
    /// at a caret, from [`Document::style_at`]; setting the current selection
    /// again keeps a pending typing style.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPosition`] if the anchor or focus is not a
    /// valid position in the document.
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

    /// Selects the whole document, from its start to its end, as
    /// [`set_selection`](Editor::set_selection) does.
    pub fn select_all(&mut self) {
        let _ = self.set_selection(Selection::new(Position::default(), self.document.end()));
    }

    /// Selects the [word range](Document::word_range_at) at `position`, as a
    /// double click does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPosition`] if `position` is not a valid
    /// position in the document.
    pub fn select_word_at(&mut self, position: Position) -> Result<(), Error> {
        let range = self.document.word_range_at(position)?;
        self.set_selection(Selection::new(range.start, range.end))
    }

    /// Selects the text of paragraph `index`, without its paragraph break, as
    /// a triple click does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPosition`] holding `Position::new(index, 0)` if
    /// there is no paragraph `index`.
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

    /// Returns the selected text as plain text, written as by
    /// [`Document::text`], for a plain-text copy.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "the expect checks an internal invariant: the editor's selection is valid"
    )]
    pub fn selected_text(&self) -> String {
        self.document
            .text(self.selection.range())
            .expect("editor maintains a valid selection")
    }

    /// Returns the selected text with its formatting, links, and paragraph
    /// kinds, captured as by [`Document::fragment`], for a rich copy.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "the expect checks an internal invariant: the editor's selection is valid"
    )]
    pub fn selected_fragment(&self) -> Fragment {
        self.document
            .fragment(self.selection.range())
            .expect("editor maintains a valid selection")
    }

    /// Replaces the selection with `fragment`, as a rich paste does, in one
    /// undo step, and leaves a caret after the inserted content.
    ///
    /// Complete pasted paragraphs keep their kinds, including explicit list
    /// starts, and surviving text at the edges of the selection keeps its
    /// formatting and paragraph kind. A fragment without a paragraph break
    /// takes its source kind only when it fills an empty paragraph or whole
    /// selected paragraphs. A fragment of one empty paragraph changes nothing
    /// at a caret and deletes a range selection.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn insert_fragment(&mut self, fragment: &Fragment) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let before = self.state();
        let Replacement { delta, caret } = self
            .document
            .replace_fragment(self.selection.range(), fragment)?;
        self.selection = Selection::caret(caret);
        self.typing_style = self.document.style_at(caret);
        self.preferred_column = None;
        self.break_history_group();
        self.record(delta, before, Group::None);
        Ok(())
    }

    /// Selects the first match of `query` that starts at or after the end of
    /// the selection, and returns whether there was one.
    ///
    /// Starting from the selection's end skips a selected match, so repeated
    /// calls step through the matches. With [`SearchWrap::Around`], the search continues from
    /// the start of the document when no later match exists. The match is
    /// selected from its start to its end; without a match, the selection
    /// stays as it is. Matching follows [`Document::find`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn find_next(
        &mut self,
        query: &str,
        options: SearchOptions,
        wrap: SearchWrap,
    ) -> Result<bool, Error> {
        self.find_match(query, options, wrap, Direction::Forward)
    }

    /// Selects the last match of `query` that ends at or before the start of
    /// the selection, and returns whether there was one.
    ///
    /// With [`SearchWrap::Around`], the search continues from the end of the document when no
    /// earlier match exists. The match is selected from its start to its end;
    /// without a match, the selection stays as it is. Matching follows
    /// [`Document::find`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn find_previous(
        &mut self,
        query: &str,
        options: SearchOptions,
        wrap: SearchWrap,
    ) -> Result<bool, Error> {
        self.find_match(query, options, wrap, Direction::Backward)
    }

    fn find_match(
        &mut self,
        query: &str,
        options: SearchOptions,
        wrap: SearchWrap,
        direction: Direction,
    ) -> Result<bool, Error> {
        self.ensure_no_composition()?;
        let found =
            self.document
                .find_relative(query, options, self.selection.range(), wrap, direction);
        if let Some(range) = found {
            self.set_selection(Selection::new(range.start, range.end))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Replaces the selection with `replacement` if the selection is a match
    /// of `query`, as a Replace button does, and returns whether it was.
    ///
    /// The selection must be exactly a match that [`Document::find`] reports,
    /// so an overlapping occurrence that the match sequence skips is not
    /// replaced. The replacement takes the style and link of the match's first
    /// grapheme, is one undo step, and leaves a caret after it; follow with
    /// [`find_next`](Editor::find_next) to select the next match.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
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

    /// Replaces every match of `query`, as [`Document::find`] reports them,
    /// with `replacement` in one undo step, and returns the number of matches.
    ///
    /// Each replacement takes the style and link of its match's first
    /// grapheme, and the caret ends after the last one. Without matches,
    /// nothing changes and no step is recorded.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
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
        let Replacement { delta, caret } = self.document.replace_matches(matches, replacement);
        self.selection = Selection::caret(caret);
        self.typing_style = self.document.style_at(caret);
        self.preferred_column = None;
        self.break_history_group();
        self.record(delta, before, Group::None);
    }

    /// Replaces the selection with `text` in the [typing
    /// style](Editor::typing_style), as typing or a plain-text paste does, and
    /// leaves a caret after it.
    ///
    /// LF, CRLF, a lone CR, and U+2029 in `text` become paragraph breaks;
    /// U+2028 stays as a soft line break. New paragraphs continue the kind of
    /// the paragraph they split, except that text after the end of a heading
    /// becomes body text. Text inserted strictly inside a link joins the link.
    /// Consecutive insertions without paragraph breaks in one paragraph form
    /// one undo step.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn insert_text(&mut self, text: &str) -> Result<(), Error> {
        self.ensure_no_composition()?;
        self.replace_selection(text, typing_group(text))
    }

    /// Replaces `range` with `text` in the typing style, as
    /// [`insert_text`](Editor::insert_text) does for the selection, in its own
    /// undo step, and leaves a caret after the inserted text.
    ///
    /// Undo restores the selection from before the call, which need not
    /// overlap `range`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition,
    /// [`Error::InvalidRange`] if `range.start` comes after `range.end`, and
    /// [`Error::InvalidPosition`] if either end is not a valid position.
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
        let Replacement { delta, caret } = self.document.replace_with_kind(
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

    /// Replaces the selection with a paragraph break, as Enter does, in its own
    /// undo step.
    ///
    /// The new paragraph continues the kind of the one it splits, except after
    /// the end of a heading. At a caret in an empty list item, the item is
    /// outdented one level instead, so a top-level item becomes body text,
    /// and an empty quote paragraph becomes body text.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
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

    /// Replaces the selection with a soft line break (U+2028 LINE SEPARATOR),
    /// as Shift+Enter does.
    ///
    /// The break stays inside the paragraph, keeping its kind, and joins the
    /// typing undo step like typed text.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn insert_line_break(&mut self) -> Result<(), Error> {
        self.ensure_no_composition()?;
        self.replace_selection("\u{2028}", Group::Typing)
    }

    /// Deletes the selection, or the grapheme before a caret, as Backspace
    /// does.
    ///
    /// At the start of a list item, it outdents the item one level instead (a
    /// top-level item becomes body text), and at the start of a quote
    /// paragraph it makes the paragraph body text. At the start of another
    /// paragraph, it joins the paragraph to the previous one, and at the start
    /// of the document it does nothing. Consecutive deletions within one
    /// paragraph form one undo step; a deleted paragraph break is its own.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
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

    /// Deletes the selection, or the grapheme after a caret, as Delete does.
    ///
    /// At the end of a paragraph, it joins the next paragraph to it, and at
    /// the end of the document it does nothing. Consecutive deletions within
    /// one paragraph form one undo step; a deleted paragraph break is its own.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
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

    /// Deletes the selection, or from a caret back to where
    /// [`Movement::WordBackward`] would move it, as Ctrl+Backspace does.
    ///
    /// At the start of a paragraph, it deletes the paragraph break before it.
    /// Each deletion is its own undo step.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn delete_word_backward(&mut self) -> Result<(), Error> {
        self.delete_word(Direction::Backward)
    }

    /// Deletes the selection, or from a caret forward to where
    /// [`Movement::WordForward`] would move it, as Ctrl+Delete does.
    ///
    /// At the end of a paragraph, it deletes the paragraph break after it.
    /// Each deletion is its own undo step.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn delete_word_forward(&mut self) -> Result<(), Error> {
        self.delete_word(Direction::Forward)
    }

    /// Deletes the selection, or from a caret back to the start of its
    /// paragraph, in one undo step.
    ///
    /// It never joins paragraphs: at a paragraph start it does nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn delete_to_paragraph_start(&mut self) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if !self.selection.is_caret() {
            return self.replace_selection("", Group::None);
        }
        let focus = self.selection.focus;
        self.delete_to(Position::new(focus.paragraph, 0), Group::None)
    }

    fn delete_word(&mut self, direction: Direction) -> Result<(), Error> {
        self.ensure_no_composition()?;
        if !self.selection.is_caret() {
            return self.replace_selection("", Group::None);
        }
        let focus = self.selection.focus;
        let target = match direction {
            Direction::Forward => self.next_word(focus),
            Direction::Backward => self.previous_word(focus),
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

    /// Applies `patch` to the selected text and to the typing style, in one
    /// undo step.
    ///
    /// At a caret, only the typing style changes, which affects the next
    /// inserted text. A patch that changes no text records no step. Ends the
    /// current undo group.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn apply_style(&mut self, patch: StylePatch) -> Result<(), Error> {
        self.ensure_no_composition()?;
        self.break_history_group();
        let before = self.state();
        let delta = self.document.apply_style(self.selection.range(), patch)?;
        patch.apply(&mut self.typing_style);
        self.record(delta, before, Group::None);
        Ok(())
    }

    /// Toggles `attribute` as the adapters' Ctrl+B, Ctrl+I, and Ctrl+U
    /// shortcuts do: removes it if the [selection
    /// style](Editor::selection_style) has it throughout, and otherwise,
    /// including for mixed selections, applies it.
    ///
    /// Applies the change as [`apply_style`](Editor::apply_style) does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
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

    /// Resets the inline attributes, text color, and fill of the selected text
    /// and of the typing style, like Excel's Clear Formats, in one undo step.
    ///
    /// Links and paragraph kinds are kept. At a caret, only the typing style
    /// is reset.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
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

    /// Links the selected text to `link`, or unlinks it with `None`, in one
    /// undo step, keeping its formatting.
    ///
    /// At a caret, it changes or removes the whole link next to the caret (the
    /// one [`selected_link`](Editor::selected_link) reports), and does nothing
    /// if there is none.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
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

    /// Replaces the selection with `text` linked to `link`, in the typing
    /// style, as one undo step, and leaves a caret after it.
    ///
    /// Empty `text` inserts the link's target as its text.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn insert_link(&mut self, text: &str, link: Link) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let text = if text.is_empty() { link.as_str() } else { text };
        self.break_history_group();
        let before = self.state();
        let Replacement { delta, caret } = self.document.replace_with_link(
            self.selection.range(),
            text,
            self.typing_style,
            Some(&link),
        )?;
        self.finish_replacement(delta, caret, Group::None, before);
        self.break_history_group();
        Ok(())
    }

    /// Sets the kind of the [selected paragraphs](Editor::selected_paragraphs)
    /// to `kind`, in one undo step.
    ///
    /// For an ordered kind, `start` applies only to the first selected
    /// paragraph, and the others continue its list.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition, and
    /// [`Error::InvalidHeadingLevel`] for a heading level outside `1..=6`.
    pub fn set_paragraph_kind(&mut self, kind: ParagraphKind) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let before = self.state();
        let delta = self.document.set_kind(self.selected_paragraphs(), kind)?;
        self.break_history_group();
        self.record(delta, before, Group::None);
        Ok(())
    }

    /// Makes the [selected paragraphs](Editor::selected_paragraphs) ordered
    /// items at `indent`, as a Numbered List command does, in one undo step.
    ///
    /// The items continue a list whose last item at `indent` comes right
    /// before the selection, deeper items aside, and otherwise start a new
    /// list at 1. A first item already ordered at `indent` keeps its start.
    /// When every selected paragraph is already an ordered item at `indent`,
    /// nothing changes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
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

    /// Indents the selected list items by one level, up to 255, in one undo
    /// step, keeping their list starts.
    ///
    /// Other selected paragraphs, including quotes, are unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn indent_list(&mut self) -> Result<(), Error> {
        self.change_list_indent(Indent::Increase)
    }

    /// Outdents the selected list items by one level, in one undo step; items
    /// at level 0 become body text.
    ///
    /// Other selected paragraphs, including quotes, are unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    pub fn outdent_list(&mut self) -> Result<(), Error> {
        self.change_list_indent(Indent::Decrease)
    }

    fn change_list_indent(&mut self, indent: Indent) -> Result<(), Error> {
        self.ensure_no_composition()?;
        let before = self.state();
        let delta = self
            .document
            .change_list_indent(self.selected_paragraphs(), indent);
        self.break_history_group();
        self.record(delta, before, Group::None);
        Ok(())
    }

    /// Moves the caret by `movement`, or with [`SelectionMode::Extend`], moves
    /// only the selection's focus and keeps its anchor, as Shift does.
    ///
    /// With [`SelectionMode::Move`], a grapheme or word movement from a range
    /// selection collapses it to its start (backward movements) or end (forward
    /// movements) instead. A changed selection updates the typing style as
    /// [`set_selection`](Editor::set_selection) does. Ends the current undo
    /// group.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CompositionActive`] during an IME composition.
    #[expect(
        clippy::missing_panics_doc,
        reason = "the expects check an internal invariant: the editor's selection is valid"
    )]
    pub fn move_cursor(&mut self, movement: Movement, mode: SelectionMode) -> Result<(), Error> {
        let extend = mode == SelectionMode::Extend;
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

    /// Undoes the latest step and returns whether there was one.
    ///
    /// Restores the document, selection, and typing style from before the
    /// step and makes it available to [`redo`](Editor::redo). Cancels an IME
    /// composition first, even when there is nothing to undo. Inside a
    /// [transaction](Editor::transaction), it does nothing and returns
    /// `false`.
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

    /// Redoes the latest undone step and returns whether there was one.
    ///
    /// Restores the document, selection, and typing style from after the step.
    /// Any new change discards the undone steps. Cancels an IME composition
    /// first, even when there is nothing to redo. Inside a
    /// [transaction](Editor::transaction), it does nothing and returns
    /// `false`.
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

    pub(crate) fn presentation(&self) -> Presentation {
        Presentation {
            revision: self.document.revision(),
            selection: self.selection,
            typing_style: self.typing_style,
            composing: self.composition.is_some(),
        }
    }

    fn state(&self) -> EditState {
        EditState {
            selection: self.selection,
            style: self.typing_style,
        }
    }
    pub(super) fn normalize_history(&mut self) {
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

    pub(super) fn replace_selection(&mut self, text: &str, group: Group) -> Result<(), Error> {
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
        let Replacement { delta, caret } = self.document.replace(range, text, style)?;
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

pub(super) fn typing_group(text: &str) -> Group {
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

struct TransactionGuard<'editor> {
    editor: &'editor mut Editor,
    level: TransactionLevel,
    _scope: Arc<()>,
    rollback: Option<Rollback>,
}

struct Rollback {
    state: EditState,
    preferred_column: Option<usize>,
    composition: Option<Arc<Composition>>,
}

impl<'editor> TransactionGuard<'editor> {
    fn begin(editor: &'editor mut Editor) -> Self {
        let state = editor.state();
        let OpenTransaction { level, scope } = editor.history.begin_transaction(state);
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
