use std::sync::Arc;
use textloom::{
    Color, Document, Editor, Error, Fragment, HistoryLimits, InlineAttribute, InlineStyle,
    Movement, Paragraph, ParagraphKind, Position, Selection, SelectionStyle, Span, StylePatch,
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
    editor.break_history_group();
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
fn style_lookup_rejects_invalid_utf8_and_grapheme_positions() {
    let mut editor = Editor::from_text("é e\u{301} 👩‍💻");
    editor.select_all();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    let document = editor.document();
    for byte in 0..=document.end().byte + 1 {
        let position = Position::new(0, byte);
        if document.validate_position(position).is_ok() {
            assert!(
                document.style_at(position).bold,
                "valid position {position:?}"
            );
        } else {
            assert_eq!(
                document.style_at(position),
                Default::default(),
                "{position:?}"
            );
        }
    }
    assert_eq!(
        document.style_at(Position::new(usize::MAX, 0)),
        Default::default()
    );
    assert_eq!(
        document.style_at(Position::new(0, usize::MAX)),
        Default::default()
    );
    assert_eq!(
        Document::new().style_at(Position::default()),
        Default::default()
    );
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
fn unchanged_formatting_preserves_redo_and_directional_selection() {
    let mut editor = Editor::from_text("abc");
    let selection = Selection::new(Position::new(0, 3), Position::default());
    editor.set_selection(selection).unwrap();
    let bold = StylePatch {
        bold: Some(true),
        ..StylePatch::default()
    };
    editor.apply_style(bold).unwrap();
    editor
        .apply_style(StylePatch {
            underline: Some(true),
            ..StylePatch::default()
        })
        .unwrap();
    assert!(editor.undo());
    let paragraph = Arc::clone(&editor.document().paragraphs()[0]);
    let revision = editor.document().revision();
    let history = (editor.undo_len(), editor.redo_len(), editor.history_bytes());
    editor.apply_style(bold).unwrap();
    assert!(Arc::ptr_eq(&paragraph, &editor.document().paragraphs()[0]));
    assert_eq!(editor.document().revision(), revision);
    assert_eq!(editor.selection(), selection);
    assert_eq!(
        (editor.undo_len(), editor.redo_len(), editor.history_bytes()),
        history
    );
    assert!(editor.typing_style().bold);
    assert!(!editor.typing_style().underline);
    assert!(editor.redo());
    assert!(editor.document().style_at(Position::new(0, 1)).underline);
    assert_eq!(editor.selection(), selection);
}

#[test]
fn formatting_only_paragraph_breaks_updates_typing_style_without_history() {
    let mut editor = Editor::from_text("a\n\nb");
    let original = editor.document().clone();
    let selection = Selection::new(Position::new(2, 0), Position::new(0, 1));
    editor.set_selection(selection).unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..StylePatch::default()
        })
        .unwrap();
    assert_eq!(editor.document(), &original);
    for (before, after) in original
        .paragraphs()
        .iter()
        .zip(editor.document().paragraphs())
    {
        assert!(Arc::ptr_eq(before, after));
    }
    assert_eq!(editor.selection(), selection);
    assert!(editor.typing_style().bold);
    assert!(!editor.can_undo());
}

#[test]
fn selection_style_reports_independent_mixed_attributes_inside_unicode_runs() {
    let mut editor = Editor::from_text("a👩‍💻e\u{301}z");
    let red = Color([255, 0, 0, 255]);
    let blue = Color([0, 0, 255, 255]);
    editor
        .set_selection(Selection::new(Position::new(0, 1), Position::new(0, 15)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            underline: Some(true),
            code: Some(true),
            foreground: Some(Some(red)),
            background: Some(Some(Color::YELLOW)),
            ..Default::default()
        })
        .unwrap();
    editor
        .set_selection(Selection::new(Position::new(0, 1), Position::new(0, 12)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            italic: Some(true),
            ..Default::default()
        })
        .unwrap();
    editor
        .set_selection(Selection::new(Position::new(0, 12), Position::new(0, 15)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            strikethrough: Some(true),
            foreground: Some(Some(blue)),
            ..Default::default()
        })
        .unwrap();
    let selection = Selection::new(Position::new(0, 15), Position::new(0, 1));
    editor.set_selection(selection).unwrap();
    assert_eq!(
        editor.typing_style(),
        InlineStyle {
            bold: true,
            italic: true,
            underline: true,
            code: true,
            foreground: Some(red),
            background: Some(Color::YELLOW),
            ..InlineStyle::default()
        }
    );
    let expected = SelectionStyle {
        bold: Some(true),
        italic: None,
        underline: Some(true),
        strikethrough: None,
        code: Some(true),
        foreground: None,
        background: Some(Some(Color::YELLOW)),
    };
    let before = editor.document().clone();
    let history = (editor.undo_len(), editor.redo_len(), editor.history_bytes());
    assert_eq!(editor.selection_style(), expected);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.document(), &before);
    assert_eq!(
        (editor.undo_len(), editor.redo_len(), editor.history_bytes()),
        history
    );
    editor
        .set_selection(Selection::new(selection.focus, selection.anchor))
        .unwrap();
    assert_eq!(editor.selection_style(), expected);
    assert!(editor.undo());
    editor.set_selection(selection).unwrap();
    assert_eq!(
        editor.selection_style(),
        SelectionStyle {
            strikethrough: Some(false),
            foreground: Some(Some(red)),
            ..expected
        }
    );
    assert!(editor.redo());
    editor.set_selection(selection).unwrap();
    assert_eq!(editor.selection_style(), expected);
    editor.clear_formatting().unwrap();
    assert_eq!(
        editor.selection_style(),
        SelectionStyle::from(InlineStyle::default())
    );
}

#[test]
fn selection_style_ignores_empty_paragraphs_and_excluded_endpoint_text() {
    let mut editor = Editor::from_text("one\n\nTWO\nexcluded");
    let color = Color([42, 71, 149, 128]);
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(2, 3)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            foreground: Some(Some(color)),
            ..Default::default()
        })
        .unwrap();
    editor
        .set_selection(Selection::new(Position::new(3, 0), Position::new(0, 0)))
        .unwrap();
    let expected = SelectionStyle::from(InlineStyle {
        bold: true,
        foreground: Some(color),
        ..Default::default()
    });
    assert_eq!(editor.selection_style(), expected);
    editor
        .set_selection(Selection::new(Position::new(0, 1), Position::new(2, 2)))
        .unwrap();
    assert_eq!(editor.selection_style(), expected);
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(3, 1)))
        .unwrap();
    assert_eq!(editor.selection_style().bold, None);
    assert_eq!(editor.selection_style().foreground, None);
    assert_eq!(editor.selection_style().italic, Some(false));
}

#[test]
fn selection_style_uses_pending_typing_for_carets_and_separator_only_selections() {
    let mut editor = Editor::from_text("a\n\nb");
    caret(&mut editor, 0, 1);
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            italic: Some(true),
            foreground: Some(Some(Color([1, 2, 3, 4]))),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        editor.document().style_at(Position::new(0, 1)),
        InlineStyle::default()
    );
    assert_eq!(
        editor.selection_style(),
        SelectionStyle::from(editor.typing_style())
    );
    assert_eq!(editor.selection_style().bold, Some(true));
    editor.clear_formatting().unwrap();
    assert_eq!(
        editor.selection_style(),
        SelectionStyle::from(InlineStyle::default())
    );
    editor
        .set_selection(Selection::new(Position::new(0, 1), Position::new(2, 0)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            underline: Some(true),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(editor.selection_style().underline, Some(true));
    assert_eq!(
        editor.selection_style(),
        SelectionStyle::from(editor.typing_style())
    );
    assert_eq!(editor.undo_len(), 0);
    editor.begin_composition();
    editor.update_composition("仮", Some(0..3)).unwrap();
    let composition = editor.composition().cloned();
    assert_eq!(editor.selection_style().underline, Some(true));
    assert_eq!(editor.composition(), composition.as_ref());
}

#[test]
fn selection_style_all_mixed_attributes_remain_mixed_after_later_matching_runs() {
    let mut editor = Editor::from_text("abc");
    let emphasis = StylePatch {
        bold: Some(true),
        italic: Some(true),
        underline: Some(true),
        strikethrough: Some(true),
        code: Some(true),
        foreground: Some(Some(Color([10, 20, 30, 40]))),
        background: Some(Some(Color([50, 60, 70, 80]))),
    };
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 1)))
        .unwrap();
    editor.apply_style(emphasis).unwrap();
    editor
        .set_selection(Selection::new(Position::new(0, 2), Position::new(0, 3)))
        .unwrap();
    editor.apply_style(emphasis).unwrap();
    editor.select_all();
    assert_eq!(
        editor.selection_style(),
        SelectionStyle {
            bold: None,
            italic: None,
            underline: None,
            strikethrough: None,
            code: None,
            foreground: None,
            background: None,
        }
    );
}

#[test]
fn clear_formatting_resets_every_attribute_and_restores_rich_state_on_undo() {
    let mut editor = Editor::from_text("left\ncafe\u{301}\nkeep");
    editor.select_all();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            italic: Some(true),
            underline: Some(true),
            strikethrough: Some(true),
            code: Some(true),
            foreground: Some(Some(Color([23, 71, 149, 128]))),
            background: Some(Some(Color::LIGHT_GREEN)),
        })
        .unwrap();
    caret(&mut editor, 0, 0);
    editor
        .set_paragraph_kind(ParagraphKind::Heading { level: 2 })
        .unwrap();
    caret(&mut editor, 1, 0);
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 3 })
        .unwrap();
    let selection = Selection::new(Position::new(1, 6), Position::new(0, 1));
    editor.set_selection(selection).unwrap();
    editor.clear_history();
    let before = Fragment::from_document(editor.document());
    let style = editor.typing_style();
    editor.clear_formatting().unwrap();
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), InlineStyle::default());
    assert_eq!(editor.undo_len(), 1);
    let first = editor.document().paragraph(0).unwrap();
    assert_eq!(first.kind(), ParagraphKind::Heading { level: 2 });
    assert_eq!(first.spans().len(), 2);
    assert_eq!(first.spans()[0].range, 0..1);
    assert_eq!(first.spans()[0].style, style);
    assert_eq!(first.spans()[1].style, InlineStyle::default());
    let second = editor.document().paragraph(1).unwrap();
    assert_eq!(second.kind(), ParagraphKind::Bullet { indent: 3 });
    assert_eq!(second.spans()[0].style, InlineStyle::default());
    assert!(Arc::ptr_eq(
        &before.paragraphs()[2],
        &editor.document().paragraphs()[2]
    ));
    let after = Fragment::from_document(editor.document());
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), style);
    assert!(editor.redo());
    assert_eq!(Fragment::from_document(editor.document()), after);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), InlineStyle::default());
}

#[test]
fn clear_formatting_at_a_caret_only_resets_typing_and_breaks_its_group() {
    let mut editor = Editor::default();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            foreground: Some(Some(Color([1, 2, 3, 4]))),
            ..Default::default()
        })
        .unwrap();
    editor.insert_text("a").unwrap();
    let before = editor.document().clone();
    let selection = editor.selection();
    editor.clear_formatting().unwrap();
    assert_eq!(editor.document(), &before);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), InlineStyle::default());
    assert_eq!(editor.undo_len(), 1);
    editor.insert_text("b").unwrap();
    assert_eq!(editor.undo_len(), 2);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "a");
    assert_eq!(editor.typing_style(), InlineStyle::default());
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "");
    assert!(editor.typing_style().bold);
}

#[test]
fn clear_formatting_noops_preserve_revision_history_and_active_ime() {
    let mut editor = Editor::from_text("plain");
    editor.select_all();
    let before = editor.document().clone();
    editor.clear_formatting().unwrap();
    assert_eq!(editor.document(), &before);
    assert_eq!(editor.undo_len(), 0);
    editor.begin_composition();
    editor.update_composition("仮", Some(0..3)).unwrap();
    let selection = editor.selection();
    let composition = editor.composition().cloned();
    assert_eq!(editor.clear_formatting(), Err(Error::CompositionActive));
    assert_eq!(editor.document(), &before);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.composition(), composition.as_ref());
    assert_eq!(editor.undo_len(), 0);
}

#[test]
fn fill_color_formats_independently_types_on_and_undoes_like_excel() {
    let mut editor = Editor::from_text("Total due");
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 5)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            background: Some(Some(Color::YELLOW)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(editor.undo_len(), 1);
    let style = editor.selection_style();
    assert_eq!(style.background, Some(Some(Color::YELLOW)));
    assert_eq!((style.bold, style.foreground), (Some(false), Some(None)));
    let spans = editor.document().paragraph(0).unwrap().spans();
    assert_eq!(spans.len(), 2);
    assert_eq!(
        spans[0].style,
        InlineStyle {
            background: Some(Color::YELLOW),
            ..InlineStyle::default()
        }
    );
    assert_eq!(spans[1].style, InlineStyle::default());
    editor.select_all();
    assert_eq!(editor.selection_style().background, None);
    assert_eq!(StylePatch::from(editor.selection_style()).background, None);

    caret(&mut editor, 0, 5);
    assert_eq!(editor.typing_style().background, Some(Color::YELLOW));
    editor.insert_text("s").unwrap();
    assert_eq!(
        editor.document().style_at(Position::new(0, 6)).background,
        Some(Color::YELLOW)
    );

    editor.select_all();
    let filled = Fragment::from_document(editor.document());
    editor
        .apply_style(StylePatch {
            background: Some(None),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(editor.selection_style().background, Some(None));
    assert!(
        editor.document().paragraph(0).unwrap().spans()[0]
            .style
            .background
            .is_none()
    );
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), filled);
    assert_eq!(editor.selection_style().background, None);
    assert!(editor.redo());
    assert_eq!(editor.selection_style().background, Some(None));

    caret(&mut editor, 0, 0);
    let steps = editor.undo_len();
    editor
        .apply_style(StylePatch {
            background: Some(Some(Color::LIGHT_GREEN)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(editor.undo_len(), steps);
    editor.insert_text(">").unwrap();
    assert_eq!(
        editor.document().style_at(Position::new(0, 1)).background,
        Some(Color::LIGHT_GREEN)
    );
    editor.clear_formatting().unwrap();
    assert_eq!(editor.typing_style().background, None);
}

#[test]
fn standard_colors_follow_excels_palette_order() {
    let palette: Vec<_> = Color::STANDARD
        .iter()
        .map(|(name, Color([red, green, blue, alpha]))| {
            assert_eq!(*alpha, 255, "{name} is opaque");
            (*name, format!("#{red:02X}{green:02X}{blue:02X}"))
        })
        .collect();
    let expected = [
        ("Dark Red", "#C00000"),
        ("Red", "#FF0000"),
        ("Orange", "#FFC000"),
        ("Yellow", "#FFFF00"),
        ("Light Green", "#92D050"),
        ("Green", "#00B050"),
        ("Light Blue", "#00B0F0"),
        ("Blue", "#0070C0"),
        ("Dark Blue", "#002060"),
        ("Purple", "#7030A0"),
    ];
    assert_eq!(palette, expected.map(|(name, hex)| (name, hex.to_owned())));
    let constants = [
        Color::DARK_RED,
        Color::RED,
        Color::ORANGE,
        Color::YELLOW,
        Color::LIGHT_GREEN,
        Color::GREEN,
        Color::LIGHT_BLUE,
        Color::BLUE,
        Color::DARK_BLUE,
        Color::PURPLE,
    ];
    assert_eq!(Color::STANDARD.map(|(_, color)| color), constants);
}

#[test]
fn unicode_typing_coalesces_joined_graphemes_into_one_reversible_delta() {
    let mut editor = Editor::default();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    let before_style = editor.typing_style();
    for text in ["a", "\u{301}", "👩", "\u{200d}", "💻"] {
        editor.insert_text(text).unwrap();
        assert_eq!(editor.undo_len(), 1);
        assert!(
            editor
                .document()
                .validate_position(editor.selection().focus)
                .is_ok()
        );
    }
    let after = Fragment::from_document(editor.document());
    let selection = editor.selection();
    let bytes = editor.history_bytes();
    assert_eq!(editor.document().plain_text(), "a\u{301}👩‍💻");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "");
    assert_eq!(editor.selection(), Selection::default());
    assert_eq!(editor.typing_style(), before_style);
    assert_eq!(editor.history_bytes(), bytes);
    assert!(editor.redo());
    assert_eq!(Fragment::from_document(editor.document()), after);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), before_style);
    assert_eq!(editor.history_bytes(), bytes);
}

#[test]
fn either_zero_history_limit_disables_retention_and_reenabling_starts_a_fresh_chain() {
    for limits in [
        HistoryLimits {
            max_entries: 0,
            max_bytes: usize::MAX,
        },
        HistoryLimits {
            max_entries: usize::MAX,
            max_bytes: 0,
        },
    ] {
        let mut editor = Editor::default();
        editor.insert_text("old").unwrap();
        assert!(editor.undo());
        editor.set_history_limits(limits);
        for text in ["a", "b", "c"] {
            editor.insert_text(text).unwrap();
            assert_eq!(editor.undo_len(), 0);
            assert_eq!(editor.redo_len(), 0);
            assert_eq!(editor.history_bytes(), 0);
        }
        editor.set_history_limits(HistoryLimits::default());
        editor.insert_text("new").unwrap();
        assert_eq!(editor.undo_len(), 1);
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "abc");
        assert!(!editor.undo());
        assert!(editor.redo());
        assert_eq!(editor.document().plain_text(), "abcnew");
    }
}

#[test]
fn list_enter_continues_then_empty_enter_exits() {
    let mut editor = Editor::from_text("first");
    editor
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: 1,
            start: Some(7),
        })
        .unwrap();
    caret(&mut editor, 0, 5);
    editor.insert_paragraph().unwrap();
    let continued = ParagraphKind::Ordered {
        indent: 1,
        start: None,
    };
    assert_eq!(editor.document().paragraph(1).unwrap().kind(), continued);
    assert_eq!(numbers(&editor), [Some(7), Some(8)]);
    editor.insert_paragraph().unwrap();
    assert_eq!(editor.document().paragraphs().len(), 2);
    assert!(matches!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Ordered { indent: 0, .. }
    ));
    editor.insert_paragraph().unwrap();
    assert_eq!(editor.document().paragraphs().len(), 2);
    assert_eq!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Body
    );
    assert_eq!(editor.selection(), Selection::caret(Position::new(1, 0)));
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(editor.document().paragraph(1).unwrap().kind(), continued);
    assert_eq!(numbers(&editor), [Some(7), Some(8)]);
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
fn empty_caret_insertions_preserve_pending_style_savepoints_and_redo() {
    let mut editor = Editor::from_text("e\u{301}x");
    caret(&mut editor, 0, 4);
    editor.insert_text("!").unwrap();
    assert!(editor.undo());
    caret(&mut editor, 0, 3);
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    editor.mark_saved();
    let document = editor.document().clone();
    let selection = editor.selection();
    let style = editor.typing_style();
    let history = (editor.undo_len(), editor.redo_len(), editor.history_bytes());

    editor.insert_text("").unwrap();
    assert_eq!(editor.document(), &document);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), style);
    editor.update_composition("仮", Some(0..3)).unwrap();
    assert_eq!(editor.insert_text(""), Err(Error::CompositionActive));
    assert_eq!(
        editor.insert_fragment(&Fragment::from_text("")),
        Err(Error::CompositionActive)
    );
    assert_eq!(editor.composition().unwrap().text, "仮");
    editor.commit_composition("").unwrap();
    assert!(editor.composition().is_none());
    assert_eq!(editor.document(), &document);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), style);
    editor.insert_fragment(&Fragment::from_text("")).unwrap();
    assert_eq!(editor.document(), &document);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), document.style_at(selection.focus));
    assert_eq!(
        (editor.undo_len(), editor.redo_len(), editor.history_bytes()),
        history
    );
    assert!(!editor.is_modified());
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "e\u{301}x!");
    assert!(editor.undo());
    assert!(!editor.is_modified());
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
fn history_limits_discard_farthest_redo_and_preserve_the_next_steps() {
    let mut editor = Editor::default();
    for text in ["a", "b", "c", "d"] {
        editor.break_history_group();
        editor.insert_text(text).unwrap();
    }
    while editor.undo() {}
    assert_eq!(editor.redo_len(), 4);
    editor.set_history_limits(HistoryLimits {
        max_entries: 2,
        max_bytes: usize::MAX,
    });
    assert_eq!(editor.redo_len(), 2);
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "a");
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "ab");
    assert!(!editor.redo());
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "");

    editor.set_history_limits(HistoryLimits {
        max_entries: 0,
        max_bytes: 0,
    });
    assert_eq!(editor.redo_len(), 0);
    assert_eq!(editor.history_bytes(), 0);
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

fn numbers(editor: &Editor) -> Vec<Option<u32>> {
    (0..editor.document().paragraphs().len())
        .map(|index| editor.document().list_number(index))
        .collect()
}

fn ordered(start: Option<u32>) -> ParagraphKind {
    ParagraphKind::Ordered { indent: 0, start }
}

#[test]
fn ordered_selection_numbers_items_consecutively_and_undo_restores_blocks() {
    let mut editor = Editor::from_text("one\ntwo\nthree");
    editor.select_all();
    editor.set_paragraph_kind(ordered(Some(4))).unwrap();
    assert_eq!(
        ordered_kinds(&editor),
        [ordered(Some(4)), ordered(None), ordered(None)]
    );
    assert_eq!(numbers(&editor), [Some(4), Some(5), Some(6)]);
    assert!(editor.undo());
    assert_eq!(ordered_kinds(&editor), [ParagraphKind::Body; 3]);
    assert_eq!(numbers(&editor), [None; 3]);
    assert!(editor.redo());
    assert_eq!(numbers(&editor), [Some(4), Some(5), Some(6)]);
}

#[test]
fn ordered_restart_updates_the_following_run_and_survives_plain_typing() {
    let mut editor = Editor::from_text("one\ntwo\nthree\nfour");
    editor.select_all();
    editor.set_paragraph_kind(ordered(None)).unwrap();
    assert_eq!(numbers(&editor), [Some(1), Some(2), Some(3), Some(4)]);
    let following = editor.document().paragraphs()[2..].to_vec();
    caret(&mut editor, 1, 0);
    editor.set_paragraph_kind(ordered(Some(10))).unwrap();
    assert_eq!(numbers(&editor), [Some(1), Some(10), Some(11), Some(12)]);
    assert_eq!(editor.document().paragraphs()[2..], following);
    caret(&mut editor, 0, 1);
    editor.insert_text("X").unwrap();
    assert_eq!(numbers(&editor), [Some(1), Some(10), Some(11), Some(12)]);
    editor.delete_backward().unwrap();
    assert_eq!(numbers(&editor), [Some(1), Some(10), Some(11), Some(12)]);
    assert!(editor.undo());
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(numbers(&editor), [Some(1), Some(2), Some(3), Some(4)]);
    caret(&mut editor, 1, 0);
    editor.set_paragraph_kind(ordered(Some(10))).unwrap();
    editor.set_paragraph_kind(ordered(None)).unwrap();
    assert_eq!(ordered_kinds(&editor), [ordered(None); 4]);
}

#[test]
fn mid_list_enter_renumbers_following_items_and_shares_their_text() {
    let mut editor = Editor::from_text("one\ntwo\nthree");
    editor.select_all();
    editor.set_paragraph_kind(ordered(Some(4))).unwrap();
    let before = editor.document().paragraphs().to_vec();
    let trailing_text = before[2].text().as_ptr();
    let trailing_spans = before[2].spans().as_ptr();
    caret(&mut editor, 1, 1);
    let history = editor.history_bytes();
    editor.insert_paragraph().unwrap();
    assert_eq!(editor.document().plain_text(), "one\nt\nwo\nthree");
    assert_eq!(numbers(&editor), [Some(4), Some(5), Some(6), Some(7)]);
    assert!(Arc::ptr_eq(&before[0], &editor.document().paragraphs()[0]));
    assert!(Arc::ptr_eq(&before[2], &editor.document().paragraphs()[3]));
    assert_eq!(
        editor.history_bytes() - history,
        2 * std::mem::size_of::<Paragraph>() + "two".len() + 2 * std::mem::size_of::<Span>()
    );
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
    assert_eq!(numbers(&editor), [Some(4), Some(5), Some(6), Some(7)]);
}

#[test]
fn joining_or_deleting_ordered_items_renumbers_in_the_same_undo_step() {
    let mut editor = Editor::from_text("one\ntwo\nthree");
    editor.select_all();
    editor.set_paragraph_kind(ordered(Some(4))).unwrap();
    let before = editor.document().paragraphs().to_vec();
    caret(&mut editor, 1, 0);
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "one\ntwo\nthree");
    assert_eq!(
        editor.document().paragraph(1).unwrap().kind(),
        ParagraphKind::Body
    );
    assert_eq!(numbers(&editor), [Some(4), None, Some(5)]);
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "onetwo\nthree");
    assert_eq!(numbers(&editor), [Some(4), Some(5)]);
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(editor.document().paragraphs(), before);
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(1, 0)))
        .unwrap();
    editor.insert_text("").unwrap();
    assert_eq!(editor.document().plain_text(), "two\nthree");
    assert_eq!(numbers(&editor), [Some(4), Some(5)]);
    assert!(editor.undo());
    assert_eq!(editor.document().paragraphs(), before);
}

#[test]
fn ordered_continuations_skip_nested_items_and_body_text_until_a_heading() {
    let mut editor = Editor::from_text("one\ntwo\nnested\nbody\nfive");
    editor.select_all();
    editor.set_paragraph_kind(ordered(None)).unwrap();
    caret(&mut editor, 2, 0);
    let nested = ParagraphKind::Ordered {
        indent: 1,
        start: Some(7),
    };
    editor.set_paragraph_kind(nested).unwrap();
    assert_eq!(
        numbers(&editor),
        [Some(1), Some(2), Some(7), Some(3), Some(4)]
    );
    caret(&mut editor, 3, 0);
    editor.set_paragraph_kind(ParagraphKind::Body).unwrap();
    caret(&mut editor, 0, 0);
    editor.set_paragraph_kind(ordered(Some(10))).unwrap();
    assert_eq!(
        ordered_kinds(&editor),
        [
            ordered(Some(10)),
            ordered(None),
            nested,
            ParagraphKind::Body,
            ordered(None)
        ]
    );
    assert_eq!(
        numbers(&editor),
        [Some(10), Some(11), Some(7), None, Some(12)]
    );
    caret(&mut editor, 0, 3);
    editor.insert_paragraph().unwrap();
    assert_eq!(
        numbers(&editor),
        [Some(10), Some(11), Some(12), Some(7), None, Some(13)]
    );
    caret(&mut editor, 4, 0);
    editor
        .set_paragraph_kind(ParagraphKind::Heading { level: 2 })
        .unwrap();
    assert_eq!(
        numbers(&editor),
        [Some(10), Some(11), Some(12), Some(7), None, Some(1)]
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

fn bold() -> StylePatch {
    StylePatch {
        bold: Some(true),
        ..Default::default()
    }
}

fn bold_prefix_editor() -> Editor {
    let mut editor = Editor::from_text("Bplain");
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 1)))
        .unwrap();
    editor.apply_style(bold()).unwrap();
    editor
}

fn span_bold(editor: &Editor) -> Vec<(std::ops::Range<usize>, bool)> {
    editor.document().paragraphs()[0]
        .spans()
        .iter()
        .map(|span| (span.range.clone(), span.style.bold))
        .collect()
}

#[test]
fn typing_over_a_selection_uses_its_first_grapheme_style_in_either_direction() {
    let forward = Selection::new(Position::new(0, 1), Position::new(0, 6));
    let backward = Selection::new(Position::new(0, 6), Position::new(0, 1));
    for selection in [forward, backward] {
        let mut editor = bold_prefix_editor();
        editor.set_selection(selection).unwrap();
        assert_eq!(editor.selection_style().bold, Some(false));
        assert!(!editor.typing_style().bold);
        editor.insert_text("x").unwrap();
        assert_eq!(span_bold(&editor), [(0..1, true), (1..2, false)]);

        let mut editor = bold_prefix_editor();
        editor.set_selection(selection).unwrap();
        editor.update_composition("日本", None).unwrap();
        editor.commit_composition("日本").unwrap();
        assert_eq!(span_bold(&editor), [(0..1, true), (1..7, false)]);
    }

    let mut editor = bold_prefix_editor();
    caret(&mut editor, 0, 3);
    for _ in 0..2 {
        editor
            .move_cursor(Movement::GraphemeBackward, true)
            .unwrap();
    }
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 3), Position::new(0, 1))
    );
    assert!(!editor.typing_style().bold);

    editor
        .set_selection(Selection::new(Position::new(0, 3), Position::new(0, 0)))
        .unwrap();
    assert!(editor.typing_style().bold);
    let mut editor = Editor::from_text("a\n\nb");
    editor
        .set_selection(Selection::new(Position::new(0, 1), Position::new(2, 0)))
        .unwrap();
    assert_eq!(
        editor.typing_style(),
        editor.document().style_at(Position::new(2, 0))
    );
}

#[test]
fn no_op_moves_keep_a_pending_caret_style() {
    let mut editor = Editor::from_text("plain");
    caret(&mut editor, 0, 5);
    editor.apply_style(bold()).unwrap();
    for movement in [
        Movement::GraphemeForward,
        Movement::WordForward,
        Movement::ParagraphEnd,
        Movement::DocumentEnd,
        Movement::ParagraphDown,
    ] {
        editor.move_cursor(movement, false).unwrap();
        assert!(editor.typing_style().bold, "{movement:?}");
    }
    editor.insert_text("!").unwrap();
    assert_eq!(span_bold(&editor), [(0..5, false), (5..6, true)]);
    editor.move_cursor(Movement::DocumentStart, false).unwrap();
    assert!(!editor.typing_style().bold);
}

#[test]
fn ime_commits_join_adjacent_typing_and_selection_or_break_commits_do_not() {
    let mut editor = Editor::default();
    for text in ["a", "b", "c"] {
        editor.commit_composition(text).unwrap();
    }
    assert_eq!(editor.undo_len(), 1);

    let mut editor = Editor::default();
    for syllable in ["가", "나", "다"] {
        editor.update_composition(syllable, None).unwrap();
        editor.commit_composition(syllable).unwrap();
    }
    assert_eq!(editor.document().plain_text(), "가나다");
    assert_eq!(editor.undo_len(), 1);

    let mut editor = Editor::default();
    editor.insert_text("ab").unwrap();
    editor.update_composition("にほん", Some(9..9)).unwrap();
    editor.commit_composition("日本").unwrap();
    editor.insert_text("c").unwrap();
    assert_eq!(editor.document().plain_text(), "ab日本c");
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "");

    let mut editor = Editor::default();
    editor.insert_text("a").unwrap();
    editor.commit_composition("\n").unwrap();
    editor.commit_composition("b").unwrap();
    assert_eq!(editor.undo_len(), 3);
    editor.update_composition("x", None).unwrap();
    assert!(editor.cancel_composition());
    editor.commit_composition("c").unwrap();
    assert_eq!(editor.undo_len(), 4);

    let mut editor = Editor::from_text("abc");
    caret(&mut editor, 0, 3);
    editor.insert_text("d").unwrap();
    let selection = Selection::new(Position::new(0, 0), Position::new(0, 2));
    editor.set_selection(selection).unwrap();
    editor.update_composition("x", None).unwrap();
    editor.commit_composition("x").unwrap();
    editor.commit_composition("y").unwrap();
    assert_eq!(editor.document().plain_text(), "xycd");
    assert_eq!(editor.undo_len(), 2);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "abcd");
    assert_eq!(editor.selection(), selection);
}

#[test]
fn consecutive_deletions_coalesce_by_direction_within_a_paragraph() {
    let mut editor = Editor::from_text("hello world");
    caret(&mut editor, 0, 11);
    for _ in 0..5 {
        editor.delete_backward().unwrap();
    }
    assert_eq!(editor.document().plain_text(), "hello ");
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "hello world");
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 11)));
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "hello ");

    caret(&mut editor, 0, 3);
    editor.delete_backward().unwrap();
    editor.delete_backward().unwrap();
    editor.delete_forward().unwrap();
    editor.delete_forward().unwrap();
    assert_eq!(editor.document().plain_text(), "h ");
    assert_eq!(editor.undo_len(), 3);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "hlo ");
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 1)));

    let mut editor = Editor::default();
    editor.insert_text("ab").unwrap();
    editor.delete_backward().unwrap();
    editor.insert_text("c").unwrap();
    assert_eq!(editor.document().plain_text(), "ac");
    assert_eq!(editor.undo_len(), 3);

    let mut editor = Editor::from_text("ab\ncd");
    caret(&mut editor, 1, 1);
    for _ in 0..3 {
        editor.delete_backward().unwrap();
    }
    assert_eq!(editor.document().plain_text(), "ad");
    assert_eq!(editor.undo_len(), 3);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "abd");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "ab\nd");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "ab\ncd");
}

#[test]
fn holding_backspace_keeps_earlier_history_within_default_limits() {
    let mut editor = Editor::from_text(&"x".repeat(40_000));
    caret(&mut editor, 0, 40_000);
    editor.insert_text("typed").unwrap();
    for _ in 0..300 {
        editor.delete_backward().unwrap();
    }
    assert_eq!(editor.document().paragraphs()[0].text().len(), 39_705);
    assert_eq!(editor.undo_len(), 2);
    assert!(editor.history_bytes() < 300_000);
    assert!(editor.undo());
    assert_eq!(editor.document().paragraphs()[0].text().len(), 40_005);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "x".repeat(40_000));
}

#[test]
fn typing_over_a_selection_continues_as_one_undo_step() {
    let forward = Selection::new(Position::new(0, 6), Position::new(0, 11));
    let backward = Selection::new(Position::new(0, 11), Position::new(0, 6));
    for selection in [forward, backward] {
        let mut editor = Editor::from_text("hello world");
        editor.set_selection(selection).unwrap();
        for character in ["t", "h", "e", "r", "e"] {
            editor.insert_text(character).unwrap();
        }
        assert_eq!(editor.document().plain_text(), "hello there");
        assert_eq!(editor.undo_len(), 1);
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "hello world");
        assert_eq!(editor.selection(), selection);
        assert!(editor.redo());
        assert_eq!(editor.document().plain_text(), "hello there");
    }

    let mut editor = Editor::from_text("ab\ncd\nef");
    let selection = Selection::new(Position::new(0, 1), Position::new(2, 1));
    editor.set_selection(selection).unwrap();
    editor.insert_text("x").unwrap();
    editor.insert_text("y").unwrap();
    assert_eq!(editor.document().plain_text(), "axyf");
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "ab\ncd\nef");
    assert_eq!(editor.selection(), selection);
}

#[test]
fn transactions_record_several_commands_as_one_undo_step() {
    let mut editor = Editor::from_text("link");
    caret(&mut editor, 0, 4);
    let value = editor
        .transaction(|editor| {
            editor.replace_range(Position::new(0, 0)..Position::new(0, 0), "a ")?;
            editor.set_selection(Selection::new(Position::new(0, 2), Position::new(0, 6)))?;
            editor.apply_style(bold())?;
            editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })?;
            Ok(7)
        })
        .unwrap();
    assert_eq!(value, 7);
    let edited = editor.document().paragraphs().to_vec();
    assert_eq!(editor.undo_len(), 1);
    assert_eq!(span_bold(&editor), [(0..2, false), (2..6, true)]);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "link");
    assert_eq!(span_bold(&editor), [(0..4, false)]);
    assert_eq!(
        editor.document().paragraphs()[0].kind(),
        ParagraphKind::Body
    );
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 4)));
    assert!(editor.redo());
    assert_eq!(editor.document().paragraphs(), edited);
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 2), Position::new(0, 6))
    );

    editor.insert_text("x").unwrap();
    editor.transaction(|_| Ok(())).unwrap();
    editor.insert_text("y").unwrap();
    assert_eq!(editor.undo_len(), 3);
    editor
        .transaction(|editor| editor.insert_text("z"))
        .unwrap();
    editor.insert_text("w").unwrap();
    assert_eq!(editor.undo_len(), 5);
}

#[test]
fn failed_transactions_roll_back_without_changing_history() {
    let mut editor = Editor::from_text("one");
    caret(&mut editor, 0, 3);
    editor.insert_text(" two").unwrap();
    editor.break_history_group();
    editor.insert_text(" three").unwrap();
    assert!(editor.undo());
    let content = editor.document().paragraphs().to_vec();
    let selection = Selection::new(Position::new(0, 7), Position::new(0, 4));
    editor.set_selection(selection).unwrap();
    let style = editor.typing_style();
    let history = (editor.undo_len(), editor.redo_len(), editor.history_bytes());
    let result: Result<(), Error> = editor.transaction(|editor| {
        editor.insert_text("X")?;
        editor.select_all();
        editor.apply_style(bold())?;
        assert!(!editor.undo());
        assert!(!editor.redo());
        editor.set_selection(Selection::caret(Position::new(9, 0)))
    });
    assert_eq!(result, Err(Error::InvalidPosition(Position::new(9, 0))));
    assert_eq!(editor.document().paragraphs(), content);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.typing_style(), style);
    assert_eq!(
        (editor.undo_len(), editor.redo_len(), editor.history_bytes()),
        history
    );
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "one two three");

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = editor.transaction(|editor| -> Result<(), Error> {
            editor.insert_text("lost")?;
            panic!("host failure");
        });
    }));
    assert!(panicked.is_err());
    assert_eq!(editor.document().plain_text(), "one two three");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "one two");
}

#[test]
fn nested_transactions_join_the_outer_step_and_inner_errors_revert_inner_edits() {
    let mut editor = Editor::default();
    editor
        .transaction(|editor| {
            editor.insert_text("a")?;
            let inner = editor.transaction(|editor| {
                editor.insert_text("b")?;
                editor.update_composition("preedit", None)?;
                editor.insert_text("c")
            });
            assert_eq!(inner, Err(Error::CompositionActive));
            assert!(editor.composition().is_none());
            assert_eq!(editor.document().plain_text(), "a");
            editor.transaction(|editor| editor.insert_text("d"))?;
            editor.insert_text("e")
        })
        .unwrap();
    assert_eq!(editor.document().plain_text(), "ade");
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "");
}

#[test]
fn transactions_respect_history_limits_and_roll_back_without_history() {
    let mut editor = Editor::default();
    editor.set_history_limits(HistoryLimits {
        max_entries: 0,
        max_bytes: 0,
    });
    let result = editor.transaction(|editor| {
        editor.insert_text("abc")?;
        editor.replace_range(Position::new(0, 9)..Position::new(0, 9), "x")
    });
    assert!(result.is_err());
    assert_eq!(editor.document().plain_text(), "");
    editor
        .transaction(|editor| editor.insert_text("kept"))
        .unwrap();
    assert_eq!(editor.document().plain_text(), "kept");
    assert!(!editor.can_undo());

    let mut editor = Editor::default();
    editor.set_history_limits(HistoryLimits {
        max_entries: 2,
        max_bytes: usize::MAX,
    });
    for text in ["a", "b", "c"] {
        editor
            .transaction(|editor| {
                editor.insert_text(text)?;
                editor.insert_text("\n")
            })
            .unwrap();
    }
    assert_eq!(editor.undo_len(), 2);
    editor
        .transaction(|editor| {
            editor.insert_text("d")?;
            editor.clear_history();
            editor.insert_text("e")
        })
        .unwrap();
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "a\nb\nc\n");
}

#[test]
fn transactions_keep_an_editor_that_replaced_the_original() {
    let mut editor = Editor::from_text("old");
    editor
        .transaction(|editor| {
            editor.insert_text("x")?;
            *editor = Editor::from_text("opened");
            Ok(())
        })
        .unwrap();
    assert_eq!(editor.document().plain_text(), "opened");
    assert!(!editor.can_undo());

    let result: Result<(), Error> = editor.transaction(|editor| {
        *editor = Editor::from_text("abc");
        editor.select_all();
        Err(Error::InvalidRange)
    });
    assert_eq!(result, Err(Error::InvalidRange));
    assert_eq!(editor.selected_text(), "abc");
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = editor.transaction(|editor| -> Result<(), Error> {
            *editor = Editor::from_text("new");
            panic!("host failure");
        });
    }));
    assert!(panicked.is_err());
    assert_eq!(editor.document().plain_text(), "new");

    let mut editor = Editor::from_text("a");
    editor
        .transaction(|editor| {
            editor.replace_range(Position::new(0, 1)..Position::new(0, 1), "b")?;
            *editor = editor.clone();
            editor.insert_text("c")
        })
        .unwrap();
    assert_eq!(editor.document().plain_text(), "abc");
    assert_eq!(editor.undo_len(), 2);
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "a");
}

#[test]
fn displaced_transaction_snapshots_keep_undo_after_success_error_and_panic() {
    for outcome in 0..3 {
        let mut editor = Editor::from_text("abc");
        caret(&mut editor, 0, 1);
        let before = editor.selection();
        let mut displaced = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            editor.transaction(|editor| -> Result<(), Error> {
                editor.insert_text("é")?;
                editor.apply_style(bold())?;
                editor.mark_saved();
                editor.update_composition("仮", Some(0..3))?;
                displaced = Some(std::mem::replace(editor, Editor::from_text("replacement")));
                match outcome {
                    0 => Ok(()),
                    1 => Err(Error::InvalidRange),
                    _ => panic!("host failure after moving its editor"),
                }
            })
        }));
        assert_eq!(result.is_err(), outcome == 2);
        if let Ok(result) = result {
            assert_eq!(result.is_err(), outcome == 1);
        }
        assert_eq!(editor.document().plain_text(), "replacement");
        assert!(!editor.can_undo());
        let mut displaced = displaced.unwrap();
        assert_eq!(displaced.document().plain_text(), "aébc");
        assert_eq!(displaced.composition().unwrap().text, "仮");
        assert!(!displaced.is_modified());
        assert_eq!((displaced.undo_len(), displaced.redo_len()), (1, 0));
        let bytes = displaced.history_bytes();
        assert!(bytes > 0);
        assert_eq!(displaced.history_bytes(), bytes);
        assert!(displaced.undo());
        assert_eq!(displaced.document().plain_text(), "abc");
        assert_eq!(displaced.selection(), before);
        assert!(!displaced.typing_style().bold);
        assert!(displaced.composition().is_none());
        assert!(displaced.is_modified());
        assert!(displaced.redo());
        assert_eq!(displaced.document().plain_text(), "aébc");
        assert_eq!(displaced.selection(), Selection::caret(Position::new(0, 3)));
        assert!(displaced.typing_style().bold);
        assert!(!displaced.is_modified());
    }
}

#[test]
fn returned_transaction_snapshots_seal_before_later_caret_and_typing_changes() {
    for read_history_first in [false, true] {
        let mut owner = Editor::from_text("abc");
        let mut moved = owner
            .transaction(|editor| {
                editor.insert_text("x")?;
                editor.apply_style(bold())?;
                Ok(std::mem::take(editor))
            })
            .unwrap();
        assert_eq!(owner.document().plain_text(), "");
        if read_history_first {
            assert_eq!(moved.undo_len(), 1);
            assert!(moved.history_bytes() > 0);
        }
        moved
            .set_selection(Selection::caret(moved.document().end()))
            .unwrap();
        moved
            .apply_style(StylePatch {
                italic: Some(true),
                ..StylePatch::default()
            })
            .unwrap();
        moved.update_composition("pending", None).unwrap();
        assert!(moved.undo());
        assert_eq!(moved.document().plain_text(), "abc");
        assert_eq!(moved.selection(), Selection::default());
        assert_eq!(moved.typing_style(), InlineStyle::default());
        assert!(moved.redo());
        assert_eq!(moved.document().plain_text(), "xabc");
        assert_eq!(moved.selection(), Selection::caret(Position::new(0, 1)));
        assert!(moved.typing_style().bold);
        assert!(!moved.typing_style().italic);
    }
}

#[test]
fn detached_transaction_queries_apply_limits_and_keep_noop_redo() {
    for limits in [
        HistoryLimits {
            max_entries: 1,
            max_bytes: usize::MAX,
        },
        HistoryLimits {
            max_entries: 0,
            max_bytes: usize::MAX,
        },
        HistoryLimits {
            max_entries: usize::MAX,
            max_bytes: 0,
        },
    ] {
        let mut owner = Editor::from_text("abc");
        owner.insert_text("prior").unwrap();
        owner.set_history_limits(limits);
        let mut moved = owner
            .transaction(|editor| {
                editor.insert_text("x")?;
                Ok(std::mem::take(editor))
            })
            .unwrap();
        let retained = usize::from(limits.max_entries != 0 && limits.max_bytes != 0);
        assert_eq!(moved.undo_len(), retained);
        assert_eq!(moved.redo_len(), 0);
        assert_eq!(moved.history_bytes() > 0, retained != 0);
        assert_eq!(moved.undo(), retained != 0);
        if retained != 0 {
            assert_eq!(moved.document().plain_text(), "priorabc");
            assert!(!moved.can_undo());
        }
    }

    let mut owner = Editor::from_text("abc");
    owner.insert_text("x").unwrap();
    assert!(owner.undo());
    let bytes = owner.history_bytes();
    let mut moved = owner
        .transaction(|editor| {
            editor.apply_style(bold())?;
            Ok(std::mem::take(editor))
        })
        .unwrap();
    assert_eq!((moved.undo_len(), moved.redo_len()), (0, 1));
    assert_eq!(moved.history_bytes(), bytes);
    assert!(moved.redo());
    assert_eq!(moved.document().plain_text(), "xabc");

    for clear in [false, true] {
        let mut owner = Editor::default();
        let mut moved = owner
            .transaction(|editor| {
                editor.insert_text("kept")?;
                Ok(std::mem::take(editor))
            })
            .unwrap();
        if clear {
            moved.clear_history();
        } else {
            moved.set_history_limits(HistoryLimits {
                max_entries: 0,
                max_bytes: 0,
            });
            moved.set_history_limits(HistoryLimits::default());
        }
        assert_eq!(
            (moved.undo_len(), moved.redo_len(), moved.history_bytes()),
            (0, 0, 0)
        );
        moved.insert_text("!").unwrap();
        assert!(moved.undo());
        assert_eq!(moved.document().plain_text(), "kept");
        assert!(!moved.can_undo());
    }
}

#[test]
fn nested_displacement_can_return_to_its_outer_guard_or_escape_it() {
    for reattach in [false, true] {
        let mut owner = Editor::from_text("original");
        let mut escaped = None;
        let result: Result<(), Error> = owner.transaction(|outer| {
            outer.insert_text("a")?;
            let mut moved = outer.transaction(|inner| {
                inner.insert_text("b")?;
                Ok(std::mem::take(inner))
            })?;
            // The outer transaction still owns this snapshot until its scope ends.
            assert!(!moved.undo());
            moved.insert_text("c")?;
            if reattach {
                *outer = moved;
            } else {
                escaped = Some(moved);
            }
            Err(Error::InvalidRange)
        });
        assert_eq!(result, Err(Error::InvalidRange));
        if reattach {
            assert_eq!(owner.document().plain_text(), "original");
            assert!(!owner.can_undo());
        } else {
            assert_eq!(owner.document().plain_text(), "");
            let mut moved = escaped.unwrap();
            assert_eq!(moved.document().plain_text(), "abcoriginal");
            assert_eq!(moved.undo_len(), 1);
            assert!(moved.undo());
            assert_eq!(moved.document().plain_text(), "original");
        }
    }
}

#[test]
fn swapped_transaction_editors_keep_their_own_guard_identity() {
    for swap_back in [false, true] {
        let mut left = Editor::from_text("left");
        let mut right = Editor::from_text("right");
        let result: Result<(), Error> = left.transaction(|left| {
            left.insert_text("a")?;
            right.transaction(|right| {
                right.insert_text("b")?;
                std::mem::swap(left, right);
                Ok(())
            })?;
            assert_eq!(left.undo_len(), 1);
            assert!(!right.undo());
            if swap_back {
                std::mem::swap(left, &mut right);
            }
            Err(Error::InvalidRange)
        });
        assert_eq!(result, Err(Error::InvalidRange));
        if swap_back {
            assert_eq!(left.document().plain_text(), "left");
            assert!(!left.can_undo());
            assert_eq!(right.document().plain_text(), "bright");
            assert!(right.undo());
            assert_eq!(right.document().plain_text(), "right");
        } else {
            assert_eq!(left.document().plain_text(), "bright");
            assert_eq!(right.document().plain_text(), "aleft");
            assert!(left.undo());
            assert!(right.undo());
            assert_eq!(left.document().plain_text(), "right");
            assert_eq!(right.document().plain_text(), "left");
        }
    }
}

#[test]
fn detached_outer_scope_waits_for_a_live_transaction_on_another_thread() {
    for outcome in 0..3 {
        let mut owner = Editor::from_text("original");
        let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
        let (continue_sender, continue_receiver) = std::sync::mpsc::channel();
        let worker = owner
            .transaction(|editor| {
                editor.insert_text("outer ")?;
                let mut moved = std::mem::take(editor);
                let worker = std::thread::spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        moved.transaction(|editor| -> Result<(), Error> {
                            editor.insert_text("inner ")?;
                            ready_sender.send(()).unwrap();
                            continue_receiver.recv().unwrap();
                            // The outer guard has ended, but this guard is still active.
                            assert_eq!(
                                (editor.undo_len(), editor.redo_len(), editor.history_bytes()),
                                (0, 0, 0)
                            );
                            editor.insert_text("later ")?;
                            match outcome {
                                0 => Ok(()),
                                1 => Err(Error::InvalidRange),
                                _ => panic!("worker failure"),
                            }
                        })
                    }));
                    assert_eq!(result.is_err(), outcome == 2);
                    if let Ok(result) = result {
                        assert_eq!(result.is_err(), outcome == 1);
                    }
                    moved
                });
                ready_receiver.recv().unwrap();
                Ok(worker)
            })
            .unwrap();
        continue_sender.send(()).unwrap();
        let mut moved = worker.join().unwrap();
        assert_eq!(owner.document().plain_text(), "");
        let expected = if outcome == 0 {
            "outer inner later original"
        } else {
            "outer original"
        };
        assert_eq!(moved.document().plain_text(), expected);
        assert_eq!(moved.undo_len(), 1);
        assert!(moved.undo());
        assert_eq!(moved.document().plain_text(), "original");
        assert!(moved.redo());
        assert_eq!(moved.document().plain_text(), expected);
    }
}

#[test]
fn transactions_merge_successive_edits_of_one_paragraph() {
    let large = "x".repeat(200_000);
    let mut editor = Editor::from_text(&large);
    editor
        .replace_range(Position::new(0, 0)..Position::new(0, 0), "earlier ")
        .unwrap();
    editor
        .set_selection(Selection::caret(editor.document().end()))
        .unwrap();
    let bytes = editor.history_bytes();
    editor
        .transaction(|editor| {
            for _ in 0..50 {
                editor.insert_text("token ")?;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(editor.undo_len(), 2);
    assert!(editor.history_bytes() - bytes < 2 * large.len());
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), format!("earlier {large}"));
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), large);

    let mut editor = Editor::default();
    editor
        .transaction(|editor| {
            editor.insert_text("a")?;
            let inner = editor.transaction(|editor| {
                editor.insert_text("b")?;
                editor.insert_text("c")?;
                editor.replace_range(Position::new(0, 9)..Position::new(0, 9), "")
            });
            assert!(inner.is_err());
            assert_eq!(editor.document().plain_text(), "a");
            editor.transaction(|editor| editor.insert_text("d"))?;
            editor.insert_text("e")
        })
        .unwrap();
    assert_eq!(editor.document().plain_text(), "ade");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "");
}

#[test]
fn transactions_record_a_step_only_when_their_commands_edit() {
    let mut editor = Editor::from_text("abc");
    editor
        .replace_range(Position::new(0, 3)..Position::new(0, 3), "d")
        .unwrap();
    assert!(editor.undo());
    editor
        .transaction(|editor| {
            editor.select_all();
            editor.replace_range(Position::new(0, 0)..Position::new(0, 0), "")
        })
        .unwrap();
    assert_eq!((editor.undo_len(), editor.redo_len()), (0, 1));
    editor
        .transaction(|editor| {
            editor.replace_range(Position::new(0, 0)..Position::new(0, 0), "x")?;
            editor.replace_range(Position::new(0, 0)..Position::new(0, 1), "")
        })
        .unwrap();
    assert_eq!(editor.document().plain_text(), "abc");
    assert_eq!((editor.undo_len(), editor.redo_len()), (1, 0));
}

#[test]
fn saved_state_follows_content_through_undo_redo_and_eviction() {
    let mut editor = Editor::from_text("draft");
    assert!(!editor.is_modified());
    caret(&mut editor, 0, 5);
    editor.insert_text("!").unwrap();
    assert!(editor.is_modified());
    assert!(editor.undo());
    assert!(!editor.is_modified());
    assert!(editor.redo());
    assert!(editor.is_modified());
    editor.mark_saved();
    assert!(!editor.is_modified());

    editor.select_all();
    caret(&mut editor, 0, 6);
    editor.apply_style(bold()).unwrap();
    assert!(!editor.is_modified());

    editor.insert_text("x").unwrap();
    assert!(editor.is_modified());
    editor.delete_backward().unwrap();
    assert!(!editor.is_modified());
    editor.select_all();
    editor.apply_style(bold()).unwrap();
    assert!(editor.is_modified());
    assert!(editor.undo());
    assert!(!editor.is_modified());
    editor
        .set_paragraph_kind(ParagraphKind::Heading { level: 1 })
        .unwrap();
    assert!(editor.is_modified());
    assert!(editor.undo());
    assert!(!editor.is_modified());

    caret(&mut editor, 0, 6);
    editor.insert_text("?").unwrap();
    editor.mark_saved();
    assert!(editor.undo());
    assert!(editor.is_modified());
    editor.insert_text("-").unwrap();
    assert!(!editor.can_redo());
    assert!(editor.is_modified());

    let mut editor = Editor::from_text("a");
    editor.set_history_limits(HistoryLimits {
        max_entries: 1,
        max_bytes: usize::MAX,
    });
    caret(&mut editor, 0, 1);
    editor.mark_saved();
    editor.insert_text("b").unwrap();
    editor.break_history_group();
    editor.insert_text("c").unwrap();
    assert!(editor.undo());
    assert!(!editor.undo());
    assert!(editor.is_modified());
    editor.clear_history();
    assert!(editor.is_modified());
    editor.delete_backward().unwrap();
    assert!(!editor.is_modified());
    editor.set_history_limits(HistoryLimits {
        max_entries: 0,
        max_bytes: 0,
    });
    editor.insert_text("z").unwrap();
    assert!(editor.is_modified());
}

#[test]
fn repeated_saved_state_reads_follow_all_edit_commands_and_disabled_history() {
    use textloom::{Link, SearchOptions};

    type Edit = fn(&mut Editor) -> Result<(), Error>;
    let edits: &[(&str, Edit)] = &[
        ("insert", |editor| editor.insert_text("x")),
        ("replace range", |editor| {
            editor.replace_range(Position::new(0, 0)..Position::new(0, 1), "x")
        }),
        ("fragment", |editor| {
            editor.insert_fragment(&Fragment::from_text("x\ny"))
        }),
        ("paragraph", Editor::insert_paragraph),
        ("line break", Editor::insert_line_break),
        ("delete forward", Editor::delete_forward),
        ("delete backward", |editor| {
            editor.set_selection(Selection::caret(Position::new(0, 2)))?;
            editor.delete_backward()
        }),
        ("delete word", Editor::delete_word_forward),
        ("style", |editor| {
            editor.select_all();
            editor.apply_style(bold())
        }),
        ("link", |editor| {
            editor.select_all();
            editor.set_link(Some(Link::new("https://example.com")?))
        }),
        ("insert link", |editor| {
            editor.insert_link("link", Link::new("https://example.com")?)
        }),
        ("paragraph kind", |editor| {
            editor.set_paragraph_kind(ParagraphKind::Quote)
        }),
        ("numbered list", |editor| editor.apply_numbered_list(0)),
        ("indent", Editor::indent_list),
        ("outdent", Editor::outdent_list),
        ("replace all", |editor| {
            editor.replace_all("a", "x", SearchOptions::default())?;
            Ok(())
        }),
        ("replace match", |editor| {
            editor.set_selection(Selection::new(Position::new(0, 0), Position::new(0, 1)))?;
            assert!(editor.replace_selected_match("a", "x", SearchOptions::default())?);
            Ok(())
        }),
        ("composition", |editor| {
            editor.update_composition("preedit", None)?;
            editor.commit_composition("x")
        }),
    ];
    for disabled in [false, true] {
        for &(name, edit) in edits {
            let mut editor = Editor::from_text("abc\nnext");
            editor
                .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
                .unwrap();
            editor.clear_history();
            editor.mark_saved();
            if disabled {
                editor.set_history_limits(HistoryLimits {
                    max_entries: 0,
                    max_bytes: 0,
                });
            }
            for _ in 0..2 {
                assert!(!editor.is_modified(), "{name}");
            }
            edit(&mut editor).unwrap();
            for _ in 0..2 {
                assert!(editor.is_modified(), "{name}, disabled={disabled}");
            }
            if !disabled {
                assert!(editor.undo(), "{name}");
                for _ in 0..2 {
                    assert!(!editor.is_modified(), "{name}");
                }
                assert!(editor.redo(), "{name}");
                for _ in 0..2 {
                    assert!(editor.is_modified(), "{name}");
                }
            }
            editor.mark_saved();
            for _ in 0..2 {
                assert!(!editor.is_modified(), "{name}");
            }
        }
    }
}

#[test]
fn cached_saved_state_survives_clone_branches_and_nested_transaction_rollbacks() {
    let mut editor = Editor::from_text("abc");
    editor.insert_text("x").unwrap();
    assert!(editor.is_modified());
    assert!(editor.undo());
    assert!(!editor.is_modified());
    let mut branch = editor.clone();
    branch.insert_text("y").unwrap();
    assert!(branch.is_modified());
    assert!(!editor.is_modified());
    branch.mark_saved();
    editor.insert_text("z").unwrap();
    assert!(editor.is_modified());
    assert!(!branch.is_modified());

    let result: Result<(), Error> = branch.transaction(|editor| {
        editor.insert_text("outer")?;
        assert!(editor.is_modified());
        let result: Result<(), Error> = editor.transaction(|editor| {
            editor.select_all();
            editor.insert_text("inner")?;
            editor.mark_saved();
            assert!(!editor.is_modified());
            Err(Error::InvalidRange)
        });
        assert_eq!(result, Err(Error::InvalidRange));
        assert!(editor.is_modified());
        Err(Error::InvalidRange)
    });
    assert_eq!(result, Err(Error::InvalidRange));
    assert_eq!(branch.document().plain_text(), "yabc");
    assert!(branch.is_modified());
    branch.select_all();
    branch.insert_text("inner").unwrap();
    assert!(!branch.is_modified());

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = branch.transaction(|editor| -> Result<(), Error> {
            editor.insert_text("lost")?;
            assert!(editor.is_modified());
            panic!("host failure");
        });
    }));
    assert!(result.is_err());
    assert_eq!(branch.document().plain_text(), "inner");
    assert!(!branch.is_modified());
}

#[test]
fn backspace_at_list_item_start_outdents_before_joining() {
    let mut editor = Editor::from_text("body\nitem");
    caret(&mut editor, 1, 0);
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 2 })
        .unwrap();
    let steps = editor.undo_len();
    editor.delete_backward().unwrap();
    assert_eq!(
        editor.document().paragraphs()[1].kind(),
        ParagraphKind::Bullet { indent: 1 }
    );
    editor.delete_backward().unwrap();
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "body\nitem");
    assert_eq!(
        editor.document().paragraphs()[1].kind(),
        ParagraphKind::Body
    );
    assert_eq!(editor.selection(), Selection::caret(Position::new(1, 0)));
    assert_eq!(editor.undo_len(), steps + 3);
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "bodyitem");
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(
        editor.document().paragraphs()[1].kind(),
        ParagraphKind::Bullet { indent: 0 }
    );

    let mut editor = Editor::from_text("first");
    editor
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: 0,
            start: None,
        })
        .unwrap();
    editor.delete_backward().unwrap();
    assert_eq!(
        editor.document().paragraphs()[0].kind(),
        ParagraphKind::Body
    );
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
        .unwrap();
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 2)))
        .unwrap();
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "rst");
    assert_eq!(
        editor.document().paragraphs()[0].kind(),
        ParagraphKind::Bullet { indent: 0 }
    );
}

#[test]
fn enter_on_empty_nested_bullet_outdents_one_level_per_press() {
    let mut editor = Editor::from_text("item");
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 1 })
        .unwrap();
    caret(&mut editor, 0, 4);
    editor.insert_paragraph().unwrap();
    let steps = editor.undo_len();
    editor.insert_paragraph().unwrap();
    assert_eq!(editor.document().paragraphs().len(), 2);
    assert_eq!(
        editor.document().paragraphs()[1].kind(),
        ParagraphKind::Bullet { indent: 0 }
    );
    editor.insert_paragraph().unwrap();
    assert_eq!(
        editor.document().paragraphs()[1].kind(),
        ParagraphKind::Body
    );
    assert_eq!(editor.undo_len(), steps + 2);
    editor.insert_paragraph().unwrap();
    assert_eq!(editor.document().paragraphs().len(), 3);
}

#[test]
fn cloned_editors_branch_history_and_debug_summarizes_state() {
    let mut editor = Editor::from_text("base");
    caret(&mut editor, 0, 4);
    editor.insert_text(" one").unwrap();
    let mut branch = editor.clone();
    branch.insert_text(" two").unwrap();
    assert_eq!(branch.document().plain_text(), "base one two");
    assert!(branch.undo());
    assert_eq!(branch.document().plain_text(), "base");
    assert_eq!(editor.document().plain_text(), "base one");
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "base");

    let mut editor = Editor::default();
    let mut inner = None;
    editor
        .transaction(|editor| {
            editor.insert_text("a")?;
            editor.insert_text("\nb")?;
            inner = Some(editor.clone());
            editor.insert_text("c")
        })
        .unwrap();
    let mut inner = inner.unwrap();
    assert_eq!(inner.document().plain_text(), "a\nb");
    assert_eq!(inner.undo_len(), 1);
    assert!(inner.undo());
    assert_eq!(inner.document().plain_text(), "");
    assert!(inner.redo());

    let debug = format!("{editor:?}");
    assert!(debug.starts_with("Editor {"), "{debug}");
    assert!(debug.contains("undo_len: 1"), "{debug}");
    assert!(debug.contains("modified: true"), "{debug}");
}

#[test]
fn undo_redo_and_select_all_cancel_preedit_without_changing_content() {
    let replacement = Selection::new(Position::new(0, 1), Position::new(0, 3));
    for command in 0..3 {
        let mut editor = Editor::from_text("abcd");
        editor.set_selection(replacement).unwrap();
        editor.update_composition("にほ", Some(6..6)).unwrap();
        let applied = match command {
            0 => editor.undo(),
            1 => editor.redo(),
            _ => {
                editor.select_all();
                false
            }
        };
        assert!(!applied);
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "abcd");
        assert_eq!(editor.document().revision(), 0);
        let expected = if command == 2 {
            Selection::new(Position::default(), Position::new(0, 4))
        } else {
            replacement
        };
        assert_eq!(editor.selection(), expected);
    }
}

#[test]
fn word_selection_expands_word_boundaries_inside_graphemes() {
    for grapheme in ["\u{600}🦀", "\u{600}👩‍💻", "\u{600}🇺🇸", "\u{600}。"] {
        let mut editor = Editor::from_text(&format!("prefix {grapheme}"));
        let start = Position::new(0, "prefix ".len());
        let end = editor.document().end();
        for position in [start, end] {
            editor.select_word_at(position).unwrap();
            assert_eq!(
                editor.selection(),
                Selection::new(start, end),
                "{grapheme:?}"
            );
            assert_eq!(editor.selected_text(), grapheme);
        }
        let inside = Position::new(0, start.byte + '\u{600}'.len_utf8());
        assert_eq!(
            editor.select_word_at(inside),
            Err(Error::InvalidPosition(inside))
        );
        assert_eq!(editor.selection(), Selection::new(start, end));
        editor.insert_text("replacement").unwrap();
        assert_eq!(editor.document().plain_text(), "prefix replacement");
        assert!(editor.undo());
        assert_eq!(editor.selected_text(), grapheme);
    }
}

#[test]
fn word_and_paragraph_selection_follow_pointer_conventions() {
    let mut editor = Editor::from_text("cafe\u{301} 👩‍💻 end.\n\nnext");
    let word = |editor: &mut Editor, byte: usize| {
        editor.select_word_at(Position::new(0, byte)).unwrap();
        let range = editor.selection().range();
        (range.start.byte, range.end.byte)
    };
    assert_eq!(word(&mut editor, 2), (0, 6));
    assert_eq!(word(&mut editor, 6), (6, 7));
    assert_eq!(word(&mut editor, 7), (7, 18));
    assert_eq!(word(&mut editor, 19), (19, 22));
    assert_eq!(word(&mut editor, 23), (22, 23));
    assert_eq!(
        editor.document().word_range_at(Position::new(1, 0)),
        Ok(Position::new(1, 0)..Position::new(1, 0))
    );
    let selection = editor.selection();
    assert_eq!(
        editor.select_word_at(Position::new(0, 4)),
        Err(Error::InvalidPosition(Position::new(0, 4)))
    );
    assert_eq!(editor.selection(), selection);

    editor.select_paragraph(2).unwrap();
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(2, 0), Position::new(2, 4))
    );
    assert_eq!(
        editor.select_paragraph(3),
        Err(Error::InvalidPosition(Position::new(3, 0)))
    );
    editor.update_composition("x", None).unwrap();
    editor.select_paragraph(1).unwrap();
    assert!(editor.composition().is_none());
    assert!(editor.selection().is_caret());
}

#[test]
fn selected_paragraphs_and_block_kind_follow_paragraph_command_rules() {
    let mut editor = Editor::from_text("title\none\ntwo\nthree");
    editor
        .set_paragraph_kind(ParagraphKind::Heading { level: 1 })
        .unwrap();
    editor
        .set_selection(Selection::new(Position::new(1, 0), Position::new(3, 5)))
        .unwrap();
    editor.set_paragraph_kind(ordered(Some(1))).unwrap();
    caret(&mut editor, 2, 0);
    editor.indent_list().unwrap();

    let mut select = |anchor: (usize, usize), focus: (usize, usize)| {
        editor
            .set_selection(Selection::new(
                Position::new(anchor.0, anchor.1),
                Position::new(focus.0, focus.1),
            ))
            .unwrap();
        (
            editor.selected_paragraphs(),
            editor.selection_paragraph_kind(),
        )
    };
    let heading = Some(ParagraphKind::Heading { level: 1 });
    assert_eq!(select((0, 2), (0, 2)), (0..1, heading));
    assert_eq!(select((0, 1), (1, 0)), (0..1, heading));
    assert_eq!(select((1, 0), (0, 1)), (0..1, heading));
    assert_eq!(select((0, 1), (1, 1)), (0..2, None));
    assert_eq!(select((3, 5), (1, 0)), (1..4, Some(ordered(Some(1)))));
    assert_eq!(select((0, 0), (3, 5)), (0..4, None));
}

#[test]
fn style_toggles_match_the_adapter_shortcut_rule() {
    let mut editor = bold_prefix_editor();
    editor.select_all();
    assert_eq!(editor.selection_style().bold, None);
    editor.toggle_style(InlineAttribute::Bold).unwrap();
    assert_eq!(editor.selection_style().bold, Some(true));
    editor.toggle_style(InlineAttribute::Bold).unwrap();
    assert_eq!(editor.selection_style().bold, Some(false));
    for attribute in [
        InlineAttribute::Italic,
        InlineAttribute::Underline,
        InlineAttribute::Strikethrough,
        InlineAttribute::Code,
    ] {
        editor.toggle_style(attribute).unwrap();
    }
    assert_eq!(
        editor.selection_style(),
        SelectionStyle {
            bold: Some(false),
            italic: Some(true),
            underline: Some(true),
            strikethrough: Some(true),
            code: Some(true),
            foreground: Some(None),
            background: Some(None),
        }
    );
    let steps = editor.undo_len();
    caret(&mut editor, 0, 6);
    editor.toggle_style(InlineAttribute::Italic).unwrap();
    assert!(!editor.typing_style().italic);
    assert_eq!(editor.undo_len(), steps);
    editor.update_composition("x", None).unwrap();
    assert_eq!(
        editor.toggle_style(InlineAttribute::Bold),
        Err(Error::CompositionActive)
    );
}

#[test]
fn style_patches_convert_from_complete_and_uniform_styles() {
    let red = Color([255, 0, 0, 255]);
    let style = InlineStyle {
        italic: true,
        foreground: Some(red),
        background: Some(Color::YELLOW),
        ..InlineStyle::default()
    };
    let mut target = InlineStyle {
        bold: true,
        ..InlineStyle::default()
    };
    StylePatch::from(style).apply(&mut target);
    assert_eq!(target, style);

    let mut editor = Editor::from_text("ab cd");
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 1)))
        .unwrap();
    editor.apply_style(bold()).unwrap();
    editor.select_all();
    editor
        .apply_style(StylePatch {
            italic: Some(true),
            ..Default::default()
        })
        .unwrap();
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 2)))
        .unwrap();
    let painter = StylePatch::from(editor.selection_style());
    assert_eq!(painter.bold, None);
    assert_eq!(painter.italic, Some(true));
}

#[test]
fn delete_to_paragraph_start_is_one_step_and_never_joins_paragraphs() {
    let mut editor = Editor::from_text("one\ntwo three");
    caret(&mut editor, 1, 4);
    editor.delete_to_paragraph_start().unwrap();
    assert_eq!(editor.document().plain_text(), "one\nthree");
    assert_eq!(editor.selection(), Selection::caret(Position::new(1, 0)));
    editor.delete_to_paragraph_start().unwrap();
    assert_eq!(editor.document().plain_text(), "one\nthree");
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.selection(), Selection::caret(Position::new(1, 4)));
    let selection = Selection::new(Position::new(1, 5), Position::new(0, 1));
    editor.set_selection(selection).unwrap();
    editor.delete_to_paragraph_start().unwrap();
    assert_eq!(editor.document().plain_text(), "ohree");
    assert!(editor.undo());
    assert_eq!(editor.selection(), selection);
    editor.update_composition("x", None).unwrap();
    assert_eq!(
        editor.delete_to_paragraph_start(),
        Err(Error::CompositionActive)
    );
}

#[test]
#[deny(unreachable_patterns)]
fn public_enums_keep_fallback_arms_reachable() {
    let kind = match ParagraphKind::Body {
        ParagraphKind::Body => "body",
        ParagraphKind::Heading { .. } => "heading",
        ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. } => "list",
        _ => "other",
    };
    let movement = match Movement::DocumentEnd {
        Movement::GraphemeBackward
        | Movement::GraphemeForward
        | Movement::WordBackward
        | Movement::WordForward => "inline",
        Movement::ParagraphStart
        | Movement::ParagraphEnd
        | Movement::DocumentStart
        | Movement::DocumentEnd => "edge",
        Movement::ParagraphUp | Movement::ParagraphDown => "vertical",
        _ => "other",
    };
    let attribute = match InlineAttribute::Code {
        InlineAttribute::Bold | InlineAttribute::Italic => "emphasis",
        InlineAttribute::Underline | InlineAttribute::Strikethrough => "decoration",
        InlineAttribute::Code => "code",
        _ => "other",
    };
    assert_eq!([kind, movement, attribute], ["body", "edge", "code"]);
}

#[test]
fn soft_line_breaks_keep_paragraph_kinds_and_join_typing_undo() {
    for kind in [
        ParagraphKind::Body,
        ParagraphKind::Heading { level: 2 },
        ParagraphKind::Bullet { indent: 1 },
    ] {
        let mut editor = Editor::from_text("ab");
        editor.set_paragraph_kind(kind).unwrap();
        caret(&mut editor, 0, 2);
        editor.insert_line_break().unwrap();
        editor.insert_line_break().unwrap();
        editor.insert_text("cd").unwrap();
        let paragraph = editor.document().paragraph(0).unwrap();
        assert_eq!(editor.document().paragraphs().len(), 1, "{kind:?}");
        assert_eq!(paragraph.kind(), kind);
        assert_eq!(paragraph.text(), "ab\u{2028}\u{2028}cd");
        assert_eq!(editor.selection(), Selection::caret(Position::new(0, 10)));
        assert_eq!(editor.document().plain_text(), "ab\n\ncd");
        assert_eq!(
            editor.undo_len(),
            usize::from(kind != ParagraphKind::Body) + 1
        );
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "ab");
        assert_eq!(editor.selection(), Selection::caret(Position::new(0, 2)));
        assert!(editor.redo());
        assert_eq!(editor.document().paragraph(0).unwrap().text().len(), 10);
    }

    let mut editor = Editor::from_text("");
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
        .unwrap();
    editor.insert_line_break().unwrap();
    assert_eq!(
        editor.document().paragraph(0).unwrap().kind(),
        ParagraphKind::Bullet { indent: 0 }
    );
    assert_eq!(editor.document().paragraph(0).unwrap().text(), "\u{2028}");

    let mut editor = Editor::from_text("abc");
    editor
        .set_selection(Selection::new(Position::new(0, 2), Position::new(0, 1)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..StylePatch::default()
        })
        .unwrap();
    editor.insert_line_break().unwrap();
    assert_eq!(editor.document().plain_text(), "a\nc");
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 4)));
    let spans = editor.document().paragraph(0).unwrap().spans();
    assert_eq!(spans[1].range, 1..4);
    assert!(spans[1].style.bold);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "abc");

    editor.update_composition("に", None).unwrap();
    assert_eq!(editor.insert_line_break(), Err(Error::CompositionActive));
    assert_eq!(editor.document().plain_text(), "abc");
    editor.commit_composition("\u{2028}").unwrap();
    assert_eq!(editor.document().paragraph(0).unwrap().text(), "a\u{2028}c");
}

#[test]
fn movement_and_deletion_cross_soft_line_breaks_as_whitespace_graphemes() {
    let mut editor = Editor::from_text("ab\u{2028}cd");
    let focus = |editor: &mut Editor, movement, extend| {
        editor.move_cursor(movement, extend).unwrap();
        editor.selection().focus.byte
    };
    caret(&mut editor, 0, 5);
    assert_eq!(focus(&mut editor, Movement::GraphemeBackward, false), 2);
    assert_eq!(focus(&mut editor, Movement::GraphemeForward, true), 5);
    assert_eq!(editor.selected_text(), "\n");
    assert_eq!(focus(&mut editor, Movement::ParagraphEnd, false), 7);
    assert_eq!(focus(&mut editor, Movement::WordBackward, false), 5);
    assert_eq!(focus(&mut editor, Movement::WordBackward, false), 0);
    assert_eq!(focus(&mut editor, Movement::WordForward, false), 2);
    assert_eq!(focus(&mut editor, Movement::WordForward, false), 7);
    assert_eq!(focus(&mut editor, Movement::ParagraphStart, false), 0);
    assert_eq!(
        editor.document().word_range_at(Position::new(0, 2)),
        Ok(Position::new(0, 0)..Position::new(0, 2))
    );

    for (byte, forward) in [(5, false), (2, true)] {
        caret(&mut editor, 0, byte);
        if forward {
            editor.delete_forward().unwrap();
        } else {
            editor.delete_backward().unwrap();
        }
        assert_eq!(editor.document().plain_text(), "abcd");
        assert_eq!(editor.selection(), Selection::caret(Position::new(0, 2)));
        assert!(editor.undo());
    }
    caret(&mut editor, 0, 5);
    editor.delete_word_backward().unwrap();
    assert_eq!(editor.document().paragraph(0).unwrap().text(), "cd");
    assert!(editor.undo());
    caret(&mut editor, 0, 2);
    editor.delete_word_forward().unwrap();
    assert_eq!(editor.document().paragraph(0).unwrap().text(), "ab");
}

#[test]
fn unicode_separators_import_as_soft_and_paragraph_breaks() {
    let mut editor = Editor::default();
    editor.insert_text("a\u{2029}b\u{2028}c").unwrap();
    let texts: Vec<_> = editor
        .document()
        .paragraphs()
        .iter()
        .map(|paragraph| paragraph.text().to_owned())
        .collect();
    assert_eq!(texts, ["a", "b\u{2028}c"]);
    assert_eq!(editor.selection(), Selection::caret(Position::new(1, 5)));
    editor.insert_text("d").unwrap();
    assert_eq!(editor.undo_len(), 2);

    let mut editor = Editor::default();
    editor.insert_text("a").unwrap();
    editor.commit_composition("\u{2028}").unwrap();
    editor.commit_composition("b").unwrap();
    assert_eq!(editor.undo_len(), 1);
    editor.select_all();
    assert_eq!(editor.selected_text(), "a\nb");
    let fragment = editor.selected_fragment();
    assert_eq!(fragment.plain_text(), "a\nb");
    assert_eq!(fragment.paragraphs()[0].text(), "a\u{2028}b");
    assert_eq!(
        Document::from_text(&editor.document().plain_text())
            .paragraphs()
            .len(),
        2
    );
}

fn kinds(editor: &Editor) -> Vec<ParagraphKind> {
    editor
        .document()
        .paragraphs()
        .iter()
        .map(|paragraph| paragraph.kind())
        .collect()
}

const QUOTE: ParagraphKind = ParagraphKind::Quote;
const BODY: ParagraphKind = ParagraphKind::Body;

#[test]
fn quote_enter_continues_then_empty_enter_leaves_the_quote() {
    let mut editor = Editor::from_text("Quoted words\nafter");
    editor.set_paragraph_kind(QUOTE).unwrap();
    assert_eq!(kinds(&editor), [QUOTE, BODY]);
    caret(&mut editor, 0, 6);
    editor.insert_paragraph().unwrap();
    assert_eq!(editor.document().plain_text(), "Quoted\n words\nafter");
    assert_eq!(kinds(&editor), [QUOTE, QUOTE, BODY]);
    caret(&mut editor, 1, 6);
    editor.insert_paragraph().unwrap();
    assert_eq!(kinds(&editor), [QUOTE, QUOTE, QUOTE, BODY]);
    assert_eq!(editor.selection(), Selection::caret(Position::new(2, 0)));
    editor.insert_text("x").unwrap();
    editor.insert_line_break().unwrap();
    assert_eq!(kinds(&editor), [QUOTE, QUOTE, QUOTE, BODY]);
    editor.undo();
    assert_eq!(editor.document().paragraph(2).unwrap().text(), "");

    let steps = editor.undo_len();
    editor.insert_paragraph().unwrap();
    assert_eq!(editor.document().paragraphs().len(), 4);
    assert_eq!(kinds(&editor), [QUOTE, QUOTE, BODY, BODY]);
    assert_eq!(editor.selection(), Selection::caret(Position::new(2, 0)));
    assert_eq!(editor.undo_len(), steps + 1);
    editor.insert_paragraph().unwrap();
    assert_eq!(editor.document().paragraphs().len(), 5);
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(kinds(&editor), [QUOTE, QUOTE, QUOTE, BODY]);
    assert_eq!(editor.selection(), Selection::caret(Position::new(2, 0)));
    assert!(editor.redo());
    assert_eq!(kinds(&editor), [QUOTE, QUOTE, BODY, BODY]);

    let mut editor = Editor::from_text("ab");
    editor.set_paragraph_kind(QUOTE).unwrap();
    editor.select_all();
    editor.insert_paragraph().unwrap();
    assert_eq!(kinds(&editor), [QUOTE, QUOTE]);
}

#[test]
fn backspace_at_quote_start_leaves_the_quote_before_joining() {
    let mut editor = Editor::from_text("body\nquote");
    caret(&mut editor, 1, 0);
    editor.set_paragraph_kind(QUOTE).unwrap();
    let steps = editor.undo_len();
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "body\nquote");
    assert_eq!(kinds(&editor), [BODY, BODY]);
    assert_eq!(editor.selection(), Selection::caret(Position::new(1, 0)));
    assert_eq!(editor.undo_len(), steps + 1);
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "bodyquote");
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(kinds(&editor), [BODY, QUOTE]);
    assert_eq!(editor.selection(), Selection::caret(Position::new(1, 0)));

    let mut editor = Editor::from_text("first\nsecond");
    editor.set_paragraph_kind(QUOTE).unwrap();
    caret(&mut editor, 0, 5);
    editor.delete_backward().unwrap();
    assert_eq!(kinds(&editor), [QUOTE, BODY]);
    caret(&mut editor, 1, 0);
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "firssecond");
    assert_eq!(kinds(&editor), [QUOTE]);

    let mut editor = Editor::from_text("quote");
    editor.set_paragraph_kind(QUOTE).unwrap();
    editor.delete_backward().unwrap();
    assert_eq!(kinds(&editor), [BODY]);
    editor.set_paragraph_kind(QUOTE).unwrap();
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 2)))
        .unwrap();
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "ote");
    assert_eq!(kinds(&editor), [QUOTE]);
}

#[test]
fn quotes_follow_paragraph_commands_and_interrupt_list_numbering() {
    let mut editor = Editor::from_text("one\ntwo\nthree\nfour");
    editor.select_all();
    editor.set_paragraph_kind(ordered(None)).unwrap();
    caret(&mut editor, 1, 0);
    editor.set_paragraph_kind(QUOTE).unwrap();
    assert_eq!(numbers(&editor), [Some(1), None, Some(2), Some(3)]);
    assert_eq!(
        ordered_kinds(&editor),
        [ordered(None), QUOTE, ordered(None), ordered(None)]
    );
    assert_eq!(editor.selection_paragraph_kind(), Some(QUOTE));
    let revision = editor.document().revision();
    editor.indent_list().unwrap();
    editor.outdent_list().unwrap();
    assert_eq!(editor.document().revision(), revision);
    caret(&mut editor, 0, 0);
    editor.set_paragraph_kind(ordered(Some(5))).unwrap();
    assert_eq!(numbers(&editor), [Some(5), None, Some(6), Some(7)]);
    caret(&mut editor, 2, 0);
    editor.set_paragraph_kind(ordered(Some(1))).unwrap();
    assert_eq!(numbers(&editor), [Some(5), None, Some(1), Some(2)]);
    assert!(editor.undo());
    caret(&mut editor, 1, 0);
    editor.set_paragraph_kind(ordered(None)).unwrap();
    assert_eq!(numbers(&editor), [Some(5), Some(6), Some(7), Some(8)]);
    assert!(editor.undo());
    assert_eq!(kinds(&editor)[1], QUOTE);

    editor
        .set_selection(Selection::new(Position::new(1, 0), Position::new(2, 1)))
        .unwrap();
    assert_eq!(editor.selection_paragraph_kind(), None);
    editor.set_paragraph_kind(QUOTE).unwrap();
    assert_eq!(editor.selection_paragraph_kind(), Some(QUOTE));
    editor
        .set_paragraph_kind(ParagraphKind::Heading { level: 2 })
        .unwrap();
    assert_eq!(
        kinds(&editor)[1..3],
        [ParagraphKind::Heading { level: 2 }; 2]
    );
    assert!(editor.undo());
    assert_eq!(kinds(&editor)[1..3], [QUOTE; 2]);
}
