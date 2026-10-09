//! A complete native host for Textloom, with a separate dependency graph.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use eframe::egui;
use textloom::{Editor, ParagraphKind, Position, SearchOptions, Selection, StylePatch};

mod clipboard;
mod qa;

struct NativeEditor {
    editor: Editor,
    query: String,
    replacement: String,
    case_sensitive: bool,
    whole_word: bool,
    read_only: bool,
    enabled: bool,
    snapshot: Option<Vec<u8>>,
    status: String,
    focus_editor: bool,
    smoke_frames: Option<usize>,
    smoke_error: bool,
    last_frame: Option<u64>,
    qa: Option<qa::Session>,
    failed: Arc<AtomicBool>,
    clipboard: Option<clipboard::NativeClipboard>,
    clipboard_self_test: bool,
    document_id: Option<egui::Id>,
}

impl NativeEditor {
    fn new(
        options: &qa::Options,
        failed: Arc<AtomicBool>,
        clipboard: Option<clipboard::NativeClipboard>,
    ) -> Self {
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
        let qa = qa::Session::new(options, &editor);
        Self {
            editor,
            query: String::new(),
            replacement: String::new(),
            case_sensitive: false,
            whole_word: false,
            read_only: options.read_only,
            enabled: !options.disabled,
            snapshot: None,
            status: "Ready".into(),
            focus_editor: !options.disabled,
            smoke_frames: options.smoke_test.then_some(0),
            smoke_error: false,
            last_frame: None,
            qa,
            failed,
            clipboard,
            clipboard_self_test: options.clipboard_self_test,
            document_id: None,
        }
    }

    fn command_error(&mut self, error: impl std::fmt::Display) {
        self.status = error.to_string();
        if let Some(qa) = &mut self.qa {
            qa.host_command_error();
        }
    }

    fn result(&mut self, result: Result<(), textloom::Error>) {
        if let Err(error) = result {
            self.command_error(error);
        }
        self.focus_editor = self.enabled;
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(self.enabled && !self.read_only, |ui| {
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
                    self.enabled && !self.read_only && self.editor.can_undo(),
                    egui::Button::new("Undo"),
                )
                .clicked()
            {
                self.editor.undo();
                self.focus_editor = true;
            }
            if ui
                .add_enabled(
                    self.enabled && !self.read_only && self.editor.can_redo(),
                    egui::Button::new("Redo"),
                )
                .clicked()
            {
                self.editor.redo();
                self.focus_editor = true;
            }
            if ui.checkbox(&mut self.read_only, "Read only").changed() {
                self.focus_editor = self.enabled;
            }
            if ui.checkbox(&mut self.enabled, "Enabled").changed() {
                self.focus_editor = self.enabled;
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(self.enabled && !self.read_only, |ui| {
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
                    self.enabled && !self.read_only && self.snapshot.is_some(),
                    egui::Button::new("Restore snapshot"),
                )
                .clicked()
            {
                match textloom::Document::from_bytes(self.snapshot.as_ref().unwrap()) {
                    Ok(document) => {
                        self.editor = Editor::new(document);
                        if let Some(qa) = &mut self.qa {
                            qa.snapshot_restored();
                        }
                        self.status = "Snapshot restored; editing history cleared".into();
                        self.focus_editor = true;
                    }
                    Err(error) => self.command_error(error),
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
            let label = ui.label("Find");
            let query_response = ui
                .add(egui::TextEdit::singleline(&mut self.query).desired_width(180.0))
                .labelled_by(label.id);
            let next = ui
                .add_enabled(
                    self.enabled && !self.query.is_empty(),
                    egui::Button::new("Next"),
                )
                .clicked()
                || self.enabled
                    && query_response.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let previous = ui
                .add_enabled(
                    self.enabled && !self.query.is_empty(),
                    egui::Button::new("Previous"),
                )
                .clicked();
            if next || previous {
                let found = if previous {
                    self.editor.find_previous(&self.query, options, true)
                } else {
                    self.editor.find_next(&self.query, options, true)
                };
                match found {
                    Ok(true) => self.status = "Match selected".into(),
                    Ok(false) => self.status = "No matches".into(),
                    Err(error) => self.command_error(error),
                }
                self.focus_editor = true;
            }
            ui.checkbox(&mut self.case_sensitive, "Match case");
            ui.checkbox(&mut self.whole_word, "Whole words");
        });
        ui.horizontal_wrapped(|ui| {
            let label = ui.label("Replace");
            ui.add(egui::TextEdit::singleline(&mut self.replacement).desired_width(180.0))
                .labelled_by(label.id);
            if ui
                .add_enabled(
                    self.enabled && !self.read_only && !self.query.is_empty(),
                    egui::Button::new("Replace all"),
                )
                .clicked()
            {
                match self
                    .editor
                    .replace_all(&self.query, &self.replacement, options)
                {
                    Ok(count) => self.status = format!("Replaced {count} matches"),
                    Err(error) => self.command_error(error),
                }
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
    fn raw_input_hook(&mut self, context: &egui::Context, input: &mut egui::RawInput) {
        if let Some(clipboard) = &mut self.clipboard {
            clipboard.clear_staged_paste();
            if self.enabled
                && !self.read_only
                && input.focused
                && self
                    .document_id
                    .is_some_and(|id| context.memory(|memory| memory.has_focus(id)))
                && self
                    .editor
                    .composition()
                    .is_none_or(|composition| composition.text.is_empty())
            {
                clipboard.stage_paste(input);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let frame = ui.ctx().cumulative_frame_nr();
        let new_frame = self.last_frame != Some(frame);
        if new_frame {
            self.last_frame = Some(frame);
            if let Some(qa) = &mut self.qa {
                qa.begin_frame(ui.ctx());
            }
        }
        if self.clipboard_self_test {
            self.clipboard_self_test = false;
            let result = self
                .clipboard
                .as_mut()
                .ok_or_else(|| "Native rich clipboard was not enabled".to_owned())
                .and_then(clipboard::NativeClipboard::self_test);
            if let Some(qa) = &mut self.qa {
                qa.clipboard_self_test_result(result.is_ok());
            }
            match result {
                Ok(()) => println!("Native rich clipboard roundtrip and undo/redo passed"),
                Err(error) => {
                    self.status = error;
                    self.smoke_error = true;
                    self.failed.store(true, Ordering::Relaxed);
                }
            }
        }
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
                if self.editor.composition().is_some() {
                    ui.label("IME composing");
                }
            });
            let document_label = ui.label("Document");
            egui::ScrollArea::vertical().show(ui, |ui| {
                let output = ui
                    .add_enabled_ui(self.enabled, |ui| {
                        let mut widget =
                            textloom::adapter::egui::RichTextEditor::new(&mut self.editor)
                                .id_salt("native-document")
                                .min_rows(18)
                                .read_only(self.read_only);
                        if let Some(clipboard) = &mut self.clipboard {
                            widget = widget.rich_clipboard(clipboard);
                        }
                        widget.show(ui)
                    })
                    .inner;
                let response = output.response.labelled_by(document_label.id);
                self.document_id = Some(response.id);
                if self.focus_editor && self.enabled {
                    response.request_focus();
                    self.focus_editor = false;
                }
                if let Some(qa) = &mut self.qa {
                    qa.widget_errors(output.errors.len(), output.accessibility_errors.len());
                    qa.observe_editor(
                        &self.editor,
                        qa::FocusState {
                            window_focused: ui.input(|input| input.focused),
                            widget_focus_retained: ui
                                .memory(|memory| memory.has_focus(response.id)),
                        },
                        self.enabled,
                        self.read_only,
                    );
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
        if let Some(clipboard) = &mut self.clipboard {
            clipboard.clear_staged_paste();
            if let Some(error) = clipboard.take_error() {
                self.status = format!("{error}; copied plain text instead");
            }
        }
        if self.smoke_frames.is_some() && self.smoke_error {
            self.failed.store(true, Ordering::Relaxed);
        }
        if new_frame && let Some(frames) = &mut self.smoke_frames {
            *frames += 1;
            if *frames == 20 {
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

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.smoke_frames.is_some() && self.smoke_error {
            eprintln!("Native smoke test failed: {}", self.status);
        }
        if let Some(qa) = &self.qa {
            match qa.write_report(&self.editor) {
                Ok(path) => println!("Native QA observations written to {}", path.display()),
                Err(error) => {
                    eprintln!("Could not write native QA report: {error}");
                    self.failed.store(true, Ordering::Relaxed);
                }
            }
        }
    }
}

fn main() -> eframe::Result {
    let raw_arguments: Vec<_> = qa::arguments().collect();
    if let Some(result) = clipboard::helper(&raw_arguments) {
        if let Err(error) = result {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let arguments = match qa::Options::parse(raw_arguments) {
        Ok(arguments) => arguments,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    if arguments.help {
        println!("{}", qa::HELP);
        return Ok(());
    }
    let regular_font = qa::read_font(arguments.regular_font.as_ref())?;
    let bold_font = qa::read_font(arguments.bold_font.as_ref())?;
    let failed = Arc::new(AtomicBool::new(false));
    let app_failed = Arc::clone(&failed);
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
            let clipboard = if arguments.rich_clipboard {
                Some(clipboard::NativeClipboard::new(context).map_err(std::io::Error::other)?)
            } else {
                None
            };
            Ok(Box::new(NativeEditor::new(
                &arguments, app_failed, clipboard,
            )))
        }),
    )?;
    if failed.load(Ordering::Relaxed) {
        return Err(eframe::Error::AppCreation(
            std::io::Error::other("Native smoke test or QA report failed; see stderr").into(),
        ));
    }
    Ok(())
}
