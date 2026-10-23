use serde_json::json;

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
