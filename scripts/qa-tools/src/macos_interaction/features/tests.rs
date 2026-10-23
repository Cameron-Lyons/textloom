use serde_json::{Value, json};

use super::{COLORED, FILLED, FONT_ONLY, PLAIN, check_html, html_document, wrapped_prefix};

use std::fs::File;
use std::path::PathBuf;

use super::{Probe, Rich, System, TEXT, URL};
use crate::macos_interaction::{Ran, Recipient, Signal};
use crate::{Result, TempDir};

/// A small independent whole-selection editor for the plain-paste scenario.
/// Copy captures text AND styles; ordinary rich paste restores both. History
/// records actual mutations, so a skipped undo cannot fake a successful state.
#[derive(Clone)]
struct Document {
    text: String,
    colors: bool,
    link: bool,
}

impl Document {
    fn html(&self) -> String {
        let body: String = self
            .text
            .split('\n')
            .map(|text| {
                let mut content = text.to_owned();
                if self.colors {
                    content = format!(
                        "<span style=\"color:#ff0000;background-color:#ffff00\">{content}</span>"
                    );
                }
                if self.link {
                    content = format!("<a href=\"{URL}\">{content}</a>");
                }
                format!("<p>{content}</p>")
            })
            .collect();
        html_document(&body)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    DropPaste,
    PasteRich,
    DropUndo,
    DropRedo,
    StaleCopy,
    KeepFormatting,
}

struct Desktop {
    document: Document,
    clipboard: Document,
    selected: bool,
    undo: Vec<Document>,
    redo: Vec<Document>,
    generation: i64,
    now: f64,
    fault: Fault,
    actions: Vec<(String, String)>,
    rich_offer_at_paste: bool,
}

impl Desktop {
    fn new(fault: Fault) -> Self {
        let document = Document {
            text: TEXT.to_owned(),
            colors: true,
            link: true,
        };
        Self {
            clipboard: document.clone(),
            document,
            selected: false,
            undo: vec![],
            redo: vec![],
            generation: 0,
            now: 0.0,
            fault,
            actions: vec![],
            rich_offer_at_paste: false,
        }
    }

    fn edit(&mut self, edit: impl FnOnce(&mut Document)) {
        self.undo.push(self.document.clone());
        self.redo.clear();
        edit(&mut self.document);
    }

    fn native(&mut self, command: &str, value: &str) -> Result<()> {
        self.actions.push((command.to_owned(), value.to_owned()));
        match (command, value) {
            ("chord", "a") => self.selected = true,
            ("chord", "c") => {
                assert!(
                    self.selected,
                    "oracle only permits copies of witnessed whole selections"
                );
                self.clipboard = self.document.clone();
                if self.fault != Fault::StaleCopy {
                    self.generation += 1;
                }
            }
            ("press-button", "Remove link") => self.edit(|document| document.link = false),
            ("press-button", "Clear formatting") => {
                if self.fault != Fault::KeepFormatting {
                    self.edit(|document| document.colors = false);
                }
            }
            ("type", text) => {
                assert!(self.selected);
                self.edit(|document| document.text = text.to_owned());
                self.selected = false;
            }
            ("shift-chord", "v") => {
                assert!(self.selected);
                self.rich_offer_at_paste =
                    self.clipboard.colors && self.clipboard.link && self.clipboard.text == TEXT;
                if self.fault != Fault::DropPaste {
                    let clipboard = self.clipboard.clone();
                    if self.fault == Fault::PasteRich {
                        self.edit(|document| *document = clipboard);
                    } else {
                        self.edit(|document| document.text = clipboard.text);
                    }
                    self.selected = false;
                }
            }
            ("chord", "z") => {
                if self.fault != Fault::DropUndo {
                    self.redo.push(self.document.clone());
                    self.document = self.undo.pop().ok_or("oracle undo underflow")?;
                }
            }
            ("shift-chord", "z") => {
                if self.fault != Fault::DropRedo {
                    self.undo.push(self.document.clone());
                    self.document = self.redo.pop().ok_or("oracle redo underflow")?;
                }
            }
            _ => return Err(format!("unexpected native action {command} {value}").into()),
        }
        Ok(())
    }
}

fn response(code: i32, stdout: String, stderr: &str) -> Ran {
    Ran {
        code,
        stdout: stdout.into_bytes(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

impl System for Desktop {
    fn run(&mut self, arguments: &[String], _input: Option<&str>, _timeout: f64) -> Result<Ran> {
        if arguments[0] == "/usr/bin/osascript" {
            if arguments[2] != "focus" {
                self.native(&arguments[2], &arguments[5])?;
            }
            return Ok(response(0, "native-action-delivered".into(), ""));
        }
        if arguments[0] == "/usr/bin/env" {
            assert_eq!(
                arguments[2], "/usr/bin/pbpaste",
                "preparing a paste target must not overwrite the rich offer with pbcopy"
            );
            return Ok(response(0, self.clipboard.text.clone(), ""));
        }
        assert!(arguments[0].ends_with("macos-clipboard"));
        let value = match arguments[1].as_str() {
            "ax-press" => {
                self.native(&arguments[4], &arguments[5])?;
                json!({"pressed": true})
            }
            "state" => json!({"change_count":self.generation}),
            "native-html" => json!({"change_count":self.generation,"html":self.clipboard.html()}),
            "plain" if arguments[2] == self.clipboard.text => {
                json!({"change_count":self.generation,"plain_exact":true})
            }
            "plain" => return Ok(response(1, String::new(), "oracle text mismatch")),
            other => panic!("unexpected inspector {other}"),
        };
        Ok(response(0, value.to_string(), ""))
    }
    fn spawn(&mut self, _: &[String], _: File, _: File) -> Result<u32> {
        unreachable!()
    }
    fn poll(&mut self, _: u32) -> Option<i32> {
        unreachable!()
    }
    fn wait(&mut self, _: u32, _: f64) -> Result<Option<i32>> {
        unreachable!()
    }
    fn signal(&mut self, _: u32, _: Signal, _: Recipient) {
        unreachable!()
    }
    fn monotonic(&self) -> f64 {
        self.now
    }
    fn sleep(&mut self, seconds: f64) -> Result<()> {
        self.now += seconds;
        Ok(())
    }
}

#[test]
fn actual_shift_chord_plain_paste_sequence_preserves_rich_offer_and_checks_history() {
    for fault in [
        Fault::None,
        Fault::DropPaste,
        Fault::PasteRich,
        Fault::DropUndo,
        Fault::DropRedo,
        Fault::StaleCopy,
        Fault::KeepFormatting,
    ] {
        let work = TempDir::try_new("macos-feature-replay-").unwrap();
        let mut probe = Probe::try_new(
            Desktop::new(fault),
            PathBuf::from("unused"),
            work.path().to_path_buf(),
            work.path(),
            "candidate",
        )
        .unwrap();
        let linked_body = COLORED
            .body
            .replace("<p>", &format!("<p><a href=\"{URL}\">"))
            .replace("</p>", "</a></p>");
        let result = probe.plain_paste(
            101,
            Rich {
                text: TEXT,
                body: &linked_body,
            },
        );
        if fault == Fault::None {
            result.unwrap();
            assert!(probe.system.rich_offer_at_paste);
            assert_eq!(probe.system.document.text, TEXT);
            assert!(!probe.system.document.colors && !probe.system.document.link);
            assert_eq!(probe.observations["checks"].as_array().unwrap().len(), 1);
            assert_eq!(
                probe
                    .system
                    .actions
                    .iter()
                    .filter(|(command, value)| command == "shift-chord" && value == "v")
                    .count(),
                1
            );
            assert!(probe.system.actions.len() <= 16);
        } else {
            assert!(result.is_err(), "fault {fault:?} incorrectly passed");
            assert!(
                probe.observations["checks"].as_array().unwrap().is_empty(),
                "fault {fault:?} recorded a success"
            );
        }
    }
}

#[test]
fn exact_html_oracle_rejects_missing_or_extra_styles_paragraphs_and_wrong_links() {
    for expected in [PLAIN, FILLED, FONT_ONLY, COLORED] {
        check_html(&json!(html_document(expected.body)), expected.body).unwrap();
        for other in [PLAIN, FILLED, FONT_ONLY, COLORED] {
            if other.body != expected.body {
                assert!(check_html(&json!(html_document(other.body)), expected.body).is_err());
            }
        }
        let extra = format!("{}<p><br></p>", expected.body);
        assert!(check_html(&json!(html_document(&extra)), expected.body).is_err());
    }
    for (expected, broken) in [
        ("<h2>first<br>second</h2>", "<h2>first</h2><h2>second</h2>"),
        (
            "<ol start=\"2\"><li>two</li><li>three</li></ol>",
            "<ol start=\"1\"><li>two</li><li>three</li></ol>",
        ),
        (
            "<blockquote><p>one</p><p>two</p></blockquote>",
            "<p>one</p><p>two</p>",
        ),
        (
            "<p><a href=\"https://example.invalid/right\">link</a></p>",
            "<p><a href=\"https://example.invalid/wrong\">link</a></p>",
        ),
    ] {
        assert!(check_html(&json!(html_document(broken)), expected).is_err());
    }
    assert!(check_html(&json!(null), PLAIN.body).is_err());
}

#[test]
fn forced_wrap_oracle_requires_an_exact_nonempty_proper_single_paragraph_suffix() {
    assert_eq!(
        wrapped_prefix("café long line", "long line").unwrap(),
        "café "
    );
    for (fixture, suffix) in [
        ("whole", "whole"),
        ("whole", ""),
        ("whole", "wrong"),
        ("a\nb", "b"),
        ("a\u{2028}b", "b"),
    ] {
        assert!(wrapped_prefix(fixture, suffix).is_err());
    }
    assert!(wrapped_prefix("café", "cafe\u{301}").is_err());
}

/// This fixture imports only the driver's protocol, not AppKit semantics. A
/// false helper result must survive later successful checks and fail completion.
struct ImportDesktop {
    observation: Value,
    generation: i64,
    now: f64,
    actions: Vec<String>,
    open: bool,
    prepared: bool,
    fault: &'static str,
}

impl ImportDesktop {
    fn new(observation: Value) -> Self {
        Self {
            observation,
            generation: 1,
            now: 0.0,
            actions: Vec::new(),
            open: true,
            prepared: false,
            fault: "",
        }
    }
}

impl System for ImportDesktop {
    fn run(&mut self, arguments: &[String], _: Option<&str>, _: f64) -> Result<Ran> {
        if arguments[0] == "/usr/bin/osascript" {
            let command = arguments[2].as_str();
            let value = arguments[5].as_str();
            let owner = if arguments[3] == "101" { "native:" } else { "" };
            self.actions.push(format!("{owner}{command}:{value}"));
            if self.fault == "paste" && command == "chord" && value == "v" {
                return Ok(response(1, String::new(), "native paste delivery failed"));
            }
            match (command, value) {
                ("chord", "c") if self.fault != "stale" => self.generation += 1,
                ("close", "") => self.open = false,
                _ => {}
            }
            return Ok(response(0, "delivered".into(), ""));
        }
        if arguments[0] == "/usr/bin/env" {
            assert_eq!(arguments[2], "/usr/bin/pbpaste");
            return Ok(response(0, super::QUOTE.text.to_owned(), ""));
        }
        if arguments[0] == "/usr/bin/open" {
            assert!(
                !self.open && self.prepared,
                "must close before preparing/reopening"
            );
            self.open = true;
            self.actions.push("open".into());
            return Ok(response(0, String::new(), ""));
        }
        assert!(arguments[0].ends_with("macos-clipboard"));
        let command = arguments[1].as_str();
        self.actions.push(command.into());
        let mut result = match command {
            "state" | "diagnose" => json!({}),
            "plain" => {
                assert_eq!(arguments[2], super::QUOTE.text);
                json!({"plain_exact":true})
            }
            "native-html" => json!({"html":html_document(super::QUOTE.body)}),
            "capture-native-offer" => {
                self.actions
                    .push(format!("capture-native-offer:{}", arguments[3]));
                if self.fault == "capture" {
                    return Err("Native source changed during capture".into());
                }
                assert_eq!(arguments[4], self.generation.to_string());
                let metadata = json!({"native_source_snapshot":true,"label":arguments[3],
                    "change_count":self.generation,"types":["public.html","public.utf8-plain-text","org.textloom.fragment"]});
                let prefix =
                    PathBuf::from(&arguments[2]).join(format!("macos-fixture-{}", arguments[3]));
                let envelope = [
                    b"TLCL\x01\x00\xff".as_slice(),
                    &self.generation.to_le_bytes(),
                ]
                .concat();
                for (extension, bytes) in [
                    ("html", html_document(super::QUOTE.body).into_bytes()),
                    ("envelope", envelope),
                    ("plain", super::QUOTE.text.as_bytes().to_vec()),
                    ("json", serde_json::to_vec(&metadata)?),
                ] {
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(prefix.with_extension(extension))?;
                    file.write_all(&bytes)?;
                }
                metadata
            }
            "prepare-import" => {
                assert!(!self.open, "must not replace an open document on disk");
                self.prepared = true;
                json!({"neutral_import_prepared":true})
            }
            "textedit-features" if self.fault == "helper" => {
                return Err("helper transport failed".into());
            }
            "textedit-features" => self.observation.clone(),
            other => panic!("unexpected import helper {other}"),
        };
        result["change_count"] = json!(self.generation);
        Ok(response(0, result.to_string(), ""))
    }
    fn spawn(&mut self, _: &[String], _: File, _: File) -> Result<u32> {
        unreachable!()
    }
    fn poll(&mut self, _: u32) -> Option<i32> {
        unreachable!()
    }
    fn wait(&mut self, _: u32, _: f64) -> Result<Option<i32>> {
        unreachable!()
    }
    fn signal(&mut self, _: u32, _: Signal, _: Recipient) {
        unreachable!()
    }
    fn monotonic(&self) -> f64 {
        self.now
    }
    fn sleep(&mut self, seconds: f64) -> Result<()> {
        self.now += seconds;
        Ok(())
    }
}

#[test]
fn failed_import_is_durable_followups_run_and_final_aggregate_still_fails() {
    let work = TempDir::try_new("macos-import-failures-").unwrap();
    let failure = json!({"textedit_rtf_features_exact":false, "error":"lost quote indentation",
        "attribute_runs":[{"paragraph":{"head_indent":0,"text_blocks":[]}}]});
    let mut probe = Probe::try_new(
        ImportDesktop::new(failure.clone()),
        PathBuf::from("unused"),
        work.path().to_path_buf(),
        work.path(),
        "candidate",
    )
    .unwrap();
    probe.textedit_pid = Some(202);
    probe
        .feature_import("quote", "quote-native-reused", super::quote_specification())
        .unwrap();
    assert_eq!(probe.observations["checks"], json!([]));
    assert_eq!(
        probe.observations["feature_failures"][0]["observation"]["attribute_runs"],
        failure["attribute_runs"]
    );
    assert!(!probe.passed());
    assert_eq!(
        probe
            .system
            .actions
            .iter()
            .filter(|action| *action == "textedit-features")
            .count(),
        1
    );
    assert_eq!(probe.system.actions.last().unwrap(), "chord:s");
    let artifact = work
        .path()
        .join("macos-fixture-quote-native-reused.features.json");
    let saved = std::fs::read(&artifact).unwrap();
    probe.system.observation = json!({"textedit_rtf_features_exact":true});
    probe
        .feature_import("later numbering", "feature-numbering", json!({}))
        .unwrap();
    assert_eq!(probe.observations["checks"][0]["name"], "later numbering");
    assert_eq!(
        std::fs::read(artifact).unwrap(),
        saved,
        "later imports must not overwrite evidence"
    );
    assert!(
        probe
            .require_feature_success()
            .unwrap_err()
            .to_string()
            .contains("quote")
    );
    assert!(!probe.passed());
}

#[test]
fn import_ui_transport_stale_copy_and_malformed_results_are_fatal() {
    for fault in ["paste", "helper", "stale", "malformed"] {
        let work = TempDir::try_new("macos-import-fatal-").unwrap();
        let mut system = ImportDesktop::new(json!({"textedit_rtf_features_exact":true}));
        system.fault = fault;
        if fault == "malformed" {
            system.observation = json!({"error":"missing status"});
        }
        let mut probe = Probe::try_new(
            system,
            PathBuf::from("unused"),
            work.path().to_path_buf(),
            work.path(),
            "candidate",
        )
        .unwrap();
        probe.textedit_pid = Some(202);
        assert!(
            probe
                .feature_import("fatal", "quote-native-reused", json!({}))
                .is_err(),
            "{fault}"
        );
        assert_eq!(probe.observations["checks"], json!([]));
        assert!(!probe.observations.contains_key("feature_failures"));
        assert!(!probe.passed());
    }
    for invalid in [
        json!({}),
        json!({"textedit_rtf_features_exact":false}),
        json!({"textedit_rtf_features_exact":false,"error":""}),
        json!({"textedit_rtf_features_exact":true,"error":"contradictory"}),
    ] {
        assert!(super::import_passed(invalid.as_object().unwrap()).is_err());
    }
}

#[test]
fn neutral_destination_closes_before_reset_and_preserves_native_clipboard_generation() {
    let work = TempDir::try_new("macos-import-neutral-").unwrap();
    let mut probe = Probe::try_new(
        ImportDesktop::new(json!({})),
        PathBuf::from("unused"),
        work.path().to_path_buf(),
        work.path(),
        "candidate",
    )
    .unwrap();
    probe.textedit_pid = Some(202);
    probe.fresh_feature_destination().unwrap();
    assert_eq!(
        probe.system.actions,
        ["close:", "prepare-import", "open", "focus:"]
    );
    assert_eq!(probe.system.generation, 1);
    assert!(probe.system.open && probe.system.prepared);
}

#[test]
fn both_native_quote_sources_are_captured_before_destination_actions_and_remain_distinct() {
    let work = TempDir::try_new("macos-quote-sources-").unwrap();
    let mut probe = Probe::try_new(
        ImportDesktop::new(json!({"textedit_rtf_features_exact":true})),
        PathBuf::from("unused"),
        work.path().to_path_buf(),
        work.path(),
        "candidate",
    )
    .unwrap();
    probe.textedit_pid = Some(202);
    let mut saved = Vec::new();
    for (fresh, label) in [
        (false, "quote-native-reused-source"),
        (true, "quote-native-fresh-source"),
    ] {
        probe.system.actions.clear();
        probe.native_quote_import(101, fresh).unwrap();
        let actions = &probe.system.actions;
        let capture = actions
            .iter()
            .position(|action| action == &format!("capture-native-offer:{label}"))
            .unwrap();
        let destination = actions
            .iter()
            .position(|action| action == if fresh { "close:" } else { "chord:a" })
            .unwrap();
        assert!(
            capture < destination,
            "source evidence must precede even destination focus/close"
        );
        assert_eq!(
            actions
                .iter()
                .filter(|action| *action == "capture-native-offer")
                .count(),
            1
        );
        for extension in ["html", "plain", "envelope", "json"] {
            let path = work
                .path()
                .join(format!("macos-fixture-{label}.{extension}"));
            let bytes = std::fs::read(&path).unwrap();
            if extension == "html" {
                assert_eq!(bytes, html_document(super::QUOTE.body).as_bytes());
            }
            if extension == "plain" {
                assert_eq!(bytes, super::QUOTE.text.as_bytes());
            }
            saved.push((path, bytes));
        }
    }
    let manifests: Vec<Value> = saved
        .iter()
        .filter(|(path, _)| path.extension().unwrap() == "json")
        .map(|(_, bytes)| serde_json::from_slice(bytes).unwrap())
        .collect();
    assert_ne!(manifests[0]["change_count"], manifests[1]["change_count"]);
    assert_ne!(manifests[0]["label"], manifests[1]["label"]);
    probe
        .feature_import("later numbering", "feature-numbering", json!({}))
        .unwrap();
    // Reusing a capture label must fail before another destination paste.
    probe.system.actions.clear();
    assert!(probe.native_quote_import(101, false).is_err());
    assert!(
        !probe
            .system
            .actions
            .iter()
            .any(|action| action == "chord:v")
    );
    for (path, bytes) in saved {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn unstable_native_source_capture_is_fatal_before_any_destination_action() {
    let work = TempDir::try_new("macos-quote-source-race-").unwrap();
    let mut system = ImportDesktop::new(json!({"textedit_rtf_features_exact":true}));
    system.fault = "capture";
    let mut probe = Probe::try_new(
        system,
        PathBuf::from("unused"),
        work.path().to_path_buf(),
        work.path(),
        "candidate",
    )
    .unwrap();
    probe.textedit_pid = Some(202);
    for fresh in [false, true] {
        probe.system.actions.clear();
        assert!(probe.native_quote_import(101, fresh).is_err());
        assert!(
            !probe
                .system
                .actions
                .iter()
                .any(|action| action == "chord:a" || action == "close:")
        );
        assert_eq!(
            probe
                .system
                .actions
                .iter()
                .filter(|action| *action == "capture-native-offer")
                .count(),
            1
        );
        assert_eq!(probe.observations["checks"], json!([]));
    }
}
