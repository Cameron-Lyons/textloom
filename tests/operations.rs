//! Batch replacement, search navigation, word deletion, and list indentation contracts.

use std::sync::Arc;

use textloom::{
    Editor, Error, Fragment, ParagraphKind, Position, SearchOptions, Selection, StylePatch,
};

fn caret(editor: &mut Editor, paragraph: usize, byte: usize) {
    editor
        .set_selection(Selection::caret(Position::new(paragraph, byte)))
        .unwrap();
}

fn kind(editor: &mut Editor, paragraph: usize, kind: ParagraphKind) {
    caret(editor, paragraph, 0);
    editor.set_paragraph_kind(kind).unwrap();
}

fn style(editor: &mut Editor, paragraph: usize, start: usize, end: usize, patch: StylePatch) {
    editor
        .set_selection(Selection::new(
            Position::new(paragraph, start),
            Position::new(paragraph, end),
        ))
        .unwrap();
    editor.apply_style(patch).unwrap();
}

#[test]
fn replace_all_preserves_each_match_style_and_undo_restores_directional_selection() {
    let mut editor = Editor::from_text("keep\nred red\nmiddle\nred blue\nend");
    style(
        &mut editor,
        1,
        0,
        3,
        StylePatch {
            bold: Some(true),
            ..Default::default()
        },
    );
    style(
        &mut editor,
        3,
        0,
        3,
        StylePatch {
            italic: Some(true),
            ..Default::default()
        },
    );
    kind(&mut editor, 1, ParagraphKind::Heading { level: 2 });
    kind(&mut editor, 3, ParagraphKind::Bullet { indent: 1 });
    let selection = Selection::new(editor.document().end(), Position::new(1, 1));
    editor.set_selection(selection).unwrap();
    let before = Fragment::from_document(editor.document());
    let typing_style = editor.typing_style();
    editor.clear_history();
    let revision = editor.document().revision();
    assert_eq!(
        editor
            .replace_all("red", "R", SearchOptions::default())
            .unwrap(),
        3
    );
    assert_eq!(
        editor.document().plain_text(),
        "keep\nR R\nmiddle\nR blue\nend"
    );
    assert_eq!(editor.document().revision(), revision + 1);
    assert_eq!(editor.undo_len(), 1);
    assert_eq!(editor.selection(), Selection::caret(Position::new(3, 1)));
    let first = editor.document().paragraph(1).unwrap().spans();
    assert_eq!(first[0].range, 0..1);
    assert!(first[0].style.bold);
    assert!(!first[1].style.bold);
    assert!(
        editor.document().paragraph(3).unwrap().spans()[0]
            .style
            .italic
    );
    assert_eq!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Heading { level: 2 }
    );
    assert_eq!(
        editor.document().paragraph(3).unwrap().kind(),
        ParagraphKind::Bullet { indent: 1 }
    );
    let after = Fragment::from_document(editor.document());
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), typing_style);
    assert!(!editor.can_undo());
    assert!(editor.redo());
    assert_eq!(Fragment::from_document(editor.document()), after);
    assert_eq!(editor.selection(), Selection::caret(Position::new(3, 1)));
}

#[test]
fn replace_all_adjacent_combining_replacements_use_original_match_ranges() {
    let mut editor = Editor::from_text("aa");
    style(
        &mut editor,
        0,
        0,
        1,
        StylePatch {
            bold: Some(true),
            ..Default::default()
        },
    );
    style(
        &mut editor,
        0,
        1,
        2,
        StylePatch {
            italic: Some(true),
            ..Default::default()
        },
    );
    editor.clear_history();
    let before = Fragment::from_document(editor.document());
    assert_eq!(
        editor
            .replace_all("a", "\u{301}", SearchOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(editor.document().plain_text(), "\u{301}\u{301}");
    assert_eq!(editor.selection().focus, Position::new(0, 4));
    assert!(
        editor
            .document()
            .validate_position(Position::new(0, 2))
            .is_err()
    );
    let spans = editor.document().paragraph(0).unwrap().spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].range, 0..4);
    assert!(spans[0].style.bold);
    assert!(!spans[0].style.italic);
    assert!(textloom::Document::from_bytes(&editor.document().to_bytes()).is_ok());
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "\u{301}\u{301}");
}

#[test]
fn replace_all_can_insert_line_breaks_without_shifting_later_matches() {
    let mut editor = Editor::from_text("a a\nfinish");
    assert_eq!(
        editor
            .replace_all("a", "x\r\ny", SearchOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(editor.document().plain_text(), "x\ny x\ny\nfinish");
    assert_eq!(editor.selection().focus, Position::new(2, 1));
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "a a\nfinish");
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "x\ny x\ny\nfinish");
}

#[test]
fn replace_all_handles_crossparagraph_queries_and_nonoverlapping_matches() {
    let mut editor = Editor::from_text("a\nb\nc\na\nb\nc");
    assert_eq!(
        editor
            .replace_all("b\r\nc", "!", SearchOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(editor.document().plain_text(), "a\n!\na\n!");
    assert_eq!(editor.selection().focus, Position::new(3, 1));
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "a\nb\nc\na\nb\nc");
    let mut editor = Editor::from_text("aaaaa");
    assert_eq!(
        editor
            .replace_all("aa", "x", SearchOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(editor.document().plain_text(), "xxa");
}

#[test]
fn replace_all_preserves_untouched_allocations_inside_and_outside_its_changed_range() {
    let mut editor = Editor::from_text("prefix\na\nuntouched 🦀\na\nsuffix");
    let before = editor.document().paragraphs().to_vec();
    assert_eq!(
        editor
            .replace_all("a", "b", SearchOptions::default())
            .unwrap(),
        2
    );
    for index in [0, 2, 4] {
        assert!(
            Arc::ptr_eq(&before[index], &editor.document().paragraphs()[index]),
            "paragraph {index}"
        );
    }
    assert!(!Arc::ptr_eq(&before[1], &editor.document().paragraphs()[1]));
    assert!(!Arc::ptr_eq(&before[3], &editor.document().paragraphs()[3]));
    assert!(editor.undo());
    for (source, restored) in before.iter().zip(editor.document().paragraphs()) {
        assert!(Arc::ptr_eq(source, restored));
    }
}

#[test]
fn structural_replace_all_shares_untouched_paragraphs_after_index_shifts() {
    let mut editor = Editor::from_text("prefix\na\nuntouched 🦀\na\nsuffix");
    let before = editor.document().paragraphs().to_vec();
    assert_eq!(
        editor
            .replace_all("a", "x\ny", SearchOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(
        editor.document().plain_text(),
        "prefix\nx\ny\nuntouched 🦀\nx\ny\nsuffix"
    );
    for (source, destination) in [(0, 0), (2, 3), (4, 6)] {
        assert!(
            Arc::ptr_eq(
                &before[source],
                &editor.document().paragraphs()[destination]
            ),
            "paragraph {source} -> {destination}"
        );
    }
    assert!(editor.undo());
    for (source, restored) in before.iter().zip(editor.document().paragraphs()) {
        assert!(Arc::ptr_eq(source, restored));
    }
}

#[test]
fn replace_all_joins_shared_paragraphs_and_normalizes_the_final_caret_and_styles() {
    for (suffix, replacement) in [("\u{301}b", ""), ("b", "\u{301}")] {
        let mut editor = Editor::from_text(&format!("a\n{suffix}"));
        style(
            &mut editor,
            0,
            0,
            1,
            StylePatch {
                italic: Some(true),
                ..Default::default()
            },
        );
        style(
            &mut editor,
            1,
            0,
            suffix.len(),
            StylePatch {
                bold: Some(true),
                ..Default::default()
            },
        );
        kind(&mut editor, 0, ParagraphKind::Heading { level: 2 });
        kind(&mut editor, 1, ParagraphKind::Bullet { indent: 3 });
        let selection = Selection::new(editor.document().end(), Position::default());
        editor.set_selection(selection).unwrap();
        let before = Fragment::from_document(editor.document());
        let paragraphs = editor.document().paragraphs().to_vec();
        editor.clear_history();

        assert_eq!(
            editor
                .replace_all("\n", replacement, SearchOptions::default())
                .unwrap(),
            1
        );
        assert_eq!(editor.document().plain_text(), "a\u{301}b");
        assert_eq!(editor.selection(), Selection::caret(Position::new(0, 3)));
        let paragraph = editor.document().paragraph(0).unwrap();
        assert_eq!(paragraph.kind(), ParagraphKind::Heading { level: 2 });
        assert_eq!(paragraph.spans().len(), 2);
        assert_eq!(paragraph.spans()[0].range, 0..3);
        assert!(paragraph.spans()[0].style.italic);
        assert!(!paragraph.spans()[0].style.bold);
        assert_eq!(paragraph.spans()[1].range, 3..4);
        assert!(paragraph.spans()[1].style.bold);
        assert!(!paragraph.spans()[1].style.italic);
        assert_eq!(editor.undo_len(), 1);
        assert!(editor.undo());
        assert_eq!(Fragment::from_document(editor.document()), before);
        assert_eq!(editor.selection(), selection);
        for (source, restored) in paragraphs.iter().zip(editor.document().paragraphs()) {
            assert!(Arc::ptr_eq(source, restored));
        }
        assert!(editor.redo());
        assert_eq!(editor.document().plain_text(), "a\u{301}b");
        assert_eq!(editor.selection(), Selection::caret(Position::new(0, 3)));
    }
}

#[test]
fn sparse_identical_replacements_preserve_empty_paragraph_metadata_and_allocations() {
    let mut editor = Editor::from_text("match\n\nkeep\n\nmatch");
    kind(&mut editor, 1, ParagraphKind::Heading { level: 3 });
    kind(&mut editor, 3, ParagraphKind::Bullet { indent: 2 });
    let before = Fragment::from_document(editor.document());
    let paragraphs = editor.document().paragraphs().to_vec();
    let revision = editor.document().revision();
    editor.clear_history();

    assert_eq!(
        editor
            .replace_all("match", "match", SearchOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert_eq!(editor.document().revision(), revision);
    assert_eq!(editor.undo_len(), 0);
    assert_eq!(editor.selection(), Selection::caret(Position::new(4, 5)));
    for (source, unchanged) in paragraphs.iter().zip(editor.document().paragraphs()) {
        assert!(Arc::ptr_eq(source, unchanged));
    }
}

#[test]
fn structural_replace_all_continues_ordered_numbering_and_undo_restores_metadata() {
    let mut editor = Editor::from_text("one\nkeep\none\ntail");
    editor.select_all();
    editor
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: 1,
            start: 7,
        })
        .unwrap();
    let before = Fragment::from_document(editor.document());
    editor.clear_history();
    assert_eq!(
        editor
            .replace_all("one", "x\ny", SearchOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(editor.document().plain_text(), "x\ny\nkeep\nx\ny\ntail");
    for (index, paragraph) in editor.document().paragraphs().iter().enumerate() {
        assert_eq!(
            paragraph.kind(),
            ParagraphKind::Ordered {
                indent: 1,
                start: 7 + index as u32
            }
        );
    }
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert!(editor.redo());
    assert_eq!(
        editor.document().paragraph(5).unwrap().kind(),
        ParagraphKind::Ordered {
            indent: 1,
            start: 12
        }
    );
}

#[test]
fn replace_all_preserves_surviving_suffix_paragraph_metadata() {
    for (text, query, replacement) in [
        ("lead\nkeep", "lead\n", ""),
        ("left\nright", "ft\nri", "x\ny"),
        ("\nkeep", "\n", ""),
    ] {
        let mut original = Editor::from_text(text);
        kind(&mut original, 0, ParagraphKind::Heading { level: 2 });
        kind(&mut original, 1, ParagraphKind::Bullet { indent: 3 });
        original.clear_history();
        let range = original
            .document()
            .find(query, SearchOptions::default())
            .remove(0);
        let source = Fragment::from_document(original.document());
        let mut sequential = Editor::new(textloom::Document::from_fragment(&source));
        sequential.replace_range(range, replacement).unwrap();
        let mut batch = Editor::new(textloom::Document::from_fragment(&source));
        assert_eq!(
            batch
                .replace_all(query, replacement, SearchOptions::default())
                .unwrap(),
            1
        );
        assert_eq!(
            Fragment::from_document(batch.document()),
            Fragment::from_document(sequential.document()),
            "{query:?} -> {replacement:?}"
        );
    }
}

#[test]
fn deleting_complete_ordered_item_keeps_its_number_for_surviving_same_indent_item() {
    let mut editor = Editor::from_text("a\nb\nc");
    editor.select_all();
    editor
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: 1,
            start: 4,
        })
        .unwrap();
    let before = Fragment::from_document(editor.document());
    let range = editor
        .document()
        .find("a\n", SearchOptions::default())
        .remove(0);
    let mut single = Editor::new(textloom::Document::from_fragment(&before));
    single.replace_range(range, "").unwrap();
    editor.clear_history();
    assert_eq!(
        editor
            .replace_all("a\n", "", SearchOptions::default())
            .unwrap(),
        1
    );
    assert_eq!(editor.document().plain_text(), "b\nc");
    assert_eq!(
        Fragment::from_document(editor.document()),
        Fragment::from_document(single.document())
    );
    assert_eq!(
        editor.document().paragraph(0).unwrap().kind(),
        ParagraphKind::Ordered {
            indent: 1,
            start: 4
        }
    );
    assert_eq!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Ordered {
            indent: 1,
            start: 5
        }
    );
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), before);
}

#[test]
fn deterministic_unicode_batch_replacements_match_plain_string_oracle_and_restore_metadata() {
    let tokens = ["a", "b", "β", "🦀", "👩‍💻", "e\u{301}", "\n"];
    let queries = [
        "a",
        "b",
        "β",
        "🦀",
        "👩‍💻",
        "e\u{301}",
        "\n",
        "a\n",
        "β\n",
        "ab",
        "\r\n",
    ];
    let replacements = [
        "",
        "X",
        "x\ny",
        "\u{301}",
        "👨‍💻",
        "e\u{301}",
        "\r\nZ",
        "\n\nX\n",
        "\r\r\n",
    ];
    let mut seed = 0x173a_491e_028b_d65cu64;
    for case in 0..96 {
        let mut text = String::new();
        for _ in 0..case % 19 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            text.push_str(tokens[(seed as usize) % tokens.len()]);
        }
        let mut original = Editor::from_text(&text);
        for index in 0..original.document().paragraphs().len() {
            let paragraph_kind = match (case + index) % 4 {
                0 => ParagraphKind::Body,
                1 => ParagraphKind::Heading {
                    level: (index % 6 + 1) as u8,
                },
                2 => ParagraphKind::Bullet {
                    indent: (index % 3) as u8,
                },
                _ => ParagraphKind::Ordered {
                    indent: 1,
                    start: index as u32 + 3,
                },
            };
            kind(&mut original, index, paragraph_kind);
            let end = original.document().paragraph(index).unwrap().text().len();
            if end != 0 {
                style(
                    &mut original,
                    index,
                    0,
                    end,
                    StylePatch {
                        bold: Some(index % 2 == 0),
                        italic: Some(index % 3 == 0),
                        ..Default::default()
                    },
                );
            }
        }
        let before = Fragment::from_document(original.document());
        for query in queries {
            let normalized_query = query.replace("\r\n", "\n").replace('\r', "\n");
            let matches = text.matches(&normalized_query).count();
            for replacement in replacements {
                let normalized_replacement = replacement.replace("\r\n", "\n").replace('\r', "\n");
                let expected = text.replace(&normalized_query, &normalized_replacement);
                let mut editor = Editor::new(textloom::Document::from_fragment(&before));
                let selection = Selection::new(editor.document().end(), Position::default());
                editor.set_selection(selection).unwrap();
                assert_eq!(
                    editor
                        .replace_all(query, replacement, SearchOptions::default())
                        .unwrap(),
                    matches,
                    "match count for {text:?}, query {query:?}, replacement {replacement:?}"
                );
                assert_eq!(
                    editor.document().plain_text(),
                    expected,
                    "text {text:?}, query {query:?}, replacement {replacement:?}"
                );
                let after = Fragment::from_document(editor.document());
                if editor.can_undo() {
                    assert_eq!(editor.undo_len(), 1);
                    assert!(editor.undo());
                    assert_eq!(Fragment::from_document(editor.document()), before);
                    assert_eq!(editor.selection(), selection);
                    assert!(editor.redo());
                    assert_eq!(Fragment::from_document(editor.document()), after);
                } else {
                    assert_eq!(after, before);
                }
            }
        }
    }
}

#[test]
fn replace_all_applies_search_options_and_preserves_expanding_unicode_coordinates() {
    let options = SearchOptions {
        case_sensitive: false,
        whole_word: true,
    };
    let mut editor = Editor::from_text("cat scat catfish CAT cat");
    assert_eq!(editor.replace_all("cat", "dog", options).unwrap(), 3);
    assert_eq!(editor.document().plain_text(), "dog scat catfish dog dog");
    let mut editor = Editor::from_text("İ i\u{307}");
    assert_eq!(
        editor
            .replace_all(
                "i\u{307}",
                "L",
                SearchOptions {
                    whole_word: false,
                    ..options
                }
            )
            .unwrap(),
        2
    );
    assert_eq!(editor.document().plain_text(), "L L");
}

#[test]
fn replace_all_no_matches_and_identical_replacement_do_not_add_history_or_revision() {
    let mut editor = Editor::from_text("a a");
    let selection = Selection::new(Position::new(0, 3), Position::new(0, 1));
    editor.set_selection(selection).unwrap();
    let revision = editor.document().revision();
    let paragraphs = editor.document().paragraphs().to_vec();
    for query in ["missing", ""] {
        assert_eq!(
            editor
                .replace_all(query, "x", SearchOptions::default())
                .unwrap(),
            0
        );
        assert_eq!(editor.selection(), selection);
    }
    assert_eq!(
        editor
            .replace_all("a", "a", SearchOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(editor.undo_len(), 0);
    assert_eq!(editor.document().revision(), revision);
    assert!(Arc::ptr_eq(
        &paragraphs[0],
        &editor.document().paragraphs()[0]
    ));
}

#[test]
fn find_next_and_previous_skip_selected_matches_wrap_and_leave_misses_unchanged() {
    let mut editor = Editor::from_text("a x a\na");
    let options = SearchOptions::default();
    assert!(editor.find_next("a", options, false).unwrap());
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 0), Position::new(0, 1))
    );
    assert!(editor.find_next("a", options, false).unwrap());
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 4), Position::new(0, 5))
    );
    assert!(editor.find_next("a", options, false).unwrap());
    let last = editor.selection();
    assert_eq!(
        last,
        Selection::new(Position::new(1, 0), Position::new(1, 1))
    );
    assert!(!editor.find_next("a", options, false).unwrap());
    assert_eq!(editor.selection(), last);
    assert!(editor.find_next("a", options, true).unwrap());
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 0), Position::new(0, 1))
    );
    assert!(!editor.find_previous("a", options, false).unwrap());
    assert!(editor.find_previous("a", options, true).unwrap());
    assert_eq!(editor.selection(), last);
    assert!(editor.find_previous("a", options, false).unwrap());
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 4), Position::new(0, 5))
    );
    let selected = editor.selection();
    for query in ["missing", ""] {
        assert!(!editor.find_next(query, options, true).unwrap());
        assert!(!editor.find_previous(query, options, true).unwrap());
        assert_eq!(editor.selection(), selected);
    }
    assert_eq!(editor.undo_len(), 0);
    assert_eq!(editor.document().revision(), 0);
}

#[test]
fn find_navigation_handles_backward_selections_and_crossparagraph_matches() {
    let mut editor = Editor::from_text("one\ntwo\none\ntwo");
    editor
        .set_selection(Selection::new(Position::new(1, 3), Position::new(0, 0)))
        .unwrap();
    assert!(
        editor
            .find_next("one\ntwo", SearchOptions::default(), false)
            .unwrap()
    );
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(2, 0), Position::new(3, 3))
    );
    assert!(
        editor
            .find_previous("one\ntwo", SearchOptions::default(), false)
            .unwrap()
    );
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 0), Position::new(1, 3))
    );
}

#[test]
fn word_deletion_includes_intervening_whitespace_and_remains_one_undo_step() {
    let mut backward = Editor::from_text("hello   world!");
    caret(&mut backward, 0, 8);
    backward.delete_word_backward().unwrap();
    assert_eq!(backward.document().plain_text(), "world!");
    assert_eq!(backward.selection(), Selection::default());
    assert_eq!(backward.undo_len(), 1);
    assert!(backward.undo());
    assert_eq!(backward.selection(), Selection::caret(Position::new(0, 8)));
    assert_eq!(backward.document().plain_text(), "hello   world!");
    let mut forward = Editor::from_text("hello   world!");
    caret(&mut forward, 0, 5);
    forward.delete_word_forward().unwrap();
    assert_eq!(forward.document().plain_text(), "hello!");
    assert_eq!(forward.selection(), Selection::caret(Position::new(0, 5)));
    assert!(forward.undo());
    assert_eq!(forward.document().plain_text(), "hello   world!");
}

#[test]
fn word_deletion_preserves_unicode_graphemes_and_selection_direction_on_undo() {
    let mut editor = Editor::from_text("cafe\u{301}  👩‍💻 word");
    editor.delete_word_forward().unwrap();
    assert_eq!(editor.document().plain_text(), "  👩‍💻 word");
    assert!(
        editor
            .document()
            .validate_position(editor.selection().focus)
            .is_ok()
    );
    editor.delete_word_forward().unwrap();
    assert_eq!(editor.document().plain_text(), "");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "  👩‍💻 word");
    let mut editor = Editor::from_text("abc def");
    let selection = Selection::new(Position::new(0, 6), Position::new(0, 1));
    editor.set_selection(selection).unwrap();
    editor.delete_word_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "af");
    assert!(editor.undo());
    assert_eq!(editor.selection(), selection);
    editor.delete_word_forward().unwrap();
    assert_eq!(editor.document().plain_text(), "af");
}

#[test]
fn word_deletion_crosses_paragraph_breaks_and_document_edges_are_noops() {
    let mut editor = Editor::from_text("one\ntwo");
    caret(&mut editor, 1, 0);
    editor.delete_word_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "onetwo");
    assert_eq!(editor.selection().focus, Position::new(0, 3));
    assert!(editor.undo());
    caret(&mut editor, 0, 3);
    editor.delete_word_forward().unwrap();
    assert_eq!(editor.document().plain_text(), "onetwo");
    assert!(editor.undo());
    editor.clear_history();
    caret(&mut editor, 0, 0);
    editor.delete_word_backward().unwrap();
    let end = editor.document().end();
    editor.set_selection(Selection::caret(end)).unwrap();
    editor.delete_word_forward().unwrap();
    assert_eq!(editor.undo_len(), 0);
    assert_eq!(editor.document().plain_text(), "one\ntwo");
}

#[test]
fn indent_mixed_selection_excludes_endpoint_at_next_paragraph_start() {
    let mut editor = Editor::from_text("body\nheading\nbullet\nordered\nexcluded\nclamp");
    kind(&mut editor, 1, ParagraphKind::Heading { level: 2 });
    kind(&mut editor, 2, ParagraphKind::Bullet { indent: 2 });
    kind(
        &mut editor,
        3,
        ParagraphKind::Ordered {
            indent: 2,
            start: 9,
        },
    );
    kind(&mut editor, 4, ParagraphKind::Bullet { indent: 1 });
    kind(&mut editor, 5, ParagraphKind::Bullet { indent: 255 });
    let selection = Selection::new(Position::new(4, 0), Position::new(0, 0));
    editor.set_selection(selection).unwrap();
    let before = Fragment::from_document(editor.document());
    editor.clear_history();
    editor.indent_list().unwrap();
    assert_eq!(editor.undo_len(), 1);
    assert_eq!(editor.selection(), selection);
    assert_eq!(
        editor.document().paragraph(2).unwrap().kind(),
        ParagraphKind::Bullet { indent: 3 }
    );
    assert_eq!(
        editor.document().paragraph(3).unwrap().kind(),
        ParagraphKind::Ordered {
            indent: 3,
            start: 9
        }
    );
    for index in [0, 1, 4, 5] {
        assert!(Arc::ptr_eq(
            &before.paragraphs()[index],
            &editor.document().paragraphs()[index]
        ));
    }
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert_eq!(editor.selection(), selection);
    assert!(editor.redo());
    assert_eq!(
        editor.document().paragraph(3).unwrap().kind(),
        ParagraphKind::Ordered {
            indent: 3,
            start: 9
        }
    );
}

#[test]
fn outdent_zero_level_items_becomes_body_and_other_items_decrease_one_level() {
    let mut editor = Editor::from_text("a\nb\nc");
    kind(&mut editor, 0, ParagraphKind::Bullet { indent: 0 });
    kind(
        &mut editor,
        1,
        ParagraphKind::Ordered {
            indent: 0,
            start: 17,
        },
    );
    kind(&mut editor, 2, ParagraphKind::Bullet { indent: 1 });
    style(
        &mut editor,
        1,
        0,
        1,
        StylePatch {
            bold: Some(true),
            ..Default::default()
        },
    );
    editor.select_all();
    let before = Fragment::from_document(editor.document());
    editor.clear_history();
    editor.outdent_list().unwrap();
    assert_eq!(editor.undo_len(), 1);
    assert_eq!(
        editor.document().paragraph(0).unwrap().kind(),
        ParagraphKind::Body
    );
    assert_eq!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Body
    );
    assert!(
        editor.document().paragraph(1).unwrap().spans()[0]
            .style
            .bold
    );
    assert_eq!(
        editor.document().paragraph(2).unwrap().kind(),
        ParagraphKind::Bullet { indent: 0 }
    );
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), before);
}

#[test]
fn list_indentation_clamps_at_maximum_and_nonlist_operations_are_noops() {
    let mut editor = Editor::from_text("clamped\nheading\nbody");
    kind(&mut editor, 0, ParagraphKind::Bullet { indent: u8::MAX });
    kind(&mut editor, 1, ParagraphKind::Heading { level: 2 });
    editor.clear_history();
    let before = Fragment::from_document(editor.document());
    let revision = editor.document().revision();
    caret(&mut editor, 0, 0);
    editor.indent_list().unwrap();
    caret(&mut editor, 1, 0);
    editor.indent_list().unwrap();
    editor.outdent_list().unwrap();
    caret(&mut editor, 2, 0);
    editor.indent_list().unwrap();
    editor.outdent_list().unwrap();
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert_eq!(editor.document().revision(), revision);
    assert_eq!(editor.undo_len(), 0);
    for (source, current) in before
        .paragraphs()
        .iter()
        .zip(editor.document().paragraphs())
    {
        assert!(Arc::ptr_eq(source, current));
    }
}

#[test]
fn operations_reject_active_ime_without_mutating_content_selection_or_history() {
    let mut editor = Editor::from_text("word");
    editor.begin_composition();
    editor.update_composition("仮", Some(0..3)).unwrap();
    let before = Fragment::from_document(editor.document());
    let composition = editor.composition().cloned();
    let selection = editor.selection();
    let options = SearchOptions::default();
    assert_eq!(
        editor.find_next("word", options, true),
        Err(Error::CompositionActive)
    );
    assert_eq!(
        editor.find_previous("word", options, true),
        Err(Error::CompositionActive)
    );
    assert_eq!(
        editor.replace_all("word", "next", options),
        Err(Error::CompositionActive)
    );
    assert_eq!(editor.delete_word_backward(), Err(Error::CompositionActive));
    assert_eq!(editor.delete_word_forward(), Err(Error::CompositionActive));
    assert_eq!(editor.indent_list(), Err(Error::CompositionActive));
    assert_eq!(editor.outdent_list(), Err(Error::CompositionActive));
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.composition(), composition.as_ref());
    assert_eq!(editor.undo_len(), 0);
}
