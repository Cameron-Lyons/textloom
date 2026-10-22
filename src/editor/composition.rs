use std::ops::Range;
use std::sync::Arc;

use super::{Editor, typing_group};
use crate::{Error, Selection};

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Composition {
    pub text: String,
    pub selection: Option<Range<usize>>,
    pub replacement: Selection,
}

impl Editor {
    pub fn composition(&self) -> Option<&Composition> {
        self.composition.as_deref()
    }
    #[cfg(feature = "egui")]
    pub(crate) fn composition_snapshot(&self) -> Option<Arc<Composition>> {
        self.composition.clone()
    }
    pub fn begin_composition(&mut self) {
        self.normalize_history();
        if self.composition.is_none() {
            self.composition = Some(Arc::new(Composition {
                text: String::new(),
                selection: None,
                replacement: self.selection,
            }));
        }
    }

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
                *composition = Arc::new(Composition {
                    text: text.to_owned(),
                    selection,
                    replacement: composition.replacement,
                });
            }
        }
        Ok(())
    }

    pub fn commit_composition(&mut self, text: &str) -> Result<(), Error> {
        self.normalize_history();
        if let Some(composition) = self.composition.take() {
            self.selection = composition.replacement;
        }
        self.replace_selection(text, typing_group(text))
    }

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
        editor
            .update_composition("café 👩\u{200d}💻", None)
            .unwrap();
        let storage = editor.composition().unwrap().text.as_ptr();
        for text in ["café 👩\u{200d}💻", "café", "café 👩\u{200d}💻"] {
            editor.update_composition(text, None).unwrap();
            assert_eq!(editor.composition().unwrap().text.as_ptr(), storage);
            assert_eq!(editor.composition().unwrap().text, text);
        }
        assert_eq!(editor.document().plain_text(), "");
        assert!(!editor.can_undo());
    }
}
