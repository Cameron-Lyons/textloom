//! Measures the editing core without a GUI: edits with undo, transactions,
//! snapshot and HTML serialization, formatting, Unicode navigation, search, and
//! replacement, on documents of up to 100,000 paragraphs. With the `accesskit`
//! feature it also measures AccessKit position conversion and tree updates.
//!
//! Each benchmark prints the mean time per operation in microseconds. Setting
//! `TEXTLOOM_BENCH_QUICK=1` runs a tenth of the iterations as a smoke test.

use std::time::{Duration, Instant};
use std::{env, hint, slice};
use textloom::accessibility::Focus;
use textloom::{
    Document, Editor, Fragment, HistoryLimits, Link, Movement, ParagraphKind, Position,
    SearchOptions, SearchWrap, Selection, SelectionMode, StylePatch,
};

/// `expect` message for editing calls, which fail only while an IME
/// composition is active. The benchmarks commit every composition they start.
const NO_COMPOSITION: &str = "no IME composition is active";

/// `expect` message for selection changes, which fail only for positions
/// outside the document or inside a grapheme.
const VALID_POSITION: &str = "benchmark positions are grapheme boundaries inside the document";

/// `expect` message for decoding a snapshot that `Document::to_bytes` produced.
const OWN_SNAPSHOT: &str = "the bytes come from `Document::to_bytes`";

fn main() {
    short_paragraph_edits();
    transaction_edits();
    paragraph_import();
    fragment_capture();
    numbering_cache();
    saved_state_checks();
    serialization();
    formatting();
    caret_link_queries();
    empty_insertions();
    unicode_navigation();
    cold_zero_index_navigation();
    long_unicode_edits();
    contextual_unicode_search();
    ascii_navigation();
    literal_search();
    sparse_whole_word_search();
    search_navigation();
    long_literal_navigation();
    long_whole_word_navigation();
    batch_replacement();
    sparse_replacement();
    #[cfg(feature = "accesskit")]
    accessibility_navigation();
}

fn transaction_edits() {
    for (label, nested) in [("transaction", false), ("nested transaction", true)] {
        let text = "A short paragraph with café.";
        let mut editor = Editor::from_text(text);
        measure(
            &format!("short Unicode paragraph {label} insert + undo"),
            5_000,
            || {
                editor
                    .transaction(|editor| {
                        if nested {
                            editor.transaction(|editor| editor.insert_text(hint::black_box("x")))
                        } else {
                            editor.insert_text(hint::black_box("x"))
                        }
                    })
                    .expect(NO_COMPOSITION);
                assert!(
                    editor.undo(),
                    "the committed {label} should be on the undo stack"
                );
            },
        );
        assert_eq!(
            editor.document().plain_text(),
            text,
            "every {label} should have been undone"
        );
    }
}

fn fragment_capture() {
    for paragraphs in [100, 100_000] {
        let source = "A short paragraph with Unicode café 👩\u{200d}💻.\n".repeat(paragraphs);
        let document = Document::from_text(&source);
        let range = Position::new(paragraphs - 1, 2)..Position::new(paragraphs - 1, 7);
        assert_eq!(
            document
                .fragment(range.clone())
                .expect("the range is inside the last nonempty paragraph")
                .plain_text(),
            "short",
            "the benchmark should capture the paragraph's second word"
        );
        measure(
            &format!("{paragraphs} paragraphs short fragment capture near document end"),
            2_000,
            || {
                hint::black_box(
                    document
                        .fragment(hint::black_box(range.clone()))
                        .expect("the range is inside the last nonempty paragraph"),
                );
            },
        );
    }
}

fn numbering_cache() {
    for paragraphs in [100, 100_000] {
        let mut editor = Editor::from_text(&vec!["item"; paragraphs].join("\n"));
        editor.select_all();
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: None,
            })
            .expect(NO_COMPOSITION);
        editor
            .set_selection(Selection::caret(Position::default()))
            .expect(VALID_POSITION);
        editor.set_history_limits(HistoryLimits {
            max_entries: 0,
            max_bytes: 0,
        });
        for (label, first, second) in [
            ("body/quote", ParagraphKind::Body, ParagraphKind::Quote),
            (
                "heading level",
                ParagraphKind::Heading { level: 1 },
                ParagraphKind::Heading { level: 2 },
            ),
        ] {
            editor
                .set_paragraph_kind(first)
                .expect("no IME composition is active, and both heading levels are valid");
            let expected = Some((paragraphs - 1) as u32);
            assert_eq!(
                editor.document().list_number(paragraphs - 1),
                expected,
                "with the first paragraph out of the list, the other items should be numbered from 1"
            );
            let mut kind = second;
            measure(
                &format!("{paragraphs} paragraphs {label} edit and cached list number"),
                1_000,
                || {
                    editor
                        .set_paragraph_kind(kind)
                        .expect("no IME composition is active, and both heading levels are valid");
                    assert_eq!(
                        hint::black_box(editor.document()).list_number(paragraphs - 1),
                        expected,
                        "switching the first paragraph between {label} kinds should not renumber the list"
                    );
                    kind = if kind == first { second } else { first };
                },
            );
        }
    }
}

fn saved_state_checks() {
    for paragraphs in [100, 100_000] {
        let mut editor = Editor::from_text(&"A short paragraph.\n".repeat(paragraphs));
        editor
            .set_selection(Selection::caret(editor.document().end()))
            .expect("the document end is always a valid caret position");
        editor.insert_text("x").expect(NO_COMPOSITION);
        assert!(
            editor.is_modified(),
            "the insertion should move the document away from its saved state"
        );
        measure(
            &format!("{paragraphs} paragraphs repeated dirty-state check"),
            20_000,
            || {
                hint::black_box(hint::black_box(&editor).is_modified());
            },
        );
        assert!(editor.undo(), "the insertion should be on the undo stack");
        assert!(
            !editor.is_modified(),
            "undoing the only edit should return the document to its saved state"
        );
        measure(
            &format!("{paragraphs} paragraphs repeated restored-state check"),
            20_000,
            || {
                hint::black_box(hint::black_box(&editor).is_modified());
            },
        );
    }
}

fn paragraph_import() {
    let source = "A short paragraph with Unicode café 👩\u{200d}💻.\n".repeat(100_000);
    let bytes = Document::from_text(&source).to_bytes();
    measure("100000 paragraph fragment import", 20, || {
        let fragment = Fragment::from_text(hint::black_box(&source));
        assert_eq!(
            fragment.paragraphs().len(),
            100_001,
            "the trailing newline should start a final empty paragraph"
        );
        hint::black_box(fragment);
    });
    measure("100000 paragraph snapshot import", 20, || {
        let document = Document::from_bytes(hint::black_box(&bytes)).expect(OWN_SNAPSHOT);
        assert_eq!(
            document.paragraphs().len(),
            100_001,
            "the snapshot should keep every paragraph, including the final empty one"
        );
        assert_eq!(
            document.revision(),
            0,
            "decoding should build a fresh document rather than edit one"
        );
        hint::black_box(document);
    });
    let source = "café 👩\u{200d}💻. ".repeat(100_000);
    let bytes = Document::from_text(&source).to_bytes();
    measure("long Unicode paragraph snapshot import", 50, || {
        let document = Document::from_bytes(hint::black_box(&bytes)).expect(OWN_SNAPSHOT);
        assert_eq!(
            document.paragraphs().len(),
            1,
            "the source text has no newlines"
        );
        assert_eq!(
            document.paragraphs()[0].text().len(),
            source.len(),
            "the snapshot should keep the whole paragraph"
        );
        hint::black_box(document);
    });
}

fn serialization() {
    let source = "A short paragraph with Unicode café 👩\u{200d}💻.\n".repeat(100_000);
    let document = Document::from_text(&source);
    let expected_bytes = document.to_bytes();
    let expected_html = document.to_html();
    println!(
        "100000 paragraphs HTML output: {} bytes, {} capacity",
        expected_html.len(),
        expected_html.capacity()
    );
    measure("100000 paragraphs native snapshot export", 20, || {
        let bytes = hint::black_box(&document).to_bytes();
        assert_eq!(
            bytes.len(),
            expected_bytes.len(),
            "exporting an unchanged document should give the same snapshot"
        );
        hint::black_box(bytes);
    });
    measure("100000 paragraphs HTML export", 20, || {
        let html = hint::black_box(&document).to_html();
        assert_eq!(
            html.len(),
            expected_html.len(),
            "exporting an unchanged document should give the same HTML"
        );
        hint::black_box(html);
    });

    let document = styled_repetitions("match café 👩\u{200d}💻 ", 10_000);
    let expected_bytes = document.to_bytes();
    measure("10000 styled spans native snapshot export", 100, || {
        let bytes = hint::black_box(&document).to_bytes();
        assert_eq!(
            bytes.len(),
            expected_bytes.len(),
            "exporting an unchanged document should give the same snapshot"
        );
        hint::black_box(bytes);
    });
    assert_eq!(
        Document::from_bytes(&expected_bytes).expect(OWN_SNAPSHOT),
        document,
        "the native snapshot should round-trip styled spans losslessly"
    );
}

/// Returns a one-paragraph document of `phrase` repeated `repetitions` times,
/// with each repetition in its own span, alternating bold and italic.
///
/// The document is decoded from a hand-built native snapshot so that building
/// many spans doesn't depend on the editing code being measured.
fn styled_repetitions(phrase: &str, repetitions: usize) -> Document {
    let text = phrase.repeat(repetitions);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"TLFR\x02");
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&(text.len() as u64).to_le_bytes());
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(&(repetitions as u64).to_le_bytes());
    for index in 0..repetitions {
        bytes.extend_from_slice(&((index + 1) as u64 * phrase.len() as u64).to_le_bytes());
        bytes.push(if index % 2 == 0 { 1 } else { 2 });
    }
    Document::from_bytes(&bytes)
        .expect("the snapshot has one body paragraph whose spans cover its text in order")
}

fn caret_link_queries() {
    for repetitions in [100, 100_000] {
        let mut editor = Editor::new(styled_repetitions("é", repetitions));
        let target =
            Link::try_new("https://example.com").expect("a short URL is a valid link target");
        editor.select_all();
        editor.set_link(Some(target.clone())).expect(NO_COMPOSITION);
        editor.clear_history();
        for (label, byte) in [
            ("start", 0),
            ("middle", repetitions),
            ("end", repetitions * 2),
        ] {
            editor
                .set_selection(Selection::caret(Position::new(0, byte)))
                .expect("each \"é\" is two bytes, so every even offset is a grapheme boundary");
            assert_eq!(
                editor.selected_link(),
                Some(target.clone()),
                "the whole paragraph is linked, so a caret at its {label} should report the link"
            );
            measure(
                &format!("{repetitions} linked styled spans caret target at {label}"),
                20_000,
                || {
                    hint::black_box(hint::black_box(&editor).selected_link());
                },
            );
        }
    }
}

fn formatting() {
    let phrase = "match café 👩\u{200d}💻 ";
    for repetitions in [1, 10_000] {
        let document = styled_repetitions(phrase, repetitions);
        let original = document.to_bytes();
        let mut editor = Editor::new(document);
        let start = repetitions / 2 * phrase.len() + 1;
        let local = Selection::new(Position::new(0, start), Position::new(0, start + 4));
        let unchanged = StylePatch {
            underline: Some(false),
            ..StylePatch::default()
        };
        let changed = StylePatch {
            underline: Some(true),
            ..StylePatch::default()
        };
        editor.set_selection(local).expect(VALID_POSITION);
        measure(
            &format!("{repetitions} styled spans local unchanged formatting"),
            5_000,
            || {
                editor
                    .apply_style(hint::black_box(unchanged))
                    .expect(NO_COMPOSITION);
            },
        );
        assert_eq!(
            editor.undo_len(),
            0,
            "nothing is underlined, so removing underline should not record an undo step"
        );
        assert_eq!(
            editor.document().to_bytes(),
            original,
            "nothing is underlined, so removing underline should not change the document"
        );
        measure(
            &format!("{repetitions} styled spans local formatting + undo"),
            1_000,
            || {
                editor
                    .apply_style(hint::black_box(changed))
                    .expect(NO_COMPOSITION);
                assert!(editor.undo(), "underlining should record an undo step");
                assert_eq!(
                    editor.selection(),
                    local,
                    "undo should restore the selection for the next iteration"
                );
            },
        );
        assert_eq!(
            editor.document().to_bytes(),
            original,
            "every underline should have been undone"
        );

        editor.select_all();
        measure(
            &format!("{repetitions} styled spans full unchanged formatting"),
            500,
            || {
                editor
                    .apply_style(hint::black_box(unchanged))
                    .expect(NO_COMPOSITION);
            },
        );
        measure(
            &format!("{repetitions} styled spans full formatting + undo"),
            100,
            || {
                editor
                    .apply_style(hint::black_box(changed))
                    .expect(NO_COMPOSITION);
                assert!(editor.undo(), "underlining should record an undo step");
            },
        );
        measure(
            &format!("{repetitions} styled spans clear formatting + undo"),
            100,
            || {
                editor.clear_formatting().expect(NO_COMPOSITION);
                assert!(
                    editor.undo(),
                    "every span is bold or italic, so clearing formatting should record an undo step"
                );
            },
        );
        assert_eq!(
            editor.document().to_bytes(),
            original,
            "every formatting edit should have been undone"
        );
    }
}

fn empty_insertions() {
    for repetitions in [100, 10_000] {
        let document = styled_repetitions("é", repetitions);
        let original = document.to_bytes();
        let mut editor = Editor::new(document);
        let fragment = Fragment::from_text("");
        measure(
            &format!("{repetitions} styled spans empty text insertion"),
            1_000,
            || {
                editor
                    .insert_text(hint::black_box(""))
                    .expect(NO_COMPOSITION);
            },
        );
        measure(
            &format!("{repetitions} styled spans empty fragment insertion"),
            1_000,
            || {
                editor
                    .insert_fragment(hint::black_box(&fragment))
                    .expect(NO_COMPOSITION);
            },
        );
        measure(
            &format!("{repetitions} styled spans empty composition commit"),
            1_000,
            || {
                editor.begin_composition();
                editor
                    .commit_composition(hint::black_box(""))
                    .expect("the composition replaces the editor's own, valid selection");
            },
        );
        assert!(
            editor.composition().is_none(),
            "every composition should have been committed"
        );
        assert_eq!(
            editor.undo_len(),
            0,
            "empty insertions should not record undo steps"
        );
        assert_eq!(
            editor.document().to_bytes(),
            original,
            "empty insertions should not change the document"
        );
    }
}

fn cold_zero_index_navigation() {
    for repetitions in [100, 100_000] {
        let source = format!("x\n{}", "é👩\u{200d}💻e\u{301}".repeat(repetitions));
        let operations = [
            "grapheme zero conversion",
            "scalar zero conversion",
            "paragraph-down at column zero",
        ];
        for (operation, label) in operations.into_iter().enumerate() {
            let iterations = scaled(30);
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                // Each operation gets an unindexed paragraph; construction is not timed.
                let mut editor = Editor::from_text(&source);
                let start = Instant::now();
                match operation {
                    0 => assert_eq!(
                        hint::black_box(
                            editor
                                .document()
                                .paragraph(1)
                                .expect("the source has two paragraphs")
                        )
                        .byte_from_grapheme(hint::black_box(0)),
                        Some(0),
                        "a paragraph's first grapheme starts at its first byte"
                    ),
                    1 => assert_eq!(
                        hint::black_box(
                            editor
                                .document()
                                .paragraph(1)
                                .expect("the source has two paragraphs")
                        )
                        .byte_from_scalar(hint::black_box(0)),
                        Some(0),
                        "a paragraph's first scalar starts at its first byte"
                    ),
                    _ => editor
                        .move_cursor(
                            hint::black_box(Movement::ParagraphDown),
                            SelectionMode::Move,
                        )
                        .expect(NO_COMPOSITION),
                }
                elapsed += start.elapsed();
            }
            println!(
                "{repetitions} Unicode repetitions cold {label}: {:.2} µs / operation ({iterations} iterations)",
                elapsed.as_secs_f64() * 1_000_000.0 / iterations as f64,
            );
        }
    }
}

fn sparse_whole_word_search() {
    let tail = "other café 👩\u{200d}💻. ".repeat(50_000);
    let source = format!("match {tail}");
    let document = Document::from_text(&source);
    let options = SearchOptions {
        whole_word: true,
        ..SearchOptions::default()
    };
    let expected = Position::new(0, 0)..Position::new(0, 5);
    assert_eq!(
        document.find("match", options).as_slice(),
        slice::from_ref(&expected),
        "\"match\" occurs only once, at the start of the document"
    );
    measure("long Unicode paragraph sparse whole-word find", 50, || {
        assert_eq!(
            hint::black_box(&document)
                .find(hint::black_box("match"), options)
                .as_slice(),
            slice::from_ref(&expected),
            "\"match\" occurs only once, at the start of the document"
        );
    });
    let mut editor = Editor::new(document);
    measure(
        "long Unicode paragraph whole-word find_next from start",
        100,
        || {
            editor
                .set_selection(Selection::default())
                .expect(VALID_POSITION);
            assert!(
                editor
                    .find_next(hint::black_box("match"), options, SearchWrap::Stop)
                    .expect(NO_COMPOSITION),
                "the document starts with \"match\""
            );
            assert_eq!(
                editor.selection(),
                Selection::new(expected.start, expected.end),
                "\"match\" occurs only once, at the start of the document"
            );
        },
    );
    for query in ["match", "abcaX", "aa"] {
        let document = Document::from_text(&format!("{query}X {tail}"));
        assert!(
            document.find(query, options).is_empty(),
            "{query:?} occurs only as a prefix of the first word"
        );
        measure(
            &format!("long Unicode paragraph rejected whole-word {query:?}"),
            50,
            || {
                assert!(
                    document.find(hint::black_box(query), options).is_empty(),
                    "{query:?} occurs only as a prefix of the first word"
                );
            },
        );
    }
}

fn short_paragraph_edits() {
    for paragraphs in [100, 10_000, 100_000] {
        let source = "A short paragraph with Unicode café 👩\u{200d}💻.\n".repeat(paragraphs);
        let mut editor = Editor::from_text(&source);
        editor
            .set_selection(Selection::caret(Position::new(paragraphs / 2, 5)))
            .expect(VALID_POSITION);
        let iterations = scaled(2_000);
        let start = Instant::now();
        for _ in 0..iterations {
            editor
                .insert_text(hint::black_box("x"))
                .expect(NO_COMPOSITION);
            editor.break_history_group();
            assert!(editor.undo(), "the insertion should be on the undo stack");
        }
        println!(
            "{paragraphs:>6} paragraphs: {:.2} µs / insert + undo",
            start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64
        );
    }
}

fn unicode_navigation() {
    let phrase = "café 👩\u{200d}💻 नमस्ते 🇺🇸 ";
    let source = phrase.repeat(10_000);
    let position = Position::new(0, phrase.len() * 9_000);
    println!("long Unicode paragraph: {} bytes", source.len());
    let document = Document::from_text(&source);
    let start = Instant::now();
    document
        .validate_position(hint::black_box(position))
        .expect("the position falls between two repetitions of the phrase");
    println!(
        "validation cold: {:.2} µs / first query",
        start.elapsed().as_secs_f64() * 1_000_000.0
    );
    measure("validation warm", 2_000, || {
        hint::black_box(document.validate_position(hint::black_box(position)))
            .expect("the position falls between two repetitions of the phrase");
    });

    let mut editor = Editor::from_text(&source);
    editor
        .set_selection(Selection::caret(position))
        .expect(VALID_POSITION);
    let start = Instant::now();
    grapheme_pair(&mut editor);
    println!(
        "grapheme backward + forward cold: {:.2} µs / first pair",
        start.elapsed().as_secs_f64() * 1_000_000.0
    );
    measure("grapheme backward + forward warm", 2_000, || {
        grapheme_pair(&mut editor);
    });

    let mut editor = Editor::from_text(&source);
    editor
        .set_selection(Selection::caret(position))
        .expect(VALID_POSITION);
    let start = Instant::now();
    word_pair(&mut editor);
    println!(
        "word backward + forward cold: {:.2} µs / first pair",
        start.elapsed().as_secs_f64() * 1_000_000.0
    );
    measure("word backward + forward warm", 2_000, || {
        word_pair(&mut editor);
    });

    let mut editor = Editor::from_text(&format!("{source}\n{source}"));
    editor
        .set_selection(Selection::caret(position))
        .expect(VALID_POSITION);
    let start = Instant::now();
    paragraph_pair(&mut editor);
    println!(
        "paragraph down + up cold: {:.2} µs / first pair",
        start.elapsed().as_secs_f64() * 1_000_000.0
    );
    measure("paragraph down + up warm", 1_000, || {
        paragraph_pair(&mut editor);
    });
}

/// Moves the caret one grapheme backward and forward again.
fn grapheme_pair(editor: &mut Editor) {
    let original = editor.selection();
    editor
        .move_cursor(Movement::GraphemeBackward, SelectionMode::Move)
        .expect(NO_COMPOSITION);
    editor
        .move_cursor(Movement::GraphemeForward, SelectionMode::Move)
        .expect(NO_COMPOSITION);
    assert_eq!(
        editor.selection(),
        original,
        "the caret is between graphemes away from the paragraph edges, so the moves should cancel out"
    );
}

/// Moves the caret one word backward and forward again.
fn word_pair(editor: &mut Editor) {
    editor
        .move_cursor(Movement::WordBackward, SelectionMode::Move)
        .expect(NO_COMPOSITION);
    editor
        .move_cursor(Movement::WordForward, SelectionMode::Move)
        .expect(NO_COMPOSITION);
    hint::black_box(editor.selection());
}

fn long_unicode_edits() {
    let phrase = "café 👩\u{200d}💻 नमस्ते 🇺🇸 ";
    let source = phrase.repeat(10_000);
    let original = Selection::caret(Position::new(0, phrase.len() * 5_000));
    let mut editor = Editor::from_text(&source);
    editor.set_selection(original).expect(VALID_POSITION);
    measure("long Unicode paragraph middle insert + undo", 200, || {
        editor
            .insert_text(hint::black_box("x"))
            .expect(NO_COMPOSITION);
        editor.break_history_group();
        assert!(editor.undo(), "the insertion should be on the undo stack");
        assert_eq!(
            editor.selection(),
            original,
            "undo should restore the caret for the next iteration"
        );
    });
    assert_eq!(
        editor.document().plain_text(),
        source,
        "every insertion should have been undone"
    );

    for backward in [true, false] {
        let mut editor = Editor::from_text(&source);
        editor.set_selection(original).expect(VALID_POSITION);
        measure(
            if backward {
                "long Unicode paragraph insert + backspace + undo"
            } else {
                "long Unicode paragraph insert + delete + undo"
            },
            200,
            || {
                editor
                    .insert_text(hint::black_box("x"))
                    .expect(NO_COMPOSITION);
                if backward {
                    editor.delete_backward().expect(NO_COMPOSITION);
                } else {
                    editor.delete_forward().expect(NO_COMPOSITION);
                }
                assert!(editor.undo(), "the deletion should be on the undo stack");
                assert!(
                    editor.undo(),
                    "the insertion should be an undo step separate from the deletion"
                );
                assert_eq!(
                    editor.selection(),
                    original,
                    "undo should restore the caret for the next iteration"
                );
            },
        );
        assert_eq!(
            editor.document().plain_text(),
            source,
            "every edit should have been undone"
        );
    }

    let source = "🇺🇸".repeat(10_000);
    let original = Selection::caret(Position::new(0, source.len() / 2));
    let mut editor = Editor::from_text(&source);
    editor.set_selection(original).expect(VALID_POSITION);
    measure("long regional run middle insert + undo", 100, || {
        editor
            .insert_text(hint::black_box("🇨🇦"))
            .expect(NO_COMPOSITION);
        editor.break_history_group();
        assert!(editor.undo(), "the insertion should be on the undo stack");
        assert_eq!(
            editor.selection(),
            original,
            "undo should restore the caret for the next iteration"
        );
    });
    assert_eq!(
        editor.document().plain_text(),
        source,
        "every insertion should have been undone"
    );
}

fn contextual_unicode_search() {
    let document = Document::from_text(&"🇺🇸".repeat(10_000));
    assert_eq!(
        document.find("🇺🇸", SearchOptions::default()).len(),
        10_000,
        "regional indicators pair up from the start of the run, so every flag should match"
    );
    measure("10000 regional flag literal matches", 50, || {
        let matches = document.find(hint::black_box("🇺🇸"), SearchOptions::default());
        assert_eq!(
            matches.len(),
            10_000,
            "regional indicators pair up from the start of the run, so every flag should match"
        );
        hint::black_box(matches);
    });
    for count in [1_000, 16_000] {
        for (kind, source, query) in [
            (
                "combining scalars",
                "\u{301}".repeat(count),
                "\u{301}".repeat(count / 2),
            ),
            (
                "repeated graphemes",
                "a\u{301}".repeat(count),
                format!("{}a", "a\u{301}".repeat(count / 2)),
            ),
        ] {
            let document = Document::from_text(&source);
            assert!(
                document.find(&query, SearchOptions::default()).is_empty(),
                "the query ends inside a grapheme of the {kind} source, so it should not match"
            );
            measure(
                &format!("{count} {kind} partial-grapheme literal miss"),
                50,
                || {
                    assert!(
                        document
                            .find(hint::black_box(&query), SearchOptions::default())
                            .is_empty(),
                        "the query ends inside a grapheme of the {kind} source, so it should not match"
                    );
                },
            );
        }
    }
}

/// Moves the caret one paragraph down and up again.
fn paragraph_pair(editor: &mut Editor) {
    editor
        .move_cursor(Movement::ParagraphDown, SelectionMode::Move)
        .expect(NO_COMPOSITION);
    editor
        .move_cursor(Movement::ParagraphUp, SelectionMode::Move)
        .expect(NO_COMPOSITION);
    hint::black_box(editor.selection());
}

fn ascii_navigation() {
    let phrase = "A short paragraph with ASCII words. ";
    let source = phrase.repeat(10_000);
    let mut editor = Editor::from_text(&source);
    editor
        .set_selection(Selection::caret(Position::new(0, phrase.len() * 9_000)))
        .expect(VALID_POSITION);
    let start = Instant::now();
    word_pair(&mut editor);
    println!(
        "ASCII word backward + forward cold: {:.2} µs / first pair",
        start.elapsed().as_secs_f64() * 1_000_000.0
    );
    measure("ASCII word backward + forward warm", 2_000, || {
        word_pair(&mut editor);
    });
}

fn literal_search() {
    let needle = "A repeated literal search phrase. ".repeat(32);
    let document = Document::from_text(&needle.repeat(1_000));
    measure("1000 long literal matches", 50, || {
        let matches = document.find(hint::black_box(&needle), SearchOptions::default());
        assert_eq!(
            matches.len(),
            1_000,
            "the document is the needle repeated 1000 times"
        );
        hint::black_box(matches);
    });
    measure("long paragraph literal miss", 50, || {
        assert!(
            document
                .find(hint::black_box("absent"), SearchOptions::default())
                .is_empty(),
            "the document never contains \"absent\""
        );
    });
    let document =
        Document::from_text(&"A short paragraph with Unicode café 👩\u{200d}💻.\n".repeat(100_000));
    measure("100000 paragraph literal matches", 20, || {
        let matches = document.find(hint::black_box("Unicode"), SearchOptions::default());
        assert_eq!(
            matches.len(),
            100_000,
            "each nonempty paragraph contains \"Unicode\" once"
        );
        hint::black_box(matches);
    });
    measure("100000 paragraph whole-word matches", 10, || {
        let matches = document.find(
            hint::black_box("Unicode"),
            SearchOptions {
                case_sensitive: true,
                whole_word: true,
            },
        );
        assert_eq!(
            matches.len(),
            100_000,
            "each nonempty paragraph contains the word \"Unicode\" once"
        );
        hint::black_box(matches);
    });
    measure("100000 paragraph lowercase whole-word matches", 10, || {
        let matches = document.find(
            hint::black_box("unicode"),
            SearchOptions {
                case_sensitive: false,
                whole_word: true,
            },
        );
        assert_eq!(
            matches.len(),
            100_000,
            "each nonempty paragraph contains the word \"Unicode\" once, and case is ignored"
        );
        hint::black_box(matches);
    });
    measure("100000 paragraph literal miss", 20, || {
        assert!(
            document
                .find(hint::black_box("absent"), SearchOptions::default())
                .is_empty(),
            "the document never contains \"absent\""
        );
    });
    for case_sensitive in [true, false] {
        measure(
            if case_sensitive {
                "100000 paragraph whole-word miss"
            } else {
                "100000 paragraph lowercase whole-word miss"
            },
            20,
            || {
                assert!(
                    document
                        .find(
                            hint::black_box("absent"),
                            SearchOptions {
                                case_sensitive,
                                whole_word: true,
                            },
                        )
                        .is_empty(),
                    "the document never contains \"absent\""
                );
            },
        );
    }
    measure("100000 paragraphs shorter than query", 20, || {
        assert!(
            document
                .find(hint::black_box(&needle), SearchOptions::default())
                .is_empty(),
            "every paragraph is shorter than the needle"
        );
    });
    measure("100000 paragraphs shorter than lowercase query", 20, || {
        assert!(
            document
                .find(
                    hint::black_box(&needle),
                    SearchOptions {
                        case_sensitive: false,
                        whole_word: true
                    }
                )
                .is_empty(),
            "every paragraph is shorter than the needle"
        );
    });
}

fn batch_replacement() {
    let mut editor = Editor::from_text(&"match ".repeat(10_000));
    assert_eq!(
        editor
            .replace_all("match", "replacement", SearchOptions::default())
            .expect(NO_COMPOSITION),
        10_000,
        "the text repeats \"match\" 10000 times"
    );
    assert!(editor.undo(), "replace_all should record an undo step");
    measure("10000 matches replace_all + undo", 50, || {
        assert_eq!(
            editor
                .replace_all(
                    hint::black_box("match"),
                    hint::black_box("replacement"),
                    SearchOptions::default()
                )
                .expect(NO_COMPOSITION),
            10_000,
            "undo should restore every match for the next iteration"
        );
        assert!(editor.undo(), "replace_all should record an undo step");
        hint::black_box(editor.document().revision());
    });

    let mut editor = Editor::new(styled_repetitions("match café 👩\u{200d}💻 ", 10_000));
    assert_eq!(
        editor
            .replace_all("match", "replacement", SearchOptions::default())
            .expect(NO_COMPOSITION),
        10_000,
        "each of the 10000 styled repetitions starts with \"match\""
    );
    assert!(editor.undo(), "replace_all should record an undo step");
    measure(
        "10000 styled Unicode matches replace_all + undo",
        50,
        || {
            assert_eq!(
                editor
                    .replace_all(
                        hint::black_box("match"),
                        hint::black_box("replacement"),
                        SearchOptions::default()
                    )
                    .expect(NO_COMPOSITION),
                10_000,
                "undo should restore every match for the next iteration"
            );
            assert!(editor.undo(), "replace_all should record an undo step");
        },
    );
}

fn search_navigation() {
    let mut editor = Editor::from_text(&"MATCH match café\n".repeat(100_000));
    let options = SearchOptions {
        case_sensitive: false,
        whole_word: true,
    };
    measure("100000 paragraphs find_next from start", 100, || {
        editor
            .set_selection(Selection::default())
            .expect(VALID_POSITION);
        assert!(
            editor
                .find_next(hint::black_box("match"), options, SearchWrap::Around)
                .expect(NO_COMPOSITION),
            "every nonempty paragraph contains \"match\""
        );
        assert_eq!(
            editor.selection(),
            Selection::new(Position::new(0, 0), Position::new(0, 5)),
            "case is ignored, so the first match is the document's leading \"MATCH\""
        );
    });
    measure("100000 paragraphs find_previous near start", 100, || {
        editor
            .set_selection(Selection::caret(Position::new(1, 0)))
            .expect(VALID_POSITION);
        assert!(
            editor
                .find_previous(hint::black_box("match"), options, SearchWrap::Around)
                .expect(NO_COMPOSITION),
            "every nonempty paragraph contains \"match\""
        );
        assert_eq!(
            editor.selection(),
            Selection::new(Position::new(0, 6), Position::new(0, 11)),
            "the nearest match before paragraph 1 is the second word of paragraph 0"
        );
    });
    for (label, paragraph) in [("middle", 50_000), ("end", 99_999)] {
        measure(
            &format!("100000 paragraphs find_next near {label}"),
            1_000,
            || {
                editor
                    .set_selection(Selection::caret(Position::new(paragraph, 5)))
                    .expect(VALID_POSITION);
                assert!(
                    editor
                        .find_next(hint::black_box("match"), options, SearchWrap::Stop)
                        .expect(NO_COMPOSITION),
                    "paragraph {paragraph} has a match after its first word"
                );
                assert_eq!(
                    editor.selection(),
                    Selection::new(Position::new(paragraph, 6), Position::new(paragraph, 11)),
                    "the next match after paragraph {paragraph}'s first word is its second word"
                );
            },
        );
        measure(
            &format!("100000 paragraphs find_previous near {label}"),
            1_000,
            || {
                editor
                    .set_selection(Selection::caret(Position::new(paragraph, 6)))
                    .expect(VALID_POSITION);
                assert!(
                    editor
                        .find_previous(hint::black_box("match"), options, SearchWrap::Stop)
                        .expect(NO_COMPOSITION),
                    "paragraph {paragraph} has a match before its second word"
                );
                assert_eq!(
                    editor.selection(),
                    Selection::new(Position::new(paragraph, 0), Position::new(paragraph, 5)),
                    "the previous match before paragraph {paragraph}'s second word is its first word"
                );
            },
        );
    }
    measure(
        "100000 paragraphs find_next wrapped from end",
        1_000,
        || {
            editor
                .set_selection(Selection::caret(editor.document().end()))
                .expect("the document end is always a valid caret position");
            assert!(
                editor
                    .find_next(hint::black_box("match"), options, SearchWrap::Around)
                    .expect(NO_COMPOSITION),
                "the search wraps, and the document contains matches"
            );
            assert_eq!(
                editor.selection(),
                Selection::new(Position::new(0, 0), Position::new(0, 5)),
                "wrapping from the end should find the first match in the document"
            );
        },
    );
    measure(
        "100000 paragraphs find_previous wrapped from start",
        1_000,
        || {
            editor
                .set_selection(Selection::default())
                .expect(VALID_POSITION);
            assert!(
                editor
                    .find_previous(hint::black_box("match"), options, SearchWrap::Around)
                    .expect(NO_COMPOSITION),
                "the search wraps, and the document contains matches"
            );
            assert_eq!(
                editor.selection(),
                Selection::new(Position::new(99_999, 6), Position::new(99_999, 11)),
                "wrapping from the start should find the last match in the document"
            );
        },
    );
}

fn long_literal_navigation() {
    let phrase = "café match 👩\u{200d}💻 ";
    let mut editor = Editor::from_text(&phrase.repeat(50_000));
    let repetition = 45_000;
    let start = phrase.len() * repetition + "café ".len();
    let end = start + "match".len();
    editor
        .set_selection(Selection::caret(Position::new(0, start)))
        .expect(VALID_POSITION);
    measure(
        "long Unicode paragraph literal find_next near end",
        100,
        || {
            editor
                .set_selection(Selection::caret(Position::new(0, end)))
                .expect(VALID_POSITION);
            assert!(
                editor
                    .find_next(
                        hint::black_box("match"),
                        SearchOptions::default(),
                        SearchWrap::Stop
                    )
                    .expect(NO_COMPOSITION),
                "later repetitions of the phrase contain \"match\""
            );
            assert_eq!(
                editor.selection(),
                Selection::new(
                    Position::new(0, start + phrase.len()),
                    Position::new(0, end + phrase.len())
                ),
                "the next match should be in the following repetition of the phrase"
            );
        },
    );
    measure(
        "long Unicode paragraph literal find_previous near end",
        100,
        || {
            editor
                .set_selection(Selection::caret(Position::new(0, start)))
                .expect(VALID_POSITION);
            assert!(
                editor
                    .find_previous(
                        hint::black_box("match"),
                        SearchOptions::default(),
                        SearchWrap::Stop
                    )
                    .expect(NO_COMPOSITION),
                "earlier repetitions of the phrase contain \"match\""
            );
            assert_eq!(
                editor.selection(),
                Selection::new(
                    Position::new(0, start - phrase.len()),
                    Position::new(0, end - phrase.len())
                ),
                "the previous match should be in the preceding repetition of the phrase"
            );
        },
    );
}

fn long_whole_word_navigation() {
    let options = SearchOptions {
        case_sensitive: false,
        whole_word: true,
    };
    for (script, filler, target) in [
        ("Latin", "lorem", "match"),
        ("Cyrillic", "привет", "слово"),
        ("Greek", "καλημέρα", "λόγος"),
        ("space-separated CJK", "你好", "世界"),
    ] {
        let cycle = format!("{target} {}", format!("{filler} ").repeat(39));
        let repetitions = 1_100_000 / cycle.len();
        let mut editor = Editor::from_text(&cycle.repeat(repetitions));
        let query = target.to_uppercase();
        let start = cycle.len() * (repetitions / 2);
        let found = |start: usize| {
            Selection::new(
                Position::new(0, start),
                Position::new(0, start + target.len()),
            )
        };
        measure(
            &format!("long {script} paragraph lowercase whole-word find_next near middle"),
            100,
            || {
                editor
                    .set_selection(Selection::caret(Position::new(0, start + target.len())))
                    .expect(VALID_POSITION);
                assert!(
                    editor
                        .find_next(hint::black_box(&query), options, SearchWrap::Stop)
                        .expect(NO_COMPOSITION),
                    "every cycle starts with the {script} target word"
                );
                assert_eq!(
                    editor.selection(),
                    found(start + cycle.len()),
                    "the next {script} match should start the following cycle"
                );
            },
        );
        measure(
            &format!("long {script} paragraph lowercase whole-word find_previous near middle"),
            100,
            || {
                editor
                    .set_selection(Selection::caret(Position::new(0, start)))
                    .expect(VALID_POSITION);
                assert!(
                    editor
                        .find_previous(hint::black_box(&query), options, SearchWrap::Stop)
                        .expect(NO_COMPOSITION),
                    "every cycle starts with the {script} target word"
                );
                assert_eq!(
                    editor.selection(),
                    found(start - cycle.len()),
                    "the previous {script} match should start the preceding cycle"
                );
            },
        );
    }
}

fn sparse_replacement() {
    for repeats in [1, 40] {
        let paragraph = "untouched café 👩\u{200d}💻. ".repeat(repeats);
        let source = format!("match\n{}match", format!("{paragraph}\n").repeat(10_000));
        let mut editor = Editor::from_text(&source);
        assert_eq!(
            editor
                .replace_all("match", "replacement", SearchOptions::default())
                .expect(NO_COMPOSITION),
            2,
            "only the first and last paragraphs contain \"match\""
        );
        assert!(editor.undo(), "replace_all should record an undo step");
        measure(
            &format!(
                "sparse replace_all + undo, 10000 unchanged {}-byte paragraphs",
                paragraph.len()
            ),
            50,
            || {
                assert_eq!(
                    editor
                        .replace_all(
                            "match",
                            hint::black_box("replacement"),
                            SearchOptions::default()
                        )
                        .expect(NO_COMPOSITION),
                    2,
                    "undo should restore both matches for the next iteration"
                );
                assert!(editor.undo(), "replace_all should record an undo step");
            },
        );
        assert_eq!(
            editor.document().plain_text(),
            source,
            "every replacement should have been undone"
        );
    }
}

/// Returns `iterations`, or a tenth of it (at least two) when the
/// `TEXTLOOM_BENCH_QUICK` environment variable is set to a nonempty value.
fn scaled(iterations: usize) -> usize {
    let quick = env::var_os("TEXTLOOM_BENCH_QUICK").is_some_and(|value| !value.is_empty());
    if quick {
        (iterations / 10).max(2)
    } else {
        iterations
    }
}

/// Runs `operation` a [`scaled`] number of times and prints the mean time per
/// call in microseconds under `label`.
fn measure(label: &str, iterations: usize, mut operation: impl FnMut()) {
    let iterations = scaled(iterations);
    let start = Instant::now();
    for _ in 0..iterations {
        operation();
    }
    println!(
        "{label}: {:.2} µs / operation ({iterations} iterations)",
        start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64
    );
}

#[cfg(feature = "accesskit")]
fn accessibility_navigation() {
    use textloom::accessibility::AccessKitAdapter;

    const TREE_UPDATE: &str =
        "the editor's selection is valid, and a benchmark-sized tree cannot exhaust node IDs";

    for paragraphs in [100, 10_000] {
        let mut editor = Editor::from_text(&"aé👩\u{200d}💻\n".repeat(paragraphs));
        let mut adapter = AccessKitAdapter::default();
        adapter
            .update(&editor, "Notes", Focus::Focused)
            .expect(TREE_UPDATE);
        let position = Position::new(paragraphs - 1, 3);
        let text_position = adapter
            .text_position(&editor, position)
            .expect("the adapter is current, and the position follows \"aé\"");
        measure(
            &format!("{paragraphs} paragraphs AccessKit position roundtrip"),
            2_000,
            || {
                let encoded = adapter
                    .text_position(&editor, hint::black_box(position))
                    .expect("the adapter is current, and the position follows \"aé\"");
                assert_eq!(
                    encoded, text_position,
                    "converting the same position should give the same result"
                );
                assert_eq!(
                    adapter
                        .editor_position(&editor, hint::black_box(encoded))
                        .expect("the text position came from this adapter and document"),
                    position,
                    "converting to an AccessKit position and back should be lossless"
                );
            },
        );
        measure(
            &format!("{paragraphs} paragraphs AccessKit selection update"),
            200,
            || {
                editor
                    .move_cursor(Movement::GraphemeForward, SelectionMode::Move)
                    .expect(NO_COMPOSITION);
                hint::black_box(
                    adapter
                        .update(&editor, "Notes", Focus::Focused)
                        .expect(TREE_UPDATE),
                );
                editor
                    .move_cursor(Movement::GraphemeBackward, SelectionMode::Move)
                    .expect(NO_COMPOSITION);
                hint::black_box(
                    adapter
                        .update(&editor, "Notes", Focus::Focused)
                        .expect(TREE_UPDATE),
                );
            },
        );
        measure(
            &format!("{paragraphs} paragraphs AccessKit local edit + update"),
            200,
            || {
                editor
                    .insert_text(hint::black_box("x"))
                    .expect(NO_COMPOSITION);
                hint::black_box(
                    adapter
                        .update(&editor, "Notes", Focus::Focused)
                        .expect(TREE_UPDATE),
                );
                editor.break_history_group();
                assert!(editor.undo(), "the insertion should be on the undo stack");
                hint::black_box(
                    adapter
                        .update(&editor, "Notes", Focus::Focused)
                        .expect(TREE_UPDATE),
                );
            },
        );
    }
}
