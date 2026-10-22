//! Command-line options and the content-free QA report of the native host.
//!
//! [`Options::parse`] reads the host's flags. With `--qa-report PATH`, a
//! [`Session`] counts raw host input events and observed editor state
//! transitions while the window is open, and [`Session::write_report`] writes
//! them as JSON on graceful exit. The report records sizes, counts, and flags
//! only: document, clipboard, search, and IME preedit text never appear in it.
//! `scripts/qa-tools` validates the report, so its field names and layout are
//! a format other tools parse.

use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui;
use textloom::{Composition, DecodeLimits, Document, Editor, InlineStyle, Selection};

/// The `--help` text, printed verbatim to stdout.
pub const HELP: &str = "TextLoom native example\n\nUsage: textloom-native-example [--smoke-test] [--read-only] [--disabled] [--rich-clipboard] [--clipboard-self-test] [--font PATH] [--bold-font PATH] [--theme light|dark] [--qa-report PATH]\n\n--smoke-test requests a graceful close after 20 native frames.\n--read-only permits selection and copy while preventing edits.\n--disabled disables document focus and interaction.\n--rich-clipboard enables the example's native rich clipboard transport.\n--clipboard-self-test runs a deterministic rich clipboard fixture check.\n                      Requires --rich-clipboard and writes the system clipboard;\n                      use only in an isolated test session.\n--font adds a proportional font, such as a CJK fallback.\n--bold-font supplies the widget's Bold font family.\n--theme overrides the system theme; Linux reports none, so egui uses dark.\n--qa-report writes content-free JSON observations on graceful exit.\n            PATH is created at startup and must not already exist;\n            reports do not certify manual checks.\n\nSet TEXTLOOM_QA_REVISION when compiling to identify the source revision.";

/// The host's command-line flags; see [`HELP`] for what each one does.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `--smoke-test`: close gracefully after 20 rendered frames.
    pub smoke_test: bool,
    /// `--read-only`: start with editing disabled but selection and copy
    /// allowed.
    pub read_only: bool,
    /// `--disabled`: start with the document widget disabled entirely.
    pub disabled: bool,
    /// `--rich-clipboard`: use the native rich clipboard transport.
    pub rich_clipboard: bool,
    /// `--clipboard-self-test`: run the rich clipboard fixture check, which
    /// writes the system clipboard. Only valid with `--rich-clipboard`.
    pub clipboard_self_test: bool,
    /// `--font PATH`: a font file added first to the proportional family.
    pub regular_font: Option<PathBuf>,
    /// `--bold-font PATH`: a font file registered as the widget's `Bold`
    /// family.
    pub bold_font: Option<PathBuf>,
    /// `--qa-report PATH`: where to write the QA report. The file must not
    /// exist yet.
    pub qa_report: Option<PathBuf>,
    /// `--theme light|dark`: overrides the system theme.
    pub theme: Option<egui::Theme>,
    /// `--help` or `-h`: print [`HELP`] and exit.
    pub help: bool,
}

impl Options {
    /// Parses the host's arguments, excluding the program name.
    ///
    /// # Errors
    ///
    /// Returns a message for stderr when an argument is unknown, given twice,
    /// or missing its value (a path value may not be empty or start with
    /// `--`), when `--clipboard-self-test` lacks `--rich-clipboard`, or when
    /// `--rich-clipboard` is requested on a platform without a backend.
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
            } else if argument == "--rich-clipboard" {
                options.rich_clipboard = true;
            } else if argument == "--clipboard-self-test" {
                options.clipboard_self_test = true;
            } else if argument == "--help" || argument == "-h" {
                options.help = true;
            } else if argument == "--theme" {
                if options.theme.is_some() {
                    return Err("--theme was supplied twice".to_owned());
                }
                options.theme = match arguments.next().as_ref().and_then(|theme| theme.to_str()) {
                    Some("light") => Some(egui::Theme::Light),
                    Some("dark") => Some(egui::Theme::Dark),
                    _ => return Err("--theme requires light or dark".to_owned()),
                };
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
        if options.clipboard_self_test && !options.rich_clipboard {
            return Err("--clipboard-self-test requires --rich-clipboard".to_owned());
        }
        if options.rich_clipboard && !crate::clipboard::SUPPORTED {
            return Err("--rich-clipboard is not supported on this platform".to_owned());
        }
        Ok(options)
    }
}

/// Counts of raw host input events by kind, reported as `host_input_events`.
///
/// These count events as they arrive, including those for the toolbar
/// fields; they do not show which inputs the document accepted.
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
    /// Counts `events`, the raw events of one input pass.
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

/// Native window focus and document widget focus, observed separately.
///
/// The report counts the document as focused only when both hold.
#[derive(Clone, Copy, Default)]
pub struct FocusState {
    /// Whether the native window has keyboard focus.
    pub window_focused: bool,
    /// Whether egui's keyboard focus is still on the document widget.
    pub widget_focus_retained: bool,
}

/// The QA report's destination, claimed when the host starts.
pub struct ReportFile {
    path: PathBuf,
    /// The open, still empty report file; `None` once the report is written.
    file: Option<File>,
}

impl ReportFile {
    /// Creates the empty report file at `path`, before the window opens, so an
    /// unusable path fails at startup instead of discarding a session.
    ///
    /// # Errors
    ///
    /// Returns the I/O error from creating the file, including
    /// [`io::ErrorKind::AlreadyExists`] when `path` already exists: the report
    /// never overwrites earlier evidence.
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self {
            path: path.to_owned(),
            file: Some(file),
        })
    }
}

/// QA observations of one host session, written as the report on exit.
///
/// Each `*_changes` counter counts observed transitions between UI passes, so
/// several changes within one pass count once.
pub struct Session {
    report_file: ReportFile,
    started_unix_seconds: u64,
    smoke_test: bool,
    rich_clipboard_requested: bool,
    clipboard_self_test_requested: bool,
    clipboard_self_test_passed: bool,
    regular_font_supplied: bool,
    bold_font_supplied: bool,
    frames: u64,
    input: InputCounts,
    // The editor state last observed by `observe_editor`, compared with the
    // next observation to count changes.
    revision: u64,
    selection: Selection,
    typing_style: InlineStyle,
    composition: Option<Composition>,
    /// Whether the window was focused and the document kept widget focus.
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
    clipboard_self_test_errors: u64,
    snapshot_restores: u64,
    /// egui's pixels per point in the latest frame; `None` before the first
    /// frame.
    pixels_per_point: Option<f32>,
    minimum_pixels_per_point: Option<f32>,
    maximum_pixels_per_point: Option<f32>,
    native_pixels_per_point: Option<f32>,
}

impl Session {
    /// Starts observing a session whose report goes to `report_file`, taking
    /// `editor`'s current state as the baseline for change counts.
    pub fn new(options: &Options, report_file: ReportFile, editor: &Editor) -> Self {
        Self {
            report_file,
            started_unix_seconds: unix_seconds(),
            smoke_test: options.smoke_test,
            rich_clipboard_requested: options.rich_clipboard,
            clipboard_self_test_requested: options.clipboard_self_test,
            clipboard_self_test_passed: false,
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
            clipboard_self_test_errors: 0,
            snapshot_restores: 0,
            pixels_per_point: None,
            minimum_pixels_per_point: None,
            maximum_pixels_per_point: None,
            native_pixels_per_point: None,
        }
    }

    /// Counts a rendered frame and records its display scale. Call it once per
    /// frame, not once per input pass.
    pub fn begin_frame(&mut self, context: &egui::Context) {
        self.frames += 1;
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

    /// Counts the raw input events of the current pass. With input routing,
    /// egui may run several passes per frame, each with its own events.
    pub fn observe_input_pass(&mut self, context: &egui::Context) {
        context.input(|input| self.input.observe(&input.raw.events));
    }

    /// Compares the editor and widget state after a UI pass with the previous
    /// observation and counts each change.
    pub fn observe_editor(
        &mut self,
        editor: &Editor,
        focus_state: FocusState,
        enabled: bool,
        read_only: bool,
    ) {
        observe_change(
            &mut self.revision,
            &mut self.revision_changes,
            editor.document().revision(),
        );
        observe_change(
            &mut self.selection,
            &mut self.selection_changes,
            editor.selection(),
        );
        observe_change(
            &mut self.typing_style,
            &mut self.typing_style_changes,
            editor.typing_style(),
        );
        // Compare by reference to avoid cloning the composition every pass.
        let composition = editor.composition();
        if self.composition.as_ref() != composition {
            self.composition_changes += 1;
            self.composition = composition.cloned();
        }
        observe_change(&mut self.enabled, &mut self.enabled_changes, enabled);
        observe_change(&mut self.read_only, &mut self.read_only_changes, read_only);
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

    /// Adds the editing and accessibility errors the widget reported in one
    /// pass.
    pub fn widget_errors(&mut self, editing: usize, accessibility: usize) {
        self.editing_errors += editing as u64;
        self.accessibility_errors += accessibility as u64;
    }

    /// Counts a toolbar, link, or search command that failed.
    pub fn host_command_error(&mut self) {
        self.host_command_errors += 1;
    }

    /// Records the clipboard fixture check's outcome. A failure is sticky: a
    /// later passing result cannot clear it. Ignored unless the check was
    /// requested.
    pub fn clipboard_self_test_result(&mut self, passed: bool) {
        if !self.clipboard_self_test_requested {
            return;
        }
        if !passed {
            self.clipboard_self_test_errors = 1;
        }
        self.clipboard_self_test_passed = passed && self.clipboard_self_test_errors == 0;
    }

    /// Counts a restored in-memory snapshot.
    pub fn snapshot_restored(&mut self) {
        self.snapshot_restores += 1;
    }

    /// Renders the JSON report for the session so far, ending with `editor`'s
    /// final state. `source_revision` is `TEXTLOOM_QA_REVISION` at build time.
    fn report(&self, editor: &Editor, source_revision: Option<&str>) -> String {
        let document = editor.document();
        let snapshot = document.to_bytes();
        let round_trip = Document::from_bytes_with_limits(&snapshot, DecodeLimits::UNLIMITED)
            .is_ok_and(|restored| restored.to_bytes() == snapshot);
        let text_bytes: usize = document
            .paragraphs()
            .iter()
            .map(|paragraph| paragraph.text().len())
            .sum();
        let selection = editor.selection();
        let style = editor.typing_style();
        format!(
            concat!(
                "{{\n",
                "  \"schema_version\": 1,\n",
                "  \"manual_signoff\": \"not_recorded\",\n",
                "  \"document_content_included\": false,\n",
                "  \"host\": {{\"name\": \"textloom-native-example\", \"egui_api_version\": \"0.37\", \"eframe_api_version\": \"0.37\", \"os\": {os}, \"architecture\": {arch}, \"source_revision\": {source_revision}}},\n",
                "  \"session\": {{\"started_unix_seconds\": {started}, \"ended_unix_seconds\": {ended}, \"graceful_exit\": true, \"smoke_test_requested\": {smoke}, \"rich_clipboard_requested\": {rich_clipboard}, \"clipboard_self_test_requested\": {clipboard_test}, \"clipboard_self_test_passed\": {clipboard_passed}, \"rendered_frames\": {frames}, \"regular_font_supplied\": {regular_font}, \"bold_font_supplied\": {bold_font}}},\n",
                "  \"display\": {{\"pixels_per_point\": {scale}, \"minimum_pixels_per_point\": {minimum_scale}, \"maximum_pixels_per_point\": {maximum_scale}, \"native_pixels_per_point\": {native_scale}}},\n",
                "  \"host_input_events\": {{\"text\": {text}, \"key_press\": {key}, \"paste\": {paste}, \"copy\": {copy}, \"cut\": {cut}, \"ime_preedit\": {preedit}, \"ime_commit\": {commit}, \"ime_delete_surrounding\": {delete_surrounding}, \"accessibility_action\": {accessibility_action}, \"window_focus_gained\": {window_focus_gained}, \"window_focus_lost\": {window_focus_lost}}},\n",
                "  \"observed_state_changes\": {{\"document_revision\": {revision_changes}, \"selection\": {selection_changes}, \"typing_style\": {typing_changes}, \"composition\": {composition_changes}, \"focus_gains\": {focus_gains}, \"focus_losses\": {focus_losses}, \"enabled\": {enabled_changes}, \"read_only\": {read_only_changes}, \"snapshot_restores\": {snapshot_restores}}},\n",
                "  \"errors\": {{\"editing\": {editing_errors}, \"accessibility\": {accessibility_errors}, \"host_command\": {host_errors}, \"clipboard_self_test\": {clipboard_errors}}},\n",
                "  \"final_widget\": {{\"enabled\": {enabled}, \"read_only\": {read_only}, \"focused\": {focused}, \"window_focused\": {window_focused}, \"widget_focus_retained\": {widget_focus_retained}, \"composition_active\": {composition_active}}},\n",
                "  \"final_document\": {{\"paragraphs\": {paragraphs}, \"text_utf8_bytes_excluding_paragraph_breaks\": {text_bytes}, \"revision\": {revision}, \"tlfr_bytes\": {tlfr_bytes}, \"tlfr_round_trip_equal\": {round_trip}, \"html_bytes\": {html_bytes}, \"undo_steps\": {undo}, \"redo_steps\": {redo}}},\n",
                "  \"final_selection\": {{\"anchor\": {{\"paragraph\": {anchor_paragraph}, \"byte\": {anchor_byte}}}, \"focus\": {{\"paragraph\": {focus_paragraph}, \"byte\": {focus_byte}}}}},\n",
                "  \"final_typing_style\": {{\"bold\": {bold}, \"italic\": {italic}, \"underline\": {underline}, \"strikethrough\": {strike}, \"code\": {code}, \"explicit_foreground\": {foreground}}}\n",
                "}}\n",
            ),
            os = json_string(env::consts::OS),
            arch = json_string(env::consts::ARCH),
            source_revision = source_revision.map_or_else(|| "null".into(), json_string),
            started = self.started_unix_seconds,
            ended = unix_seconds(),
            smoke = self.smoke_test,
            rich_clipboard = self.rich_clipboard_requested,
            clipboard_test = self.clipboard_self_test_requested,
            clipboard_passed = self.clipboard_self_test_passed,
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
            clipboard_errors = self.clipboard_self_test_errors,
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

    /// Writes the report into the file claimed at startup, syncs it, and
    /// returns its path.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::AlreadyExists`] if the report was already
    /// written, or the I/O error from writing or syncing the file.
    pub fn write_report(&mut self, editor: &Editor) -> io::Result<&Path> {
        let mut file = self.report_file.file.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::AlreadyExists, "QA report already written")
        })?;
        let report = self.report(editor, option_env!("TEXTLOOM_QA_REVISION"));
        file.write_all(report.as_bytes())?;
        file.sync_all()?;
        Ok(&self.report_file.path)
    }

    #[cfg(test)]
    pub fn host_command_errors(&self) -> u64 {
        self.host_command_errors
    }
}

/// Stores `current` in `previous` and counts a change in `changes` when the
/// two differ.
fn observe_change<T: PartialEq>(previous: &mut T, changes: &mut u64, current: T) {
    if *previous != current {
        *changes += 1;
        *previous = current;
    }
}

/// The current time in whole seconds since the Unix epoch, or 0 if the clock
/// is set before it.
fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Formats `value` as a JSON number, or `null` when it is absent or not
/// finite.
fn json_number(value: Option<f32>) -> String {
    value
        .filter(|value| value.is_finite())
        .map_or_else(|| "null".into(), |value| value.to_string())
}

/// Formats `value` as a quoted JSON string, escaping quotes, backslashes, and
/// the control characters U+0000 to U+001F.
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
                write!(output, "\\u{:04x}", character as u32)
                    .expect("formatting into a String cannot fail");
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

/// Reads the font file at `path`, if one was supplied.
///
/// # Errors
///
/// Returns the read failure as an [`eframe::Error::AppCreation`], so `main`
/// can return it before opening a window.
pub fn read_font(path: Option<&Path>) -> Result<Option<Vec<u8>>, eframe::Error> {
    path.map(fs::read)
        .transpose()
        .map_err(|error| eframe::Error::AppCreation(error.into()))
}

/// The host's arguments, without the program name.
pub fn arguments() -> impl Iterator<Item = OsString> {
    env::args_os().skip(1)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::{mem, process};

    use super::*;

    fn options(arguments: &[&str]) -> Result<Options, String> {
        Options::parse(arguments.iter().map(OsString::from))
    }

    fn unwritten_session(arguments: &[&str], editor: &Editor) -> Session {
        let report_file = ReportFile {
            path: "unused.json".into(),
            file: None,
        };
        Session::new(&options(arguments).unwrap(), report_file, editor)
    }

    fn temporary_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("textloom-{name}-{}-{unique}", process::id()))
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
        assert_eq!(
            options(&["--theme", "light"]).unwrap().theme,
            Some(egui::Theme::Light)
        );
        assert_eq!(
            options(&["--theme", "dark"]).unwrap().theme,
            Some(egui::Theme::Dark)
        );
    }

    #[test]
    fn options_reject_missing_duplicate_and_unknown_arguments() {
        for arguments in [
            &["--qa-report"][..],
            &["--font", "--read-only"],
            &["--bold-font", ""],
            &["--qa-report", "one", "--qa-report", "two"],
            &["--theme"],
            &["--theme", "blue"],
            &["--theme", "light", "--theme", "dark"],
            &["--unknown"],
        ] {
            assert!(options(arguments).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn clipboard_fixture_requires_explicit_transport_opt_in_in_either_order() {
        assert_eq!(
            options(&["--clipboard-self-test"]).unwrap_err(),
            "--clipboard-self-test requires --rich-clipboard"
        );
        for arguments in [
            ["--rich-clipboard", "--clipboard-self-test"],
            ["--clipboard-self-test", "--rich-clipboard"],
        ] {
            let parsed = options(&arguments).unwrap();
            assert!(
                parsed.rich_clipboard && parsed.clipboard_self_test,
                "{arguments:?}"
            );
        }
        let parsed = options(&["--rich-clipboard"]).unwrap();
        assert!(parsed.rich_clipboard && !parsed.clipboard_self_test);
        assert!(HELP.contains("writes the system clipboard"));
        assert!(HELP.contains("isolated test session"));
    }

    #[test]
    fn clipboard_fixture_report_distinguishes_unrun_passed_and_sticky_failure() {
        let editor = Editor::from_text("PRIVATE_FIXTURE_TEXT");
        let mut session = unwritten_session(
            &[
                "--qa-report",
                "unused.json",
                "--rich-clipboard",
                "--clipboard-self-test",
            ],
            &editor,
        );
        let report = session.report(&editor, None);
        assert!(report.contains("\"rich_clipboard_requested\": true"));
        assert!(report.contains("\"clipboard_self_test_requested\": true"));
        assert!(report.contains("\"clipboard_self_test_passed\": false"));
        assert!(report.contains("\"clipboard_self_test\": 0"));
        session.clipboard_self_test_result(true);
        session.clipboard_self_test_result(true);
        let report = session.report(&editor, None);
        assert!(report.contains("\"clipboard_self_test_passed\": true"));
        assert!(report.contains("\"clipboard_self_test\": 0"));
        session.clipboard_self_test_result(false);
        session.clipboard_self_test_result(false);
        session.clipboard_self_test_result(true);
        let report = session.report(&editor, None);
        assert!(report.contains("\"clipboard_self_test_passed\": false"));
        assert!(report.contains("\"clipboard_self_test\": 1"));
        assert!(report.contains("\"manual_signoff\": \"not_recorded\""));
        assert!(!report.contains("PRIVATE"));

        let mut unrequested = unwritten_session(&["--qa-report", "unused.json"], &editor);
        unrequested.clipboard_self_test_result(true);
        let report = unrequested.report(&editor, None);
        assert!(report.contains("\"rich_clipboard_requested\": false"));
        assert!(report.contains("\"clipboard_self_test_requested\": false"));
        assert!(report.contains("\"clipboard_self_test_passed\": false"));
        assert!(report.contains("\"clipboard_self_test\": 0"));
    }

    #[test]
    fn report_escapes_metadata_and_omits_document_and_preedit_text() {
        let mut editor = Editor::from_text("PRIVATE_DOCUMENT_秘密");
        editor.update_composition("PRIVATE_PREEDIT", None).unwrap();
        let session = unwritten_session(&["--qa-report", "unused.json"], &editor);
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
    fn report_round_trips_documents_over_the_default_decode_limits() {
        let editor = Editor::from_text(&"\n".repeat(1_000_000));
        let session = unwritten_session(&["--qa-report", "unused.json"], &editor);
        let report = session.report(&editor, None);
        assert!(report.contains("\"paragraphs\": 1000001"));
        assert!(report.contains("\"tlfr_round_trip_equal\": true"));
    }

    #[test]
    fn counters_observe_host_events_and_editor_transitions_without_text() {
        let mut editor = Editor::from_text("ab");
        let mut session = unwritten_session(&["--qa-report", "unused.json"], &editor);
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
        editor.update_composition("にほん", Some(0..3)).unwrap();
        session.observe_editor(&editor, focus, true, false);
        session.observe_editor(&editor, focus, true, false);
        assert_eq!(session.composition_changes, 2);
        editor.update_composition("にほん", Some(3..6)).unwrap();
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
        assert_eq!(session.composition_changes, 4);
        assert_eq!((session.focus_gains, session.focus_losses), (1, 1));
        assert_eq!((session.enabled_changes, session.read_only_changes), (1, 1));
    }

    #[test]
    fn routed_input_passes_are_counted_once_without_inflating_frame_counts() {
        for max_passes in [1, 2, 8] {
            let editor = Editor::default();
            let mut session = unwritten_session(&["--qa-report", "unused.json"], &editor);
            let context = egui::Context::default();
            textloom::adapter::egui::install_input_routing(&context);
            context.enable_accesskit();
            context.options_mut(|options| {
                options.max_passes = NonZeroUsize::new(max_passes).unwrap();
            });
            let first_id = egui::Id::unique("qa-first-field");
            let second_id = egui::Id::unique("qa-second-field");
            context.memory_mut(|memory| memory.request_focus(first_id));
            let focus = |id: egui::Id| {
                egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Focus,
                    target_node: id.accesskit_id(),
                    target_tree: egui::accesskit::TreeId::ROOT,
                    data: None,
                })
            };
            let mut events = vec![
                egui::Event::Text("before".into()),
                focus(second_id),
                egui::Event::Paste("after".into()),
                focus(first_id),
                egui::Event::Text("!".into()),
                egui::Event::WindowFocused(true),
            ];
            let mut last_frame = None;
            let (mut first, mut second) = (String::new(), String::new());
            for _ in 0..6 {
                let output = context.run_ui(
                    egui::RawInput {
                        focused: true,
                        events: mem::take(&mut events),
                        ..Default::default()
                    },
                    |ui| {
                        let frame = ui.ctx().cumulative_frame_nr();
                        if last_frame != Some(frame) {
                            last_frame = Some(frame);
                            session.begin_frame(ui.ctx());
                        }
                        session.observe_input_pass(ui.ctx());
                        ui.add(egui::TextEdit::singleline(&mut first).id(first_id));
                        ui.add(egui::TextEdit::singleline(&mut second).id(second_id));
                    },
                );
                output.drop_without_applying_deltas();
            }
            assert_eq!(session.frames, 6, "max_passes={max_passes}");
            assert_eq!(session.input.text, 2, "max_passes={max_passes}");
            assert_eq!(session.input.paste, 1, "max_passes={max_passes}");
            assert_eq!(
                session.input.accessibility_action, 2,
                "max_passes={max_passes}"
            );
            assert_eq!(
                session.input.window_focus_gained, 1,
                "max_passes={max_passes}"
            );
            assert_eq!(first, "before!", "max_passes={max_passes}");
            assert_eq!(second, "after", "max_passes={max_passes}");
        }
    }

    #[test]
    fn focus_diagnostics_distinguish_native_blur_from_widget_focus_loss() {
        let editor = Editor::from_text("secret");
        let mut session = unwritten_session(&["--qa-report", "unused.json"], &editor);
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
    fn report_path_is_claimed_at_startup_and_never_overwrites_evidence() {
        let path = temporary_path("qa.json");
        let editor = Editor::from_text("secret");
        let options = Options {
            qa_report: Some(path.clone()),
            ..Options::default()
        };
        let mut session = Session::new(&options, ReportFile::create(&path).unwrap(), &editor);
        assert_eq!(
            ReportFile::create(&path).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&path).unwrap(), b"");
        assert_eq!(session.write_report(&editor).unwrap(), path);
        let initial = fs::read(&path).unwrap();
        assert!(initial.starts_with(b"{"));
        assert_eq!(
            session.write_report(&editor).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&path).unwrap(), initial);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn unusable_report_paths_fail_at_startup() {
        let existing = temporary_path("existing.json");
        fs::write(&existing, b"{}").unwrap();
        assert_eq!(
            ReportFile::create(&existing).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&existing).unwrap(), b"{}");
        fs::remove_file(existing).unwrap();
        let missing_parent = temporary_path("missing").join("report.json");
        assert_eq!(
            ReportFile::create(&missing_parent).err().unwrap().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn rich_clipboard_is_rejected_where_the_host_has_no_backend() {
        assert_eq!(
            options(&["--rich-clipboard"]).is_ok(),
            cfg!(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "windows"
            ))
        );
    }
}
