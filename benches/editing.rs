use std::{hint::black_box, time::Instant};
use textloom::{Document, Editor, Movement, Position, SearchOptions, Selection};

fn main() {
    short_paragraph_edits();
    unicode_navigation();
    batch_replacement();
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
