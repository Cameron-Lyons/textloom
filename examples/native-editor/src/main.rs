//! A complete native host for Textloom, with a separate dependency graph.

use eframe::egui;
use textloom::{Editor, ParagraphKind, Position, SearchOptions, Selection, StylePatch};

struct NativeEditor {
    editor: Editor,
    query: String,
    replacement: String,
    case_sensitive: bool,
    whole_word: bool,
    read_only: bool,
    snapshot: Option<Vec<u8>>,
    status: String,
    focus_editor: bool,
    smoke_frames: Option<usize>,
    smoke_error: bool,
}

impl NativeEditor {
    fn new(smoke_test: bool, read_only: bool) -> Self {
        let mut editor = Editor::from_text(
            "TextLoom\nSelect text to format it, or start writing.\nUnicode: café, 日本語, 👨‍👩‍👧‍👦.\nLists, undo, search, and rich snapshots.\nEnter continues a list; Enter on an empty item exits it.",
        );
        editor
            .set_paragraph_kind(ParagraphKind::Heading { level: 1 })
            .unwrap();
        editor
            .set_selection(Selection::new(Position::new(2, 0), Position::new(4, 0)))
            .unwrap();
        editor
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
            .unwrap();
        editor
            .set_selection(Selection::caret(Position::new(1, 0)))
            .unwrap();
        editor.clear_history();
        Self {
            editor,
            query: String::new(),
            replacement: String::new(),
            case_sensitive: false,
            whole_word: false,
            read_only,
            snapshot: None,
            status: "Ready".into(),
            focus_editor: true,
            smoke_frames: smoke_test.then_some(0),
            smoke_error: false,
        }
    }

    fn result(&mut self, result: Result<(), textloom::Error>) {
        if let Err(error) = result {
            self.status = error.to_string();
        }
        self.focus_editor = true;
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!self.read_only, |ui| {
                let style = self.editor.selection_style();
                for (label, active, patch) in [
                    (
                        "Bold",
                        style.bold,
                        StylePatch {
                            bold: Some(style.bold != Some(true)),
                            ..Default::default()
                        },
                    ),
                    (
                        "Italic",
                        style.italic,
                        StylePatch {
                            italic: Some(style.italic != Some(true)),
                            ..Default::default()
                        },
                    ),
                    (
                        "Underline",
                        style.underline,
                        StylePatch {
                            underline: Some(style.underline != Some(true)),
                            ..Default::default()
                        },
                    ),
                    (
                        "Strike",
                        style.strikethrough,
                        StylePatch {
                            strikethrough: Some(style.strikethrough != Some(true)),
                            ..Default::default()
                        },
                    ),
                    (
                        "Code",
                        style.code,
                        StylePatch {
                            code: Some(style.code != Some(true)),
                            ..Default::default()
                        },
                    ),
                ] {
                    let response = ui.selectable_label(active == Some(true), label);
                    let response = if active.is_none() {
                        response.on_hover_text(
                            "Mixed formatting; click to apply to the entire selection",
                        )
                    } else {
                        response
                    };
                    if response.clicked() {
                        let result = self.editor.apply_style(patch);
                        self.result(result);
                    }
                }
                if ui.button("Clear formatting").clicked() {
                    let result = self.editor.clear_formatting();
                    self.result(result);
                }
            });
            ui.separator();
            if ui
                .add_enabled(
                    !self.read_only && self.editor.can_undo(),
                    egui::Button::new("Undo"),
                )
                .clicked()
            {
                self.editor.undo();
                self.focus_editor = true;
            }
            if ui
                .add_enabled(
                    !self.read_only && self.editor.can_redo(),
                    egui::Button::new("Redo"),
                )
                .clicked()
            {
                self.editor.redo();
                self.focus_editor = true;
            }
            if ui.checkbox(&mut self.read_only, "Read only").changed() {
                self.focus_editor = true;
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!self.read_only, |ui| {
                let current = self
                    .editor
                    .document()
                    .paragraph(self.editor.selection().focus.paragraph)
                    .unwrap()
                    .kind();
                let mut kind = current;
                egui::ComboBox::from_id_salt("paragraph-kind")
                    .selected_text(kind_name(kind))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut kind, ParagraphKind::Body, "Body");
                        for level in 1..=6 {
                            ui.selectable_value(
                                &mut kind,
                                ParagraphKind::Heading { level },
                                format!("Heading {level}"),
                            );
                        }
                        ui.selectable_value(
                            &mut kind,
                            ParagraphKind::Bullet { indent: 0 },
                            "Bullets",
                        );
                        ui.selectable_value(
                            &mut kind,
                            ParagraphKind::Ordered {
                                indent: 0,
                                start: 1,
                            },
                            "Numbered list",
                        );
                    });
                if kind != current {
                    let result = self.editor.set_paragraph_kind(kind);
                    self.result(result);
                }
                if ui.button("Indent").clicked() {
                    let result = self.editor.indent_list();
                    self.result(result);
                }
                if ui.button("Outdent").clicked() {
                    let result = self.editor.outdent_list();
                    self.result(result);
                }
            });
            ui.separator();
            if ui.button("Capture snapshot").clicked() {
                let bytes = self.editor.document().to_bytes();
                self.status = format!("Captured {} bytes in memory", bytes.len());
                self.snapshot = Some(bytes);
            }
            if ui
                .add_enabled(
                    !self.read_only && self.snapshot.is_some(),
                    egui::Button::new("Restore snapshot"),
                )
                .clicked()
            {
                match textloom::Document::from_bytes(self.snapshot.as_ref().unwrap()) {
                    Ok(document) => {
                        self.editor = Editor::new(document);
                        self.status = "Snapshot restored; editing history cleared".into();
                        self.focus_editor = true;
                    }
                    Err(error) => self.status = error.to_string(),
                }
            }
        });
    }

    fn search(&mut self, ui: &mut egui::Ui) {
        let options = SearchOptions {
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
        };
        ui.horizontal_wrapped(|ui| {
            ui.label("Find");
            let query_response =
                ui.add(egui::TextEdit::singleline(&mut self.query).desired_width(180.0));
            let next = ui
                .add_enabled(!self.query.is_empty(), egui::Button::new("Next"))
                .clicked()
                || query_response.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let previous = ui
                .add_enabled(!self.query.is_empty(), egui::Button::new("Previous"))
                .clicked();
            if next || previous {
                let found = if previous {
                    self.editor.find_previous(&self.query, options, true)
                } else {
                    self.editor.find_next(&self.query, options, true)
                };
                self.status = match found {
                    Ok(true) => "Match selected".into(),
                    Ok(false) => "No matches".into(),
                    Err(error) => error.to_string(),
                };
                self.focus_editor = true;
            }
            ui.checkbox(&mut self.case_sensitive, "Match case");
            ui.checkbox(&mut self.whole_word, "Whole words");
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Replace");
            ui.add(egui::TextEdit::singleline(&mut self.replacement).desired_width(180.0));
            if ui
                .add_enabled(
                    !self.read_only && !self.query.is_empty(),
                    egui::Button::new("Replace all"),
                )
                .clicked()
            {
                self.status = match self
                    .editor
                    .replace_all(&self.query, &self.replacement, options)
                {
                    Ok(count) => format!("Replaced {count} matches"),
                    Err(error) => error.to_string(),
                };
                self.focus_editor = true;
            }
        });
    }
}

fn kind_name(kind: ParagraphKind) -> String {
    match kind {
        ParagraphKind::Body => "Body".into(),
        ParagraphKind::Heading { level } => format!("Heading {level}"),
        ParagraphKind::Bullet { .. } => "Bullets".into(),
        ParagraphKind::Ordered { .. } => "Numbered list".into(),
    }
}

impl eframe::App for NativeEditor {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("TextLoom native editor");
            self.toolbar(ui);
            ui.separator();
            self.search(ui);
            ui.separator();
            let focus = self.editor.selection().focus;
            ui.horizontal(|ui| {
                ui.label(&self.status);
                ui.separator();
                ui.label(format!(
                    "Paragraph {}, byte {}",
                    focus.paragraph + 1,
                    focus.byte
                ));
            });
            egui::ScrollArea::vertical().show(ui, |ui| {
                let output = textloom::adapter::egui::RichTextEditor::new(&mut self.editor)
                    .id_salt("native-document")
                    .min_rows(18)
                    .read_only(self.read_only)
                    .show(ui);
                if self.focus_editor {
                    output.response.request_focus();
                    self.focus_editor = false;
                }
                for error in output.errors {
                    self.status = error.to_string();
                    self.smoke_error = true;
                }
                for error in output.accessibility_errors {
                    self.status = error.to_string();
                    self.smoke_error = true;
                }
            });
        });
        if let Some(frames) = &mut self.smoke_frames {
            *frames += 1;
            if *frames == 20 {
                if self.smoke_error {
                    eprintln!("Native smoke test failed: {}", self.status);
                    std::process::exit(1);
                }
                println!(
                    "Rendered {frames} native frames at {} pixels per point",
                    ui.ctx().pixels_per_point()
                );
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else if *frames < 20 {
                ui.ctx().request_repaint();
            }
        }
    }
}

fn main() -> eframe::Result {
    let mut smoke_test = false;
    let mut read_only = false;
    let mut regular_font = None;
    let mut bold_font = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--smoke-test" => smoke_test = true,
            "--read-only" => read_only = true,
            "--font" | "--bold-font" => {
                let Some(path) = arguments.next() else {
                    eprintln!("{argument} requires a font file path");
                    std::process::exit(2);
                };
                let bytes = std::fs::read(&path)
                    .map_err(|error| eframe::Error::AppCreation(error.into()))?;
                if argument == "--font" {
                    regular_font = Some(bytes);
                } else {
                    bold_font = Some(bytes);
                }
            }
            "--help" | "-h" => {
                println!(
                    "TextLoom native example\n\nUsage: textloom-native-example [--smoke-test] [--read-only] [--font PATH] [--bold-font PATH]\n\n--smoke-test renders 20 native frames and exits.\n--read-only starts with editing disabled.\n--font adds a proportional font, such as a CJK fallback.\n--bold-font supplies the widget's Bold font family."
                );
                return Ok(());
            }
            _ => {
                eprintln!("Unknown argument: {argument}; use --help");
                std::process::exit(2);
            }
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TextLoom native example")
            .with_app_id("org.textloom.NativeExample")
            .with_inner_size([920.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "TextLoom native example",
        options,
        Box::new(move |context| {
            let mut fonts = egui::FontDefinitions::default();
            if let Some(bytes) = regular_font {
                fonts.font_data.insert(
                    "custom-regular".into(),
                    egui::FontData::from_owned(bytes).into(),
                );
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "custom-regular".into());
            }
            if let Some(bytes) = bold_font {
                fonts.font_data.insert(
                    "custom-bold".into(),
                    egui::FontData::from_owned(bytes).into(),
                );
                let mut fallback = fonts.families[&egui::FontFamily::Proportional].clone();
                fallback.insert(0, "custom-bold".into());
                fonts
                    .families
                    .insert(egui::FontFamily::Name("Bold".into()), fallback);
            }
            context.egui_ctx.set_fonts(fonts);
            Ok(Box::new(NativeEditor::new(smoke_test, read_only)))
        }),
    )
}
