//! A read-only, toolkit-neutral view of an editor for assistive technology,
//! with grapheme-based positions.

use crate::{Composition, Document, Editor, Error, ParagraphKind, Position, Selection, Span};

/// A caret position counted in extended grapheme clusters within its paragraph,
/// instead of in the UTF-8 bytes of a [`Position`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "an accessible position is exactly a paragraph index and a grapheme offset, like `Position`"
)]
pub struct AccessiblePosition {
    /// The zero-based index of the paragraph in the document.
    pub paragraph: usize,
    /// The number of grapheme clusters in the paragraph's text before the
    /// position.
    pub grapheme: usize,
}

/// A directional selection whose endpoints are [`AccessiblePosition`]s, the
/// grapheme-based counterpart of a [`Selection`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "an accessible selection is exactly an anchor and a focus, like `Selection`"
)]
pub struct AccessibleSelection {
    /// The endpoint that stays in place when the selection is extended.
    pub anchor: AccessiblePosition,
    /// The endpoint that moves when the selection is extended, where the caret is.
    pub focus: AccessiblePosition,
}

/// One paragraph of an [`AccessibilitySnapshot`], borrowed from the document.
#[non_exhaustive]
#[derive(Clone, Copy, Debug)]
pub struct AccessibleParagraph<'document> {
    /// The zero-based index of the paragraph in the document.
    pub index: usize,
    /// The paragraph's text, without a paragraph separator. Soft line breaks are
    /// U+2028 LINE SEPARATOR characters.
    pub text: &'document str,
    /// Whether the paragraph is body text, a heading, a list item, or a quote, for
    /// the host to map to an accessible role.
    pub kind: ParagraphKind,
    /// The inline styles and links of [`text`](Self::text): contiguous, nonempty
    /// spans in order, with UTF-8 byte ranges that together cover the text. An
    /// empty paragraph has none.
    pub spans: &'document [Span],
}

/// Whether the host gives an editor keyboard focus, for
/// [`AccessibilitySnapshot::new`] and the AccessKit adapter's `update`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[expect(
    clippy::exhaustive_enums,
    reason = "an editor either has keyboard focus or does not"
)]
pub enum Focus {
    /// The editor has keyboard focus.
    Focused,
    /// The editor does not have keyboard focus.
    Unfocused,
}

/// A read-only view of an [`Editor`] for describing it to assistive technology.
///
/// The snapshot borrows the editor, so its contents always match the editor, and
/// the editor cannot change until the snapshot is dropped. Take a new snapshot
/// after each change.
#[derive(Clone, Copy, Debug)]
pub struct AccessibilitySnapshot<'editor> {
    label: &'editor str,
    focused: bool,
    selection: Selection,
    composition: Option<&'editor Composition>,
    revision: u64,
    document: &'editor Document,
}

impl<'editor> AccessibilitySnapshot<'editor> {
    /// Takes a snapshot of `editor`, with `label` as its accessible name and
    /// `focus` saying whether the host considers it focused.
    #[must_use]
    pub fn new(editor: &'editor Editor, label: &'editor str, focus: Focus) -> Self {
        Self {
            label,
            focused: focus == Focus::Focused,
            selection: editor.selection(),
            composition: editor.composition(),
            revision: editor.document().revision(),
            document: editor.document(),
        }
    }

    /// The accessible name that the host gave the editor.
    #[must_use]
    pub fn label(&self) -> &'editor str {
        self.label
    }

    /// Whether the host considered the editor focused when it took the snapshot.
    #[must_use]
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// The directional selection, in UTF-8 byte positions.
    /// [`selection_graphemes`](Self::selection_graphemes) gives it in grapheme
    /// positions.
    #[must_use]
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// The IME composition in progress, if any. Its preedit text is not part of
    /// [`paragraphs`](Self::paragraphs).
    #[must_use]
    pub fn composition(&self) -> Option<&'editor Composition> {
        self.composition
    }

    /// The document's [`revision`](Document::revision), which advances with every
    /// change to its content, including undo and redo.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The document's paragraphs, in order.
    pub fn paragraphs(
        &self,
    ) -> impl ExactSizeIterator<Item = AccessibleParagraph<'editor>> + DoubleEndedIterator {
        self.document
            .paragraphs()
            .iter()
            .enumerate()
            .map(|(index, paragraph)| AccessibleParagraph {
                index,
                text: paragraph.text(),
                kind: paragraph.kind(),
                spans: paragraph.spans(),
            })
    }

    /// The selection in grapheme positions.
    #[must_use]
    pub fn selection_graphemes(&self) -> AccessibleSelection {
        AccessibleSelection {
            anchor: accessible_position(self.document, self.selection.anchor),
            focus: accessible_position(self.document, self.selection.focus),
        }
    }

    /// Converts a selection in grapheme positions, as assistive technology may
    /// report it, to a [`Selection`] in byte positions, such as for
    /// [`AccessibleAction::SetSelection`](super::AccessibleAction::SetSelection). The editor is not changed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidGraphemePosition`] for the first endpoint, anchor
    /// before focus, whose paragraph does not exist or has fewer graphemes than
    /// the endpoint counts.
    pub fn selection_from_graphemes(
        &self,
        selection: AccessibleSelection,
    ) -> Result<Selection, Error> {
        Ok(Selection::new(
            editor_position(self.document, selection.anchor)?,
            editor_position(self.document, selection.focus)?,
        ))
    }
}

/// Converts a position that the editor keeps valid to grapheme coordinates.
fn accessible_position(document: &Document, position: Position) -> AccessiblePosition {
    let paragraph = document
        .paragraph(position.paragraph)
        .expect("editor maintains valid selection");
    AccessiblePosition {
        paragraph: position.paragraph,
        grapheme: paragraph
            .grapheme_index(position.byte)
            .expect("editor maintains valid selection"),
    }
}

/// Converts grapheme coordinates to a byte position, if they exist in `document`.
fn editor_position(document: &Document, position: AccessiblePosition) -> Result<Position, Error> {
    document
        .paragraph(position.paragraph)
        .and_then(|paragraph| paragraph.byte_from_grapheme(position.grapheme))
        .map(|byte| Position::new(position.paragraph, byte))
        .ok_or(Error::InvalidGraphemePosition {
            paragraph: position.paragraph,
            grapheme: position.grapheme,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_exposes_semantics_and_grapheme_selections() {
        let mut editor = Editor::from_text("a👩🏽\u{200d}💻\nitem");
        editor
            .set_selection(Selection::new(Position::new(0, 1), Position::new(1, 4)))
            .unwrap();
        editor
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 1 })
            .unwrap();
        let snapshot = AccessibilitySnapshot::new(&editor, "Notes", Focus::Focused);
        assert_eq!(snapshot.label(), "Notes");
        assert!(snapshot.is_focused());
        assert_eq!(snapshot.selection(), editor.selection());
        assert_eq!(snapshot.revision(), editor.document().revision());
        assert!(snapshot.composition().is_none());
        assert_eq!(snapshot.paragraphs().count(), 2);
        assert_eq!(
            snapshot.paragraphs().nth(1).unwrap().kind,
            ParagraphKind::Bullet { indent: 1 }
        );
        let selection = snapshot.selection_graphemes();
        assert_eq!(selection.anchor.grapheme, 1);
        assert_eq!(
            snapshot.selection_from_graphemes(selection).unwrap(),
            editor.selection()
        );
        assert!(
            snapshot
                .selection_from_graphemes(AccessibleSelection {
                    anchor: AccessiblePosition {
                        paragraph: 0,
                        grapheme: 20
                    },
                    focus: selection.focus,
                })
                .is_err()
        );
    }

    #[test]
    fn invalid_grapheme_positions_report_grapheme_coordinates() {
        let editor = Editor::from_text("h\u{e9}llo");
        let snapshot = AccessibilitySnapshot::new(&editor, "Notes", Focus::Focused);
        let caret = |paragraph, grapheme| {
            let position = AccessiblePosition {
                paragraph,
                grapheme,
            };
            AccessibleSelection {
                anchor: position,
                focus: position,
            }
        };
        assert_eq!(
            snapshot.selection_from_graphemes(caret(0, 6)),
            Err(Error::InvalidGraphemePosition {
                paragraph: 0,
                grapheme: 6
            })
        );
        assert_eq!(
            snapshot.selection_from_graphemes(caret(1, 0)),
            Err(Error::InvalidGraphemePosition {
                paragraph: 1,
                grapheme: 0
            })
        );
        assert_eq!(
            snapshot.selection_from_graphemes(caret(0, 5)),
            Ok(Selection::caret(Position::new(0, 6)))
        );
    }
}
