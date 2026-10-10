use std::{ops::Range, sync::Arc};

use super::Editor;
use crate::{Error, Selection};

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

impl Editor {
    /// Current transient IME preedit, or `None` when no composition is active.
    pub fn composition(&self) -> Option<&Composition> {
        self.composition.as_deref()
    }
    /// Retain immutable preedit state without copying text on each GUI frame.
    #[cfg(feature = "egui")]
    pub(crate) fn composition_snapshot(&self) -> Option<Arc<Composition>> {
        self.composition.clone()
    }
    /// Begin transient IME preedit over the current selection and break the typing group.
    /// Repeated calls preserve the existing composition and its replacement selection.
    pub fn begin_composition(&mut self) {
        if self.composition.is_none() {
            self.break_history_group();
            self.composition = Some(Arc::new(Composition {
                text: String::new(),
                selection: None,
                replacement: self.selection,
            }));
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
        let text_changed = composition.text != text;
        if text_changed || composition.selection != selection {
            if let Some(composition) = Arc::get_mut(composition) {
                if text_changed {
                    composition.text.clear();
                    composition.text.push_str(text);
                }
                composition.selection = selection;
            } else {
                // Retained snapshots keep the previous value. Copy incoming
                // text directly instead of cloning text that will be replaced.
                *composition = Arc::new(Composition {
                    text: text.to_owned(),
                    selection,
                    replacement: composition.replacement,
                });
            }
        }
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

    pub(super) fn ensure_no_composition(&self) -> Result<(), Error> {
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
            assert_eq!(snapshot.replacement, replacement);
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
        editor.update_composition("café 👩‍💻", None).unwrap();
        let storage = editor.composition().unwrap().text.as_ptr();
        for text in ["café 👩‍💻", "café", "café 👩‍💻"] {
            editor.update_composition(text, None).unwrap();
            assert_eq!(editor.composition().unwrap().text.as_ptr(), storage);
            assert_eq!(editor.composition().unwrap().text, text);
        }
        assert_eq!(editor.document().plain_text(), "");
        assert!(!editor.can_undo());
    }
}
