//! Fuzzes the native TLFR decoder with seeded random mutations of valid
//! version 1 and version 2 fragments. Each mutated input must either be rejected
//! or decode to a fragment that re-encodes canonically and can be pasted,
//! exported, and undone.

use textloom::{
    Color, Document, Editor, Fragment, Link, ParagraphKind, Position, Selection, StylePatch,
};

/// A xorshift64 pseudorandom generator, so that every run tests the same inputs.
struct Generator(u64);

impl Generator {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// Applies `patch` to the first occurrence of `text` in paragraph `paragraph`.
fn style(editor: &mut Editor, paragraph: usize, text: &str, patch: StylePatch) {
    let paragraph_text = editor
        .document()
        .paragraph(paragraph)
        .expect("callers pass a paragraph of the fixture text")
        .text();
    let start = paragraph_text
        .find(text)
        .expect("callers pass text that occurs in the paragraph");
    editor
        .set_selection(Selection::new(
            Position::new(paragraph, start),
            Position::new(paragraph, start + text.len()),
        ))
        .expect("a substring found in the paragraph has grapheme boundaries at both ends");
    editor
        .apply_style(patch)
        .expect("no IME composition is active");
}

/// Builds a fragment with every paragraph kind, style flag, both colors, and
/// links, over text that needs HTML escaping and has multi-scalar graphemes.
fn styled_fragment() -> Fragment {
    let mut editor = Editor::from_text(
        "<b>Hi</b> & \"世界\" e\u{301} 👨\u{200d}👩\u{200d}👧 🇺🇸\nitem\nnested\n\nlast",
    );
    style(
        &mut editor,
        0,
        "<b>Hi</b>",
        StylePatch {
            bold: Some(true),
            foreground: Some(Some(Color([0, 128, 255, 127]))),
            ..Default::default()
        },
    );
    style(
        &mut editor,
        0,
        "\"世界\" e\u{301}",
        StylePatch {
            italic: Some(true),
            underline: Some(true),
            background: Some(Some(Color([255, 255, 0, 64]))),
            ..Default::default()
        },
    );
    style(
        &mut editor,
        0,
        "👨\u{200d}👩\u{200d}👧",
        StylePatch {
            strikethrough: Some(true),
            code: Some(true),
            foreground: Some(Some(Color([1, 2, 3, 4]))),
            background: Some(Some(Color::YELLOW)),
            ..Default::default()
        },
    );
    for (paragraph, text, target) in [
        (0, "Hi</b> & \"世界", "https://example.com/?q=世界&x=\"1\""),
        (0, "🇺🇸", "mailto:a@b"),
        (2, "nested", "https://example.com"),
    ] {
        let start = editor
            .document()
            .paragraph(paragraph)
            .expect("each case names a paragraph of the fixture text")
            .text()
            .find(text)
            .expect("each case names text that occurs in its paragraph");
        editor
            .set_selection(Selection::new(
                Position::new(paragraph, start),
                Position::new(paragraph, start + text.len()),
            ))
            .expect("a substring found in the paragraph has grapheme boundaries at both ends");
        let link =
            Link::new(target).expect("each target is a short URL without control characters");
        editor
            .set_link(Some(link))
            .expect("no IME composition is active");
    }
    style(
        &mut editor,
        4,
        "last",
        StylePatch {
            bold: Some(true),
            ..Default::default()
        },
    );
    for (paragraph, kind) in [
        (0, ParagraphKind::Heading { level: 6 }),
        (1, ParagraphKind::Bullet { indent: 0 }),
        (
            2,
            ParagraphKind::Ordered {
                indent: 2,
                start: Some(u32::MAX),
            },
        ),
        (3, ParagraphKind::Quote),
    ] {
        editor
            .set_selection(Selection::caret(Position::new(paragraph, 0)))
            .expect("each case names a paragraph of the fixture text");
        editor
            .set_paragraph_kind(kind)
            .expect("no IME composition is active, and heading level 6 is valid");
    }
    Fragment::from_document(editor.document())
}

const BYTES: [u8; 10] = [0, 1, 2, 3, 6, 7, 9, 0x7f, 0x80, 0xff];
const LENGTHS: [u64; 8] = [0, 1, 2, 9, 17, 1_000_000, 1_000_001, u64::MAX];

fn mutate(bytes: &mut Vec<u8>, seeds: &[Vec<u8>], generator: &mut Generator) {
    if bytes.is_empty() {
        bytes.push(generator.next() as u8);
        return;
    }
    let index = generator.below(bytes.len());
    let end = (index + 1 + generator.below(8)).min(bytes.len());
    match generator.below(8) {
        0 => bytes[index] = BYTES[generator.below(BYTES.len())],
        1 => bytes[index] = generator.next() as u8,
        2 => bytes[index] = bytes[index].wrapping_add(1),
        3 => bytes[index] = bytes[index].wrapping_sub(1),
        4 => {
            bytes.drain(index..end);
        }
        5 => {
            let copy = bytes[index..end].to_vec();
            bytes.splice(index..index, copy);
        }
        6 => {
            let end = (index + 8).min(bytes.len());
            let length = match generator.below(3) {
                0 => LENGTHS[generator.below(LENGTHS.len())],
                1 => (bytes.len() as u64).wrapping_sub(generator.below(32) as u64),
                _ => generator.below(64) as u64,
            };
            bytes[index..end].copy_from_slice(&length.to_le_bytes()[..end - index]);
        }
        _ => {
            let other = &seeds[generator.below(seeds.len())];
            let from = generator.below(other.len());
            bytes.truncate(index);
            bytes.extend_from_slice(&other[from..]);
        }
    }
}

/// The list number that a version 1 fragment stores for an ordered paragraph.
struct StoredOrdinal {
    paragraph: usize,
    ordinal: u32,
}

/// Returns the list numbers stored in `bytes`, a version 1 fragment that
/// [`Fragment::from_bytes`] accepted.
fn version_one_ordinals(bytes: &[u8]) -> Vec<StoredOrdinal> {
    let length = |at: usize| {
        let field: [u8; 8] = bytes[at..at + 8]
            .try_into()
            .expect("a range of eight bytes converts to an eight-byte array");
        u64::from_le_bytes(field) as usize
    };
    // The paragraph count follows the signature and version byte, and the first
    // paragraph follows the count.
    let mut at = 13;
    let mut ordinals = Vec::new();
    for paragraph in 0..length(5) {
        at += 1;
        match bytes[at - 1] {
            0 => {}
            1 | 2 => at += 1,
            _ => {
                let field: [u8; 4] = bytes[at + 1..at + 5]
                    .try_into()
                    .expect("a range of four bytes converts to a four-byte array");
                ordinals.push(StoredOrdinal {
                    paragraph,
                    ordinal: u32::from_le_bytes(field),
                });
                at += 5;
            }
        }
        at += 8 + length(at);
        let spans = length(at);
        at += 8;
        for _ in 0..spans {
            at += if bytes[at + 8] & 32 != 0 { 13 } else { 9 };
        }
    }
    ordinals
}

#[test]
fn randomly_corrupted_tlfr_is_rejected_or_canonical_and_usable() {
    let seeds = [
        include_bytes!("fixtures/native-v1.tlfr").to_vec(),
        include_bytes!("fixtures/native-v2.tlfr").to_vec(),
        styled_fragment().to_bytes(),
        Fragment::from_text("a\n\nb").to_bytes(),
    ];
    let mut generator = Generator(0x9e37_79b9_7f4a_7c15);
    let cases = 100_000;
    let mut accepted = 0;
    let mut versions = [0; 2];
    for case in 0..cases {
        let mut bytes = seeds[case % seeds.len()].clone();
        for _ in 0..=generator.below(4) {
            mutate(&mut bytes, &seeds, &mut generator);
        }
        let Ok(fragment) = Fragment::from_bytes(&bytes) else {
            continue;
        };
        accepted += 1;
        let canonical = fragment.to_bytes();
        if bytes[4] == 1 {
            versions[0] += 1;
            assert_eq!(canonical[4], 2, "case {case}");
            assert_eq!(
                Fragment::from_bytes(&canonical).unwrap(),
                fragment,
                "case {case}"
            );
            let document = Document::from_bytes(&bytes).unwrap();
            for stored in version_one_ordinals(&bytes) {
                assert_eq!(
                    document.list_number(stored.paragraph),
                    Some(stored.ordinal),
                    "case {case}"
                );
            }
        } else {
            versions[1] += 1;
            assert_eq!(canonical, bytes, "case {case}");
        }
        assert_eq!(
            Document::from_bytes(&bytes).unwrap().to_bytes(),
            canonical,
            "case {case}"
        );

        let mut editor = Editor::from_text("before after");
        editor
            .set_selection(Selection::caret(Position::new(0, 7)))
            .unwrap();
        let original = editor.document().to_bytes();
        editor.insert_fragment(&fragment).unwrap();
        let document = editor.document();
        // Exporting any accepted fragment must not panic.
        let _ = document.to_html();
        let saved = document.to_bytes();
        assert_eq!(
            Document::from_bytes(&saved).unwrap().to_bytes(),
            saved,
            "case {case}"
        );
        while editor.undo() {}
        assert_eq!(editor.document().to_bytes(), original, "case {case}");
    }
    assert!(accepted > cases / 100, "only {accepted} of {cases} decoded");
    assert!(
        versions.iter().all(|&count| count > cases / 1000),
        "decoded versions: {versions:?}"
    );
}
