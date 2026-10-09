//! Render a headless egui frame without a window or another dependency.
//! Run: `cargo run --example egui_editor --features egui`
//! Copy `editor_ui` into an eframe `App::update` or your native egui host.

use textloom::{Editor, ParagraphKind, StylePatch, adapter::egui::RichTextEditor};

fn editor_ui(ui: &mut egui::Ui, editor: &mut Editor) {
    let mut errors = Vec::new();
    ui.horizontal(|ui| {
        let style = editor.selection_style();
        if ui.button("Bold").clicked()
            && let Err(error) = editor.apply_style(StylePatch {
                bold: Some(style.bold != Some(true)),
                ..Default::default()
            })
        {
            errors.push(error);
        }
        if ui.button("Italic").clicked()
            && let Err(error) = editor.apply_style(StylePatch {
                italic: Some(style.italic != Some(true)),
                ..Default::default()
            })
        {
            errors.push(error);
        }
        if ui.button("Clear formatting").clicked()
            && let Err(error) = editor.clear_formatting()
        {
            errors.push(error);
        }
        if ui.button("Bullets").clicked()
            && let Err(error) = editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
        {
            errors.push(error);
        }
        if ui
            .add_enabled(editor.can_undo(), egui::Button::new("Undo"))
            .clicked()
        {
            editor.undo();
        }
        if ui
            .add_enabled(editor.can_redo(), egui::Button::new("Redo"))
            .clicked()
        {
            editor.redo();
        }
    });
    for error in errors {
        ui.colored_label(egui::Color32::RED, error.to_string());
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        let output = RichTextEditor::new(editor)
            .id_salt("document")
            .min_rows(10)
            .hint_text("Write something…")
            .show(ui);
        for error in output.errors {
            ui.colored_label(egui::Color32::RED, error.to_string());
        }
        for error in output.accessibility_errors {
            ui.colored_label(egui::Color32::RED, error.to_string());
        }
    });
}

fn main() {
    let mut editor = Editor::from_text(
        "Textloom\nSelect text to format it. Native input stays in your GUI host.\nUnicode: 日本語, café, 👨‍👩‍👧‍👦",
    );
    let context = egui::Context::default();
    let output = context.run_ui(egui::RawInput::default(), |ui| {
        egui::CentralPanel::default().show(ui, |ui| editor_ui(ui, &mut editor));
    });
    println!(
        "Rendered {} egui shapes.\n{}",
        output.shapes.len(),
        editor.document().plain_text()
    );
    output.drop_without_applying_deltas();
}
