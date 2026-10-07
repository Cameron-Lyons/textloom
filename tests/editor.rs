use std::sync::Arc;
use textloom::{
    Document, Editor, Error, HistoryLimits, Movement, ParagraphKind, Position, Selection,
    StylePatch,
};

fn caret(editor: &mut Editor, paragraph: usize, byte: usize) {
    editor
        .set_selection(Selection::caret(Position::new(paragraph, byte)))
        .unwrap();
}

#[test]
fn extended_graphemes_are_indivisible_for_movement_and_deletion() {
    let mut editor = Editor::from_text("a👩‍💻e\u{301}🇺🇳z");
    let clusters = ["a", "👩‍💻", "e\u{301}", "🇺🇳", "z"];
    let mut byte = 0;
    for cluster in clusters {
        editor
            .move_cursor(Movement::GraphemeForward, false)
            .unwrap();
        byte += cluster.len();
        assert_eq!(editor.selection().focus.byte, byte);
    }
    editor.delete_backward().unwrap();
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "a👩‍💻e\u{301}");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "a👩‍💻e\u{301}🇺🇳");
    caret(&mut editor, 0, 1);
    editor.delete_forward().unwrap();
    assert_eq!(editor.document().plain_text(), "ae\u{301}🇺🇳");
}

#[test]
fn invalid_selection_is_atomic() {
    let mut editor = Editor::from_text("e\u{301}🦀");
    let original = editor.selection();
    for byte in [1, 2, 4, 99] {
        assert!(matches!(
            editor.set_selection(Selection::caret(Position::new(0, byte))),
            Err(Error::InvalidPosition(_))
        ));
        assert_eq!(editor.selection(), original);
    }
    assert!(
        editor
            .set_selection(Selection::caret(Position::new(99, 0)))
            .is_err()
    );
    assert!(!editor.can_undo());
}

#[test]
fn backward_selection_replacement_and_history_preserve_direction() {
    let mut editor = Editor::from_text("hello\nworld");
    let selected = Selection::new(Position::new(1, 3), Position::new(0, 2));
    editor.set_selection(selected).unwrap();
    assert_eq!(editor.selected_text(), "llo\nwor");
    editor.insert_text("X").unwrap();
    assert_eq!(editor.document().plain_text(), "heXld");
    assert!(editor.undo());
    assert_eq!(editor.selection(), selected);
    assert_eq!(editor.document().plain_text(), "hello\nworld");
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "heXld");
}

#[test]
fn typing_coalesces_without_retaining_intermediate_paragraphs() {
    let mut editor = Editor::default();
    editor.insert_text("a").unwrap();
    editor.insert_text("b").unwrap();
    editor.insert_text("c").unwrap();
    assert_eq!(editor.undo_len(), 1);
    let bytes = editor.history_bytes();
    editor.break_history_group();
    editor.insert_text("d").unwrap();
    assert_eq!(editor.undo_len(), 2);
    assert!(editor.history_bytes() > bytes);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "abc");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "");
    assert!(editor.redo());
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "abcd");
}

#[test]
fn selection_and_formatting_break_typing_groups() {
    let mut editor = Editor::default();
    editor.insert_text("abc").unwrap();
    caret(&mut editor, 0, 3);
    editor.insert_text("d").unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    editor.insert_text("e").unwrap();
    assert_eq!(editor.undo_len(), 3);
    assert!(editor.document().style_at(Position::new(0, 5)).bold);
    assert!(editor.undo());
    assert!(editor.typing_style().bold);
}

#[test]
fn formatting_undo_redo_restores_document_and_selection() {
    let mut editor = Editor::from_text("one two");
    let selection = Selection::new(Position::new(0, 0), Position::new(0, 3));
    editor.set_selection(selection).unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            italic: Some(true),
            ..Default::default()
        })
        .unwrap();
    assert!(editor.document().style_at(Position::new(0, 1)).bold);
    assert!(!editor.document().style_at(Position::new(0, 5)).bold);
    assert!(editor.undo());
    assert_eq!(editor.selection(), selection);
    assert!(!editor.document().style_at(Position::new(0, 1)).bold);
    assert!(editor.redo());
    assert!(editor.document().style_at(Position::new(0, 1)).italic);
}

#[test]
fn list_enter_continues_then_empty_enter_exits() {
    let mut editor = Editor::from_text("first");
    editor
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: 1,
            start: 7,
        })
        .unwrap();
    caret(&mut editor, 0, 5);
    editor.insert_paragraph().unwrap();
    assert_eq!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Ordered {
            indent: 1,
            start: 8
        }
    );
    editor.insert_paragraph().unwrap();
    assert_eq!(editor.document().paragraphs().len(), 2);
    assert_eq!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Body
    );
    assert!(editor.undo());
    assert_eq!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Ordered {
            indent: 1,
            start: 8
        }
    );
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "first");
}

#[test]
fn heading_enter_returns_to_body() {
    let mut editor = Editor::from_text("Title");
    editor
        .set_paragraph_kind(ParagraphKind::Heading { level: 2 })
        .unwrap();
    caret(&mut editor, 0, 5);
    editor.insert_paragraph().unwrap();
    assert_eq!(
        editor.document().paragraph(0).unwrap().kind(),
        ParagraphKind::Heading { level: 2 }
    );
    assert_eq!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Body
    );
    assert!(
        editor
            .set_paragraph_kind(ParagraphKind::Heading { level: 0 })
            .is_err()
    );
    assert_eq!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Body
    );
}

#[test]
fn paragraph_formatting_excludes_end_at_next_paragraph_start() {
    let mut editor = Editor::from_text("one\ntwo\nthree");
    editor
        .set_selection(Selection::new(Position::new(0, 1), Position::new(2, 0)))
        .unwrap();
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
        .unwrap();
    assert_eq!(
        editor.document().paragraph(0).unwrap().kind(),
        ParagraphKind::Bullet { indent: 0 }
    );
    assert_eq!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Bullet { indent: 0 }
    );
    assert_eq!(
        editor.document().paragraph(2).unwrap().kind(),
        ParagraphKind::Body
    );
}

#[test]
fn ime_is_transient_cancelable_and_commits_as_one_undo_step() {
    let mut editor = Editor::from_text("replace me");
    let original = Selection::new(Position::new(0, 0), Position::new(0, 7));
    editor.set_selection(original).unwrap();
    let revision = editor.document().revision();
    editor.update_composition("に", Some(0..3)).unwrap();
    editor.update_composition("日本", Some(3..6)).unwrap();
    assert_eq!(editor.document().plain_text(), "replace me");
    assert_eq!(editor.document().revision(), revision);
    assert!(!editor.can_undo());
    assert_eq!(editor.insert_text("oops"), Err(Error::CompositionActive));
    assert!(editor.cancel_composition());
    assert_eq!(editor.selection(), original);
    editor.update_composition("日本", Some(3..6)).unwrap();
    editor.commit_composition("日本語").unwrap();
    assert!(editor.composition().is_none());
    assert_eq!(editor.document().plain_text(), "日本語 me");
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.selection(), original);
    assert_eq!(editor.document().plain_text(), "replace me");
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "日本語 me");
}

#[test]
fn malformed_ime_offsets_do_not_change_composition() {
    let mut editor = Editor::default();
    assert_eq!(
        editor.update_composition("🦀", Some(1..4)),
        Err(Error::InvalidCompositionSelection)
    );
    assert!(editor.composition().is_none());
    editor.update_composition("abc", Some(1..2)).unwrap();
    let saved = editor.composition().cloned();
    assert_eq!(
        editor.update_composition("x", Some(0..2)),
        Err(Error::InvalidCompositionSelection)
    );
    assert_eq!(editor.composition(), saved.as_ref());
}

#[test]
fn empty_ime_commit_replaces_selected_text() {
    let mut editor = Editor::from_text("abc");
    editor.select_all();
    editor.begin_composition();
    editor.commit_composition("").unwrap();
    assert_eq!(editor.document().plain_text(), "");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "abc");
}

#[test]
fn history_limits_apply_to_both_directions_and_new_edit_discards_redo() {
    let mut editor = Editor::default();
    editor.set_history_limits(HistoryLimits {
        max_entries: 2,
        max_bytes: usize::MAX,
    });
    for text in ["a", "b", "c"] {
        editor.break_history_group();
        editor.insert_text(text).unwrap();
    }
    assert_eq!(editor.undo_len(), 2);
    assert!(editor.undo());
    assert!(editor.undo());
    assert!(!editor.undo());
    assert_eq!(editor.document().plain_text(), "a");
    assert!(editor.redo());
    editor.insert_text("X").unwrap();
    assert!(!editor.can_redo());
    editor.set_history_limits(HistoryLimits {
        max_entries: 0,
        max_bytes: 0,
    });
    assert!(!editor.can_undo());
    assert_eq!(editor.history_bytes(), 0);
    editor.insert_text("Y").unwrap();
    assert!(!editor.can_undo());
}

#[test]
fn unchanged_paragraphs_remain_shared_during_edit_and_undo() {
    let mut editor = Editor::from_text("before\nmiddle\nafter");
    let first = editor.document().paragraphs()[0].clone();
    let last = editor.document().paragraphs()[2].clone();
    caret(&mut editor, 1, 3);
    editor.insert_text("X").unwrap();
    assert!(Arc::ptr_eq(&first, &editor.document().paragraphs()[0]));
    assert!(Arc::ptr_eq(&last, &editor.document().paragraphs()[2]));
    editor.undo();
    assert!(Arc::ptr_eq(&first, &editor.document().paragraphs()[0]));
    assert!(Arc::ptr_eq(&last, &editor.document().paragraphs()[2]));
}

#[test]
fn joining_and_splitting_paragraphs_are_reversible() {
    let mut editor = Editor::from_text("one\ntwo");
    caret(&mut editor, 1, 0);
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "onetwo");
    assert_eq!(editor.selection().focus, Position::new(0, 3));
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "one\ntwo");
    assert_eq!(editor.selection().focus, Position::new(1, 0));
    caret(&mut editor, 0, 3);
    editor.delete_forward().unwrap();
    assert_eq!(editor.document().plain_text(), "onetwo");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "one\ntwo");
}

#[test]
fn vertical_movement_preserves_grapheme_column_across_short_paragraph() {
    let mut editor = Editor::from_text("abcdef\nx\n🦀🦀🦀🦀🦀🦀");
    caret(&mut editor, 0, 5);
    editor.move_cursor(Movement::ParagraphDown, false).unwrap();
    assert_eq!(editor.selection().focus, Position::new(1, 1));
    editor.move_cursor(Movement::ParagraphDown, false).unwrap();
    assert_eq!(editor.selection().focus, Position::new(2, 20));
    editor.move_cursor(Movement::ParagraphUp, true).unwrap();
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(2, 20), Position::new(1, 1))
    );
}

#[test]
fn combining_insertions_and_joins_leave_valid_carets() {
    let mut editor = Editor::from_text("eX");
    caret(&mut editor, 0, 1);
    editor.insert_text("\u{301}").unwrap();
    assert_eq!(editor.selection().focus, Position::new(0, 3));
    assert!(
        editor
            .document()
            .validate_position(editor.selection().focus)
            .is_ok()
    );
    let mut editor = Editor::from_text("👩\n‍💻");
    caret(&mut editor, 1, 0);
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "👩‍💻");
    assert!(
        editor
            .document()
            .validate_position(editor.selection().focus)
            .is_ok()
    );
}

#[test]
fn deterministic_edit_sequence_matches_plain_text_and_roundtrips_history() {
    let mut editor = Editor::default();
    editor.set_history_limits(HistoryLimits {
        max_entries: 1024,
        max_bytes: 32 * 1024 * 1024,
    });
    let mut plain = String::new();
    let mut state = 123456789_u64;
    for _ in 0..300 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let position = editor.document().end();
        editor.set_selection(Selection::caret(position)).unwrap();
        if state.is_multiple_of(4) && !plain.is_empty() {
            use unicode_segmentation::UnicodeSegmentation;
            let last = plain.grapheme_indices(true).next_back().unwrap().0;
            plain.truncate(last);
            editor.delete_backward().unwrap();
        } else {
            let text = ["abc", "🦀", "e\u{301}", "\n"][((state >> 8) % 4) as usize];
            plain.push_str(text);
            editor.insert_text(text).unwrap();
        }
        assert_eq!(editor.document().plain_text(), plain);
        assert!(
            editor
                .document()
                .validate_position(editor.selection().focus)
                .is_ok()
        );
    }
    let completed = plain;
    while editor.undo() {}
    assert_eq!(editor.document().plain_text(), "");
    while editor.redo() {}
    assert_eq!(editor.document().plain_text(), completed);
}

#[test]
fn normalized_line_endings_and_empty_document() {
    let document = Document::from_text("a\r\nb\rc\n");
    assert_eq!(document.plain_text(), "a\nb\nc\n");
    assert_eq!(document.paragraphs().len(), 4);
    assert_eq!(Document::default().paragraphs().len(), 1);
}

#[test]
fn rejected_heading_does_not_change_typing_history_or_style() {
    let mut editor = Editor::default();
    editor.insert_text("a").unwrap();
    let selection = editor.selection();
    let style = editor.typing_style();
    let revision = editor.document().revision();
    assert_eq!(
        editor.set_paragraph_kind(ParagraphKind::Heading { level: 0 }),
        Err(Error::InvalidHeadingLevel(0))
    );
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), style);
    assert_eq!(editor.document().revision(), revision);
    editor.insert_text("b").unwrap();
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "");
}

#[test]
fn invalid_selection_preserves_active_composition_and_history() {
    let mut editor = Editor::from_text("e\u{301}");
    editor.begin_composition();
    editor.update_composition("日本", Some(3..6)).unwrap();
    let composition = editor.composition().cloned();
    let selection = editor.selection();
    let style = editor.typing_style();
    assert!(
        editor
            .set_selection(Selection::new(Position::default(), Position::new(0, 1)))
            .is_err()
    );
    assert_eq!(editor.composition(), composition.as_ref());
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), style);
    assert!(!editor.can_undo());
}

#[test]
fn word_movement_remains_at_grapheme_boundaries_with_prepend_and_joiners() {
    let mut editor = Editor::from_text("\u{600}one e\u{301}clair 👩‍💻 test\nनमस्ते world");
    for movement in [Movement::WordForward, Movement::WordBackward] {
        editor
            .move_cursor(
                if movement == Movement::WordForward {
                    Movement::DocumentStart
                } else {
                    Movement::DocumentEnd
                },
                false,
            )
            .unwrap();
        for _ in 0..20 {
            let old = editor.selection().focus;
            editor.move_cursor(movement, false).unwrap();
            let new = editor.selection().focus;
            assert!(editor.document().validate_position(new).is_ok());
            if movement == Movement::WordForward {
                assert!(new >= old);
            } else {
                assert!(new <= old);
            }
        }
    }
}

fn ordered_kinds(editor: &Editor) -> Vec<ParagraphKind> {
    editor
        .document()
        .paragraphs()
        .iter()
        .map(|paragraph| paragraph.kind())
        .collect()
}

fn ordered(start: u32) -> ParagraphKind {
    ParagraphKind::Ordered { indent: 0, start }
}

#[test]
fn ordered_selection_numbers_items_consecutively_and_undo_restores_blocks() {
    let mut editor = Editor::from_text("one\ntwo\nthree");
    editor.select_all();
    editor.set_paragraph_kind(ordered(4)).unwrap();
    assert_eq!(ordered_kinds(&editor), [ordered(4), ordered(5), ordered(6)]);
    assert!(editor.undo());
    assert_eq!(ordered_kinds(&editor), [ParagraphKind::Body; 3]);
    assert!(editor.redo());
    assert_eq!(ordered_kinds(&editor), [ordered(4), ordered(5), ordered(6)]);
}

#[test]
fn ordered_restart_updates_the_following_run_and_survives_plain_typing() {
    let mut editor = Editor::from_text("one\ntwo\nthree\nfour");
    editor.select_all();
    editor.set_paragraph_kind(ordered(1)).unwrap();
    caret(&mut editor, 1, 0);
    editor.set_paragraph_kind(ordered(10)).unwrap();
    assert_eq!(
        ordered_kinds(&editor),
        [ordered(1), ordered(10), ordered(11), ordered(12)]
    );
    caret(&mut editor, 0, 1);
    editor.insert_text("X").unwrap();
    assert_eq!(
        ordered_kinds(&editor),
        [ordered(1), ordered(10), ordered(11), ordered(12)]
    );
    editor.delete_backward().unwrap();
    assert_eq!(
        ordered_kinds(&editor),
        [ordered(1), ordered(10), ordered(11), ordered(12)]
    );
    assert!(editor.undo());
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(
        ordered_kinds(&editor),
        [ordered(1), ordered(2), ordered(3), ordered(4)]
    );
}

#[test]
fn mid_list_enter_renumbers_following_items_and_shares_their_text() {
    let mut editor = Editor::from_text("one\ntwo\nthree");
    editor.select_all();
    editor.set_paragraph_kind(ordered(4)).unwrap();
    let before = editor.document().paragraphs().to_vec();
    let trailing_text = before[2].text().as_ptr();
    let trailing_spans = before[2].spans().as_ptr();
    caret(&mut editor, 1, 1);
    editor.insert_paragraph().unwrap();
    assert_eq!(editor.document().plain_text(), "one\nt\nwo\nthree");
    assert_eq!(
        ordered_kinds(&editor),
        [ordered(4), ordered(5), ordered(6), ordered(7)]
    );
    assert!(Arc::ptr_eq(&before[0], &editor.document().paragraphs()[0]));
    assert_eq!(
        trailing_text,
        editor.document().paragraph(3).unwrap().text().as_ptr()
    );
    assert_eq!(
        trailing_spans,
        editor.document().paragraph(3).unwrap().spans().as_ptr()
    );
    assert!(editor.undo());
    assert_eq!(editor.document().paragraphs(), before);
    assert!(editor.redo());
    assert_eq!(
        ordered_kinds(&editor),
        [ordered(4), ordered(5), ordered(6), ordered(7)]
    );
}

#[test]
fn joining_or_deleting_ordered_items_renumbers_in_the_same_undo_step() {
    let mut editor = Editor::from_text("one\ntwo\nthree");
    editor.select_all();
    editor.set_paragraph_kind(ordered(4)).unwrap();
    let before = editor.document().paragraphs().to_vec();
    caret(&mut editor, 1, 0);
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "onetwo\nthree");
    assert_eq!(ordered_kinds(&editor), [ordered(4), ordered(5)]);
    assert!(editor.undo());
    assert_eq!(editor.document().paragraphs(), before);
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(1, 0)))
        .unwrap();
    editor.insert_text("").unwrap();
    assert_eq!(editor.document().plain_text(), "two\nthree");
    assert_eq!(ordered_kinds(&editor), [ordered(4), ordered(5)]);
    assert!(editor.undo());
    assert_eq!(editor.document().paragraphs(), before);
}

#[test]
fn ordered_continuations_stop_at_different_indent_and_body() {
    let mut editor = Editor::from_text("one\ntwo\nnested\nbody\nfive");
    editor.select_all();
    editor.set_paragraph_kind(ordered(1)).unwrap();
    caret(&mut editor, 2, 0);
    let nested = ParagraphKind::Ordered {
        indent: 1,
        start: 7,
    };
    editor.set_paragraph_kind(nested).unwrap();
    caret(&mut editor, 3, 0);
    editor.set_paragraph_kind(ParagraphKind::Body).unwrap();
    caret(&mut editor, 0, 0);
    editor.set_paragraph_kind(ordered(10)).unwrap();
    assert_eq!(
        ordered_kinds(&editor),
        [
            ordered(10),
            ordered(11),
            nested,
            ParagraphKind::Body,
            ordered(5)
        ]
    );
    caret(&mut editor, 0, 3);
    editor.insert_paragraph().unwrap();
    assert_eq!(
        ordered_kinds(&editor),
        [
            ordered(10),
            ordered(11),
            ordered(12),
            nested,
            ParagraphKind::Body,
            ordered(5)
        ]
    );
}

#[test]
fn programmatic_range_replacement_restores_original_caret_on_undo() {
    let mut editor = Editor::from_text("abcdef");
    caret(&mut editor, 0, 4);
    editor
        .replace_range(Position::new(0, 2)..Position::new(0, 4), "")
        .unwrap();
    assert_eq!(editor.document().plain_text(), "abef");
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 2)));
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "abcdef");
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 4)));
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "abef");
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 2)));
}

#[test]
fn programmatic_range_validation_preserves_history_and_composition() {
    let mut editor = Editor::default();
    editor.insert_text("a").unwrap();
    assert_eq!(
        editor.replace_range(Position::new(0, 1)..Position::default(), "x"),
        Err(Error::InvalidRange)
    );
    assert!(
        editor
            .replace_range(Position::default()..Position::new(0, 99), "x")
            .is_err()
    );
    editor.insert_text("b").unwrap();
    assert_eq!(editor.undo_len(), 1);
    editor.update_composition("x", Some(0..1)).unwrap();
    let composition = editor.composition().cloned();
    assert_eq!(
        editor.replace_range(Position::default()..Position::new(0, 1), "y"),
        Err(Error::CompositionActive)
    );
    assert_eq!(editor.composition(), composition.as_ref());
    assert_eq!(editor.document().plain_text(), "ab");
}
