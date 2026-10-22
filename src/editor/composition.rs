use std::ops::Range;
use std::sync::Arc;

use super::{Editor, typing_group};
use crate::{Error, Selection};

/// An active IME composition, as [`Editor::composition`] reports it.
///
/// The preedit text is transient: it is not part of the document or of
/// history. [`Editor::commit_composition`] replaces the `replacement` selection with the
/// committed text, and [`Editor::cancel_composition`] discards the preedit and
/// restores that selection. Fields may be added in minor releases.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Composition {
    /// The uncommitted text that the input method is composing.
    pub text: String,
    /// The input method's cursor or selection in `text`, as a byte range on
    /// UTF-8 character boundaries (not necessarily grapheme boundaries, since
    /// the preedit may hold unfinished graphemes), or `None` if it gave none.
    pub selection: Option<Range<usize>>,
    /// The editor selection when the composition began, which the committed
    /// text replaces.
    pub replacement: Selection,
}

impl Editor {
    /// Returns the active IME composition, or `None` if there is none.
    #[must_use]
    pub fn composition(&self) -> Option<&Composition> {
        self.composition.as_deref()
    }
    #[cfg(feature = "egui")]
    pub(crate) fn composition_snapshot(&self) -> Option<Arc<Composition>> {
        self.composition.clone()
    }
    /// Starts an IME composition with empty preedit text that will replace
    /// the current selection, unless a composition is already active.
    ///
    /// Commands that change the document fail with
    /// [`Error::CompositionActive`] until the composition is committed or
    /// canceled.
    pub fn begin_composition(&mut self) {
        self.normalize_history();
        self.active_composition_mut();
    }

    // Returns the active composition, beginning one at the selection if there is none.
    fn active_composition_mut(&mut self) -> &mut Arc<Composition> {
        let replacement = self.selection;
        self.composition.get_or_insert_with(|| {
            Arc::new(Composition {
                text: String::new(),
                selection: None,
                replacement,
            })
        })
    }

    /// Sets the preedit text and the input method's selection in it, first
    /// [beginning a composition](Editor::begin_composition) if none is active.
    ///
    /// The document does not change. An update equal to the current preedit
    /// keeps the existing [`Composition`] snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCompositionSelection`] if `selection` is
    /// reversed, ends past `text`, or splits a UTF-8 character. The composition
    /// stays as it was.
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
        self.normalize_history();
        let composition = self.active_composition_mut();
        let text_changed = composition.text != text;
        if text_changed || composition.selection != selection {
            if let Some(composition) = Arc::get_mut(composition) {
                if text_changed {
                    composition.text.clear();
                    composition.text.push_str(text);
                }
                composition.selection = selection;
            } else {
                *composition = Arc::new(Composition {
                    text: text.to_owned(),
                    selection,
                    replacement: composition.replacement,
                });
            }
        }
        Ok(())
    }

    /// Ends the composition and replaces its replacement selection with
    /// `text`, as typing does; without a composition, replaces the current
    /// selection.
    ///
    /// The committed text may differ from the last preedit. Commits without
    /// paragraph breaks join adjacent typing in one undo step, so
    /// syllable-by-syllable input undoes at once. An empty commit deletes the
    /// replacement selection.
    ///
    /// # Errors
    ///
    /// This does not fail in practice: the replacement selection stays valid
    /// because commands that would change the document are rejected during a
    /// composition.
    pub fn commit_composition(&mut self, text: &str) -> Result<(), Error> {
        self.normalize_history();
        if let Some(composition) = self.composition.take() {
            self.selection = composition.replacement;
        }
        self.replace_selection(text, typing_group(text))
    }

    /// Discards the composition, restores the selection from before it
    /// began, and returns whether a composition was active.
    ///
    /// Ends the current undo group when a composition was active.
    pub fn cancel_composition(&mut self) -> bool {
        self.normalize_history();
        if let Some(composition) = self.composition.take() {
            self.selection = composition.replacement;
            self.break_history_group();
            true
        } else {
            false
        }
    }

    pub(super) fn ensure_no_composition(&mut self) -> Result<(), Error> {
        self.normalize_history();
        if self.composition.is_some() {
            Err(Error::CompositionActive)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Position;

    #[test]
    fn retained_composition_snapshots_preserve_text_selection_and_replacement() {
        let mut editor = Editor::from_text("abc");
        let replacement = Selection::new(Position::new(0, 3), Position::new(0, 1));
        editor.set_selection(replacement).unwrap();
        editor.update_composition("é", Some(0..2)).unwrap();
        let first = Arc::clone(editor.composition.as_ref().unwrap());
        assert_eq!(
            editor.update_composition("x", Some(0..2)),
            Err(Error::InvalidCompositionSelection)
        );
        editor.update_composition("é", Some(0..2)).unwrap();
        assert!(Arc::ptr_eq(&first, editor.composition.as_ref().unwrap()));

        editor.update_composition("é", Some(2..2)).unwrap();
        let moved = Arc::clone(editor.composition.as_ref().unwrap());
        assert_eq!(first.selection, Some(0..2));
        assert_eq!(moved.selection, Some(2..2));
        assert_eq!(first.text, "é");
        assert_eq!(moved.text, "é");
        assert!(!Arc::ptr_eq(&first, &moved));

        editor.update_composition("日本", None).unwrap();
        let changed = Arc::clone(editor.composition.as_ref().unwrap());
        assert_eq!(changed.text, "日本");
        for snapshot in [&first, &moved, &changed] {
            assert_eq!(snapshot.replacement, replacement, "{snapshot:?}");
        }
        editor.commit_composition("日本語").unwrap();
        assert_eq!(editor.document().plain_text(), "a日本語");
        assert_eq!(changed.text, "日本");
        assert!(editor.undo());
        assert_eq!(editor.selection(), replacement);
        assert_eq!(editor.document().plain_text(), "abc");

        editor.update_composition("canceled", None).unwrap();
        let canceled = Arc::clone(editor.composition.as_ref().unwrap());
        assert!(editor.cancel_composition());
        assert_eq!(canceled.text, "canceled");
        assert_eq!(canceled.replacement, replacement);
        assert_eq!(first.text, "é");
        assert_eq!(moved.selection, Some(2..2));
    }

    #[test]
    fn exclusive_composition_updates_reuse_text_storage() {
        let mut editor = Editor::default();
        editor
            .update_composition("café 👩\u{200d}💻", None)
            .unwrap();
        let storage = editor.composition().unwrap().text.as_ptr();
        for text in ["café 👩\u{200d}💻", "café", "café 👩\u{200d}💻"] {
            editor.update_composition(text, None).unwrap();
            assert_eq!(
                editor.composition().unwrap().text.as_ptr(),
                storage,
                "{text:?}"
            );
            assert_eq!(editor.composition().unwrap().text, text);
        }
        assert_eq!(editor.document().plain_text(), "");
        assert!(!editor.can_undo());
    }
}
