use std::{hint::black_box, time::Instant};
use textloom::{Editor, Position, Selection};

fn main() {
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
