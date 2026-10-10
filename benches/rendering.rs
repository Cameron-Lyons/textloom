//! Headless egui frame benchmark. Run with `cargo bench --features egui --bench rendering`.
//! Native rendering, GPU submission, and platform assistive technology are excluded.
//! Initial-frame timers exclude editor/context construction and include lazy font setup.
//! Invalidation setters run before the timed frame; each relayout follows warmup.
use std::{
    hint::black_box,
    time::{Duration, Instant},
};
use textloom::{Editor, Position, Selection, adapter::egui::RichTextEditor};

#[derive(Clone, Copy, Default)]
struct FrameConfig {
    width: Option<f32>,
    pixels_per_point: Option<f32>,
}

fn frame(context: &egui::Context, editor: &mut Editor) {
    frame_with_events(context, editor, Vec::new());
}

fn frame_with_events(context: &egui::Context, editor: &mut Editor, events: Vec<egui::Event>) {
    frame_config(context, editor, events, FrameConfig::default());
}

fn frame_config(
    context: &egui::Context,
    editor: &mut Editor,
    events: Vec<egui::Event>,
    config: FrameConfig,
) {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        events,
        ..Default::default()
    };
    if let Some(pixels_per_point) = config.pixels_per_point {
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(pixels_per_point);
    }
    let output = context.run_ui(input, |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let mut widget = RichTextEditor::new(editor).id(egui::Id::new("bench"));
                if let Some(width) = config.width {
                    widget = widget.desired_width(width);
                }
                let result = widget.show(ui);
                assert!(result.errors.is_empty(), "{:?}", result.errors);
                assert!(
                    result.accessibility_errors.is_empty(),
                    "{:?}",
                    result.accessibility_errors
                );
                black_box(result);
            });
        });
    });
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
        let start = Instant::now();
        for _ in 0..iterations {
            editor.insert_paragraph().unwrap();
            frame(&context, &mut editor);
            assert!(editor.undo());
            frame(&context, &mut editor);
        }
        let split = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64 / 2.0;
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
            "{paragraphs:>6} paragraphs: {idle:.2} µs / idle frame; {edit:.2} µs / local edit + frame; {split:.2} µs / paragraph split or undo + frame; {preedit:.2} µs / preedit update + frame; {cursor:.2} µs / preedit cursor + frame"
        );
    }
    accessibility_frames();
    native_preedit_frames();
    long_preedit_frames();
    initialization_and_invalidation_frames();
}

fn initialization_and_invalidation_frames() {
    for paragraphs in [1, 100, 10_000] {
        let initial = initial_frames(paragraphs, false);
        let accessible_initial = initial_frames(paragraphs, true);
        let width = invalidated_frames(paragraphs, |_, index| FrameConfig {
            width: Some(if index % 2 == 0 { 700.0 } else { 320.0 }),
            ..FrameConfig::default()
        });
        let theme = invalidated_frames(paragraphs, |context, index| {
            context.set_visuals(if index % 2 == 0 {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            FrameConfig::default()
        });
        let default_fonts = egui::FontDefinitions::default();
        let mut alternate_fonts = default_fonts.clone();
        alternate_fonts.families.insert(
            egui::FontFamily::Proportional,
            alternate_fonts.families[&egui::FontFamily::Monospace].clone(),
        );
        let definitions = [default_fonts, alternate_fonts];
        let font = invalidated_frames(paragraphs, |context, index| {
            context.set_fonts(definitions[index % 2].clone());
            FrameConfig::default()
        });
        let dpi = invalidated_frames(paragraphs, |_, index| FrameConfig {
            pixels_per_point: Some(if index % 2 == 0 { 1.0 } else { 2.0 }),
            ..FrameConfig::default()
        });
        println!(
            "{paragraphs:>6} paragraphs: {initial:.2} µs / initial frame; {accessible_initial:.2} µs / accessible initial frame; {width:.2} µs / width invalidation + frame; {theme:.2} µs / theme invalidation + frame; {font:.2} µs / font invalidation + frame; {dpi:.2} µs / DPI invalidation + frame"
        );
    }
}

fn initial_frames(paragraphs: usize, accessible: bool) -> f64 {
    let source = document_text(paragraphs);
    let iterations = 20;
    let mut elapsed = Duration::ZERO;
    for _ in 0..iterations {
        let mut editor = Editor::from_text(&source);
        let context = egui::Context::default();
        if accessible {
            context.enable_accesskit();
        }
        let start = Instant::now();
        frame(&context, &mut editor);
        elapsed += start.elapsed();
        assert_eq!(editor.document().plain_text(), source);
    }
    elapsed.as_secs_f64() * 1_000_000.0 / iterations as f64
}

fn invalidated_frames(
    paragraphs: usize,
    mut configure: impl FnMut(&egui::Context, usize) -> FrameConfig,
) -> f64 {
    let source = document_text(paragraphs);
    let mut editor = Editor::from_text(&source);
    let context = egui::Context::default();
    // Warm both alternating configurations and finish on the second one.
    for index in 0..6 {
        let config = configure(&context, index);
        frame_config(&context, &mut editor, Vec::new(), config);
    }
    let iterations = 20;
    let mut elapsed = Duration::ZERO;
    for index in 0..iterations {
        let config = configure(&context, index);
        let start = Instant::now();
        frame_config(&context, &mut editor, Vec::new(), config);
        elapsed += start.elapsed();
    }
    assert_eq!(editor.document().plain_text(), source);
    elapsed.as_secs_f64() * 1_000_000.0 / iterations as f64
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

fn long_preedit_frames() {
    let text = "café 👩‍💻 ".repeat(10_000);
    let changed_text = format!("{text}x");
    let context = egui::Context::default();
    let mut editor = Editor::default();
    frame(&context, &mut editor);
    context.memory_mut(|memory| memory.request_focus(egui::Id::new("bench")));
    editor.update_composition(&text, Some(0..0)).unwrap();
    for _ in 0..3 {
        frame(&context, &mut editor);
    }
    for mode in ["idle", "identical update", "cursor update", "text update"] {
        let iterations = if mode == "text update" { 20 } else { 200 };
        let start = Instant::now();
        for index in 0..iterations {
            match mode {
                "idle" => {}
                "identical update" => editor
                    .update_composition(black_box(&text), Some(0..0))
                    .unwrap(),
                "cursor update" => {
                    let end = if index % 2 == 0 { 0 } else { text.len() };
                    editor
                        .update_composition(black_box(&text), Some(end..end))
                        .unwrap();
                }
                "text update" => {
                    let text = if index % 2 == 0 { &changed_text } else { &text };
                    editor
                        .update_composition(black_box(text), Some(0..0))
                        .unwrap();
                }
                _ => unreachable!(),
            }
            frame(&context, &mut editor);
            assert_eq!(
                editor.composition().unwrap().text.len(),
                text.len() + usize::from(mode == "text update" && index % 2 == 0)
            );
        }
        let elapsed = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64;
        assert_eq!(editor.composition().unwrap().text, text);
        println!("long Unicode preedit: {elapsed:.2} µs / {mode} + frame");
    }
    editor.cancel_composition();
    assert_eq!(editor.document().plain_text(), "");
}
