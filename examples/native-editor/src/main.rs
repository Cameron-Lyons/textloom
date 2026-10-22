use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use textloom::{
    Color, Document, Editor, Link, ParagraphKind, Position, SearchOptions, Selection, StylePatch,
};

mod clipboard;
mod ime_handoff;
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
    fill_color: Color,
    font_color: Color,
    link_url: Option<String>,
    focus_link_field: bool,
    commands: Vec<EditorCommand>,
    text_field_ids: [Option<egui::Id>; 3],
    cleared_document_preedit: Option<String>,
    ime_handoff: Option<ime_handoff::Guard>,
}

enum EditorCommand {
    Style(StylePatch),
    ClearFormatting,
    RemoveLink,
    Undo,
    Redo,
    ParagraphKind(ParagraphKind),
    Indent,
    Outdent,
    ItemStart(Option<u32>),
    CaptureSnapshot,
    RestoreSnapshot,
    ApplyLink(String),
    Find {
        previous: bool,
        options: SearchOptions,
    },
    ReplaceAll(SearchOptions),
}

impl NativeEditor {
    fn new(
        options: &qa::Options,
        report: Option<qa::ReportFile>,
        failed: Arc<AtomicBool>,
        clipboard: Option<clipboard::NativeClipboard>,
    ) -> Self {
        let mut editor = Editor::from_text(
            "TextLoom\nSelect text to format it, or start writing.\nUnicode: café, 日本語, 👨\u{200d}👩\u{200d}👧\u{200d}👦.\nLists, undo, search, and rich snapshots.\nEnter continues a list; Enter on an empty item exits it.",
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
        let qa = report.map(|report| qa::Session::new(options, report, &editor));
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
            fill_color: Color::YELLOW,
            font_color: Color::RED,
            link_url: None,
            focus_link_field: false,
            commands: Vec::new(),
            text_field_ids: [None; 3],
            cleared_document_preedit: None,
            ime_handoff: None,
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

    fn finish_composition(&mut self, ui: &egui::Ui) {
        let Some(mut text) = self
            .editor
            .composition()
            .map(|composition| composition.text.clone())
        else {
            return;
        };
        text.retain(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'));
        if text.is_empty() || self.read_only || !self.enabled {
            self.editor.cancel_composition();
        } else if let Err(error) = self.editor.commit_composition(&text) {
            self.command_error(error);
        }
        ui.memory_mut(|memory| memory.interrupt_ime());
    }

    fn apply_commands(&mut self, ui: &egui::Ui) {
        for command in std::mem::take(&mut self.commands) {
            self.apply_command(ui, command);
        }
    }

    fn apply_command(&mut self, ui: &egui::Ui, command: EditorCommand) {
        if (!self.enabled && !matches!(command, EditorCommand::CaptureSnapshot))
            || (self.read_only
                && !matches!(
                    command,
                    EditorCommand::CaptureSnapshot | EditorCommand::Find { .. }
                ))
        {
            return;
        }
        self.finish_composition(ui);
        match command {
            EditorCommand::Style(patch) => {
                let result = self.editor.apply_style(patch);
                self.result(result);
            }
            EditorCommand::ClearFormatting => {
                let result = self.editor.clear_formatting();
                self.result(result);
            }
            EditorCommand::RemoveLink => {
                self.link_url = None;
                let result = link_selection(&mut self.editor, None);
                self.result(result);
            }
            EditorCommand::Undo => {
                self.editor.undo();
                self.focus_editor = true;
            }
            EditorCommand::Redo => {
                self.editor.redo();
                self.focus_editor = true;
            }
            EditorCommand::ParagraphKind(kind) => {
                if let ParagraphKind::Ordered { indent, .. } = kind {
                    let result = self.editor.apply_numbered_list(indent);
                    self.result(result);
                } else {
                    let selected = self.editor.selected_paragraphs();
                    if let Some(kind) = chosen_kind(self.editor.document(), selected, kind) {
                        let result = self.editor.set_paragraph_kind(kind);
                        self.result(result);
                    }
                }
            }
            EditorCommand::Indent => {
                let result = self.editor.indent_list();
                self.result(result);
            }
            EditorCommand::Outdent => {
                let result = self.editor.outdent_list();
                self.result(result);
            }
            EditorCommand::ItemStart(start) => {
                let index = kind_paragraph(self.editor.selection());
                let result = set_item_start(&mut self.editor, index, start);
                self.result(result);
            }
            EditorCommand::CaptureSnapshot => {
                let bytes = self.editor.document().to_bytes();
                self.status = format!("Captured {} bytes in memory", bytes.len());
                self.snapshot = Some(bytes);
            }
            EditorCommand::RestoreSnapshot => {
                let Some(bytes) = &self.snapshot else {
                    return;
                };
                match Document::from_bytes_with_limits(bytes, textloom::DecodeLimits::UNLIMITED) {
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
            EditorCommand::ApplyLink(url) => match apply_link(&mut self.editor, &url) {
                Ok(()) => {
                    self.link_url = None;
                    self.focus_editor = true;
                }
                Err(error) => self.command_error(error),
            },
            EditorCommand::Find { previous, options } => {
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
            EditorCommand::ReplaceAll(options) => {
                match self
                    .editor
                    .replace_all(&self.query, &self.replacement, options)
                {
                    Ok(count) => self.status = format!("Replaced {count} matches"),
                    Err(error) => self.command_error(error),
                }
                self.focus_editor = true;
            }
        }
        ui.ctx().request_repaint();
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
                        self.commands.push(EditorCommand::Style(patch));
                    }
                }
                for target in [ColorTarget::Fill, ColorTarget::Font] {
                    let last = match target {
                        ColorTarget::Fill => &mut self.fill_color,
                        ColorTarget::Font => &mut self.font_color,
                    };
                    if let Some(color) = color_control(ui, target, last) {
                        self.commands
                            .push(EditorCommand::Style(target.patch(color)));
                    }
                }
                if ui.button("Clear formatting").clicked() {
                    self.commands.push(EditorCommand::ClearFormatting);
                }
                let link = self.editor.selected_link();
                let hint = link.as_ref().map_or_else(
                    || "Insert a link (Ctrl+K)".to_owned(),
                    |link| format!("Edit link to {link} (Ctrl+K)"),
                );
                if ui
                    .selectable_label(link.is_some(), "Link")
                    .on_hover_text(hint)
                    .clicked()
                {
                    self.open_link_field();
                }
                if ui
                    .add_enabled(link.is_some(), egui::Button::new("Remove link"))
                    .clicked()
                {
                    self.commands.push(EditorCommand::RemoveLink);
                }
            });
            ui.separator();
            if ui
                .add_enabled(
                    self.enabled
                        && !self.read_only
                        && (self.editor.can_undo()
                            || self
                                .editor
                                .composition()
                                .is_some_and(|composition| !composition.text.is_empty())),
                    egui::Button::new("Undo"),
                )
                .clicked()
            {
                self.commands.push(EditorCommand::Undo);
            }
            if ui
                .add_enabled(
                    self.enabled && !self.read_only && self.editor.can_redo(),
                    egui::Button::new("Redo"),
                )
                .clicked()
            {
                self.commands.push(EditorCommand::Redo);
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
                let focused = kind_paragraph(self.editor.selection());
                let current = self.editor.document().paragraph(focused).unwrap().kind();
                let indent = match current {
                    ParagraphKind::Bullet { indent } | ParagraphKind::Ordered { indent, .. } => {
                        indent
                    }
                    _ => 0,
                };
                let mut choices = vec![ParagraphKind::Body];
                choices.extend((1..=6).map(|level| ParagraphKind::Heading { level }));
                choices.push(ParagraphKind::Quote);
                choices.push(ParagraphKind::Bullet { indent });
                choices.push(ParagraphKind::Ordered {
                    indent,
                    start: None,
                });
                let mut chosen = None;
                egui::ComboBox::from_id_salt("paragraph-kind")
                    .selected_text(kind_name(current))
                    .height(f32::INFINITY)
                    .show_ui(ui, |ui| {
                        for kind in choices {
                            let label = kind_name(kind);
                            if ui
                                .selectable_label(label == kind_name(current), label)
                                .clicked()
                            {
                                chosen = Some(kind);
                            }
                        }
                    });
                if let Some(kind) = chosen {
                    self.commands.push(EditorCommand::ParagraphKind(kind));
                }
                if ui.button("Indent").clicked() {
                    self.commands.push(EditorCommand::Indent);
                }
                if ui.button("Outdent").clicked() {
                    self.commands.push(EditorCommand::Outdent);
                }
                let focused = kind_paragraph(self.editor.selection());
                let document = self.editor.document();
                if let ParagraphKind::Ordered { start, .. } =
                    document.paragraph(focused).unwrap().kind()
                {
                    let number = document.list_number(focused).unwrap();
                    ui.label(format!("Item {number}."));
                    let mut restart = None;
                    if ui
                        .add_enabled(start != Some(1), egui::Button::new("Restart at 1"))
                        .clicked()
                    {
                        restart = Some(Some(1));
                    }
                    if ui
                        .add_enabled(start.is_some(), egui::Button::new("Continue numbering"))
                        .clicked()
                    {
                        restart = Some(None);
                    }
                    if let Some(start) = restart {
                        self.commands.push(EditorCommand::ItemStart(start));
                    }
                }
            });
            ui.separator();
            if ui.button("Capture snapshot").clicked() {
                self.commands.push(EditorCommand::CaptureSnapshot);
            }
            if ui
                .add_enabled(
                    self.enabled && !self.read_only && self.snapshot.is_some(),
                    egui::Button::new("Restore snapshot"),
                )
                .clicked()
            {
                self.commands.push(EditorCommand::RestoreSnapshot);
            }
        });
    }

    fn open_link_field(&mut self) {
        self.link_url = Some(
            self.editor
                .selected_link()
                .map(|link| link.to_string())
                .unwrap_or_default(),
        );
        self.focus_link_field = true;
    }

    fn link_field(&mut self, ui: &mut egui::Ui) {
        self.text_field_ids[0] = None;
        let Some(mut url) = self.link_url.take() else {
            return;
        };
        let (mut apply, mut cancel) = (false, false);
        ui.horizontal_wrapped(|ui| {
            let label = ui.label("Link URL");
            let field_focus = ui.memory(|memory| memory.focused());
            let response = ui
                .add(
                    egui::TextEdit::singleline(&mut url)
                        .hint_text("https://")
                        .desired_width(320.0),
                )
                .labelled_by(label.id);
            self.text_field_ids[0] = Some(response.id);
            if std::mem::take(&mut self.focus_link_field) {
                response.request_focus();
            }
            apply |= field_finished_with_key(ui, &response, field_focus, egui::Key::Enter);
            cancel |= field_finished_with_key(ui, &response, field_focus, egui::Key::Escape);
            apply |= ui.button("Apply link").clicked();
            cancel |= ui.button("Cancel").clicked();
        });
        if cancel || !self.enabled || self.read_only {
            self.focus_editor = self.enabled;
            return;
        }
        if apply {
            self.commands.push(EditorCommand::ApplyLink(url.clone()));
        }
        self.link_url = Some(url);
    }

    fn follow_link(&mut self, context: &egui::Context, link: &Link) {
        if let Some(target) = followable(link) {
            context.open_url(egui::OpenUrl::new_tab(target));
            self.status = format!("Opened {target}");
        } else {
            self.status = format!("Not opening {link}: only web and mail links are followed");
        }
    }

    fn search(&mut self, ui: &mut egui::Ui) {
        let (next, previous) = ui
            .horizontal_wrapped(|ui| {
                let label = ui.label("Find");
                let field_focus = ui.memory(|memory| memory.focused());
                let query_response = ui
                    .add(egui::TextEdit::singleline(&mut self.query).desired_width(180.0))
                    .labelled_by(label.id);
                self.text_field_ids[1] = Some(query_response.id);
                let next = ui
                    .add_enabled(
                        self.enabled && !self.query.is_empty(),
                        egui::Button::new("Next"),
                    )
                    .clicked()
                    || self.enabled
                        && field_finished_with_key(
                            ui,
                            &query_response,
                            field_focus,
                            egui::Key::Enter,
                        );
                let previous = ui
                    .add_enabled(
                        self.enabled && !self.query.is_empty(),
                        egui::Button::new("Previous"),
                    )
                    .clicked();
                ui.checkbox(&mut self.case_sensitive, "Match case");
                ui.checkbox(&mut self.whole_word, "Whole words");
                (next, previous)
            })
            .inner;
        let options = SearchOptions {
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
        };
        if next || previous {
            self.commands
                .push(EditorCommand::Find { previous, options });
        }
        ui.horizontal_wrapped(|ui| {
            let label = ui.label("Replace");
            self.text_field_ids[2] = Some(
                ui.add(egui::TextEdit::singleline(&mut self.replacement).desired_width(180.0))
                    .labelled_by(label.id)
                    .id,
            );
            if ui
                .add_enabled(
                    self.enabled && !self.read_only && !self.query.is_empty(),
                    egui::Button::new("Replace all"),
                )
                .clicked()
            {
                self.commands.push(EditorCommand::ReplaceAll(options));
            }
        });
    }
}

fn field_finished_with_key(
    ui: &egui::Ui,
    response: &egui::Response,
    initial_focus: Option<egui::Id>,
    requested: egui::Key,
) -> bool {
    // Escape can clear focus before the UI runs. Enter is handled by the
    // field itself; a stale lost_focus response cannot establish its owner.
    let mut owned = initial_focus == Some(response.id)
        || (requested == egui::Key::Escape
            && initial_focus.is_none()
            && ui.memory(|memory| memory.had_focus_last_frame(response.id)));
    ui.input(|input| {
        for event in &input.events {
            match event {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    ..
                } => owned = response.rect.contains(*pos),
                egui::Event::AccessKitActionRequest(request)
                    if request.action == egui::accesskit::Action::Focus
                        && request.target_tree == egui::accesskit::TreeId::ROOT
                        && request.data.is_none() =>
                {
                    owned = request.target_node == response.id.accesskit_id();
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if (*key == egui::Key::Enter
                        && modifiers.matches_logically(egui::Modifiers::NONE))
                        || (*key == egui::Key::Escape && !modifiers.any())
                    {
                        if owned {
                            return *key == requested;
                        }
                    } else if *key == egui::Key::Tab && (!modifiers.any() || modifiers.shift_only())
                    {
                        owned = false;
                    }
                }
                _ => {}
            }
        }
        false
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ColorTarget {
    Fill,
    Font,
}

impl ColorTarget {
    fn label(self) -> &'static str {
        match self {
            Self::Fill => "Fill Color",
            Self::Font => "Font Color",
        }
    }

    fn menu_label(self) -> &'static str {
        match self {
            Self::Fill => "Fill Color menu",
            Self::Font => "Font Color menu",
        }
    }

    fn none_label(self) -> &'static str {
        match self {
            Self::Fill => "No Fill",
            Self::Font => "Automatic",
        }
    }

    fn patch(self, color: Option<Color>) -> StylePatch {
        match self {
            Self::Fill => StylePatch {
                background: Some(color),
                ..Default::default()
            },
            Self::Font => StylePatch {
                foreground: Some(color),
                ..Default::default()
            },
        }
    }
}

fn color_control(
    ui: &mut egui::Ui,
    target: ColorTarget,
    last: &mut Color,
) -> Option<Option<Color>> {
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let name = Color::STANDARD
            .iter()
            .find_map(|(name, color)| (color == last).then_some(*name))
            .unwrap_or("Custom");
        if swatch_button(ui, Some(*last), target.label(), false)
            .on_hover_text(format!("{}: {name}", target.label()))
            .clicked()
        {
            chosen = Some(Some(*last));
        }
        let response = menu_arrow(ui, target.menu_label());
        egui::Popup::menu(&response).show(|ui| {
            if swatch_button(ui, None, target.none_label(), false).clicked() {
                chosen = Some(None);
            }
            ui.label("Standard Colors");
            for (name, color) in Color::STANDARD {
                if swatch_button(ui, Some(color), name, color == *last).clicked() {
                    *last = color;
                    chosen = Some(Some(color));
                }
            }
        });
    });
    chosen
}

fn swatch_button(
    ui: &mut egui::Ui,
    color: Option<Color>,
    text: &str,
    selected: bool,
) -> egui::Response {
    let swatch = egui::IdSalt::new("swatch");
    let size = egui::Vec2::splat(ui.text_style_height(&egui::TextStyle::Button));
    let response = egui::Button::new((egui::Atom::custom(swatch, size), text))
        .selected(selected)
        .atom_ui(ui);
    if let Some(rect) = response.rect(swatch) {
        let stroke = ui.visuals().widgets.noninteractive.fg_stroke;
        let fill = color.map_or(egui::Color32::TRANSPARENT, |Color([r, g, b, a])| {
            egui::Color32::from_rgba_unmultiplied(r, g, b, a)
        });
        ui.painter().rect(
            rect.shrink(1.0),
            1.0,
            fill,
            stroke,
            egui::StrokeKind::Inside,
        );
    }
    response.response
}

fn menu_arrow(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let height = ui.spacing().interact_size.y;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(height * 0.75, height), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label));
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let painter = ui.painter();
        painter.rect(
            rect.expand(visuals.expansion),
            visuals.corner_radius,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let center = rect.center();
        let half = rect.width() / 5.0;
        painter.add(egui::Shape::convex_polygon(
            vec![
                center + egui::vec2(-half, -half / 2.0),
                center + egui::vec2(half, -half / 2.0),
                center + egui::vec2(0.0, half / 2.0),
            ],
            visuals.fg_stroke.color,
            egui::Stroke::NONE,
        ));
    }
    response
}

fn apply_link(editor: &mut Editor, url: &str) -> Result<(), textloom::Error> {
    let url = url.trim();
    if url.is_empty() {
        return link_selection(editor, None);
    }
    let link = Link::new(url)?;
    if editor.selection().is_caret() && editor.selected_link().is_none() {
        editor.insert_link("", link)
    } else {
        link_selection(editor, Some(link))
    }
}

fn link_selection(editor: &mut Editor, link: Option<Link>) -> Result<(), textloom::Error> {
    let selection = editor.selection();
    let range = selection.range();
    let whole = editor
        .document()
        .link_range_at(range.start)
        .filter(|whole| !selection.is_caret() && range.end <= whole.end && *whole != range);
    let Some(whole) = whole else {
        return editor.set_link(link);
    };
    editor.transaction(|editor| {
        editor.set_selection(Selection::new(whole.start, whole.end))?;
        editor.set_link(link)?;
        editor.set_selection(selection)
    })
}

fn followable(link: &Link) -> Option<&str> {
    let target = link.as_str().trim();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| {
            target
                .get(..scheme.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
        })
        .then_some(target)
}

fn kind_paragraph(selection: Selection) -> usize {
    let focus = selection.focus;
    if focus.byte == 0 && focus.paragraph > selection.anchor.paragraph {
        focus.paragraph - 1
    } else {
        focus.paragraph
    }
}

fn set_item_start(
    editor: &mut Editor,
    index: usize,
    start: Option<u32>,
) -> Result<(), textloom::Error> {
    let Some(ParagraphKind::Ordered { indent, .. }) = editor
        .document()
        .paragraph(index)
        .map(|paragraph| paragraph.kind())
    else {
        return Ok(());
    };
    let kind = ParagraphKind::Ordered { indent, start };
    let selection = editor.selection();
    editor.transaction(|editor| {
        editor.set_selection(Selection::caret(Position::new(index, 0)))?;
        editor.set_paragraph_kind(kind)?;
        editor.set_selection(selection)
    })
}

fn chosen_kind(
    document: &Document,
    selected: Range<usize>,
    chosen: ParagraphKind,
) -> Option<ParagraphKind> {
    let paragraphs = &document.paragraphs()[selected];
    (!paragraphs
        .iter()
        .all(|paragraph| paragraph.kind() == chosen))
    .then_some(chosen)
}

fn kind_name(kind: ParagraphKind) -> String {
    match kind {
        ParagraphKind::Body => "Body".into(),
        ParagraphKind::Heading { level } => format!("Heading {level}"),
        ParagraphKind::Quote => "Quote".into(),
        ParagraphKind::Bullet { .. } => "Bullets".into(),
        ParagraphKind::Ordered { .. } => "Numbered list".into(),
        _ => "Other".into(),
    }
}

/// Prepares an egui context for the host. egui's default quit shortcut matches
/// Ctrl/Command+Q while ignoring Alt, so it would also close the window for
/// Ctrl+Alt+Q, which Windows treats as AltGr+Q (`@` on a German layout); the
/// host handles Q itself instead.
fn configure_context(context: &egui::Context) {
    textloom::adapter::egui::install_input_routing(context);
    context.options_mut(|options| options.quit_shortcuts.clear());
}

/// Removes presses of `key` with Ctrl or Command and reports whether there were
/// any. Chords with Alt are left alone: they type AltGr or Option characters.
fn take_command_shortcut(input: &mut egui::InputState, key: egui::Key) -> bool {
    let mut requested = false;
    input.events.retain(|event| {
        let matches = matches!(
            event,
            egui::Event::Key {
                key: pressed_key,
                pressed: true,
                modifiers,
                ..
            } if *pressed_key == key
                && modifiers.command
                && !modifiers.alt
                && !(modifiers.mac_cmd && modifiers.ctrl)
        );
        requested |= matches;
        !matches
    });
    requested
}

impl eframe::App for NativeEditor {
    fn raw_input_hook(&mut self, _context: &egui::Context, input: &mut egui::RawInput) {
        if let Some(clipboard) = &mut self.clipboard {
            // An earlier queued Focus can change the eventual owner. Snapshot
            // eligible pastes now, before the clipboard can change again.
            let capture_native = self.enabled && !self.read_only && input.focused;
            clipboard.capture_pastes(input, capture_native);
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
        if let Some(qa) = &mut self.qa {
            qa.observe_input_pass(ui.ctx());
        }
        let focused = ui.memory(|memory| memory.focused());
        let document_was_focused = focused.is_some() && focused == self.document_id;
        // Backward keyboard focus traversal takes effect at the start of a frame.
        // The destination then owns fresh IME input before any widgets are rendered.
        let field_owned_ime = focused.is_some_and(|id| self.text_field_ids.contains(&Some(id)))
            && self
                .document_id
                .is_some_and(|id| ui.memory(|memory| memory.had_focus_last_frame(id)));
        if field_owned_ime
            && let Some(candidate) = ime_handoff::candidate(
                self.editor.composition(),
                self.cleared_document_preedit.as_deref(),
                &[],
                false,
            )
        {
            self.ime_handoff = Some(ime_handoff::Guard::new(
                focused.unwrap(),
                candidate,
                ui.input(|input| input.time),
            ));
        }
        if let Some(guard) = &self.ime_handoff {
            let selected = focused
                .and_then(|id| egui::text_edit::TextEditState::load(ui.ctx(), id))
                .is_some_and(|state| !state.cursor.is_empty());
            if !ui.input_mut(|input| guard.filter(input, focused, selected)) {
                self.ime_handoff = None;
            }
        }
        if let Some(clipboard) = &mut self.clipboard {
            let document_owns_input = self.enabled
                && !self.read_only
                && ui.input(|input| input.focused)
                && document_was_focused
                && self
                    .editor
                    .composition()
                    .is_none_or(|composition| composition.text.is_empty());
            // Snapshot capture happens before routing. Advance every delivered
            // Paste slot so native-field input cannot shift the document's queue.
            ui.input(|input| clipboard.stage_paste(&input.events, document_owns_input));
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
        if self.enabled
            && !self.read_only
            && ui.input_mut(|input| take_command_shortcut(input, egui::Key::K))
        {
            self.open_link_field();
        }
        if ui.input_mut(|input| take_command_shortcut(input, egui::Key::Q)) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("TextLoom native editor");
            self.toolbar(ui);
            self.link_field(ui);
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
            if field_owned_ime {
                // Text fields have already consumed the events they own. Leave the
                // document's blur path to finish only its previous composition.
                ui.input_mut(|input| {
                    input
                        .events
                        .retain(|event| !matches!(event, egui::Event::Ime(_)));
                });
            }
            if !self.commands.is_empty()
                && self.editor.composition().is_some()
                && let Some(id) = self.document_id
                && ui.memory(|memory| memory.has_focus(id))
            {
                // Accessibility activation can leave the document focused. Use the
                // widget's blur path to finish IME input and remember its commit echo.
                ui.memory_mut(|memory| memory.surrender_focus(id));
                self.focus_editor = self.enabled;
            }
            let target = ui
                .memory(|memory| memory.focused())
                .filter(|id| document_was_focused && self.text_field_ids.contains(&Some(*id)));
            let capture_candidate = target.is_some()
                || (document_was_focused
                    && ui.input(|input| ime_handoff::has_empty_preedit(&input.events)));
            let candidate = capture_candidate
                .then(|| {
                    ui.input(|input| {
                        ime_handoff::candidate(
                            self.editor.composition(),
                            self.cleared_document_preedit.as_deref(),
                            &input.events,
                            target.is_some(),
                        )
                    })
                })
                .flatten();
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
                if let Some(qa) = &mut self.qa {
                    qa.widget_errors(output.errors.len(), output.accessibility_errors.len());
                }
                for error in output.errors {
                    self.status = error.to_string();
                    self.smoke_error = true;
                }
                for error in output.accessibility_errors {
                    self.status = error.to_string();
                    self.smoke_error = true;
                }
                if let Some(link) = output.clicked_link {
                    self.follow_link(ui.ctx(), &link);
                }
            });
            if self
                .editor
                .composition()
                .is_some_and(|composition| composition.text.is_empty())
            {
                if capture_candidate {
                    self.cleared_document_preedit = candidate;
                }
            } else {
                self.cleared_document_preedit = None;
                if self.editor.composition().is_none()
                    && let (Some(target), Some(candidate)) = (target, candidate)
                {
                    self.ime_handoff = Some(ime_handoff::Guard::new(
                        target,
                        candidate,
                        ui.input(|input| input.time),
                    ));
                }
            }
        });
        // Let the document consume queued IME input before toolbar commands commit
        // its remaining preedit or change its selection and history.
        self.apply_commands(ui);
        if self.focus_editor
            && self.enabled
            && let Some(id) = self.document_id
        {
            ui.memory_mut(|memory| memory.request_focus(id));
            self.focus_editor = false;
        }
        if let Some(qa) = &mut self.qa {
            qa.observe_editor(
                &self.editor,
                qa::FocusState {
                    window_focused: ui.input(|input| input.focused),
                    widget_focus_retained: self
                        .document_id
                        .is_some_and(|id| ui.memory(|memory| memory.has_focus(id))),
                },
                self.enabled,
                self.read_only,
            );
        }
        ui.input_mut(|input| {
            input
                .events
                .retain(|event| !matches!(event, egui::Event::Ime(_)));
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
        if let Some(qa) = &mut self.qa {
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
    let regular_font = qa::read_font(arguments.regular_font.as_deref())?;
    let bold_font = qa::read_font(arguments.bold_font.as_deref())?;
    let report = match &arguments.qa_report {
        Some(path) => match qa::ReportFile::create(path) {
            Ok(report) => Some(report),
            Err(error) => {
                eprintln!("Cannot create QA report {}: {error}", path.display());
                std::process::exit(2);
            }
        },
        None => None,
    };
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
            configure_context(&context.egui_ctx);
            if let Some(theme) = arguments.theme {
                context.egui_ctx.set_theme(theme);
            }
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
                &arguments, report, app_failed, clipboard,
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use eframe::App as _;
    use egui::accesskit::{Node, NodeId, Role};

    use super::*;

    struct Harness {
        app: NativeEditor,
        context: egui::Context,
        frame: eframe::Frame,
        time: f64,
        nodes: HashMap<NodeId, Node>,
        commands: Vec<egui::OutputCommand>,
        viewport_commands: Vec<egui::ViewportCommand>,
        report: PathBuf,
    }

    impl Harness {
        fn new(name: &str) -> Self {
            let report = std::env::temp_dir().join(format!(
                "textloom-host-{name}-{}-{}.json",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let options = qa::Options {
                qa_report: Some(report.clone()),
                ..qa::Options::default()
            };
            let report_file = qa::ReportFile::create(&report).unwrap();
            let app = NativeEditor::new(
                &options,
                Some(report_file),
                Arc::new(AtomicBool::new(false)),
                None,
            );
            let context = egui::Context::default();
            configure_context(&context);
            context.enable_accesskit();
            let mut harness = Self {
                app,
                context,
                frame: eframe::Frame::_new_kittest(),
                time: 0.0,
                nodes: HashMap::new(),
                commands: Vec::new(),
                viewport_commands: Vec::new(),
                report,
            };
            harness.step(Vec::new());
            harness.step(Vec::new());
            harness
        }

        fn step(&mut self, events: Vec<egui::Event>) {
            self.time += 0.016;
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(920.0, 720.0),
                )),
                time: Some(self.time),
                focused: true,
                events,
                ..Default::default()
            };
            self.app.raw_input_hook(&self.context, &mut input);
            let (app, frame) = (&mut self.app, &mut self.frame);
            let mut output = self.context.run_ui(input, |ui| app.ui(ui, frame));
            self.commands.append(&mut output.platform_output.commands);
            for viewport in output.viewport_output.values() {
                self.viewport_commands
                    .extend(viewport.commands.iter().cloned());
            }
            let update = output.platform_output.accesskit_update.take();
            output.drop_without_applying_deltas();
            if let Some(update) = update {
                self.nodes = update.nodes.into_iter().collect();
            }
        }

        fn preedit(&mut self, text: &str) {
            assert!(self.editor_focused(), "the document must own the IME");
            self.step(vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                text: text.into(),
                active_range_chars: None,
            })]);
            assert!(self.app.editor.composition().is_some());
        }

        fn click(&mut self, label: &str, role: Role) {
            self.click_with_release_events(label, role, Vec::new());
        }

        fn click_with_release_events(
            &mut self,
            label: &str,
            role: Role,
            mut release_events: Vec<egui::Event>,
        ) {
            self.step(Vec::new());
            let node = self
                .nodes
                .values()
                .find(|node| {
                    node.role() == role
                        && (node.label() == Some(label) || node.value() == Some(label))
                })
                .unwrap_or_else(|| panic!("no {role:?} labelled {label}"));
            let bounds = node.bounds().unwrap();
            let position = egui::pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            );
            let button = |pressed| egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.step(vec![egui::Event::PointerMoved(position), button(true)]);
            release_events.push(button(false));
            self.step(release_events);
            self.step(Vec::new());
        }

        fn accessibility_click(&mut self, label: &str) {
            let (&target, _) = self
                .nodes
                .iter()
                .find(|(_, node)| node.role() == Role::Button && node.label() == Some(label))
                .unwrap_or_else(|| panic!("no button labelled {label}"));
            self.step(vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Click,
                    target_node: target,
                    target_tree: egui::accesskit::TreeId::ROOT,
                    data: None,
                },
            )]);
        }

        fn choose_paragraph_kind(&mut self, shown: &str, choice: &str) {
            self.click(shown, Role::ComboBox);
            self.click(choice, Role::Button);
        }

        fn editor_focused(&self) -> bool {
            let id = self.app.document_id.unwrap();
            self.context.memory(|memory| memory.has_focus(id))
        }

        fn assert_no_command_errors(&self) {
            let qa = self.app.qa.as_ref().unwrap();
            assert_eq!(qa.host_command_errors(), 0, "{}", self.app.status);
        }

        fn kinds(&self, paragraphs: std::ops::RangeInclusive<usize>) -> Vec<ParagraphKind> {
            paragraphs
                .map(|index| self.app.editor.document().paragraph(index).unwrap().kind())
                .collect()
        }

        fn numbers(&self, paragraphs: std::ops::RangeInclusive<usize>) -> Vec<Option<u32>> {
            paragraphs
                .map(|index| self.app.editor.document().list_number(index))
                .collect()
        }

        fn has_label(&mut self, text: &str) -> bool {
            self.step(Vec::new());
            self.nodes
                .values()
                .any(|node| node.role() == Role::Label && node.value() == Some(text))
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.report);
        }
    }

    #[test]
    fn formatting_during_ime_composition_commits_it_instead_of_failing() {
        for (preedits, committed) in [(&["にほん"][..], "にほん"), (&["n", ""], "")] {
            let mut harness = Harness::new("ime-format");
            for preedit in preedits {
                harness.preedit(preedit);
            }
            harness.click("Bold", Role::Button);
            assert_eq!(harness.app.status, "Ready", "{preedits:?}");
            harness.assert_no_command_errors();
            let editor = &harness.app.editor;
            assert!(editor.composition().is_none());
            assert!(editor.typing_style().bold);
            let paragraph = editor.document().paragraph(1).unwrap().text();
            assert!(paragraph.starts_with(&format!("{committed}Select")));
        }
    }

    #[test]
    fn undo_during_ime_composition_preserves_the_previous_edit() {
        for queued_commit in [None, Some("日本")] {
            let mut harness = Harness::new("ime-undo");
            harness.step(vec![egui::Event::Text("existing ".into())]);
            harness.app.editor.break_history_group();
            let before_composition = harness.app.editor.document().plain_text();
            harness.preedit("にほん");
            let events = queued_commit
                .map(|text| egui::Event::Ime(egui::ImeEvent::Commit(text.into())))
                .into_iter()
                .collect();
            harness.click_with_release_events("Undo", Role::Button, events);
            harness.assert_no_command_errors();
            assert!(harness.app.editor.composition().is_none());
            assert_eq!(
                harness.app.editor.document().plain_text(),
                before_composition
            );
            assert!(harness.editor_focused());
            harness.click("Redo", Role::Button);
            let committed = queued_commit.unwrap_or("にほん");
            assert!(
                harness
                    .app
                    .editor
                    .document()
                    .plain_text()
                    .contains(&format!("existing {committed}Select"))
            );
        }
    }

    #[test]
    fn undo_can_remove_the_initial_ime_composition() {
        let mut harness = Harness::new("ime-first-undo");
        let original = harness.app.editor.document().plain_text();
        assert!(!harness.app.editor.can_undo());
        harness.preedit("にほん");
        harness.click("Undo", Role::Button);
        harness.assert_no_command_errors();
        assert!(harness.app.editor.composition().is_none());
        assert_eq!(harness.app.editor.document().plain_text(), original);
        harness.click("Redo", Role::Button);
        assert!(
            harness
                .app
                .editor
                .document()
                .plain_text()
                .starts_with("TextLoom\nにほんSelect")
        );
    }

    #[test]
    fn toolbar_composition_commit_filters_control_characters() {
        for accessibility in [false, true] {
            for preedit in ["に\0ほん", "\0"] {
                let mut harness = Harness::new("ime-toolbar-controls");
                harness.preedit(preedit);
                if accessibility {
                    harness.accessibility_click("Bold");
                } else {
                    harness.click("Bold", Role::Button);
                }
                harness.assert_no_command_errors();
                assert!(harness.app.editor.composition().is_none());
                assert!(!harness.app.editor.document().plain_text().contains('\0'));
            }
        }
    }

    #[test]
    fn toolbar_composition_commit_ignores_a_following_ime_echo() {
        for accessibility in [false, true] {
            let mut harness = Harness::new("ime-toolbar-echo");
            harness.preedit("にほん");
            if accessibility {
                harness.accessibility_click("Bold");
            } else {
                harness.click("Bold", Role::Button);
            }
            let committed = harness.app.editor.document().plain_text();
            harness.step(vec![egui::Event::Ime(egui::ImeEvent::Commit(
                "にほん".into(),
            ))]);
            harness.assert_no_command_errors();
            assert_eq!(harness.app.editor.document().plain_text(), committed);
        }
    }

    #[test]
    fn restarting_numbering_targets_the_item_after_ime_replaces_a_selection() {
        let mut harness = Harness::new("ime-numbering-selection");
        harness.app.editor = Editor::from_text("first\nsecond\nthird");
        harness.app.editor.select_all();
        harness
            .app
            .editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 2,
                start: Some(7),
            })
            .unwrap();
        harness.preedit("にほん");
        harness.accessibility_click("Restart at 1");
        harness.assert_no_command_errors();
        assert_eq!(harness.app.editor.document().plain_text(), "にほん");
        assert_eq!(
            harness.kinds(0..=0),
            [ParagraphKind::Ordered {
                indent: 2,
                start: Some(1),
            }]
        );
    }

    #[test]
    fn numbering_commands_leave_body_text_alone_after_ime_merges_the_item_away() {
        for label in ["Restart at 1", "Continue numbering"] {
            for accessibility in [false, true] {
                let mut harness = Harness::new("ime-numbering-merge");
                harness.app.editor = Editor::from_text("first\nsecond\nthird");
                let editor = &mut harness.app.editor;
                editor
                    .set_selection(Selection::caret(Position::new(2, 0)))
                    .unwrap();
                editor
                    .set_paragraph_kind(ParagraphKind::Ordered {
                        indent: 2,
                        start: Some(7),
                    })
                    .unwrap();
                editor
                    .set_selection(Selection::new(Position::new(0, 2), Position::new(2, 3)))
                    .unwrap();
                assert!(harness.has_label("Item 7."));
                harness.preedit("に");
                if accessibility {
                    harness.accessibility_click(label);
                } else {
                    harness.click(label, Role::Button);
                }
                harness.assert_no_command_errors();
                assert_eq!(harness.app.editor.document().plain_text(), "fiにrd");
                assert_eq!(
                    harness.kinds(0..=0),
                    [ParagraphKind::Body],
                    "{label} accessibility={accessibility}"
                );
            }
        }
    }

    #[test]
    fn numbering_command_batched_after_a_caret_move_keeps_body_text() {
        let mut harness = Harness::new("batched-numbering");
        harness.app.editor = Editor::from_text("first\nsecond");
        harness
            .app
            .editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 1,
                start: Some(7),
            })
            .unwrap();
        assert!(harness.has_label("Item 7."));
        assert!(harness.editor_focused());
        let (&target, _) = harness
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::Button && node.label() == Some("Restart at 1"))
            .unwrap();
        harness.step(vec![
            plain_key(egui::Key::ArrowDown),
            egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Click,
                target_node: target,
                target_tree: egui::accesskit::TreeId::ROOT,
                data: None,
            }),
        ]);
        harness.assert_no_command_errors();
        assert_eq!(harness.app.editor.selection().focus.paragraph, 1);
        assert_eq!(
            harness.kinds(0..=1),
            [
                ParagraphKind::Ordered {
                    indent: 1,
                    start: Some(7)
                },
                ParagraphKind::Body
            ]
        );
    }

    #[test]
    fn redo_during_ime_composition_commits_it_before_changing_history() {
        let mut harness = Harness::new("ime-redo");
        harness.step(vec![egui::Event::Text("undone ".into())]);
        harness.click("Undo", Role::Button);
        harness.preedit("にほん");
        harness.click("Redo", Role::Button);
        harness.assert_no_command_errors();
        assert!(harness.app.editor.composition().is_none());
        assert!(
            harness
                .app
                .editor
                .document()
                .plain_text()
                .starts_with("TextLoom\nにほんSelect")
        );
        assert!(!harness.app.editor.can_redo());
    }

    #[test]
    fn capturing_a_snapshot_commits_the_visible_ime_composition() {
        let mut harness = Harness::new("ime-snapshot");
        harness.preedit("にほん");
        harness.click("Capture snapshot", Role::Button);
        harness.assert_no_command_errors();
        assert!(harness.app.editor.composition().is_none());
        let captured = Document::from_bytes(harness.app.snapshot.as_ref().unwrap()).unwrap();
        assert_eq!(
            captured.plain_text(),
            harness.app.editor.document().plain_text()
        );
        assert!(captured.plain_text().starts_with("TextLoom\nにほんSelect"));
    }

    #[test]
    fn restoring_a_snapshot_reopens_documents_over_the_default_decode_limits() {
        let mut harness = Harness::new("large-snapshot");
        harness.app.editor = Editor::from_text(&"\n".repeat(1_000_000));
        let (app, context) = (&mut harness.app, &harness.context);
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            app.apply_command(ui, EditorCommand::CaptureSnapshot);
            app.editor = Editor::from_text("edited");
            app.apply_command(ui, EditorCommand::RestoreSnapshot);
        });
        output.drop_without_applying_deltas();
        harness.assert_no_command_errors();
        assert_eq!(
            harness.app.status,
            "Snapshot restored; editing history cleared"
        );
        assert_eq!(harness.app.editor.document().paragraphs().len(), 1_000_001);
    }

    #[test]
    fn capturing_a_snapshot_uses_ime_input_queued_before_the_click() {
        for (event, committed) in [
            (egui::ImeEvent::Commit("日本".into()), "日本"),
            (
                egui::ImeEvent::Preedit {
                    text: "日本".into(),
                    active_range_chars: Some(0..2),
                },
                "日本",
            ),
            (
                egui::ImeEvent::Preedit {
                    text: String::new(),
                    active_range_chars: None,
                },
                "",
            ),
        ] {
            let mut harness = Harness::new("ime-queued-snapshot");
            harness.preedit("に");
            harness.click_with_release_events(
                "Capture snapshot",
                Role::Button,
                vec![egui::Event::Ime(event)],
            );
            harness.assert_no_command_errors();
            assert!(harness.app.editor.composition().is_none());
            let captured = Document::from_bytes(harness.app.snapshot.as_ref().unwrap()).unwrap();
            assert_eq!(
                captured.plain_text(),
                harness.app.editor.document().plain_text()
            );
            assert!(
                captured
                    .plain_text()
                    .starts_with(&format!("TextLoom\n{committed}Select"))
            );
        }
    }

    #[test]
    fn multiple_accessibility_toolbar_commands_are_applied_in_one_frame() {
        let mut harness = Harness::new("multiple-toolbar-commands");
        harness.app.editor.select_all();
        harness.step(Vec::new());
        let events = ["Bold", "Italic"]
            .map(|label| {
                let (&target, _) = harness
                    .nodes
                    .iter()
                    .find(|(_, node)| node.role() == Role::Button && node.label() == Some(label))
                    .unwrap();
                egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Click,
                    target_node: target,
                    target_tree: egui::accesskit::TreeId::ROOT,
                    data: None,
                })
            })
            .into_iter()
            .collect();
        harness.step(events);
        harness.assert_no_command_errors();
        let style = harness.app.editor.selection_style();
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.italic, Some(true));
    }

    #[test]
    fn replace_all_uses_search_options_changed_in_the_same_input_batch() {
        for (label, text, query, expected) in [
            ("Match case", "a A", "a", "x A"),
            ("Whole words", "cat scatter", "cat", "x scatter"),
        ] {
            let mut harness = Harness::new("replace-current-options");
            harness.app.editor = Editor::from_text(text);
            harness.app.query = query.into();
            harness.app.replacement = "x".into();
            harness.step(Vec::new());
            let events = [label, "Replace all"]
                .map(|label| {
                    let (&target, _) = harness
                        .nodes
                        .iter()
                        .find(|(_, node)| node.label() == Some(label))
                        .unwrap();
                    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Click,
                        target_node: target,
                        target_tree: egui::accesskit::TreeId::ROOT,
                        data: None,
                    })
                })
                .into_iter()
                .collect();
            harness.step(events);
            assert_eq!(harness.app.editor.document().plain_text(), expected);
            harness.assert_no_command_errors();
        }
    }

    #[test]
    fn find_uses_search_options_changed_in_the_same_input_batch() {
        for (label, text, query, expected) in [
            ("Match case", "A a A", "a", 2..3),
            ("Whole words", "scatter cat scatter", "cat", 8..11),
        ] {
            for button in ["Next", "Previous"] {
                let mut harness = Harness::new("find-current-options");
                harness.app.editor = Editor::from_text(text);
                if button == "Previous" {
                    harness
                        .app
                        .editor
                        .set_selection(Selection::caret(Position::new(0, text.len())))
                        .unwrap();
                }
                harness.app.query = query.into();
                harness.step(Vec::new());
                let events = [label, button]
                    .map(|label| {
                        let (&target, _) = harness
                            .nodes
                            .iter()
                            .find(|(_, node)| node.label() == Some(label))
                            .unwrap();
                        egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                            action: egui::accesskit::Action::Click,
                            target_node: target,
                            target_tree: egui::accesskit::TreeId::ROOT,
                            data: None,
                        })
                    })
                    .into_iter()
                    .collect();
                harness.step(events);
                assert_eq!(
                    harness.app.editor.selection(),
                    Selection::new(
                        Position::new(0, expected.start),
                        Position::new(0, expected.end)
                    )
                );
                harness.assert_no_command_errors();
            }
        }
    }

    fn field_press(harness: &Harness, field: usize) -> Vec<egui::Event> {
        let id = harness.app.text_field_ids[field].unwrap();
        let bounds = harness.nodes[&id.accesskit_id()].bounds().unwrap();
        let position = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn plain_key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn accessibility_focus(id: egui::Id) -> egui::Event {
        egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Focus,
            target_node: id.accesskit_id(),
            target_tree: egui::accesskit::TreeId::ROOT,
            data: None,
        })
    }

    fn field_value(app: &NativeEditor, field: usize) -> &str {
        match field {
            0 => app.link_url.as_deref().unwrap(),
            1 => &app.query,
            2 => &app.replacement,
            _ => unreachable!(),
        }
    }

    #[test]
    fn accessibility_focus_routes_text_and_paste_between_native_fields() {
        for max_passes in [1, 2, 8] {
            for (source, target) in [(1, 2), (2, 1), (0, 1), (1, 0)] {
                let mut harness = Harness::new("field-focus-text");
                harness.context.options_mut(|options| {
                    options.max_passes = std::num::NonZeroUsize::new(max_passes).unwrap();
                });
                harness.app.editor = Editor::from_text("document");
                harness.app.link_url = Some(String::new());
                harness.step(Vec::new());
                let source_id = harness.app.text_field_ids[source].unwrap();
                let target_id = harness.app.text_field_ids[target].unwrap();
                harness
                    .context
                    .memory_mut(|memory| memory.request_focus(source_id));
                harness.step(Vec::new());
                harness.step(vec![
                    egui::Event::Text("before".into()),
                    accessibility_focus(target_id),
                    egui::Event::Paste("after".into()),
                ]);
                for _ in 0..3 {
                    harness.step(Vec::new());
                }
                assert_eq!(field_value(&harness.app, source), "before");
                assert_eq!(field_value(&harness.app, target), "after");
                assert_eq!(harness.app.editor.document().plain_text(), "document");
                assert!(harness.context.memory(|memory| memory.has_focus(target_id)));
                harness.assert_no_command_errors();
            }
        }
    }

    #[test]
    fn accessibility_focus_routes_each_document_and_field_input_segment_once() {
        for max_passes in [1, 2, 8] {
            for from_document in [false, true] {
                let mut harness = Harness::new("document-focus-text");
                harness.context.options_mut(|options| {
                    options.max_passes = std::num::NonZeroUsize::new(max_passes).unwrap();
                });
                harness.app.editor = Editor::default();
                let document = harness.app.document_id.unwrap();
                let field = harness.app.text_field_ids[1].unwrap();
                let (source, target) = if from_document {
                    (document, field)
                } else {
                    (field, document)
                };
                harness
                    .context
                    .memory_mut(|memory| memory.request_focus(source));
                harness.step(Vec::new());
                harness.step(vec![
                    egui::Event::Text("before".into()),
                    accessibility_focus(target),
                    egui::Event::Text("after".into()),
                    accessibility_focus(source),
                    egui::Event::Paste("!".into()),
                ]);
                for _ in 0..5 {
                    harness.step(Vec::new());
                }
                let (document_text, query) = if from_document {
                    ("before!", "after")
                } else {
                    ("after", "before!")
                };
                assert_eq!(harness.app.editor.document().plain_text(), document_text);
                assert_eq!(harness.app.query, query);
                assert!(harness.context.memory(|memory| memory.has_focus(source)));
                harness.assert_no_command_errors();
            }
        }
    }

    #[test]
    fn accessibility_focus_preserves_document_ime_handoff_across_input_passes() {
        for max_passes in [1, 2, 8] {
            for cleared in [false, true] {
                for fresh in [false, true] {
                    let mut harness = Harness::new("focus-ime-passes");
                    harness.context.options_mut(|options| {
                        options.max_passes = std::num::NonZeroUsize::new(max_passes).unwrap();
                    });
                    harness.app.editor = Editor::from_text("original");
                    harness.app.editor.select_all();
                    let field = harness.app.text_field_ids[2].unwrap();
                    let mut events = vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                        text: "한".into(),
                        active_range_chars: None,
                    })];
                    if cleared {
                        events.push(egui::Event::Ime(egui::ImeEvent::Preedit {
                            text: String::new(),
                            active_range_chars: None,
                        }));
                    }
                    events.push(accessibility_focus(field));
                    if fresh {
                        events.push(egui::Event::Ime(egui::ImeEvent::Preedit {
                            text: "新".into(),
                            active_range_chars: None,
                        }));
                    }
                    events.push(egui::Event::Ime(egui::ImeEvent::Commit(
                        if fresh { "新" } else { "한" }.into(),
                    )));
                    harness.step(events);
                    for _ in 0..3 {
                        harness.step(Vec::new());
                    }
                    assert_eq!(
                        harness.app.editor.document().plain_text(),
                        if cleared { "original" } else { "한" }
                    );
                    assert_eq!(harness.app.replacement, if fresh { "新" } else { "" });
                    assert!(harness.app.editor.composition().is_none());
                    harness.assert_no_command_errors();
                }
            }
        }
    }

    #[test]
    fn field_keys_after_focus_moves_do_not_submit_or_cancel_the_previous_field() {
        for (source, key) in [
            (1, egui::Key::Enter),
            (0, egui::Key::Enter),
            (0, egui::Key::Escape),
        ] {
            for accessibility in [false, true] {
                for separate_frame in [false, true] {
                    let mut harness = Harness::new("field-key-ownership");
                    harness.app.editor = Editor::from_text("before target after");
                    harness.app.query = "target".into();
                    if source == 0 {
                        harness.app.link_url = Some("https://example.com/".into());
                    }
                    harness.step(Vec::new());
                    let id = harness.app.text_field_ids[source].unwrap();
                    harness
                        .context
                        .memory_mut(|memory| memory.request_focus(id));
                    harness.step(Vec::new());
                    let selection = harness.app.editor.selection();
                    let mut events = if accessibility {
                        vec![egui::Event::AccessKitActionRequest(
                            egui::accesskit::ActionRequest {
                                action: egui::accesskit::Action::Focus,
                                target_node: harness.app.text_field_ids[2].unwrap().accesskit_id(),
                                target_tree: egui::accesskit::TreeId::ROOT,
                                data: None,
                            },
                        )]
                    } else {
                        field_press(&harness, 2)
                    };
                    if separate_frame {
                        harness.step(std::mem::take(&mut events));
                    }
                    events.push(plain_key(key));
                    harness.step(events);
                    assert_eq!(harness.app.editor.selection(), selection);
                    assert_eq!(
                        harness.app.editor.document().plain_text(),
                        "before target after"
                    );
                    assert_ne!(harness.app.status, "Match selected");
                    if source == 0 {
                        assert_eq!(
                            harness.app.link_url.as_deref(),
                            Some("https://example.com/")
                        );
                    }
                    harness.assert_no_command_errors();
                }
            }
        }
    }

    #[test]
    fn field_enter_respects_click_acquisition_and_submission_before_leaving() {
        for field in [0, 1] {
            for entering in [false, true] {
                let mut harness = Harness::new("field-enter-order");
                harness.app.editor = Editor::from_text("before target after");
                harness.app.editor.select_all();
                harness.app.query = "target".into();
                if field == 0 {
                    harness.app.link_url = Some("https://example.com/".into());
                }
                harness.step(Vec::new());
                let mut events = if entering {
                    field_press(&harness, field)
                } else {
                    let id = harness.app.text_field_ids[field].unwrap();
                    harness
                        .context
                        .memory_mut(|memory| memory.request_focus(id));
                    harness.step(Vec::new());
                    Vec::new()
                };
                events.push(plain_key(egui::Key::Enter));
                if !entering {
                    events.extend(field_press(&harness, 2));
                }
                harness.step(events);
                if field == 0 {
                    assert!(harness.app.link_url.is_none());
                    assert_eq!(
                        harness.app.editor.selected_link().unwrap().as_str(),
                        "https://example.com/"
                    );
                } else {
                    assert_eq!(harness.app.status, "Match selected");
                    assert_eq!(harness.app.editor.selected_text(), "target");
                }
                harness.assert_no_command_errors();
            }
        }
    }

    #[test]
    fn delayed_keyboard_focus_transfer_does_not_submit_the_previous_field() {
        for field in [0, 1] {
            let mut harness = Harness::new("field-delayed-enter");
            harness.app.editor = Editor::from_text("before target after");
            harness.app.query = "target".into();
            if field == 0 {
                harness.app.link_url = Some("https://example.com/".into());
            }
            harness.step(Vec::new());
            let id = harness.app.text_field_ids[field].unwrap();
            harness
                .context
                .memory_mut(|memory| memory.request_focus(id));
            harness.step(Vec::new());
            harness.step(vec![egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::SHIFT,
            }]);
            let selection = harness.app.editor.selection();
            harness.step(vec![plain_key(egui::Key::Enter)]);
            assert_eq!(harness.app.editor.selection(), selection);
            assert_eq!(
                harness.app.editor.document().plain_text(),
                "before target after"
            );
            if field == 0 {
                assert_eq!(
                    harness.app.link_url.as_deref(),
                    Some("https://example.com/")
                );
            }
            harness.assert_no_command_errors();
        }
        let mut harness = Harness::new("field-normal-escape");
        harness.app.open_link_field();
        harness.step(Vec::new());
        harness.step(vec![plain_key(egui::Key::Escape)]);
        assert!(harness.app.link_url.is_none());
    }

    #[test]
    fn find_and_replace_during_ime_composition_commit_it_instead_of_failing() {
        let mut harness = Harness::new("ime-search");
        harness.app.query = "café".into();
        harness.app.replacement = "cafe".into();
        harness.preedit("にほん");
        harness.click("Next", Role::Button);
        assert_eq!(harness.app.status, "Match selected");
        assert!(harness.app.editor.composition().is_none());
        harness
            .app
            .editor
            .set_selection(Selection::caret(Position::new(0, 0)))
            .unwrap();
        harness.preedit("ご");
        harness.click("Replace all", Role::Button);
        assert_eq!(harness.app.status, "Replaced 1 matches");
        harness.assert_no_command_errors();
        let text = harness.app.editor.document().plain_text();
        assert!(text.starts_with("ごTextLoom\nにほんSelect"), "{text}");
        assert!(text.contains("Unicode: cafe, "), "{text}");
    }

    #[test]
    fn choosing_the_current_list_kind_keeps_numbers_and_indent() {
        let mut harness = Harness::new("list-kind");
        let editor = &mut harness.app.editor;
        editor
            .set_selection(Selection::new(Position::new(2, 0), Position::new(4, 1)))
            .unwrap();
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: None,
            })
            .unwrap();
        editor
            .set_selection(Selection::caret(Position::new(3, 0)))
            .unwrap();
        let numbered = harness.kinds(2..=4);
        let undo = harness.app.editor.undo_len();
        harness.choose_paragraph_kind("Numbered list", "Numbered list");
        assert_eq!(harness.kinds(2..=4), numbered);
        assert_eq!(harness.app.editor.undo_len(), undo);

        harness.app.editor.indent_list().unwrap();
        let nested = harness.kinds(2..=4);
        harness.choose_paragraph_kind("Numbered list", "Numbered list");
        assert_eq!(harness.kinds(2..=4), nested);
        harness.choose_paragraph_kind("Numbered list", "Bullets");
        assert_eq!(harness.kinds(3..=3), [ParagraphKind::Bullet { indent: 1 }]);
        harness.choose_paragraph_kind("Bullets", "Bullets");
        assert_eq!(harness.kinds(3..=3), [ParagraphKind::Bullet { indent: 1 }]);
        harness.assert_no_command_errors();
    }

    #[test]
    fn choosing_the_shown_kind_converts_a_mixed_selection() {
        let mut harness = Harness::new("mixed-kind");
        harness
            .app
            .editor
            .set_selection(Selection::new(Position::new(1, 0), Position::new(2, 3)))
            .unwrap();
        harness.choose_paragraph_kind("Bullets", "Bullets");
        assert_eq!(
            harness.kinds(1..=2),
            [ParagraphKind::Bullet { indent: 0 }; 2]
        );
        harness.assert_no_command_errors();
    }

    #[test]
    fn choosing_numbered_list_for_several_items_keeps_their_numbers() {
        let ordered = |start| ParagraphKind::Ordered { indent: 0, start };
        let mut harness = Harness::new("list-range");
        let editor = &mut harness.app.editor;
        editor
            .set_selection(Selection::new(Position::new(2, 0), Position::new(4, 1)))
            .unwrap();
        editor.set_paragraph_kind(ordered(Some(5))).unwrap();
        editor
            .set_selection(Selection::new(Position::new(3, 0), Position::new(4, 1)))
            .unwrap();
        let undo = editor.undo_len();
        harness.choose_paragraph_kind("Numbered list", "Numbered list");
        assert_eq!(
            harness.kinds(2..=4),
            [ordered(Some(5)), ordered(None), ordered(None)]
        );
        assert_eq!(harness.numbers(2..=4), [Some(5), Some(6), Some(7)]);
        assert_eq!(harness.app.editor.undo_len(), undo);

        harness
            .app
            .editor
            .set_selection(Selection::new(Position::new(1, 0), Position::new(3, 1)))
            .unwrap();
        harness.choose_paragraph_kind("Numbered list", "Numbered list");
        assert_eq!(
            harness.kinds(1..=4),
            [
                ordered(Some(1)),
                ordered(None),
                ordered(None),
                ordered(None)
            ]
        );
        assert_eq!(harness.numbers(1..=4), [Some(1), Some(2), Some(3), Some(4)]);

        let editor = &mut harness.app.editor;
        editor
            .set_selection(Selection::caret(Position::new(4, 0)))
            .unwrap();
        editor.set_paragraph_kind(ParagraphKind::Body).unwrap();
        editor
            .set_selection(Selection::new(Position::new(3, 0), Position::new(4, 1)))
            .unwrap();
        harness.choose_paragraph_kind("Body", "Numbered list");
        assert_eq!(harness.numbers(3..=4), [Some(3), Some(4)]);
        harness.assert_no_command_errors();
    }

    #[test]
    fn quote_choice_quotes_paragraphs_and_publishes_one_blockquote() {
        let mut harness = Harness::new("quote-kind");
        harness
            .app
            .editor
            .set_selection(Selection::new(Position::new(1, 0), Position::new(2, 3)))
            .unwrap();
        harness.choose_paragraph_kind("Bullets", "Quote");
        assert_eq!(harness.kinds(1..=2), [ParagraphKind::Quote; 2]);
        assert_eq!(harness.kinds(3..=3), [ParagraphKind::Bullet { indent: 0 }]);
        let undo = harness.app.editor.undo_len();
        harness.choose_paragraph_kind("Quote", "Quote");
        assert_eq!(harness.app.editor.undo_len(), undo);
        harness.step(Vec::new());
        let quotes: Vec<_> = harness
            .nodes
            .values()
            .filter(|node| node.role() == Role::Blockquote)
            .collect();
        assert_eq!(quotes.len(), 1);
        let members = quotes[0].children();
        assert_eq!(members.len(), 2);
        assert!(
            members
                .iter()
                .all(|member| harness.nodes[member].role() == Role::Paragraph)
        );

        let end = harness
            .app
            .editor
            .document()
            .paragraph(2)
            .unwrap()
            .text()
            .len();
        harness
            .app
            .editor
            .set_selection(Selection::caret(Position::new(2, end)))
            .unwrap();
        harness.app.focus_editor = true;
        harness.step(Vec::new());
        assert!(harness.editor_focused());
        let enter = || egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        harness.step(vec![enter()]);
        assert_eq!(harness.kinds(3..=3), [ParagraphKind::Quote]);
        harness.step(vec![enter()]);
        assert_eq!(harness.kinds(3..=3), [ParagraphKind::Body]);
        harness.choose_paragraph_kind("Body", "Quote");
        harness.choose_paragraph_kind("Quote", "Body");
        assert_eq!(harness.kinds(3..=3), [ParagraphKind::Body]);
        harness.assert_no_command_errors();
    }

    impl Harness {
        fn select(&mut self, paragraph: usize, bytes: Range<usize>) {
            self.app
                .editor
                .set_selection(Selection::new(
                    Position::new(paragraph, bytes.start),
                    Position::new(paragraph, bytes.end),
                ))
                .unwrap();
        }

        fn choose_color(&mut self, target: ColorTarget, choice: &str) {
            self.click(target.menu_label(), Role::Button);
            let mut entries = vec![target.none_label()];
            entries.extend(Color::STANDARD.map(|(name, _)| name));
            for entry in entries {
                assert!(
                    self.nodes
                        .values()
                        .any(|node| node.role() == Role::Button && node.label() == Some(entry)),
                    "{target:?} menu lacks {entry}"
                );
            }
            self.click(choice, Role::Button);
        }
    }

    #[test]
    fn fill_color_applies_the_last_fill_and_its_menu_offers_excels_palette() {
        let mut harness = Harness::new("fill-color");
        harness.select(1, 0..6);
        harness.click("Fill Color", Role::Button);
        let style = harness.app.editor.selection_style();
        assert_eq!(style.background, Some(Some(Color::YELLOW)));
        assert_eq!(style.foreground, Some(None));

        harness.choose_color(ColorTarget::Fill, "Light Blue");
        assert_eq!(
            harness.app.editor.selection_style().background,
            Some(Some(Color::LIGHT_BLUE))
        );
        assert_eq!(harness.app.fill_color, Color::LIGHT_BLUE);
        harness.select(1, 7..11);
        harness.click("Fill Color", Role::Button);
        assert_eq!(
            harness.app.editor.selection_style().background,
            Some(Some(Color::LIGHT_BLUE))
        );

        let undo = harness.app.editor.undo_len();
        harness.choose_color(ColorTarget::Fill, "No Fill");
        assert_eq!(harness.app.editor.selection_style().background, Some(None));
        assert_eq!(harness.app.fill_color, Color::LIGHT_BLUE);
        assert_eq!(harness.app.editor.undo_len(), undo + 1);
        harness.app.editor.undo();
        assert_eq!(
            harness.app.editor.selection_style().background,
            Some(Some(Color::LIGHT_BLUE))
        );
        harness.assert_no_command_errors();
    }

    #[test]
    fn font_color_uses_the_same_palette_and_starts_with_red() {
        let mut harness = Harness::new("font-color");
        harness.select(1, 0..6);
        harness.click("Font Color", Role::Button);
        let style = harness.app.editor.selection_style();
        assert_eq!(style.foreground, Some(Some(Color::RED)));
        assert_eq!(style.background, Some(None));
        harness.choose_color(ColorTarget::Font, "Purple");
        assert_eq!(
            harness.app.editor.selection_style().foreground,
            Some(Some(Color::PURPLE))
        );
        assert_eq!(harness.app.font_color, Color::PURPLE);
        harness.choose_color(ColorTarget::Font, "Automatic");
        assert_eq!(harness.app.editor.selection_style().foreground, Some(None));
        assert_eq!(harness.app.font_color, Color::PURPLE);
        assert_eq!(harness.app.fill_color, Color::YELLOW);
        harness.assert_no_command_errors();
    }

    #[test]
    fn fill_and_font_color_activated_in_one_frame_both_apply() {
        let mut harness = Harness::new("fill-and-font-color");
        harness.select(1, 0..6);
        harness.step(Vec::new());
        let events = [ColorTarget::Fill, ColorTarget::Font]
            .map(|target| {
                let (&node, _) = harness
                    .nodes
                    .iter()
                    .find(|(_, node)| {
                        node.role() == Role::Button && node.label() == Some(target.label())
                    })
                    .unwrap();
                egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Click,
                    target_node: node,
                    target_tree: egui::accesskit::TreeId::ROOT,
                    data: None,
                })
            })
            .into_iter()
            .collect();
        let undo = harness.app.editor.undo_len();
        harness.step(events);
        harness.assert_no_command_errors();
        let style = harness.app.editor.selection_style();
        assert_eq!(style.background, Some(Some(Color::YELLOW)));
        assert_eq!(style.foreground, Some(Some(Color::RED)));
        assert_eq!(harness.app.editor.undo_len(), undo + 2);
    }

    #[test]
    fn color_controls_commit_ime_composition_and_format_typing() {
        let mut harness = Harness::new("ime-fill");
        harness.preedit("にほん");
        harness.click("Fill Color", Role::Button);
        harness.assert_no_command_errors();
        let editor = &harness.app.editor;
        assert!(editor.composition().is_none());
        assert_eq!(editor.typing_style().background, Some(Color::YELLOW));
        assert!(
            editor
                .document()
                .paragraph(1)
                .unwrap()
                .text()
                .starts_with("にほんSelect")
        );
    }

    #[test]
    fn shift_enter_starts_a_new_line_in_the_same_list_item() {
        let mut harness = Harness::new("soft-break");
        let item = Position::new(2, 0);
        harness
            .app
            .editor
            .set_selection(Selection::caret(item))
            .unwrap();
        assert!(harness.editor_focused());
        harness.step(vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::SHIFT,
        }]);
        let document = harness.app.editor.document();
        assert_eq!(document.paragraphs().len(), 5);
        let paragraph = document.paragraph(2).unwrap();
        assert_eq!(paragraph.kind(), ParagraphKind::Bullet { indent: 0 });
        assert!(paragraph.text().starts_with("\u{2028}Unicode"));
        harness.assert_no_command_errors();
    }

    #[test]
    fn numbering_controls_show_restart_and_continue_the_focused_item() {
        let mut harness = Harness::new("list-numbers");
        let editor = &mut harness.app.editor;
        editor
            .set_selection(Selection::new(Position::new(2, 0), Position::new(4, 1)))
            .unwrap();
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 0,
                start: None,
            })
            .unwrap();
        let selection = Selection::new(Position::new(2, 1), Position::new(3, 2));
        editor.set_selection(selection).unwrap();
        assert!(harness.has_label("Item 2."));
        let undo = harness.app.editor.undo_len();
        harness.click("Restart at 1", Role::Button);
        assert_eq!(harness.numbers(2..=4), [Some(1), Some(1), Some(2)]);
        assert_eq!(
            harness.kinds(2..=4)[1],
            ParagraphKind::Ordered {
                indent: 0,
                start: Some(1)
            }
        );
        assert_eq!(harness.app.editor.selection(), selection);
        assert_eq!(harness.app.editor.undo_len(), undo + 1);
        assert!(harness.has_label("Item 1."));

        harness.click("Continue numbering", Role::Button);
        assert_eq!(harness.numbers(2..=4), [Some(1), Some(2), Some(3)]);
        assert!(harness.has_label("Item 2."));
        assert!(harness.app.editor.undo());
        assert_eq!(harness.numbers(2..=4), [Some(1), Some(1), Some(2)]);
        assert_eq!(harness.app.editor.selection(), selection);

        harness
            .app
            .editor
            .set_selection(Selection::caret(Position::new(1, 0)))
            .unwrap();
        assert!(!harness.has_label("Item 1."));
        harness.assert_no_command_errors();
    }

    #[test]
    fn initial_fixture_matches_the_native_reader_report_sizes() {
        let harness = Harness::new("fixture-size");
        let document = harness.app.editor.document();
        let bytes = document.to_bytes();
        assert_eq!(&bytes[..5], b"TLFR\x02");
        assert_eq!(bytes.len(), 346);
        let text: usize = document
            .paragraphs()
            .iter()
            .map(|paragraph| paragraph.text().len())
            .sum();
        assert_eq!(text, 200);
    }

    #[test]
    fn paragraph_kind_follows_the_paragraphs_commands_change() {
        let selection = |anchor: (usize, usize), focus: (usize, usize)| {
            Selection::new(
                Position::new(anchor.0, anchor.1),
                Position::new(focus.0, focus.1),
            )
        };
        assert_eq!(kind_paragraph(selection((2, 0), (2, 0))), 2);
        assert_eq!(kind_paragraph(selection((1, 3), (3, 0))), 2);
        assert_eq!(kind_paragraph(selection((1, 0), (2, 0))), 1);
        assert_eq!(kind_paragraph(selection((3, 0), (1, 3))), 1);
        assert_eq!(kind_paragraph(selection((1, 3), (3, 2))), 3);
    }

    impl Harness {
        fn click_link(&mut self, label: &str, modifiers: egui::Modifiers) {
            self.time += 1.0;
            self.step(Vec::new());
            let bounds = self
                .nodes
                .values()
                .find(|node| node.role() == Role::Link && node.label() == Some(label))
                .unwrap_or_else(|| panic!("no link labelled {label}"))
                .bounds()
                .unwrap();
            let position = egui::pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            );
            let button = |pressed| egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            };
            self.step(vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::PointerMoved(position),
                button(true),
            ]);
            self.step(vec![button(false)]);
            self.step(vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE)]);
        }

        fn opened(&mut self) -> Vec<String> {
            self.commands
                .drain(..)
                .filter_map(|command| match command {
                    egui::OutputCommand::OpenUrl(open) => Some(open.url),
                    _ => None,
                })
                .collect()
        }

        fn link_at(&self, paragraph: usize, byte: usize) -> Option<String> {
            self.app
                .editor
                .document()
                .link_at(Position::new(paragraph, byte))
                .map(ToString::to_string)
        }
    }

    #[test]
    fn link_button_and_shortcut_add_edit_and_remove_links() {
        let mut harness = Harness::new("links");
        harness.select(1, 0..6);
        harness.step(vec![egui::Event::Key {
            key: egui::Key::K,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }]);
        assert_eq!(harness.app.link_url.as_deref(), Some(""));
        let undo = harness.app.editor.undo_len();
        harness.app.link_url = Some(" https://example.com/a ".into());
        harness.click("Apply link", Role::Button);
        assert_eq!(harness.app.link_url, None);
        assert_eq!(
            harness.link_at(1, 0).as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(harness.link_at(1, 6), None);
        assert_eq!(harness.app.editor.undo_len(), undo + 1);

        harness
            .app
            .editor
            .set_selection(Selection::caret(Position::new(1, 3)))
            .unwrap();
        harness.click("Link", Role::Button);
        assert_eq!(
            harness.app.link_url.as_deref(),
            Some("https://example.com/a")
        );
        harness.app.link_url = Some("https://example.com/b".into());
        harness.click("Apply link", Role::Button);
        for byte in [0, 5] {
            assert_eq!(
                harness.link_at(1, byte).as_deref(),
                Some("https://example.com/b")
            );
        }

        harness.click("Link", Role::Button);
        harness.app.link_url = Some("https://example.com/\u{7}".into());
        harness.click("Apply link", Role::Button);
        assert!(harness.app.link_url.is_some());
        assert_eq!(harness.app.status, textloom::Error::InvalidLink.to_string());
        harness.click("Cancel", Role::Button);
        assert_eq!(harness.app.link_url, None);
        assert_eq!(
            harness.link_at(1, 0).as_deref(),
            Some("https://example.com/b")
        );

        let undo = harness.app.editor.undo_len();
        harness.click("Remove link", Role::Button);
        assert_eq!(harness.link_at(1, 0), None);
        assert_eq!(harness.app.editor.undo_len(), undo + 1);
        assert!(harness.app.editor.undo());
        assert!(harness.link_at(1, 0).is_some());

        harness
            .app
            .editor
            .set_selection(Selection::caret(Position::new(0, 8)))
            .unwrap();
        harness.click("Link", Role::Button);
        assert_eq!(harness.app.link_url.as_deref(), Some(""));
        harness.app.link_url = Some("mailto:team@example.com".into());
        harness.click("Apply link", Role::Button);
        let heading = harness.app.editor.document().paragraph(0).unwrap();
        assert_eq!(heading.text(), "TextLoommailto:team@example.com");
        assert_eq!(
            harness.link_at(0, 8).as_deref(),
            Some("mailto:team@example.com")
        );
        assert_eq!(harness.app.qa.as_ref().unwrap().host_command_errors(), 1);
    }

    #[test]
    fn link_shortcut_preserves_altgr_text_and_combined_modifiers() {
        for modifiers in [
            egui::Modifiers::CTRL | egui::Modifiers::ALT | egui::Modifiers::COMMAND,
            egui::Modifiers::MAC_CMD | egui::Modifiers::ALT | egui::Modifiers::COMMAND,
            egui::Modifiers::CTRL | egui::Modifiers::MAC_CMD | egui::Modifiers::COMMAND,
        ] {
            let mut harness = Harness::new("link-altgr");
            harness.app.editor = Editor::from_text("abc");
            harness.app.editor.select_all();
            harness.step(vec![
                egui::Event::Key {
                    key: egui::Key::K,
                    physical_key: Some(egui::Key::K),
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                egui::Event::Text("ķ".into()),
            ]);
            assert!(
                harness.app.link_url.is_none(),
                "the link shortcut intercepted {modifiers:?}"
            );
            assert_eq!(harness.app.editor.document().plain_text(), "ķ");
            assert!(harness.editor_focused());
            harness.assert_no_command_errors();
        }
    }

    #[test]
    fn quit_shortcut_ignores_altgr_chords() {
        let quit = |modifiers| egui::Event::Key {
            key: egui::Key::Q,
            physical_key: Some(egui::Key::Q),
            pressed: true,
            repeat: false,
            modifiers,
        };
        // Windows reports AltGr as Ctrl+Alt; macOS types Option characters.
        for modifiers in [
            egui::Modifiers::CTRL | egui::Modifiers::ALT | egui::Modifiers::COMMAND,
            egui::Modifiers::MAC_CMD | egui::Modifiers::ALT | egui::Modifiers::COMMAND,
        ] {
            let mut harness = Harness::new("quit-altgr");
            harness.step(vec![quit(modifiers)]);
            assert!(
                !harness
                    .viewport_commands
                    .contains(&egui::ViewportCommand::Close),
                "{modifiers:?} closed the window"
            );
            assert!(harness.editor_focused());
        }
        let mut harness = Harness::new("quit");
        harness.step(vec![quit(egui::Modifiers::CTRL | egui::Modifiers::COMMAND)]);
        assert!(
            harness
                .viewport_commands
                .contains(&egui::ViewportCommand::Close)
        );
    }

    #[test]
    fn opening_link_field_does_not_receive_the_documents_late_ime_commit() {
        let mut harness = Harness::new("link-ime-handoff");
        harness.app.editor = Editor::from_text("abc");
        harness.app.editor.select_all();
        harness.preedit("한");
        harness.step(vec![egui::Event::Key {
            key: egui::Key::K,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }]);
        assert_eq!(harness.app.editor.document().plain_text(), "한");
        assert_eq!(harness.app.link_url.as_deref(), Some(""));
        harness.step(vec![egui::Event::Ime(egui::ImeEvent::Commit("한".into()))]);
        assert_eq!(harness.app.link_url.as_deref(), Some(""));
        assert_eq!(harness.app.editor.document().plain_text(), "한");
    }

    #[test]
    fn pointer_handoff_preserves_queued_and_cleared_document_preedit() {
        for field in [1, 2] {
            for (prior_empty, queued, candidate, document) in [
                (false, None, "한", "한"),
                (
                    false,
                    Some(egui::ImeEvent::Preedit {
                        text: "漢".into(),
                        active_range_chars: None,
                    }),
                    "漢",
                    "漢",
                ),
                (
                    false,
                    Some(egui::ImeEvent::Preedit {
                        text: String::new(),
                        active_range_chars: None,
                    }),
                    "한",
                    "abc",
                ),
                (
                    false,
                    Some(egui::ImeEvent::Commit("\0".into())),
                    "한",
                    "abc",
                ),
                (
                    false,
                    Some(egui::ImeEvent::Commit("\r\n".into())),
                    "한",
                    "한",
                ),
                (true, None, "한", "abc"),
            ] {
                for same_frame_echo in [false, true] {
                    let mut harness = Harness::new("field-ime-handoff");
                    harness.app.editor = Editor::from_text("abc");
                    harness.app.editor.select_all();
                    harness.preedit("한");
                    if prior_empty {
                        harness.preedit("");
                    }
                    let id = harness.app.text_field_ids[field].unwrap();
                    let bounds = harness.nodes[&id.accesskit_id()].bounds().unwrap();
                    let position = egui::pos2(
                        ((bounds.x0 + bounds.x1) / 2.0) as f32,
                        ((bounds.y0 + bounds.y1) / 2.0) as f32,
                    );
                    let mut events = Vec::new();
                    if let Some(event) = &queued {
                        events.push(egui::Event::Ime(event.clone()));
                    }
                    events.extend([
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]);
                    if same_frame_echo {
                        events.push(egui::Event::Ime(egui::ImeEvent::Commit(candidate.into())));
                    }
                    harness.step(events);
                    assert!(harness.context.memory(|memory| memory.has_focus(id)));
                    assert_eq!(harness.app.editor.document().plain_text(), document);
                    if !same_frame_echo {
                        harness.step(vec![egui::Event::Ime(egui::ImeEvent::Commit(
                            candidate.into(),
                        ))]);
                    }
                    assert!(harness.app.query.is_empty());
                    assert!(harness.app.replacement.is_empty());
                    harness.assert_no_command_errors();
                }
            }
        }
    }

    #[test]
    fn first_queued_preedit_stays_with_document_when_a_field_receives_focus() {
        for field in [1, 2] {
            for cleared in [false, true] {
                for same_frame_echo in [false, true] {
                    let mut harness = Harness::new("first-preedit-field-handoff");
                    harness.app.editor = Editor::from_text("abc");
                    harness.app.editor.select_all();
                    let id = harness.app.text_field_ids[field].unwrap();
                    let bounds = harness.nodes[&id.accesskit_id()].bounds().unwrap();
                    let position = egui::pos2(
                        ((bounds.x0 + bounds.x1) / 2.0) as f32,
                        ((bounds.y0 + bounds.y1) / 2.0) as f32,
                    );
                    let mut events = vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                        text: "한".into(),
                        active_range_chars: None,
                    })];
                    if cleared {
                        events.push(egui::Event::Ime(egui::ImeEvent::Preedit {
                            text: String::new(),
                            active_range_chars: None,
                        }));
                    }
                    events.extend([
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]);
                    if same_frame_echo {
                        events.push(egui::Event::Ime(egui::ImeEvent::Commit("한".into())));
                    }
                    harness.step(events);
                    assert!(harness.context.memory(|memory| memory.has_focus(id)));
                    assert_eq!(
                        harness.app.editor.document().plain_text(),
                        if cleared { "abc" } else { "한" }
                    );
                    if !same_frame_echo {
                        harness.step(vec![egui::Event::Ime(egui::ImeEvent::Commit("한".into()))]);
                    }
                    assert!(harness.app.query.is_empty());
                    assert!(harness.app.replacement.is_empty());
                    harness.assert_no_command_errors();
                }
            }
        }
    }

    #[test]
    fn keyboard_focus_handoff_does_not_receive_the_documents_late_ime_commit() {
        for cleared in [false, true] {
            for (mut transfer, expected) in [
                (vec![], ""),
                (
                    vec![egui::Event::Ime(egui::ImeEvent::Commit("한".into()))],
                    "",
                ),
                (
                    vec![
                        egui::Event::Ime(egui::ImeEvent::Preedit {
                            text: "新".into(),
                            active_range_chars: None,
                        }),
                        egui::Event::Ime(egui::ImeEvent::Commit("新".into())),
                    ],
                    "新",
                ),
                (
                    vec![
                        egui::Event::Text("x".into()),
                        egui::Event::Ime(egui::ImeEvent::Commit("한".into())),
                    ],
                    "x한",
                ),
            ] {
                let mut harness = Harness::new("tab-ime-handoff");
                harness.app.editor = Editor::from_text("abc");
                harness.app.editor.select_all();
                harness.preedit("한");
                if cleared {
                    harness.preedit("");
                }
                harness.step(vec![egui::Event::Key {
                    key: egui::Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::SHIFT,
                }]);
                let delayed_echo = transfer.is_empty();
                harness.step(std::mem::take(&mut transfer));
                let field = harness.app.text_field_ids[2].unwrap();
                assert!(harness.context.memory(|memory| memory.has_focus(field)));
                if delayed_echo {
                    harness.step(vec![egui::Event::Ime(egui::ImeEvent::Commit("한".into()))]);
                }
                assert_eq!(harness.app.replacement, expected);
                assert_eq!(
                    harness.app.editor.document().plain_text(),
                    if cleared { "abc" } else { "한" }
                );
                harness.assert_no_command_errors();
            }
        }
    }

    #[test]
    fn handoff_echo_guard_preserves_ignored_events_and_accepts_fresh_field_input() {
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        for (before_echo, expected) in [
            (vec![], ""),
            (
                vec![
                    egui::Event::Ime(egui::ImeEvent::Preedit {
                        text: "invalid".into(),
                        active_range_chars: Some(0..99),
                    }),
                    egui::Event::Ime(egui::ImeEvent::Commit("\r\n".into())),
                    egui::Event::Ime(egui::ImeEvent::Commit("\0".into())),
                    key(egui::Key::X, egui::Modifiers::NONE),
                ],
                "",
            ),
            (
                vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                    text: "한".into(),
                    active_range_chars: None,
                })],
                "한",
            ),
            (vec![egui::Event::Text("x".into())], "x한"),
            (
                vec![
                    key(egui::Key::V, egui::Modifiers::COMMAND),
                    egui::Event::Paste("x".into()),
                ],
                "x한",
            ),
        ] {
            for separate_frame in [false, true] {
                let mut harness = Harness::new("handoff-new-input");
                harness.app.editor = Editor::from_text("abc");
                harness.app.editor.select_all();
                harness.preedit("한");
                harness.step(vec![key(egui::Key::K, egui::Modifiers::COMMAND)]);
                let mut events = before_echo.clone();
                if separate_frame {
                    harness.step(std::mem::take(&mut events));
                }
                events.push(egui::Event::Ime(egui::ImeEvent::Commit("한".into())));
                harness.step(events);
                assert_eq!(harness.app.link_url.as_deref(), Some(expected));
                assert_eq!(harness.app.editor.document().plain_text(), "한");
                harness.assert_no_command_errors();
            }
        }
        for expire in [false, true] {
            let mut harness = Harness::new("handoff-guard-lifetime");
            harness.preedit("한");
            harness.step(vec![key(egui::Key::K, egui::Modifiers::COMMAND)]);
            if expire {
                harness.time += 1.0;
            } else {
                harness.step(vec![egui::Event::Ime(egui::ImeEvent::Commit("한".into()))]);
            }
            harness.step(vec![egui::Event::Ime(egui::ImeEvent::Commit("한".into()))]);
            assert_eq!(harness.app.link_url.as_deref(), Some("한"));
        }
        let mut harness = Harness::new("handoff-pointer-input");
        harness.preedit("한");
        harness.step(vec![key(egui::Key::K, egui::Modifiers::COMMAND)]);
        let field = harness.app.text_field_ids[0].unwrap();
        let bounds = harness.nodes[&field.accesskit_id()].bounds().unwrap();
        let position = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        harness.step(vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Ime(egui::ImeEvent::Commit("한".into())),
        ]);
        assert_eq!(harness.app.link_url.as_deref(), Some("한"));
    }

    #[test]
    fn ctrl_click_and_read_only_click_open_web_and_mail_links_only() {
        let mut harness = Harness::new("open-links");
        for (bytes, target) in [
            (0..6, "https://example.com/"),
            (7..11, "javascript:alert(1)"),
        ] {
            harness.select(1, bytes);
            harness
                .app
                .editor
                .set_link(Some(Link::new(target).unwrap()))
                .unwrap();
        }
        harness
            .app
            .editor
            .set_selection(Selection::caret(Position::new(0, 0)))
            .unwrap();
        harness.click_link("Select", egui::Modifiers::NONE);
        assert!(harness.opened().is_empty());
        assert_eq!(harness.app.editor.selection().focus.paragraph, 1);

        harness
            .app
            .editor
            .set_selection(Selection::caret(Position::new(0, 0)))
            .unwrap();
        harness.click_link("Select", egui::Modifiers::COMMAND);
        assert_eq!(harness.opened(), ["https://example.com/"]);
        assert_eq!(harness.app.status, "Opened https://example.com/");
        assert_eq!(
            harness.app.editor.selection(),
            Selection::caret(Position::new(0, 0))
        );

        harness.click_link("text", egui::Modifiers::COMMAND);
        assert!(harness.opened().is_empty());
        assert!(harness.app.status.starts_with("Not opening javascript:"));

        harness.app.read_only = true;
        harness.click_link("Select", egui::Modifiers::NONE);
        assert_eq!(harness.opened(), ["https://example.com/"]);
        harness.assert_no_command_errors();
    }

    #[test]
    fn followed_links_open_the_target_without_surrounding_whitespace() {
        let mut harness = Harness::new("open-trimmed");
        harness.select(1, 0..6);
        harness
            .app
            .editor
            .set_link(Some(Link::new("\u{3000}https://example.com/").unwrap()))
            .unwrap();
        harness
            .app
            .editor
            .set_selection(Selection::caret(Position::new(0, 0)))
            .unwrap();
        harness.click_link("Select", egui::Modifiers::COMMAND);
        assert_eq!(harness.opened(), ["https://example.com/"]);
        assert_eq!(harness.app.status, "Opened https://example.com/");
    }

    #[test]
    fn a_selection_inside_a_link_edits_and_removes_the_whole_link() {
        let mut harness = Harness::new("partial-link");
        harness.select(1, 0..11);
        harness
            .app
            .editor
            .set_link(Some(Link::new("https://exmaple.com/typo").unwrap()))
            .unwrap();
        let targets = |harness: &Harness| -> Vec<_> {
            [0, 6, 7, 10, 11]
                .into_iter()
                .map(|byte| harness.link_at(1, byte))
                .collect()
        };
        harness.select(1, 7..11);
        let selection = harness.app.editor.selection();
        harness.click("Link", Role::Button);
        assert_eq!(
            harness.app.link_url.as_deref(),
            Some("https://exmaple.com/typo")
        );
        let undo = harness.app.editor.undo_len();
        harness.app.link_url = Some("https://example.com/fixed".into());
        harness.click("Apply link", Role::Button);
        let fixed = Some("https://example.com/fixed".to_owned());
        assert_eq!(
            targets(&harness),
            [fixed.clone(), fixed.clone(), fixed.clone(), fixed, None]
        );
        assert_eq!(harness.app.editor.selection(), selection);
        assert_eq!(harness.app.editor.undo_len(), undo + 1);

        harness.select(1, 7..11);
        harness.click("Remove link", Role::Button);
        assert_eq!(targets(&harness), vec![None; 5]);
        assert_eq!(harness.app.editor.undo_len(), undo + 2);
        assert!(harness.app.editor.undo());
        assert_eq!(harness.app.editor.selection(), selection);
        assert_eq!(
            harness.link_at(1, 0).as_deref(),
            Some("https://example.com/fixed")
        );

        harness.select(1, 7..14);
        harness.click("Link", Role::Button);
        assert_eq!(harness.app.link_url.as_deref(), Some(""));
        harness.click("Apply link", Role::Button);
        let fixed = Some("https://example.com/fixed".to_owned());
        assert_eq!(targets(&harness), [fixed.clone(), fixed, None, None, None]);
        harness.assert_no_command_errors();
    }

    #[test]
    fn followed_links_and_the_link_field_apply_rules_match_the_host_ui() {
        let link = |target| Link::new(target).unwrap();
        for (target, opened) in [
            ("https://a", "https://a"),
            ("HTTP://a", "HTTP://a"),
            (" mailto:a@b", "mailto:a@b"),
            ("Https://x", "Https://x"),
            ("\u{3000}https://example.com/ ", "https://example.com/"),
        ] {
            assert_eq!(followable(&link(target)), Some(opened), "{target}");
        }
        for target in [
            "javascript:x",
            "file:///etc/passwd",
            "ftp://a",
            "data:,x",
            "a",
            "\u{3000}javascript:x",
        ] {
            assert_eq!(followable(&link(target)), None, "{target}");
        }
        let mut editor = Editor::from_text("one two");
        editor
            .set_selection(Selection::new(Position::new(0, 4), Position::new(0, 7)))
            .unwrap();
        apply_link(&mut editor, "https://two").unwrap();
        assert_eq!(editor.selected_link(), Some(link("https://two")));
        assert_eq!(
            apply_link(&mut editor, "\u{0}"),
            Err(textloom::Error::InvalidLink)
        );
        apply_link(&mut editor, "   ").unwrap();
        assert_eq!(editor.selected_link(), None);
        assert_eq!(editor.document().plain_text(), "one two");
    }
}
