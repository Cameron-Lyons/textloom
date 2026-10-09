//! Headless egui frame benchmark. Run with `cargo bench --features egui --bench rendering`.
use std::{hint::black_box, time::Instant};
use textloom::{Editor, Position, Selection, adapter::egui::RichTextEditor};

fn frame(context: &egui::Context, editor: &mut Editor) {
    frame_with_events(context, editor, Vec::new());
}

fn frame_with_events(context: &egui::Context, editor: &mut Editor, events: Vec<egui::Event>) {
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let result = RichTextEditor::new(editor)
                        .id(egui::Id::new("bench"))
                        .show(ui);
                    assert!(result.errors.is_empty(), "{:?}", result.errors);
                    assert!(
                        result.accessibility_errors.is_empty(),
                        "{:?}",
                        result.accessibility_errors
                    );
                    black_box(result);
                });
            });
        },
    );
    black_box(output).drop_without_applying_deltas();
}

fn main() {
    for paragraphs in [100, 10_000] {
        // Unique paragraphs exercise the galley cache instead of one repeated job.
        let source = document_text(paragraphs);
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
        context.memory_mut(|memory| memory.request_focus(egui::Id::new("bench")));
        editor.begin_composition();
        editor.update_composition("preedit", Some(0..7)).unwrap();
        for _ in 0..3 {
            frame(&context, &mut editor);
            assert!(editor.composition().is_some());
        }
        let start = Instant::now();
        for index in 0..iterations {
            let text = if index % 2 == 0 {
                "preedit x"
            } else {
                "preedit"
            };
            editor
                .update_composition(black_box(text), Some(0..text.len()))
                .unwrap();
            frame(&context, &mut editor);
            assert!(editor.composition().is_some());
        }
        let preedit = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64;
        editor.update_composition("preedit", Some(0..7)).unwrap();
        for _ in 0..3 {
            frame(&context, &mut editor);
        }
        let start = Instant::now();
        for index in 0..iterations {
            let end = if index % 2 == 0 { 3 } else { 7 };
            editor
                .update_composition(black_box("preedit"), Some(0..end))
                .unwrap();
            frame(&context, &mut editor);
            assert!(editor.composition().is_some());
        }
        let cursor = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64;
        editor.cancel_composition();
        assert_eq!(editor.document().plain_text(), source);
        println!(
            "{paragraphs:>6} paragraphs: {idle:.2} µs / idle frame; {edit:.2} µs / local edit + frame; {preedit:.2} µs / preedit update + frame; {cursor:.2} µs / preedit cursor + frame"
        );
    }
    accessibility_frames();
    native_preedit_frames();
}

fn document_text(paragraphs: usize) -> String {
    (0..paragraphs)
        .map(|index| format!("Paragraph {index}: Unicode café 👩‍💻, with some words.\n"))
        .collect()
}

fn accessibility_frames() {
    for paragraphs in [100, 10_000] {
        let source = document_text(paragraphs);
        let mut editor = Editor::from_text(&source);
        let context = egui::Context::default();
        context.enable_accesskit();
        editor
            .set_selection(Selection::caret(Position::new(paragraphs / 2, 5)))
            .unwrap();
        for _ in 0..3 {
            frame(&context, &mut editor);
        }
        let iterations = if paragraphs == 100 { 1_000 } else { 100 };
        let start = Instant::now();
        for _ in 0..iterations {
            editor.insert_text(black_box("x")).unwrap();
            frame(&context, &mut editor);
            editor.break_history_group();
            assert!(editor.undo());
            frame(&context, &mut editor);
        }
        let edit = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64 / 2.0;
        assert_eq!(editor.document().plain_text(), source);
        println!("{paragraphs:>6} paragraphs: {edit:.2} µs / accessible local edit + frame");
    }
}

fn native_preedit_frames() {
    let text = "café 👩‍💻 ".repeat(10_000);
    let scalars = text.chars().count();
    for (label, scalar, byte) in [("start", 0, 0), ("end", scalars, text.len())] {
        let context = egui::Context::default();
        let mut editor = Editor::default();
        frame(&context, &mut editor);
        context.memory_mut(|memory| memory.request_focus(egui::Id::new("bench")));
        let event = egui::Event::Ime(egui::ImeEvent::Preedit {
            text: text.clone(),
            active_range_chars: Some(scalar..scalar),
        });
        for _ in 0..3 {
            frame_with_events(&context, &mut editor, vec![event.clone()]);
        }
        assert_eq!(editor.composition().unwrap().selection, Some(byte..byte));
        let start = Instant::now();
        for _ in 0..100 {
            frame_with_events(&context, &mut editor, vec![black_box(event.clone())]);
            assert_eq!(editor.composition().unwrap().selection, Some(byte..byte));
        }
        let elapsed = start.elapsed().as_secs_f64() * 1_000_000.0 / 100.0;
        editor.cancel_composition();
        assert_eq!(editor.document().plain_text(), "");
        println!("long Unicode preedit: {elapsed:.2} µs / native cursor at {label} + frame");
    }
}
