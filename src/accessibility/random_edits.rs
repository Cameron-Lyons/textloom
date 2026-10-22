//! Random edit streams for the differential tests of both AccessKit bridges.

use std::ops::Range;

use crate::model::{self, SparseCoverage};
use crate::{Editor, Link, ParagraphKind, Position, SearchOptions, Selection, StylePatch};

pub(crate) fn seeded(mut seed: u64) -> impl FnMut(usize) -> usize {
    move |limit| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % limit as u64) as usize
    }
}

pub(crate) fn editor() -> Editor {
    let items: Vec<_> = (1..=11)
        .map(|item| format!("item {item} alpha beta\u{2028}gamma delta epsilon"))
        .collect();
    let mut editor = Editor::from_text(&format!(
        "intro é 👩\u{200d}💻 text\n{}\nquoted one\nquoted two\nlast",
        items.join("\n")
    ));
    set_kinds(
        &mut editor,
        [
            (
                1..12,
                ParagraphKind::Ordered {
                    indent: 0,
                    start: None,
                },
            ),
            (
                1..2,
                ParagraphKind::Ordered {
                    indent: 0,
                    start: Some(995),
                },
            ),
            (
                4..7,
                ParagraphKind::Ordered {
                    indent: 1,
                    start: None,
                },
            ),
            (12..14, ParagraphKind::Quote),
        ],
    );
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 5)))
        .unwrap();
    editor
        .set_link(Some(Link::try_new("https://a.example").unwrap()))
        .unwrap();
    editor
}

pub(crate) fn sparse_editor() -> Editor {
    let text: Vec<_> = (0..48)
        .map(|index| {
            if index % 12 == 5 {
                format!("tok {index} é")
            } else {
                format!("plain {index} words")
            }
        })
        .collect();
    let mut editor = Editor::from_text(&text.join("\n"));
    let ordered = |indent, start| ParagraphKind::Ordered { indent, start };
    set_kinds(
        &mut editor,
        [
            (0..16, ordered(0, None)),
            (6..10, ordered(1, None)),
            (16..17, ordered(0, Some(7))),
            (20..28, ParagraphKind::Bullet { indent: 0 }),
            (30..38, ParagraphKind::Quote),
            (42..43, ParagraphKind::Heading { level: 2 }),
        ],
    );
    editor
}

fn set_kinds(editor: &mut Editor, kinds: impl IntoIterator<Item = (Range<usize>, ParagraphKind)>) {
    for (paragraphs, kind) in kinds {
        editor
            .set_selection(Selection::new(
                Position::new(paragraphs.start, 0),
                Position::new(paragraphs.end - 1, 1),
            ))
            .unwrap();
        editor.set_paragraph_kind(kind).unwrap();
    }
}

fn position(editor: &Editor, random: &mut impl FnMut(usize) -> usize) -> Position {
    let paragraphs = editor.document().paragraphs();
    let index = random(paragraphs.len());
    let paragraph = &paragraphs[index];
    let byte = paragraph
        .byte_from_grapheme(random(paragraph.grapheme_count() + 1))
        .unwrap();
    Position::new(index, byte)
}

pub(crate) fn apply(editor: &mut Editor, random: &mut impl FnMut(usize) -> usize) {
    let kinds = [
        ParagraphKind::Body,
        ParagraphKind::Quote,
        ParagraphKind::Heading { level: 2 },
        ParagraphKind::Bullet { indent: 1 },
        ParagraphKind::Ordered {
            indent: 0,
            start: None,
        },
        ParagraphKind::Ordered {
            indent: 1,
            start: None,
        },
        ParagraphKind::Ordered {
            indent: 2,
            start: Some(9),
        },
        ParagraphKind::Ordered {
            indent: 0,
            start: Some(998),
        },
    ];
    let kind = kinds[random(kinds.len())];
    let link = Link::try_new(["https://a.example", "https://b.example"][random(2)]).unwrap();
    let anchor = position(editor, random);
    let focus = position(editor, random);
    let range = Selection::new(anchor, focus);
    let _ = editor.set_selection(Selection::caret(focus));
    let _ = match random(16) {
        0 => {
            editor.insert_text(["x", "é", "ab cd", "👩\u{200d}💻", "x\ny", "a\u{2028}b"][random(6)])
        }
        1 => editor.insert_paragraph(),
        2 => editor.insert_line_break(),
        3 => editor.delete_backward(),
        4 => editor.delete_forward(),
        5 => {
            if random(2) == 0 {
                let _ = editor.set_selection(range);
            }
            editor.set_paragraph_kind(kind)
        }
        6 => {
            let _ = editor.set_selection(range);
            match random(3) {
                0 => editor.indent_list(),
                1 => editor.outdent_list(),
                _ => editor.apply_numbered_list(0),
            }
        }
        7 => {
            editor.undo();
            Ok(())
        }
        8 => {
            editor.redo();
            Ok(())
        }
        9 => {
            let _ = editor.set_selection(range);
            editor.set_link((random(3) != 0).then_some(link))
        }
        10 => editor.insert_link("site", link),
        11 => {
            let _ = editor.set_selection(range);
            editor.apply_style(StylePatch {
                bold: Some(random(2) == 0),
                ..StylePatch::default()
            })
        }
        12 => editor.transaction(|editor| {
            editor.insert_paragraph()?;
            editor.insert_text("t")?;
            editor.set_paragraph_kind(kind)
        }),
        13 => {
            let paragraph = &editor.document().paragraphs()[focus.paragraph];
            let end = Position::new(focus.paragraph, paragraph.text().len());
            let fragment = editor
                .document()
                .fragment(Position::new(focus.paragraph, 0)..end)
                .unwrap();
            editor.insert_fragment(&fragment)
        }
        14 => {
            let _ = editor.set_selection(Selection::caret(editor.document().end()));
            editor.insert_paragraph()
        }
        _ => {
            editor.break_history_group();
            let _ = editor.set_selection(range);
            editor.delete_backward()
        }
    };
    if random(3) == 0 {
        let selection = Selection::new(position(editor, random), position(editor, random));
        let _ = editor.set_selection(selection);
    }
}

// Edits far-apart token paragraphs so that most changes, and their undo and redo, record
// several runs, some of which add or join paragraphs.
pub(crate) fn apply_sparse(editor: &mut Editor, random: &mut impl FnMut(usize) -> usize) {
    let kinds = [
        ParagraphKind::Body,
        ParagraphKind::Quote,
        ParagraphKind::Bullet { indent: 0 },
        ParagraphKind::Ordered {
            indent: 0,
            start: None,
        },
    ];
    let kind = kinds[random(kinds.len())];
    let bold = StylePatch {
        bold: Some(random(2) == 0),
        ..StylePatch::default()
    };
    let options = SearchOptions::default();
    let tokens = |editor: &Editor| -> Vec<usize> {
        let paragraphs = editor.document().paragraphs().iter();
        paragraphs
            .enumerate()
            .filter(|(_, paragraph)| paragraph.text().contains("tok"))
            .map(|(index, _)| index)
            .collect()
    };
    let _ = match random(14) {
        0 | 1 => editor.replace_all("tok", "tok\nnew", options).map(drop),
        2 | 3 => editor.replace_all("tok\nnew", "tok", options).map(drop),
        4 => editor.replace_all("new", "", options).map(drop),
        5 | 6 => {
            let marked = tokens(editor);
            editor.transaction(|editor| {
                for index in marked {
                    let end = editor.document().paragraphs()[index].text().len();
                    editor.set_selection(Selection::new(
                        Position::new(index, 0),
                        Position::new(index, end),
                    ))?;
                    editor.set_paragraph_kind(kind)?;
                    editor.apply_style(bold)?;
                }
                Ok(())
            })
        }
        7 => {
            editor.select_all();
            editor.set_paragraph_kind(kind)
        }
        8 => {
            editor.select_all();
            editor.apply_style(bold)
        }
        9 => {
            editor.select_all();
            match random(3) {
                0 => editor.indent_list(),
                1 => editor.outdent_list(),
                _ => editor.apply_numbered_list(0),
            }
        }
        10 | 11 => {
            editor.undo();
            Ok(())
        }
        12 => {
            editor.redo();
            Ok(())
        }
        _ => {
            apply(editor, random);
            Ok(())
        }
    };
    if tokens(editor).len() < 3 {
        let paragraphs = editor.document().paragraphs().len();
        let _ = editor.transaction(|editor| {
            for index in (5..paragraphs).step_by(12) {
                editor.set_selection(Selection::caret(Position::new(index, 0)))?;
                editor.insert_text("tok ")?;
            }
            Ok(())
        });
    }
    let missing = 40usize.saturating_sub(editor.document().paragraphs().len());
    if missing > 0 {
        let _ = editor.set_selection(Selection::caret(editor.document().end()));
        let _ = editor.insert_text(&"\nfiller words".repeat(missing));
    }
}

pub(crate) fn assert_sparse_coverage(minimum: SparseCoverage) {
    let coverage = model::take_sparse_coverage();
    assert!(
        coverage.recorded >= minimum.recorded
            && coverage.replayed >= minimum.replayed
            && coverage.shifted_tail >= minimum.shifted_tail,
        "too few sparse deltas recorded, replayed, or applied with a shifted tail: \
         {coverage:?}, expected at least {minimum:?}"
    );
}
