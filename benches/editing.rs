//! Standalone benchmarks for localized editing, Unicode navigation, and replacement.

use std::{hint::black_box, time::Instant};
use textloom::{Document, Editor, Movement, Position, SearchOptions, Selection};

fn main() {
    short_paragraph_edits();
    unicode_navigation();
    ascii_navigation();
    literal_search();
    search_navigation();
    batch_replacement();
    #[cfg(feature = "accesskit")]
    accessibility_navigation();
}

fn short_paragraph_edits() {
    for paragraphs in [100, 10_000, 100_000] {
        let source = "A short paragraph with Unicode café 👩‍💻.\n".repeat(paragraphs);
        let mut editor = Editor::from_text(&source);
        editor
            .set_selection(Selection::caret(Position::new(paragraphs / 2, 5)))
            .unwrap();
        let start = Instant::now();
        for _ in 0..2_000 {
            editor.insert_text(black_box("x")).unwrap();
            editor.break_history_group();
            assert!(editor.undo());
        }
        println!(
            "{paragraphs:>6} paragraphs: {:.2} µs / insert + undo",
            start.elapsed().as_secs_f64() * 1_000_000.0 / 2_000.0
        );
    }
}

fn unicode_navigation() {
    // This fixed phrase is 46 UTF-8 bytes. The caret is 90% through a
    // 460000-byte paragraph, where a repeated linear scan is expensive.
    let phrase = "café 👩‍💻 नमस्ते 🇺🇸 ";
    let source = phrase.repeat(10_000);
    let position = Position::new(0, phrase.len() * 9_000);
    println!("long Unicode paragraph: {} bytes", source.len());
    let document = Document::from_text(&source);
    let start = Instant::now();
    document.validate_position(black_box(position)).unwrap();
    println!(
        "validation cold: {:.2} µs / first query",
        start.elapsed().as_secs_f64() * 1_000_000.0
    );
    measure("validation warm", 2_000, || {
        black_box(document.validate_position(black_box(position))).unwrap();
    });

    let mut editor = Editor::from_text(&source);
    // Explicit selection warms grapheme validation separately from words.
    editor.set_selection(Selection::caret(position)).unwrap();
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
    editor.set_selection(Selection::caret(position)).unwrap();
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

fn word_pair(editor: &mut Editor) {
    editor.move_cursor(Movement::WordBackward, false).unwrap();
    editor.move_cursor(Movement::WordForward, false).unwrap();
    black_box(editor.selection());
}

fn paragraph_pair(editor: &mut Editor) {
    editor.move_cursor(Movement::ParagraphDown, false).unwrap();
    editor.move_cursor(Movement::ParagraphUp, false).unwrap();
    black_box(editor.selection());
}

fn ascii_navigation() {
    let phrase = "A short paragraph with ASCII words. ";
    let source = phrase.repeat(10_000);
    let mut editor = Editor::from_text(&source);
    editor
        .set_selection(Selection::caret(Position::new(0, phrase.len() * 9_000)))
        .unwrap();
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
        let matches = document.find(black_box(&needle), SearchOptions::default());
        assert_eq!(matches.len(), 1_000);
        black_box(matches);
    });
    measure("long paragraph literal miss", 50, || {
        assert!(
            document
                .find(black_box("absent"), SearchOptions::default())
                .is_empty()
        );
    });
    let document =
        Document::from_text(&"A short paragraph with Unicode café 👩‍💻.\n".repeat(100_000));
    measure("100000 paragraph literal matches", 20, || {
        let matches = document.find(black_box("Unicode"), SearchOptions::default());
        assert_eq!(matches.len(), 100_000);
        black_box(matches);
    });
    measure("100000 paragraph whole-word matches", 10, || {
        let matches = document.find(
            black_box("Unicode"),
            SearchOptions {
                case_sensitive: true,
                whole_word: true,
            },
        );
        assert_eq!(matches.len(), 100_000);
        black_box(matches);
    });
    measure("100000 paragraph lowercase whole-word matches", 10, || {
        let matches = document.find(
            black_box("unicode"),
            SearchOptions {
                case_sensitive: false,
                whole_word: true,
            },
        );
        assert_eq!(matches.len(), 100_000);
        black_box(matches);
    });
    measure("100000 paragraph literal miss", 20, || {
        assert!(
            document
                .find(black_box("absent"), SearchOptions::default())
                .is_empty()
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
                            black_box("absent"),
                            SearchOptions {
                                case_sensitive,
                                whole_word: true,
                            },
                        )
                        .is_empty()
                );
            },
        );
    }
    measure("100000 paragraphs shorter than query", 20, || {
        assert!(
            document
                .find(black_box(&needle), SearchOptions::default())
                .is_empty()
        );
    });
    measure("100000 paragraphs shorter than lowercase query", 20, || {
        assert!(
            document
                .find(
                    black_box(&needle),
                    SearchOptions {
                        case_sensitive: false,
                        whole_word: true
                    }
                )
                .is_empty()
        );
    });
}

fn batch_replacement() {
    let mut editor = Editor::from_text(&"match ".repeat(10_000));
    assert_eq!(
        editor
            .replace_all("match", "replacement", SearchOptions::default())
            .unwrap(),
        10_000
    );
    assert!(editor.undo());
    measure("10000 matches replace_all + undo", 50, || {
        assert_eq!(
            editor
                .replace_all(
                    black_box("match"),
                    black_box("replacement"),
                    SearchOptions::default()
                )
                .unwrap(),
            10_000
        );
        assert!(editor.undo());
        black_box(editor.document().revision());
    });
}

fn search_navigation() {
    let mut editor = Editor::from_text(&"MATCH match café\n".repeat(100_000));
    let options = SearchOptions {
        case_sensitive: false,
        whole_word: true,
    };
    measure("100000 paragraphs find_next from start", 100, || {
        editor.set_selection(Selection::default()).unwrap();
        assert!(editor.find_next(black_box("match"), options, true).unwrap());
        assert_eq!(
            editor.selection(),
            Selection::new(Position::new(0, 0), Position::new(0, 5))
        );
    });
    measure("100000 paragraphs find_previous near start", 100, || {
        editor
            .set_selection(Selection::caret(Position::new(1, 0)))
            .unwrap();
        assert!(
            editor
                .find_previous(black_box("match"), options, true)
                .unwrap()
        );
        assert_eq!(
            editor.selection(),
            Selection::new(Position::new(0, 6), Position::new(0, 11))
        );
    });
    for (label, paragraph) in [("middle", 50_000), ("end", 99_999)] {
        measure(
            &format!("100000 paragraphs find_next near {label}"),
            1_000,
            || {
                editor
                    .set_selection(Selection::caret(Position::new(paragraph, 5)))
                    .unwrap();
                assert!(
                    editor
                        .find_next(black_box("match"), options, false)
                        .unwrap()
                );
                assert_eq!(
                    editor.selection(),
                    Selection::new(Position::new(paragraph, 6), Position::new(paragraph, 11))
                );
            },
        );
        measure(
            &format!("100000 paragraphs find_previous near {label}"),
            1_000,
            || {
                editor
                    .set_selection(Selection::caret(Position::new(paragraph, 6)))
                    .unwrap();
                assert!(
                    editor
                        .find_previous(black_box("match"), options, false)
                        .unwrap()
                );
                assert_eq!(
                    editor.selection(),
                    Selection::new(Position::new(paragraph, 0), Position::new(paragraph, 5))
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
                .unwrap();
            assert!(editor.find_next(black_box("match"), options, true).unwrap());
            assert_eq!(
                editor.selection(),
                Selection::new(Position::new(0, 0), Position::new(0, 5))
            );
        },
    );
    measure(
        "100000 paragraphs find_previous wrapped from start",
        1_000,
        || {
            editor.set_selection(Selection::default()).unwrap();
            assert!(
                editor
                    .find_previous(black_box("match"), options, true)
                    .unwrap()
            );
            assert_eq!(
                editor.selection(),
                Selection::new(Position::new(99_999, 6), Position::new(99_999, 11))
            );
        },
    );
}

fn measure(label: &str, iterations: usize, mut operation: impl FnMut()) {
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

    for paragraphs in [100, 10_000] {
        let mut editor = Editor::from_text(&"aé👩‍💻\n".repeat(paragraphs));
        let mut adapter = AccessKitAdapter::default();
        adapter.update(&editor, "Notes", true).unwrap();
        let position = Position::new(paragraphs - 1, 3);
        let text_position = adapter.to_text_position(&editor, position).unwrap();
        measure(
            &format!("{paragraphs} paragraphs AccessKit position roundtrip"),
            2_000,
            || {
                let encoded = adapter
                    .to_text_position(&editor, black_box(position))
                    .unwrap();
                assert_eq!(encoded, text_position);
                assert_eq!(
                    adapter
                        .from_text_position(&editor, black_box(encoded))
                        .unwrap(),
                    position
                );
            },
        );
        measure(
            &format!("{paragraphs} paragraphs AccessKit selection update"),
            200,
            || {
                editor
                    .move_cursor(Movement::GraphemeForward, false)
                    .unwrap();
                black_box(adapter.update(&editor, "Notes", true).unwrap());
                editor
                    .move_cursor(Movement::GraphemeBackward, false)
                    .unwrap();
                black_box(adapter.update(&editor, "Notes", true).unwrap());
            },
        );
    }
}
