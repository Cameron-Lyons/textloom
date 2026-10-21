use std::{
    hint::black_box,
    time::{Duration, Instant},
};
use textloom::{
    Document, Editor, Fragment, HistoryLimits, Link, Movement, ParagraphKind, Position,
    SearchOptions, Selection, StylePatch,
};

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
                            editor.transaction(|editor| editor.insert_text(black_box("x")))
                        } else {
                            editor.insert_text(black_box("x"))
                        }
                    })
                    .unwrap();
                assert!(editor.undo());
            },
        );
        assert_eq!(editor.document().plain_text(), text);
    }
}

fn fragment_capture() {
    for paragraphs in [100, 100_000] {
        let source = "A short paragraph with Unicode café 👩‍💻.\n".repeat(paragraphs);
        let document = Document::from_text(&source);
        let range = Position::new(paragraphs - 1, 2)..Position::new(paragraphs - 1, 7);
        assert_eq!(
            document.fragment(range.clone()).unwrap().plain_text(),
            "short"
        );
        measure(
            &format!("{paragraphs} paragraphs short fragment capture near document end"),
            2_000,
            || {
                black_box(document.fragment(black_box(range.clone())).unwrap());
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
            .unwrap();
        editor
            .set_selection(Selection::caret(Position::default()))
            .unwrap();
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
            editor.set_paragraph_kind(first).unwrap();
            let expected = Some((paragraphs - 1) as u32);
            assert_eq!(editor.document().list_number(paragraphs - 1), expected);
            let mut kind = second;
            measure(
                &format!("{paragraphs} paragraphs {label} edit and cached list number"),
                1_000,
                || {
                    editor.set_paragraph_kind(kind).unwrap();
                    assert_eq!(
                        black_box(editor.document()).list_number(paragraphs - 1),
                        expected
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
            .unwrap();
        editor.insert_text("x").unwrap();
        assert!(editor.is_modified());
        measure(
            &format!("{paragraphs} paragraphs repeated dirty-state check"),
            20_000,
            || {
                black_box(black_box(&editor).is_modified());
            },
        );
        assert!(editor.undo());
        assert!(!editor.is_modified());
        measure(
            &format!("{paragraphs} paragraphs repeated restored-state check"),
            20_000,
            || {
                black_box(black_box(&editor).is_modified());
            },
        );
    }
}

fn paragraph_import() {
    let source = "A short paragraph with Unicode café 👩‍💻.\n".repeat(100_000);
    let bytes = Document::from_text(&source).to_bytes();
    measure("100000 paragraph fragment import", 20, || {
        let fragment = Fragment::from_text(black_box(&source));
        assert_eq!(fragment.paragraphs().len(), 100_001);
        black_box(fragment);
    });
    measure("100000 paragraph snapshot import", 20, || {
        let document = Document::from_bytes(black_box(&bytes)).unwrap();
        assert_eq!(document.paragraphs().len(), 100_001);
        assert_eq!(document.revision(), 0);
        black_box(document);
    });
    let source = "café 👩‍💻. ".repeat(100_000);
    let bytes = Document::from_text(&source).to_bytes();
    measure("long Unicode paragraph snapshot import", 50, || {
        let document = Document::from_bytes(black_box(&bytes)).unwrap();
        assert_eq!(document.paragraphs().len(), 1);
        assert_eq!(document.paragraphs()[0].text().len(), source.len());
        black_box(document);
    });
}

fn serialization() {
    let source = "A short paragraph with Unicode café 👩‍💻.\n".repeat(100_000);
    let document = Document::from_text(&source);
    let expected_bytes = document.to_bytes();
    let expected_html = document.to_html();
    println!(
        "100000 paragraphs HTML output: {} bytes, {} capacity",
        expected_html.len(),
        expected_html.capacity()
    );
    measure("100000 paragraphs native snapshot export", 20, || {
        let bytes = black_box(&document).to_bytes();
        assert_eq!(bytes.len(), expected_bytes.len());
        black_box(bytes);
    });
    measure("100000 paragraphs HTML export", 20, || {
        let html = black_box(&document).to_html();
        assert_eq!(html.len(), expected_html.len());
        black_box(html);
    });

    let document = styled_repetitions("match café 👩‍💻 ", 10_000);
    let expected_bytes = document.to_bytes();
    measure("10000 styled spans native snapshot export", 100, || {
        let bytes = black_box(&document).to_bytes();
        assert_eq!(bytes.len(), expected_bytes.len());
        black_box(bytes);
    });
    assert_eq!(Document::from_bytes(&expected_bytes).unwrap(), document);
}

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
    Document::from_bytes(&bytes).unwrap()
}

fn caret_link_queries() {
    for repetitions in [100, 100_000] {
        let mut editor = Editor::new(styled_repetitions("é", repetitions));
        let target = Link::new("https://example.com").unwrap();
        editor.select_all();
        editor.set_link(Some(target.clone())).unwrap();
        editor.clear_history();
        for (label, byte) in [
            ("start", 0),
            ("middle", repetitions),
            ("end", repetitions * 2),
        ] {
            editor
                .set_selection(Selection::caret(Position::new(0, byte)))
                .unwrap();
            assert_eq!(editor.selected_link(), Some(target.clone()));
            measure(
                &format!("{repetitions} linked styled spans caret target at {label}"),
                20_000,
                || {
                    black_box(black_box(&editor).selected_link());
                },
            );
        }
    }
}

fn formatting() {
    let phrase = "match café 👩‍💻 ";
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
        editor.set_selection(local).unwrap();
        measure(
            &format!("{repetitions} styled spans local unchanged formatting"),
            5_000,
            || editor.apply_style(black_box(unchanged)).unwrap(),
        );
        assert_eq!(editor.undo_len(), 0);
        assert_eq!(editor.document().to_bytes(), original);
        measure(
            &format!("{repetitions} styled spans local formatting + undo"),
            1_000,
            || {
                editor.apply_style(black_box(changed)).unwrap();
                assert!(editor.undo());
                assert_eq!(editor.selection(), local);
            },
        );
        assert_eq!(editor.document().to_bytes(), original);

        editor.select_all();
        measure(
            &format!("{repetitions} styled spans full unchanged formatting"),
            500,
            || editor.apply_style(black_box(unchanged)).unwrap(),
        );
        measure(
            &format!("{repetitions} styled spans full formatting + undo"),
            100,
            || {
                editor.apply_style(black_box(changed)).unwrap();
                assert!(editor.undo());
            },
        );
        measure(
            &format!("{repetitions} styled spans clear formatting + undo"),
            100,
            || {
                editor.clear_formatting().unwrap();
                assert!(editor.undo());
            },
        );
        assert_eq!(editor.document().to_bytes(), original);
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
            || editor.insert_text(black_box("")).unwrap(),
        );
        measure(
            &format!("{repetitions} styled spans empty fragment insertion"),
            1_000,
            || editor.insert_fragment(black_box(&fragment)).unwrap(),
        );
        measure(
            &format!("{repetitions} styled spans empty composition commit"),
            1_000,
            || {
                editor.begin_composition();
                editor.commit_composition(black_box("")).unwrap();
            },
        );
        assert!(editor.composition().is_none());
        assert_eq!(editor.undo_len(), 0);
        assert_eq!(editor.document().to_bytes(), original);
    }
}

fn cold_zero_index_navigation() {
    for repetitions in [100, 100_000] {
        let source = format!("x\n{}", "é👩‍💻e\u{301}".repeat(repetitions));
        for (operation, label) in [
            "grapheme zero conversion",
            "scalar zero conversion",
            "paragraph-down at column zero",
        ]
        .into_iter()
        .enumerate()
        {
            let iterations = scaled(30);
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                // Each operation gets an unindexed paragraph; construction is not timed.
                let mut editor = Editor::from_text(&source);
                let start = Instant::now();
                match operation {
                    0 => assert_eq!(
                        black_box(editor.document().paragraph(1).unwrap())
                            .byte_from_grapheme(black_box(0)),
                        Some(0)
                    ),
                    1 => assert_eq!(
                        black_box(editor.document().paragraph(1).unwrap())
                            .byte_from_scalar(black_box(0)),
                        Some(0)
                    ),
                    _ => editor
                        .move_cursor(black_box(Movement::ParagraphDown), false)
                        .unwrap(),
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
    let tail = "other café 👩‍💻. ".repeat(50_000);
    let source = format!("match {tail}");
    let document = Document::from_text(&source);
    let options = SearchOptions {
        whole_word: true,
        ..SearchOptions::default()
    };
    let expected = Position::new(0, 0)..Position::new(0, 5);
    assert_eq!(
        document.find("match", options).as_slice(),
        std::slice::from_ref(&expected)
    );
    measure("long Unicode paragraph sparse whole-word find", 50, || {
        assert_eq!(
            black_box(&document)
                .find(black_box("match"), options)
                .as_slice(),
            std::slice::from_ref(&expected)
        );
    });
    let mut editor = Editor::new(document);
    measure(
        "long Unicode paragraph whole-word find_next from start",
        100,
        || {
            editor.set_selection(Selection::default()).unwrap();
            assert!(
                editor
                    .find_next(black_box("match"), options, false)
                    .unwrap()
            );
            assert_eq!(
                editor.selection(),
                Selection::new(expected.start, expected.end)
            );
        },
    );
    for query in ["match", "abcaX", "aa"] {
        let document = Document::from_text(&format!("{query}X {tail}"));
        assert!(document.find(query, options).is_empty());
        measure(
            &format!("long Unicode paragraph rejected whole-word {query:?}"),
            50,
            || assert!(document.find(black_box(query), options).is_empty()),
        );
    }
}

fn short_paragraph_edits() {
    for paragraphs in [100, 10_000, 100_000] {
        let source = "A short paragraph with Unicode café 👩‍💻.\n".repeat(paragraphs);
        let mut editor = Editor::from_text(&source);
        editor
            .set_selection(Selection::caret(Position::new(paragraphs / 2, 5)))
            .unwrap();
        let iterations = scaled(2_000);
        let start = Instant::now();
        for _ in 0..iterations {
            editor.insert_text(black_box("x")).unwrap();
            editor.break_history_group();
            assert!(editor.undo());
        }
        println!(
            "{paragraphs:>6} paragraphs: {:.2} µs / insert + undo",
            start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64
        );
    }
}

fn unicode_navigation() {
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
    editor.set_selection(Selection::caret(position)).unwrap();
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

fn grapheme_pair(editor: &mut Editor) {
    let original = editor.selection();
    editor
        .move_cursor(Movement::GraphemeBackward, false)
        .unwrap();
    editor
        .move_cursor(Movement::GraphemeForward, false)
        .unwrap();
    assert_eq!(editor.selection(), original);
}

fn word_pair(editor: &mut Editor) {
    editor.move_cursor(Movement::WordBackward, false).unwrap();
    editor.move_cursor(Movement::WordForward, false).unwrap();
    black_box(editor.selection());
}

fn long_unicode_edits() {
    let phrase = "café 👩‍💻 नमस्ते 🇺🇸 ";
    let source = phrase.repeat(10_000);
    let original = Selection::caret(Position::new(0, phrase.len() * 5_000));
    let mut editor = Editor::from_text(&source);
    editor.set_selection(original).unwrap();
    measure("long Unicode paragraph middle insert + undo", 200, || {
        editor.insert_text(black_box("x")).unwrap();
        editor.break_history_group();
        assert!(editor.undo());
        assert_eq!(editor.selection(), original);
    });
    assert_eq!(editor.document().plain_text(), source);

    for backward in [true, false] {
        let mut editor = Editor::from_text(&source);
        editor.set_selection(original).unwrap();
        measure(
            if backward {
                "long Unicode paragraph insert + backspace + undo"
            } else {
                "long Unicode paragraph insert + delete + undo"
            },
            200,
            || {
                editor.insert_text(black_box("x")).unwrap();
                if backward {
                    editor.delete_backward().unwrap();
                } else {
                    editor.delete_forward().unwrap();
                }
                assert!(editor.undo());
                assert!(editor.undo());
                assert_eq!(editor.selection(), original);
            },
        );
        assert_eq!(editor.document().plain_text(), source);
    }

    let source = "🇺🇸".repeat(10_000);
    let original = Selection::caret(Position::new(0, source.len() / 2));
    let mut editor = Editor::from_text(&source);
    editor.set_selection(original).unwrap();
    measure("long regional run middle insert + undo", 100, || {
        editor.insert_text(black_box("🇨🇦")).unwrap();
        editor.break_history_group();
        assert!(editor.undo());
        assert_eq!(editor.selection(), original);
    });
    assert_eq!(editor.document().plain_text(), source);
}

fn contextual_unicode_search() {
    let document = Document::from_text(&"🇺🇸".repeat(10_000));
    assert_eq!(document.find("🇺🇸", SearchOptions::default()).len(), 10_000);
    measure("10000 regional flag literal matches", 50, || {
        let matches = document.find(black_box("🇺🇸"), SearchOptions::default());
        assert_eq!(matches.len(), 10_000);
        black_box(matches);
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
            assert!(document.find(&query, SearchOptions::default()).is_empty());
            measure(
                &format!("{count} {kind} partial-grapheme literal miss"),
                50,
                || {
                    assert!(
                        document
                            .find(black_box(&query), SearchOptions::default())
                            .is_empty()
                    );
                },
            );
        }
    }
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

    let mut editor = Editor::new(styled_repetitions("match café 👩‍💻 ", 10_000));
    assert_eq!(
        editor
            .replace_all("match", "replacement", SearchOptions::default())
            .unwrap(),
        10_000
    );
    assert!(editor.undo());
    measure(
        "10000 styled Unicode matches replace_all + undo",
        50,
        || {
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

fn long_literal_navigation() {
    let phrase = "café match 👩‍💻 ";
    let mut editor = Editor::from_text(&phrase.repeat(50_000));
    let repetition = 45_000;
    let start = phrase.len() * repetition + "café ".len();
    let end = start + "match".len();
    editor
        .set_selection(Selection::caret(Position::new(0, start)))
        .unwrap();
    measure(
        "long Unicode paragraph literal find_next near end",
        100,
        || {
            editor
                .set_selection(Selection::caret(Position::new(0, end)))
                .unwrap();
            assert!(
                editor
                    .find_next(black_box("match"), SearchOptions::default(), false)
                    .unwrap()
            );
            assert_eq!(
                editor.selection(),
                Selection::new(
                    Position::new(0, start + phrase.len()),
                    Position::new(0, end + phrase.len())
                )
            );
        },
    );
    measure(
        "long Unicode paragraph literal find_previous near end",
        100,
        || {
            editor
                .set_selection(Selection::caret(Position::new(0, start)))
                .unwrap();
            assert!(
                editor
                    .find_previous(black_box("match"), SearchOptions::default(), false)
                    .unwrap()
            );
            assert_eq!(
                editor.selection(),
                Selection::new(
                    Position::new(0, start - phrase.len()),
                    Position::new(0, end - phrase.len())
                )
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
                    .unwrap();
                assert!(editor.find_next(black_box(&query), options, false).unwrap());
                assert_eq!(editor.selection(), found(start + cycle.len()));
            },
        );
        measure(
            &format!("long {script} paragraph lowercase whole-word find_previous near middle"),
            100,
            || {
                editor
                    .set_selection(Selection::caret(Position::new(0, start)))
                    .unwrap();
                assert!(
                    editor
                        .find_previous(black_box(&query), options, false)
                        .unwrap()
                );
                assert_eq!(editor.selection(), found(start - cycle.len()));
            },
        );
    }
}

fn sparse_replacement() {
    for repeats in [1, 40] {
        let paragraph = "untouched café 👩‍💻. ".repeat(repeats);
        let source = format!("match\n{}match", format!("{paragraph}\n").repeat(10_000));
        let mut editor = Editor::from_text(&source);
        editor.set_history_limits(HistoryLimits {
            max_bytes: 64 * 1024 * 1024,
            ..HistoryLimits::default()
        });
        assert_eq!(
            editor
                .replace_all("match", "replacement", SearchOptions::default())
                .unwrap(),
            2
        );
        assert!(editor.undo());
        measure(
            &format!(
                "sparse replace_all + undo, 10000 unchanged {}-byte paragraphs",
                paragraph.len()
            ),
            50,
            || {
                assert_eq!(
                    editor
                        .replace_all("match", black_box("replacement"), SearchOptions::default())
                        .unwrap(),
                    2
                );
                assert!(editor.undo());
            },
        );
        assert_eq!(editor.document().plain_text(), source);
    }
}

fn scaled(iterations: usize) -> usize {
    let quick = std::env::var_os("TEXTLOOM_BENCH_QUICK").is_some_and(|value| !value.is_empty());
    if quick {
        (iterations / 10).max(2)
    } else {
        iterations
    }
}

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
        measure(
            &format!("{paragraphs} paragraphs AccessKit local edit + update"),
            200,
            || {
                editor.insert_text(black_box("x")).unwrap();
                black_box(adapter.update(&editor, "Notes", true).unwrap());
                editor.break_history_group();
                assert!(editor.undo());
                black_box(adapter.update(&editor, "Notes", true).unwrap());
            },
        );
    }
}
