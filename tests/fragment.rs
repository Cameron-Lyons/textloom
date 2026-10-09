//! Rich clipboard, persisted-format compatibility, and HTML export regressions.

use std::sync::Arc;

use textloom::{
    Color, Document, Editor, Error, Fragment, FragmentError, ParagraphKind, Position, Selection,
    StylePatch,
};

fn select_paragraph(editor: &mut Editor, index: usize) {
    let end = editor.document().paragraph(index).unwrap().text().len();
    editor
        .set_selection(Selection::new(
            Position::new(index, 0),
            Position::new(index, end),
        ))
        .unwrap();
}

fn rich_fragment() -> Fragment {
    let mut editor = Editor::from_text("<Hello> & \"世界\" e\u{301}\nitem\nnested\nlast\n");
    select_paragraph(&mut editor, 0);
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            italic: Some(true),
            underline: Some(true),
            strikethrough: Some(true),
            code: Some(true),
            foreground: Some(Some(Color([0, 128, 255, 127]))),
        })
        .unwrap();
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 7)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(false),
            ..Default::default()
        })
        .unwrap();
    editor
        .set_paragraph_kind(ParagraphKind::Heading { level: 3 })
        .unwrap();
    for (index, kind) in [
        (1, ParagraphKind::Bullet { indent: 0 }),
        (
            2,
            ParagraphKind::Ordered {
                indent: 2,
                start: 17,
            },
        ),
        (
            3,
            ParagraphKind::Ordered {
                indent: 2,
                start: u32::MAX,
            },
        ),
    ] {
        select_paragraph(&mut editor, index);
        editor.set_paragraph_kind(kind).unwrap();
    }
    Fragment::from_document(editor.document())
}

#[test]
fn fragment_shares_paragraphs_and_plain_import_normalizes_line_breaks() {
    let document = Document::from_text("a\r\nb\rc\n");
    let fragment = Fragment::from_document(&document);
    for (source, copied) in document.paragraphs().iter().zip(fragment.paragraphs()) {
        assert!(Arc::ptr_eq(source, copied));
    }
    assert_eq!(fragment.plain_text(), "a\nb\nc\n");
    assert_eq!(fragment, Fragment::from_text("a\nb\nc\n"));
    assert_eq!(Fragment::default().paragraphs().len(), 1);
    assert_eq!(Fragment::default().plain_text(), "");
}

#[test]
fn native_roundtrip_preserves_text_all_styles_paragraph_kinds_and_empty_edges() {
    for fragment in [
        Fragment::default(),
        Fragment::from_text("\n\n"),
        rich_fragment(),
    ] {
        let bytes = fragment.to_bytes();
        assert_eq!(&bytes[..5], b"TLFR\x01");
        let decoded = Fragment::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, fragment);
        assert_eq!(decoded.to_bytes(), bytes);
    }
}

#[test]
fn every_truncated_prefix_is_rejected_and_trailing_bytes_are_rejected() {
    let bytes = rich_fragment().to_bytes();
    for length in 0..bytes.len() {
        assert!(
            Fragment::from_bytes(&bytes[..length]).is_err(),
            "prefix {length}"
        );
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert_eq!(
        Fragment::from_bytes(&trailing),
        Err(FragmentError::TrailingData)
    );
}

#[test]
fn document_interchange_and_rich_paste_preserve_metadata_in_one_undo_step() {
    let fragment = rich_fragment();
    let document = Document::from_fragment(&fragment);
    assert_eq!(Fragment::from_document(&document), fragment);
    for (source, copied) in fragment.paragraphs().iter().zip(document.paragraphs()) {
        assert!(Arc::ptr_eq(source, copied));
    }
    let mut editor = Editor::default();
    editor.insert_fragment(&fragment).unwrap();
    assert_eq!(Fragment::from_document(editor.document()), fragment);
    assert_eq!(editor.selection().focus, editor.document().end());
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "");
    assert_eq!(editor.selection(), Selection::default());
    assert!(editor.redo());
    assert_eq!(Fragment::from_document(editor.document()), fragment);
}

#[test]
fn selected_fragment_clips_styles_preserves_kinds_and_shares_complete_paragraphs() {
    let mut editor = Editor::new(Document::from_fragment(&rich_fragment()));
    editor
        .set_selection(Selection::new(Position::new(2, 6), Position::new(0, 3)))
        .unwrap();
    let fragment = editor.selected_fragment();
    assert_eq!(
        fragment.plain_text(),
        "llo> & \"世界\" e\u{301}\nitem\nnested"
    );
    assert_eq!(
        fragment.paragraphs()[0].kind(),
        ParagraphKind::Heading { level: 3 }
    );
    let first = fragment.paragraphs()[0].spans();
    assert_eq!(first[0].range, 0..4);
    assert!(!first[0].style.bold);
    assert!(first[0].style.italic);
    assert!(first[1].style.bold);
    assert_eq!(
        fragment.paragraphs()[1].kind(),
        ParagraphKind::Bullet { indent: 0 }
    );
    assert!(Arc::ptr_eq(
        &fragment.paragraphs()[1],
        &editor.document().paragraphs()[1]
    ));
    assert_eq!(
        fragment.paragraphs()[2].kind(),
        ParagraphKind::Ordered {
            indent: 2,
            start: 17
        }
    );
    assert_eq!(
        Fragment::from_bytes(&fragment.to_bytes()).unwrap(),
        fragment
    );
}

#[test]
fn partial_rich_paste_keeps_surviving_edge_styles_and_restores_selection() {
    let document = Document::from_fragment(&rich_fragment());
    let fragment = document
        .fragment(Position::new(0, 1)..Position::new(0, 7))
        .unwrap();
    let source_style = fragment.paragraphs()[0].spans()[0].style;
    let mut editor = Editor::from_text("prefixsuffix");
    let selection = Selection::caret(Position::new(0, 6));
    editor.set_selection(selection).unwrap();
    editor.insert_fragment(&fragment).unwrap();
    assert_eq!(editor.document().plain_text(), "prefixHello>suffix");
    assert_eq!(
        editor.document().style_at(Position::new(0, 7)),
        source_style
    );
    assert_eq!(
        editor.document().style_at(Position::new(0, 6)),
        Default::default()
    );
    assert_eq!(
        editor.document().style_at(Position::new(0, 13)),
        Default::default()
    );
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "prefixsuffix");
    assert_eq!(editor.selection(), selection);
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "prefixHello>suffix");
}

#[test]
fn fragment_ranges_validate_graphemes_and_empty_selection_is_an_empty_fragment() {
    let document = Document::from_text("e\u{301}\n🦀");
    assert_eq!(
        document.fragment(Position::new(1, 0)..Position::new(0, 0)),
        Err(Error::InvalidRange)
    );
    assert_eq!(
        document.fragment(Position::new(0, 1)..Position::new(0, 3)),
        Err(Error::InvalidPosition(Position::new(0, 1)))
    );
    let position = Position::new(1, 4);
    let fragment = document.fragment(position..position).unwrap();
    assert_eq!(fragment.paragraphs().len(), 1);
    assert_eq!(fragment.plain_text(), "");
    assert!(fragment.paragraphs()[0].spans().is_empty());
}

#[test]
fn rich_paste_rejects_active_ime_without_mutating_document_or_history() {
    let mut editor = Editor::from_text("before");
    editor.begin_composition();
    editor.update_composition("仮", Some(0..3)).unwrap();
    let before = Fragment::from_document(editor.document());
    let selection = editor.selection();
    let composition = editor.composition().cloned();
    assert_eq!(
        editor.insert_fragment(&rich_fragment()),
        Err(Error::CompositionActive)
    );
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.composition(), composition.as_ref());
    assert_eq!(editor.undo_len(), 0);
    assert!(editor.cancel_composition());
    editor
        .insert_fragment(&Fragment::from_text("after"))
        .unwrap();
    assert_eq!(editor.document().plain_text(), "afterbefore");
}

#[test]
fn rich_paste_normalizes_combining_joins_and_snaps_caret_to_grapheme_end() {
    let mut source = Editor::from_text("\u{301}");
    source.select_all();
    source
        .apply_style(StylePatch {
            bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    let fragment = source.selected_fragment();
    let mut editor = Editor::from_text("aZ");
    editor
        .set_selection(Selection::caret(Position::new(0, 1)))
        .unwrap();
    editor.insert_fragment(&fragment).unwrap();
    assert_eq!(editor.document().plain_text(), "a\u{301}Z");
    assert_eq!(editor.selection().focus, Position::new(0, 3));
    assert!(
        editor
            .document()
            .validate_position(Position::new(0, 1))
            .is_err()
    );
    assert_eq!(editor.document().paragraph(0).unwrap().spans().len(), 1);
    assert!(
        !editor.document().paragraph(0).unwrap().spans()[0]
            .style
            .bold
    );
    let encoded = editor.document().to_bytes();
    assert_eq!(
        Document::from_bytes(&encoded).unwrap().plain_text(),
        "a\u{301}Z"
    );
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "aZ");
    assert!(editor.redo());
    assert_eq!(editor.selection().focus, Position::new(0, 3));
}

#[test]
fn rich_paste_joining_an_existing_combining_suffix_uses_inserted_base_style() {
    let mut source = Editor::from_text("a");
    source.select_all();
    source
        .apply_style(StylePatch {
            bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    let mut editor = Editor::from_text("\u{301}Z");
    editor.insert_fragment(&source.selected_fragment()).unwrap();
    assert_eq!(editor.document().plain_text(), "a\u{301}Z");
    assert_eq!(editor.selection().focus, Position::new(0, 3));
    let spans = editor.document().paragraph(0).unwrap().spans();
    assert_eq!(spans[0].range, 0..3);
    assert!(spans[0].style.bold);
    assert_eq!(spans[1].range, 3..4);
    assert!(!spans[1].style.bold);
    assert!(Document::from_bytes(&editor.document().to_bytes()).is_ok());
}

#[test]
fn rich_paste_normalizes_emoji_joins_without_splitting_style_runs() {
    let mut source = Editor::from_text("\u{200d}💻");
    source.select_all();
    source
        .apply_style(StylePatch {
            italic: Some(true),
            ..Default::default()
        })
        .unwrap();
    let mut editor = Editor::from_text("👩X");
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 4)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    editor
        .set_selection(Selection::caret(Position::new(0, 4)))
        .unwrap();
    let before = Fragment::from_document(editor.document());
    editor.insert_fragment(&source.selected_fragment()).unwrap();
    assert_eq!(editor.document().plain_text(), "👩‍💻X");
    assert_eq!(editor.selection().focus, Position::new(0, 11));
    let spans = editor.document().paragraph(0).unwrap().spans();
    assert_eq!(spans[0].range, 0..11);
    assert!(spans[0].style.bold);
    assert!(!spans[0].style.italic);
    assert_eq!(spans[1].range, 11..12);
    assert!(!spans[1].style.bold);
    assert!(Document::from_bytes(&editor.document().to_bytes()).is_ok());
    assert!(editor.undo());
    assert_eq!(Fragment::from_document(editor.document()), before);
    assert!(editor.redo());
    assert_eq!(editor.document().plain_text(), "👩‍💻X");
}

fn header(paragraphs: u64) -> Vec<u8> {
    let mut output = b"TLFR\x01".to_vec();
    output.extend_from_slice(&paragraphs.to_le_bytes());
    output
}

fn paragraph(kind: &[u8], text: &[u8], spans: &[(u64, u8)]) -> Vec<u8> {
    let mut output = header(1);
    output.extend_from_slice(kind);
    output.extend_from_slice(&(text.len() as u64).to_le_bytes());
    output.extend_from_slice(text);
    output.extend_from_slice(&(spans.len() as u64).to_le_bytes());
    for &(end, flags) in spans {
        output.extend_from_slice(&end.to_le_bytes());
        output.push(flags);
    }
    output
}

#[test]
fn version_one_encoding_has_fixed_architecture_independent_layout() {
    assert_eq!(
        Fragment::from_text("a").to_bytes(),
        paragraph(&[0], b"a", &[(1, 0)])
    );
    assert_eq!(Fragment::default().to_bytes(), paragraph(&[0], b"", &[]));
}

#[test]
fn version_one_rich_fixture_remains_readable_and_byte_identical() {
    // This fixture was assembled directly from the documented v1 layout,
    // independently of Textloom's encoder. Keep its bytes stable for 1.x.
    let bytes = include_bytes!("fixtures/native-v1.tlfr");
    let mut editor = Editor::from_text("\nAé👩‍💻\ne\u{301}\nZ\n");
    select_paragraph(&mut editor, 1);
    editor
        .set_paragraph_kind(ParagraphKind::Heading { level: 6 })
        .unwrap();
    editor
        .set_selection(Selection::new(Position::new(1, 1), Position::new(1, 3)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            italic: Some(true),
            underline: Some(true),
            strikethrough: Some(true),
            code: Some(true),
            foreground: Some(Some(Color([1, 2, 3, 4]))),
        })
        .unwrap();
    editor
        .set_selection(Selection::new(Position::new(1, 3), Position::new(1, 14)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            code: Some(true),
            ..Default::default()
        })
        .unwrap();
    select_paragraph(&mut editor, 2);
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: u8::MAX })
        .unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    select_paragraph(&mut editor, 3);
    editor
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: u8::MAX,
            start: u32::MAX,
        })
        .unwrap();
    editor
        .apply_style(StylePatch {
            italic: Some(true),
            ..Default::default()
        })
        .unwrap();
    let expected = Fragment::from_document(editor.document());
    let decoded = Fragment::from_bytes(bytes).unwrap();
    assert_eq!(decoded, expected);
    assert_eq!(decoded.to_bytes(), bytes);
    assert_eq!(expected.to_bytes(), bytes);
    assert_eq!(Document::from_bytes(bytes).unwrap().to_bytes(), bytes);
}

#[test]
fn decoder_rejects_inputs_larger_than_its_byte_limit_before_parsing() {
    let oversized = vec![0; 64 * 1024 * 1024 + 1];
    assert_eq!(
        Fragment::from_bytes(&oversized),
        Err(FragmentError::LimitExceeded)
    );
}

#[test]
fn rejects_bad_signature_version_lengths_counts_and_unknown_kinds() {
    let mut invalid_magic = Fragment::default().to_bytes();
    invalid_magic[0] = b'X';
    assert_eq!(
        Fragment::from_bytes(&invalid_magic),
        Err(FragmentError::InvalidMagic)
    );
    let mut invalid_version = Fragment::default().to_bytes();
    invalid_version[4] = 255;
    assert_eq!(
        Fragment::from_bytes(&invalid_version),
        Err(FragmentError::UnsupportedVersion(255))
    );
    assert_eq!(
        Fragment::from_bytes(&header(0)),
        Err(FragmentError::InvalidParagraphCount)
    );
    for count in [1_000_001, u64::MAX] {
        assert_eq!(
            Fragment::from_bytes(&header(count)),
            Err(FragmentError::LimitExceeded)
        );
    }
    assert_eq!(
        Fragment::from_bytes(&header(1_000_000)),
        Err(FragmentError::Truncated)
    );
    let mut huge_text = header(1);
    huge_text.push(0);
    huge_text.extend_from_slice(&u64::MAX.to_le_bytes());
    huge_text.extend_from_slice(&0u64.to_le_bytes());
    assert!(Fragment::from_bytes(&huge_text).is_err());
    let mut huge_spans = paragraph(&[0], b"", &[]);
    let length = huge_spans.len();
    huge_spans[length - 8..].copy_from_slice(&1_000_001u64.to_le_bytes());
    assert_eq!(
        Fragment::from_bytes(&huge_spans),
        Err(FragmentError::LimitExceeded)
    );
    for kind in [&[4][..], &[1, 0], &[1, 7]] {
        assert_eq!(
            Fragment::from_bytes(&paragraph(kind, b"", &[])),
            Err(FragmentError::InvalidParagraphKind)
        );
    }
}

#[test]
fn rejects_invalid_utf8_line_breaks_noncovering_spans_and_grapheme_splits() {
    assert_eq!(
        Fragment::from_bytes(&paragraph(&[0], &[255], &[(1, 0)])),
        Err(FragmentError::InvalidUtf8)
    );
    for text in [b"a\nb".as_slice(), b"a\rb"] {
        assert_eq!(
            Fragment::from_bytes(&paragraph(&[0], text, &[(3, 0)])),
            Err(FragmentError::InvalidParagraphText)
        );
    }
    for spans in [&[][..], &[(0, 0)], &[(2, 0)], &[(1, 0), (1, 1)]] {
        assert_eq!(
            Fragment::from_bytes(&paragraph(&[0], b"a", spans)),
            Err(FragmentError::InvalidSpans)
        );
    }
    // A UTF-8 scalar boundary inside a combining grapheme is also invalid.
    assert_eq!(
        Fragment::from_bytes(&paragraph(&[0], "e\u{301}".as_bytes(), &[(1, 0), (3, 1)])),
        Err(FragmentError::InvalidSpans)
    );
    assert_eq!(
        Fragment::from_bytes(&paragraph(&[0], "🦀".as_bytes(), &[(1, 0), (4, 1)])),
        Err(FragmentError::InvalidSpans)
    );
    // Canonical spans merge identical neighbors.
    assert_eq!(
        Fragment::from_bytes(&paragraph(&[0], b"ab", &[(1, 0), (2, 0)])),
        Err(FragmentError::InvalidSpans)
    );
    assert_eq!(
        Fragment::from_bytes(&paragraph(&[0], b"a", &[(1, 128)])),
        Err(FragmentError::InvalidStyle)
    );
    assert_eq!(
        Fragment::from_bytes(&paragraph(&[0], b"a", &[(1, 32)])),
        Err(FragmentError::Truncated)
    );
}

#[test]
fn mutated_valid_inputs_never_panic_and_successes_are_canonical() {
    let original = rich_fragment().to_bytes();
    for index in 0..original.len() {
        for byte in [0, 1, 127, 255] {
            let mut bytes = original.clone();
            bytes[index] = byte;
            if let Ok(fragment) = Fragment::from_bytes(&bytes) {
                assert_eq!(fragment.to_bytes(), bytes, "mutation at {index}");
            }
        }
    }
}

#[test]
fn html_escapes_text_and_exports_all_inline_styles_and_heading_levels() {
    let html = rich_fragment().to_html();
    assert!(html.starts_with("<div style=\"white-space:pre-wrap\"><h3>"));
    assert!(html.contains(
        "<em><u><s><code><span style=\"color:rgba(0,128,255,0.498040)\">&lt;Hello&gt;</span></code></s></u></em>"
    ));
    assert!(html.contains("<strong><em><u><s><code><span style=\"color:rgba(0,128,255,0.498040)\"> &amp; &quot;世界&quot; e\u{301}</span></code></s></u></em></strong>"));
    assert!(
        html.contains("<ol start=\"17\"><li>nested</li><li value=\"4294967295\">last</li></ol>")
    );
    assert!(html.ends_with("<p><br></p></div>"));
    assert_eq!(
        Fragment::from_text("<script>alert('x')</script> & 🦀").to_html(),
        "<div style=\"white-space:pre-wrap\"><p>&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt; &amp; 🦀</p></div>"
    );
}

#[test]
fn html_colors_preserve_channels_and_alpha_with_rounding_or_truncating_importers() {
    for alpha in 0..=255 {
        let mut editor = Editor::from_text("色");
        select_paragraph(&mut editor, 0);
        editor
            .apply_style(StylePatch {
                foreground: Some(Some(Color([42, 100, 200, alpha]))),
                ..Default::default()
            })
            .unwrap();
        let html = editor.document().to_html();
        assert_eq!(Fragment::from_document(editor.document()).to_html(), html);
        if alpha == 255 {
            assert!(html.contains("color:#2a64c8\""));
        } else {
            let opacity = html
                .split_once("color:rgba(42,100,200,")
                .unwrap()
                .1
                .split_once(')')
                .unwrap()
                .0
                .parse::<f64>()
                .unwrap();
            assert!((0.0..=1.0).contains(&opacity));
            assert_eq!((opacity * 255.0).round() as u8, alpha);
            assert_eq!((opacity * 255.0).trunc() as u8, alpha);
        }
    }
}

#[test]
fn html_replaces_nulls_without_losing_surrounding_unicode_or_escaping() {
    let document = Document::from_text("\0é\0<&🦀\0");
    let expected = "<div style=\"white-space:pre-wrap\"><p>�é�&lt;&amp;🦀�</p></div>";
    assert_eq!(document.to_html(), expected);
    let fragment = Fragment::from_document(&document);
    assert_eq!(fragment.to_html(), expected);
    // The native interchange remains lossless for text HTML cannot represent.
    assert_eq!(
        Fragment::from_bytes(&fragment.to_bytes()).unwrap(),
        fragment
    );
}

#[test]
fn html_exports_each_valid_heading_level_and_keeps_empty_headings_visible() {
    let mut editor = Editor::from_text("one\ntwo\nthree\nfour\nfive\n");
    for level in 1..=6 {
        select_paragraph(&mut editor, usize::from(level - 1));
        editor
            .set_paragraph_kind(ParagraphKind::Heading { level })
            .unwrap();
    }
    assert_eq!(
        editor.document().to_html(),
        "<div style=\"white-space:pre-wrap\"><h1>one</h1><h2>two</h2><h3>three</h3><h4>four</h4><h5>five</h5><h6><br></h6></div>"
    );
}

#[test]
fn html_nests_lists_inside_items_and_preserves_number_restarts() {
    let mut editor = Editor::from_text("a\nb\nc\nd\ne\nbody\nf\ng");
    for (index, kind) in [
        (0, ParagraphKind::Bullet { indent: 0 }),
        (1, ParagraphKind::Bullet { indent: 2 }),
        (
            2,
            ParagraphKind::Ordered {
                indent: 7,
                start: 7,
            },
        ),
        (3, ParagraphKind::Bullet { indent: 2 }),
        (4, ParagraphKind::Bullet { indent: 0 }),
        (
            6,
            ParagraphKind::Ordered {
                indent: 0,
                start: 0,
            },
        ),
        (
            7,
            ParagraphKind::Ordered {
                indent: 0,
                start: 12,
            },
        ),
    ] {
        select_paragraph(&mut editor, index);
        editor.set_paragraph_kind(kind).unwrap();
    }
    assert_eq!(
        Fragment::from_document(editor.document()).to_html(),
        "<div style=\"white-space:pre-wrap\"><ul><li>a<ul><li>b<ol start=\"7\"><li>c</li></ol></li><li>d</li></ul></li><li>e</li></ul><p>body</p><ol start=\"0\"><li>f</li><li value=\"12\">g</li></ol></div>"
    );
}

#[test]
fn html_changes_list_types_at_one_indent_without_invalid_list_children() {
    let mut editor = Editor::from_text("a\nb\nc\nd");
    for (index, kind) in [
        (0, ParagraphKind::Bullet { indent: 0 }),
        (1, ParagraphKind::Bullet { indent: 1 }),
        (
            2,
            ParagraphKind::Ordered {
                indent: 1,
                start: 3,
            },
        ),
        (
            3,
            ParagraphKind::Ordered {
                indent: 0,
                start: 9,
            },
        ),
    ] {
        select_paragraph(&mut editor, index);
        editor.set_paragraph_kind(kind).unwrap();
    }
    assert_eq!(
        Fragment::from_document(editor.document()).to_html(),
        "<div style=\"white-space:pre-wrap\"><ul><li>a<ul><li>b</li></ul><ol start=\"3\"><li>c</li></ol></li></ul><ol start=\"9\"><li>d</li></ol></div>"
    );
}
