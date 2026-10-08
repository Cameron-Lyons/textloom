//! Headless egui frame benchmark. Run with `cargo bench --features egui --bench rendering`.
use std::{hint::black_box, time::Instant};
use textloom::{Editor, Position, Selection, adapter::egui::RichTextEditor};

fn frame(context: &egui::Context, editor: &mut Editor) {
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    black_box(ui.add(RichTextEditor::new(editor).id_salt("bench")));
                });
            });
        },
    );
    black_box(output).drop_without_applying_deltas();
}

fn main() {
    for paragraphs in [100, 10_000] {
        // Unique paragraphs exercise the galley cache instead of one repeated job.
        let source = (0..paragraphs)
            .map(|index| format!("Paragraph {index}: Unicode café 👩‍💻, with some words.\n"))
            .collect::<String>();
        let mut editor = Editor::from_text(&source);
        let context = egui::Context::default();
        editor
            .set_selection(Selection::caret(Position::new(paragraphs / 2, 5)))
            .unwrap();
        for _ in 0..3 {
            frame(&context, &mut editor);
        }
        let iterations = if paragraphs == 100 { 1_000 } else { 100 };
        let start = Instant::now();
        for _ in 0..iterations {
            frame(&context, &mut editor);
        }
        let idle = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64;
        let start = Instant::now();
        for _ in 0..iterations {
            editor.insert_text(black_box("x")).unwrap();
            frame(&context, &mut editor);
            editor.break_history_group();
            assert!(editor.undo());
            frame(&context, &mut editor);
        }
        let edit = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64 / 2.0;
        println!(
            "{paragraphs:>6} paragraphs: {idle:.2} µs / idle frame; {edit:.2} µs / local edit + frame"
        );
    }
}
