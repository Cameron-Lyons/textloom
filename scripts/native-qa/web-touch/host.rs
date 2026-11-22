use eframe::egui::{self, Id, ScrollArea};
use textloom::{
    Editor, Link, Position, Selection,
    adapter::egui::{RichTextEditor, install_input_routing},
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    fn observe(
        focused: bool,
        find: bool,
        anchor_line: usize,
        anchor_byte: usize,
        focus_line: usize,
        focus_byte: usize,
        scroll: f32,
        held: bool,
        secondary: bool,
        contacts: usize,
    );
    #[wasm_bindgen(js_name = observeLink)]
    fn observe_link(url: &str);
}

struct Probe {
    editor: Editor,
    find: String,
    read_only: bool,
}

impl eframe::App for Probe {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            let find = ui.horizontal(|ui| {
                ui.label("Find");
                ui.text_edit_singleline(&mut self.find)
            });
            ui.separator();
            let area = ScrollArea::vertical().animated(false).show(ui, |ui| {
                RichTextEditor::new(&mut self.editor)
                    .read_only(self.read_only)
                    .id(Id::unique("editor"))
                    .show(ui)
            });
            if let Some(link) = area.inner.clicked_link {
                observe_link(link.as_str());
            }
            let selection = self.editor.selection();
            observe(
                area.inner.response.has_focus(),
                find.inner.has_focus(),
                selection.anchor.paragraph,
                selection.anchor.byte,
                selection.focus.paragraph,
                selection.focus.byte,
                area.state.clamped_offset().y,
                ui.input(|i| i.pointer.primary_down()),
                ui.input(|i| i.pointer.secondary_down()),
                ui.input(|i| {
                    i.multi_touch()
                        .map_or(usize::from(i.any_touches()), |t| t.num_touches)
                }),
            );
        });
    }
}

#[wasm_bindgen]
pub async fn start(canvas: web_sys::HtmlCanvasElement, read_only: bool) -> Result<(), JsValue> {
    eframe::WebRunner::new()
        .start(
            canvas,
            Default::default(),
            Box::new(move |cc| {
                if read_only {
                    cc.egui_ctx
                        .options_mut(|o| o.max_passes = 1.try_into().unwrap());
                }
                install_input_routing(&cc.egui_ctx);
                let text = (0..90)
                    .map(|i| format!("Line {i:02} select these characters with the pointer."))
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut editor = Editor::from_text(&text);
                editor
                    .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 20)))
                    .unwrap();
                editor
                    .set_link(Some(Link::try_new("https://example.invalid/qa").unwrap()))
                    .unwrap();
                editor
                    .set_selection(Selection::caret(Position::new(0, 0)))
                    .unwrap();
                Ok(Box::new(Probe {
                    editor,
                    find: String::new(),
                    read_only,
                }))
            }),
        )
        .await
}
