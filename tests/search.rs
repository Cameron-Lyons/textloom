use textloom::{Document, Editor, Error, Position, SearchOptions, Selection, StylePatch};
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
fn partial_grapheme_retries_keep_complete_matches_after_long_combining_clusters() {
    let needle = "\u{301}".repeat(32);
    let rejected = needle.repeat(2);
    let source = format!("{needle}\u{2028}{rejected}\u{2028}{needle}");
    let document = Document::from_text(&source);
    let last = source.len() - needle.len();
    let expected = vec![
        Position::new(0, 0)..Position::new(0, needle.len()),
        Position::new(0, last)..Position::new(0, source.len()),
    ];
    assert_eq!(document.find(&needle, default_options()), expected);
    let mut editor = Editor::new(document);
    editor
        .set_selection(Selection::caret(Position::new(0, needle.len())))
        .unwrap();
    assert!(editor.find_next(&needle, default_options(), false).unwrap());
    assert_eq!(editor.selection().range(), expected[1]);
    assert!(
        editor
            .find_previous(&needle, default_options(), false)
            .unwrap()
    );
    assert_eq!(editor.selection().range(), expected[0]);
    assert_eq!(
        editor
            .replace_all(&needle, "found", default_options())
            .unwrap(),
        2
    );
    assert_eq!(
        editor.document().paragraphs()[0].text(),
        format!("found\u{2028}{rejected}\u{2028}found")
    );
    assert!(editor.undo());
    assert_eq!(editor.document().paragraphs()[0].text(), source);
}

#[test]
fn repetitive_rejected_matches_preserve_later_whole_matches() {
    let cluster = "a\u{301}";
    let needle = format!("{}a", cluster.repeat(32));
    let rejected = cluster.repeat(64);
    let source = format!("{rejected} {needle} {needle}");
    let first = rejected.len() + 1;
    let last = first + needle.len() + 1;
    let expected = vec![
        Position::new(0, first)..Position::new(0, first + needle.len()),
        Position::new(0, last)..Position::new(0, source.len()),
    ];
    for case_sensitive in [true, false] {
        for whole_word in [false, true] {
            let options = SearchOptions {
                case_sensitive,
                whole_word,
            };
            let query = if case_sensitive {
                needle.clone()
            } else {
                needle.to_uppercase()
            };
            let mut editor = Editor::from_text(&source);
            assert_eq!(editor.document().find(&query, options), expected);
            assert!(editor.find_next(&query, options, false).unwrap());
            assert_eq!(editor.selection().range(), expected[0]);
            assert!(editor.find_next(&query, options, false).unwrap());
            assert_eq!(editor.selection().range(), expected[1]);
            assert!(editor.find_previous(&query, options, false).unwrap());
            assert_eq!(editor.selection().range(), expected[0]);
            assert_eq!(editor.replace_all(&query, "found", options), Ok(2));
            assert_eq!(
                editor.document().plain_text(),
                format!("{rejected} found found")
            );
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), source);
        }
    }
}

#[test]
fn alternating_rejected_and_valid_candidates_preserve_the_match_stream() {
    for (part, match_start, whole_word) in [("\u{600}aaa ", 3, false), ("aaX aa ", 4, true)] {
        let source = part.repeat(32);
        let expected: Vec<_> = (0..32)
            .map(|index| {
                let start = index * part.len() + match_start;
                Position::new(0, start)..Position::new(0, start + 2)
            })
            .collect();
        for case_sensitive in [false, true] {
            let options = SearchOptions {
                case_sensitive,
                whole_word,
            };
            let query = if case_sensitive { "aa" } else { "AA" };
            let mut editor = Editor::from_text(&source);
            assert_eq!(editor.document().find(query, options), expected);
            for range in &expected {
                assert!(editor.find_next(query, options, false).unwrap());
                assert_eq!(editor.selection().range(), *range);
            }
            assert!(!editor.find_next(query, options, false).unwrap());
            assert_eq!(editor.replace_all(query, "Z", options), Ok(32));
            assert_eq!(
                editor.document().plain_text(),
                if whole_word { "aaX Z " } else { "\u{600}aZ " }.repeat(32)
            );
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), source);
        }
    }
}

#[test]
fn paragraph_local_options_fold_final_sigma_and_respect_word_boundaries() {
    let document = Document::from_text("ΟΣ\nΟΣΑ\nİ\ni\ncat's\ncat\n你好\n\u{0600}word\nword");
    let insensitive = SearchOptions {
        case_sensitive: false,
        whole_word: false,
    };
    for query in ["ος", "οσ", "ΟΣ"] {
        assert_eq!(
            document.find(query, insensitive),
            vec![
                Position::new(0, 0)..Position::new(0, 4),
                Position::new(1, 0)..Position::new(1, 4)
            ]
        );
    }
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
            needle.to_lowercase().replace('ς', "σ")
        };
        if needle.is_empty() {
            return Vec::new();
        }
        let searched = if options.case_sensitive {
            source.clone()
        } else {
            source.to_lowercase().replace('ς', "σ")
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
fn local_navigation_keeps_forward_nonoverlap_and_folds_final_sigma() {
    let options = SearchOptions::default();
    let mut editor = Editor::from_text("ana\nbanana\nΟΣΑ");
    editor
        .set_selection(Selection::caret(Position::new(1, 2)))
        .unwrap();
    let caret = editor.selection();
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
    for query in ["ος", "οσ", "ΟΣ"] {
        editor
            .set_selection(Selection::caret(Position::new(2, 4)))
            .unwrap();
        assert!(editor.find_previous(query, insensitive, false).unwrap());
        assert_eq!(
            editor.selection(),
            Selection::new(Position::new(2, 0), Position::new(2, 4))
        );
    }
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

#[test]
fn nearby_navigation_in_long_paragraphs_matches_the_forward_match_stream() {
    let mut text = String::new();
    for index in 0..420 {
        text.push_str(match index % 7 {
            0 => "\u{212A}elvin İ matches ",
            1 => "café 👩‍💻 ",
            2 => "e\u{301}xmatch\u{2028}",
            3 => "ΣΟΣ, ",
            4 => "aab\u{3000}",
            _ => "filler ",
        });
        if index % 97 == 0 {
            text.push_str("Match. ");
        }
    }
    let mut editor = Editor::from_text(&format!("{text}\n{text}"));
    let mut selections = Vec::new();
    for index in 0..2 {
        let paragraph = editor.document().paragraph(index).unwrap();
        for grapheme in (0..=paragraph.grapheme_count()).step_by(53) {
            let byte = paragraph.byte_from_grapheme(grapheme).unwrap();
            selections.push(Selection::caret(Position::new(index, byte)));
        }
    }
    for case_sensitive in [false, true] {
        for whole_word in [false, true] {
            let options = SearchOptions {
                case_sensitive,
                whole_word,
            };
            for query in ["match", "kelvin", "σος", "i\u{307}", "aab", "missing"] {
                let matches = editor.document().find(query, options);
                let mut selections = selections.clone();
                for range in &matches {
                    selections.push(Selection::new(range.start, range.end));
                    let paragraph = editor.document().paragraph(range.start.paragraph).unwrap();
                    let offsets = if matches.len() <= 16 {
                        1020..1030
                    } else {
                        0..0
                    };
                    for offset in offsets {
                        for byte in [
                            range.start.byte.checked_sub(offset),
                            Some(range.end.byte + offset),
                        ]
                        .into_iter()
                        .flatten()
                        {
                            let byte = paragraph.boundary_at_or_before(byte);
                            selections
                                .push(Selection::caret(Position::new(range.start.paragraph, byte)));
                        }
                    }
                    let mut copy = editor.clone();
                    copy.set_selection(Selection::new(range.start, range.end))
                        .unwrap();
                    assert!(copy.replace_selected_match(query, "x", options).unwrap());
                }
                for &selection in &selections {
                    let range = selection.range();
                    for backward in [false, true] {
                        let expected = if backward {
                            matches.iter().rev().find(|item| item.end <= range.start)
                        } else {
                            matches.iter().find(|item| item.start >= range.end)
                        };
                        editor.set_selection(selection).unwrap();
                        let found = if backward {
                            editor.find_previous(query, options, false).unwrap()
                        } else {
                            editor.find_next(query, options, false).unwrap()
                        };
                        assert_eq!(found, expected.is_some());
                        assert_eq!(
                            editor.selection(),
                            expected.map_or(selection, |item| Selection::new(item.start, item.end)),
                            "query={query:?}, options={options:?}, selection={selection:?}, backward={backward}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn nearby_navigation_keeps_shrinking_lowercase_and_joined_space_candidates_at_every_caret() {
    let padding = "x ".repeat(520);
    let mut editor = Editor::from_text(&format!(
        "{padding}a\u{212A}\u{212A}\u{212A} x \u{3000}aab  aab \u{AD}aab{padding}"
    ));
    let paragraph = editor.document().paragraph(0).unwrap();
    let carets: Vec<_> = (0..=paragraph.grapheme_count())
        .map(|grapheme| {
            Selection::caret(Position::new(
                0,
                paragraph.byte_from_grapheme(grapheme).unwrap(),
            ))
        })
        .collect();
    for case_sensitive in [false, true] {
        for whole_word in [false, true] {
            let options = SearchOptions {
                case_sensitive,
                whole_word,
            };
            for query in [
                "a\u{212A}\u{212A}\u{212A}",
                "\u{3000}aab",
                " aab",
                "\u{AD}aab",
            ] {
                let matches = editor.document().find(query, options);
                for &selection in &carets {
                    let range = selection.range();
                    for backward in [false, true] {
                        let expected = if backward {
                            matches.iter().rev().find(|item| item.end <= range.start)
                        } else {
                            matches.iter().find(|item| item.start >= range.end)
                        };
                        editor.set_selection(selection).unwrap();
                        let found = if backward {
                            editor.find_previous(query, options, false).unwrap()
                        } else {
                            editor.find_next(query, options, false).unwrap()
                        };
                        assert_eq!(found, expected.is_some());
                        assert_eq!(
                            editor.selection(),
                            expected.map_or(selection, |item| Selection::new(item.start, item.end)),
                            "query={query:?}, options={options:?}, selection={selection:?}, backward={backward}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn navigation_after_edits_between_presses_matches_document_search() {
    let text = "Match matches aab ΣΟΣ café 👩‍💻 \u{212A}elvin. ".repeat(80);
    for case_sensitive in [false, true] {
        for whole_word in [false, true] {
            let options = SearchOptions {
                case_sensitive,
                whole_word,
            };
            for query in ["match", "aab", "kelvin"] {
                let mut editor = Editor::from_text(&format!("{text}\n{text}"));
                let mut presses = 0;
                loop {
                    let selection = editor.selection();
                    let range = selection.range();
                    let matches = editor.document().find(query, options);
                    let previous = matches.iter().rev().find(|item| item.end <= range.start);
                    let mut copy = editor.clone();
                    assert_eq!(
                        copy.find_previous(query, options, false).unwrap(),
                        previous.is_some()
                    );
                    assert_eq!(
                        copy.selection(),
                        previous.map_or(selection, |item| Selection::new(item.start, item.end)),
                        "query={query:?}, options={options:?}, selection={selection:?}"
                    );
                    let Some(next) = matches.iter().find(|item| item.start >= range.end) else {
                        assert!(!editor.find_next(query, options, false).unwrap());
                        assert_eq!(editor.selection(), selection);
                        break;
                    };
                    assert!(editor.find_next(query, options, false).unwrap());
                    assert_eq!(
                        editor.selection(),
                        Selection::new(next.start, next.end),
                        "query={query:?}, options={options:?}, selection={selection:?}"
                    );
                    let revision = editor.document().revision();
                    match presses % 4 {
                        0 => assert!(editor.replace_selected_match(query, "x", options).unwrap()),
                        1 => editor.replace_range(next.end..next.end, "s").unwrap(),
                        2 => {
                            let start = Position::new(next.start.paragraph, 0);
                            let inserted = "MATCH \u{212A}ELVIN aab ";
                            editor.replace_range(start..start, inserted).unwrap();
                            let shift = |position: Position| {
                                Position::new(position.paragraph, position.byte + inserted.len())
                            };
                            editor
                                .set_selection(Selection::new(shift(next.start), shift(next.end)))
                                .unwrap();
                        }
                        _ => assert!(editor.undo()),
                    }
                    assert!(editor.document().revision() > revision);
                    presses += 1;
                }
                assert!(
                    presses > 0 || (case_sensitive && query != "aab"),
                    "query={query:?}, options={options:?}"
                );
            }
        }
    }
}

#[test]
fn cross_paragraph_navigation_after_edits_between_presses_matches_document_search() {
    let text = "Match café 👩‍💻\nΣΟΣ matches \u{212A}elvin.\naab ΣΟΣ\n".repeat(12);
    let mut total = 0;
    for case_sensitive in [false, true] {
        for whole_word in [false, true] {
            let options = SearchOptions {
                case_sensitive,
                whole_word,
            };
            for query in [
                "café 👩‍💻\r\nΣΟΣ",
                "\u{212A}elvin.\naab",
                "👩‍💻\nΣΟΣ matches \u{212A}elvin.\naab",
                "οσ\nmatch",
                "\n",
                "\nΣΟΣ matches \u{212A}elvin.\n",
            ] {
                let mut editor = Editor::from_text(&text);
                let mut presses = 0;
                loop {
                    let selection = editor.selection();
                    let range = selection.range();
                    let matches = editor.document().find(query, options);
                    let context = format!("query={query:?}, options={options:?}, {selection:?}");
                    let mut copy = editor.clone();
                    assert_eq!(
                        copy.replace_selected_match(query, "x", options).unwrap(),
                        matches.contains(&range),
                        "{context}"
                    );
                    for wrap in [false, true] {
                        let previous = matches
                            .iter()
                            .rev()
                            .find(|item| item.end <= range.start)
                            .or_else(|| wrap.then(|| matches.last()).flatten());
                        let mut copy = editor.clone();
                        assert_eq!(
                            copy.find_previous(query, options, wrap).unwrap(),
                            previous.is_some()
                        );
                        assert_eq!(
                            copy.selection(),
                            previous.map_or(selection, |item| Selection::new(item.start, item.end)),
                            "wrap={wrap}, {context}"
                        );
                    }
                    let Some(next) = matches.iter().find(|item| item.start >= range.end) else {
                        assert!(!editor.find_next(query, options, false).unwrap());
                        assert_eq!(editor.selection(), selection);
                        assert_eq!(
                            editor.find_next(query, options, true).unwrap(),
                            !matches.is_empty()
                        );
                        assert_eq!(
                            editor.selection(),
                            matches
                                .first()
                                .map_or(selection, |item| Selection::new(item.start, item.end)),
                            "{context}"
                        );
                        break;
                    };
                    assert!(editor.find_next(query, options, false).unwrap());
                    assert_eq!(
                        editor.selection(),
                        Selection::new(next.start, next.end),
                        "{context}"
                    );
                    let revision = editor.document().revision();
                    match presses % 4 {
                        0 => assert!(editor.replace_selected_match(query, "x", options).unwrap()),
                        1 => editor.replace_range(next.end..next.end, "s").unwrap(),
                        2 => {
                            let start = Position::new(next.start.paragraph, 0);
                            let inserted = "MATCH ";
                            editor
                                .replace_range(start..start, &format!("aab ΣΟΣ\n{inserted}"))
                                .unwrap();
                            let shift = |position: Position| {
                                let byte = if position.paragraph == start.paragraph {
                                    position.byte + inserted.len()
                                } else {
                                    position.byte
                                };
                                Position::new(position.paragraph + 1, byte)
                            };
                            editor
                                .set_selection(Selection::new(shift(next.start), shift(next.end)))
                                .unwrap();
                        }
                        _ => assert!(editor.undo()),
                    }
                    assert!(editor.document().revision() > revision);
                    presses += 1;
                }
                total += presses;
            }
        }
    }
    assert!(total > 300, "{total}");
}

#[test]
fn case_insensitive_search_finds_case_sensitive_sigma_matches_in_any_context() {
    let insensitive = SearchOptions {
        case_sensitive: false,
        whole_word: false,
    };
    for (text, query) in [
        ("ΟΣΑ", "ΟΣ"),
        ("ΟΣ", "Σ"),
        ("ΑΣ.Α", "ΑΣ"),
        ("λόγος", "σ"),
        ("ΟΣΑ", "ς"),
        ("ΟΣ ΟΣΑ λόγος", "οσ"),
    ] {
        let document = Document::from_text(text);
        let sensitive = document.find(query, default_options());
        let matches = document.find(query, insensitive);
        assert!(!matches.is_empty(), "{text:?} {query:?}");
        assert!(
            sensitive.iter().all(|found| matches
                .iter()
                .any(|candidate| candidate.start < found.end && found.start < candidate.end)),
            "{text:?} {query:?} {sensitive:?} {matches:?}"
        );
    }
    let mut editor = Editor::from_text("ΟΣΑ");
    assert!(editor.find_next("ΟΣ", insensitive, true).unwrap());
    assert_eq!(editor.selected_text(), "ΟΣ");
    assert_eq!(editor.replace_all("οσ", "x", insensitive).unwrap(), 1);
    assert_eq!(editor.document().plain_text(), "xΑ");
}

#[test]
fn replacing_the_selected_match_matches_replace_all_formatting_in_one_step() {
    let mut editor = Editor::from_text("xx foo yy foo");
    editor
        .set_selection(Selection::new(Position::new(0, 3), Position::new(0, 4)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    let mut all = Editor::new(editor.document().clone());
    all.replace_all("foo", "bar", default_options()).unwrap();
    caret(&mut editor, 0, 0);
    assert!(editor.find_next("foo", default_options(), true).unwrap());
    let selection = editor.selection();
    let steps = editor.undo_len();
    assert!(
        editor
            .replace_selected_match("foo", "bar", default_options())
            .unwrap()
    );
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 6)));
    assert!(
        !editor
            .replace_selected_match("foo", "bar", default_options())
            .unwrap()
    );
    assert!(editor.find_next("foo", default_options(), true).unwrap());
    assert!(
        editor
            .replace_selected_match("foo", "bar", default_options())
            .unwrap()
    );
    assert_eq!(editor.document().paragraphs(), all.document().paragraphs());
    assert_eq!(editor.undo_len(), steps + 2);
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "xx foo yy foo");
    assert_eq!(editor.selection(), selection);
}

#[test]
fn only_selections_in_the_match_stream_are_replaced() {
    let insensitive = SearchOptions {
        case_sensitive: false,
        whole_word: false,
    };
    let whole_word = SearchOptions {
        case_sensitive: true,
        whole_word: true,
    };
    let mut editor = Editor::from_text("banana Foo cat category\nend");
    let select = |editor: &mut Editor, start: (usize, usize), end: (usize, usize)| {
        editor
            .set_selection(Selection::new(
                Position::new(start.0, start.1),
                Position::new(end.0, end.1),
            ))
            .unwrap();
    };
    for (start, end, query, options, expected) in [
        ((0, 3), (0, 6), "ana", default_options(), false),
        ((0, 1), (0, 4), "ana", default_options(), true),
        ((0, 7), (0, 10), "foo", default_options(), false),
        ((0, 7), (0, 10), "foo", insensitive, true),
        ((0, 15), (0, 18), "cat", whole_word, false),
        ((0, 11), (0, 14), "cat", whole_word, true),
        ((0, 22), (1, 2), "y\nen", default_options(), true),
        ((0, 22), (1, 1), "y\nen", default_options(), false),
        ((0, 3), (0, 3), "", default_options(), false),
    ] {
        let mut copy = editor.clone();
        select(&mut copy, start, end);
        let selection = copy.selection();
        let content = copy.document().paragraphs().to_vec();
        assert_eq!(
            copy.replace_selected_match(query, "_", options).unwrap(),
            expected,
            "{query:?} {start:?}..{end:?}"
        );
        if !expected {
            assert_eq!(copy.selection(), selection);
            assert_eq!(copy.document().paragraphs(), content);
        }
    }
    select(&mut editor, (0, 22), (1, 2));
    assert!(
        editor
            .replace_selected_match("y\nen", "y!\nEN", default_options())
            .unwrap()
    );
    assert_eq!(
        editor.document().plain_text(),
        "banana Foo cat category!\nENd"
    );
    assert_eq!(editor.undo_len(), 1);
    editor.update_composition("x", None).unwrap();
    assert_eq!(
        editor.replace_selected_match("x", "y", default_options()),
        Err(Error::CompositionActive)
    );
}

fn caret(editor: &mut Editor, paragraph: usize, byte: usize) {
    editor
        .set_selection(Selection::caret(Position::new(paragraph, byte)))
        .unwrap();
}

#[test]
fn line_feed_queries_match_paragraph_breaks_but_not_soft_line_breaks() {
    let document = Document::from_text("ab\u{2028}cd\nef");
    let find = |query| document.find(query, default_options());
    let range = |start: (usize, usize), end: (usize, usize)| {
        Position::new(start.0, start.1)..Position::new(end.0, end.1)
    };
    assert_eq!(find("\n"), [range((0, 7), (1, 0))]);
    assert_eq!(find("d\ne"), [range((0, 6), (1, 1))]);
    assert!(find("b\nc").is_empty());
    assert_eq!(find("\u{2028}"), [range((0, 2), (0, 5))]);
    assert_eq!(find("b\u{2028}cd\ne"), [range((0, 1), (1, 1))]);
    assert_eq!(find("f"), [range((1, 1), (1, 2))]);
    assert_eq!(find("d\u{2029}e"), [range((0, 6), (1, 1))]);
    let words = SearchOptions {
        case_sensitive: false,
        whole_word: true,
    };
    assert_eq!(document.find("CD", words), [range((0, 5), (0, 7))]);
    assert_eq!(
        document.find("AB\u{2028}CD", words),
        [range((0, 0), (0, 7))]
    );
    assert!(document.find("B\u{2028}C", words).is_empty());

    let mut editor = Editor::new(document.clone());
    assert!(
        editor
            .find_next("b\u{2028}cd\ne", default_options(), true)
            .unwrap()
    );
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 1), Position::new(1, 1))
    );
    assert!(
        editor
            .replace_selected_match("b\u{2028}cd\ne", "X", default_options())
            .unwrap()
    );
    assert_eq!(editor.document().plain_text(), "aXf");
    assert!(editor.undo());
    assert_eq!(
        editor.replace_all("\u{2028}", " ", default_options()),
        Ok(1)
    );
    assert_eq!(editor.document().plain_text(), "ab cd\nef");
    assert_eq!(
        editor.replace_all(" ", "\u{2028}", default_options()),
        Ok(1)
    );
    assert_eq!(editor.document().paragraphs().len(), 2);
    assert_eq!(editor.document().paragraphs()[0].text(), "ab\u{2028}cd");
}
