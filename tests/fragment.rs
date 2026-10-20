use std::sync::Arc;

use textloom::{
    Color, DecodeLimits, Document, Editor, Error, Fragment, FragmentError, InlineStyle, Paragraph,
    ParagraphKind, Position, Selection, Span, StylePatch,
};

fn select_paragraph(editor: &mut Editor, index: usize) {
    editor.select_paragraph(index).unwrap();
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
            background: Some(Some(Color([255, 255, 0, 64]))),
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
    select_paragraph(&mut editor, 1);
    editor
        .apply_style(StylePatch {
            background: Some(Some(Color::YELLOW)),
            ..Default::default()
        })
        .unwrap();
    for (index, kind) in [
        (1, ParagraphKind::Bullet { indent: 0 }),
        (
            2,
            ParagraphKind::Ordered {
                indent: 2,
                start: Some(17),
            },
        ),
        (
            3,
            ParagraphKind::Ordered {
                indent: 2,
                start: Some(u32::MAX),
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
        assert_eq!(&bytes[..5], b"TLFR\x02");
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
            start: Some(17)
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
fn empty_single_paragraph_paste_preserves_blocks_and_restores_document_typing_style() {
    let kinds = [
        ParagraphKind::Body,
        ParagraphKind::Quote,
        ParagraphKind::Heading { level: 2 },
        ParagraphKind::Bullet { indent: 1 },
        ParagraphKind::Ordered {
            indent: 1,
            start: Some(7),
        },
    ];
    for destination in kinds {
        for source in kinds {
            let fragment = Fragment::from_document(&Document::from_paragraphs([Paragraph::new(
                "",
                Vec::new(),
                source,
            )
            .unwrap()]));
            for text in ["", "e\u{301}x"] {
                let mut editor = Editor::from_text(text);
                editor.set_paragraph_kind(destination).unwrap();
                editor.clear_history();
                for byte in [0, text.len()] {
                    let position = Position::new(0, byte);
                    editor.set_selection(Selection::caret(position)).unwrap();
                    editor
                        .apply_style(StylePatch {
                            bold: Some(true),
                            ..Default::default()
                        })
                        .unwrap();
                    editor.mark_saved();
                    let before = editor.document().clone();
                    editor.insert_fragment(&fragment).unwrap();
                    assert_eq!(editor.document(), &before);
                    assert_eq!(editor.selection(), Selection::caret(position));
                    assert_eq!(editor.typing_style(), before.style_at(position));
                    assert_eq!(editor.document().paragraph(0).unwrap().kind(), destination);
                    assert_eq!(editor.undo_len(), 0);
                    assert!(!editor.is_modified());
                }
            }
        }
    }
}

#[test]
fn empty_fragments_still_delete_selections_and_multiline_fragments_insert_breaks() {
    let mut editor = Editor::from_text("e\u{301}x");
    let selection = Selection::new(Position::new(0, 0), Position::new(0, 3));
    editor.set_selection(selection).unwrap();
    editor.insert_fragment(&Fragment::from_text("")).unwrap();
    assert_eq!(editor.document().plain_text(), "x");
    assert_eq!(editor.selection(), Selection::caret(Position::default()));
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "e\u{301}x");
    assert_eq!(editor.selection(), selection);

    editor
        .set_selection(Selection::caret(Position::new(0, 3)))
        .unwrap();
    editor
        .insert_fragment(&Fragment::from_text("\n\n"))
        .unwrap();
    assert_eq!(editor.document().plain_text(), "e\u{301}\n\nx");
    assert_eq!(editor.selection(), Selection::caret(Position::new(2, 0)));
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "e\u{301}x");
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
    versioned_header(1, paragraphs)
}

fn versioned_header(version: u8, paragraphs: u64) -> Vec<u8> {
    let mut output = b"TLFR".to_vec();
    output.push(version);
    output.extend_from_slice(&paragraphs.to_le_bytes());
    output
}

fn paragraph(kind: &[u8], text: &[u8], spans: &[(u64, u8)]) -> Vec<u8> {
    versioned_paragraphs(1, &[(kind, text, spans)])
}

type RawParagraph<'a> = (&'a [u8], &'a [u8], &'a [(u64, u8)]);

fn versioned_paragraphs(version: u8, paragraphs: &[RawParagraph<'_>]) -> Vec<u8> {
    let mut output = versioned_header(version, paragraphs.len() as u64);
    for (kind, text, spans) in paragraphs {
        output.extend_from_slice(kind);
        output.extend_from_slice(&(text.len() as u64).to_le_bytes());
        output.extend_from_slice(text);
        output.extend_from_slice(&(spans.len() as u64).to_le_bytes());
        for &(end, flags) in *spans {
            output.extend_from_slice(&end.to_le_bytes());
            output.push(flags);
        }
    }
    output
}

fn numbers(document: &Document) -> Vec<Option<u32>> {
    (0..document.paragraphs().len())
        .map(|index| document.list_number(index))
        .collect()
}

#[test]
fn version_two_encoding_has_fixed_architecture_independent_layout() {
    assert_eq!(
        Fragment::from_text("a").to_bytes(),
        versioned_paragraphs(2, &[(&[0], b"a", &[(1, 0)])])
    );
    assert_eq!(
        Fragment::default().to_bytes(),
        versioned_paragraphs(2, &[(&[0], b"", &[])])
    );
    let document = Document::from_paragraphs([
        Paragraph::new(
            "",
            Vec::new(),
            ParagraphKind::Ordered {
                indent: 3,
                start: None,
            },
        )
        .unwrap(),
        Paragraph::new(
            "",
            Vec::new(),
            ParagraphKind::Ordered {
                indent: 4,
                start: Some(0x0102_0304),
            },
        )
        .unwrap(),
    ]);
    assert_eq!(
        document.to_bytes(),
        versioned_paragraphs(2, &[(&[4, 3], b"", &[]), (&[5, 4, 4, 3, 2, 1], b"", &[])])
    );
}

#[test]
fn version_one_rich_fixture_remains_readable_and_reencodes_as_version_two() {
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
            background: None,
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
            start: Some(u32::MAX),
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
    let document = Document::from_bytes(bytes).unwrap();
    assert_eq!(numbers(&document), [None, None, None, Some(u32::MAX), None]);
    let mut version_two = bytes.to_vec();
    version_two[4] = 2;
    assert_eq!(version_two[123], 3);
    version_two[123] = 5;
    assert_eq!(decoded.to_bytes(), version_two);
    assert_eq!(document.to_bytes(), version_two);
    assert_eq!(Fragment::from_bytes(&version_two).unwrap(), expected);
}

#[test]
fn version_two_rich_fixture_remains_readable_and_byte_identical() {
    let bytes = include_bytes!("fixtures/native-v2.tlfr");
    let version_one = Fragment::from_bytes(include_bytes!("fixtures/native-v1.tlfr")).unwrap();
    let plain = |text: &str| {
        (!text.is_empty())
            .then(|| Span {
                range: 0..text.len(),
                style: InlineStyle::default(),
                link: None,
            })
            .into_iter()
            .collect()
    };
    let item = |text: &str, indent, start| {
        Arc::new(
            Paragraph::new(text, plain(text), ParagraphKind::Ordered { indent, start }).unwrap(),
        )
    };
    let mut paragraphs = version_one.paragraphs()[..4].to_vec();
    paragraphs.extend([
        item("Y", u8::MAX, None),
        item("a", 0, None),
        item("b", 1, None),
        item("c", 0, Some(0)),
        item("", 0, None),
        Arc::clone(&version_one.paragraphs()[4]),
    ]);
    let expected = Document::from_paragraphs(paragraphs);
    let decoded = Document::from_bytes(bytes).unwrap();
    assert_eq!(Fragment::from(&decoded), Fragment::from(&expected));
    assert_eq!(expected.to_bytes(), bytes);
    assert_eq!(decoded.to_bytes(), bytes);
    assert_eq!(
        numbers(&decoded),
        [
            None,
            None,
            None,
            Some(u32::MAX),
            Some(u32::MAX),
            Some(1),
            Some(1),
            Some(0),
            Some(1),
            None
        ]
    );
}

#[test]
fn version_one_ordinals_read_as_continuations_unless_they_restart() {
    let ordered = |indent: u8, ordinal: u32| {
        let mut kind = vec![3, indent];
        kind.extend_from_slice(&ordinal.to_le_bytes());
        kind
    };
    let kinds = [
        ordered(0, 1),
        ordered(0, 2),
        ordered(1, 1),
        ordered(0, 3),
        ordered(0, 7),
        vec![0],
        ordered(0, 2),
        vec![2, 0],
        ordered(0, 1),
        ordered(1, 5),
        ordered(0, u32::MAX),
        ordered(0, u32::MAX),
        ordered(0, 0),
        vec![0],
        ordered(0, 1),
    ];
    let paragraphs: Vec<RawParagraph<'_>> = kinds
        .iter()
        .map(|kind| (kind.as_slice(), b"".as_slice(), [].as_slice()))
        .collect();
    let document = Document::from_bytes(&versioned_paragraphs(1, &paragraphs)).unwrap();
    let starts: Vec<_> = document
        .paragraphs()
        .iter()
        .map(|paragraph| match paragraph.kind() {
            ParagraphKind::Ordered { start, .. } => Some(start),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        [
            Some(None),
            Some(None),
            Some(None),
            Some(None),
            Some(Some(7)),
            None,
            Some(Some(2)),
            None,
            Some(Some(1)),
            Some(Some(5)),
            Some(Some(u32::MAX)),
            Some(None),
            Some(Some(0)),
            None,
            Some(None),
        ]
    );
    let ordinals: Vec<_> = kinds
        .iter()
        .map(|kind| (kind[0] == 3).then(|| u32::from_le_bytes(kind[2..6].try_into().unwrap())))
        .collect();
    assert_eq!(numbers(&document), ordinals);
}

#[test]
fn each_version_accepts_only_its_own_paragraph_kinds() {
    let restart = [5, 0, 1, 0, 0, 0];
    let legacy = [3, 0, 1, 0, 0, 0];
    for kind in [&[4, 0][..], &restart, &[6]] {
        assert_eq!(
            Fragment::from_bytes(&versioned_paragraphs(1, &[(kind, b"", &[])])),
            Err(FragmentError::InvalidParagraphKind)
        );
        assert!(Fragment::from_bytes(&versioned_paragraphs(2, &[(kind, b"", &[])])).is_ok());
    }
    assert!(Fragment::from_bytes(&versioned_paragraphs(1, &[(&legacy, b"", &[])])).is_ok());
    for kind in [&legacy[..], &[7], &[255]] {
        assert_eq!(
            Fragment::from_bytes(&versioned_paragraphs(2, &[(kind, b"", &[])])),
            Err(FragmentError::InvalidParagraphKind)
        );
    }
    for flags in [64, 128, 192] {
        assert_eq!(
            Fragment::from_bytes(&versioned_paragraphs(1, &[(&[0], b"a", &[(1, flags)])])),
            Err(FragmentError::InvalidStyle)
        );
    }
    for flags in [64, 128] {
        assert_eq!(
            Fragment::from_bytes(&versioned_paragraphs(2, &[(&[0], b"a", &[(1, flags)])])),
            Err(FragmentError::Truncated)
        );
    }
    for version in [0, 3, 255] {
        assert_eq!(
            Fragment::from_bytes(&versioned_paragraphs(version, &[(&[0], b"", &[])])),
            Err(FragmentError::UnsupportedVersion(version))
        );
    }
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
    assert_eq!(
        Fragment::from_bytes(&paragraph(&[0], "e\u{301}".as_bytes(), &[(1, 0), (3, 1)])),
        Err(FragmentError::InvalidSpans)
    );
    assert_eq!(
        Fragment::from_bytes(&paragraph(&[0], "🦀".as_bytes(), &[(1, 0), (4, 1)])),
        Err(FragmentError::InvalidSpans)
    );
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
fn fills_encode_after_the_foreground_and_survive_rich_paste() {
    let mut editor = Editor::from_text("ab");
    editor
        .set_selection(Selection::new(Position::new(0, 1), Position::new(0, 2)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            foreground: Some(Some(Color([1, 2, 3, 4]))),
            background: Some(Some(Color([5, 6, 7, 8]))),
            ..Default::default()
        })
        .unwrap();
    let bytes = editor.document().to_bytes();
    let mut expected = paragraph(&[0], b"ab", &[(1, 0), (2, 0x60)]);
    expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(bytes[5..], expected[5..]);
    let fragment = Fragment::from_bytes(&bytes).unwrap();
    assert_eq!(fragment.to_bytes(), bytes);
    assert_eq!(fragment.plain_text(), "ab");

    let mut fill_only = paragraph(&[0], b"a", &[(1, 64)]);
    fill_only[4] = bytes[4];
    for (length, error) in [
        (0, Some(FragmentError::Truncated)),
        (3, Some(FragmentError::Truncated)),
        (4, None),
    ] {
        let mut input = fill_only.clone();
        input.extend_from_slice(&Color::YELLOW.0[..length]);
        let decoded = Fragment::from_bytes(&input);
        match error {
            Some(error) => assert_eq!(decoded, Err(error)),
            None => {
                let style = decoded.unwrap().paragraphs()[0].spans()[0].style;
                assert_eq!(style.background, Some(Color::YELLOW));
                assert_eq!(style.foreground, None);
            }
        }
    }
    let mut both = paragraph(&[0], b"a", &[(1, 0x60)]);
    both[4] = bytes[4];
    both.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(Fragment::from_bytes(&both), Err(FragmentError::Truncated));

    let mut destination = Editor::from_text("xy");
    destination
        .set_selection(Selection::caret(Position::new(0, 1)))
        .unwrap();
    destination.insert_fragment(&fragment).unwrap();
    assert_eq!(destination.document().plain_text(), "xaby");
    let spans = destination.document().paragraph(0).unwrap().spans();
    assert_eq!(spans[1].range, 2..3);
    assert_eq!(spans[1].style.background, Some(Color([5, 6, 7, 8])));
    assert_eq!(spans[2].style.background, None);
    assert_eq!(destination.undo_len(), 1);
    assert!(destination.undo());
    assert_eq!(destination.document().plain_text(), "xy");
    assert!(destination.redo());
    assert_eq!(
        destination.document().paragraph(0).unwrap().spans()[1]
            .style
            .background,
        Some(Color([5, 6, 7, 8]))
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
        "<em><code><span style=\"color:rgba(0,128,255,0.498040);background-color:rgba(255,255,0,0.250981)\"><u><s>&lt;Hello&gt;</s></u></span></code></em>"
    ));
    assert!(html.contains("<strong><em><code><span style=\"color:rgba(0,128,255,0.498040);background-color:rgba(255,255,0,0.250981)\"><u><s> &amp; &quot;世界&quot; e\u{301}</s></u></span></code></em></strong>"));
    assert!(html.contains("<ul><li><span style=\"background-color:#ffff00\">item</span>"));
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
                start: Some(7),
            },
        ),
        (3, ParagraphKind::Bullet { indent: 2 }),
        (4, ParagraphKind::Bullet { indent: 0 }),
        (
            6,
            ParagraphKind::Ordered {
                indent: 0,
                start: Some(0),
            },
        ),
        (
            7,
            ParagraphKind::Ordered {
                indent: 0,
                start: Some(12),
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
                start: Some(3),
            },
        ),
        (
            3,
            ParagraphKind::Ordered {
                indent: 0,
                start: Some(9),
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

#[test]
fn html_colors_enclose_decorations_so_lines_use_the_text_color() {
    let mut editor = Editor::from_text("x");
    editor.select_all();
    editor
        .apply_style(StylePatch {
            underline: Some(true),
            foreground: Some(Some(Color([255, 0, 0, 255]))),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        editor.document().to_html(),
        "<div style=\"white-space:pre-wrap\"><p><span style=\"color:#ff0000\"><u>x</u></span></p></div>"
    );
}

#[test]
fn html_fills_share_the_color_span_and_its_color_formatting() {
    let mut editor = Editor::from_text("x");
    editor.select_all();
    editor
        .apply_style(StylePatch {
            code: Some(true),
            underline: Some(true),
            background: Some(Some(Color::YELLOW)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        editor.document().to_html(),
        "<div style=\"white-space:pre-wrap\"><p><code><span style=\"background-color:#ffff00\"><u>x</u></span></code></p></div>"
    );
    editor
        .apply_style(StylePatch {
            foreground: Some(Some(Color::RED)),
            background: Some(Some(Color([0, 112, 192, 128]))),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        editor.document().to_html(),
        "<div style=\"white-space:pre-wrap\"><p><code><span style=\"color:#ff0000;background-color:rgba(0,112,192,0.501961)\"><u>x</u></span></code></p></div>"
    );
    assert_eq!(
        Fragment::from_document(editor.document()).to_html(),
        editor.document().to_html()
    );
}

#[test]
fn decode_limits_reject_oversized_input_and_trusted_limits_accept_it() {
    let document = Document::from_text("a\nbb\nccc");
    let bytes = document.to_bytes();
    let content = |document: &Document| Fragment::from(document);
    assert_eq!(
        content(&Document::from_bytes_with_limits(&bytes, DecodeLimits::default()).unwrap()),
        content(&document)
    );
    let decode = |paragraphs: usize, spans: usize, length: usize| {
        let mut limits = DecodeLimits::default();
        limits.max_paragraphs = paragraphs;
        limits.max_spans = spans;
        limits.max_bytes = length;
        Fragment::from_bytes_with_limits(&bytes, limits)
    };
    for (paragraphs, spans, length) in [
        (2, 3, bytes.len()),
        (3, 2, bytes.len()),
        (3, 3, bytes.len() - 1),
    ] {
        assert_eq!(
            decode(paragraphs, spans, length),
            Err(FragmentError::LimitExceeded)
        );
    }
    assert!(decode(3, 3, bytes.len()).is_ok());
    let mut truncated = bytes.clone();
    truncated[5..13].copy_from_slice(&u64::from(u32::MAX).to_le_bytes());
    assert_eq!(
        Fragment::from_bytes_with_limits(&truncated, DecodeLimits::UNLIMITED),
        Err(FragmentError::Truncated)
    );
    assert_eq!(
        Fragment::from_bytes_with_limits(b"TLFR\x03", DecodeLimits::UNLIMITED),
        Err(FragmentError::UnsupportedVersion(3))
    );
    for version in [1, 2] {
        assert_eq!(
            Fragment::from_bytes_with_limits(
                &[b'T', b'L', b'F', b'R', version],
                DecodeLimits::UNLIMITED
            ),
            Err(FragmentError::Truncated)
        );
    }

    let large = Document::from_text(&"\n".repeat(1_000_000));
    let bytes = large.to_bytes();
    assert_eq!(
        Document::from_bytes(&bytes),
        Err(FragmentError::LimitExceeded)
    );
    let restored = Document::from_bytes_with_limits(&bytes, DecodeLimits::UNLIMITED).unwrap();
    assert_eq!(restored.paragraphs().len(), 1_000_001);
}

#[test]
fn fragments_and_documents_convert_without_copying_paragraphs() {
    let document = Document::from_text("one\ntwo");
    let fragment = Fragment::from(&document);
    assert_eq!(fragment, Fragment::from_document(&document));
    let restored = Document::from(fragment.clone());
    assert!(Arc::ptr_eq(
        &restored.paragraphs()[1],
        &fragment.paragraphs()[1]
    ));
    assert_eq!(restored, Document::from_fragment(&fragment));
}

#[test]
fn soft_line_breaks_roundtrip_natively_and_export_as_html_breaks() {
    let mut editor =
        Editor::from_text("Title\u{2028}sub\nitem\u{2028}more\u{2028}\n<a>\u{2028}b\n\u{2028}");
    select_paragraph(&mut editor, 0);
    editor
        .set_paragraph_kind(ParagraphKind::Heading { level: 1 })
        .unwrap();
    select_paragraph(&mut editor, 1);
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
        .unwrap();
    editor
        .set_selection(Selection::new(Position::new(2, 0), Position::new(2, 6)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..StylePatch::default()
        })
        .unwrap();
    let document = editor.document();
    let bytes = document.to_bytes();
    assert!(bytes.windows(3).any(|bytes| bytes == "\u{2028}".as_bytes()));
    let decoded = Document::from_bytes(&bytes).unwrap();
    assert_eq!(
        Fragment::from_document(&decoded),
        Fragment::from_document(document)
    );
    assert_eq!(
        document.to_html(),
        "<div style=\"white-space:pre-wrap\"><h1>Title<br>sub</h1><ul><li>item<br>more<br><br></li></ul><p><strong>&lt;a&gt;<br></strong>b</p><p><br><br></p></div>"
    );
    let plain = "Title\nsub\nitem\nmore\n\n<a>\nb\n\n";
    assert_eq!(document.plain_text(), plain);
    assert_eq!(Fragment::from_document(document).plain_text(), plain);
    assert_eq!(Fragment::from_text(plain).paragraphs().len(), 9);
    assert_eq!(Fragment::from_text("a\u{2029}b").plain_text(), "a\nb");
    assert_eq!(Fragment::from_text("a\u{2029}b").paragraphs().len(), 2);

    let encoded = |spans: &[(u64, u8)]| {
        Fragment::from_bytes(&paragraph(&[0], "a\u{2028}b".as_bytes(), spans))
    };
    let fragment = encoded(&[(4, 1), (5, 0)]).unwrap();
    assert_eq!(fragment.paragraphs()[0].text(), "a\u{2028}b");
    assert_eq!(
        fragment.to_html(),
        "<div style=\"white-space:pre-wrap\"><p><strong>a<br></strong>b</p></div>"
    );
    assert!(encoded(&[(2, 1), (5, 0)]).is_err());
}

#[test]
fn quotes_encode_as_version_two_tag_six_and_export_one_blockquote_per_run() {
    let mut editor = Editor::from_text("intro\n<q>\nsaid\u{2028}it\n\nitem\nagain");
    editor
        .set_selection(Selection::new(Position::new(1, 0), Position::new(3, 0)))
        .unwrap();
    editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
    editor
        .set_selection(Selection::caret(Position::new(3, 0)))
        .unwrap();
    editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
    editor
        .set_selection(Selection::caret(Position::new(4, 0)))
        .unwrap();
    editor
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: 0,
            start: None,
        })
        .unwrap();
    editor
        .set_selection(Selection::caret(Position::new(5, 0)))
        .unwrap();
    editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
    let document = editor.document();

    let quote = |text: &str| {
        let spans = (!text.is_empty())
            .then(|| Span {
                range: 0..text.len(),
                style: InlineStyle::default(),
                link: None,
            })
            .into_iter()
            .collect();
        Paragraph::new(text, spans, ParagraphKind::Quote).unwrap()
    };
    assert_eq!(
        Document::from_paragraphs([quote("")]).to_bytes(),
        versioned_paragraphs(2, &[(&[6], b"", &[])])
    );
    let bytes = document.to_bytes();
    let decoded = Document::from_bytes(&bytes).unwrap();
    assert_eq!(
        Fragment::from_document(&decoded),
        Fragment::from_document(document)
    );
    assert_eq!(decoded.to_bytes(), bytes);

    assert_eq!(
        document.to_html(),
        "<div style=\"white-space:pre-wrap\"><p>intro</p><blockquote><p>&lt;q&gt;</p><p>said<br>it</p><p><br></p></blockquote><ol start=\"1\"><li>item</li></ol><blockquote><p>again</p></blockquote></div>"
    );
    assert_eq!(document.plain_text(), "intro\n<q>\nsaid\nit\n\nitem\nagain");
    let quotes = Document::from_paragraphs([quote("a"), quote("b")]);
    assert_eq!(
        Fragment::from_document(&quotes).to_html(),
        "<div style=\"white-space:pre-wrap\"><blockquote><p>a</p><p>b</p></blockquote></div>"
    );
}

#[test]
fn rich_paste_and_fragments_carry_quotes_like_other_kinds() {
    let mut source = Editor::from_text("one\ntwo\nthree");
    source.select_all();
    source.set_paragraph_kind(ParagraphKind::Quote).unwrap();
    source
        .set_selection(Selection::new(Position::new(0, 0), Position::new(1, 3)))
        .unwrap();
    let copied = source.selected_fragment();
    assert_eq!(
        copied
            .paragraphs()
            .iter()
            .map(|paragraph| paragraph.kind())
            .collect::<Vec<_>>(),
        [ParagraphKind::Quote, ParagraphKind::Quote]
    );
    let mut editor = Editor::from_text("body\n");
    editor
        .set_selection(Selection::caret(Position::new(1, 0)))
        .unwrap();
    editor.insert_fragment(&copied).unwrap();
    let kinds: Vec<_> = editor
        .document()
        .paragraphs()
        .iter()
        .map(|paragraph| paragraph.kind())
        .collect();
    assert_eq!(
        kinds,
        [
            ParagraphKind::Body,
            ParagraphKind::Quote,
            ParagraphKind::Quote
        ]
    );
    assert_eq!(editor.document().plain_text(), "body\none\ntwo");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "body\n");

    source
        .set_selection(Selection::new(Position::new(1, 0), Position::new(1, 2)))
        .unwrap();
    let inline = source.selected_fragment();
    editor
        .set_selection(Selection::caret(Position::new(0, 4)))
        .unwrap();
    editor.insert_fragment(&inline).unwrap();
    assert_eq!(editor.document().paragraph(0).unwrap().text(), "bodytw");
    assert_eq!(
        editor.document().paragraph(0).unwrap().kind(),
        ParagraphKind::Body
    );
    let mut quoted = Editor::from_text("quoted");
    quoted.set_paragraph_kind(ParagraphKind::Quote).unwrap();
    quoted
        .set_selection(Selection::caret(Position::new(0, 3)))
        .unwrap();
    quoted
        .insert_fragment(&Fragment::from_text("x\ny"))
        .unwrap();
    assert_eq!(quoted.document().plain_text(), "quox\nyted");
    assert!(
        quoted
            .document()
            .paragraphs()
            .iter()
            .all(|paragraph| paragraph.kind() == ParagraphKind::Quote)
    );
}

#[test]
fn copied_list_items_export_the_numbers_they_showed_without_changing_native_bytes() {
    let mut editor = Editor::from_text("Title\na\nb\nc\nd\ne\nnested\nf");
    editor
        .set_selection(Selection::new(Position::new(1, 0), Position::new(7, 1)))
        .unwrap();
    editor.apply_numbered_list(0).unwrap();
    editor
        .set_selection(Selection::caret(Position::new(6, 0)))
        .unwrap();
    editor.indent_list().unwrap();
    editor
        .set_selection(Selection::new(Position::new(3, 0), Position::new(7, 1)))
        .unwrap();
    let copied = editor.selected_fragment();
    let html = copied.to_html();
    assert!(
        html.starts_with("<div style=\"white-space:pre-wrap\"><ol start=\"3\"><li>c"),
        "{html}"
    );
    assert!(
        html.contains("<li>e<ol start=\"1\"><li>nested</li></ol></li><li>f</li>"),
        "{html}"
    );
    let continuing = |indent| ParagraphKind::Ordered {
        indent,
        start: None,
    };
    let kinds: Vec<_> = copied.paragraphs().iter().map(|p| p.kind()).collect();
    assert_eq!(
        kinds,
        [
            continuing(0),
            continuing(0),
            continuing(0),
            ParagraphKind::Ordered {
                indent: 1,
                start: Some(1)
            },
            continuing(0)
        ]
    );
    let decoded = Fragment::from_bytes(&copied.to_bytes()).unwrap();
    assert_eq!(decoded, copied);
    assert_eq!(decoded.to_bytes(), copied.to_bytes());
    assert!(decoded.to_html().contains("<ol start=\"1\"><li>c"));
    assert!(
        Document::from_fragment(&copied)
            .to_html()
            .contains("<ol start=\"1\"><li>c")
    );

    editor
        .set_selection(Selection::new(Position::new(1, 0), Position::new(2, 1)))
        .unwrap();
    let first = editor.selected_fragment();
    assert_eq!(
        first.paragraphs()[0].kind(),
        ParagraphKind::Ordered {
            indent: 0,
            start: Some(1)
        }
    );
    assert_eq!(first.paragraphs()[1].kind(), continuing(0));
    let mut destination = Editor::from_text("x\ny");
    destination.select_all();
    destination.apply_numbered_list(0).unwrap();
    destination
        .set_selection(Selection::caret(Position::new(1, 1)))
        .unwrap();
    destination.insert_text("\n").unwrap();
    destination.insert_fragment(&first).unwrap();
    assert_eq!(
        numbers(destination.document()),
        [Some(1), Some(2), Some(1), Some(2)]
    );
}

#[test]
fn paragraph_separators_cannot_appear_inside_paragraph_text() {
    let text = "a\u{2029}b";
    let spans = || {
        vec![Span {
            range: 0..text.len(),
            style: InlineStyle::default(),
            link: None,
        }]
    };
    assert_eq!(
        Paragraph::new(text, spans(), ParagraphKind::Body),
        Err(Error::InvalidFragment)
    );
    let raw = |version| {
        versioned_paragraphs(
            version,
            &[(&[0], text.as_bytes(), &[(text.len() as u64, 0)])],
        )
    };
    assert_eq!(
        Fragment::from_bytes(&raw(2)),
        Err(FragmentError::InvalidParagraphText)
    );
    let document = Document::from_bytes(&raw(1)).unwrap();
    assert_eq!(document.paragraphs()[0].text(), "a\u{2028}b");
    assert_eq!(document.plain_text(), "a\nb");
    assert_eq!(Document::from_text(text).paragraphs().len(), 2);
}
