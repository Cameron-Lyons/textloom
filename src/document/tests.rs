//! Unit tests for documents and paragraphs: newline normalization, Unicode indexing, formatting
//! runs, replacements, list numbering, and the deltas that undo and redo edits.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;
use std::{iter, mem};

use unicode_segmentation::UnicodeSegmentation;

use super::change::Delta;
use crate::{
    Color, Document, Editor, Error, Fragment, InlineStyle, Movement, Paragraph, ParagraphKind,
    Position, SearchOptions, Selection, Span, StylePatch,
};

#[test]
fn saved_state_cache_tracks_edits_and_rollback_after_revision_saturation() {
    let mut document = Document::from_text("abc");
    document.revision = u64::MAX;
    let mut editor = Editor::new(document);
    assert!(!editor.is_modified());
    editor.insert_text("x").unwrap();
    assert_eq!(editor.document().revision(), u64::MAX);
    assert!(editor.is_modified());
    assert!(editor.undo());
    assert!(!editor.is_modified());
    assert!(editor.redo());
    assert!(editor.is_modified());
    editor.mark_saved();
    assert!(!editor.is_modified());
    let result: Result<(), Error> = editor.transaction(|editor| {
        editor.insert_text("y")?;
        assert!(editor.is_modified());
        Err(Error::InvalidRange)
    });
    assert_eq!(result, Err(Error::InvalidRange));
    assert_eq!(editor.document().plain_text(), "xabc");
    assert!(!editor.is_modified());
    assert_eq!(editor.document().revision(), u64::MAX);
}

#[test]
fn newline_normalization_handles_all_adjacent_breaks_and_keeps_lf_borrowed() {
    let tokens = ["a", "é", "\r", "\n", "\u{2028}", "\u{2029}"];
    for length in 0..=5 {
        for mut case in 0..tokens.len().pow(length) {
            let mut text = String::new();
            for _ in 0..length {
                text.push_str(tokens[case % tokens.len()]);
                case /= tokens.len();
            }
            let expected = text.replace("\r\n", "\n").replace(['\r', '\u{2029}'], "\n");
            let normalized = super::normalize_newlines(&text);
            assert_eq!(normalized, expected, "input {text:?}");
            assert_eq!(
                matches!(normalized, Cow::Borrowed(_)),
                !text.contains(['\r', '\u{2029}']),
                "input {text:?}"
            );
        }
    }
}

#[test]
fn soft_line_breaks_are_whitespace_graphemes_inside_one_paragraph() {
    let document = Document::from_text("ab\u{2028}cd\u{2029}e\r\u{2028}");
    let texts: Vec<_> = document
        .paragraphs()
        .iter()
        .map(|paragraph| paragraph.text())
        .collect();
    assert_eq!(texts, ["ab\u{2028}cd", "e", "\u{2028}"]);
    let paragraph = &document.paragraphs()[0];
    assert_eq!(paragraph.grapheme_count(), 5);
    assert_eq!(paragraph.byte_from_grapheme(3), Some(5));
    assert_eq!(paragraph.next_grapheme(2), 5);
    assert_eq!(paragraph.previous_grapheme(5), 2);
    assert_eq!(paragraph.next_word(2), 7);
    assert_eq!(paragraph.previous_word(5), 0);
    assert_eq!(paragraph.word_ranges(), [0..2, 5..7]);
    for byte in [3, 4] {
        assert!(
            document.validate_position(Position::new(0, byte)).is_err(),
            "byte {byte}"
        );
    }
    let span = |range: Range<usize>, bold| Span {
        range,
        style: InlineStyle {
            bold,
            ..InlineStyle::default()
        },
        link: None,
    };
    let rich = Paragraph::new(
        "a\u{2028}b",
        vec![span(0..1, false), span(1..4, true), span(4..5, false)],
        ParagraphKind::Heading { level: 1 },
    )
    .unwrap();
    assert_eq!(rich.text(), "a\u{2028}b");
    assert_eq!(
        Paragraph::new(
            "a\u{2028}b",
            vec![span(0..2, false), span(2..5, true)],
            ParagraphKind::Body
        ),
        Err(Error::InvalidFragment)
    );

    let mut document = Document::from_text("a");
    let (_, caret) = document
        .replace(
            Position::new(0, 1)..Position::new(0, 1),
            "\u{2028}\u{301}",
            InlineStyle::default(),
        )
        .unwrap();
    assert_eq!(caret, Position::new(0, 6));
    assert_eq!(document.paragraphs()[0].grapheme_count(), 3);
    assert_normalized(&document);
}

#[test]
fn word_selection_treats_soft_line_ends_like_paragraph_ends() {
    let document = Document::from_text("one two\u{2028}three\u{2028}\u{2028}four\u{2028}");
    let word = |byte| {
        let range = document.word_range_at(Position::new(0, byte)).unwrap();
        assert_eq!(range.start.paragraph, 0);
        assert_eq!(range.end.paragraph, 0);
        range.start.byte..range.end.byte
    };
    assert_eq!(word(7), 4..7);
    assert_eq!(word(10), 10..15);
    assert_eq!(word(15), 10..15);
    assert_eq!(word(18), 18..18);
    assert_eq!(word(document.end().byte), 28..28);
    assert_eq!(word(21), 21..25);
    let document = Document::from_text("\u{2028}x");
    assert_eq!(
        document.word_range_at(Position::default()),
        Ok(Position::default()..Position::default())
    );
}

#[test]
fn indexed_unicode_offsets_match_extended_graphemes_and_scalars() {
    for text in [
        "",
        "plain ASCII",
        "café e\u{301}",
        "👨\u{200d}👩\u{200d}👧\u{200d}👦🇺🇸🇨🇦🇯🇵",
        "क्\u{200d}ष नमस्ते",
        "a\u{301}\u{302}👩🏽\u{200d}💻z",
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
        assert_eq!(paragraph.grapheme_count(), graphemes.len() - 1, "{text:?}");
        assert_eq!(paragraph.scalar_count(), scalars.len() - 1, "{text:?}");
        for (index, byte) in graphemes.iter().enumerate() {
            assert_eq!(
                paragraph.byte_from_grapheme(index),
                Some(*byte),
                "{text:?}, grapheme {index}"
            );
            assert_eq!(
                paragraph.grapheme_index(*byte),
                Some(index),
                "{text:?}, grapheme {index}"
            );
        }
        for (index, byte) in scalars.iter().enumerate() {
            assert_eq!(
                paragraph.byte_from_scalar(index),
                Some(*byte),
                "{text:?}, scalar {index}"
            );
            assert_eq!(
                paragraph.scalar_index(*byte),
                Some(index),
                "{text:?}, scalar {index}"
            );
        }
        assert_eq!(
            paragraph.byte_from_grapheme(graphemes.len()),
            None,
            "{text:?}"
        );
        assert_eq!(paragraph.byte_from_scalar(scalars.len()), None, "{text:?}");
        for byte in 0..=text.len() + 2 {
            assert_eq!(
                paragraph.grapheme_index(byte),
                graphemes.iter().position(|offset| *offset == byte),
                "{text:?}, byte {byte}"
            );
            assert_eq!(
                paragraph.scalar_index(byte),
                scalars.iter().position(|offset| *offset == byte),
                "{text:?}, byte {byte}"
            );
            assert_eq!(
                paragraph.previous_grapheme(byte),
                graphemes
                    .iter()
                    .copied()
                    .rev()
                    .find(|offset| *offset < byte)
                    .unwrap_or(0),
                "{text:?}, byte {byte}"
            );
            assert_eq!(
                paragraph.next_grapheme(byte),
                graphemes
                    .iter()
                    .copied()
                    .find(|offset| *offset > byte)
                    .unwrap_or(text.len()),
                "{text:?}, byte {byte}"
            );
            assert_eq!(
                paragraph.boundary_at_or_before(byte),
                graphemes
                    .iter()
                    .copied()
                    .rev()
                    .find(|offset| *offset <= byte)
                    .unwrap_or(0),
                "{text:?}, byte {byte}"
            );
            assert_eq!(
                paragraph.boundary_at_or_after(byte),
                graphemes
                    .iter()
                    .copied()
                    .find(|offset| *offset >= byte)
                    .unwrap_or(text.len()),
                "{text:?}, byte {byte}"
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
        "👨\u{200d}👩\u{200d}👧\u{200d}👦🇺🇸🇨🇦🇯🇵",
        "क्\u{200d}ष नमस्ते",
        "각나",
        "a\t\u{301}\u{0000}👩🏽\u{200d}💻z",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    texts.push(format!("a👩{}\u{200d}💻z", "\u{301}".repeat(64)));
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
        let _ = paragraph.grapheme_count();
        let cached = paragraph.cached_grapheme_boundaries().map(<[_]>::as_ptr);
        for byte in (0..=text.len() + 2).chain([usize::MAX]) {
            check(&paragraph, byte);
        }
        assert_eq!(
            paragraph.cached_grapheme_boundaries().map(<[_]>::as_ptr),
            cached,
            "{text:?}"
        );
    }
}

#[test]
fn trailing_grapheme_tracking_matches_complete_segmentation_while_text_grows() {
    let pieces = [
        "a", " ", "\t", "é", "\u{301}", "\u{300}", "🇦", "🇧", "\u{200d}", "👩", "💻", "🏽", "क",
        "\u{94d}", "ष", "\u{600}", "\u{1100}", "\u{1161}", "\u{11a8}", "\u{903}",
    ];
    let mut seed = 0x7a11_c105_7e25_0b5eu64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    for _ in 0..400 {
        let mut text = String::new();
        let (mut tail, mut scanned) = (0, 0);
        for _ in 0..24 {
            for _ in 0..=next(3) {
                text.push_str(pieces[next(pieces.len())]);
            }
            if next(3) == 0 {
                continue;
            }
            tail = super::last_grapheme_start(&text, tail, scanned);
            scanned = text.len();
            assert_eq!(
                tail,
                text.grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(start, _)| start),
                "{text:?}"
            );
        }
    }

    for (prefix, appended, expected) in [("🇦🇧", "🇨", 8), ("🇦", "🇨", 0), ("🇦", "🇨🇩", 8)]
    {
        assert_eq!(
            super::last_grapheme_start(&format!("{prefix}{appended}"), 0, prefix.len()),
            expected,
            "{prefix:?} + {appended:?}"
        );
    }
    assert_eq!(
        super::last_grapheme_start("ab\u{301}", 0, 2),
        0,
        "the cached start is trusted rather than recomputed, so a growing cluster is never \
         rescanned"
    );
    assert_eq!(super::last_grapheme_start("ab\u{301}c", 0, 2), 4);
    assert_eq!(super::last_grapheme_start("abcd", 0, 2), 3);
}

#[test]
fn nearby_grapheme_navigation_and_deletion_keep_indexes_lazy() {
    let phrase = "café e\u{301} 👩🏽\u{200d}💻 🇺🇸 नमस्ते ";
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
    let text = "café e\u{301} 👩🏽\u{200d}💻 🇦🇧🇨 क्\u{200d}ष ".repeat(16);
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
        assert_eq!(
            document.validate_position(position).is_ok(),
            valid,
            "byte {byte}"
        );
        assert_eq!(document.style_at(position).bold, valid, "byte {byte}");
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
            format!("a👩{}\u{200d}💻z", "\u{301}".repeat(64)),
            "a👩\u{200d}".len() + "\u{301}".len() * 64,
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
            boundaries.contains(&byte),
            "{text:?}"
        );
        let cached = paragraph.cached_grapheme_boundaries().unwrap().as_ptr();
        for byte in 0..=text.len() {
            assert_eq!(
                paragraph.is_grapheme_boundary(byte),
                boundaries.contains(&byte),
                "{text:?}, byte {byte}"
            );
            assert_eq!(
                paragraph.cached_grapheme_boundaries().unwrap().as_ptr(),
                cached,
                "{text:?}, byte {byte}"
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
    assert_eq!(document.find("🇦🇦", SearchOptions::default()), expected);
    let paragraph = document.paragraph(0).unwrap();
    let cached = paragraph.cached_grapheme_boundaries().unwrap().as_ptr();
    assert_eq!(document.find("🇦🇦", SearchOptions::default()), expected);
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
            "\u{200d}",
            "café 👩\u{200d}💻".len(),
            false,
        ),
        ("a🇦🇧🇨🇩z".to_owned(), "a🇦🇧".len(), "🇪", "a🇦🇧🇪🇨".len(), false),
        (
            format!("a👩{}💻z", "\u{301}".repeat(64)),
            "a👩".len() + "\u{301}".len() * 64,
            "\u{200d}",
            "a👩\u{200d}💻".len() + "\u{301}".len() * 64,
            true,
        ),
    ] {
        let mut editor = Editor::from_text(&text);
        editor
            .set_selection(Selection::caret(Position::new(0, byte)))
            .unwrap();
        editor.insert_text(insertion).unwrap();
        let paragraph = editor.document().paragraph(0).unwrap();
        assert_eq!(
            editor.selection().focus,
            Position::new(0, expected_byte),
            "{text:?}"
        );
        assert_eq!(
            paragraph.cached_grapheme_boundaries().is_some(),
            indexed,
            "{text:?}"
        );
        assert!(
            editor
                .document()
                .validate_position(editor.selection().focus)
                .is_ok(),
            "{text:?}"
        );
        assert_eq!(
            paragraph.cached_grapheme_boundaries().is_some(),
            indexed,
            "{text:?}"
        );
        assert!(editor.undo(), "{text:?}");
        assert_eq!(editor.document().plain_text(), text, "{text:?}");
        assert!(editor.redo(), "{text:?}");
        assert_eq!(
            editor.selection().focus,
            Position::new(0, expected_byte),
            "{text:?}"
        );
    }
}

#[test]
fn word_navigation_stops_at_unicode_words_and_symbol_runs() {
    let stops = |text: &str| -> Vec<Range<usize>> {
        let mut stops: Vec<Range<usize>> = Vec::new();
        let mut symbols = false;
        for (start, segment) in text.split_word_bound_indices() {
            let range = start..start + segment.len();
            if text.unicode_word_indices().any(|(word, _)| word == start) {
                stops.push(range);
                symbols = false;
            } else if segment.trim().is_empty() {
                symbols = false;
            } else if symbols {
                stops.last_mut().unwrap().end = range.end;
            } else {
                stops.push(range);
                symbols = true;
            }
        }
        stops
    };
    for text in [
        "",
        "one, two's!",
        "can't 123,456 12.34 under_score a:b; c.d 'word'",
        "\t  one\u{000b}two\u{000c}three \u{0000}four",
        "café 👩\u{200d}💻 नमस्ते 🇺🇸",
        "你好世界 e\u{301}",
        "!!!",
        "hello 🦀🦀🦀",
        "foo ---------- bar",
        "a--b 👩\u{200d}💻!? c",
    ] {
        let paragraph = Paragraph::plain(text);
        let stops = stops(text);
        for byte in 0..=text.len() + 1 {
            let previous = stops
                .iter()
                .rev()
                .find(|stop| stop.start < byte)
                .map_or(0, |stop| stop.start);
            let next = stops
                .iter()
                .find(|stop| stop.end > byte)
                .map_or(text.len(), |stop| stop.end);
            assert_eq!(
                paragraph.previous_word(byte),
                paragraph.boundary_at_or_before(previous),
                "{text:?} previous from {byte}"
            );
            assert_eq!(
                paragraph.next_word(byte),
                paragraph.boundary_at_or_after(next),
                "{text:?} next from {byte}"
            );
        }
    }
    let crabs = Paragraph::plain("hello 🦀🦀🦀");
    assert_eq!(crabs.previous_word(crabs.text().len()), 6);
    assert_eq!(crabs.previous_word(6), 0);
    assert_eq!(crabs.next_word(5), crabs.text().len());
    let dashes = Paragraph::plain("foo ---------- bar");
    assert_eq!(dashes.previous_word(14), 4);
    assert_eq!(dashes.next_word(3), 14);
    let mixed = Paragraph::plain("one, two's!");
    assert_eq!(mixed.word_ranges(), &[0..3, 3..4, 5..10, 10..11]);
}

#[test]
fn word_stops_match_general_word_segmentation() {
    let check = |paragraph: &Paragraph| {
        let text = paragraph.text();
        let words: Vec<usize> = if text.is_ascii() {
            text.split_word_bound_indices()
                .filter(|(_, segment)| segment.bytes().any(|byte| byte.is_ascii_alphanumeric()))
                .map(|(start, _)| start)
                .collect()
        } else {
            text.unicode_word_indices()
                .map(|(start, _)| start)
                .collect()
        };
        let mut stops: Vec<Range<usize>> = Vec::new();
        let mut symbols = false;
        for (start, segment) in text.split_word_bound_indices() {
            let range = start..start + segment.len();
            if words.binary_search(&start).is_ok() {
                stops.push(range);
                symbols = false;
            } else if segment.chars().all(char::is_whitespace) {
                symbols = false;
            } else if symbols {
                stops.last_mut().unwrap().end = range.end;
            } else {
                stops.push(range);
                symbols = true;
            }
        }
        assert_eq!(paragraph.word_ranges(), stops, "{text:?}");
    };
    for text in [
        "",
        " ",
        "___",
        "__x__ a_b _1",
        "can't 'twas rock 'n' roll''",
        "1,000.5 3.14 1;2 1'000 1..2 1.,2 x.1 1.x",
        "e.g. a:b a::b a..b a.'b",
        "http://example.com/a_b?c=d&e#f",
        "\r\n\r\n\t \u{b}\u{c}\r \n",
        "!!! a!!!b --a-- #1 $2 %3 \"quoted\"",
        "x\u{0}y\u{1f}z\u{7f}w",
    ] {
        check(&Paragraph::plain(text));
    }
    check(&Paragraph::plain(
        &"A short paragraph with ASCII words, 1,000.5 e.g. can't __x__ ___ !? \t".repeat(50),
    ));
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as usize
    };
    let ascii: Vec<char> = (0..=127u8).map(char::from).collect();
    let punctuated: Vec<char> = "aZ09_'.,;:\"-!?/@ \t\u{b}\u{c}\r\n\u{0}\u{1f}\u{7f}"
        .chars()
        .collect();
    let unicode: Vec<char> = "aZ09_'.,: \t\r\néß你ा\u{301}\u{200d}👩💻🇺🇸\u{a0}\u{2028}\u{3000}٣²"
        .chars()
        .collect();
    for alphabet in [&ascii, &punctuated, &unicode] {
        for _ in 0..10_000 {
            let text: String = (0..next() % 24)
                .map(|_| alphabet[next() % alphabet.len()])
                .collect();
            check(&Paragraph::plain(&text));
        }
    }
    let mut document = Document::from_text("can't 1,000 e.g. __x__");
    for _ in 0..500 {
        let paragraph = document.paragraph(0).unwrap();
        check(paragraph);
        let len = paragraph.text().len();
        let start = next() % (len + 1);
        let end = (start + next() % 3).min(len);
        let token = ["a", "0", "_", "'", ".", ",", ":", "-", " ", "\t", "", "x.y"][next() % 12];
        document
            .replace(
                Position::new(0, start)..Position::new(0, end),
                token,
                InlineStyle::default(),
            )
            .unwrap();
    }
}

#[test]
fn zero_index_conversion_and_vertical_navigation_keep_unicode_indexes_lazy() {
    for text in ["", "plain", "é👩\u{200d}💻", "e\u{301}🇦🇧क्\u{200d}ष"] {
        let paragraph = Paragraph::plain(text);
        assert_eq!(paragraph.byte_from_grapheme(0), Some(0), "{text:?}");
        assert_eq!(paragraph.byte_from_scalar(0), Some(0), "{text:?}");
        assert!(paragraph.indices.get().is_none(), "{text:?}");
        if let Some(first) = text.graphemes(true).next() {
            assert_eq!(
                paragraph.byte_from_grapheme(1),
                Some(first.len()),
                "{text:?}"
            );
            assert_eq!(
                paragraph.byte_from_scalar(1),
                Some(text.chars().next().unwrap().len_utf8()),
                "{text:?}"
            );
        }
    }

    let paragraph = "é👩\u{200d}💻e\u{301}".repeat(256);
    let source = format!("start\n{paragraph}\n{paragraph}");
    let mut editor = Editor::from_text(&source);
    editor.move_cursor(Movement::ParagraphDown, false).unwrap();
    assert_eq!(editor.selection(), Selection::caret(Position::new(1, 0)));
    editor.move_cursor(Movement::ParagraphDown, true).unwrap();
    let selection = Selection::new(Position::new(1, 0), Position::new(2, 0));
    assert_eq!(editor.selection(), selection);
    assert!(
        editor
            .document()
            .paragraphs()
            .iter()
            .all(|paragraph| paragraph.indices.get().is_none())
    );
    editor.insert_text("x").unwrap();
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), source);
    assert_eq!(editor.selection(), selection);
    assert!(
        editor
            .document()
            .paragraphs()
            .iter()
            .all(|paragraph| paragraph.indices.get().is_none())
    );
}

#[test]
fn unicode_indexes_are_independently_lazy_and_shared_across_metadata_changes() {
    let mut document = Document::from_text("café 👩\u{200d}💻");
    let original = Arc::clone(&document.paragraphs()[0]);
    assert!(original.indices.get().is_none());
    assert_eq!(original.scalar_count(), 8);
    assert!(original.indices.get().is_none());
    let equal = Paragraph::plain(original.text());
    assert_eq!(*original, equal);
    let _ = original.grapheme_count();
    assert_eq!(*original, equal);
    assert_eq!(format!("{original:?}"), format!("{equal:?}"));
    let indices = original.indices.get().unwrap();
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
    let edited = Arc::clone(&document.paragraphs()[0]);
    assert!(edited.indices.get().is_none());
    document.set_kind(0..1, ParagraphKind::Body).unwrap();
    let retyped = document.paragraph(0).unwrap();
    assert!(Arc::ptr_eq(
        edited.indices.get().unwrap(),
        retyped.indices.get().unwrap()
    ));
    assert!(!Arc::ptr_eq(indices, retyped.indices.get().unwrap()));
    let _ = retyped.grapheme_count();
    assert!(edited.cached_grapheme_boundaries().is_some());
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
    let mut document = Document::from_text("café 👩\u{200d}💻");
    for _ in 0..3 {
        document
            .replace(document.end()..document.end(), "é", InlineStyle::default())
            .unwrap();
        let paragraph = document.paragraph(0).unwrap();
        assert_eq!(paragraph.spans().len(), 1);
        assert!(paragraph.indices.get().is_none());
    }
    assert_normalized(&document);
}

#[test]
fn rich_parts_validate_and_merge_runs() {
    let paragraph = Paragraph::new(
        "aéébc",
        vec![
            Span {
                range: 0..1,
                style: bold(),
                link: None,
            },
            Span {
                range: 1..3,
                style: bold(),
                link: None,
            },
            Span {
                range: 3..5,
                style: bold(),
                link: None,
            },
            Span {
                range: 5..6,
                style: InlineStyle::default(),
                link: None,
            },
            Span {
                range: 6..7,
                style: InlineStyle::default(),
                link: None,
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
                style: bold(),
                link: None,
            },
            Span {
                range: 5..7,
                style: InlineStyle::default(),
                link: None,
            }
        ]
    );
    assert!(Paragraph::new("", vec![], ParagraphKind::Body).is_ok());
    for level in [0, 7] {
        assert_eq!(
            Paragraph::new("", vec![], ParagraphKind::Heading { level }),
            Err(Error::InvalidHeadingLevel(level))
        );
    }
    for (text, spans, kind) in [
        (
            "a\nb",
            vec![Span {
                range: 0..3,
                style: bold(),
                link: None,
            }],
            ParagraphKind::Body,
        ),
        (
            "a\rb",
            vec![Span {
                range: 0..3,
                style: bold(),
                link: None,
            }],
            ParagraphKind::Body,
        ),
        ("a", vec![], ParagraphKind::Body),
        (
            "",
            vec![Span {
                range: 0..0,
                style: bold(),
                link: None,
            }],
            ParagraphKind::Body,
        ),
        (
            "ab",
            vec![Span {
                range: 0..1,
                style: bold(),
                link: None,
            }],
            ParagraphKind::Body,
        ),
        (
            "ab",
            vec![Span {
                range: 1..2,
                style: bold(),
                link: None,
            }],
            ParagraphKind::Body,
        ),
        (
            "ab",
            vec![Span {
                range: 0..3,
                style: bold(),
                link: None,
            }],
            ParagraphKind::Body,
        ),
        (
            "e\u{301}",
            vec![
                Span {
                    range: 0..1,
                    style: bold(),
                    link: None,
                },
                Span {
                    range: 1..3,
                    style: bold(),
                    link: None,
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
                    link: None,
                },
                Span {
                    range: 1..2,
                    style: bold(),
                    link: None,
                },
            ],
            ParagraphKind::Body,
        ),
    ] {
        let case = format!("{text:?} with {spans:?}");
        assert_eq!(
            Paragraph::new(text, spans, kind),
            Err(Error::InvalidFragment),
            "{case}"
        );
    }
}

#[test]
fn documents_assemble_from_validated_or_shared_paragraphs() {
    let heading = Paragraph::new(
        "Title",
        vec![Span {
            range: 0..5,
            style: bold(),
            link: None,
        }],
        ParagraphKind::Heading { level: 2 },
    )
    .unwrap();
    let item = Paragraph::new(
        "ité",
        vec![Span {
            range: 0..4,
            style: InlineStyle::default(),
            link: None,
        }],
        ParagraphKind::Ordered {
            indent: 1,
            start: Some(9),
        },
    )
    .unwrap();
    let document = Document::from_paragraphs([heading.clone(), item]);
    assert_eq!(document.plain_text(), "Title\nité");
    assert_eq!(*document.paragraph(0).unwrap(), heading);
    assert_eq!(
        document.paragraph(1).unwrap().kind(),
        ParagraphKind::Ordered {
            indent: 1,
            start: Some(9)
        }
    );
    assert_eq!(document.list_number(1), Some(9));
    assert_eq!(
        Document::from_bytes(&document.to_bytes()).unwrap(),
        document
    );
    let fragment = Fragment::from_document(&document);
    let shared = Document::from_paragraphs(fragment.paragraphs().iter().cloned());
    for (source, copy) in fragment.paragraphs().iter().zip(shared.paragraphs()) {
        assert!(Arc::ptr_eq(source, copy), "{source:?}");
    }
    let empty = Document::from_paragraphs(Vec::<Paragraph>::new());
    assert_eq!(empty, Document::new());
    assert_eq!(
        format!("{empty:?}"),
        "Document { paragraphs: [Paragraph { text: \"\", spans: [], kind: Body }], revision: 0, .. }"
    );
}

#[test]
fn rich_unicode_run_validation_keeps_navigation_indexes_lazy() {
    let paragraph = Paragraph::new(
        "ééé🦀",
        vec![
            Span {
                range: 0..2,
                style: bold(),
                link: None,
            },
            Span {
                range: 2..6,
                style: InlineStyle::default(),
                link: None,
            },
            Span {
                range: 6..10,
                style: bold(),
                link: None,
            },
        ],
        ParagraphKind::Body,
    )
    .unwrap();
    assert!(paragraph.indices.get().is_none());
    assert_eq!(paragraph.spans().len(), 3);
    assert_eq!(paragraph.grapheme_count(), 4);
}

#[test]
fn rich_run_ends_match_complete_segmentation_beyond_local_context() {
    let texts = [
        "café e\u{301}👨\u{200d}👩\u{200d}👧\u{200d}👦🇺🇸각나".to_owned(),
        format!("a👩{}\u{200d}💻z", "\u{301}".repeat(64)),
        format!("a{}z", "🇦".repeat(65)),
        format!("aक{}षz", "\u{94d}\u{301}".repeat(33)),
        format!("{}\u{1161}\u{11a8}é", "\u{1100}".repeat(40)),
    ];
    let styles = [bold(), InlineStyle::default(), bold()];
    let validate = |text: &str, ends: &[usize]| {
        let spans: Vec<Span> = [0]
            .into_iter()
            .chain(ends.iter().copied())
            .zip(ends.iter().copied().chain([text.len()]))
            .zip(styles)
            .map(|((start, end), style)| Span {
                range: start..end,
                style,
                link: None,
            })
            .collect();
        Paragraph::new(text, spans, ParagraphKind::Body).is_ok()
    };
    for text in texts {
        let boundaries: Vec<_> = text.grapheme_indices(true).map(|(byte, _)| byte).collect();
        for byte in 1..text.len() {
            assert_eq!(
                validate(&text, &[byte]),
                boundaries.contains(&byte),
                "run end {byte} in {text:?}"
            );
        }
        let chars: Vec<_> = text.char_indices().map(|(byte, _)| byte).skip(1).collect();
        for (index, &first) in chars.iter().enumerate() {
            for &second in &chars[index + 1..] {
                assert_eq!(
                    validate(&text, &[first, second]),
                    boundaries.contains(&first) && boundaries.contains(&second),
                    "run ends {first} and {second} in {text:?}"
                );
            }
        }
    }
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
    assert!(!document.set_kind(0..1, kind).unwrap().is_empty());
    assert!(!Arc::ptr_eq(&document.identity, &before_kind));
    let before_style = Arc::clone(&document.identity);
    assert!(document.set_kind(0..1, kind).unwrap().is_empty());
    assert!(Arc::ptr_eq(&document.identity, &before_style));
    let patch = StylePatch {
        bold: Some(true),
        ..StylePatch::default()
    };
    assert!(
        !document
            .apply_style(Position::default()..document.end(), patch)
            .unwrap()
            .is_empty()
    );
    assert!(!Arc::ptr_eq(&document.identity, &before_style));
    let styled = Arc::clone(&document.identity);
    assert!(
        document
            .apply_style(Position::default()..document.end(), patch)
            .unwrap()
            .is_empty()
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
            assert_eq!(span.range.start, previous_end, "{span:?} in {paragraph:?}");
            assert!(
                span.range.start < span.range.end,
                "{span:?} in {paragraph:?}"
            );
            assert!(
                paragraph.text().is_char_boundary(span.range.start),
                "{span:?} in {paragraph:?}"
            );
            assert!(
                paragraph.text().is_char_boundary(span.range.end),
                "{span:?} in {paragraph:?}"
            );
            assert_ne!(
                Some(span.style),
                previous_style,
                "{span:?} in {paragraph:?}"
            );
            let boundaries: Vec<_> = paragraph
                .text()
                .grapheme_indices(true)
                .map(|(index, _)| index)
                .chain([paragraph.text().len()])
                .collect();
            assert!(
                boundaries.contains(&span.range.start),
                "{span:?} in {paragraph:?}"
            );
            assert!(
                boundaries.contains(&span.range.end),
                "{span:?} in {paragraph:?}"
            );
            previous_end = span.range.end;
            previous_style = Some(span.style);
        }
        assert_eq!(previous_end, paragraph.text().len(), "{paragraph:?}");
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
    let document = Document::from_text("e\u{301}👨\u{200d}👩\u{200d}👧\u{200d}👦🇺🇸");
    for (byte, _) in document.paragraph(0).unwrap().text().grapheme_indices(true) {
        assert!(
            document.validate_position(Position::new(0, byte)).is_ok(),
            "byte {byte}"
        );
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
            .is_empty()
    );
    assert_eq!(document.revision(), revision);
    document
        .apply_style(Position::new(0, 0)..Position::new(0, 6), patch)
        .unwrap();
    assert_eq!(
        document.paragraph(0).unwrap().spans(),
        &[Span {
            range: 0..6,
            style: bold(),
            link: None,
        }]
    );
    assert_eq!(document.style_at(Position::new(0, 0)), bold());
    assert_eq!(document.style_at(document.end()), bold());
    assert_normalized(&document);
}

#[test]
fn span_lookup_skips_empty_ranges_and_runs_that_only_touch() {
    let mut document = Document::from_text("abcdef\nxy");
    document
        .apply_style(
            Position::new(0, 2)..Position::new(0, 4),
            StylePatch {
                bold: Some(true),
                ..StylePatch::default()
            },
        )
        .unwrap();
    let paragraph = &document.paragraphs()[0];
    assert_eq!(paragraph.overlapping(3..3), 0..0);
    assert_eq!(paragraph.overlapping(2..4), 1..2);
    assert_eq!(paragraph.overlapping(1..5), 0..3);
    assert_eq!(paragraph.overlapping(4..6), 2..3);
    let styles = |range: Range<Position>| -> Vec<InlineStyle> {
        document.spans_in(&range).map(|span| span.style).collect()
    };
    assert!(styles(Position::new(0, 6)..Position::new(1, 0)).is_empty());
    assert_eq!(
        styles(Position::new(0, 3)..Position::new(1, 1)),
        [bold(), InlineStyle::default(), InlineStyle::default()]
    );
}

#[test]
fn formatting_ranges_match_per_grapheme_styles_and_share_unchanged_data() {
    let red = Color([255, 0, 0, 255]);
    let blue = Color([0, 0, 255, 255]);
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
        InlineStyle {
            background: Some(Color::YELLOW),
            ..InlineStyle::default()
        },
    ];
    let texts = ["ab e\u{301}👩\u{200d}💻cd 🇺🇸ef", "", "क्\u{200d}ष xyz"];
    let paragraphs: Vec<Arc<Paragraph>> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let spans: Vec<Span> = text
                .grapheme_indices(true)
                .enumerate()
                .map(|(grapheme, (byte, text))| Span {
                    range: byte..byte + text.len(),
                    style: styles[(grapheme / 2 + index) % styles.len()],
                    link: None,
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
        .collect();
    let source = Document::from_paragraphs(paragraphs);
    let positions: Vec<_> = source
        .paragraphs()
        .iter()
        .enumerate()
        .flat_map(|(index, paragraph)| {
            let _ = paragraph.grapheme_count();
            let _ = paragraph.scalar_count();
            let _ = paragraph.previous_word(paragraph.text().len());
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
        StylePatch {
            background: Some(None),
            ..StylePatch::default()
        },
        StylePatch {
            background: Some(Some(Color::YELLOW)),
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
                background: Some(value.then_some(blue)),
            },
        ]);
    }
    for (first, &start) in positions.iter().enumerate() {
        for end in positions[first..].iter().copied() {
            for patch in patches.iter().copied() {
                let expected: Vec<_> = source
                    .paragraphs()
                    .iter()
                    .enumerate()
                    .map(|(index, paragraph)| {
                        let spans: Vec<Span> = paragraph
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
                                    link: None,
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
                assert_eq!(!delta.is_empty(), changed, "{start:?}..{end:?}, {patch:?}");
                assert_eq!(
                    document.paragraphs(),
                    expected,
                    "{start:?}..{end:?}, {patch:?}"
                );
                assert_eq!(
                    document.revision(),
                    u64::from(changed),
                    "{start:?}..{end:?}, {patch:?}"
                );
                assert_eq!(
                    Arc::ptr_eq(&document.identity, &source.identity),
                    !changed,
                    "{start:?}..{end:?}, {patch:?}"
                );
                for (before, after) in source.paragraphs().iter().zip(document.paragraphs()) {
                    assert!(
                        Arc::ptr_eq(&before.text, &after.text),
                        "{start:?}..{end:?}, {patch:?}"
                    );
                    match (before.indices.get(), after.indices.get()) {
                        (Some(before), Some(after)) => {
                            assert!(Arc::ptr_eq(before, after), "{start:?}..{end:?}, {patch:?}")
                        }
                        (None, None) => {}
                        _ => panic!("formatting changed the navigation cache"),
                    }
                    if before == after {
                        assert!(Arc::ptr_eq(before, after), "{start:?}..{end:?}, {patch:?}");
                    }
                }
                document.replay(&delta, false);
                assert_eq!(
                    document.paragraphs(),
                    source.paragraphs(),
                    "{start:?}..{end:?}, {patch:?}"
                );
                document.replay(&delta, true);
                assert_eq!(
                    document.paragraphs(),
                    expected,
                    "{start:?}..{end:?}, {patch:?}"
                );
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
    assert_eq!(document.plain_text(), "👩\u{200d}👧");
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
fn kind_changes_share_unchanged_paragraphs_inside_the_changed_range() {
    let bullet = ParagraphKind::Bullet { indent: 0 };
    let mut document = Document::from_paragraphs(
        [bullet, ParagraphKind::Body, bullet]
            .map(|kind| Arc::new(Paragraph::plain("item")).with_kind(kind)),
    );
    let middle = Arc::clone(&document.paragraphs()[1]);
    let delta = document.change_list_indent(0..3, true);
    assert_eq!((delta.start, delta.after.len()), (0, 3));
    assert!(Arc::ptr_eq(&middle, &document.paragraphs()[1]));
    let delta = document.set_kind(0..3, ParagraphKind::Body).unwrap();
    assert_eq!((delta.start, delta.after.len()), (0, 3));
    assert!(Arc::ptr_eq(&middle, &document.paragraphs()[1]));
}

#[test]
fn sparse_changes_retain_only_their_changed_runs() {
    let changed = [0, 3, 13, 14, 30, 39];
    let source: Vec<String> = (0..40)
        .map(|index| {
            if changed.contains(&index) {
                format!("match {index}")
            } else {
                format!("plain {index}")
            }
        })
        .collect();
    let mut document = Document::from_text(&source.join("\n"));
    let original = document.paragraphs.clone();
    let matches: Vec<_> = changed
        .iter()
        .map(|&paragraph| Position::new(paragraph, 0)..Position::new(paragraph, 5))
        .collect();
    let (delta, _) = document.replace_matches(&matches, "swap");
    let splices: Vec<_> = iter::once(&delta)
        .chain(&delta.rest)
        .map(|splice| (splice.start, splice.before.len(), splice.after.len()))
        .collect();
    assert_eq!(splices, [(0, 4, 4), (13, 2, 2), (30, 10, 10)]);
    let contiguous = Delta {
        start: 0,
        before: original.clone(),
        after: document.paragraphs.clone(),
        rest: Vec::new(),
    };
    assert_eq!(
        contiguous.retained_bytes() - delta.retained_bytes(),
        24 * (mem::size_of::<Paragraph>() + 2 * mem::size_of::<Arc<Paragraph>>())
            - 2 * mem::size_of::<Delta>()
    );
    let shared = |expected: &[Arc<Paragraph>], document: &Document| -> Vec<bool> {
        expected
            .iter()
            .zip(&document.paragraphs)
            .map(|(old, new)| Arc::ptr_eq(old, new))
            .collect()
    };
    let unchanged: Vec<_> = (0..40).map(|index| !changed.contains(&index)).collect();
    assert_eq!(shared(&original, &document), unchanged);
    let replaced = document.paragraphs.clone();
    let revision = document.revision();
    document.replay(&delta, false);
    assert_eq!(document.revision(), revision + 1);
    assert_eq!(shared(&original, &document), [true; 40]);
    document.replay(&delta, true);
    assert_eq!(shared(&replaced, &document), [true; 40]);
    assert_eq!(
        document.plain_text(),
        source.join("\n").replace("match", "swap")
    );
}

#[test]
fn sparse_changes_that_shift_paragraphs_retain_only_their_changed_runs() {
    let identical = |expected: &[Arc<Paragraph>], document: &Document| {
        expected.len() == document.paragraphs.len()
            && expected
                .iter()
                .zip(&document.paragraphs)
                .all(|(expected, paragraph)| Arc::ptr_eq(expected, paragraph))
    };
    let mut retained = Vec::new();
    for (first_gap, second_gap) in [(9, 15), (90, 150)] {
        let split = 2;
        let joined = split + 1 + first_gap;
        let expanded = joined + 3 + second_gap;
        let last = expanded + 1 + 8;
        let source: Vec<String> = (0..=last)
            .map(|index| format!("plain {index:03}"))
            .collect();
        let mut document = Document::from_text(&source.join("\n"));
        let original = document.paragraphs.clone();
        let matches = [
            Position::new(split, 0)..Position::new(split, 5),
            Position::new(joined, 6)..Position::new(joined + 2, 5),
            Position::new(expanded, 0)..Position::new(expanded, 5),
            Position::new(last, 0)..Position::new(last, 5),
        ];
        let (delta, _) = document.replace_matches(&matches, "x\ny");
        let splices: Vec<_> = iter::once(&delta)
            .chain(&delta.rest)
            .map(|splice| (splice.start, splice.before.len(), splice.after.len()))
            .collect();
        assert_eq!(
            splices,
            [(split, 1, 2), (joined + 1, 3, 2), (expanded, 10, 12)],
            "gaps {first_gap} and {second_gap}"
        );
        let mut expected = source.clone();
        expected[last].replace_range(..5, "x\ny");
        expected[expanded].replace_range(..5, "x\ny");
        expected[joined + 2].replace_range(..5, "y");
        expected.drain(joined + 1..joined + 2);
        expected[joined].replace_range(6.., "x");
        expected[split].replace_range(..5, "x\ny");
        assert_eq!(
            document.plain_text(),
            expected.join("\n"),
            "gaps {first_gap} and {second_gap}"
        );
        let kept = document
            .paragraphs
            .iter()
            .filter(|paragraph| original.iter().any(|old| Arc::ptr_eq(old, paragraph)))
            .count();
        assert_eq!(
            kept,
            original.len() - 6,
            "gaps {first_gap} and {second_gap}"
        );
        retained.push(delta.retained_bytes());
        let replaced = document.paragraphs.clone();
        let revision = document.revision();
        document.replay(&delta, false);
        assert_eq!(
            document.revision(),
            revision + 1,
            "gaps {first_gap} and {second_gap}"
        );
        assert!(
            identical(&original, &document),
            "gaps {first_gap} and {second_gap}"
        );
        document.replay(&delta, true);
        assert!(
            identical(&replaced, &document),
            "gaps {first_gap} and {second_gap}"
        );
    }
    assert_eq!(retained[0], retained[1]);
}

#[test]
fn shifting_sparse_changes_replay_exactly_with_repeated_paragraphs() {
    let mut seed = 0x5a1f_7ed0_c0de_d1ffu64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let repeated = Arc::new(Paragraph::plain("same"));
    let identical = |expected: &[Arc<Paragraph>], document: &Document| {
        expected.len() == document.paragraphs.len()
            && expected
                .iter()
                .zip(&document.paragraphs)
                .all(|(expected, paragraph)| Arc::ptr_eq(expected, paragraph))
    };
    let mut split = 0;
    for _ in 0..300 {
        let count = 12 + next(48);
        let mut document = Document::from_paragraphs((0..count).map(|index| {
            if next(4) == 0 {
                Arc::clone(&repeated)
            } else {
                Arc::new(Paragraph::plain(&format!("p{index}")))
            }
        }));
        let mut matches = Vec::new();
        let mut paragraph = next(4);
        while paragraph < count {
            let end = (paragraph + next(3)).min(count - 1);
            matches.push(Position::new(paragraph, 0)..Position::new(end, 1));
            paragraph = end + 1 + next(16);
        }
        if matches.is_empty() {
            continue;
        }
        let replacement = ["", "r", "r\ns", "r\n\ns"][next(4)];
        let offset = |position: Position| {
            document.paragraphs[..position.paragraph]
                .iter()
                .map(|paragraph| paragraph.text.len() + 1)
                .sum::<usize>()
                + position.byte
        };
        let mut expected = document.plain_text();
        for range in matches.iter().rev() {
            expected.replace_range(offset(range.start)..offset(range.end), replacement);
        }
        let original = document.paragraphs.clone();
        let (delta, _) = document.replace_matches(&matches, replacement);
        assert_eq!(
            document.plain_text(),
            expected,
            "{matches:?} {replacement:?}"
        );
        if delta
            .rest
            .iter()
            .any(|splice| splice.before.len() != splice.after.len())
        {
            split += 1;
        }
        let replaced = document.paragraphs.clone();
        document.replay(&delta, false);
        assert!(
            identical(&original, &document),
            "{matches:?} {replacement:?}"
        );
        document.replay(&delta, true);
        assert!(
            identical(&replaced, &document),
            "{matches:?} {replacement:?}"
        );
    }
    assert!(split > 50, "{split}");
}

#[test]
fn sparse_kind_changes_renumber_lists_through_history() {
    let ordered = ParagraphKind::Ordered {
        indent: 0,
        start: None,
    };
    let mut document = Document::from_text(&["item"; 30].join("\n"));
    document.set_kind(0..30, ordered).unwrap();
    for index in [3, 20] {
        document
            .set_kind(index..index + 1, ParagraphKind::Body)
            .unwrap();
    }
    let interrupted = numbers(&document);
    let delta = document.set_kind(0..30, ordered).unwrap();
    assert!(!delta.is_empty());
    assert_eq!(delta.rest.len(), 1);
    let expected: Vec<_> = (1..=30).map(Some).collect();
    assert_eq!(numbers(&document), expected);
    document.replay(&delta, false);
    assert_eq!(numbers(&document), interrupted);
    document.replay(&delta, true);
    assert_eq!(numbers(&document), expected);
}

#[test]
fn history_charges_the_larger_side_without_shared_text_or_runs() {
    let record = mem::size_of::<Paragraph>();
    let run = mem::size_of::<Span>();
    let pointer = mem::size_of::<Arc<Paragraph>>();
    let text = "word ".repeat(1000);
    let mut document = Document::from_text(&format!("{text}\n{text}"));
    let (typed, _) = document
        .replace(Position::new(0, 0)..Position::new(0, 0), "x", bold())
        .unwrap();
    assert_eq!(
        typed.retained_bytes(),
        record + text.len() + 1 + 2 * run + 2 * pointer
    );

    let formatted = document
        .apply_style(
            Position::new(0, 1)..Position::new(1, 4),
            StylePatch {
                italic: Some(true),
                ..StylePatch::default()
            },
        )
        .unwrap();
    assert_eq!(
        formatted.retained_bytes(),
        2 * record + 4 * run + 4 * pointer
    );

    let len = document.paragraphs().len();
    let retyped = document
        .set_kind(
            0..len,
            ParagraphKind::Ordered {
                indent: 0,
                start: None,
            },
        )
        .unwrap();
    assert_eq!(retyped.retained_bytes(), 2 * record + 4 * pointer);
}

#[test]
fn sparse_history_charges_the_pointers_and_record_of_each_run() {
    let record = mem::size_of::<Paragraph>();
    let pointer = mem::size_of::<Arc<Paragraph>>();
    let mut document = Document::from_text(&vec!["text"; 100].join("\n"));
    document.set_kind(0..100, ParagraphKind::Quote).unwrap();
    for index in (0..100).step_by(10) {
        document
            .set_kind(index..index + 1, ParagraphKind::Body)
            .unwrap();
    }
    let delta = document.set_kind(0..100, ParagraphKind::Quote).unwrap();
    assert_eq!(delta.rest.len(), 9);
    assert_eq!(
        delta.retained_bytes(),
        10 * (record + 2 * pointer) + 9 * mem::size_of::<Delta>()
    );
}

fn numbers(document: &Document) -> Vec<Option<u32>> {
    (0..document.paragraphs().len())
        .map(|index| document.list_number(index))
        .collect()
}

#[test]
fn numbering_cache_survives_equivalent_block_changes_and_replay() {
    let ordered = |start| ParagraphKind::Ordered { indent: 0, start };
    let mut document = Document::from_paragraphs(
        [
            ordered(Some(7)),
            ParagraphKind::Body,
            ordered(None),
            ParagraphKind::Heading { level: 1 },
            ordered(None),
        ]
        .map(|kind| Arc::new(Paragraph::plain("item")).with_kind(kind)),
    );
    let expected = [Some(7), None, Some(8), None, Some(1)];
    assert_eq!(numbers(&document), expected);
    let cached = Arc::clone(document.numbers.get().unwrap());
    let snapshot = document.clone();

    for (index, kind) in [
        (1, ParagraphKind::Quote),
        (1, ParagraphKind::Body),
        (3, ParagraphKind::Heading { level: 6 }),
        (3, ParagraphKind::Heading { level: 2 }),
    ] {
        let before = document.paragraphs[index].kind;
        let delta = document.set_kind(index..index + 1, kind).unwrap();
        assert!(!delta.is_empty(), "paragraph {index} to {kind:?}");
        assert!(
            Arc::ptr_eq(document.numbers.get().unwrap(), &cached),
            "paragraph {index} to {kind:?}"
        );
        assert_eq!(
            numbers(&document),
            expected,
            "paragraph {index} to {kind:?}"
        );
        document.replay(&delta, false);
        assert_eq!(
            document.paragraphs[index].kind, before,
            "paragraph {index} to {kind:?}"
        );
        assert!(
            Arc::ptr_eq(document.numbers.get().unwrap(), &cached),
            "paragraph {index} to {kind:?}"
        );
        document.replay(&delta, true);
        assert_eq!(
            document.paragraphs[index].kind, kind,
            "paragraph {index} to {kind:?}"
        );
        assert!(
            Arc::ptr_eq(document.numbers.get().unwrap(), &cached),
            "paragraph {index} to {kind:?}"
        );
    }

    let delta = document.set_kind(3..4, ParagraphKind::Quote).unwrap();
    assert!(document.numbers.get().is_none());
    assert_eq!(numbers(&document), [Some(7), None, Some(8), None, Some(9)]);
    document.replay(&delta, false);
    assert!(document.numbers.get().is_none());
    assert_eq!(numbers(&document), expected);
    document
        .set_kind(1..2, ParagraphKind::Heading { level: 1 })
        .unwrap();
    assert!(document.numbers.get().is_none());
    assert_eq!(numbers(&document), [Some(7), None, Some(1), None, Some(1)]);
    assert!(Arc::ptr_eq(snapshot.numbers.get().unwrap(), &cached));
    assert_eq!(numbers(&snapshot), expected);
}

#[test]
fn list_edits_share_following_items_and_text_edits_keep_computed_numbers() {
    let mut document = Document::from_text(&vec!["item"; 100].join("\n"));
    document
        .set_kind(
            0..100,
            ParagraphKind::Ordered {
                indent: 0,
                start: None,
            },
        )
        .unwrap();
    document
        .set_kind(
            10..11,
            ParagraphKind::Ordered {
                indent: 0,
                start: Some(1),
            },
        )
        .unwrap();
    let expected: Vec<_> = (1..=10).chain(1..=90).map(Some).collect();
    assert_eq!(numbers(&document), expected);
    let before = document.paragraphs.clone();
    let (delta, _) = document
        .replace(
            Position::new(0, 4)..Position::new(0, 4),
            "\n",
            InlineStyle::default(),
        )
        .unwrap();
    assert_eq!(
        (delta.start, delta.before.len(), delta.after.len()),
        (1, 0, 1)
    );
    let shifted = before.iter().zip(&document.paragraphs[1..]).skip(1);
    for (old, new) in shifted {
        assert!(Arc::ptr_eq(old, new), "{old:?}");
    }
    let expected: Vec<_> = (1..=11).chain(1..=90).map(Some).collect();
    assert_eq!(numbers(&document), expected);
    assert!(document.numbers.get().is_some());
    document
        .replace(
            Position::new(5, 0)..Position::new(5, 4),
            "text",
            InlineStyle::default(),
        )
        .unwrap();
    document
        .apply_style(
            Position::new(0, 0)..Position::new(3, 2),
            StylePatch {
                bold: Some(true),
                ..StylePatch::default()
            },
        )
        .unwrap();
    assert!(document.numbers.get().is_some());
    let clone = document.clone();
    document.set_kind(3..4, ParagraphKind::Body).unwrap();
    assert!(document.numbers.get().is_none());
    assert_eq!(document.list_number(11), Some(1));
    assert!(document.numbers.get().is_none());
    assert_eq!(document.list_number(10), Some(10));
    assert_eq!(numbers(&clone), expected);
    let (delta, _) = document
        .replace(
            Position::new(0, 4)..Position::new(1, 0),
            "",
            InlineStyle::default(),
        )
        .unwrap();
    assert_eq!(numbers(&document)[..4], [Some(1), Some(2), None, Some(3)]);
    document.replay(&delta, false);
    assert_eq!(
        numbers(&document)[..5],
        [Some(1), Some(2), Some(3), None, Some(4)]
    );
    assert_eq!(document.list_number(100), Some(90));
    assert_eq!(document.list_number(101), None);
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
                start: Some(7),
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
    for index in 2..4 {
        assert_eq!(
            document.paragraph(index).unwrap().kind(),
            ParagraphKind::Ordered {
                indent: 1,
                start: None
            },
            "paragraph {index}"
        );
    }
    assert_eq!(numbers(&document), [None, Some(7), Some(8), Some(9)]);
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
            .is_empty()
    );
    assert!(
        document
            .apply_style(Position::default()..document.end(), StylePatch::default())
            .unwrap()
            .is_empty()
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
    let invalid_caret = Position::new(0, 1)..Position::new(0, 1);
    assert!(matches!(
        document.replace_with_link(invalid_caret.clone(), "", bold(), None),
        Err(Error::InvalidPosition(_))
    ));
    assert!(matches!(
        document.replace_fragment(invalid_caret, &Fragment::from_text("")),
        Err(Error::InvalidPosition(_))
    ));
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
                        &super::normalize_newlines(insertion),
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
                    assert_eq!(
                        edited.plain_text(),
                        expected,
                        "{input:?}, {start:?}..{end:?}, {insertion:?}"
                    );
                    assert!(
                        edited.validate_position(caret).is_ok(),
                        "{input:?}, {start:?}..{end:?}, {insertion:?}"
                    );
                    assert_normalized(&edited);
                    let result = edited.paragraphs().to_vec();
                    edited.replay(&delta, false);
                    assert_eq!(
                        edited.paragraphs(),
                        original.paragraphs(),
                        "{input:?}, {start:?}..{end:?}, {insertion:?}"
                    );
                    edited.replay(&delta, true);
                    assert_eq!(
                        edited.paragraphs(),
                        result,
                        "{input:?}, {start:?}..{end:?}, {insertion:?}"
                    );
                }
            }
        }
    }
}
