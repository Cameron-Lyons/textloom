//! Tests editing operations on paragraph structure: batch replacement against
//! sequential edits and string oracles, find navigation, word deletion, list
//! indentation and numbering, heading splits, and how typed and pasted text
//! chooses paragraph kinds, including undo and allocation sharing.

use std::iter;
use std::sync::Arc;

use textloom::{
    Document, Editor, Error, Fragment, HistoryLimits, InlineStyle, Movement, Paragraph,
    ParagraphKind, Position, SearchOptions, SearchWrap, Selection, SelectionMode, Span, StylePatch,
};

const H1: ParagraphKind = ParagraphKind::Heading { level: 1 };
const BODY: ParagraphKind = ParagraphKind::Body;

/// Returns an ordered item kind with an explicit start, which `shown` also
/// uses to report each item's displayed number.
fn ordered(indent: u8, number: u32) -> ParagraphKind {
    ParagraphKind::Ordered {
        indent,
        start: Some(number),
    }
}

/// Returns each paragraph's kind, with ordered items' `start` replaced by the
/// number the item displays.
fn shown(document: &Document) -> Vec<ParagraphKind> {
    document
        .paragraphs()
        .iter()
        .enumerate()
        .map(|(index, paragraph)| match paragraph.kind() {
            ParagraphKind::Ordered { indent, .. } => ParagraphKind::Ordered {
                indent,
                start: document.list_number(index),
            },
            kind => kind,
        })
        .collect()
}

fn kinds(editor: &Editor) -> Vec<ParagraphKind> {
    shown(editor.document())
}

fn stored_kinds(editor: &Editor) -> Vec<ParagraphKind> {
    editor
        .document()
        .paragraphs()
        .iter()
        .map(|paragraph| paragraph.kind())
        .collect()
}

/// A paragraph's text and its kind as `shown` reports it.
#[derive(Debug, PartialEq)]
struct Line {
    text: String,
    kind: ParagraphKind,
}

fn lines(editor: &Editor) -> Vec<Line> {
    editor
        .document()
        .paragraphs()
        .iter()
        .zip(kinds(editor))
        .map(|(paragraph, kind)| Line {
            text: paragraph.text().to_owned(),
            kind,
        })
        .collect()
}

fn line(text: &str, kind: ParagraphKind) -> Line {
    Line {
        text: text.to_owned(),
        kind,
    }
}

/// The observable paragraph structure after a replacement: what each paragraph
/// shows and the kinds stored for it.
#[derive(Debug, PartialEq)]
struct Outcome {
    lines: Vec<Line>,
    stored_kinds: Vec<ParagraphKind>,
}

fn outcome(editor: &Editor) -> Outcome {
    Outcome {
        lines: lines(editor),
        stored_kinds: stored_kinds(editor),
    }
}

/// Returns an editor for `text` whose paragraphs show `paragraph_kinds`,
/// storing an ordered item's start only when continuing its list would show a
/// different number.
fn with_kinds(text: &str, paragraph_kinds: &[ParagraphKind]) -> Editor {
    let mut editor = Editor::from_text(text);
    for (index, paragraph_kind) in paragraph_kinds.iter().enumerate() {
        if let ParagraphKind::Ordered { indent, start } = *paragraph_kind {
            kind(
                &mut editor,
                index,
                ParagraphKind::Ordered {
                    indent,
                    start: None,
                },
            );
            if editor.document().list_number(index) == start {
                continue;
            }
        }
        kind(&mut editor, index, *paragraph_kind);
    }
    assert_eq!(
        kinds(&editor),
        paragraph_kinds,
        "the setup should produce the requested kinds and numbers"
    );
    editor.clear_history();
    editor
}

fn caret(editor: &mut Editor, paragraph: usize, byte: usize) {
    editor
        .set_selection(Selection::caret(Position::new(paragraph, byte)))
        .expect("callers place the caret on a grapheme boundary of an existing paragraph");
}

/// Deletes the break between paragraph `paragraph` and the one before it.
fn join_with_previous(editor: &mut Editor, paragraph: usize) {
    let end = editor
        .document()
        .paragraph(paragraph - 1)
        .expect("callers pass a paragraph that has a previous one")
        .text()
        .len();
    editor
        .replace_range(
            Position::new(paragraph - 1, end)..Position::new(paragraph, 0),
            "",
        )
        .expect("the range spans exactly one paragraph break");
}

fn kind(editor: &mut Editor, paragraph: usize, kind: ParagraphKind) {
    caret(editor, paragraph, 0);
    editor
        .set_paragraph_kind(kind)
        .expect("no IME composition is active, and callers pass valid heading levels");
}

fn style(editor: &mut Editor, paragraph: usize, start: usize, end: usize, patch: StylePatch) {
    editor
        .set_selection(Selection::new(
            Position::new(paragraph, start),
            Position::new(paragraph, end),
        ))
        .expect("callers select between grapheme boundaries of an existing paragraph");
    editor
        .apply_style(patch)
        .expect("no IME composition is active");
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
    assert!(Document::from_bytes(&editor.document().to_bytes()).is_ok());
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
        assert!(Arc::ptr_eq(source, restored), "{source:?}");
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
        assert!(Arc::ptr_eq(source, restored), "{source:?}");
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
            1,
            "{suffix:?} -> {replacement:?}"
        );
        assert_eq!(
            editor.document().plain_text(),
            "a\u{301}b",
            "{suffix:?} -> {replacement:?}"
        );
        assert_eq!(
            editor.selection(),
            Selection::caret(Position::new(0, 3)),
            "{suffix:?} -> {replacement:?}"
        );
        let paragraph = editor.document().paragraph(0).unwrap();
        assert_eq!(
            paragraph.kind(),
            ParagraphKind::Heading { level: 2 },
            "{suffix:?} -> {replacement:?}"
        );
        assert_eq!(paragraph.spans().len(), 2, "{suffix:?} -> {replacement:?}");
        assert_eq!(
            paragraph.spans()[0].range,
            0..3,
            "{suffix:?} -> {replacement:?}"
        );
        assert!(
            paragraph.spans()[0].style.italic,
            "{suffix:?} -> {replacement:?}"
        );
        assert!(
            !paragraph.spans()[0].style.bold,
            "{suffix:?} -> {replacement:?}"
        );
        assert_eq!(
            paragraph.spans()[1].range,
            3..4,
            "{suffix:?} -> {replacement:?}"
        );
        assert!(
            paragraph.spans()[1].style.bold,
            "{suffix:?} -> {replacement:?}"
        );
        assert!(
            !paragraph.spans()[1].style.italic,
            "{suffix:?} -> {replacement:?}"
        );
        assert_eq!(editor.undo_len(), 1, "{suffix:?} -> {replacement:?}");
        assert!(editor.undo(), "{suffix:?} -> {replacement:?}");
        assert_eq!(
            Fragment::from_document(editor.document()),
            before,
            "{suffix:?} -> {replacement:?}"
        );
        assert_eq!(
            editor.selection(),
            selection,
            "{suffix:?} -> {replacement:?}"
        );
        for (source, restored) in paragraphs.iter().zip(editor.document().paragraphs()) {
            assert!(
                Arc::ptr_eq(source, restored),
                "{suffix:?} -> {replacement:?}"
            );
        }
        assert!(editor.redo(), "{suffix:?} -> {replacement:?}");
        assert_eq!(
            editor.document().plain_text(),
            "a\u{301}b",
            "{suffix:?} -> {replacement:?}"
        );
        assert_eq!(
            editor.selection(),
            Selection::caret(Position::new(0, 3)),
            "{suffix:?} -> {replacement:?}"
        );
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
        assert!(Arc::ptr_eq(source, unchanged), "{source:?}");
    }
}

#[test]
fn structural_replace_all_continues_ordered_numbering_and_undo_restores_metadata() {
    let mut editor = Editor::from_text("one\nkeep\none\ntail");
    editor.select_all();
    editor.set_paragraph_kind(ordered(1, 7)).unwrap();
    let before = Fragment::from_document(editor.document());
    editor.clear_history();
    assert_eq!(
        editor
            .replace_all("one", "x\ny", SearchOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(editor.document().plain_text(), "x\ny\nkeep\nx\ny\ntail");
    let expected: Vec<_> = (7..13).map(|number| ordered(1, number)).collect();
    assert_eq!(kinds(&editor), expected);
    assert_eq!(
        stored_kinds(&editor)[1..],
        [ParagraphKind::Ordered {
            indent: 1,
            start: None
        }; 5]
    );
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert!(editor.redo());
    assert_eq!(kinds(&editor), expected);
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
        let mut sequential = Editor::new(Document::from_fragment(&source));
        sequential.replace_range(range, replacement).unwrap();
        let mut batch = Editor::new(Document::from_fragment(&source));
        assert_eq!(
            batch
                .replace_all(query, replacement, SearchOptions::default())
                .unwrap(),
            1,
            "{text:?}"
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
    editor.set_paragraph_kind(ordered(1, 4)).unwrap();
    let before = Fragment::from_document(editor.document());
    let range = editor
        .document()
        .find("a\n", SearchOptions::default())
        .remove(0);
    let mut single = Editor::new(Document::from_fragment(&before));
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
    assert_eq!(kinds(&editor), [ordered(1, 4), ordered(1, 5)]);
    assert_eq!(
        stored_kinds(&editor),
        [
            ordered(1, 4),
            ParagraphKind::Ordered {
                indent: 1,
                start: None
            }
        ]
    );
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), before);
}

/// Replaces every match of `query` in `source` one `replace_range` call at a
/// time, as the oracle for `replace_all`.
fn replace_sequentially(source: &Fragment, query: &str, replacement: &str) -> Outcome {
    let mut editor = Editor::new(Document::from_fragment(source));
    let matches = editor.document().find(query, SearchOptions::default());
    let normalized = replacement.replace("\r\n", "\n").replace('\r', "\n");
    let breaks = normalized.matches('\n').count();
    let last_chunk = normalized
        .rsplit('\n')
        .next()
        .expect("`rsplit` yields at least one piece")
        .len();
    let mut shift: Option<(Position, Position)> = None;
    for range in matches {
        let map = |position: Position| match shift {
            Some((end, suffix)) if position.paragraph == end.paragraph => {
                Position::new(suffix.paragraph, suffix.byte + position.byte - end.byte)
            }
            Some((end, suffix)) => Position::new(
                suffix.paragraph + position.paragraph - end.paragraph,
                position.byte,
            ),
            None => position,
        };
        let (start, end) = (map(range.start), map(range.end));
        editor
            .replace_range(start..end, replacement)
            .expect("earlier replacements shift later matches to valid positions");
        let suffix = if breaks == 0 {
            Position::new(start.paragraph, start.byte + last_chunk)
        } else {
            Position::new(start.paragraph + breaks, last_chunk)
        };
        shift = Some((range.end, suffix));
    }
    outcome(&editor)
}

#[test]
fn deterministic_unicode_batch_replacements_match_plain_string_oracle_and_restore_metadata() {
    let tokens = ["a", "b", "β", "🦀", "👩\u{200d}💻", "e\u{301}", "\n"];
    let queries = [
        "a",
        "b",
        "β",
        "🦀",
        "👩\u{200d}💻",
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
        "👨\u{200d}💻",
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
            let paragraph_kind = match (case % 3, (case + index) % 4) {
                (2, _) if index % 4 == 2 => ParagraphKind::Bullet { indent: 1 },
                (2, _) => ParagraphKind::Ordered {
                    indent: u8::from(index % 4 == 3),
                    start: (index == 4).then_some(1),
                },
                (_, 0) if index % 2 == 1 => ParagraphKind::Quote,
                (_, 0) => ParagraphKind::Body,
                (_, 1) => ParagraphKind::Heading {
                    level: (index % 6 + 1) as u8,
                },
                (_, 2) => ParagraphKind::Bullet {
                    indent: (index % 3) as u8,
                },
                _ => ParagraphKind::Ordered {
                    indent: 1,
                    start: (index % 2 == 0).then_some(index as u32 + 3),
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
                let mut editor = Editor::new(Document::from_fragment(&before));
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
                assert_eq!(
                    outcome(&editor),
                    replace_sequentially(&before, query, replacement),
                    "sequential kinds for {text:?}, query {query:?}, replacement {replacement:?}"
                );
                let after = Fragment::from_document(editor.document());
                if editor.can_undo() {
                    assert_eq!(
                        editor.undo_len(),
                        1,
                        "text {text:?}, query {query:?}, replacement {replacement:?}"
                    );
                    assert!(
                        editor.undo(),
                        "text {text:?}, query {query:?}, replacement {replacement:?}"
                    );
                    assert_eq!(
                        Fragment::from_document(editor.document()),
                        before,
                        "text {text:?}, query {query:?}, replacement {replacement:?}"
                    );
                    assert_eq!(
                        editor.selection(),
                        selection,
                        "text {text:?}, query {query:?}, replacement {replacement:?}"
                    );
                    assert!(
                        editor.redo(),
                        "text {text:?}, query {query:?}, replacement {replacement:?}"
                    );
                    assert_eq!(
                        Fragment::from_document(editor.document()),
                        after,
                        "text {text:?}, query {query:?}, replacement {replacement:?}"
                    );
                } else {
                    assert_eq!(
                        after, before,
                        "text {text:?}, query {query:?}, replacement {replacement:?}"
                    );
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
            0,
            "{query:?}"
        );
        assert_eq!(editor.selection(), selection, "{query:?}");
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
    assert!(editor.find_next("a", options, SearchWrap::Stop).unwrap());
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 0), Position::new(0, 1))
    );
    assert!(editor.find_next("a", options, SearchWrap::Stop).unwrap());
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 4), Position::new(0, 5))
    );
    assert!(editor.find_next("a", options, SearchWrap::Stop).unwrap());
    let last = editor.selection();
    assert_eq!(
        last,
        Selection::new(Position::new(1, 0), Position::new(1, 1))
    );
    assert!(!editor.find_next("a", options, SearchWrap::Stop).unwrap());
    assert_eq!(editor.selection(), last);
    assert!(editor.find_next("a", options, SearchWrap::Around).unwrap());
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 0), Position::new(0, 1))
    );
    assert!(
        !editor
            .find_previous("a", options, SearchWrap::Stop)
            .unwrap()
    );
    assert!(
        editor
            .find_previous("a", options, SearchWrap::Around)
            .unwrap()
    );
    assert_eq!(editor.selection(), last);
    assert!(
        editor
            .find_previous("a", options, SearchWrap::Stop)
            .unwrap()
    );
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 4), Position::new(0, 5))
    );
    let selected = editor.selection();
    for query in ["missing", ""] {
        assert!(
            !editor
                .find_next(query, options, SearchWrap::Around)
                .unwrap(),
            "{query:?}"
        );
        assert!(
            !editor
                .find_previous(query, options, SearchWrap::Around)
                .unwrap(),
            "{query:?}"
        );
        assert_eq!(editor.selection(), selected, "{query:?}");
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
            .find_next("one\ntwo", SearchOptions::default(), SearchWrap::Stop)
            .unwrap()
    );
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(2, 0), Position::new(3, 3))
    );
    assert!(
        editor
            .find_previous("one\ntwo", SearchOptions::default(), SearchWrap::Stop)
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
    let mut editor = Editor::from_text("cafe\u{301}  👩\u{200d}💻 word");
    editor.delete_word_forward().unwrap();
    assert_eq!(editor.document().plain_text(), "  👩\u{200d}💻 word");
    assert!(
        editor
            .document()
            .validate_position(editor.selection().focus)
            .is_ok()
    );
    editor.delete_word_forward().unwrap();
    assert_eq!(editor.document().plain_text(), " word");
    editor.delete_word_forward().unwrap();
    assert_eq!(editor.document().plain_text(), "");
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "  👩\u{200d}💻 word");
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
fn word_deletion_and_movement_stop_at_symbol_and_emoji_runs() {
    for (text, expected) in [
        ("hello 🦀🦀🦀", "hello "),
        ("foo ----------", "foo "),
        ("call(x);", "call(x"),
        ("done. ", "done"),
    ] {
        let mut editor = Editor::from_text(text);
        let end = editor.document().end();
        editor.set_selection(Selection::caret(end)).unwrap();
        editor.delete_word_backward().unwrap();
        assert_eq!(editor.document().plain_text(), expected, "{text:?}");
    }
    let mut editor = Editor::from_text("a, 👩\u{200d}💻 b");
    let mut stops = Vec::new();
    loop {
        let before = editor.selection().focus;
        editor
            .move_cursor(Movement::WordForward, SelectionMode::Move)
            .unwrap();
        if editor.selection().focus == before {
            break;
        }
        stops.push(editor.selection().focus.byte);
    }
    assert_eq!(stops, [1, 2, 14, 16]);
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
    kind(&mut editor, 3, ordered(2, 9));
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
        ordered(3, 9)
    );
    for index in [0, 1, 4, 5] {
        assert!(
            Arc::ptr_eq(
                &before.paragraphs()[index],
                &editor.document().paragraphs()[index]
            ),
            "paragraph {index}"
        );
    }
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert_eq!(editor.selection(), selection);
    assert!(editor.redo());
    assert_eq!(
        editor.document().paragraph(3).unwrap().kind(),
        ordered(3, 9)
    );
}

#[test]
fn outdent_zero_level_items_becomes_body_and_other_items_decrease_one_level() {
    let mut editor = Editor::from_text("a\nb\nc");
    kind(&mut editor, 0, ParagraphKind::Bullet { indent: 0 });
    kind(&mut editor, 1, ordered(0, 17));
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
        editor.find_next("word", options, SearchWrap::Around),
        Err(Error::CompositionActive)
    );
    assert_eq!(
        editor.find_previous("word", options, SearchWrap::Around),
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

#[test]
fn splitting_a_heading_keeps_its_text_a_heading_and_ending_it_continues_as_body() {
    let mut editor = with_kinds("Title", &[H1]);
    let original = Arc::clone(&editor.document().paragraphs()[0]);
    caret(&mut editor, 0, 0);
    editor.insert_paragraph().unwrap();
    assert_eq!(lines(&editor), [line("", H1), line("Title", H1)]);
    assert_eq!(editor.selection(), Selection::caret(Position::new(1, 0)));
    assert!(Arc::ptr_eq(&original, &editor.document().paragraphs()[1]));
    assert!(editor.undo());
    assert_eq!(lines(&editor), [line("Title", H1)]);
    assert!(editor.redo());
    assert_eq!(lines(&editor), [line("", H1), line("Title", H1)]);

    let mut middle = with_kinds("Title", &[H1]);
    caret(&mut middle, 0, 2);
    middle.insert_paragraph().unwrap();
    assert_eq!(lines(&middle), [line("Ti", H1), line("tle", H1)]);

    let mut end = with_kinds("Title", &[H1]);
    let heading = Arc::clone(&end.document().paragraphs()[0]);
    caret(&mut end, 0, 5);
    end.insert_paragraph().unwrap();
    assert_eq!(lines(&end), [line("Title", H1), line("", BODY)]);
    assert!(Arc::ptr_eq(&heading, &end.document().paragraphs()[0]));
    end.insert_paragraph().unwrap();
    assert_eq!(
        lines(&end),
        [line("Title", H1), line("", BODY), line("", BODY)]
    );

    let mut pasted = with_kinds("Title", &[H1]);
    caret(&mut pasted, 0, 2);
    pasted.insert_text("x\ny\nz").unwrap();
    assert_eq!(
        lines(&pasted),
        [line("Tix", H1), line("y", BODY), line("ztle", H1)]
    );

    let mut empty = with_kinds("", &[H1]);
    empty.insert_paragraph().unwrap();
    assert_eq!(lines(&empty), [line("", H1), line("", BODY)]);
}

#[test]
fn replace_all_splits_headings_like_sequential_replacement() {
    for (text, query, replacement) in [
        ("axbxc", "x", "1\n2"),
        ("xTitle", "x", "\n"),
        ("Titlex", "x", "\n"),
        ("Title", "Title", "\nTitle"),
        ("axb\ncxd", "x", "1\n2\n3"),
    ] {
        let paragraph_kinds = vec![H1; text.split('\n').count()];
        let mut batch = with_kinds(text, &paragraph_kinds);
        let mut sequential = with_kinds(text, &paragraph_kinds);
        let matches = batch.document().find(query, SearchOptions::default());
        batch
            .replace_all(query, replacement, SearchOptions::default())
            .unwrap();
        for range in matches.into_iter().rev() {
            sequential.replace_range(range, replacement).unwrap();
        }
        assert_eq!(lines(&batch), lines(&sequential), "{text:?}");
    }
    let mut editor = with_kinds("axb", &[H1]);
    editor
        .replace_all("x", "1\n2", SearchOptions::default())
        .unwrap();
    assert_eq!(lines(&editor), [line("a1", H1), line("2b", H1)]);
}

#[test]
fn typing_over_a_selection_from_a_paragraph_start_matches_deleting_it_first() {
    for end in [
        Position::new(1, 0),
        Position::new(1, 2),
        Position::new(1, 4),
    ] {
        let mut typed = with_kinds("Title\nbody", &[H1, BODY]);
        typed
            .set_selection(Selection::new(Position::default(), end))
            .unwrap();
        typed.insert_text("X").unwrap();
        let mut deleted = with_kinds("Title\nbody", &[H1, BODY]);
        deleted
            .set_selection(Selection::new(Position::default(), end))
            .unwrap();
        deleted.delete_backward().unwrap();
        deleted.insert_text("X").unwrap();
        assert_eq!(lines(&typed), lines(&deleted), "{end:?}");
        assert_eq!(
            typed.document().paragraph(0).unwrap().kind(),
            BODY,
            "{end:?}"
        );
        let mut replaced = with_kinds("Title\nbody", &[H1, BODY]);
        let query = replaced.document().text(Position::default()..end).unwrap();
        replaced
            .replace_all(&query, "X", SearchOptions::default())
            .unwrap();
        assert_eq!(lines(&replaced), lines(&typed), "{end:?}");
    }
    let mut whole = with_kinds("Title\nbody", &[H1, BODY]);
    whole
        .set_selection(Selection::new(Position::default(), Position::new(1, 0)))
        .unwrap();
    whole.insert_text("X").unwrap();
    assert_eq!(lines(&whole), [line("Xbody", BODY)]);
}

#[test]
fn pasting_lines_over_a_selection_from_a_paragraph_start_matches_deleting_it_first() {
    let bullet = ParagraphKind::Bullet { indent: 0 };
    for (text, paragraph_kinds, end, inserted, expected) in [
        (
            "Title\nbody",
            vec![H1, BODY],
            Position::new(1, 2),
            "x\ny",
            vec![line("x", BODY), line("ydy", BODY)],
        ),
        (
            "Title\nbody",
            vec![H1, BODY],
            Position::new(1, 0),
            "a\nb\n",
            vec![line("a", BODY), line("b", BODY), line("body", BODY)],
        ),
        (
            "Title\nbody text",
            vec![H1, BODY],
            Position::new(1, 4),
            "\n",
            vec![line("", BODY), line(" text", BODY)],
        ),
        (
            "item\nTitle",
            vec![bullet, H1],
            Position::new(1, 5),
            "x\ny",
            vec![line("x", H1), line("y", BODY)],
        ),
        (
            "a\nb\nc",
            vec![ordered(0, 1), ordered(0, 5), ordered(0, 6)],
            Position::new(1, 0),
            "x\ny",
            vec![
                line("x", ordered(0, 5)),
                line("yb", ordered(0, 6)),
                line("c", ordered(0, 7)),
            ],
        ),
    ] {
        let selection = Selection::new(Position::default(), end);
        let mut pasted = with_kinds(text, &paragraph_kinds);
        pasted.set_selection(selection).unwrap();
        pasted.insert_text(inserted).unwrap();
        assert_eq!(lines(&pasted), expected, "{text:?} {end:?} {inserted:?}");

        let mut deleted = with_kinds(text, &paragraph_kinds);
        deleted.set_selection(selection).unwrap();
        deleted.delete_backward().unwrap();
        deleted.insert_text(inserted).unwrap();
        assert_eq!(
            deleted.document().paragraphs(),
            pasted.document().paragraphs(),
            "{text:?} {end:?} {inserted:?}"
        );

        let mut replaced = with_kinds(text, &paragraph_kinds);
        let query = replaced.document().text(selection.range()).unwrap();
        replaced
            .replace_all(&query, inserted, SearchOptions::default())
            .unwrap();
        assert_eq!(
            replaced.document().paragraphs(),
            pasted.document().paragraphs(),
            "{text:?} {end:?} {inserted:?}"
        );
    }

    let mut entered = with_kinds("Title\nbody text", &[H1, BODY]);
    entered
        .set_selection(Selection::new(Position::default(), Position::new(1, 4)))
        .unwrap();
    entered.insert_paragraph().unwrap();
    assert_eq!(lines(&entered), [line("", BODY), line(" text", BODY)]);
}

#[test]
fn seeded_documents_replace_selections_like_deleting_them_first() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let paragraph_kinds = [
        BODY,
        H1,
        ParagraphKind::Quote,
        ParagraphKind::Bullet { indent: 0 },
        ParagraphKind::Bullet { indent: 1 },
        ParagraphKind::Ordered {
            indent: 0,
            start: None,
        },
        ParagraphKind::Ordered {
            indent: 0,
            start: Some(4),
        },
        ParagraphKind::Ordered {
            indent: 1,
            start: None,
        },
        ParagraphKind::Ordered {
            indent: 1,
            start: Some(2),
        },
    ];
    let styles = [
        InlineStyle::default(),
        InlineStyle {
            bold: true,
            ..InlineStyle::default()
        },
    ];
    for case in 0..300 {
        let document = Document::from_paragraphs((0..1 + next(4)).map(|_| {
            let mut text = String::new();
            let mut spans = Vec::new();
            for _ in 0..next(3) {
                let start = text.len();
                text.push_str(["a", "bc", "e\u{301}"][next(3)]);
                spans.push(Span {
                    range: start..text.len(),
                    style: styles[next(styles.len())],
                    link: None,
                });
            }
            Paragraph::try_new(&text, spans, paragraph_kinds[next(paragraph_kinds.len())]).unwrap()
        }));
        let positions: Vec<_> = document
            .paragraphs()
            .iter()
            .enumerate()
            .flat_map(|(index, paragraph)| {
                (0..=paragraph.grapheme_count()).map(move |grapheme| {
                    Position::new(index, paragraph.byte_from_grapheme(grapheme).unwrap())
                })
            })
            .collect();
        for (index, &start) in positions.iter().enumerate() {
            for &end in &positions[index..] {
                for text in ["", "x", "x\ny", "\n", "a\n\nb", "x\n"] {
                    let selection = Selection::new(start, end);
                    let mut replaced = Editor::new(document.clone());
                    replaced.set_selection(selection).unwrap();
                    replaced.insert_text(text).unwrap();
                    let mut deleted = Editor::new(document.clone());
                    deleted.set_selection(selection).unwrap();
                    if !selection.is_caret() {
                        deleted.delete_backward().unwrap();
                    }
                    deleted.insert_text(text).unwrap();
                    // The rule covers selections from a paragraph start. After a later
                    // start, text following a selection across a break keeps its own
                    // paragraph's kind on the last inserted line, as with rich paste.
                    let tail = (start.byte > 0 && start.paragraph < end.paragraph)
                        .then(|| start.paragraph + text.matches('\n').count())
                        .filter(|&tail| tail > start.paragraph);
                    let paragraphs = |editor: &Editor| -> Vec<_> {
                        editor
                            .document()
                            .paragraphs()
                            .iter()
                            .enumerate()
                            .map(|(index, paragraph)| {
                                (
                                    paragraph.text().to_owned(),
                                    paragraph.spans().to_vec(),
                                    (Some(index) != tail).then(|| paragraph.kind()),
                                )
                            })
                            .collect()
                    };
                    assert_eq!(
                        (paragraphs(&replaced), replaced.selection()),
                        (paragraphs(&deleted), deleted.selection()),
                        "case {case}: {document:?} {start:?}..{end:?} {text:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn rich_paste_without_a_copied_paragraph_break_keeps_the_destination_kind() {
    let mut source = Editor::from_text("some word here");
    source
        .set_selection(Selection::new(Position::new(0, 5), Position::new(0, 9)))
        .unwrap();
    let word = source.selected_fragment();
    let mut heading = with_kinds("Title", &[H1]);
    heading.insert_fragment(&word).unwrap();
    assert_eq!(lines(&heading), [line("wordTitle", H1)]);

    let mut titled = with_kinds("Title", &[H1]);
    titled
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 2)))
        .unwrap();
    let from_heading = titled.selected_fragment();
    let mut bullet = with_kinds("body text", &[ParagraphKind::Bullet { indent: 0 }]);
    bullet.insert_fragment(&from_heading).unwrap();
    assert_eq!(
        lines(&bullet),
        [line("Tibody text", ParagraphKind::Bullet { indent: 0 })]
    );

    let mut numbered = with_kinds("it", &[ordered(0, 7)]);
    numbered.select_all();
    let item_text = numbered.selected_fragment();
    caret(&mut numbered, 0, 1);
    let caret_fragment = numbered.selected_fragment();
    let mut body = with_kinds("body", &[BODY]);
    body.insert_fragment(&item_text).unwrap();
    assert_eq!(lines(&body), [line("itbody", BODY)]);
    assert!(
        body.document()
            .to_html()
            .starts_with("<div style=\"white-space:pre-wrap\"><p>")
    );

    for fragment in [Fragment::default(), caret_fragment] {
        let mut editor = with_kinds("Title", &[H1]);
        let revision = editor.document().revision();
        editor.insert_fragment(&fragment).unwrap();
        assert_eq!(lines(&editor), [line("Title", H1)], "{fragment:?}");
        assert_eq!(editor.undo_len(), 0, "{fragment:?}");
        assert_eq!(editor.document().revision(), revision, "{fragment:?}");
    }

    let mut tail = with_kinds("Title", &[H1]);
    tail.insert_fragment(&Fragment::from_text("foo\nbar"))
        .unwrap();
    assert_eq!(lines(&tail), [line("foo", BODY), line("barTitle", H1)]);

    heading.select_all();
    let mut adopted = Editor::default();
    adopted
        .insert_fragment(&heading.selected_fragment())
        .unwrap();
    assert_eq!(lines(&adopted), [line("wordTitle", H1)]);
}

#[test]
fn rich_and_plain_paste_choose_the_same_paragraph_kinds() {
    let cases: [(&str, &[ParagraphKind], Position, Position, &str); 4] = [
        (
            "abc\nTitle",
            &[BODY, H1],
            Position::new(0, 1),
            Position::new(1, 0),
            "x\ny",
        ),
        (
            "Title\nabc",
            &[H1, BODY],
            Position::new(0, 0),
            Position::new(1, 1),
            "x",
        ),
        (
            "Title",
            &[H1],
            Position::new(0, 2),
            Position::new(0, 2),
            "x\ny",
        ),
        (
            "a\nb\nc\nd",
            &[ordered(0, 1), ordered(0, 2), ordered(0, 3), ordered(0, 4)],
            Position::new(0, 1),
            Position::new(2, 0),
            "x",
        ),
    ];
    for (text, paragraph_kinds, start, end, inserted) in cases {
        let mut rich = with_kinds(text, paragraph_kinds);
        rich.set_selection(Selection::new(start, end)).unwrap();
        rich.insert_fragment(&Fragment::from_text(inserted))
            .unwrap();
        let mut plain = with_kinds(text, paragraph_kinds);
        plain.set_selection(Selection::new(start, end)).unwrap();
        plain.insert_text(inserted).unwrap();
        assert_eq!(lines(&rich), lines(&plain), "{text:?} {start:?}..{end:?}");
    }
}

#[test]
fn rich_paste_into_a_list_keeps_source_numbering_and_renumbers_the_rest() {
    let mut source = with_kinds("x\ny\nz", &[ordered(0, 5), ordered(0, 6), ordered(0, 7)]);
    source.select_all();
    let items = source.selected_fragment();
    let mut editor = with_kinds("a\nb\nc", &[ordered(0, 1), ordered(0, 2), ordered(0, 3)]);
    let following = Arc::clone(&editor.document().paragraphs()[2]);
    caret(&mut editor, 1, 0);
    editor.insert_fragment(&items).unwrap();
    assert_eq!(
        lines(&editor),
        [
            line("a", ordered(0, 1)),
            line("x", ordered(0, 5)),
            line("y", ordered(0, 6)),
            line("zb", ordered(0, 7)),
            line("c", ordered(0, 8)),
        ]
    );
    assert!(Arc::ptr_eq(&following, &editor.document().paragraphs()[4]));
    assert!(
        editor
            .document()
            .to_html()
            .contains("<li>a</li><li value=\"5\">x</li><li>y</li>")
    );
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(
        kinds(&editor),
        [ordered(0, 1), ordered(0, 2), ordered(0, 3)]
    );

    let mut lines_source = with_kinds("p\nq\nr", &[ordered(0, 1), ordered(0, 2), BODY]);
    lines_source
        .set_selection(Selection::new(Position::new(0, 0), Position::new(2, 0)))
        .unwrap();
    let copied = lines_source.selected_fragment();
    assert_eq!(
        copied.paragraphs()[0].kind(),
        ParagraphKind::Ordered {
            indent: 0,
            start: Some(1)
        }
    );
    let mut destination = with_kinds("a\nb\nc", &[ordered(0, 1), ordered(0, 2), ordered(0, 3)]);
    caret(&mut destination, 1, 0);
    destination.insert_fragment(&copied).unwrap();
    assert_eq!(
        lines(&destination),
        [
            line("a", ordered(0, 1)),
            line("p", ordered(0, 1)),
            line("q", ordered(0, 2)),
            line("b", ordered(0, 3)),
            line("c", ordered(0, 4)),
        ]
    );
    assert!(
        destination
            .document()
            .to_html()
            .contains("<li value=\"1\">p")
    );
    lines_source
        .set_selection(Selection::new(Position::new(1, 0), Position::new(2, 0)))
        .unwrap();
    let continuation = lines_source.selected_fragment();
    let mut destination = with_kinds("a\nb\nc", &[ordered(0, 1), ordered(0, 2), ordered(0, 3)]);
    caret(&mut destination, 1, 0);
    destination.insert_fragment(&continuation).unwrap();
    assert_eq!(
        lines(&destination),
        [
            line("a", ordered(0, 1)),
            line("q", ordered(0, 2)),
            line("b", ordered(0, 3)),
            line("c", ordered(0, 4)),
        ]
    );
    assert!(!destination.document().to_html().contains("value="));
}

#[test]
fn indenting_and_outdenting_renumber_the_runs_items_leave_and_join() {
    let mut editor = with_kinds(
        "A\nB\nC\nD",
        &[ordered(0, 1), ordered(0, 2), ordered(0, 3), ordered(0, 4)],
    );
    caret(&mut editor, 1, 0);
    editor.indent_list().unwrap();
    assert_eq!(
        kinds(&editor),
        [ordered(0, 1), ordered(1, 1), ordered(0, 2), ordered(0, 3)]
    );
    assert_eq!(editor.undo_len(), 1);
    assert_eq!(
        editor.document().to_html(),
        "<div style=\"white-space:pre-wrap\"><ol start=\"1\"><li>A<ol start=\"1\"><li>B</li></ol></li><li>C</li><li>D</li></ol></div>"
    );
    caret(&mut editor, 2, 0);
    editor.indent_list().unwrap();
    assert_eq!(
        kinds(&editor),
        [ordered(0, 1), ordered(1, 1), ordered(1, 2), ordered(0, 2)]
    );
    editor.outdent_list().unwrap();
    caret(&mut editor, 1, 0);
    editor.outdent_list().unwrap();
    assert_eq!(
        kinds(&editor),
        [ordered(0, 1), ordered(0, 2), ordered(0, 3), ordered(0, 4)]
    );
    assert!(editor.undo());
    assert!(editor.undo());
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(
        kinds(&editor),
        [ordered(0, 1), ordered(0, 2), ordered(0, 3), ordered(0, 4)]
    );

    let mut selection = with_kinds(
        "A\nB\nC\nD",
        &[ordered(0, 1), ordered(0, 2), ordered(0, 3), ordered(0, 4)],
    );
    selection
        .set_selection(Selection::new(Position::new(1, 0), Position::new(2, 1)))
        .unwrap();
    selection.indent_list().unwrap();
    assert_eq!(
        kinds(&selection),
        [ordered(0, 1), ordered(1, 1), ordered(1, 2), ordered(0, 2)]
    );
    assert_eq!(selection.undo_len(), 1);

    let mut nested = with_kinds("A\nB\nC", &[ordered(0, 1), ordered(1, 1), ordered(0, 2)]);
    caret(&mut nested, 1, 0);
    nested.outdent_list().unwrap();
    assert_eq!(
        kinds(&nested),
        [ordered(0, 1), ordered(0, 2), ordered(0, 3)]
    );

    let mut parent = with_kinds(
        "a\nx\ny\nb",
        &[ordered(0, 1), ordered(1, 1), ordered(1, 2), ordered(0, 2)],
    );
    caret(&mut parent, 1, 0);
    parent.outdent_list().unwrap();
    assert_eq!(
        kinds(&parent),
        [ordered(0, 1), ordered(0, 2), ordered(1, 1), ordered(0, 3)]
    );

    let mut first = with_kinds("a\nb\nc", &[ordered(0, 1), ordered(0, 2), ordered(0, 3)]);
    caret(&mut first, 0, 0);
    first.outdent_list().unwrap();
    assert_eq!(kinds(&first), [BODY, ordered(0, 1), ordered(0, 2)]);
}

#[test]
fn ordered_runs_continue_past_nested_items() {
    let mut editor = with_kinds("a\nx\nb", &[ordered(0, 1), ordered(1, 1), ordered(0, 2)]);
    caret(&mut editor, 0, 1);
    editor.insert_paragraph().unwrap();
    assert_eq!(
        kinds(&editor),
        [ordered(0, 1), ordered(0, 2), ordered(1, 1), ordered(0, 3)]
    );
    assert!(!editor.document().to_html().contains("value="));
    let mut ended = with_kinds(
        "a\nx\nb\nbody\nc",
        &[
            ordered(1, 1),
            ParagraphKind::Bullet { indent: 1 },
            ordered(1, 5),
            BODY,
            ordered(1, 9),
        ],
    );
    caret(&mut ended, 0, 1);
    ended.insert_paragraph().unwrap();
    assert_eq!(
        kinds(&ended),
        [
            ordered(1, 1),
            ordered(1, 2),
            ParagraphKind::Bullet { indent: 1 },
            ordered(1, 5),
            BODY,
            ordered(1, 9)
        ]
    );

    let mut restart = with_kinds("A\nsub\nB", &[ordered(0, 1), ordered(1, 1), ordered(0, 2)]);
    caret(&mut restart, 0, 0);
    restart.set_paragraph_kind(ordered(0, 5)).unwrap();
    assert_eq!(
        kinds(&restart),
        [ordered(0, 5), ordered(1, 1), ordered(0, 6)]
    );
    restart
        .set_selection(Selection::new(Position::new(0, 0), Position::new(1, 0)))
        .unwrap();
    restart.delete_backward().unwrap();
    assert_eq!(kinds(&restart), [ordered(1, 1), ordered(0, 1)]);
}

#[test]
fn explicit_restarts_survive_structural_edits_earlier_in_the_list() {
    let mut editor = with_kinds(
        "a\nb\nc\nd",
        &[ordered(0, 1), ordered(0, 2), ordered(0, 1), ordered(0, 2)],
    );
    caret(&mut editor, 0, 1);
    editor.insert_paragraph().unwrap();
    assert_eq!(
        kinds(&editor),
        [
            ordered(0, 1),
            ordered(0, 2),
            ordered(0, 3),
            ordered(0, 1),
            ordered(0, 2)
        ]
    );
    join_with_previous(&mut editor, 2);
    join_with_previous(&mut editor, 1);
    assert_eq!(editor.document().plain_text(), "ab\nc\nd");
    assert_eq!(
        kinds(&editor),
        [ordered(0, 1), ordered(0, 1), ordered(0, 2)]
    );
}

#[test]
fn removing_an_item_from_a_list_closes_its_numbering_gap() {
    let mut editor = with_kinds(
        "one\n\nthree",
        &[ordered(0, 1), ordered(0, 2), ordered(0, 3)],
    );
    caret(&mut editor, 1, 0);
    editor.insert_paragraph().unwrap();
    assert_eq!(kinds(&editor), [ordered(0, 1), BODY, ordered(0, 2)]);
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(
        kinds(&editor),
        [ordered(0, 1), ordered(0, 2), ordered(0, 3)]
    );

    let mut converted = with_kinds(
        "a\nb\nc\nd",
        &[ordered(0, 1), ordered(0, 2), ordered(0, 3), ordered(0, 4)],
    );
    converted
        .set_selection(Selection::new(Position::new(1, 0), Position::new(2, 1)))
        .unwrap();
    converted.set_paragraph_kind(H1).unwrap();
    assert_eq!(kinds(&converted), [ordered(0, 1), H1, H1, ordered(0, 1)]);

    let mut first = with_kinds("a\nb", &[ordered(0, 4), ordered(0, 5)]);
    caret(&mut first, 0, 0);
    first
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 1 })
        .unwrap();
    assert_eq!(
        kinds(&first),
        [ParagraphKind::Bullet { indent: 1 }, ordered(0, 1)]
    );

    let mut joined = with_kinds("x\na\nb", &[BODY, ordered(0, 1), ordered(0, 2)]);
    join_with_previous(&mut joined, 1);
    assert_eq!(lines(&joined), [line("xa", BODY), line("b", ordered(0, 1))]);

    let mut restart = with_kinds(
        "a\nb\nc\nd",
        &[ordered(0, 1), ordered(0, 2), ordered(0, 10), ordered(0, 11)],
    );
    restart
        .set_selection(Selection::new(Position::new(2, 0), Position::new(3, 0)))
        .unwrap();
    restart.delete_backward().unwrap();
    assert_eq!(
        kinds(&restart),
        [ordered(0, 1), ordered(0, 2), ordered(0, 10)]
    );
    let mut kept = with_kinds(
        "a\nb\nc\nd",
        &[ordered(0, 1), ordered(0, 2), ordered(0, 10), ordered(0, 11)],
    );
    kept.set_selection(Selection::new(Position::new(0, 0), Position::new(2, 0)))
        .unwrap();
    kept.delete_backward().unwrap();
    assert_eq!(kinds(&kept), [ordered(0, 10), ordered(0, 11)]);

    let mut pasted = with_kinds("aa\nbb\ncc", &[ordered(0, 1), ordered(0, 2), ordered(0, 3)]);
    pasted
        .set_selection(Selection::new(Position::new(0, 0), Position::new(1, 1)))
        .unwrap();
    pasted
        .insert_fragment(&Fragment::from_text("p\nq"))
        .unwrap();
    assert_eq!(
        lines(&pasted),
        [
            line("p", BODY),
            line("qb", ordered(0, 1)),
            line("cc", ordered(0, 2))
        ]
    );
}

#[test]
fn replace_all_numbers_lists_like_sequential_replacement() {
    let mut batch = with_kinds("a\na\nb", &[ordered(0, 1), ordered(0, 2), ordered(0, 3)]);
    assert_eq!(
        batch
            .replace_all("a\n", "", SearchOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(lines(&batch), [line("b", ordered(0, 1))]);

    let paragraph_kinds = [BODY, ordered(0, 5), ordered(0, 9), BODY];
    let mut batch = with_kinds("x\na\nb\nx", &paragraph_kinds);
    let mut sequential = with_kinds("x\na\nb\nx", &paragraph_kinds);
    batch
        .replace_all("x", "y\nz", SearchOptions::default())
        .unwrap();
    sequential
        .replace_range(Position::new(3, 0)..Position::new(3, 1), "y\nz")
        .unwrap();
    sequential
        .replace_range(Position::new(0, 0)..Position::new(0, 1), "y\nz")
        .unwrap();
    assert_eq!(lines(&batch), lines(&sequential));
    assert_eq!(batch.document().paragraph(3).unwrap().kind(), ordered(0, 9));
}

#[test]
fn formatting_large_paragraphs_remains_undoable_within_default_history_limits() {
    let bold = StylePatch {
        bold: Some(true),
        ..StylePatch::default()
    };
    let mut large = Editor::from_text(&"word ".repeat(1_000_000));
    assert_eq!(large.history_limits(), HistoryLimits::default());
    large.insert_text("x").unwrap();
    style(&mut large, 0, 1, 5, bold);
    large.set_paragraph_kind(H1).unwrap();
    assert_eq!(large.undo_len(), 3);
    assert!(large.undo());
    assert!(large.undo());
    assert!(large.undo());
    assert_eq!(large.document().plain_text(), "word ".repeat(1_000_000));

    let line = "Lorem ipsum dolor sit amet, consectetur adipiscing elit.\n";
    let text = line.repeat(2 * 1024 * 1024 / line.len());
    let mut many_paragraphs = Editor::from_text(&text);
    many_paragraphs.insert_text("x").unwrap();
    many_paragraphs.select_all();
    many_paragraphs.apply_style(bold).unwrap();
    assert_eq!(many_paragraphs.undo_len(), 2);
    assert!(many_paragraphs.history_bytes() <= HistoryLimits::default().max_bytes);
    let bytes = many_paragraphs.history_bytes();
    assert!(many_paragraphs.undo());
    assert_eq!(many_paragraphs.history_bytes(), bytes);
    assert!(many_paragraphs.undo());
    assert_eq!(many_paragraphs.document().plain_text(), text);
}

#[test]
fn deterministic_list_edits_are_reversible_and_keep_documents_valid() {
    let mut seed = 0x5eed_1157_0dd5_ba11u64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    for case in 0..40 {
        let mut editor = Editor::from_text("a\nb\nc\nd\ne\nf\ng");
        editor.select_all();
        editor.set_paragraph_kind(ordered(0, 1)).unwrap();
        for step in 0..30 {
            let paragraphs = editor.document().paragraphs().len();
            let paragraph = next(paragraphs);
            let len = editor.document().paragraph(paragraph).unwrap().text().len();
            caret(&mut editor, paragraph, next(len + 1));
            if next(3) == 0 {
                let end = next(paragraphs - paragraph) + paragraph;
                let byte = next(editor.document().paragraph(end).unwrap().text().len() + 1);
                let anchor = editor.selection().focus;
                editor
                    .set_selection(Selection::new(anchor.max(Position::new(end, byte)), anchor))
                    .unwrap();
            }
            let fragment = editor.selected_fragment();
            let before = Fragment::from_document(editor.document());
            let undo_len = editor.undo_len();
            match next(9) {
                0 => editor.insert_paragraph().unwrap(),
                1 => editor.delete_backward().unwrap(),
                2 => editor.delete_forward().unwrap(),
                3 => editor.indent_list().unwrap(),
                4 => editor.outdent_list().unwrap(),
                5 => editor.insert_text("x\ny").unwrap(),
                6 => editor
                    .set_paragraph_kind(match next(4) {
                        0 if paragraph % 2 == 0 => BODY,
                        0 => ParagraphKind::Quote,
                        1 => ParagraphKind::Bullet {
                            indent: next(3) as u8,
                        },
                        _ => ParagraphKind::Ordered {
                            indent: next(3) as u8,
                            start: [None, Some(1), Some(2), Some(3)][next(4)],
                        },
                    })
                    .unwrap(),
                7 => editor.insert_fragment(&fragment).unwrap(),
                _ => {
                    editor
                        .replace_all("\n", "", SearchOptions::default())
                        .unwrap();
                }
            }
            let after = Fragment::from_document(editor.document());
            let restored = Document::from_bytes(&editor.document().to_bytes()).unwrap();
            // Exporting any edited document must not panic.
            let _ = editor.document().to_html();
            let numbered = kinds(&editor);
            assert_eq!(numbered, shown(&restored), "case {case}, step {step}");
            if editor.undo_len() > undo_len {
                assert!(editor.undo(), "case {case}, step {step}");
                assert_eq!(
                    Fragment::from_document(editor.document()),
                    before,
                    "case {case}, step {step}"
                );
                assert_eq!(
                    kinds(&editor),
                    shown(&Document::from_fragment(&before)),
                    "case {case}, step {step}"
                );
                assert!(editor.redo(), "case {case}, step {step}");
                assert_eq!(
                    Fragment::from_document(editor.document()),
                    after,
                    "case {case}, step {step}"
                );
                assert_eq!(kinds(&editor), numbered, "case {case}, step {step}");
            } else {
                assert_eq!(after, before, "case {case}, step {step}");
            }
        }
    }
}

#[test]
fn rich_paste_keeps_the_ordinal_of_destination_text_it_moves_down() {
    let mut editor = with_kinds("a\nb\nc", &[ordered(0, 1), ordered(0, 2), ordered(0, 7)]);
    caret(&mut editor, 2, 0);
    editor
        .insert_fragment(&Fragment::from_text("x\ny"))
        .unwrap();
    assert_eq!(
        lines(&editor),
        [
            line("a", ordered(0, 1)),
            line("b", ordered(0, 2)),
            line("x", BODY),
            line("yc", ordered(0, 7))
        ]
    );

    let mut editor = with_kinds(
        "Intro\nSteps\none\ntwo",
        &[BODY, H1, ordered(0, 1), ordered(0, 2)],
    );
    let original = Fragment::from_document(editor.document());
    editor
        .set_selection(Selection::new(Position::new(1, 0), Position::new(2, 0)))
        .unwrap();
    let cut = editor.selected_fragment();
    editor.delete_backward().unwrap();
    editor.insert_fragment(&cut).unwrap();
    assert_eq!(Fragment::from_document(editor.document()), original);

    let mut source = with_kinds("x\ny\nz", &[ordered(0, 1), ordered(0, 2), ordered(0, 3)]);
    source.select_all();
    let items = source.selected_fragment();
    let mut editor = with_kinds("Title\nnext", &[ordered(0, 3), ordered(0, 4)]);
    caret(&mut editor, 0, 2);
    editor.insert_fragment(&items).unwrap();
    assert_eq!(
        lines(&editor),
        [
            line("Tix", ordered(0, 3)),
            line("y", ordered(0, 4)),
            line("ztle", ordered(0, 5)),
            line("next", ordered(0, 6))
        ]
    );
    assert_eq!(
        stored_kinds(&editor)[2],
        ParagraphKind::Ordered {
            indent: 0,
            start: None
        }
    );

    for (pasted, heading, (moved_number, next_number)) in [
        ("x\np\nz", false, (4, 5)),
        ("x\nHeading\nz", true, (1, 2)),
        ("x\nHeading\np\nq\nz", true, (1, 2)),
    ] {
        let mut source = with_kinds(pasted, &vec![BODY; pasted.split('\n').count()]);
        if heading {
            kind(&mut source, 1, H1);
        }
        source.select_all();
        let fragment = source.selected_fragment();
        let mut editor = with_kinds("Title\nnext", &[ordered(0, 3), ordered(0, 4)]);
        caret(&mut editor, 0, 2);
        editor.insert_fragment(&fragment).unwrap();
        let count = editor.document().paragraphs().len();
        assert_eq!(
            lines(&editor)[count - 2..],
            [
                line("ztle", ordered(0, moved_number)),
                line("next", ordered(0, next_number))
            ],
            "{pasted:?}"
        );
        assert_eq!(
            stored_kinds(&editor)[count - 2],
            ParagraphKind::Ordered {
                indent: 0,
                start: None
            },
            "{pasted:?}"
        );
    }
}

#[test]
fn removed_items_pass_their_ordinals_only_within_their_own_run() {
    let text = "a\nbody\nc\nd";
    let paragraph_kinds = [ordered(0, 6), BODY, ordered(0, 1), ordered(0, 2)];
    let mut one_step = with_kinds(text, &paragraph_kinds);
    one_step
        .replace_range(Position::new(0, 0)..Position::new(2, 0), "")
        .unwrap();
    let mut two_steps = with_kinds(text, &paragraph_kinds);
    for _ in 0..2 {
        two_steps
            .replace_range(Position::new(0, 0)..Position::new(1, 0), "")
            .unwrap();
    }
    assert_eq!(
        lines(&one_step),
        [line("c", ordered(0, 1)), line("d", ordered(0, 2))]
    );
    assert_eq!(lines(&two_steps), lines(&one_step));

    let mut nested = with_kinds("x\ny\nz", &[ordered(1, 4), ordered(0, 8), ordered(1, 1)]);
    nested
        .set_selection(Selection::new(Position::new(0, 0), Position::new(2, 0)))
        .unwrap();
    nested.delete_backward().unwrap();
    assert_eq!(lines(&nested), [line("z", ordered(1, 1))]);

    let mut copied = with_kinds("a\nbody\nc", &[ordered(0, 6), BODY, ordered(0, 1)]);
    let original = Fragment::from_document(copied.document());
    copied
        .set_selection(Selection::new(Position::new(0, 0), Position::new(2, 0)))
        .unwrap();
    let fragment = copied.selected_fragment();
    copied.insert_fragment(&fragment).unwrap();
    assert_eq!(Fragment::from_document(copied.document()), original);
    assert_eq!(copied.undo_len(), 0);

    let paragraph_kinds = [
        BODY,
        BODY,
        ordered(2, 1),
        BODY,
        ordered(1, 1),
        ordered(2, 4),
    ];
    let mut batch = with_kinds("a  c\n\nb\n\nd\nc", &paragraph_kinds);
    assert_eq!(
        batch
            .replace_all("\n\n", "", SearchOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(
        lines(&batch),
        [line("a  cbd", BODY), line("c", ordered(2, 4))]
    );
}

#[test]
fn replace_all_adjacent_matches_take_kinds_like_sequential_replacement() {
    let paragraph_kinds = [BODY, ParagraphKind::Bullet { indent: 0 }, H1];
    let mut batch = with_kinds("\nb\nbx", &paragraph_kinds);
    let mut sequential = with_kinds("\nb\nbx", &paragraph_kinds);
    assert_eq!(
        batch
            .replace_all("\nb", "", SearchOptions::default())
            .unwrap(),
        2
    );
    for _ in 0..2 {
        sequential
            .replace_range(Position::new(0, 0)..Position::new(1, 1), "")
            .unwrap();
    }
    assert_eq!(lines(&sequential), [line("x", H1)]);
    assert_eq!(lines(&batch), lines(&sequential));
}

#[test]
fn replace_all_matches_at_paragraph_ends_take_styles_like_sequential_replacement() {
    let bold = StylePatch {
        bold: Some(true),
        ..Default::default()
    };
    let italic = StylePatch {
        italic: Some(true),
        ..Default::default()
    };
    let options = SearchOptions::default();
    let cases: [(&str, &[_], &str, &str, &[_]); 3] = [
        (
            "b\na\na",
            &[(1, 1, bold)],
            "\na",
            "X",
            &[(0..3, false, false)],
        ),
        (
            "x\n\ny",
            &[(0, 1, bold)],
            "\n",
            "-",
            &[(0..3, true, false), (3..4, false, false)],
        ),
        (
            "a\n\u{301}\nc",
            &[(0, 1, italic), (1, 2, bold)],
            "\n",
            "-",
            &[(0..5, false, true), (5..6, false, false)],
        ),
    ];
    for (text, styles, query, replacement, expected) in cases {
        let mut batch = Editor::from_text(text);
        for &(paragraph, end, patch) in styles {
            style(&mut batch, paragraph, 0, end, patch);
        }
        let mut sequential = Editor::new(batch.document().clone());
        caret(&mut sequential, 0, 0);
        assert_eq!(
            batch.replace_all(query, replacement, options).unwrap(),
            2,
            "{text:?}"
        );
        for _ in 0..2 {
            assert!(
                sequential
                    .find_next(query, options, SearchWrap::Stop)
                    .unwrap(),
                "{text:?}"
            );
            assert!(
                sequential
                    .replace_selected_match(query, replacement, options)
                    .unwrap(),
                "{text:?}"
            );
        }
        let spans: Vec<_> = batch.document().paragraphs()[0]
            .spans()
            .iter()
            .map(|span| (span.range.clone(), span.style.bold, span.style.italic))
            .collect();
        assert_eq!(spans, expected, "{text:?}");
        assert_eq!(
            batch.document().paragraphs(),
            sequential.document().paragraphs(),
            "{text:?}"
        );
    }
}

#[test]
fn replace_all_joining_many_paragraphs_into_growing_clusters_styles_like_sequential() {
    let bold = StylePatch {
        bold: Some(true),
        ..Default::default()
    };
    let options = SearchOptions::default();
    for count in [6, 1 << 14] {
        for (unit, replacement, bold_bytes) in [("\u{301}", "\u{300}", None), ("🇦", "🇧", Some(8))]
        {
            let mut batch = Editor::from_text(&vec![unit; count].join("\n"));
            style(&mut batch, 0, 0, unit.len(), bold);
            let mut sequential = Editor::new(batch.document().clone());
            assert_eq!(
                batch.replace_all("\n", replacement, options).unwrap(),
                count - 1,
                "{unit:?}, count {count}"
            );
            let [paragraph] = batch.document().paragraphs() else {
                panic!("matches join every paragraph");
            };
            let text = format!("{unit}{}", format!("{replacement}{unit}").repeat(count - 1));
            assert_eq!(paragraph.text(), text, "{unit:?}, count {count}");
            let bold_end = bold_bytes.unwrap_or(text.len());
            let mut expected = vec![(0..bold_end, true)];
            if bold_end < text.len() {
                expected.push((bold_end..text.len(), false));
            }
            let spans: Vec<_> = paragraph
                .spans()
                .iter()
                .map(|span| (span.range.clone(), span.style.bold))
                .collect();
            assert_eq!(spans, expected, "{unit:?}");
            if count < 16 {
                caret(&mut sequential, 0, 0);
                while sequential
                    .find_next("\n", options, SearchWrap::Stop)
                    .unwrap()
                {
                    assert!(
                        sequential
                            .replace_selected_match("\n", replacement, options)
                            .unwrap(),
                        "{unit:?}"
                    );
                }
                assert_eq!(
                    batch.document().paragraphs(),
                    sequential.document().paragraphs(),
                    "{unit:?}"
                );
            }
        }
    }
}

#[test]
fn edits_in_long_nested_lists_follow_distant_parents() {
    let mut text = String::from("parent");
    for _ in 0..200 {
        text.push_str("\nchild");
    }
    let mut editor = Editor::from_text(&text);
    editor.select_all();
    editor.set_paragraph_kind(ordered(1, 1)).unwrap();
    caret(&mut editor, 0, 0);
    editor.set_paragraph_kind(ordered(0, 3)).unwrap();
    let expected: Vec<_> = iter::once(ordered(0, 3))
        .chain((1..=200).map(|ordinal| ordered(1, ordinal)))
        .collect();
    assert_eq!(kinds(&editor), expected);

    caret(&mut editor, 200, 5);
    editor.insert_paragraph().unwrap();
    assert_eq!(kinds(&editor)[201], ordered(1, 201));
    editor.insert_paragraph().unwrap();
    assert_eq!(kinds(&editor)[201], ordered(0, 4));

    caret(&mut editor, 150, 0);
    editor.outdent_list().unwrap();
    let numbered = kinds(&editor);
    assert_eq!(
        numbered[149..153],
        [ordered(1, 149), ordered(0, 4), ordered(1, 1), ordered(1, 2)]
    );
    assert_eq!(numbered[200..], [ordered(1, 50), ordered(0, 5)]);
    assert!(editor.undo());
    assert_eq!(
        kinds(&editor)[149..152],
        [ordered(1, 149), ordered(1, 150), ordered(1, 151)]
    );

    let mut bullets = Editor::from_text(&["item"; 300].join("\n"));
    bullets.select_all();
    bullets
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 2 })
        .unwrap();
    caret(&mut bullets, 250, 4);
    bullets.insert_paragraph().unwrap();
    bullets.indent_list().unwrap();
    let bullet_kinds = kinds(&bullets);
    assert_eq!(bullet_kinds[251], ParagraphKind::Bullet { indent: 3 });
    assert!(
        bullet_kinds
            .iter()
            .enumerate()
            .all(|(index, kind)| index == 251 || *kind == ParagraphKind::Bullet { indent: 2 })
    );
}

/// Builds an unstyled document with one paragraph per line.
fn document_with(lines: &[Line]) -> Document {
    Document::from_paragraphs(lines.iter().map(|line| {
        let spans = if line.text.is_empty() {
            Vec::new()
        } else {
            vec![Span {
                range: 0..line.text.len(),
                style: InlineStyle::default(),
                link: None,
            }]
        };
        Paragraph::try_new(&line.text, spans, line.kind)
            .expect("one span covering the whole text, or none for empty text, is valid")
    }))
}

#[test]
fn seeded_list_documents_paste_over_themselves_and_batch_kinds_follow_sequential_edits() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let queries = ["a", "\n", "a\n", "\nb", "\n\n", " "];
    let replacements = ["", "X", "\n", "x\ny", "\n\n", "Z\n"];
    for case in 0..400 {
        let mut source = Vec::new();
        for _ in 0..1 + next(8) {
            let mut text = String::new();
            for _ in 0..next(4) {
                text.push(['a', 'b', ' '][next(3)]);
            }
            let paragraph_kind = match next(10) {
                0 => BODY,
                1 => ParagraphKind::Quote,
                2 => H1,
                3 | 4 => ParagraphKind::Bullet {
                    indent: next(3) as u8,
                },
                _ => ParagraphKind::Ordered {
                    indent: next(3) as u8,
                    start: (next(10) >= 7).then(|| 1 + next(9) as u32),
                },
            };
            source.push(Line {
                text,
                kind: paragraph_kind,
            });
        }
        let document = document_with(&source);
        let original = Fragment::from_document(&document);
        let mut position = || {
            let paragraph = next(source.len());
            Position::new(paragraph, next(source[paragraph].text.len() + 1))
        };
        let (anchor, focus) = (position(), position());

        let mut pasted = Editor::new(document.clone());
        pasted.set_selection(Selection::new(anchor, focus)).unwrap();
        let copied = pasted.selected_fragment();
        pasted.insert_fragment(&copied).unwrap();
        let context = format!("case {case}: {source:?} {anchor:?}..{focus:?}");
        assert_eq!(
            pasted.document().plain_text(),
            document.plain_text(),
            "{context}"
        );
        assert_eq!(shown(pasted.document()), shown(&document), "{context}");
        for (pasted, original) in pasted
            .document()
            .paragraphs()
            .iter()
            .zip(original.paragraphs())
        {
            assert_eq!(pasted.spans(), original.spans(), "{context}");
            match (pasted.kind(), original.kind()) {
                (
                    ParagraphKind::Ordered {
                        indent,
                        start: Some(1),
                    },
                    ParagraphKind::Ordered {
                        indent: level,
                        start: None,
                    },
                ) => assert_eq!(indent, level, "{context}"),
                (pasted, original) => assert_eq!(pasted, original, "{context}"),
            }
        }

        let query = queries[next(queries.len())];
        let replacement = replacements[next(replacements.len())];
        let mut batch = Editor::new(document);
        batch
            .replace_all(query, replacement, SearchOptions::default())
            .unwrap();
        assert_eq!(
            outcome(&batch),
            replace_sequentially(&original, query, replacement),
            "case {case}: {source:?} {query:?} -> {replacement:?}"
        );
    }
}

#[test]
fn kind_changes_never_rewrite_other_paragraphs_and_reverse_exactly() {
    let continuing = |indent| ParagraphKind::Ordered {
        indent,
        start: None,
    };
    for (converted, after) in [
        (BODY, [ordered(0, 2), ordered(0, 3)]),
        (ParagraphKind::Quote, [ordered(0, 2), ordered(0, 3)]),
        (
            ParagraphKind::Bullet { indent: 0 },
            [ordered(0, 2), ordered(0, 3)],
        ),
        (H1, [ordered(0, 1), ordered(0, 2)]),
    ] {
        let mut editor = with_kinds(
            "a\nb\nc\nd",
            &[ordered(0, 1), ordered(0, 2), ordered(0, 3), ordered(0, 4)],
        );
        let original = stored_kinds(&editor);
        kind(&mut editor, 1, converted);
        assert_eq!(
            kinds(&editor),
            [ordered(0, 1), converted, after[0], after[1]],
            "{converted:?}"
        );
        assert_eq!(stored_kinds(&editor)[2..], original[2..], "{converted:?}");
        assert_eq!(editor.undo_len(), 1, "{converted:?}");
        kind(&mut editor, 1, continuing(0));
        assert_eq!(stored_kinds(&editor), original, "{converted:?}");
        assert_eq!(
            kinds(&editor),
            [ordered(0, 1), ordered(0, 2), ordered(0, 3), ordered(0, 4)],
            "{converted:?}"
        );
    }
    let mut body = with_kinds("a\nb\nc", &[ordered(0, 1), ordered(0, 2), ordered(0, 3)]);
    kind(&mut body, 1, BODY);
    assert_eq!(
        body.document().to_html(),
        "<div style=\"white-space:pre-wrap\"><ol start=\"1\"><li>a</li></ol><p>b</p><ol start=\"2\"><li>c</li></ol></div>"
    );

    let mut restart = with_kinds("a\nb\nc", &[ordered(0, 1), ordered(0, 10), ordered(0, 11)]);
    kind(&mut restart, 1, BODY);
    assert_eq!(kinds(&restart), [ordered(0, 1), BODY, ordered(0, 2)]);
    let mut first = with_kinds("a\nb\nc", &[ordered(0, 5), ordered(0, 6), ordered(0, 7)]);
    first
        .set_selection(Selection::new(Position::new(0, 0), Position::new(1, 1)))
        .unwrap();
    first.set_paragraph_kind(H1).unwrap();
    assert_eq!(kinds(&first), [H1, H1, ordered(0, 1)]);

    let mut nested = with_kinds(
        "a\nx\nb\ny\nc",
        &[
            ordered(0, 1),
            ordered(1, 1),
            ordered(0, 2),
            ordered(1, 1),
            ordered(0, 3),
        ],
    );
    kind(&mut nested, 2, BODY);
    assert_eq!(
        kinds(&nested),
        [
            ordered(0, 1),
            ordered(1, 1),
            BODY,
            ordered(1, 2),
            ordered(0, 2)
        ]
    );
    let mut inner = with_kinds("a\nx\nb", &[ordered(0, 1), ordered(1, 1), ordered(0, 2)]);
    kind(&mut inner, 1, BODY);
    assert_eq!(kinds(&inner), [ordered(0, 1), BODY, ordered(0, 2)]);
    let mut bullet = with_kinds("a\nx\nb", &[ordered(0, 1), ordered(1, 1), ordered(0, 2)]);
    kind(&mut bullet, 1, ParagraphKind::Bullet { indent: 1 });
    assert_eq!(stored_kinds(&bullet)[2], continuing(0));
    assert_eq!(bullet.document().list_number(2), Some(2));

    let mut note = with_kinds(
        "a\nnote\nc\nd",
        &[ordered(0, 1), BODY, ordered(0, 2), ordered(0, 3)],
    );
    kind(&mut note, 1, continuing(0));
    assert_eq!(
        kinds(&note),
        [ordered(0, 1), ordered(0, 2), ordered(0, 3), ordered(0, 4)]
    );
    assert_eq!(stored_kinds(&note)[2], continuing(0));
    let mut kept = with_kinds("a\nnote\nc", &[ordered(0, 1), BODY, ordered(0, 5)]);
    kind(&mut kept, 1, continuing(0));
    assert_eq!(kinds(&kept), [ordered(0, 1), ordered(0, 2), ordered(0, 5)]);

    let mut indented = with_kinds("a\nb\nc", &[ordered(0, 1), ordered(0, 2), ordered(0, 3)]);
    caret(&mut indented, 1, 0);
    indented.indent_list().unwrap();
    assert_eq!(stored_kinds(&indented)[2], continuing(0));
    assert_eq!(
        kinds(&indented),
        [ordered(0, 1), ordered(1, 1), ordered(0, 2)]
    );

    let mut long = with_kinds(
        &["item"; 50].join("\n"),
        &(1..=50)
            .map(|number| ordered(0, number))
            .collect::<Vec<_>>(),
    );
    let paragraphs = long.document().paragraphs().to_vec();
    caret(&mut long, 10, 4);
    long.insert_paragraph().unwrap();
    long.set_selection(Selection::new(Position::new(3, 0), Position::new(5, 0)))
        .unwrap();
    long.delete_backward().unwrap();
    assert_eq!(long.document().paragraphs()[10..], paragraphs[11..]);
    let expected: Vec<_> = (1..=49).map(|number| ordered(0, number)).collect();
    assert_eq!(kinds(&long), expected);
}

fn numbers(editor: &Editor) -> Vec<Option<u32>> {
    (0..editor.document().paragraphs().len())
        .map(|index| editor.document().list_number(index))
        .collect()
}

fn numbered_list(items: &[&str]) -> Editor {
    let mut editor = Editor::from_text(&items.join("\n"));
    editor.select_all();
    editor
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: 0,
            start: None,
        })
        .expect("no IME composition is active");
    editor.clear_history();
    editor
}

#[test]
fn joining_an_item_into_the_previous_one_and_adding_items_keeps_numbers_consecutive() {
    let mut editor = numbered_list(&["a", "b", "c"]);
    caret(&mut editor, 1, 0);
    editor.delete_backward().unwrap();
    assert_eq!(numbers(&editor), [Some(1), None, Some(2)]);
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "ab\nc");
    assert_eq!(numbers(&editor), [Some(1), Some(2)]);
    caret(&mut editor, 0, 2);
    editor.insert_paragraph().unwrap();
    editor.insert_text("new").unwrap();
    assert_eq!(numbers(&editor), [Some(1), Some(2), Some(3)]);

    let mut editor = numbered_list(&["a", "b", "c"]);
    caret(&mut editor, 0, 1);
    editor.insert_paragraph().unwrap();
    editor.insert_paragraph().unwrap();
    assert_eq!(numbers(&editor), [Some(1), None, Some(2), Some(3)]);
    editor.delete_backward().unwrap();
    assert_eq!(editor.document().plain_text(), "a\nb\nc");
    caret(&mut editor, 0, 0);
    editor.insert_paragraph().unwrap();
    assert_eq!(numbers(&editor), [Some(1), Some(2), Some(3), Some(4)]);
    assert!(
        stored_kinds(&editor)
            .iter()
            .all(|kind| matches!(kind, ParagraphKind::Ordered { start: None, .. }))
    );
}

#[test]
fn toggling_list_items_off_and_on_restores_their_lists_exactly() {
    let original = [ordered(0, 5), ordered(0, 6), ordered(0, 7)];
    let mut editor = with_kinds("a\nb\nc", &original);
    let stored = stored_kinds(&editor);
    kind(&mut editor, 0, BODY);
    kind(&mut editor, 0, ordered(0, 5));
    assert_eq!(stored_kinds(&editor), stored);
    assert_eq!(kinds(&editor), original);

    let mut nested = with_kinds(
        "one\nsub\ntwo",
        &[ordered(0, 1), ordered(1, 1), ordered(0, 2)],
    );
    let stored = stored_kinds(&nested);
    kind(&mut nested, 1, ParagraphKind::Quote);
    kind(
        &mut nested,
        1,
        ParagraphKind::Ordered {
            indent: 1,
            start: None,
        },
    );
    assert_eq!(stored_kinds(&nested), stored);
    kind(&mut nested, 0, ordered(0, 5));
    assert_eq!(
        kinds(&nested),
        [ordered(0, 5), ordered(1, 1), ordered(0, 6)]
    );

    let mut restarted = with_kinds("a\nb", &[ordered(0, 1), ordered(0, 1)]);
    let stored = stored_kinds(&restarted);
    kind(&mut restarted, 0, BODY);
    kind(
        &mut restarted,
        0,
        ParagraphKind::Ordered {
            indent: 0,
            start: None,
        },
    );
    assert_eq!(stored_kinds(&restarted), stored);
    assert_eq!(kinds(&restarted), [ordered(0, 1), ordered(0, 1)]);
}

#[test]
fn seeded_kind_changes_and_their_reversal_restore_stored_kinds_exactly() {
    let mut seed = 0x0dd5_eed5_0000_0042u64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    for case in 0..2000 {
        let lines: Vec<_> = (0..2 + next(7))
            .map(|index| {
                let kind = match next(10) {
                    0 => BODY,
                    1 => ParagraphKind::Quote,
                    2 => H1,
                    3 => ParagraphKind::Bullet {
                        indent: next(3) as u8,
                    },
                    _ => ParagraphKind::Ordered {
                        indent: next(3) as u8,
                        start: (next(4) == 0).then(|| 1 + next(6) as u32),
                    },
                };
                Line {
                    text: format!("p{index}"),
                    kind,
                }
            })
            .collect();
        let document = document_with(&lines);
        let original = Fragment::from_document(&document);
        let index = next(lines.len());
        let converted = match next(5) {
            0 => BODY,
            1 => ParagraphKind::Quote,
            2 => H1,
            3 => ParagraphKind::Bullet {
                indent: next(3) as u8,
            },
            _ => ParagraphKind::Ordered {
                indent: next(3) as u8,
                start: (next(2) == 0).then(|| 1 + next(6) as u32),
            },
        };
        let mut editor = Editor::new(document);
        kind(&mut editor, index, converted);
        let paragraph_pairs = original
            .paragraphs()
            .iter()
            .zip(editor.document().paragraphs())
            .enumerate();
        for (other, (before, after)) in paragraph_pairs {
            if other != index {
                assert!(Arc::ptr_eq(before, after), "case {case}: {lines:?}");
            }
        }
        kind(&mut editor, index, lines[index].kind);
        assert_eq!(
            Fragment::from_document(editor.document()),
            original,
            "case {case}: {lines:?} {index} -> {converted:?}"
        );
    }
}

#[test]
fn numbered_list_command_extends_a_list_right_before_it_or_starts_one() {
    let continuing = ParagraphKind::Ordered {
        indent: 0,
        start: None,
    };
    let new_list = ParagraphKind::Ordered {
        indent: 0,
        start: Some(1),
    };
    let mut editor = with_kinds("a\nb\nc\nd", &[ordered(0, 1), ordered(0, 2), BODY, BODY]);
    caret(&mut editor, 2, 0);
    editor.apply_numbered_list(0).unwrap();
    assert_eq!(stored_kinds(&editor)[2], continuing);
    assert_eq!(kinds(&editor)[2], ordered(0, 3));
    let mut editor = with_kinds("a\nb\nc\nd", &[ordered(0, 1), BODY, BODY, BODY]);
    editor
        .set_selection(Selection::new(Position::new(2, 0), Position::new(3, 1)))
        .unwrap();
    editor.apply_numbered_list(0).unwrap();
    assert_eq!(stored_kinds(&editor)[2..], [new_list, continuing]);
    assert_eq!(kinds(&editor)[2..], [ordered(0, 1), ordered(0, 2)]);
    assert_eq!(editor.undo_len(), 1);
    let revision = editor.document().revision();
    editor.apply_numbered_list(0).unwrap();
    assert_eq!(editor.document().revision(), revision);
    assert_eq!(editor.undo_len(), 1);
    let mut editor = with_kinds("a\nb\nc", &[ordered(0, 5), BODY, ordered(0, 9)]);
    editor.select_all();
    editor.apply_numbered_list(0).unwrap();
    assert_eq!(
        kinds(&editor),
        [ordered(0, 5), ordered(0, 6), ordered(0, 7)]
    );
    let mut editor = with_kinds("a\nb\nc", &[ordered(0, 1), ordered(1, 1), BODY]);
    caret(&mut editor, 2, 0);
    editor.apply_numbered_list(1).unwrap();
    assert_eq!(
        kinds(&editor),
        [ordered(0, 1), ordered(1, 1), ordered(1, 2)]
    );
    caret(&mut editor, 1, 0);
    editor.apply_numbered_list(0).unwrap();
    assert_eq!(
        kinds(&editor),
        [ordered(0, 1), ordered(0, 2), ordered(1, 1)]
    );
    editor.update_composition("x", None).unwrap();
    assert_eq!(editor.apply_numbered_list(0), Err(Error::CompositionActive));
}

#[test]
fn numbered_list_command_extends_a_list_whose_last_item_is_nested() {
    let continuing = |indent| ParagraphKind::Ordered {
        indent,
        start: None,
    };
    for nested in [ordered(1, 1), ParagraphKind::Bullet { indent: 1 }] {
        let mut editor = with_kinds("one\nsub\ntwo", &[ordered(0, 1), nested, BODY]);
        caret(&mut editor, 2, 0);
        editor.apply_numbered_list(0).unwrap();
        assert_eq!(stored_kinds(&editor)[2], continuing(0), "{nested:?}");
        assert_eq!(
            kinds(&editor),
            [ordered(0, 1), nested, ordered(0, 2)],
            "{nested:?}"
        );
    }
    let mut editor = with_kinds(
        "a\nb\nc\nd",
        &[
            ordered(0, 1),
            ordered(1, 1),
            ParagraphKind::Bullet { indent: 2 },
            BODY,
        ],
    );
    caret(&mut editor, 3, 0);
    editor.apply_numbered_list(1).unwrap();
    assert_eq!(stored_kinds(&editor)[3], continuing(1));
    assert_eq!(kinds(&editor)[3], ordered(1, 2));

    let bullet = ParagraphKind::Bullet { indent: 0 };
    for before in [bullet, H1] {
        let mut editor = with_kinds("a\nb\nc\nd", &[ordered(0, 1), before, ordered(1, 1), BODY]);
        caret(&mut editor, 3, 0);
        editor.apply_numbered_list(0).unwrap();
        assert_eq!(stored_kinds(&editor)[3], ordered(0, 1), "{before:?}");
    }
}
