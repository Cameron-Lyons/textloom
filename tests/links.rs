//! Tests links: target validation, how typing, formatting, replacement, and
//! paste treat linked text, caret link queries, undo history costs, and the
//! encoding of links in the native format and HTML export.

use std::collections::hash_map::DefaultHasher;
use std::ops::Range;

use textloom::{
    Color, Document, Editor, Error, Fragment, FragmentError, InlineStyle, Link, Movement,
    Paragraph, ParagraphKind, Position, SearchOptions, Selection, SelectionMode, Span, StylePatch,
};

fn link(target: &str) -> Link {
    Link::try_new(target).expect("callers pass short targets without control characters")
}

fn select(editor: &mut Editor, paragraph: usize, bytes: Range<usize>) {
    editor
        .set_selection(Selection::new(
            Position::new(paragraph, bytes.start),
            Position::new(paragraph, bytes.end),
        ))
        .expect("callers select between grapheme boundaries of an existing paragraph");
}

fn caret(editor: &mut Editor, paragraph: usize, byte: usize) {
    editor
        .set_selection(Selection::caret(Position::new(paragraph, byte)))
        .expect("callers place the caret on a grapheme boundary of an existing paragraph");
}

/// The text of one span and the target of its link, if any.
#[derive(Debug, PartialEq)]
struct Run {
    text: String,
    link: Option<String>,
}

/// Returns the spans of paragraph `index` as [`Run`]s.
fn runs(editor: &Editor, index: usize) -> Vec<Run> {
    let paragraph = editor
        .document()
        .paragraph(index)
        .expect("callers pass the index of an existing paragraph");
    paragraph
        .spans()
        .iter()
        .map(|span| Run {
            text: paragraph.text()[span.range.clone()].to_owned(),
            link: span.link.as_ref().map(ToString::to_string),
        })
        .collect()
}

fn run(text: &str, target: Option<&str>) -> Run {
    Run {
        text: text.to_owned(),
        link: target.map(str::to_owned),
    }
}

fn linked_editor() -> Editor {
    let mut editor = Editor::from_text("Visit  today");
    caret(&mut editor, 0, 6);
    editor
        .insert_link("example", link("https://example.com"))
        .expect("no IME composition is active");
    editor.clear_history();
    editor
}

fn hash(link: &Link) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    link.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn link_targets_are_validated_and_compare_by_text() {
    for invalid in ["", "a\0b", "tab\there", "line\nbreak", "\u{7f}", "\u{85}"] {
        assert_eq!(
            Link::try_new(invalid),
            Err(Error::InvalidLink),
            "{invalid:?}"
        );
    }
    for separator in ["\u{2028}", "\u{2029}"] {
        assert_eq!(
            Link::try_new(&format!("https://example.com/{separator}")),
            Err(Error::InvalidLink),
            "{separator:?}"
        );
    }
    let longest = "a".repeat(Link::MAX_LEN);
    assert_eq!(Link::try_new(&longest).unwrap().as_str().len(), 64 * 1024);
    assert_eq!(
        Link::try_new(&format!("{longest}a")),
        Err(Error::InvalidLink)
    );

    let unicode = link("https://例え.jp/パス?q=café#👩\u{200d}💻");
    assert_eq!(unicode.as_str(), "https://例え.jp/パス?q=café#👩\u{200d}💻");
    assert_eq!(unicode.to_string(), unicode.as_str());
    assert_eq!(format!("{:?}", link("mailto:a@b")), "Link(\"mailto:a@b\")");
    let separate = link(unicode.as_str());
    assert_eq!(separate, unicode);
    assert_eq!(hash(&separate), hash(&unicode));
    assert_ne!(link("https://a"), link("https://b"));
    assert!(!Error::InvalidLink.to_string().is_empty());
}

#[test]
fn runs_merge_only_when_their_style_and_link_agree() {
    let bold = InlineStyle {
        bold: true,
        ..InlineStyle::default()
    };
    let span = |range, style, target: Option<&str>| Span {
        range,
        style,
        link: target.map(link),
    };
    let paragraph = Paragraph::try_new(
        "abcdefgh",
        vec![
            span(0..1, InlineStyle::default(), Some("https://a")),
            span(1..2, InlineStyle::default(), Some("https://a")),
            span(2..3, bold, Some("https://a")),
            span(3..4, bold, Some("https://b")),
            span(4..5, bold, None),
            span(5..6, bold, None),
            span(6..8, InlineStyle::default(), Some("https://a")),
        ],
        ParagraphKind::Body,
    )
    .unwrap();
    let merged: Vec<_> = paragraph
        .spans()
        .iter()
        .map(|span| (span.range.clone(), span.link.as_ref().map(Link::as_str)))
        .collect();
    assert_eq!(
        merged,
        [
            (0..2, Some("https://a")),
            (2..3, Some("https://a")),
            (3..4, Some("https://b")),
            (4..6, None),
            (6..8, Some("https://a")),
        ]
    );
    let document = Document::from_paragraphs([paragraph]);
    assert_eq!(
        document.link_range_at(Position::new(0, 2)),
        Some(Position::new(0, 0)..Position::new(0, 3))
    );
    assert_eq!(
        document.link_range_at(Position::new(0, 3)),
        Some(Position::new(0, 3)..Position::new(0, 4))
    );
    assert_eq!(document.link_range_at(Position::new(0, 4)), None);
    assert_eq!(
        document.link_at(Position::new(0, 7)),
        Some(&link("https://a"))
    );
    for missing in [
        Position::new(0, 8),
        Position::new(0, 9),
        Position::new(1, 0),
    ] {
        assert_eq!(document.link_at(missing), None, "{missing:?}");
        assert_eq!(document.link_range_at(missing), None, "{missing:?}");
    }
    let emoji = Document::from_paragraphs([Paragraph::try_new(
        "👩\u{200d}💻",
        vec![span(0..11, InlineStyle::default(), Some("https://a"))],
        ParagraphKind::Body,
    )
    .unwrap()]);
    assert_eq!(emoji.link_at(Position::new(0, 0)), Some(&link("https://a")));
    assert_eq!(emoji.link_at(Position::new(0, 4)), None);
    assert_eq!(emoji.link_range_at(Position::new(0, 4)), None);
}

#[test]
fn typing_inside_a_link_extends_it_but_not_at_its_edges() {
    let target = Some("https://example.com");
    let mut editor = linked_editor();
    assert_eq!(
        runs(&editor, 0),
        [
            run("Visit ", None),
            run("example", target),
            run(" today", None)
        ]
    );
    caret(&mut editor, 0, 9);
    editor.insert_text("XY").unwrap();
    editor.update_composition("日本", None).unwrap();
    editor.commit_composition("日本").unwrap();
    assert_eq!(
        runs(&editor, 0),
        [
            run("Visit ", None),
            run("exaXY日本mple", target),
            run(" today", None)
        ]
    );
    let mut editor = linked_editor();
    caret(&mut editor, 0, 13);
    editor.insert_text("!").unwrap();
    caret(&mut editor, 0, 6);
    editor.insert_text("<").unwrap();
    assert_eq!(
        runs(&editor, 0),
        [
            run("Visit <", None),
            run("example", target),
            run("! today", None)
        ]
    );
    let mut editor = linked_editor();
    select(&mut editor, 0, 8..11);
    editor.insert_text("ZZ").unwrap();
    assert_eq!(runs(&editor, 0)[1], run("exZZle", target));
    select(&mut editor, 0, 6..12);
    editor.insert_text("site").unwrap();
    assert_eq!(runs(&editor, 0), [run("Visit site today", None)]);
    let mut editor = linked_editor();
    caret(&mut editor, 0, 9);
    editor.insert_paragraph().unwrap();
    assert_eq!(runs(&editor, 0), [run("Visit ", None), run("exa", target)]);
    assert_eq!(runs(&editor, 1), [run("mple", target), run(" today", None)]);
    let mut editor = Editor::from_text("");
    editor.insert_link("a", link("https://a")).unwrap();
    editor.insert_link("b", link("https://b")).unwrap();
    caret(&mut editor, 0, 1);
    editor.insert_text("-").unwrap();
    assert_eq!(
        runs(&editor, 0),
        [
            run("a", Some("https://a")),
            run("-", None),
            run("b", Some("https://b"))
        ]
    );
}

#[test]
fn set_link_links_edits_and_removes_text_as_one_undo_step_each() {
    let first = link("https://first.example");
    let second = link("https://second.example");
    let mut editor = Editor::from_text("one two\nthree");
    let selection = Selection::new(Position::new(1, 3), Position::new(0, 4));
    editor.set_selection(selection).unwrap();
    editor.set_link(Some(first.clone())).unwrap();
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.undo_len(), 1);
    assert_eq!(
        runs(&editor, 0),
        [run("one ", None), run("two", Some(first.as_str()))]
    );
    assert_eq!(
        runs(&editor, 1),
        [run("thr", Some(first.as_str())), run("ee", None)]
    );
    assert_eq!(editor.selected_link(), Some(first.clone()));

    for byte in [4, 5, 7] {
        caret(&mut editor, 0, byte);
        assert_eq!(editor.selected_link(), Some(first.clone()), "byte {byte}");
    }
    caret(&mut editor, 0, 5);
    editor.set_link(Some(second.clone())).unwrap();
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 5)));
    assert_eq!(runs(&editor, 0)[1], run("two", Some(second.as_str())));
    assert_eq!(runs(&editor, 1)[0], run("thr", Some(first.as_str())));
    caret(&mut editor, 1, 3);
    editor.set_link(None).unwrap();
    assert_eq!(runs(&editor, 1), [run("three", None)]);
    assert_eq!(editor.undo_len(), 3);

    caret(&mut editor, 0, 2);
    let revision = editor.document().revision();
    editor.set_link(Some(first.clone())).unwrap();
    editor.set_link(None).unwrap();
    assert_eq!(editor.document().revision(), revision);
    assert_eq!(editor.undo_len(), 3);
    select(&mut editor, 0, 4..7);
    editor.set_link(Some(second.clone())).unwrap();
    assert_eq!(editor.undo_len(), 3);

    assert!(editor.undo());
    assert_eq!(runs(&editor, 1)[0], run("thr", Some(first.as_str())));
    assert!(editor.undo());
    assert_eq!(runs(&editor, 0)[1], run("two", Some(first.as_str())));
    assert!(editor.undo());
    assert_eq!(runs(&editor, 0), [run("one two", None)]);
    assert_eq!(editor.selection(), selection);
    assert!(editor.redo());
    assert_eq!(editor.selected_link(), Some(first.clone()));

    editor.update_composition("x", None).unwrap();
    assert_eq!(editor.set_link(None), Err(Error::CompositionActive));
    assert_eq!(
        editor.insert_link("x", first.clone()),
        Err(Error::CompositionActive)
    );
    assert_eq!(runs(&editor, 0)[1], run("two", Some(first.as_str())));
}

#[test]
fn set_link_replaces_every_link_in_a_selection_and_keeps_formatting() {
    let mut editor = Editor::from_text("abcdef");
    select(&mut editor, 0, 0..2);
    editor.set_link(Some(link("https://a"))).unwrap();
    select(&mut editor, 0, 1..4);
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..StylePatch::default()
        })
        .unwrap();
    select(&mut editor, 0, 4..6);
    editor.set_link(Some(link("https://b"))).unwrap();
    editor.select_all();
    assert_eq!(editor.selected_link(), None);
    editor.set_link(Some(link("https://c"))).unwrap();
    assert_eq!(editor.selected_link(), Some(link("https://c")));
    let paragraph = editor.document().paragraph(0).unwrap();
    let bold: Vec<_> = paragraph
        .spans()
        .iter()
        .map(|span| span.style.bold)
        .collect();
    assert_eq!(bold, [false, true, false]);
    assert_eq!(
        editor.document().link_range_at(Position::new(0, 5)),
        Some(Position::new(0, 0)..Position::new(0, 6))
    );
}

#[test]
fn insert_link_replaces_the_selection_and_defaults_to_its_target() {
    let target = link("https://example.com/a?b=1&c=2");
    let mut editor = Editor::from_text("see here now");
    select(&mut editor, 0, 4..8);
    editor
        .apply_style(StylePatch {
            italic: Some(true),
            ..StylePatch::default()
        })
        .unwrap();
    select(&mut editor, 0, 4..8);
    editor.insert_text("t").unwrap();
    let undo = editor.undo_len();
    editor.insert_link("docs", target.clone()).unwrap();
    assert_eq!(editor.document().plain_text(), "see tdocs now");
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 9)));
    assert_eq!(editor.undo_len(), undo + 1);
    let paragraph = editor.document().paragraph(0).unwrap();
    let span = &paragraph.spans()[2];
    assert_eq!(&paragraph.text()[span.range.clone()], "docs");
    assert!(span.style.italic);
    assert_eq!(
        editor.document().link_at(Position::new(0, 5)),
        Some(&target)
    );
    assert_eq!(editor.document().link_at(Position::new(0, 4)), None);
    editor.insert_text("!").unwrap();
    assert_eq!(editor.document().link_at(Position::new(0, 9)), None);
    assert_eq!(editor.undo_len(), undo + 2);
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "see t now");

    let mut editor = Editor::from_text("");
    editor.insert_link("", target.clone()).unwrap();
    assert_eq!(editor.document().plain_text(), target.as_str());
    let mut editor = Editor::from_text("x");
    editor.select_all();
    editor.insert_link("two\nlines", target.clone()).unwrap();
    assert_eq!(editor.document().paragraphs().len(), 2);
    for index in 0..2 {
        assert_eq!(
            editor.document().link_at(Position::new(index, 0)),
            Some(&target),
            "paragraph {index}"
        );
    }
    assert_eq!(editor.undo_len(), 1);
}

#[test]
fn caret_link_queries_and_edits_agree_at_styled_unicode_boundaries() {
    let first = link("https://first.example");
    let second = link("https://second.example");
    let replacement = link("https://replacement.example");
    let spans = [
        (0..2, false, Some(first.clone())),
        (2..13, true, Some(first.clone())),
        (13..14, false, Some(second.clone())),
        (14..15, true, Some(second.clone())),
        (15..17, false, None),
        (17..18, false, Some(first.clone())),
        (18..19, true, Some(first.clone())),
    ]
    .map(|(range, bold, link)| Span {
        range,
        style: InlineStyle {
            bold,
            ..InlineStyle::default()
        },
        link,
    });
    let document = Document::from_paragraphs([
        Paragraph::try_new("é👩\u{200d}💻ab  cd", spans.to_vec(), ParagraphKind::Body).unwrap(),
        Paragraph::try_new(
            "next",
            vec![Span {
                range: 0..4,
                style: InlineStyle::default(),
                link: None,
            }],
            ParagraphKind::Body,
        )
        .unwrap(),
    ]);
    let original = document.to_bytes();
    for (byte, target, range) in [
        (0, Some(&first), Some(0..13)),
        (2, Some(&first), Some(0..13)),
        (13, Some(&first), Some(0..13)),
        (14, Some(&second), Some(13..15)),
        (15, Some(&second), Some(13..15)),
        (16, None, None),
        (17, Some(&first), Some(17..19)),
        (18, Some(&first), Some(17..19)),
        (19, Some(&first), Some(17..19)),
    ] {
        let mut editor = Editor::new(document.clone());
        caret(&mut editor, 0, byte);
        let selection = editor.selection();
        assert_eq!(editor.selected_link().as_ref(), target, "byte {byte}");
        editor.set_link(Some(replacement.clone())).unwrap();
        assert_eq!(editor.selection(), selection, "byte {byte}");
        let changed = editor.document().to_bytes();
        assert_eq!(
            editor.document().paragraph(0).unwrap().spans().len(),
            spans.len(),
            "byte {byte}"
        );
        for (before, after) in spans
            .iter()
            .zip(editor.document().paragraph(0).unwrap().spans())
        {
            assert_eq!(after.range, before.range, "byte {byte}");
            assert_eq!(after.style, before.style, "byte {byte}");
            let expected = if range
                .as_ref()
                .is_some_and(|range| range.contains(&before.range.start))
            {
                Some(&replacement)
            } else {
                before.link.as_ref()
            };
            assert_eq!(after.link.as_ref(), expected, "byte {byte}");
        }
        assert_eq!(editor.undo(), range.is_some(), "byte {byte}");
        assert_eq!(editor.document().to_bytes(), original, "byte {byte}");
        assert_eq!(editor.selection(), selection, "byte {byte}");
        assert_eq!(editor.selected_link().as_ref(), target, "byte {byte}");
        if range.is_some() {
            assert!(editor.redo(), "byte {byte}");
            assert_eq!(editor.document().to_bytes(), changed, "byte {byte}");
        }
    }
    let mut editor = Editor::new(document);
    caret(&mut editor, 1, 0);
    assert_eq!(editor.selected_link(), None);
    editor.set_link(None).unwrap();
    assert_eq!(editor.document().to_bytes(), original);
    assert!(!editor.can_undo());
}

#[test]
fn selected_link_requires_one_link_across_the_selected_text() {
    let mut editor = linked_editor();
    let target = link("https://example.com");
    select(&mut editor, 0, 7..10);
    assert_eq!(editor.selected_link(), Some(target.clone()));
    select(&mut editor, 0, 5..10);
    assert_eq!(editor.selected_link(), None);
    caret(&mut editor, 0, 19);
    editor.insert_paragraph().unwrap();
    editor
        .set_selection(Selection::new(Position::new(0, 6), Position::new(1, 0)))
        .unwrap();
    assert_eq!(editor.selected_link(), None);
    editor
        .set_selection(Selection::new(Position::new(0, 13), Position::new(1, 0)))
        .unwrap();
    assert_eq!(editor.selected_link(), None);
    editor
        .set_selection(Selection::new(Position::new(0, 7), Position::new(0, 13)))
        .unwrap();
    assert_eq!(editor.selected_link(), Some(target));
    caret(&mut editor, 0, 2);
    assert_eq!(editor.selected_link(), None);
}

#[test]
fn clearing_formatting_keeps_links_and_rich_paste_carries_them() {
    let target = link("https://example.com");
    let mut editor = linked_editor();
    editor.select_all();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            foreground: Some(Some(Color::RED)),
            ..StylePatch::default()
        })
        .unwrap();
    editor.clear_formatting().unwrap();
    assert_eq!(
        runs(&editor, 0),
        [
            run("Visit ", None),
            run("example", Some(target.as_str())),
            run(" today", None)
        ]
    );
    assert_eq!(editor.selection_style().bold, Some(false));

    select(&mut editor, 0, 4..9);
    let fragment = editor.selected_fragment();
    assert_eq!(fragment.plain_text(), "t exa");
    let mut destination = Editor::from_text("[]");
    caret(&mut destination, 0, 1);
    destination.insert_fragment(&fragment).unwrap();
    assert_eq!(
        runs(&destination, 0),
        [
            run("[t ", None),
            run("exa", Some(target.as_str())),
            run("]", None)
        ]
    );
    assert_eq!(destination.undo_len(), 1);
    assert!(destination.undo());
    assert_eq!(runs(&destination, 0), [run("[]", None)]);
    assert!(destination.redo());
    assert_eq!(
        destination.document().link_at(Position::new(0, 3)),
        Some(&target)
    );

    assert_eq!(destination.document().plain_text(), "[t exa]");
    assert_eq!(destination.selected_text(), "");
    destination.select_all();
    assert_eq!(destination.selected_text(), "[t exa]");
}

#[test]
fn replacing_matches_keeps_the_link_where_each_match_begins() {
    let target = Some("https://example.com");
    let mut editor = linked_editor();
    let options = SearchOptions::default();
    assert_eq!(editor.replace_all("amp", "AMP", options).unwrap(), 1);
    assert_eq!(runs(&editor, 0)[1], run("exAMPle", target));
    assert_eq!(editor.replace_all("le to", "LE TO", options).unwrap(), 1);
    assert_eq!(
        runs(&editor, 0),
        [
            run("Visit ", None),
            run("exAMPLE TO", target),
            run("day", None)
        ]
    );
    assert_eq!(editor.replace_all("t ex", "t-ex", options).unwrap(), 1);
    assert_eq!(runs(&editor, 0)[0], run("Visit-ex", None));
    let mut editor = Editor::from_text("a\nb");
    select(&mut editor, 0, 0..1);
    editor.set_link(Some(link("https://a"))).unwrap();
    assert_eq!(editor.replace_all("\n", " ", options).unwrap(), 1);
    assert_eq!(
        runs(&editor, 0),
        [run("a", Some("https://a")), run(" b", None)]
    );
}

#[test]
fn history_charges_new_link_targets_once_per_paragraph() {
    let long = link(&format!("https://example.com/{}", "x".repeat(4000)));
    let mut editor = Editor::from_text("one two three");
    let before = editor.history_bytes();
    editor.select_all();
    editor.set_link(Some(link("https://a"))).unwrap();
    let short = editor.history_bytes() - before;
    editor.undo();
    editor.clear_history();
    editor.select_all();
    editor.set_link(Some(long.clone())).unwrap();
    let charged = editor.history_bytes();
    assert!(charged >= short + 4000, "{charged} after {short}");
    assert!(charged < short + 8000, "{charged} after {short}");

    let after_link = editor.history_bytes();
    select(&mut editor, 0, 4..7);
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..StylePatch::default()
        })
        .unwrap();
    assert!(editor.history_bytes() - after_link < 1000);
    caret(&mut editor, 0, 2);
    let before_typing = editor.history_bytes();
    editor.insert_text("x").unwrap();
    assert!(editor.history_bytes() - before_typing < 1000);
}

#[test]
fn native_format_encodes_links_after_fills_and_rejects_invalid_targets() {
    let mut editor = Editor::from_text("ab");
    select(&mut editor, 0, 1..2);
    editor
        .apply_style(StylePatch {
            foreground: Some(Some(Color([1, 2, 3, 4]))),
            background: Some(Some(Color([5, 6, 7, 8]))),
            ..StylePatch::default()
        })
        .unwrap();
    editor.set_link(Some(link("https://é"))).unwrap();
    let bytes = editor.document().to_bytes();
    let mut expected = b"TLFR\x02".to_vec();
    expected.extend_from_slice(&1u64.to_le_bytes());
    expected.push(0);
    expected.extend_from_slice(&2u64.to_le_bytes());
    expected.extend_from_slice(b"ab");
    expected.extend_from_slice(&2u64.to_le_bytes());
    expected.extend_from_slice(&1u64.to_le_bytes());
    expected.push(0);
    expected.extend_from_slice(&2u64.to_le_bytes());
    expected.push(0xe0);
    expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    expected.extend_from_slice(&("https://é".len() as u64).to_le_bytes());
    expected.extend_from_slice("https://é".as_bytes());
    assert_eq!(bytes, expected);
    let decoded = Document::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.to_bytes(), bytes);
    assert_eq!(
        decoded.link_at(Position::new(0, 1)),
        Some(&link("https://é"))
    );

    let encode = |spans: &[(u64, u8, &[u8])]| {
        let mut output = b"TLFR\x02".to_vec();
        output.extend_from_slice(&1u64.to_le_bytes());
        output.push(0);
        output.extend_from_slice(&2u64.to_le_bytes());
        output.extend_from_slice(b"ab");
        output.extend_from_slice(&(spans.len() as u64).to_le_bytes());
        for (end, flags, target) in spans {
            output.extend_from_slice(&end.to_le_bytes());
            output.push(*flags);
            if flags & 0x80 != 0 {
                output.extend_from_slice(&(target.len() as u64).to_le_bytes());
                output.extend_from_slice(target);
            }
        }
        output
    };
    let valid = encode(&[(1, 0x80, b"https://a"), (2, 0x81, b"https://a")]);
    let fragment = Fragment::from_bytes(&valid).unwrap();
    assert_eq!(fragment.to_bytes(), valid);
    assert!(
        Fragment::from_bytes(&encode(&[(1, 0x80, b"https://a"), (2, 0x80, b"https://b")])).is_ok()
    );
    assert_eq!(
        Fragment::from_bytes(&encode(&[(1, 0x80, b"https://a"), (2, 0x80, b"https://a")])),
        Err(FragmentError::InvalidSpans)
    );
    let too_long = vec![b'a'; Link::MAX_LEN + 1];
    for target in [
        &b""[..],
        b"a\nb",
        b"\xff\xfe",
        "a\u{2028}".as_bytes(),
        &too_long,
    ] {
        assert_eq!(
            Fragment::from_bytes(&encode(&[(2, 0x80, target)])),
            Err(FragmentError::InvalidLink),
            "{target:?}"
        );
    }
    let longest = vec![b'a'; Link::MAX_LEN];
    assert!(Fragment::from_bytes(&encode(&[(2, 0x80, &longest)])).is_ok());
    let mut truncated = encode(&[(2, 0x80, b"https://a")]);
    truncated.pop();
    assert_eq!(
        Fragment::from_bytes(&truncated),
        Err(FragmentError::Truncated)
    );
    let mut version_one = encode(&[(2, 0x80, b"https://a")]);
    version_one[4] = 1;
    assert_eq!(
        Fragment::from_bytes(&version_one),
        Err(FragmentError::InvalidStyle)
    );
    assert!(!FragmentError::InvalidLink.to_string().is_empty());
}

#[test]
fn html_wraps_each_link_once_and_drops_script_and_data_targets() {
    let mut editor = Editor::from_text("go here\u{2028}now or there");
    select(&mut editor, 0, 3..13);
    editor
        .set_link(Some(link("https://example.com/?a=1&b=\"2\"<'>")))
        .unwrap();
    select(&mut editor, 0, 5..11);
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..StylePatch::default()
        })
        .unwrap();
    select(&mut editor, 0, 17..22);
    editor
        .set_link(Some(link("mailto:me@example.com")))
        .unwrap();
    assert_eq!(
        editor.document().to_html(),
        "<div style=\"white-space:pre-wrap\"><p>go \
         <a href=\"https://example.com/?a=1&amp;b=&quot;2&quot;&lt;&#39;&gt;\">he\
         <strong>re<br>n</strong>ow</a> or \
         <a href=\"mailto:me@example.com\">there</a></p></div>"
    );
    for target in [
        "javascript:alert(1)",
        "  JavaScript:alert(1)",
        "VBSCRIPT:msgbox",
        "data:text/html,<b>x</b>",
        "\u{3000}DATA:,x",
    ] {
        let mut editor = Editor::from_text("click");
        editor.select_all();
        editor.set_link(Some(link(target))).unwrap();
        assert_eq!(
            editor.document().to_html(),
            "<div style=\"white-space:pre-wrap\"><p>click</p></div>",
            "{target}"
        );
    }
    for target in ["javascript", "data", "./data:x", "https://javascript:x"] {
        let mut editor = Editor::from_text("click");
        editor.select_all();
        editor.set_link(Some(link(target))).unwrap();
        assert!(editor.document().to_html().contains("<a href="), "{target}");
    }
    let mut editor = Editor::from_text("ab\u{2028}");
    editor.select_all();
    editor.set_link(Some(link("https://a"))).unwrap();
    select(&mut editor, 0, 1..2);
    editor.set_link(Some(link("https://b"))).unwrap();
    assert_eq!(
        Fragment::from_document(editor.document()).to_html(),
        "<div style=\"white-space:pre-wrap\"><p><a href=\"https://a\">a</a>\
         <a href=\"https://b\">b</a><a href=\"https://a\"><br></a><br></p></div>"
    );
}

#[test]
fn joined_graphemes_take_the_link_of_their_first_scalar() {
    let target = Some("https://example.com");
    let mut editor = linked_editor();
    caret(&mut editor, 0, 13);
    editor.insert_text("\u{301}").unwrap();
    assert_eq!(
        runs(&editor, 0),
        [
            run("Visit ", None),
            run("example\u{301}", target),
            run(" today", None)
        ]
    );
    assert_eq!(editor.selection().focus, Position::new(0, 15));
    let mut destination = Editor::from_text("\u{301}x");
    let fragment = Fragment::from_document(editor.document());
    destination.insert_fragment(&fragment).unwrap();
    let paragraph = destination.document().paragraph(0).unwrap();
    assert_eq!(paragraph.text(), "Visit example\u{301} today\u{301}x");
    assert_eq!(
        destination.document().link_range_at(Position::new(0, 6)),
        Some(Position::new(0, 6)..Position::new(0, 15))
    );
}

#[test]
fn history_charges_a_link_shared_by_many_paragraphs_once() {
    let target = format!("https://example.com/{}", "x".repeat(Link::MAX_LEN - 20));
    let long = link(&target);
    let mut editor = Editor::from_text(&vec!["word"; 200].join("\n"));
    editor
        .move_cursor(Movement::DocumentEnd, SelectionMode::Move)
        .unwrap();
    editor.insert_text(" earlier").unwrap();
    editor.select_all();
    editor.set_link(Some(long)).unwrap();
    assert_eq!(editor.undo_len(), 2);
    assert!(editor.history_bytes() < Link::MAX_LEN + 200 * 1024);
    assert!(editor.history_bytes() >= Link::MAX_LEN);
    assert!(editor.undo());
    assert_eq!(editor.document().link_at(Position::new(0, 0)), None);
}
