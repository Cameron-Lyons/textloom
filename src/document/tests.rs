use super::*;
use unicode_segmentation::UnicodeSegmentation;

#[test]
fn newline_normalization_handles_all_adjacent_breaks_and_keeps_lf_borrowed() {
    let tokens = ["a", "é", "\r", "\n"];
    for length in 0..=5 {
        for mut case in 0..4usize.pow(length) {
            let mut text = String::new();
            for _ in 0..length {
                text.push_str(tokens[case % tokens.len()]);
                case /= tokens.len();
            }
            let expected = text.replace("\r\n", "\n").replace('\r', "\n");
            let normalized = normalize_newlines(&text);
            assert_eq!(normalized, expected, "input {text:?}");
            assert_eq!(matches!(normalized, Cow::Borrowed(_)), !text.contains('\r'));
        }
    }
}

#[test]
fn indexed_unicode_offsets_match_extended_graphemes_and_scalars() {
    for text in [
        "",
        "plain ASCII",
        "café e\u{301}",
        "👨‍👩‍👧‍👦🇺🇸🇨🇦🇯🇵",
        "क्‍ष नमस्ते",
        "a\u{301}\u{302}👩🏽‍💻z",
    ] {
        let paragraph = Paragraph::plain(text);
        let graphemes: Vec<_> = text
            .grapheme_indices(true)
            .map(|(byte, _)| byte)
            .chain([text.len()])
            .collect();
        let scalars: Vec<_> = text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain([text.len()])
            .collect();
        assert_eq!(paragraph.grapheme_count(), graphemes.len() - 1);
        assert_eq!(paragraph.scalar_count(), scalars.len() - 1);
        for (index, byte) in graphemes.iter().enumerate() {
            assert_eq!(paragraph.byte_from_grapheme(index), Some(*byte));
            assert_eq!(paragraph.grapheme_index(*byte), Some(index));
        }
        for (index, byte) in scalars.iter().enumerate() {
            assert_eq!(paragraph.byte_from_scalar(index), Some(*byte));
            assert_eq!(paragraph.scalar_index(*byte), Some(index));
        }
        assert_eq!(paragraph.byte_from_grapheme(graphemes.len()), None);
        assert_eq!(paragraph.byte_from_scalar(scalars.len()), None);
        for byte in 0..=text.len() + 2 {
            assert_eq!(
                paragraph.grapheme_index(byte),
                graphemes.iter().position(|offset| *offset == byte)
            );
            assert_eq!(
                paragraph.scalar_index(byte),
                scalars.iter().position(|offset| *offset == byte)
            );
            assert_eq!(
                paragraph.previous_grapheme(byte),
                graphemes
                    .iter()
                    .copied()
                    .rev()
                    .find(|offset| *offset < byte)
                    .unwrap_or(0)
            );
            assert_eq!(
                paragraph.next_grapheme(byte),
                graphemes
                    .iter()
                    .copied()
                    .find(|offset| *offset > byte)
                    .unwrap_or(text.len())
            );
            assert_eq!(
                paragraph.boundary_at_or_before(byte),
                graphemes
                    .iter()
                    .copied()
                    .rev()
                    .find(|offset| *offset <= byte)
                    .unwrap_or(0)
            );
            assert_eq!(
                paragraph.boundary_at_or_after(byte),
                graphemes
                    .iter()
                    .copied()
                    .find(|offset| *offset >= byte)
                    .unwrap_or(text.len())
            );
        }
    }
}

#[test]
fn local_grapheme_checks_and_snapping_match_complete_segmentation() {
    let mut texts: Vec<String> = [
        "",
        "plain ASCII",
        "é",
        "café e\u{301}",
        "\u{600}e\u{301}z",
        "👨‍👩‍👧‍👦🇺🇸🇨🇦🇯🇵",
        "क्‍ष नमस्ते",
        "각나",
        "a\t\u{301}\u{0000}👩🏽‍💻z",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    texts.push(format!("a👩{}‍💻z", "\u{301}".repeat(64)));
    texts.push(format!("a{}z", "🇦".repeat(65)));
    texts.push(format!("aक{}षz", "\u{94d}\u{301}".repeat(33)));

    for text in texts {
        let boundaries: Vec<_> = text
            .grapheme_indices(true)
            .map(|(byte, _)| byte)
            .chain([text.len()])
            .collect();
        let check = |paragraph: &Paragraph, byte: usize| {
            assert_eq!(
                paragraph.is_grapheme_boundary(byte),
                boundaries.contains(&byte),
                "boundary at {byte} in {text:?}"
            );
            assert_eq!(
                paragraph.boundary_at_or_before(byte),
                boundaries
                    .iter()
                    .rev()
                    .copied()
                    .find(|offset| *offset <= byte)
                    .unwrap_or(0),
                "backward snap at {byte} in {text:?}"
            );
            assert_eq!(
                paragraph.boundary_at_or_after(byte),
                boundaries
                    .iter()
                    .copied()
                    .find(|offset| *offset >= byte)
                    .unwrap_or(text.len()),
                "forward snap at {byte} in {text:?}"
            );
            assert_eq!(
                paragraph.previous_grapheme(byte),
                boundaries
                    .iter()
                    .rev()
                    .copied()
                    .find(|offset| *offset < byte)
                    .unwrap_or(0),
                "previous grapheme at {byte} in {text:?}"
            );
            assert_eq!(
                paragraph.next_grapheme(byte),
                boundaries
                    .iter()
                    .copied()
                    .find(|offset| *offset > byte)
                    .unwrap_or(text.len()),
                "next grapheme at {byte} in {text:?}"
            );
        };
        for byte in (0..=text.len() + 2).chain([usize::MAX]) {
            check(&Paragraph::plain(&text), byte);
        }
        let paragraph = Paragraph::plain(&text);
        paragraph.grapheme_count();
        let cached = paragraph.cached_grapheme_boundaries().map(<[_]>::as_ptr);
        for byte in (0..=text.len() + 2).chain([usize::MAX]) {
            check(&paragraph, byte);
        }
        assert_eq!(
            paragraph.cached_grapheme_boundaries().map(<[_]>::as_ptr),
            cached
        );
    }
}

#[test]
fn nearby_grapheme_navigation_and_deletion_keep_indexes_lazy() {
    use crate::{Editor, Movement, Selection};

    let phrase = "café e\u{301} 👩🏽‍💻 🇺🇸 नमस्ते ";
    let text = phrase.repeat(256);
    let original = Selection::caret(Position::new(0, phrase.len() * 128));
    let mut editor = Editor::from_text(&text);
    editor.set_selection(original).unwrap();
    for _ in 0..3 {
        editor
            .move_cursor(Movement::GraphemeBackward, false)
            .unwrap();
        editor
            .move_cursor(Movement::GraphemeForward, false)
            .unwrap();
        assert_eq!(editor.selection(), original);
        editor.insert_text("é").unwrap();
        editor.delete_backward().unwrap();
        assert_eq!(editor.document().plain_text(), text);
        assert!(editor.undo());
        editor.delete_forward().unwrap();
        assert!(editor.undo());
        assert!(editor.undo());
        assert_eq!(editor.selection(), original);
        assert_eq!(editor.document().plain_text(), text);
        assert!(
            editor
                .document()
                .paragraph(0)
                .unwrap()
                .cached_grapheme_boundaries()
                .is_none()
        );
    }
}

#[test]
fn position_validation_and_style_lookup_leave_grapheme_indexes_lazy() {
    let text = "café e\u{301} 👩🏽‍💻 🇦🇧🇨 क्‍ष ".repeat(16);
    let mut document = Document::from_text(&text);
    document
        .apply_style(
            Position::default()..document.end(),
            StylePatch {
                bold: Some(true),
                ..StylePatch::default()
            },
        )
        .unwrap();
    let boundaries: Vec<_> = text
        .grapheme_indices(true)
        .map(|(byte, _)| byte)
        .chain([text.len()])
        .collect();
    for byte in (0..=text.len() + 1).chain([usize::MAX]) {
        let position = Position::new(0, byte);
        let valid = boundaries.contains(&byte);
        assert_eq!(document.validate_position(position).is_ok(), valid);
        assert_eq!(document.style_at(position).bold, valid);
    }
    assert!(
        document
            .paragraph(0)
            .unwrap()
            .cached_grapheme_boundaries()
            .is_none()
    );
}

#[test]
fn long_grapheme_contexts_initialize_and_reuse_one_index() {
    for (text, byte) in [
        (format!("a{}z", "🇦".repeat(65)), 1 + 4 * 33),
        (
            format!("a👩{}‍💻z", "\u{301}".repeat(64)),
            "a👩‍".len() + "\u{301}".len() * 64,
        ),
        (
            format!("aक{}षz", "\u{94d}\u{301}".repeat(33)),
            "aक".len() + "\u{94d}\u{301}".len() * 33,
        ),
    ] {
        let paragraph = Paragraph::plain(&text);
        let boundaries: Vec<_> = text
            .grapheme_indices(true)
            .map(|(byte, _)| byte)
            .chain([text.len()])
            .collect();
        assert_eq!(
            paragraph.is_grapheme_boundary(byte),
            boundaries.contains(&byte)
        );
        let cached = paragraph.cached_grapheme_boundaries().unwrap().as_ptr();
        for byte in 0..=text.len() {
            assert_eq!(
                paragraph.is_grapheme_boundary(byte),
                boundaries.contains(&byte)
            );
            assert_eq!(
                paragraph.cached_grapheme_boundaries().unwrap().as_ptr(),
                cached
            );
        }
    }

    let text = format!("a{}z", "\u{301}".repeat(64));
    let before = Paragraph::plain(&text);
    assert_eq!(before.boundary_at_or_before(text.len() - 3), 0);
    assert!(before.cached_grapheme_boundaries().is_some());
    let after = Paragraph::plain(&text);
    assert_eq!(after.boundary_at_or_after(1), text.len() - 1);
    assert!(after.cached_grapheme_boundaries().is_some());
}

#[test]
fn all_match_search_in_a_long_regional_run_reuses_grapheme_index() {
    let document = Document::from_text(&format!("a{}z", "🇦".repeat(257)));
    let expected: Vec<_> = (0..128)
        .map(|index| Position::new(0, 1 + index * 8)..Position::new(0, 1 + (index + 1) * 8))
        .collect();
    assert_eq!(
        document.find("🇦🇦", crate::SearchOptions::default()),
        expected
    );
    let paragraph = document.paragraph(0).unwrap();
    let cached = paragraph.cached_grapheme_boundaries().unwrap().as_ptr();
    assert_eq!(
        document.find("🇦🇦", crate::SearchOptions::default()),
        expected
    );
    assert_eq!(
        paragraph.cached_grapheme_boundaries().unwrap().as_ptr(),
        cached
    );
}

#[test]
fn unicode_editing_snaps_carets_and_indexes_only_long_contexts() {
    for (text, byte, insertion, expected_byte, indexed) in [
        (
            "café 👩💻 e\u{301}".to_owned(),
            "café 👩".len(),
            "‍",
            "café 👩‍💻".len(),
            false,
        ),
        ("a🇦🇧🇨🇩z".to_owned(), "a🇦🇧".len(), "🇪", "a🇦🇧🇪🇨".len(), false),
        (
            format!("a👩{}💻z", "\u{301}".repeat(64)),
            "a👩".len() + "\u{301}".len() * 64,
            "‍",
            "a👩‍💻".len() + "\u{301}".len() * 64,
            true,
        ),
    ] {
        let mut editor = crate::Editor::from_text(&text);
        editor
            .set_selection(crate::Selection::caret(Position::new(0, byte)))
            .unwrap();
        editor.insert_text(insertion).unwrap();
        let paragraph = editor.document().paragraph(0).unwrap();
        assert_eq!(editor.selection().focus, Position::new(0, expected_byte));
        assert_eq!(paragraph.cached_grapheme_boundaries().is_some(), indexed);
        assert!(
            editor
                .document()
                .validate_position(editor.selection().focus)
                .is_ok()
        );
        assert_eq!(paragraph.cached_grapheme_boundaries().is_some(), indexed);
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), text);
        assert!(editor.redo());
        assert_eq!(editor.selection().focus, Position::new(0, expected_byte));
    }
}

#[test]
fn word_navigation_matches_unicode_segmentation() {
    for text in [
        "",
        "one, two's!",
        "can't 123,456 12.34 under_score a:b; c.d 'word'",
        "\t  one\u{000b}two\u{000c}three \u{0000}four",
        "café 👩‍💻 नमस्ते 🇺🇸",
        "你好世界 e\u{301}",
        "!!!",
    ] {
        let paragraph = Paragraph::plain(text);
        for byte in 0..=text.len() + 1 {
            let previous = text
                .unicode_word_indices()
                .rev()
                .find(|(start, _)| *start < byte)
                .map_or(0, |(start, _)| start);
            let next = text
                .unicode_word_indices()
                .find(|(start, word)| start + word.len() > byte)
                .map_or(text.len(), |(start, word)| start + word.len());
            assert_eq!(
                paragraph.previous_word(byte),
                paragraph.boundary_at_or_before(previous)
            );
            assert_eq!(
                paragraph.next_word(byte),
                paragraph.boundary_at_or_after(next)
            );
        }
    }
}

#[test]
fn unicode_indexes_are_independently_lazy_and_shared_across_metadata_changes() {
    let mut document = Document::from_text("café 👩‍💻");
    let original = Arc::clone(&document.paragraphs()[0]);
    let indices = original.indices.get().unwrap();
    assert!(indices.graphemes.get().is_none());
    assert!(indices.scalars.get().is_none());
    assert!(indices.words.get().is_none());
    let equal = Paragraph::plain(original.text());
    assert_eq!(*original, equal);
    original.grapheme_count();
    assert_eq!(*original, equal);
    assert!(indices.graphemes.get().is_some());
    assert!(indices.scalars.get().is_none());
    assert!(indices.words.get().is_none());
    document
        .set_kind(0..1, ParagraphKind::Heading { level: 2 })
        .unwrap();
    assert!(Arc::ptr_eq(
        indices,
        document.paragraph(0).unwrap().indices.get().unwrap()
    ));
    document
        .apply_style(
            Position::default()..document.end(),
            StylePatch {
                bold: Some(true),
                ..StylePatch::default()
            },
        )
        .unwrap();
    assert!(Arc::ptr_eq(
        indices,
        document.paragraph(0).unwrap().indices.get().unwrap()
    ));
    document
        .replace(document.end()..document.end(), "!", InlineStyle::default())
        .unwrap();
    assert!(!Arc::ptr_eq(
        indices,
        document.paragraph(0).unwrap().indices.get().unwrap()
    ));
    assert!(Paragraph::plain("ASCII").indices.get().is_none());
}

#[test]
fn ascii_word_indexes_are_lazy_shared_and_invalidated_by_text_edits() {
    let mut document = Document::from_text("one two's 12.34");
    let original = Arc::clone(&document.paragraphs()[0]);
    assert_eq!(original.grapheme_count(), original.text().len());
    assert_eq!(original.scalar_count(), original.text().len());
    assert_eq!(original.next_grapheme(3), 4);
    assert_eq!(original.previous_grapheme(3), 2);
    assert!(original.indices.get().is_none());

    assert_eq!(original.next_word(3), 9);
    assert_eq!(original.previous_word(14), 10);
    let indices = original.indices.get().unwrap();
    let words = indices.words.get().unwrap().as_ptr();
    assert!(indices.graphemes.get().is_none());
    assert!(indices.scalars.get().is_none());

    document
        .set_kind(0..1, ParagraphKind::Heading { level: 2 })
        .unwrap();
    document
        .apply_style(
            Position::default()..document.end(),
            StylePatch {
                bold: Some(true),
                ..StylePatch::default()
            },
        )
        .unwrap();
    let formatted = document.paragraph(0).unwrap();
    assert!(Arc::ptr_eq(indices, formatted.indices.get().unwrap()));
    assert_eq!(formatted.next_word(3), 9);
    assert_eq!(formatted.word_ranges().as_ptr(), words);

    let (delta, _) = document
        .replace(
            document.end()..document.end(),
            " six",
            InlineStyle::default(),
        )
        .unwrap();
    let edited = document.paragraph(0).unwrap();
    assert!(edited.indices.get().is_none());
    assert_eq!(edited.next_word(15), 19);
    assert!(!Arc::ptr_eq(indices, edited.indices.get().unwrap()));
    document.replay(&delta, false);
    assert!(Arc::ptr_eq(
        indices,
        document.paragraph(0).unwrap().indices.get().unwrap()
    ));
}

#[test]
fn uniform_unicode_end_typing_does_not_build_text_indexes() {
    let mut document = Document::from_text("café 👩‍💻");
    for _ in 0..3 {
        document
            .replace(document.end()..document.end(), "é", InlineStyle::default())
            .unwrap();
        let paragraph = document.paragraph(0).unwrap();
        assert_eq!(paragraph.spans().len(), 1);
        let indices = paragraph.indices.get().unwrap();
        assert!(indices.graphemes.get().is_none());
        assert!(indices.scalars.get().is_none());
        assert!(indices.words.get().is_none());
    }
    assert_normalized(&document);
}

#[test]
fn rich_parts_validate_and_merge_runs() {
    let paragraph = Paragraph::from_parts(
        "aéébc",
        vec![
            Span {
                range: 0..1,
                style: bold(),
            },
            Span {
                range: 1..3,
                style: bold(),
            },
            Span {
                range: 3..5,
                style: bold(),
            },
            Span {
                range: 5..6,
                style: InlineStyle::default(),
            },
            Span {
                range: 6..7,
                style: InlineStyle::default(),
            },
        ],
        ParagraphKind::Heading { level: 1 },
    )
    .unwrap();
    assert_eq!(
        paragraph.spans(),
        &[
            Span {
                range: 0..5,
                style: bold()
            },
            Span {
                range: 5..7,
                style: InlineStyle::default()
            }
        ]
    );
    assert!(Paragraph::from_parts("", vec![], ParagraphKind::Body).is_ok());
    for (text, spans, kind) in [
        (
            "a\nb",
            vec![Span {
                range: 0..3,
                style: bold(),
            }],
            ParagraphKind::Body,
        ),
        (
            "a\rb",
            vec![Span {
                range: 0..3,
                style: bold(),
            }],
            ParagraphKind::Body,
        ),
        (
            "a",
            vec![Span {
                range: 0..1,
                style: bold(),
            }],
            ParagraphKind::Heading { level: 0 },
        ),
        ("a", vec![], ParagraphKind::Body),
        (
            "",
            vec![Span {
                range: 0..0,
                style: bold(),
            }],
            ParagraphKind::Body,
        ),
        (
            "ab",
            vec![Span {
                range: 0..1,
                style: bold(),
            }],
            ParagraphKind::Body,
        ),
        (
            "ab",
            vec![Span {
                range: 1..2,
                style: bold(),
            }],
            ParagraphKind::Body,
        ),
        (
            "ab",
            vec![Span {
                range: 0..3,
                style: bold(),
            }],
            ParagraphKind::Body,
        ),
        (
            "e\u{301}",
            vec![
                Span {
                    range: 0..1,
                    style: bold(),
                },
                Span {
                    range: 1..3,
                    style: bold(),
                },
            ],
            ParagraphKind::Body,
        ),
        (
            "é",
            vec![
                Span {
                    range: 0..1,
                    style: bold(),
                },
                Span {
                    range: 1..2,
                    style: bold(),
                },
            ],
            ParagraphKind::Body,
        ),
    ] {
        assert_eq!(
            Paragraph::from_parts(text, spans, kind),
            Err(Error::InvalidFragment)
        );
    }
}

#[test]
fn rich_unicode_run_validation_keeps_navigation_indexes_lazy() {
    let paragraph = Paragraph::from_parts(
        "ééé🦀",
        vec![
            Span {
                range: 0..2,
                style: bold(),
            },
            Span {
                range: 2..6,
                style: InlineStyle::default(),
            },
            Span {
                range: 6..10,
                style: bold(),
            },
        ],
        ParagraphKind::Body,
    )
    .unwrap();
    let indices = paragraph.indices.get().unwrap();
    assert!(indices.graphemes.get().is_none());
    assert!(indices.scalars.get().is_none());
    assert!(indices.words.get().is_none());
    assert_eq!(paragraph.spans().len(), 3);
    assert_eq!(paragraph.grapheme_count(), 4);
}

#[test]
fn document_content_identity_tracks_changes_and_preserves_noops() {
    let mut document = Document::from_text("text");
    let cloned = document.clone();
    assert!(Arc::ptr_eq(&document.identity, &cloned.identity));
    let (noop, _) = document
        .replace(
            Position::default()..document.end(),
            "text",
            InlineStyle::default(),
        )
        .unwrap();
    assert!(noop.is_empty());
    assert!(Arc::ptr_eq(&document.identity, &cloned.identity));
    let (delta, _) = document
        .replace(document.end()..document.end(), "!", InlineStyle::default())
        .unwrap();
    assert!(!Arc::ptr_eq(&document.identity, &cloned.identity));
    let changed = Arc::clone(&document.identity);
    document.replay(&delta, false);
    assert!(!Arc::ptr_eq(&document.identity, &changed));
    assert!(!Arc::ptr_eq(&document.identity, &cloned.identity));
    assert_eq!(document.paragraphs(), cloned.paragraphs());
    let undone = Arc::clone(&document.identity);
    document.replay(&delta, true);
    assert!(!Arc::ptr_eq(&document.identity, &undone));

    let before_kind = Arc::clone(&document.identity);
    let kind = ParagraphKind::Heading { level: 2 };
    assert!(document.set_kind(0..1, kind).unwrap().is_some());
    assert!(!Arc::ptr_eq(&document.identity, &before_kind));
    let before_style = Arc::clone(&document.identity);
    assert!(document.set_kind(0..1, kind).unwrap().is_none());
    assert!(Arc::ptr_eq(&document.identity, &before_style));
    let patch = StylePatch {
        bold: Some(true),
        ..StylePatch::default()
    };
    assert!(
        document
            .apply_style(Position::default()..document.end(), patch)
            .unwrap()
            .is_some()
    );
    assert!(!Arc::ptr_eq(&document.identity, &before_style));
    let styled = Arc::clone(&document.identity);
    assert!(
        document
            .apply_style(Position::default()..document.end(), patch)
            .unwrap()
            .is_none()
    );
    assert!(Arc::ptr_eq(&document.identity, &styled));
    document.replay(&noop, true);
    assert!(Arc::ptr_eq(&document.identity, &styled));
}

fn bold() -> InlineStyle {
    InlineStyle {
        bold: true,
        ..InlineStyle::default()
    }
}

fn assert_normalized(document: &Document) {
    assert!(!document.paragraphs.is_empty());
    for paragraph in document.paragraphs() {
        let mut previous_end = 0;
        let mut previous_style = None;
        for span in paragraph.spans() {
            assert_eq!(span.range.start, previous_end);
            assert!(span.range.start < span.range.end);
            assert!(paragraph.text().is_char_boundary(span.range.start));
            assert!(paragraph.text().is_char_boundary(span.range.end));
            assert_ne!(Some(span.style), previous_style);
            let boundaries: Vec<_> = paragraph
                .text()
                .grapheme_indices(true)
                .map(|(index, _)| index)
                .chain([paragraph.text().len()])
                .collect();
            assert!(boundaries.contains(&span.range.start));
            assert!(boundaries.contains(&span.range.end));
            previous_end = span.range.end;
            previous_style = Some(span.style);
        }
        assert_eq!(previous_end, paragraph.text().len());
    }
}

#[test]
fn plain_text_normalizes_newlines_and_preserves_empty_paragraphs() {
    let document = Document::from_text("a\r\nb\rc\n");
    assert_eq!(document.plain_text(), "a\nb\nc\n");
    assert_eq!(document.paragraphs().len(), 4);
    assert_eq!(document.end(), Position::new(3, 0));
    assert_eq!(
        document
            .text(Position::new(0, 1)..Position::new(2, 1))
            .unwrap(),
        "\nb\nc"
    );
    assert_normalized(&document);
    assert_eq!(Document::new().paragraphs().len(), 1);
}

#[test]
fn positions_require_extended_grapheme_boundaries() {
    let document = Document::from_text("e\u{301}👨‍👩‍👧‍👦🇺🇸");
    for (byte, _) in document.paragraph(0).unwrap().text().grapheme_indices(true) {
        assert!(document.validate_position(Position::new(0, byte)).is_ok());
    }
    assert!(document.validate_position(document.end()).is_ok());
    assert!(document.validate_position(Position::new(0, 1)).is_err());
    assert!(document.validate_position(Position::new(0, 2)).is_err());
    assert!(document.validate_position(Position::new(0, 7)).is_err());
    assert!(document.validate_position(Position::new(1, 0)).is_err());
}

#[test]
fn replacing_preserves_styles_on_surviving_edges() {
    let mut document = Document::from_text("before\nafter");
    document
        .apply_style(
            Position::new(0, 0)..Position::new(0, 6),
            StylePatch {
                bold: Some(true),
                ..StylePatch::default()
            },
        )
        .unwrap();
    document
        .apply_style(
            Position::new(1, 0)..Position::new(1, 5),
            StylePatch {
                italic: Some(true),
                ..StylePatch::default()
            },
        )
        .unwrap();
    let (delta, caret) = document
        .replace(
            Position::new(0, 3)..Position::new(1, 2),
            "X",
            InlineStyle::default(),
        )
        .unwrap();
    assert_eq!(document.plain_text(), "befXter");
    assert_eq!(caret, Position::new(0, 4));
    assert_eq!(document.paragraph(0).unwrap().spans().len(), 3);
    assert!(document.style_at(Position::new(0, 3)).bold);
    assert!(document.style_at(Position::new(0, 5)).italic);
    document.replay(&delta, false);
    assert_eq!(document.plain_text(), "before\nafter");
    document.replay(&delta, true);
    assert_eq!(document.plain_text(), "befXter");
    assert_normalized(&document);
}

#[test]
fn formatting_merges_runs_and_is_a_noop_when_unchanged() {
    let mut document = Document::from_text("abcdef");
    let patch = StylePatch {
        bold: Some(true),
        ..StylePatch::default()
    };
    document
        .apply_style(Position::new(0, 2)..Position::new(0, 4), patch)
        .unwrap();
    assert_eq!(document.paragraph(0).unwrap().spans().len(), 3);
    let revision = document.revision();
    assert!(
        document
            .apply_style(Position::new(0, 2)..Position::new(0, 4), patch)
            .unwrap()
            .is_none()
    );
    assert_eq!(document.revision(), revision);
    document
        .apply_style(Position::new(0, 0)..Position::new(0, 6), patch)
        .unwrap();
    assert_eq!(
        document.paragraph(0).unwrap().spans(),
        &[Span {
            range: 0..6,
            style: bold()
        }]
    );
    assert_eq!(document.style_at(Position::new(0, 0)), bold());
    assert_eq!(document.style_at(document.end()), bold());
    assert_normalized(&document);
}

#[test]
fn formatting_ranges_match_per_grapheme_styles_and_share_unchanged_data() {
    let red = crate::Color([255, 0, 0, 255]);
    let blue = crate::Color([0, 0, 255, 255]);
    let styles = [
        InlineStyle::default(),
        bold(),
        InlineStyle {
            italic: true,
            underline: true,
            strikethrough: true,
            code: true,
            foreground: Some(red),
            ..InlineStyle::default()
        },
        InlineStyle {
            foreground: Some(blue),
            ..bold()
        },
    ];
    let texts = ["ab e\u{301}👩‍💻cd 🇺🇸ef", "", "क्‍ष xyz"];
    let source = Document::from_paragraphs(
        texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let spans = text
                    .grapheme_indices(true)
                    .enumerate()
                    .map(|(grapheme, (byte, text))| Span {
                        range: byte..byte + text.len(),
                        style: styles[(grapheme / 2 + index) % styles.len()],
                    })
                    .collect();
                Arc::new(
                    Paragraph::from_parts(
                        text,
                        spans,
                        if index == 0 {
                            ParagraphKind::Heading { level: 2 }
                        } else {
                            ParagraphKind::Bullet { indent: 1 }
                        },
                    )
                    .unwrap(),
                )
            })
            .collect(),
    );
    let positions: Vec<_> = source
        .paragraphs()
        .iter()
        .enumerate()
        .flat_map(|(index, paragraph)| {
            paragraph.grapheme_count();
            paragraph.scalar_count();
            paragraph.previous_word(paragraph.text().len());
            paragraph
                .text()
                .grapheme_indices(true)
                .map(move |(byte, _)| Position::new(index, byte))
                .chain([Position::new(index, paragraph.text().len())])
        })
        .collect();
    let mut patches = vec![
        StylePatch::default(),
        StylePatch {
            foreground: Some(None),
            ..StylePatch::default()
        },
        StylePatch {
            foreground: Some(Some(blue)),
            ..StylePatch::default()
        },
    ];
    for value in [false, true] {
        patches.extend([
            StylePatch {
                bold: Some(value),
                ..StylePatch::default()
            },
            StylePatch {
                italic: Some(value),
                underline: Some(value),
                ..StylePatch::default()
            },
            StylePatch {
                strikethrough: Some(value),
                code: Some(value),
                ..StylePatch::default()
            },
            StylePatch {
                bold: Some(value),
                italic: Some(value),
                underline: Some(value),
                strikethrough: Some(value),
                code: Some(value),
                foreground: Some(value.then_some(red)),
            },
        ]);
    }
    for (first, start) in positions.iter().copied().enumerate() {
        for end in positions[first..].iter().copied() {
            for patch in patches.iter().copied() {
                let expected: Vec<_> = source
                    .paragraphs()
                    .iter()
                    .enumerate()
                    .map(|(index, paragraph)| {
                        let spans = paragraph
                            .text()
                            .grapheme_indices(true)
                            .map(|(byte, text)| {
                                let mut style = paragraph
                                    .spans()
                                    .iter()
                                    .find(|span| span.range.contains(&byte))
                                    .unwrap()
                                    .style;
                                if (start..end).contains(&Position::new(index, byte)) {
                                    patch.apply(&mut style);
                                }
                                Span {
                                    range: byte..byte + text.len(),
                                    style,
                                }
                            })
                            .collect();
                        Arc::new(
                            Paragraph::from_parts(paragraph.text(), spans, paragraph.kind())
                                .unwrap(),
                        )
                    })
                    .collect();
                let mut document = source.clone();
                let delta = document.apply_style(start..end, patch).unwrap();
                let changed = source.paragraphs() != expected;
                assert_eq!(delta.is_some(), changed, "{start:?}..{end:?}, {patch:?}");
                assert_eq!(document.paragraphs(), expected);
                assert_eq!(document.revision(), u64::from(changed));
                assert_eq!(Arc::ptr_eq(&document.identity, &source.identity), !changed);
                for (before, after) in source.paragraphs().iter().zip(document.paragraphs()) {
                    assert!(Arc::ptr_eq(&before.text, &after.text));
                    match (before.indices.get(), after.indices.get()) {
                        (Some(before), Some(after)) => assert!(Arc::ptr_eq(before, after)),
                        (None, None) => {}
                        _ => panic!("formatting changed the navigation cache"),
                    }
                    if before == after {
                        assert!(Arc::ptr_eq(before, after));
                    }
                }
                if let Some(delta) = delta {
                    document.replay(&delta, false);
                    assert_eq!(document.paragraphs(), source.paragraphs());
                    document.replay(&delta, true);
                    assert_eq!(document.paragraphs(), expected);
                }
            }
        }
    }
}

#[test]
fn inserted_combining_marks_cannot_split_formatting_runs() {
    let mut document = Document::from_text("ex");
    document
        .apply_style(
            Position::new(0, 0)..Position::new(0, 1),
            StylePatch {
                bold: Some(true),
                ..StylePatch::default()
            },
        )
        .unwrap();
    let (_, caret) = document
        .replace(
            Position::new(0, 1)..Position::new(0, 1),
            "\u{301}",
            InlineStyle {
                italic: true,
                ..InlineStyle::default()
            },
        )
        .unwrap();
    assert_eq!(document.plain_text(), "e\u{301}x");
    assert_eq!(caret, Position::new(0, 3));
    assert_eq!(document.paragraph(0).unwrap().spans()[0].range, 0..3);
    assert_eq!(document.paragraph(0).unwrap().spans()[0].style, bold());
    assert_normalized(&document);
}

#[test]
fn emoji_joiner_insertion_snaps_caret_past_new_cluster() {
    let mut document = Document::from_text("👩👧");
    let (_, caret) = document
        .replace(Position::new(0, 4)..Position::new(0, 4), "\u{200d}", bold())
        .unwrap();
    assert_eq!(document.plain_text(), "👩‍👧");
    assert_eq!(caret, document.end());
    assert_eq!(document.paragraph(0).unwrap().spans().len(), 1);
    assert_normalized(&document);
}

#[test]
fn joining_paragraphs_also_normalizes_clusters() {
    let mut document = Document::from_text("a\n\u{301}b");
    document
        .apply_style(
            Position::new(1, 0)..Position::new(1, 2),
            StylePatch {
                italic: Some(true),
                ..StylePatch::default()
            },
        )
        .unwrap();
    let (_, caret) = document
        .replace(
            Position::new(0, 1)..Position::new(1, 0),
            "",
            InlineStyle::default(),
        )
        .unwrap();
    assert_eq!(document.plain_text(), "a\u{301}b");
    assert_eq!(caret, Position::new(0, 3));
    assert_normalized(&document);
}

#[test]
fn history_retains_only_changed_paragraphs_and_shares_others() {
    let mut document = Document::from_text("zero\none\ntwo\nthree");
    let unaffected = Arc::clone(&document.paragraphs()[3]);
    let (delta, _) = document
        .replace(Position::new(1, 1)..Position::new(1, 2), "N", bold())
        .unwrap();
    assert_eq!(delta.start, 1);
    assert_eq!(delta.before.len(), 1);
    assert_eq!(delta.after.len(), 1);
    assert!(delta.retained_bytes() > 0);
    assert!(Arc::ptr_eq(&unaffected, &document.paragraphs()[3]));
    let revision = document.revision();
    document.replay(&delta, false);
    assert_eq!(document.revision(), revision + 1);
    assert!(Arc::ptr_eq(&unaffected, &document.paragraphs()[3]));
    assert_eq!(document.plain_text(), "zero\none\ntwo\nthree");
}

#[test]
fn heading_and_list_splits_preserve_paragraph_semantics() {
    let mut document = Document::from_text("heading");
    document
        .set_kind(0..1, ParagraphKind::Heading { level: 2 })
        .unwrap();
    document
        .replace(
            document.end()..document.end(),
            "\nbody",
            InlineStyle::default(),
        )
        .unwrap();
    assert_eq!(
        document.paragraph(0).unwrap().kind(),
        ParagraphKind::Heading { level: 2 }
    );
    assert_eq!(document.paragraph(1).unwrap().kind(), ParagraphKind::Body);
    document
        .set_kind(
            1..2,
            ParagraphKind::Ordered {
                indent: 1,
                start: 7,
            },
        )
        .unwrap();
    document
        .replace(
            document.end()..document.end(),
            "\nnext\nlast",
            InlineStyle::default(),
        )
        .unwrap();
    assert_eq!(
        document.paragraph(2).unwrap().kind(),
        ParagraphKind::Ordered {
            indent: 1,
            start: 8
        }
    );
    assert_eq!(
        document.paragraph(3).unwrap().kind(),
        ParagraphKind::Ordered {
            indent: 1,
            start: 9
        }
    );
    assert_normalized(&document);
}

#[test]
fn deletion_of_a_complete_leading_paragraph_preserves_suffix_kind() {
    let mut document = Document::from_text("first\nheading");
    document
        .set_kind(1..2, ParagraphKind::Heading { level: 3 })
        .unwrap();
    document
        .replace(
            Position::new(0, 0)..Position::new(1, 0),
            "",
            InlineStyle::default(),
        )
        .unwrap();
    assert_eq!(document.plain_text(), "heading");
    assert_eq!(
        document.paragraph(0).unwrap().kind(),
        ParagraphKind::Heading { level: 3 }
    );
}

#[test]
fn noops_do_not_change_revision_or_allocations() {
    let mut document = Document::from_text("unchanged");
    let original = Arc::clone(&document.paragraphs()[0]);
    let (delta, caret) = document
        .replace(
            Position::default()..document.end(),
            "unchanged",
            InlineStyle::default(),
        )
        .unwrap();
    assert!(delta.is_empty());
    assert_eq!(caret, document.end());
    assert_eq!(document.revision(), 0);
    assert!(Arc::ptr_eq(&original, &document.paragraphs()[0]));
    assert!(
        document
            .set_kind(0..1, ParagraphKind::Body)
            .unwrap()
            .is_none()
    );
    assert!(
        document
            .apply_style(Position::default()..document.end(), StylePatch::default())
            .unwrap()
            .is_none()
    );
    document.replay(&delta, false);
    assert_eq!(document.revision(), 0);
}

#[test]
fn invalid_operations_leave_document_unchanged() {
    let mut document = Document::from_text("é");
    let original = document.clone();
    assert!(
        document
            .replace(Position::new(0, 1)..document.end(), "x", bold())
            .is_err()
    );
    assert!(matches!(
        document.set_kind(0..1, ParagraphKind::Heading { level: 0 }),
        Err(Error::InvalidHeadingLevel(0))
    ));
    assert!(matches!(
        document.set_kind(0..2, ParagraphKind::Body),
        Err(Error::InvalidRange)
    ));
    assert_eq!(
        document.text(document.end()..Position::default()),
        Err(Error::InvalidRange)
    );
    assert!(matches!(
        document.apply_style(Position::new(0, 1)..document.end(), StylePatch::default()),
        Err(Error::InvalidPosition(_))
    ));
    assert!(matches!(
        document.apply_style(document.end()..Position::default(), StylePatch::default()),
        Err(Error::InvalidRange)
    ));
    assert_eq!(document, original);
}

#[test]
fn unicode_replacements_preserve_text_invariants_and_replay_exactly() {
    let inputs = [
        "",
        "abc",
        "e\u{301}x",
        "👩👧",
        "🇺🇸🇨🇦",
        "a\n\u{301}b",
        "क्\nष",
        "\u{600}a",
    ];
    let insertions = ["", "X", "\u{301}", "\u{200d}", "\n", "\r\n", "\n\u{301}"];
    for input in inputs {
        let mut original = Document::from_text(input);
        let mut positions = Vec::new();
        for (index, paragraph) in original.paragraphs().iter().enumerate() {
            positions.extend(
                paragraph
                    .text()
                    .grapheme_indices(true)
                    .map(|(byte, _)| Position::new(index, byte)),
            );
            positions.push(Position::new(index, paragraph.text().len()));
        }
        // Give surviving edges different styles, so replay verifies both
        // content and formatting when replacement changes segmentation.
        for (index, position) in positions.iter().enumerate() {
            if index % 2 == 0
                && let Some(end) = positions.get(index + 1)
            {
                original
                    .apply_style(
                        *position..*end,
                        StylePatch {
                            bold: Some(true),
                            ..StylePatch::default()
                        },
                    )
                    .unwrap();
            }
        }
        let plain = original.plain_text();
        for (start_index, start) in positions.iter().enumerate() {
            for end in &positions[start_index..] {
                let global_offset = |position: Position| {
                    original.paragraphs()[..position.paragraph]
                        .iter()
                        .map(|paragraph| paragraph.text().len() + 1)
                        .sum::<usize>()
                        + position.byte
                };
                for insertion in insertions {
                    let mut expected = plain.clone();
                    expected.replace_range(
                        global_offset(*start)..global_offset(*end),
                        &normalize_newlines(insertion),
                    );
                    let mut edited = original.clone();
                    let (delta, caret) = edited
                        .replace(
                            *start..*end,
                            insertion,
                            InlineStyle {
                                italic: true,
                                ..InlineStyle::default()
                            },
                        )
                        .unwrap();
                    assert_eq!(edited.plain_text(), expected);
                    assert!(edited.validate_position(caret).is_ok());
                    assert_normalized(&edited);
                    let result = edited.paragraphs().to_vec();
                    edited.replay(&delta, false);
                    assert_eq!(edited.paragraphs(), original.paragraphs());
                    edited.replay(&delta, true);
                    assert_eq!(edited.paragraphs(), result);
                }
            }
        }
    }
}
