//! Opt-in native QA evidence. Reports describe observations, never manual signoff.

use std::{
    ffi::OsString,
    fmt::Write as _,
    fs::OpenOptions,
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use textloom::{Composition, Editor, InlineStyle, Selection};

pub const HELP: &str = "TextLoom native example\n\nUsage: textloom-native-example [--smoke-test] [--read-only] [--disabled] [--font PATH] [--bold-font PATH] [--qa-report PATH]\n\n--smoke-test requests a graceful close after 20 native frames.\n--read-only permits selection and copy while preventing edits.\n--disabled disables document focus and interaction.\n--font adds a proportional font, such as a CJK fallback.\n--bold-font supplies the widget's Bold font family.\n--qa-report writes content-free JSON observations on graceful exit.\n            PATH must not already exist; reports do not certify manual checks.\n\nSet TEXTLOOM_QA_REVISION when compiling to identify the source revision.";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub smoke_test: bool,
    pub read_only: bool,
    pub disabled: bool,
    pub regular_font: Option<PathBuf>,
    pub bold_font: Option<PathBuf>,
    pub qa_report: Option<PathBuf>,
    pub help: bool,
}

impl Options {
    pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            if argument == "--smoke-test" {
                options.smoke_test = true;
            } else if argument == "--read-only" {
                options.read_only = true;
            } else if argument == "--disabled" {
                options.disabled = true;
            } else if argument == "--help" || argument == "-h" {
                options.help = true;
            } else {
                let target = match argument.to_str() {
                    Some("--font") => &mut options.regular_font,
                    Some("--bold-font") => &mut options.bold_font,
                    Some("--qa-report") => &mut options.qa_report,
                    _ => {
                        return Err(format!(
                            "Unknown argument: {}; use --help",
                            argument.to_string_lossy()
                        ));
                    }
                };
                if target.is_some() {
                    return Err(format!("{} was supplied twice", argument.to_string_lossy()));
                }
                let path = arguments
                    .next()
                    .filter(|path| !path.is_empty() && !path.to_string_lossy().starts_with("--"))
                    .ok_or_else(|| {
                        format!("{} requires a file path", argument.to_string_lossy())
                    })?;
                *target = Some(path.into());
            }
        }
        Ok(options)
    }
}

#[derive(Default)]
struct InputCounts {
    text: u64,
    key_press: u64,
    paste: u64,
    copy: u64,
    cut: u64,
    ime_preedit: u64,
    ime_commit: u64,
    ime_delete_surrounding: u64,
    accessibility_action: u64,
    window_focus_gained: u64,
    window_focus_lost: u64,
}

impl InputCounts {
    fn observe(&mut self, events: &[egui::Event]) {
        for event in events {
            match event {
                egui::Event::Text(_) => self.text += 1,
                egui::Event::Key { pressed: true, .. } => self.key_press += 1,
                egui::Event::Paste(_) => self.paste += 1,
                egui::Event::Copy => self.copy += 1,
                egui::Event::Cut => self.cut += 1,
                egui::Event::Ime(egui::ImeEvent::Preedit { .. }) => self.ime_preedit += 1,
                egui::Event::Ime(egui::ImeEvent::Commit(_)) => self.ime_commit += 1,
                egui::Event::Ime(egui::ImeEvent::DeleteSurrounding { .. }) => {
                    self.ime_delete_surrounding += 1;
                }
                egui::Event::AccessKitActionRequest(_) => self.accessibility_action += 1,
                egui::Event::WindowFocused(true) => self.window_focus_gained += 1,
                egui::Event::WindowFocused(false) => self.window_focus_lost += 1,
                _ => {}
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct FocusState {
    pub window_focused: bool,
    pub widget_focus_retained: bool,
}

pub struct Session {
    path: PathBuf,
    started_unix_seconds: u64,
    smoke_test: bool,
    regular_font_supplied: bool,
    bold_font_supplied: bool,
    frames: u64,
    input: InputCounts,
    revision: u64,
    selection: Selection,
    typing_style: InlineStyle,
    composition: Option<Composition>,
    focused: bool,
    focus_state: FocusState,
    enabled: bool,
    read_only: bool,
    revision_changes: u64,
    selection_changes: u64,
    typing_style_changes: u64,
    composition_changes: u64,
    focus_gains: u64,
    focus_losses: u64,
    enabled_changes: u64,
    read_only_changes: u64,
    editing_errors: u64,
    accessibility_errors: u64,
    host_command_errors: u64,
    snapshot_restores: u64,
    pixels_per_point: Option<f32>,
    minimum_pixels_per_point: Option<f32>,
    maximum_pixels_per_point: Option<f32>,
    native_pixels_per_point: Option<f32>,
}

impl Session {
    pub fn new(options: &Options, editor: &Editor) -> Option<Self> {
        Some(Self {
            path: options.qa_report.clone()?,
            started_unix_seconds: unix_seconds(),
            smoke_test: options.smoke_test,
            regular_font_supplied: options.regular_font.is_some(),
            bold_font_supplied: options.bold_font.is_some(),
            frames: 0,
            input: InputCounts::default(),
            revision: editor.document().revision(),
            selection: editor.selection(),
            typing_style: editor.typing_style(),
            composition: editor.composition().cloned(),
            focused: false,
            focus_state: FocusState::default(),
            enabled: !options.disabled,
            read_only: options.read_only,
            revision_changes: 0,
            selection_changes: 0,
            typing_style_changes: 0,
            composition_changes: 0,
            focus_gains: 0,
            focus_losses: 0,
            enabled_changes: 0,
            read_only_changes: 0,
            editing_errors: 0,
            accessibility_errors: 0,
            host_command_errors: 0,
            snapshot_restores: 0,
            pixels_per_point: None,
            minimum_pixels_per_point: None,
            maximum_pixels_per_point: None,
            native_pixels_per_point: None,
        })
    }

    pub fn begin_frame(&mut self, context: &egui::Context) {
        self.frames += 1;
        // Raw events retain key presses consumed later by widgets. These are host-wide
        // observations: the report does not claim the document processed each event.
        context.input(|input| self.input.observe(&input.raw.events));
        let scale = context.pixels_per_point();
        self.pixels_per_point = Some(scale);
        self.minimum_pixels_per_point = Some(
            self.minimum_pixels_per_point
                .map_or(scale, |old| old.min(scale)),
        );
        self.maximum_pixels_per_point = Some(
            self.maximum_pixels_per_point
                .map_or(scale, |old| old.max(scale)),
        );
        self.native_pixels_per_point = context.native_pixels_per_point();
    }

    pub fn observe_editor(
        &mut self,
        editor: &Editor,
        focus_state: FocusState,
        enabled: bool,
        read_only: bool,
    ) {
        macro_rules! observe {
            ($previous:ident, $counter:ident, $current:expr) => {
                let current = $current;
                if self.$previous != current {
                    self.$counter += 1;
                    self.$previous = current;
                }
            };
        }
        observe!(revision, revision_changes, editor.document().revision());
        observe!(selection, selection_changes, editor.selection());
        observe!(typing_style, typing_style_changes, editor.typing_style());
        observe!(
            composition,
            composition_changes,
            editor.composition().cloned()
        );
        observe!(enabled, enabled_changes, enabled);
        observe!(read_only, read_only_changes, read_only);
        let focused = focus_state.window_focused && focus_state.widget_focus_retained;
        self.focus_state = focus_state;
        if self.focused != focused {
            if focused {
                self.focus_gains += 1;
            } else {
                self.focus_losses += 1;
            }
            self.focused = focused;
        }
    }

    pub fn widget_errors(&mut self, editing: usize, accessibility: usize) {
        self.editing_errors += editing as u64;
        self.accessibility_errors += accessibility as u64;
    }

    pub fn host_command_error(&mut self) {
        self.host_command_errors += 1;
    }

    pub fn snapshot_restored(&mut self) {
        self.snapshot_restores += 1;
    }

    fn report(&self, editor: &Editor, source_revision: Option<&str>) -> String {
        let document = editor.document();
        let snapshot = document.to_bytes();
        let round_trip = textloom::Document::from_bytes(&snapshot)
            .is_ok_and(|restored| restored.to_bytes() == snapshot);
        let text_bytes: usize = document.paragraphs().iter().map(|p| p.text().len()).sum();
        let selection = editor.selection();
        let style = editor.typing_style();
        format!(
            concat!(
                "{{\n",
                "  \"schema_version\": 1,\n",
                "  \"manual_signoff\": \"not_recorded\",\n",
                "  \"document_content_included\": false,\n",
                "  \"host\": {{\"name\": \"textloom-native-example\", \"egui_api_version\": \"0.36\", \"eframe_api_version\": \"0.36\", \"os\": {os}, \"architecture\": {arch}, \"source_revision\": {source_revision}}},\n",
                "  \"session\": {{\"started_unix_seconds\": {started}, \"ended_unix_seconds\": {ended}, \"graceful_exit\": true, \"smoke_test_requested\": {smoke}, \"rendered_frames\": {frames}, \"regular_font_supplied\": {regular_font}, \"bold_font_supplied\": {bold_font}}},\n",
                "  \"display\": {{\"pixels_per_point\": {scale}, \"minimum_pixels_per_point\": {minimum_scale}, \"maximum_pixels_per_point\": {maximum_scale}, \"native_pixels_per_point\": {native_scale}}},\n",
                "  \"host_input_events\": {{\"text\": {text}, \"key_press\": {key}, \"paste\": {paste}, \"copy\": {copy}, \"cut\": {cut}, \"ime_preedit\": {preedit}, \"ime_commit\": {commit}, \"ime_delete_surrounding\": {delete_surrounding}, \"accessibility_action\": {accessibility_action}, \"window_focus_gained\": {window_focus_gained}, \"window_focus_lost\": {window_focus_lost}}},\n",
                "  \"observed_state_changes\": {{\"document_revision\": {revision_changes}, \"selection\": {selection_changes}, \"typing_style\": {typing_changes}, \"composition\": {composition_changes}, \"focus_gains\": {focus_gains}, \"focus_losses\": {focus_losses}, \"enabled\": {enabled_changes}, \"read_only\": {read_only_changes}, \"snapshot_restores\": {snapshot_restores}}},\n",
                "  \"errors\": {{\"editing\": {editing_errors}, \"accessibility\": {accessibility_errors}, \"host_command\": {host_errors}}},\n",
                "  \"final_widget\": {{\"enabled\": {enabled}, \"read_only\": {read_only}, \"focused\": {focused}, \"window_focused\": {window_focused}, \"widget_focus_retained\": {widget_focus_retained}, \"composition_active\": {composition_active}}},\n",
                "  \"final_document\": {{\"paragraphs\": {paragraphs}, \"text_utf8_bytes_excluding_paragraph_breaks\": {text_bytes}, \"revision\": {revision}, \"tlfr_bytes\": {tlfr_bytes}, \"tlfr_round_trip_equal\": {round_trip}, \"html_bytes\": {html_bytes}, \"undo_steps\": {undo}, \"redo_steps\": {redo}}},\n",
                "  \"final_selection\": {{\"anchor\": {{\"paragraph\": {anchor_paragraph}, \"byte\": {anchor_byte}}}, \"focus\": {{\"paragraph\": {focus_paragraph}, \"byte\": {focus_byte}}}}},\n",
                "  \"final_typing_style\": {{\"bold\": {bold}, \"italic\": {italic}, \"underline\": {underline}, \"strikethrough\": {strike}, \"code\": {code}, \"explicit_foreground\": {foreground}}}\n",
                "}}\n",
            ),
            os = json_string(std::env::consts::OS),
            arch = json_string(std::env::consts::ARCH),
            source_revision = source_revision.map_or_else(|| "null".into(), json_string),
            started = self.started_unix_seconds,
            ended = unix_seconds(),
            smoke = self.smoke_test,
            frames = self.frames,
            regular_font = self.regular_font_supplied,
            bold_font = self.bold_font_supplied,
            scale = json_number(self.pixels_per_point),
            minimum_scale = json_number(self.minimum_pixels_per_point),
            maximum_scale = json_number(self.maximum_pixels_per_point),
            native_scale = json_number(self.native_pixels_per_point),
            text = self.input.text,
            key = self.input.key_press,
            paste = self.input.paste,
            copy = self.input.copy,
            cut = self.input.cut,
            preedit = self.input.ime_preedit,
            commit = self.input.ime_commit,
            delete_surrounding = self.input.ime_delete_surrounding,
            accessibility_action = self.input.accessibility_action,
            window_focus_gained = self.input.window_focus_gained,
            window_focus_lost = self.input.window_focus_lost,
            revision_changes = self.revision_changes,
            selection_changes = self.selection_changes,
            typing_changes = self.typing_style_changes,
            composition_changes = self.composition_changes,
            focus_gains = self.focus_gains,
            focus_losses = self.focus_losses,
            enabled_changes = self.enabled_changes,
            read_only_changes = self.read_only_changes,
            snapshot_restores = self.snapshot_restores,
            editing_errors = self.editing_errors,
            accessibility_errors = self.accessibility_errors,
            host_errors = self.host_command_errors,
            enabled = self.enabled,
            read_only = self.read_only,
            focused = self.focused,
            window_focused = self.focus_state.window_focused,
            widget_focus_retained = self.focus_state.widget_focus_retained,
            composition_active = editor.composition().is_some(),
            paragraphs = document.paragraphs().len(),
            text_bytes = text_bytes,
            revision = document.revision(),
            tlfr_bytes = snapshot.len(),
            round_trip = round_trip,
            html_bytes = document.to_html().len(),
            undo = editor.undo_len(),
            redo = editor.redo_len(),
            anchor_paragraph = selection.anchor.paragraph,
            anchor_byte = selection.anchor.byte,
            focus_paragraph = selection.focus.paragraph,
            focus_byte = selection.focus.byte,
            bold = style.bold,
            italic = style.italic,
            underline = style.underline,
            strike = style.strikethrough,
            code = style.code,
            foreground = style.foreground.is_some(),
        )
    }

    pub fn write_report(&self, editor: &Editor) -> io::Result<&Path> {
        let report = self.report(editor, option_env!("TEXTLOOM_QA_REVISION"));
        // Preserve earlier runs, including when two hosts try to use the same path.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)?;
        file.write_all(report.as_bytes())?;
        file.sync_all()?;
        Ok(&self.path)
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn json_number(value: Option<f32>) -> String {
    value
        .filter(|value| value.is_finite())
        .map_or_else(|| "null".into(), |value| value.to_string())
}

fn json_string(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                write!(output, "\\u{:04x}", character as u32).unwrap();
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

pub fn read_font(path: Option<&PathBuf>) -> Result<Option<Vec<u8>>, eframe::Error> {
    path.map(std::fs::read)
        .transpose()
        .map_err(|error| eframe::Error::AppCreation(error.into()))
}

pub fn arguments() -> impl Iterator<Item = OsString> {
    std::env::args_os().skip(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(arguments: &[&str]) -> Result<Options, String> {
        Options::parse(arguments.iter().map(OsString::from))
    }

    #[test]
    fn options_distinguish_disabled_read_only_and_report() {
        let parsed = options(&[
            "--read-only",
            "--disabled",
            "--qa-report",
            "qa.json",
            "--smoke-test",
        ])
        .unwrap();
        assert!(parsed.read_only && parsed.disabled && parsed.smoke_test);
        assert_eq!(parsed.qa_report, Some(PathBuf::from("qa.json")));
        assert_eq!(options(&[]).unwrap(), Options::default());
        assert!(options(&["--help"]).unwrap().help);
    }

    #[test]
    fn options_reject_missing_duplicate_and_unknown_arguments() {
        for arguments in [
            &["--qa-report"][..],
            &["--font", "--read-only"],
            &["--bold-font", ""],
            &["--qa-report", "one", "--qa-report", "two"],
            &["--unknown"],
        ] {
            assert!(options(arguments).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn report_escapes_metadata_and_omits_document_and_preedit_text() {
        let mut editor = Editor::from_text("PRIVATE_DOCUMENT_秘密");
        editor.update_composition("PRIVATE_PREEDIT", None).unwrap();
        let session =
            Session::new(&options(&["--qa-report", "unused.json"]).unwrap(), &editor).unwrap();
        let report = session.report(&editor, Some("rev\"\n\\\u{0}"));
        assert!(report.contains("\"source_revision\": \"rev\\\"\\n\\\\\\u0000\""));
        assert!(report.contains("\"tlfr_round_trip_equal\": true"));
        assert!(report.contains("\"composition_active\": true"));
        assert!(report.contains("\"manual_signoff\": \"not_recorded\""));
        assert!(!report.contains("PRIVATE"));
        assert!(!report.contains("秘密"));
        assert_eq!(json_number(Some(f32::NAN)), "null");
    }

    #[test]
    fn counters_observe_host_events_and_editor_transitions_without_text() {
        let mut editor = Editor::from_text("ab");
        let mut session =
            Session::new(&options(&["--qa-report", "unused.json"]).unwrap(), &editor).unwrap();
        session.input.observe(&[
            egui::Event::Text("secret".into()),
            egui::Event::Paste("secret".into()),
            egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "secret".into(),
                active_range_chars: Some(0..6),
            }),
            egui::Event::Ime(egui::ImeEvent::Commit("secret".into())),
        ]);
        editor.insert_text("c").unwrap();
        editor.begin_composition();
        let focus = FocusState {
            window_focused: true,
            widget_focus_retained: true,
        };
        session.observe_editor(&editor, focus, true, false);
        session.observe_editor(&editor, focus, true, false);
        editor.cancel_composition();
        session.observe_editor(&editor, FocusState::default(), false, true);
        assert_eq!(session.input.text, 1);
        assert_eq!(session.input.paste, 1);
        assert_eq!(session.input.ime_preedit, 1);
        assert_eq!(session.input.ime_commit, 1);
        assert_eq!(session.revision_changes, 1);
        assert_eq!(session.selection_changes, 1);
        assert_eq!(session.composition_changes, 2);
        assert_eq!((session.focus_gains, session.focus_losses), (1, 1));
        assert_eq!((session.enabled_changes, session.read_only_changes), (1, 1));
    }

    #[test]
    fn focus_diagnostics_distinguish_native_blur_from_widget_focus_loss() {
        let editor = Editor::from_text("secret");
        let mut session =
            Session::new(&options(&["--qa-report", "unused.json"]).unwrap(), &editor).unwrap();
        session.input.observe(&[
            egui::Event::WindowFocused(true),
            egui::Event::WindowFocused(false),
        ]);
        session.observe_editor(
            &editor,
            FocusState {
                window_focused: false,
                widget_focus_retained: true,
            },
            true,
            false,
        );
        let report = session.report(&editor, None);
        assert!(report.contains("\"window_focused\": false"));
        assert!(report.contains("\"widget_focus_retained\": true"));
        assert!(!session.focused);
        assert_eq!(
            (
                session.input.window_focus_gained,
                session.input.window_focus_lost
            ),
            (1, 1)
        );
        session.observe_editor(
            &editor,
            FocusState {
                window_focused: true,
                widget_focus_retained: false,
            },
            true,
            false,
        );
        assert!(!session.focused);
    }

    #[test]
    fn report_never_overwrites_existing_evidence() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("textloom-qa-{}-{unique}.json", std::process::id()));
        let editor = Editor::from_text("secret");
        let session = Session::new(
            &Options {
                qa_report: Some(path.clone()),
                ..Options::default()
            },
            &editor,
        )
        .unwrap();
        assert_eq!(session.write_report(&editor).unwrap(), path);
        let initial = std::fs::read(&path).unwrap();
        assert_eq!(
            session.write_report(&editor).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), initial);
        std::fs::remove_file(path).unwrap();
    }
}
