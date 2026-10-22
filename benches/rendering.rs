//! Measures headless egui frames of `RichTextEditor`: idle frames, local edits,
//! IME preedit updates, accessibility updates, batched typing, initial layout,
//! and relayout after width, theme, font, and DPI changes.
//!
//! Each benchmark prints the mean time per frame in microseconds. Setting
//! `TEXTLOOM_BENCH_QUICK=1` runs a tenth of the iterations as a smoke test.

use std::time::{Duration, Instant};
use std::{env, hint};
use textloom::adapter::egui::RichTextEditor;
use textloom::{Editor, Position, Selection};

/// `expect` message for editing calls, which fail only while an IME
/// composition is active.
const NO_COMPOSITION: &str = "no IME composition is active";

/// `expect` message for carets placed inside a paragraph from [`document_text`].
const IN_FIRST_WORD: &str = "every paragraph starts with the ASCII word \"Paragraph\"";

/// Returns `iterations`, or a tenth of it (at least two) when the
/// `TEXTLOOM_BENCH_QUICK` environment variable is set to a nonempty value.
fn scaled(iterations: usize) -> usize {
    let quick = env::var_os("TEXTLOOM_BENCH_QUICK").is_some_and(|value| !value.is_empty());
    if quick {
        (iterations / 10).max(2)
    } else {
        iterations
    }
}

/// Per-frame overrides of the widget width and the display scale.
#[derive(Clone, Copy, Default)]
struct FrameConfig {
    /// The widget's desired width in points, or `None` for the available width.
    width: Option<f32>,
    /// The native pixels per point of the root viewport, or `None` for egui's default.
    pixels_per_point: Option<f32>,
}

/// Renders one frame without input events.
fn frame(context: &egui::Context, editor: &mut Editor) {
    frame_with_events(context, editor, Vec::new());
}

/// Renders one frame that delivers `events` to the widget.
fn frame_with_events(context: &egui::Context, editor: &mut Editor, events: Vec<egui::Event>) {
    frame_config(context, editor, events, FrameConfig::default());
}

/// Renders one 800×600 frame with the editor in a vertical scroll area.
///
/// # Panics
///
/// Panics if the widget reports editing or accessibility errors.
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
                let mut widget = RichTextEditor::new(editor).id(egui::Id::unique("bench"));
                if let Some(width) = config.width {
                    widget = widget.desired_width(width);
                }
                let result = widget.show(ui);
                assert!(
                    result.errors.is_empty(),
                    "benchmark input should never make an edit fail: {:?}",
                    result.errors
                );
                assert!(
                    result.accessibility_errors.is_empty(),
                    "accessibility updates should never fail: {:?}",
                    result.accessibility_errors
                );
                hint::black_box(result);
            });
        });
    });
    hint::black_box(output).drop_without_applying_deltas();
}

fn main() {
    for paragraphs in [100, 10_000] {
        let source = document_text(paragraphs);
        let mut editor = Editor::from_text(&source);
        let context = egui::Context::default();
        editor
            .set_selection(Selection::caret(Position::new(paragraphs / 2, 5)))
            .expect(IN_FIRST_WORD);
        for _ in 0..3 {
            frame(&context, &mut editor);
        }
        let iterations = scaled(if paragraphs == 100 { 1_000 } else { 100 });
        let start = Instant::now();
        for _ in 0..iterations {
            frame(&context, &mut editor);
        }
        let idle = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64;
        let start = Instant::now();
        for _ in 0..iterations {
            editor
                .insert_text(hint::black_box("x"))
                .expect(NO_COMPOSITION);
            frame(&context, &mut editor);
            editor.break_history_group();
            assert!(editor.undo(), "the insertion should be on the undo stack");
            frame(&context, &mut editor);
        }
        let edit = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64 / 2.0;
        let start = Instant::now();
        for _ in 0..iterations {
            editor.insert_paragraph().expect(NO_COMPOSITION);
            frame(&context, &mut editor);
            assert!(
                editor.undo(),
                "the paragraph split should be on the undo stack"
            );
            frame(&context, &mut editor);
        }
        let split = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64 / 2.0;
        context.memory_mut(|memory| memory.request_focus(egui::Id::unique("bench")));
        editor.begin_composition();
        editor
            .update_composition("preedit", Some(0..7))
            .expect("the cursor range covers the ASCII preedit text");
        for _ in 0..3 {
            frame(&context, &mut editor);
            assert!(
                editor.composition().is_some(),
                "rendering the focused widget should keep its composition"
            );
        }
        let start = Instant::now();
        for index in 0..iterations {
            let text = if index % 2 == 0 {
                "preedit x"
            } else {
                "preedit"
            };
            editor
                .update_composition(hint::black_box(text), Some(0..text.len()))
                .expect("the cursor range covers the ASCII preedit text");
            frame(&context, &mut editor);
            assert!(
                editor.composition().is_some(),
                "rendering the focused widget should keep its composition"
            );
        }
        let preedit = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64;
        editor
            .update_composition("preedit", Some(0..7))
            .expect("the cursor range covers the ASCII preedit text");
        for _ in 0..3 {
            frame(&context, &mut editor);
        }
        let start = Instant::now();
        for index in 0..iterations {
            let end = if index % 2 == 0 { 3 } else { 7 };
            editor
                .update_composition(hint::black_box("preedit"), Some(0..end))
                .expect("the cursor range lies within the ASCII preedit text");
            frame(&context, &mut editor);
            assert!(
                editor.composition().is_some(),
                "rendering the focused widget should keep its composition"
            );
        }
        let cursor = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64;
        editor.cancel_composition();
        assert_eq!(
            editor.document().plain_text(),
            source,
            "every edit should have been undone and the composition canceled"
        );
        println!(
            "{paragraphs:>6} paragraphs: {idle:.2} µs / idle frame; {edit:.2} µs / local edit + frame; {split:.2} µs / paragraph split or undo + frame; {preedit:.2} µs / preedit update + frame; {cursor:.2} µs / preedit cursor + frame"
        );
    }
    formatted_idle_frames();
    accessibility_frames();
    formatted_accessibility_frames();
    native_preedit_frames();
    long_preedit_frames();
    batched_typing_frames();
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

/// Returns the mean time in microseconds of the first frame of a new context,
/// with AccessKit enabled if `accessible` is true.
fn initial_frames(paragraphs: usize, accessible: bool) -> f64 {
    let source = document_text(paragraphs);
    let iterations = scaled(20);
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
        assert_eq!(
            editor.document().plain_text(),
            source,
            "rendering without input should not edit the document"
        );
    }
    elapsed.as_secs_f64() * 1_000_000.0 / iterations as f64
}

/// Returns the mean time in microseconds of a frame after `configure` changes
/// the context or frame settings for that frame's index.
fn invalidated_frames(
    paragraphs: usize,
    mut configure: impl FnMut(&egui::Context, usize) -> FrameConfig,
) -> f64 {
    let source = document_text(paragraphs);
    let mut editor = Editor::from_text(&source);
    let context = egui::Context::default();
    for index in 0..6 {
        let config = configure(&context, index);
        frame_config(&context, &mut editor, Vec::new(), config);
    }
    let iterations = scaled(20);
    let mut elapsed = Duration::ZERO;
    for index in 0..iterations {
        let config = configure(&context, index);
        let start = Instant::now();
        frame_config(&context, &mut editor, Vec::new(), config);
        elapsed += start.elapsed();
    }
    assert_eq!(
        editor.document().plain_text(),
        source,
        "rendering without input should not edit the document"
    );
    elapsed.as_secs_f64() * 1_000_000.0 / iterations as f64
}

fn batched_typing_frames() {
    let key = |key| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    for paragraphs in [10_000, 100_000] {
        let source = document_text(paragraphs);
        let mut editor = Editor::from_text(&source);
        let context = egui::Context::default();
        editor
            .set_selection(Selection::caret(Position::new(paragraphs / 2, 5)))
            .expect(IN_FIRST_WORD);
        frame(&context, &mut editor);
        context.memory_mut(|memory| memory.request_focus(egui::Id::unique("bench")));
        for _ in 0..3 {
            frame(&context, &mut editor);
        }
        let mut results = Vec::new();
        for events in [1, 2, 4, 8] {
            let typed = vec![egui::Event::Text("x".to_owned()); events];
            let deleted = vec![key(egui::Key::Backspace); events];
            for _ in 0..20 {
                frame_with_events(&context, &mut editor, typed.clone());
                frame_with_events(&context, &mut editor, deleted.clone());
            }
            let iterations = scaled(100);
            let start = Instant::now();
            for _ in 0..iterations {
                frame_with_events(&context, &mut editor, hint::black_box(typed.clone()));
                frame_with_events(&context, &mut editor, hint::black_box(deleted.clone()));
            }
            let elapsed = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64 / 2.0;
            assert_eq!(
                editor.document().plain_text(),
                source,
                "each frame of {events} backspaces should delete the {events} characters typed before it"
            );
            results.push(format!("{elapsed:.2} µs / {events}-edit frame"));
        }
        println!("{paragraphs:>6} paragraphs: {}", results.join("; "));
    }
}

/// Returns `paragraphs` distinct, newline-terminated paragraphs of mixed
/// Unicode text, so the document ends with an empty paragraph.
fn document_text(paragraphs: usize) -> String {
    (0..paragraphs)
        .map(|index| format!("Paragraph {index}: Unicode café 👩\u{200d}💻, with some words.\n"))
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
            .expect(IN_FIRST_WORD);
        for _ in 0..3 {
            frame(&context, &mut editor);
        }
        let iterations = scaled(if paragraphs == 100 { 1_000 } else { 100 });
        let start = Instant::now();
        for _ in 0..iterations {
            editor
                .insert_text(hint::black_box("x"))
                .expect(NO_COMPOSITION);
            frame(&context, &mut editor);
            editor.break_history_group();
            assert!(editor.undo(), "the insertion should be on the undo stack");
            frame(&context, &mut editor);
        }
        let edit = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64 / 2.0;
        assert_eq!(
            editor.document().plain_text(),
            source,
            "every insertion should have been undone"
        );
        println!("{paragraphs:>6} paragraphs: {edit:.2} µs / accessible local edit + frame");
    }
}

/// Returns an editor holding one paragraph of `count` "x" characters, each in
/// its own span, colored red and blue alternately.
fn formatted_editor(count: usize) -> Editor {
    use textloom::{Color, Document, InlineStyle, Paragraph, ParagraphKind, Span};

    let colors = [Color([255, 0, 0, 255]), Color([0, 0, 255, 255])];
    let paragraph = Paragraph::new(
        &"x".repeat(count),
        (0..count)
            .map(|index| Span {
                range: index..index + 1,
                style: InlineStyle {
                    foreground: Some(colors[index % 2]),
                    ..Default::default()
                },
                link: None,
            })
            .collect(),
        ParagraphKind::Body,
    )
    .expect("the spans cover the ASCII text one byte each, in order");
    Editor::new(Document::from_paragraphs([paragraph]))
}

fn formatted_idle_frames() {
    for count in [1_000, 16_000, 64_000] {
        let mut editor = formatted_editor(count);
        let context = egui::Context::default();
        for _ in 0..5 {
            frame(&context, &mut editor);
        }
        let iterations = scaled(1_000);
        let start = Instant::now();
        for _ in 0..iterations {
            frame(&context, &mut editor);
        }
        let elapsed = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64;
        assert_eq!(
            editor.document().plain_text(),
            "x".repeat(count),
            "rendering without input should not edit the document"
        );
        println!("{count:>6} styled spans: {elapsed:.2} µs / retained idle frame");
    }
}

fn formatted_accessibility_frames() {
    use textloom::{Color, StylePatch};

    for count in [4_000, 16_000] {
        let mut editor = formatted_editor(count);
        editor
            .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 1)))
            .expect("the paragraph starts with an ASCII character");
        let context = egui::Context::default();
        context.enable_accesskit();
        let recolor = |editor: &mut Editor, index: usize| {
            editor
                .apply_style(StylePatch {
                    foreground: Some(Some(if index.is_multiple_of(2) {
                        Color([0, 255, 0, 255])
                    } else {
                        Color([255, 0, 0, 255])
                    })),
                    ..Default::default()
                })
                .expect(NO_COMPOSITION);
        };
        for index in 0..4 {
            recolor(&mut editor, index);
            frame(&context, &mut editor);
        }
        let iterations = scaled(20);
        let start = Instant::now();
        for index in 0..iterations {
            recolor(&mut editor, index);
            frame(&context, &mut editor);
        }
        let elapsed = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64;
        assert_eq!(
            editor.document().plain_text(),
            "x".repeat(count),
            "recoloring should not change the text"
        );
        println!("{count:>6} styled spans: {elapsed:.2} µs / accessible formatting edit + frame");
    }
}

fn native_preedit_frames() {
    let text = "café 👩\u{200d}💻 ".repeat(10_000);
    let scalars = text.chars().count();
    for (label, scalar, byte) in [("start", 0, 0), ("end", scalars, text.len())] {
        let context = egui::Context::default();
        let mut editor = Editor::default();
        frame(&context, &mut editor);
        context.memory_mut(|memory| memory.request_focus(egui::Id::unique("bench")));
        let event = egui::Event::Ime(egui::ImeEvent::Preedit {
            text: text.clone(),
            active_range_chars: Some(scalar..scalar),
        });
        for _ in 0..3 {
            frame_with_events(&context, &mut editor, vec![event.clone()]);
        }
        assert_eq!(
            editor
                .composition()
                .expect("the focused widget should start a composition for the preedit")
                .selection,
            Some(byte..byte),
            "the preedit cursor at the {label} should convert from scalars to bytes"
        );
        let iterations = scaled(100);
        let start = Instant::now();
        for _ in 0..iterations {
            frame_with_events(&context, &mut editor, vec![hint::black_box(event.clone())]);
            assert_eq!(
                editor
                    .composition()
                    .expect("repeating the preedit should keep the composition")
                    .selection,
                Some(byte..byte),
                "the preedit cursor at the {label} should convert from scalars to bytes"
            );
        }
        let elapsed = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64;
        editor.cancel_composition();
        assert_eq!(
            editor.document().plain_text(),
            "",
            "preedit text should never enter the document"
        );
        println!("long Unicode preedit: {elapsed:.2} µs / native cursor at {label} + frame");
    }
}

fn long_preedit_frames() {
    let text = "café 👩\u{200d}💻 ".repeat(10_000);
    let changed_text = format!("{text}x");
    let context = egui::Context::default();
    let mut editor = Editor::default();
    frame(&context, &mut editor);
    context.memory_mut(|memory| memory.request_focus(egui::Id::unique("bench")));
    editor
        .update_composition(&text, Some(0..0))
        .expect("a caret at the start of the preedit is always valid");
    for _ in 0..3 {
        frame(&context, &mut editor);
    }
    for mode in ["idle", "identical update", "cursor update", "text update"] {
        let iterations = scaled(if mode == "text update" { 20 } else { 200 });
        let start = Instant::now();
        for index in 0..iterations {
            match mode {
                "idle" => {}
                "identical update" => editor
                    .update_composition(hint::black_box(&text), Some(0..0))
                    .expect("a caret at the start of the preedit is always valid"),
                "cursor update" => {
                    let end = if index % 2 == 0 { 0 } else { text.len() };
                    editor
                        .update_composition(hint::black_box(&text), Some(end..end))
                        .expect("a caret at either end of the preedit is always valid");
                }
                "text update" => {
                    let text = if index % 2 == 0 { &changed_text } else { &text };
                    editor
                        .update_composition(hint::black_box(text), Some(0..0))
                        .expect("a caret at the start of the preedit is always valid");
                }
                _ => unreachable!("the loop visits only the modes matched above"),
            }
            frame(&context, &mut editor);
            assert_eq!(
                editor
                    .composition()
                    .expect("rendering the focused widget should keep its composition")
                    .text
                    .len(),
                text.len() + usize::from(mode == "text update" && index % 2 == 0),
                "after a {mode}, the preedit should hold the latest text"
            );
        }
        let elapsed = start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64;
        assert_eq!(
            editor
                .composition()
                .expect("rendering the focused widget should keep its composition")
                .text,
            text,
            "an even number of {mode} iterations should end on the original preedit"
        );
        println!("long Unicode preedit: {elapsed:.2} µs / {mode} + frame");
    }
    editor.cancel_composition();
    assert_eq!(
        editor.document().plain_text(),
        "",
        "preedit text should never enter the document"
    );
}
