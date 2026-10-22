//! Validates a native host QA report: strict, content-free observations from
//! one renderer or interaction session. A valid report never certifies manual
//! QA.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};

use crate::cli::Arguments;

pub const USAGE: &str = "\
usage: check-native-report REPORT --revision REVISION --os {linux,macos,windows}
                           --mode {editable,read-only,disabled}
                           [--clipboard {rich,default}] [--interaction]

Validate native renderer/interaction observations without certifying manual QA.

  --clipboard {rich,default}  expected clipboard configuration (default: rich)
  --interaction               check non-smoke report integrity; the interaction
                              driver asserts behavior";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    pub const ALL: [Self; 3] = [Self::Linux, Self::Macos, Self::Windows];

    pub fn name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Editable,
    ReadOnly,
    Disabled,
}

impl Mode {
    pub const ALL: [Self; 3] = [Self::Editable, Self::ReadOnly, Self::Disabled];

    pub fn name(self) -> &'static str {
        match self {
            Self::Editable => "editable",
            Self::ReadOnly => "read-only",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Clipboard {
    #[default]
    Rich,
    Default,
}

impl Clipboard {
    pub const ALL: [Self; 2] = [Self::Rich, Self::Default];

    pub fn name(self) -> &'static str {
        match self {
            Self::Rich => "rich",
            Self::Default => "default",
        }
    }
}

/// What a report must show for the run being checked.
#[derive(Clone, Debug)]
pub struct Expected {
    pub revision: String,
    pub os: Os,
    pub mode: Mode,
    pub interaction: bool,
    pub clipboard: Clipboard,
}

impl Expected {
    pub fn summary(&self) -> String {
        let kind = if self.interaction {
            "interaction"
        } else {
            "rendering"
        };
        format!(
            "Validated {} {} {kind} observations; manual QA remains separate",
            self.os.name(),
            self.mode.name()
        )
    }
}

// Every object rejects unknown fields, and derived structs reject duplicates,
// so a report can carry only these content-free observations.

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema_version: u64,
    pub manual_signoff: String,
    pub document_content_included: bool,
    pub host: Host,
    pub session: Session,
    pub display: Display,
    pub host_input_events: InputEvents,
    pub observed_state_changes: StateChanges,
    pub errors: Errors,
    pub final_widget: Widget,
    pub final_document: Document,
    pub final_selection: Selection,
    pub final_typing_style: TypingStyle,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub name: String,
    pub egui_api_version: String,
    pub eframe_api_version: String,
    pub os: String,
    pub architecture: String,
    pub source_revision: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub started_unix_seconds: u64,
    pub ended_unix_seconds: u64,
    pub graceful_exit: bool,
    pub smoke_test_requested: bool,
    pub rich_clipboard_requested: bool,
    pub clipboard_self_test_requested: bool,
    pub clipboard_self_test_passed: bool,
    pub rendered_frames: u64,
    pub regular_font_supplied: bool,
    pub bold_font_supplied: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Display {
    pub pixels_per_point: f64,
    pub minimum_pixels_per_point: f64,
    pub maximum_pixels_per_point: f64,
    /// Required, but may be null.
    #[serde(deserialize_with = "nullable")]
    pub native_pixels_per_point: Option<f64>,
}

fn nullable<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
    Option::deserialize(deserializer)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputEvents {
    pub text: u64,
    pub key_press: u64,
    pub paste: u64,
    pub copy: u64,
    pub cut: u64,
    pub ime_preedit: u64,
    pub ime_commit: u64,
    pub ime_delete_surrounding: u64,
    pub accessibility_action: u64,
    pub window_focus_gained: u64,
    pub window_focus_lost: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateChanges {
    pub document_revision: u64,
    pub selection: u64,
    pub typing_style: u64,
    pub composition: u64,
    pub focus_gains: u64,
    pub focus_losses: u64,
    pub enabled: u64,
    pub read_only: u64,
    pub snapshot_restores: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Errors {
    pub editing: u64,
    pub accessibility: u64,
    pub host_command: u64,
    pub clipboard_self_test: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Widget {
    pub enabled: bool,
    pub read_only: bool,
    pub focused: bool,
    pub window_focused: bool,
    pub widget_focus_retained: bool,
    pub composition_active: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub paragraphs: u64,
    pub text_utf8_bytes_excluding_paragraph_breaks: u64,
    pub revision: u64,
    pub tlfr_bytes: u64,
    pub html_bytes: u64,
    pub undo_steps: u64,
    pub redo_steps: u64,
    pub tlfr_round_trip_equal: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub anchor: Position,
    pub focus: Position,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub paragraph: u64,
    pub byte: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypingStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub explicit_foreground: bool,
}

/// Parses a report, rejecting unknown, missing, duplicate, or mistyped fields.
pub fn parse(text: &str) -> Result<Report, String> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let report: Report = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        let path = error.path().to_string();
        let location = if path == "." {
            "report".to_owned()
        } else {
            format!("report.{path}")
        };
        format!("{location}: {}", error.into_inner())
    })?;
    deserializer.end().map_err(|error| error.to_string())?;
    let display = &report.display;
    for (name, value) in [
        ("pixels_per_point", Some(display.pixels_per_point)),
        (
            "minimum_pixels_per_point",
            Some(display.minimum_pixels_per_point),
        ),
        (
            "maximum_pixels_per_point",
            Some(display.maximum_pixels_per_point),
        ),
        ("native_pixels_per_point", display.native_pixels_per_point),
    ] {
        if let Some(value) = value
            && !(value.is_finite() && value >= 0.0)
        {
            return Err(format!(
                "report.display.{name}: expected a finite nonnegative number"
            ));
        }
    }
    Ok(report)
}

fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

/// Checks that a report shows a complete, error-free session of the expected run.
pub fn validate(report: &Report, expected: &Expected) -> Result<(), String> {
    let Expected {
        ref revision,
        os,
        mode,
        interaction,
        clipboard,
    } = *expected;
    require(
        report.schema_version == 1,
        "unsupported report schema version",
    )?;
    require(
        report.manual_signoff == "not_recorded",
        "smoke report claims manual signoff",
    )?;
    require(
        !report.document_content_included,
        "report claims document content",
    )?;
    let host = &report.host;
    require(
        host.name == "textloom-native-example",
        "unexpected native host",
    )?;
    require(
        host.egui_api_version == "0.37" && host.eframe_api_version == "0.37",
        "unexpected host API versions",
    )?;
    require(
        matches!(host.architecture.as_str(), "x86_64" | "aarch64"),
        "unexpected runner architecture",
    )?;
    require(
        host.source_revision == *revision,
        "source revision does not match this run",
    )?;
    require(host.os == os.name(), "host OS does not match this run")?;
    let session = &report.session;
    require(session.graceful_exit, "missing graceful completion")?;
    require(
        session.smoke_test_requested != interaction,
        "unexpected smoke-test request for this validation mode",
    )?;
    require(
        session.rendered_frames >= if interaction { 1 } else { 20 },
        "insufficient native frames",
    )?;
    match clipboard {
        Clipboard::Default => require(
            !(session.rich_clipboard_requested
                || session.clipboard_self_test_requested
                || session.clipboard_self_test_passed),
            "rich clipboard or clipboard self-test reported for the default configuration",
        )?,
        Clipboard::Rich => {
            require(
                session.rich_clipboard_requested,
                "native rich clipboard was not requested",
            )?;
            if interaction {
                require(
                    !session.clipboard_self_test_requested || mode == Mode::Editable,
                    "clipboard self-test requested in a guarded interaction mode",
                )?;
            } else {
                require(
                    session.clipboard_self_test_requested == (mode == Mode::Editable),
                    "unexpected clipboard self-test request for this mode",
                )?;
            }
        }
    }
    require(
        session.clipboard_self_test_passed == session.clipboard_self_test_requested,
        "missing or unexpected clipboard self-test completion",
    )?;
    let errors = &report.errors;
    require(
        [
            errors.editing,
            errors.accessibility,
            errors.host_command,
            errors.clipboard_self_test,
        ]
        .iter()
        .all(|&count| count == 0),
        "native host reported errors",
    )?;
    require(
        report.final_document.tlfr_round_trip_equal,
        "TLFR snapshot roundtrip failed",
    )?;
    let widget = &report.final_widget;
    require(
        !widget.composition_active,
        "composition remains active after smoke test",
    )?;
    require(
        widget.enabled == (mode != Mode::Disabled),
        "unexpected widget enabled state",
    )?;
    require(
        widget.read_only == (mode == Mode::ReadOnly),
        "unexpected widget read-only state",
    )?;
    require(
        widget.focused == (widget.window_focused && widget.widget_focus_retained),
        "inconsistent focus observations",
    )?;
    if mode == Mode::Disabled {
        require(
            !widget.widget_focus_retained && !widget.focused,
            "disabled widget retained focus",
        )?;
        require(
            report.observed_state_changes.focus_gains == 0,
            "disabled widget gained focus",
        )?;
    }
    let display = &report.display;
    require(
        0.0 < display.minimum_pixels_per_point
            && display.minimum_pixels_per_point <= display.pixels_per_point
            && display.pixels_per_point <= display.maximum_pixels_per_point,
        "invalid rendered display scale",
    )
}

/// Reads, parses, and validates a report file.
pub fn check_file(path: &Path, expected: &Expected) -> Result<Report, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let report = parse(&text)?;
    validate(&report, expected)?;
    Ok(report)
}

fn parse_arguments(
    arguments: impl IntoIterator<Item = String>,
) -> Result<Option<(PathBuf, Expected)>, String> {
    let mut arguments = Arguments::new(arguments);
    if arguments.flag("--help") || arguments.flag("-h") {
        return Ok(None);
    }
    let revision = arguments
        .option("--revision")?
        .ok_or("--revision is required")?;
    let os = arguments
        .choice("--os", &Os::ALL, Os::name)?
        .ok_or("--os is required")?;
    let mode = arguments
        .choice("--mode", &Mode::ALL, Mode::name)?
        .ok_or("--mode is required")?;
    let clipboard = arguments
        .choice("--clipboard", &Clipboard::ALL, Clipboard::name)?
        .unwrap_or_default();
    let interaction = arguments.flag("--interaction");
    let [report] = <[String; 1]>::try_from(arguments.finish()?)
        .map_err(|_| "expected exactly one REPORT path".to_owned())?;
    Ok(Some((
        PathBuf::from(report),
        Expected {
            revision,
            os,
            mode,
            interaction,
            clipboard,
        },
    )))
}

/// The `check-native-report` command; returns its exit status.
pub fn main(arguments: impl IntoIterator<Item = String>) -> u8 {
    let (path, expected) = match parse_arguments(arguments) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            println!("{USAGE}");
            return 0;
        }
        Err(message) => {
            eprintln!("{USAGE}\n\ncheck-native-report: error: {message}");
            return 2;
        }
    };
    match check_file(&path, &expected) {
        Ok(_) => {
            println!("{}", expected.summary());
            0
        }
        Err(message) => {
            eprintln!("Native report validation failed: {message}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::TempDir;

    const MODES: [Mode; 3] = Mode::ALL;

    fn specimen(mode: Mode, clipboard: Clipboard) -> Value {
        let rich = clipboard == Clipboard::Rich;
        let self_test = rich && mode == Mode::Editable;
        json!({
            "schema_version": 1,
            "manual_signoff": "not_recorded",
            "document_content_included": false,
            "host": {
                "name": "textloom-native-example", "egui_api_version": "0.37",
                "eframe_api_version": "0.37", "os": "linux", "architecture": "x86_64",
                "source_revision": "candidate",
            },
            "session": {
                "started_unix_seconds": 0, "ended_unix_seconds": 0, "graceful_exit": true,
                "smoke_test_requested": true, "rich_clipboard_requested": rich,
                "clipboard_self_test_requested": self_test, "clipboard_self_test_passed": self_test,
                "rendered_frames": 21, "regular_font_supplied": false, "bold_font_supplied": false,
            },
            "display": {
                "pixels_per_point": 2, "minimum_pixels_per_point": 1,
                "maximum_pixels_per_point": 2, "native_pixels_per_point": null,
            },
            "host_input_events": {
                "text": 0, "key_press": 0, "paste": 0, "copy": 0, "cut": 0, "ime_preedit": 0,
                "ime_commit": 0, "ime_delete_surrounding": 0, "accessibility_action": 0,
                "window_focus_gained": 0, "window_focus_lost": 0,
            },
            "observed_state_changes": {
                "document_revision": 0, "selection": 0, "typing_style": 0, "composition": 0,
                "focus_gains": 0, "focus_losses": 0, "enabled": 0, "read_only": 0,
                "snapshot_restores": 0,
            },
            "errors": {"editing": 0, "accessibility": 0, "host_command": 0, "clipboard_self_test": 0},
            "final_widget": {
                "enabled": mode != Mode::Disabled, "read_only": mode == Mode::ReadOnly,
                "focused": false, "window_focused": false, "widget_focus_retained": false,
                "composition_active": false,
            },
            "final_document": {
                "paragraphs": 0, "text_utf8_bytes_excluding_paragraph_breaks": 0, "revision": 0,
                "tlfr_bytes": 0, "html_bytes": 0, "undo_steps": 0, "redo_steps": 0,
                "tlfr_round_trip_equal": true,
            },
            "final_selection": {
                "anchor": {"paragraph": 0, "byte": 0},
                "focus": {"paragraph": 0, "byte": 0},
            },
            "final_typing_style": {
                "bold": false, "italic": false, "underline": false, "strikethrough": false,
                "code": false, "explicit_foreground": false,
            },
        })
    }

    fn rich(mode: Mode) -> Value {
        specimen(mode, Clipboard::Rich)
    }

    fn validate_text(
        text: &str,
        mode: Mode,
        interaction: bool,
        clipboard: Clipboard,
    ) -> Result<(), String> {
        let expected = Expected {
            revision: "candidate".to_owned(),
            os: Os::Linux,
            mode,
            interaction,
            clipboard,
        };
        validate(&parse(text)?, &expected)
    }

    fn check(
        report: &Value,
        mode: Mode,
        interaction: bool,
        clipboard: Clipboard,
    ) -> Result<(), String> {
        validate_text(&report.to_string(), mode, interaction, clipboard)
    }

    fn other(clipboard: Clipboard) -> Clipboard {
        match clipboard {
            Clipboard::Rich => Clipboard::Default,
            Clipboard::Default => Clipboard::Rich,
        }
    }

    #[test]
    fn modes_with_no_manual_signoff() {
        for mode in MODES {
            check(&rich(mode), mode, false, Clipboard::Rich).unwrap();
        }
    }

    #[test]
    fn explicit_clipboard_configuration_must_match_the_report() {
        for mode in MODES {
            for interaction in [false, true] {
                for clipboard in Clipboard::ALL {
                    let mut report = specimen(mode, clipboard);
                    report["session"]["smoke_test_requested"] = json!(!interaction);
                    let case = format!("{mode:?} interaction={interaction} {clipboard:?}");
                    check(&report, mode, interaction, clipboard).expect(&case);
                    assert!(
                        check(&report, mode, interaction, other(clipboard)).is_err(),
                        "{case}"
                    );
                }
            }
        }
    }

    #[test]
    fn default_clipboard_rejects_every_rich_or_self_test_flag_combination() {
        let fields = [
            "rich_clipboard_requested",
            "clipboard_self_test_requested",
            "clipboard_self_test_passed",
        ];
        for mode in MODES {
            for interaction in [false, true] {
                for flags in 1..8 {
                    let mut report = specimen(mode, Clipboard::Default);
                    for (bit, field) in fields.iter().enumerate() {
                        report["session"][field] = json!(flags & (1 << bit) != 0);
                    }
                    report["session"]["smoke_test_requested"] = json!(!interaction);
                    assert!(
                        check(&report, mode, interaction, Clipboard::Default).is_err(),
                        "{mode:?} interaction={interaction} flags={flags:03b}"
                    );
                }
            }
        }
    }

    fn cli(path: &Path, extra: &[&str]) -> u8 {
        let mut arguments = vec![
            path.display().to_string(),
            "--revision".into(),
            "candidate".into(),
            "--os".into(),
            "linux".into(),
            "--mode".into(),
            "editable".into(),
        ];
        arguments.extend(extra.iter().map(|argument| (*argument).to_owned()));
        main(arguments)
    }

    #[test]
    fn clipboard_cli_choice_preserves_rich_default() {
        let directory = TempDir::new("textloom-report-test-").unwrap();
        let path = directory.path().join("report.json");
        for clipboard in Clipboard::ALL {
            fs::write(&path, specimen(Mode::Editable, clipboard).to_string()).unwrap();
            for option in [None, Some(Clipboard::Rich), Some(Clipboard::Default)] {
                let extra: Vec<&str> = option
                    .iter()
                    .flat_map(|choice| ["--clipboard", choice.name()])
                    .collect();
                let expected = if clipboard == option.unwrap_or_default() {
                    0
                } else {
                    1
                };
                assert_eq!(cli(&path, &extra), expected, "{clipboard:?} {option:?}");
            }
        }
    }

    #[test]
    fn unknown_clipboard_configuration_and_missing_files_are_rejected() {
        let directory = TempDir::new("textloom-report-test-").unwrap();
        let path = directory.path().join("report.json");
        fs::write(&path, rich(Mode::Editable).to_string()).unwrap();
        assert_eq!(cli(&path, &["--clipboard", "unknown"]), 2);
        assert_eq!(cli(&path, &["--unexpected"]), 2);
        assert_eq!(cli(&directory.path().join("missing.json"), &[]), 1);
    }

    #[test]
    fn interaction_reports_do_not_certify_smoke_completion() {
        for mode in MODES {
            let mut report = rich(mode);
            let session = &mut report["session"];
            session["smoke_test_requested"] = json!(false);
            session["rendered_frames"] = json!(5);
            session["clipboard_self_test_requested"] = json!(false);
            session["clipboard_self_test_passed"] = json!(false);
            check(&report, mode, true, Clipboard::Rich).unwrap();
            assert!(check(&report, mode, false, Clipboard::Rich).is_err());
            report["session"]["rendered_frames"] = json!(0);
            assert!(check(&report, mode, true, Clipboard::Rich).is_err());
        }
    }

    #[test]
    fn interaction_fixture_completion_must_match_a_valid_request() {
        for mode in MODES {
            for (requested, passed) in [(false, true), (true, false), (true, true)] {
                if mode == Mode::Editable && requested && passed {
                    continue;
                }
                let mut report = rich(mode);
                report["session"]["smoke_test_requested"] = json!(false);
                report["session"]["clipboard_self_test_requested"] = json!(requested);
                report["session"]["clipboard_self_test_passed"] = json!(passed);
                assert!(
                    check(&report, mode, true, Clipboard::Rich).is_err(),
                    "{mode:?} requested={requested} passed={passed}"
                );
            }
        }
    }

    #[test]
    fn smoke_evidence_is_rejected_as_an_interaction_session() {
        assert!(check(&rich(Mode::Editable), Mode::Editable, true, Clipboard::Rich).is_err());
    }

    #[test]
    fn stale_incomplete_or_failed_evidence() {
        for (section, key, value) in [
            ("host", "source_revision", json!("old-commit")),
            ("host", "os", json!("windows")),
            ("session", "rendered_frames", json!(19)),
            ("session", "rendered_frames", json!(true)),
            ("session", "rendered_frames", json!(20.0)),
            ("session", "rendered_frames", json!(-1)),
            ("session", "graceful_exit", json!(false)),
            ("session", "smoke_test_requested", json!(false)),
            ("errors", "accessibility", json!(1)),
            ("final_document", "tlfr_round_trip_equal", json!(false)),
            ("final_widget", "composition_active", json!(true)),
            ("final_widget", "read_only", json!(true)),
            ("display", "pixels_per_point", json!(-2.0)),
            ("display", "native_pixels_per_point", json!("2")),
        ] {
            let mut report = rich(Mode::Editable);
            report[section][key] = value;
            assert!(
                check(&report, Mode::Editable, false, Clipboard::Rich).is_err(),
                "{section}.{key}"
            );
        }
    }

    #[test]
    fn content_and_manual_signoff_are_rejected() {
        let mut examples = Vec::new();
        let mut report = rich(Mode::Editable);
        report["document_content_included"] = json!(true);
        examples.push(report);
        let mut report = rich(Mode::Editable);
        report["manual_signoff"] = json!("passed");
        examples.push(report);
        let mut report = rich(Mode::Editable);
        report["final_document"]["text"] = json!("private document");
        examples.push(report);
        let mut report = rich(Mode::Editable);
        report["host_input_events"]["paste"] = json!("private clipboard");
        examples.push(report);
        for (index, report) in examples.iter().enumerate() {
            assert!(
                check(report, Mode::Editable, false, Clipboard::Rich).is_err(),
                "example {index}"
            );
        }
    }

    #[test]
    fn rich_clipboard_missing_failed_or_impossible_completion_is_rejected() {
        for (section, key, value) in [
            ("session", "rich_clipboard_requested", json!(false)),
            ("session", "clipboard_self_test_requested", json!(false)),
            ("session", "clipboard_self_test_passed", json!(false)),
            ("errors", "clipboard_self_test", json!(1)),
        ] {
            let mut report = rich(Mode::Editable);
            report[section][key] = value;
            assert!(
                check(&report, Mode::Editable, false, Clipboard::Rich).is_err(),
                "{section}.{key}"
            );
        }
        let mut report = rich(Mode::Editable);
        report["session"]
            .as_object_mut()
            .unwrap()
            .remove("clipboard_self_test_passed");
        assert!(check(&report, Mode::Editable, false, Clipboard::Rich).is_err());
        for mode in [Mode::ReadOnly, Mode::Disabled] {
            for field in [
                "clipboard_self_test_requested",
                "clipboard_self_test_passed",
            ] {
                let mut report = rich(mode);
                report["session"][field] = json!(true);
                assert!(
                    check(&report, mode, false, Clipboard::Rich).is_err(),
                    "{mode:?} {field}"
                );
            }
        }
    }

    #[test]
    fn disabled_retained_focus_and_focus_gain_are_rejected() {
        for (section, key, value) in [
            ("final_widget", "widget_focus_retained", json!(true)),
            ("observed_state_changes", "focus_gains", json!(1)),
        ] {
            let mut report = rich(Mode::Disabled);
            report[section][key] = value;
            assert!(
                check(&report, Mode::Disabled, false, Clipboard::Rich).is_err(),
                "{section}.{key}"
            );
        }
    }

    #[test]
    fn missing_evidence_duplicate_fields_and_nonfinite_json() {
        let mut report = rich(Mode::Editable);
        report["final_widget"]
            .as_object_mut()
            .unwrap()
            .remove("widget_focus_retained");
        let error = check(&report, Mode::Editable, false, Clipboard::Rich).unwrap_err();
        assert!(error.contains("widget_focus_retained"), "{error}");

        let mut report = rich(Mode::Editable);
        report["display"]
            .as_object_mut()
            .unwrap()
            .remove("native_pixels_per_point");
        assert!(check(&report, Mode::Editable, false, Clipboard::Rich).is_err());

        let text = rich(Mode::Editable).to_string();
        for (from, to) in [
            (
                "{\"schema_version\":1,",
                "{\"schema_version\":1,\"schema_version\":1,",
            ),
            (
                "\"errors\":{\"editing\":0,",
                "\"errors\":{\"editing\":0,\"editing\":0,",
            ),
            ("\"pixels_per_point\":2,", "\"pixels_per_point\":NaN,"),
            ("\"pixels_per_point\":2,", "\"pixels_per_point\":1e999,"),
        ] {
            assert!(text.contains(from), "{from}");
            let altered = text.replacen(from, to, 1);
            assert!(
                validate_text(&altered, Mode::Editable, false, Clipboard::Rich).is_err(),
                "{to}"
            );
        }
        validate_text(&format!("{text}\n"), Mode::Editable, false, Clipboard::Rich).unwrap();
        assert!(
            validate_text(
                &format!("{text}{{}}"),
                Mode::Editable,
                false,
                Clipboard::Rich
            )
            .is_err()
        );
    }

    #[test]
    fn errors_name_the_offending_field() {
        let mut report = rich(Mode::Editable);
        report["session"]["rendered_frames"] = json!(true);
        let error = check(&report, Mode::Editable, false, Clipboard::Rich).unwrap_err();
        assert!(
            error.starts_with("report.session.rendered_frames: "),
            "{error}"
        );
    }
}
