//! Literal and Unicode lowercase search coordinates and boundary contracts.

use textloom::{Document, Editor, Position, SearchOptions, Selection};
use unicode_segmentation::UnicodeSegmentation;

fn default_options() -> SearchOptions {
    SearchOptions::default()
}

#[test]
fn literal_matches_are_ordered_nonoverlapping_and_empty_queries_match_nothing() {
    let document = Document::from_text("banana ana\nana");
    assert_eq!(
        document.find("ana", default_options()),
        vec![
            Position::new(0, 1)..Position::new(0, 4),
            Position::new(0, 7)..Position::new(0, 10),
            Position::new(1, 0)..Position::new(1, 3),
        ]
    );
    assert!(document.find("", default_options()).is_empty());
    assert!(Document::new().find("a", default_options()).is_empty());
    assert!(document.find("BANANA", default_options()).is_empty());
}

#[test]
fn paragraph_local_literals_preserve_empty_paragraphs_and_unicode_boundaries() {
    let document = Document::from_text("\na\nba aa\n\ne\u{301} e\n🇨🇺🇺🇺\na\n");
    assert_eq!(
        document.find("a", default_options()),
        vec![
            Position::new(1, 0)..Position::new(1, 1),
            Position::new(2, 1)..Position::new(2, 2),
            Position::new(2, 3)..Position::new(2, 4),
            Position::new(2, 4)..Position::new(2, 5),
            Position::new(6, 0)..Position::new(6, 1),
        ]
    );
    assert_eq!(
        document.find("aa", default_options()),
        vec![Position::new(2, 3)..Position::new(2, 5)]
    );
    assert_eq!(
        document.find("e", default_options()),
        vec![Position::new(4, 4)..Position::new(4, 5)]
    );
    assert_eq!(
        document.find("🇺🇺", default_options()),
        vec![Position::new(5, 8)..Position::new(5, 16)]
    );
    assert!(document.find("aa aa aa", default_options()).is_empty());
}

#[test]
fn queries_normalize_newlines_and_preserve_empty_paragraph_coordinates() {
    let document = Document::from_text("first\n\nlast\n");
    assert_eq!(
        document.find("st\r\n\rla", default_options()),
        vec![Position::new(0, 3)..Position::new(2, 2)]
    );
    assert_eq!(
        document.find("\r\n", default_options()),
        vec![
            Position::new(0, 5)..Position::new(1, 0),
            Position::new(1, 0)..Position::new(2, 0),
            Position::new(2, 4)..Position::new(3, 0),
        ]
    );
}

#[test]
fn only_complete_graphemes_match() {
    let document = Document::from_text("e\u{301} e 👩‍💻 👩 🇺🇸 🇺");
    assert_eq!(
        document.find("e", default_options()),
        vec![Position::new(0, 4)..Position::new(0, 5)]
    );
    assert!(document.find("\u{301}", default_options()).is_empty());
    assert_eq!(document.find("e\u{301}", default_options()).len(), 1);
    let women = document.find("👩", default_options());
    assert_eq!(women.len(), 1);
    assert_eq!(document.text(women[0].clone()).unwrap(), "👩");
    let flags = document.find("🇺", default_options());
    assert_eq!(flags.len(), 1);
    assert_eq!(document.text(flags[0].clone()).unwrap(), "🇺");
    assert_eq!(document.find("👩‍💻", default_options()).len(), 1);
    // The first literal candidate overlaps two flags. Rejecting it must
    // still allow the later candidate that covers one complete flag.
    assert_eq!(
        Document::from_text("🇨🇺🇺🇺").find("🇺🇺", default_options()),
        vec![Position::new(0, 8)..Position::new(0, 16)]
    );
}

#[test]
fn insensitive_search_maps_original_bytes_and_rejects_partial_expansions() {
    let document = Document::from_text("İ i İ\nCAFÉ café ΟΣ");
    let options = SearchOptions {
        case_sensitive: false,
        whole_word: false,
    };
    assert_eq!(
        document.find("i", options),
        vec![Position::new(0, 3)..Position::new(0, 4)]
    );
    assert!(document.find("\u{307}", options).is_empty());
    assert_eq!(
        document.find("i\u{307}", options),
        vec![
            Position::new(0, 0)..Position::new(0, 2),
            Position::new(0, 5)..Position::new(0, 7),
        ]
    );
    let cafes = document.find("café", options);
    assert_eq!(cafes.len(), 2);
    assert_eq!(document.text(cafes[0].clone()).unwrap(), "CAFÉ");
    assert_eq!(document.text(cafes[1].clone()).unwrap(), "café");
    let greek = document.find("ος", options);
    assert_eq!(greek.len(), 1);
    assert_eq!(document.text(greek[0].clone()).unwrap(), "ΟΣ");
    assert!(
        Document::from_text("Straße")
            .find("STRASSE", options)
            .is_empty()
    );
    assert!(
        Document::from_text("é")
            .find("e\u{301}", options)
            .is_empty()
    );
}

#[test]
fn whole_words_use_unicode_word_boundaries() {
    let document = Document::from_text("cat cats bobcat cat's cat.\nCAT café cafés 你好世界");
    let options = SearchOptions {
        case_sensitive: false,
        whole_word: true,
    };
    let matches = document.find("cat", options);
    assert_eq!(matches.len(), 3);
    assert_eq!(document.text(matches[0].clone()).unwrap(), "cat");
    assert_eq!(document.text(matches[1].clone()).unwrap(), "cat");
    assert_eq!(document.text(matches[2].clone()).unwrap(), "CAT");
    assert_eq!(document.find("café", options).len(), 1);
    assert_eq!(document.find("你", options).len(), 1);
    assert!(document.find("af", options).is_empty());
}

#[test]
fn case_insensitive_cross_paragraph_queries_map_original_coordinates() {
    let document = Document::from_text("İ\nCAFÉ");
    let matches = document.find(
        "i\u{307}\rcafé",
        SearchOptions {
            case_sensitive: false,
            whole_word: true,
        },
    );
    assert_eq!(matches, vec![Position::new(0, 0)..Position::new(1, 5)]);
    assert_eq!(document.text(matches[0].clone()).unwrap(), "İ\nCAFÉ");
}

#[test]
fn lowercase_byte_width_changes_map_before_between_and_after_matches() {
    let document = Document::from_text("KȺİ Kⱥi\u{307}");
    let options = SearchOptions {
        case_sensitive: false,
        whole_word: false,
    };
    assert_eq!(
        document.find("k", options),
        vec![
            Position::new(0, 0)..Position::new(0, 3),
            Position::new(0, 8)..Position::new(0, 9)
        ]
    );
    assert_eq!(
        document.find("ⱥ", options),
        vec![
            Position::new(0, 3)..Position::new(0, 5),
            Position::new(0, 9)..Position::new(0, 12)
        ]
    );
    assert_eq!(
        document.find("i\u{307}", options),
        vec![
            Position::new(0, 5)..Position::new(0, 7),
            Position::new(0, 12)..Position::new(0, 15)
        ]
    );
    assert!(document.find("i", options).is_empty());
}

#[test]
fn overlapping_retries_preserve_earlier_and_later_matches() {
    for source in ["🇺🇺 🇨🇺🇺🇺 🇺🇺", "🇺🇺\n🇨🇺🇺🇺\n🇺🇺"] {
        let document = Document::from_text(source);
        let matches = document.find("🇺🇺", SearchOptions::default());
        assert_eq!(matches.len(), 3);
        for range in &matches {
            assert_eq!(document.text(range.clone()).unwrap(), "🇺🇺");
        }
        assert!(matches.windows(2).all(|pair| pair[0].end <= pair[1].start));
    }

    let document = Document::from_text("i İ i\ni İ i");
    let matches = document.find(
        "i",
        SearchOptions {
            case_sensitive: false,
            whole_word: false,
        },
    );
    assert_eq!(matches.len(), 4);
    for range in matches {
        assert_eq!(document.text(range).unwrap(), "i");
    }

    assert_eq!(
        Document::from_text("a-a aa-a-a a-a").find(
            "a-a",
            SearchOptions {
                whole_word: true,
                ..Default::default()
            }
        ),
        vec![
            Position::new(0, 0)..Position::new(0, 3),
            Position::new(0, 7)..Position::new(0, 10),
            Position::new(0, 11)..Position::new(0, 14),
        ]
    );
}

#[test]
fn paragraph_local_options_preserve_case_context_and_word_boundaries() {
    let document = Document::from_text("ΟΣ\nΟΣΑ\nİ\ni\ncat's\ncat\n你好\n\u{0600}word\nword");
    let insensitive = SearchOptions {
        case_sensitive: false,
        whole_word: false,
    };
    assert_eq!(
        document.find("ος", insensitive),
        vec![Position::new(0, 0)..Position::new(0, 4)]
    );
    assert_eq!(
        document.find("οσ", insensitive),
        vec![Position::new(1, 0)..Position::new(1, 4)]
    );
    assert_eq!(
        document.find("i", insensitive),
        vec![Position::new(3, 0)..Position::new(3, 1)]
    );
    for case_sensitive in [false, true] {
        let options = SearchOptions {
            case_sensitive,
            whole_word: true,
        };
        assert_eq!(
            document.find("cat", options),
            vec![Position::new(5, 0)..Position::new(5, 3)]
        );
        assert_eq!(
            document.find("你", options),
            vec![Position::new(6, 0)..Position::new(6, 3)]
        );
        assert_eq!(
            document.find("word", options),
            vec![Position::new(8, 0)..Position::new(8, 4)]
        );
    }
}

#[test]
fn longer_lowercase_queries_match_expansions_but_skip_short_paragraphs() {
    let document = Document::from_text("\nİ\ni\nK\nK\n");
    for whole_word in [false, true] {
        let options = SearchOptions {
            case_sensitive: false,
            whole_word,
        };
        assert_eq!(
            document.find("i\u{307}", options),
            vec![Position::new(1, 0)..Position::new(1, 2)]
        );
        assert_eq!(
            document.find("K", options),
            vec![
                Position::new(3, 0)..Position::new(3, 3),
                Position::new(4, 0)..Position::new(4, 1)
            ]
        );
        assert!(document.find(&"absent".repeat(100), options).is_empty());
    }
}

#[test]
fn generated_unicode_matches_agree_with_a_grapheme_reference() {
    // The reference tries every complete original grapheme, rather than using
    // literal candidates or the implementation's sparse lowercase byte map.
    fn reference(
        document: &Document,
        query: &str,
        options: SearchOptions,
    ) -> Vec<std::ops::Range<Position>> {
        let source = document.plain_text();
        let needle = query.replace("\r\n", "\n").replace('\r', "\n");
        let needle = if options.case_sensitive {
            needle
        } else {
            needle.to_lowercase()
        };
        if needle.is_empty() {
            return Vec::new();
        }
        let searched = if options.case_sensitive {
            source.clone()
        } else {
            source.to_lowercase()
        };
        let coordinates: Vec<_> = source
            .grapheme_indices(true)
            .map(|(byte, _)| byte)
            .chain([source.len()])
            .map(|byte| {
                let lowered = if options.case_sensitive {
                    byte
                } else {
                    source[..byte].to_lowercase().len()
                };
                (lowered, byte)
            })
            .collect();
        let word_boundaries: Vec<_> = source
            .split_word_bound_indices()
            .map(|(byte, _)| byte)
            .chain([source.len()])
            .collect();
        let position = |byte| {
            let prefix = &source[..byte];
            Position::new(
                prefix.bytes().filter(|&byte| byte == b'\n').count(),
                prefix
                    .rfind('\n')
                    .map_or(byte, |newline| byte - newline - 1),
            )
        };
        let mut matches = Vec::new();
        let mut next = 0;
        for &(lowered, start) in &coordinates {
            if start < next || !searched[lowered..].starts_with(&needle) {
                continue;
            }
            let Ok(end_index) = coordinates
                .binary_search_by_key(&(lowered + needle.len()), |&(lowered, _)| lowered)
            else {
                continue;
            };
            let end = coordinates[end_index].1;
            if options.whole_word
                && (word_boundaries.binary_search(&start).is_err()
                    || word_boundaries.binary_search(&end).is_err())
            {
                continue;
            }
            matches.push(position(start)..position(end));
            next = end;
        }
        matches
    }

    let tokens = [
        "a",
        "aa",
        " ",
        "-",
        "'",
        "\n",
        "\n\n",
        "İ",
        "i\u{307}",
        "K",
        "K",
        "Ⱥ",
        "ⱥ",
        "ΟΣ",
        "ΟΣΑ",
        "οσ",
        "ος",
        "e\u{301}",
        "é",
        "🇨🇺🇺🇺",
        "👩‍💻",
        "\u{0600}word",
        "你好",
    ];
    let queries = [
        "a", "aa", " ", "-", "i", "I", "i\u{307}", "k", "ⱥ", "οσ", "ΟΣ", "ος", "é", "e", "\u{301}",
        "🇺🇺", "👩", "word", "你", "\r\n", "a\r\n", "\nİ", "missing", "",
    ];
    let mut seed = 0x6a09_e667_f3bc_c909_u64;
    for case in 0..32 {
        let mut source = tokens.join("");
        for _ in 0..32 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            source.push_str(tokens[seed as usize % tokens.len()]);
        }
        let document = Document::from_text(&source);
        for case_sensitive in [true, false] {
            for whole_word in [true, false] {
                let options = SearchOptions {
                    case_sensitive,
                    whole_word,
                };
                for query in queries {
                    assert_eq!(
                        document.find(query, options),
                        reference(&document, query, options),
                        "case={case}, query={query:?}, options={options:?}, source={source:?}",
                    );
                }
            }
        }
    }
}

#[test]
fn navigation_matches_document_search_at_every_grapheme_and_direction() {
    // Selected matches, carets inside a match, overlapping rejected candidates,
    // expanding lowercase mappings, empty paragraphs, and paragraph breaks.
    let mut editor = Editor::from_text("ana banana\n\nİ i CAFÉ\n🇨🇺🇺🇺 ana\nΟΣ\nana\n");
    let positions: Vec<_> = editor
        .document()
        .paragraphs()
        .iter()
        .enumerate()
        .flat_map(|(index, paragraph)| {
            (0..=paragraph.grapheme_count()).map(move |grapheme| {
                Position::new(index, paragraph.byte_from_grapheme(grapheme).unwrap())
            })
        })
        .collect();
    for case_sensitive in [false, true] {
        for whole_word in [false, true] {
            let options = SearchOptions {
                case_sensitive,
                whole_word,
            };
            for query in [
                "ana",
                "i",
                "café",
                "🇺🇺",
                "ος",
                "a\r\n\r\nİ",
                "\n",
                "missing",
                "",
            ] {
                let matches = editor.document().find(query, options);
                let mut selections: Vec<_> =
                    positions.iter().copied().map(Selection::caret).collect();
                for range in &matches {
                    selections.push(Selection::new(range.start, range.end));
                    selections.push(Selection::new(range.end, range.start));
                }
                for selection in selections {
                    for wrap in [false, true] {
                        for backward in [false, true] {
                            let range = selection.range();
                            let expected = if backward {
                                matches
                                    .iter()
                                    .rev()
                                    .find(|item| item.end <= range.start)
                                    .or_else(|| wrap.then(|| matches.last()).flatten())
                            } else {
                                matches
                                    .iter()
                                    .find(|item| item.start >= range.end)
                                    .or_else(|| wrap.then(|| matches.first()).flatten())
                            };
                            editor.set_selection(selection).unwrap();
                            let found = if backward {
                                editor.find_previous(query, options, wrap).unwrap()
                            } else {
                                editor.find_next(query, options, wrap).unwrap()
                            };
                            assert_eq!(
                                found,
                                expected.is_some(),
                                "{query:?} {options:?} {selection:?} wrap={wrap} backward={backward}"
                            );
                            assert_eq!(
                                editor.selection(),
                                expected
                                    .map_or(selection, |item| Selection::new(item.start, item.end)),
                                "{query:?} {options:?} {selection:?} wrap={wrap} backward={backward}"
                            );
                        }
                    }
                }
            }
        }
    }
    assert_eq!(editor.undo_len(), 0);
}

#[test]
fn local_navigation_keeps_forward_nonoverlap_and_complete_lowercase_context() {
    let options = SearchOptions::default();
    let mut editor = Editor::from_text("ana\nbanana\nΟΣΑ");
    editor
        .set_selection(Selection::caret(Position::new(1, 2)))
        .unwrap();
    let caret = editor.selection();
    // Restarting the literal scanner at byte 2 would invent a later overlapping
    // "ana" at 3..6, which is absent from Document::find's accepted match set.
    assert!(!editor.find_next("ana", options, false).unwrap());
    assert_eq!(editor.selection(), caret);
    assert!(editor.find_next("ana", options, true).unwrap());
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 0), Position::new(0, 3))
    );
    editor
        .set_selection(Selection::caret(Position::new(1, 6)))
        .unwrap();
    // Searching literals in reverse would likewise prefer the overlapping 3..6.
    assert!(editor.find_previous("ana", options, false).unwrap());
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(1, 1), Position::new(1, 4))
    );
    let insensitive = SearchOptions {
        case_sensitive: false,
        whole_word: false,
    };
    editor
        .set_selection(Selection::caret(Position::new(2, 4)))
        .unwrap();
    // Lowercasing only the prefix through the caret would turn Σ into final ς;
    // the complete paragraph correctly keeps σ because Α follows it.
    assert!(!editor.find_previous("ος", insensitive, true).unwrap());
    assert!(editor.find_previous("οσ", insensitive, false).unwrap());
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(2, 0), Position::new(2, 4))
    );
}

#[test]
fn literal_navigation_agrees_with_forward_matches_around_rejected_unicode_candidates() {
    let mut editor = Editor::from_text(
        "e\u{301} e e\u{301}\n👩‍💻 👩 👩‍💻\n🇺🇸 🇺 🇨🇺\ncafé café\n\u{0600}word word\nabca abcabca\nana banana\n",
    );
    let positions: Vec<_> = editor
        .document()
        .paragraphs()
        .iter()
        .enumerate()
        .flat_map(|(index, paragraph)| {
            (0..=paragraph.grapheme_count()).map(move |grapheme| {
                Position::new(index, paragraph.byte_from_grapheme(grapheme).unwrap())
            })
        })
        .collect();
    let options = SearchOptions::default();
    for query in [
        "e",
        "\u{301}",
        "👩",
        "👩‍💻",
        "🇺",
        "café",
        "word",
        "abc",
        "abca",
        "ana",
    ] {
        let matches = editor.document().find(query, options);
        let selections =
            positions
                .iter()
                .copied()
                .map(Selection::caret)
                .chain(matches.iter().flat_map(|range| {
                    [
                        Selection::new(range.start, range.end),
                        Selection::new(range.end, range.start),
                    ]
                }));
        for selection in selections {
            let range = selection.range();
            for backward in [false, true] {
                for wrap in [false, true] {
                    let expected = if backward {
                        matches
                            .iter()
                            .rev()
                            .find(|item| item.end <= range.start)
                            .or_else(|| wrap.then(|| matches.last()).flatten())
                    } else {
                        matches
                            .iter()
                            .find(|item| item.start >= range.end)
                            .or_else(|| wrap.then(|| matches.first()).flatten())
                    };
                    editor.set_selection(selection).unwrap();
                    let found = if backward {
                        editor.find_previous(query, options, wrap).unwrap()
                    } else {
                        editor.find_next(query, options, wrap).unwrap()
                    };
                    assert_eq!(found, expected.is_some());
                    assert_eq!(
                        editor.selection(),
                        expected.map_or(selection, |item| Selection::new(item.start, item.end)),
                        "query={query:?}, selection={selection:?}, wrap={wrap}, backward={backward}",
                    );
                }
            }
        }
    }
    assert_eq!(editor.document().revision(), 0);
    assert_eq!(editor.undo_len(), 0);
}

#[test]
fn navigation_matches_global_results_for_sparse_paragraphs_and_crossing_selections() {
    let mut paragraphs = vec![""; 257];
    for index in [0, 64, 127, 255] {
        paragraphs[index] = "banana aa aa İ i\u{307} ΟΣΑ ΟΣ 🇨🇺🇺🇺";
    }
    let mut editor = Editor::from_text(&paragraphs.join("\n"));
    let mut positions = Vec::new();
    for index in [0, 1, 63, 64, 126, 127, 128, 254, 255, 256] {
        let paragraph = editor.document().paragraph(index).unwrap();
        positions.extend(
            (0..=paragraph.grapheme_count()).map(|grapheme| {
                Position::new(index, paragraph.byte_from_grapheme(grapheme).unwrap())
            }),
        );
    }
    let mut selections: Vec<_> = positions.iter().copied().map(Selection::caret).collect();
    for (index, &position) in positions.iter().enumerate() {
        let other = positions[(index * 7 + 11) % positions.len()];
        selections.extend([
            Selection::new(position, other),
            Selection::new(other, position),
        ]);
    }
    for case_sensitive in [false, true] {
        for whole_word in [false, true] {
            let options = SearchOptions {
                case_sensitive,
                whole_word,
            };
            for query in [
                "aa", "ana", "i\u{307}", "οσ", "ος", "🇺🇺", "a\r\n", "\n", "missing",
            ] {
                let matches = editor.document().find(query, options);
                for &selection in &selections {
                    let range = selection.range();
                    for wrap in [false, true] {
                        for backward in [false, true] {
                            let expected = if backward {
                                matches
                                    .iter()
                                    .rev()
                                    .find(|item| item.end <= range.start)
                                    .or_else(|| wrap.then(|| matches.last()).flatten())
                            } else {
                                matches
                                    .iter()
                                    .find(|item| item.start >= range.end)
                                    .or_else(|| wrap.then(|| matches.first()).flatten())
                            };
                            editor.set_selection(selection).unwrap();
                            let found = if backward {
                                editor.find_previous(query, options, wrap).unwrap()
                            } else {
                                editor.find_next(query, options, wrap).unwrap()
                            };
                            assert_eq!(found, expected.is_some());
                            assert_eq!(
                                editor.selection(),
                                expected
                                    .map_or(selection, |item| Selection::new(item.start, item.end)),
                                "query={query:?}, options={options:?}, selection={selection:?}, wrap={wrap}, backward={backward}"
                            );
                        }
                    }
                }
            }
        }
    }
    assert_eq!(editor.document().revision(), 0);
    assert_eq!(editor.undo_len(), 0);
}
