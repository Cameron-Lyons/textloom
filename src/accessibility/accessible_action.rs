//! Requests from assistive technology and how an editor carries them out.

use crate::{Editor, Error, Link, Selection};

/// A request from assistive technology for [`apply_action`] to carry out.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessibleAction<'text> {
    /// Move keyboard focus to the editor. The editor does not change; the
    /// outcome's [`focus_requested`](AccessibilityOutcome::focus_requested) asks
    /// the host to focus the editor widget and, if needed, its window.
    Focus,
    /// Select this range, which must have both endpoints on grapheme boundaries of
    /// the document, and cancel any IME composition.
    SetSelection(Selection),
    /// Replace the selection with this text, or insert it at the caret, as one
    /// undo step after canceling any IME composition.
    ReplaceSelectedText(&'text str),
    /// Replace the whole document with this text, as one undo step after canceling
    /// any IME composition. The new text takes the inline style and the paragraph
    /// kind at the start of the document. Undo restores the previous selection.
    ReplaceAllText(&'text str),
}

/// What an accessibility action did, for the host to act on.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessibilityOutcome {
    /// Whether the action was recognized and carried out. An action for another
    /// node or tree, or one that the editor does not support, is not handled.
    pub handled: bool,
    /// Whether the host should move keyboard focus to the editor widget and, if
    /// needed, its window.
    pub focus_requested: bool,
    /// Whether the document, the selection, the typing style, or whether a
    /// composition is in progress changed, or the AccessKit adapter's read-only
    /// mode. The host should redraw and publish an update.
    pub changed: bool,
    /// Whether the action discarded an IME composition, so that the host can reset
    /// the platform's input method.
    pub composition_canceled: bool,
    /// The link that assistive technology activated by clicking its node. The
    /// editor does not open links; the host decides whether to follow it.
    pub activated_link: Option<Link>,
}

/// Carries out `action` on `editor` through the editor's ordinary validation and
/// undo history.
///
/// # Errors
///
/// Returns the editor's error if it rejects the action, such as
/// [`Error::InvalidPosition`] for a selection endpoint that is not a grapheme
/// boundary of the document.
pub fn apply_action(
    editor: &mut Editor,
    action: AccessibleAction<'_>,
) -> Result<AccessibilityOutcome, Error> {
    let before = editor.presentation();
    match action {
        AccessibleAction::Focus => {
            return Ok(AccessibilityOutcome {
                handled: true,
                focus_requested: true,
                ..AccessibilityOutcome::default()
            });
        }
        AccessibleAction::SetSelection(selection) => editor.set_selection(selection)?,
        AccessibleAction::ReplaceSelectedText(text) => {
            editor.cancel_composition();
            editor.isolated_edit(|editor| editor.insert_text(text))?;
        }
        AccessibleAction::ReplaceAllText(text) => {
            editor.cancel_composition();
            editor.isolated_edit(|editor| editor.replace_all_text(text))?;
        }
    }
    let after = editor.presentation();
    Ok(AccessibilityOutcome {
        handled: true,
        focus_requested: false,
        changed: before != after,
        composition_canceled: before.composing && !after.composing,
        activated_link: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ParagraphKind, Position, StylePatch};

    #[test]
    fn outcomes_report_canceled_composition() {
        let mut editor = Editor::from_text("Hello");
        editor.update_composition("にほん", None).unwrap();
        let outcome = apply_action(
            &mut editor,
            AccessibleAction::SetSelection(Selection::caret(Position::new(0, 1))),
        )
        .unwrap();
        assert!(outcome.changed && outcome.composition_canceled);
        let outcome = apply_action(
            &mut editor,
            AccessibleAction::SetSelection(Selection::caret(Position::new(0, 2))),
        )
        .unwrap();
        assert!(outcome.changed && !outcome.composition_canceled);

        editor.update_composition("か", None).unwrap();
        let outcome = apply_action(&mut editor, AccessibleAction::Focus).unwrap();
        assert!(outcome.focus_requested && !outcome.composition_canceled);
        assert!(editor.composition().is_some());
        let outcome =
            apply_action(&mut editor, AccessibleAction::ReplaceSelectedText("x")).unwrap();
        assert!(outcome.composition_canceled);
        assert_eq!(editor.document().plain_text(), "Hexllo");
    }

    #[test]
    fn replacing_all_text_uses_the_document_start_style() {
        let mut editor = Editor::from_text("Title\nbody bold");
        editor
            .set_paragraph_kind(ParagraphKind::Heading { level: 1 })
            .unwrap();
        editor
            .set_selection(Selection::new(Position::new(1, 5), Position::new(1, 9)))
            .unwrap();
        editor
            .apply_style(StylePatch {
                bold: Some(true),
                ..StylePatch::default()
            })
            .unwrap();
        let selection = Selection::caret(Position::new(1, 9));
        editor.set_selection(selection).unwrap();
        let typing_style = editor.typing_style();
        assert!(typing_style.bold);
        apply_action(
            &mut editor,
            AccessibleAction::ReplaceAllText("brand new\ncontent"),
        )
        .unwrap();
        let paragraphs = editor.document().paragraphs();
        assert_eq!(paragraphs[0].kind(), ParagraphKind::Heading { level: 1 });
        assert_eq!(paragraphs[1].kind(), ParagraphKind::Body);
        assert!(
            paragraphs
                .iter()
                .flat_map(|paragraph| paragraph.spans())
                .all(|span| !span.style.bold)
        );
        assert!(!editor.typing_style().bold);
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "Title\nbody bold");
        assert_eq!(editor.selection(), selection);
        assert_eq!(editor.typing_style(), typing_style);
    }

    #[test]
    fn actions_request_focus_validate_selection_and_use_undo() {
        let mut editor = Editor::from_text("hello");
        assert!(
            apply_action(&mut editor, AccessibleAction::Focus)
                .unwrap()
                .focus_requested
        );
        assert!(
            apply_action(
                &mut editor,
                AccessibleAction::SetSelection(Selection::caret(Position::new(0, 99)))
            )
            .is_err()
        );
        editor.select_all();
        assert!(
            apply_action(&mut editor, AccessibleAction::ReplaceSelectedText("new"))
                .unwrap()
                .changed
        );
        assert_eq!(editor.document().plain_text(), "new");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "hello");
    }

    #[test]
    fn actions_cancel_composition_and_report_preview_only_changes() {
        let mut editor = Editor::from_text("original");
        editor.select_all();
        let selection = editor.selection();
        editor.update_composition("candidate", None).unwrap();
        let outcome = apply_action(&mut editor, AccessibleAction::SetSelection(selection)).unwrap();
        assert!(outcome.changed);
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "original");

        editor.update_composition("candidate", None).unwrap();
        let outcome =
            apply_action(&mut editor, AccessibleAction::ReplaceSelectedText("new")).unwrap();
        assert!(outcome.changed);
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "new");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "original");
        assert_eq!(editor.selection(), selection);
        assert!(!editor.can_undo());
    }

    #[test]
    fn replacing_all_text_preserves_selection_and_typing_undo_boundaries() {
        let mut editor = Editor::from_text("original");
        editor.insert_text("typed ").unwrap();
        let selection = Selection::new(Position::new(0, 7), Position::new(0, 2));
        editor.set_selection(selection).unwrap();
        editor.update_composition("candidate", None).unwrap();
        let outcome = apply_action(&mut editor, AccessibleAction::ReplaceAllText("new")).unwrap();
        assert!(outcome.handled && outcome.changed);
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "new");
        editor.insert_text(" typing").unwrap();
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "new");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "typed original");
        assert_eq!(editor.selection(), selection);
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "original");
    }
}
