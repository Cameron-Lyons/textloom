//! Replays the probe against a small fixture oracle instead of a desktop. It is
//! not a replacement for the hosted native run: explicit grapheme tokens and
//! column-zero vertical moves let the authored sequences be checked without
//! macOS, font metrics, or editor bindings.

use std::collections::VecDeque;

use super::*;

type Ignored = Box<dyn Fn(&str, &str) -> bool>;

const WOMAN_TECHNOLOGIST: &str = "\u{1f469}\u{1f3fd}\u{200d}\u{1f4bb}";

fn tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut rest = text;
    while let Some(character) = rest.chars().next() {
        let length = ["e\u{301}", WOMAN_TECHNOLOGIST]
            .into_iter()
            .find(|cluster| rest.starts_with(cluster))
            .map_or(character.len_utf8(), str::len);
        tokens.push(rest[..length].to_owned());
        rest = &rest[length..];
    }
    tokens
}

/// The editor the oracle simulates: grapheme tokens, a selection, and undo.
/// Shift-Return inserts U+2028, which copies as LF.
#[derive(Default)]
struct Editor {
    text: Vec<String>,
    anchor: usize,
    focus: usize,
    history: Vec<(Vec<String>, usize, usize)>,
    undo_count: usize,
}

impl Editor {
    fn load(&mut self, text: &str, caret: usize) {
        self.text = tokens(text);
        self.anchor = caret;
        self.focus = caret;
    }

    fn boundaries(&self, soft: bool) -> (Vec<usize>, Vec<usize>, usize) {
        let mut ends: Vec<usize> = (0..self.text.len())
            .filter(|&index| self.text[index] == "\n" || (soft && self.text[index] == "\u{2028}"))
            .collect();
        let starts: Vec<usize> = std::iter::once(0)
            .chain(ends.iter().map(|end| end + 1))
            .collect();
        let row = starts
            .iter()
            .rposition(|&start| start <= self.focus)
            .unwrap_or(0);
        ends.push(self.text.len());
        (starts, ends, row)
    }

    fn selection(&self) -> (usize, usize) {
        (self.anchor.min(self.focus), self.anchor.max(self.focus))
    }

    fn selected(&self) -> String {
        let (start, end) = self.selection();
        self.text[start..end].concat().replace('\u{2028}', "\n")
    }

    fn replace(&mut self, start: usize, end: usize, text: &str) -> Result<(), String> {
        if start > end || end > self.text.len() {
            return Err(format!("edit {start}..{end} is outside the document"));
        }
        self.history
            .push((self.text.clone(), self.anchor, self.focus));
        let inserted = tokens(text);
        let caret = start + inserted.len();
        self.text.splice(start..end, inserted);
        self.anchor = caret;
        self.focus = caret;
        Ok(())
    }

    fn row(rows: &[usize], row: usize, step: isize) -> Result<usize, String> {
        row.checked_add_signed(step)
            .and_then(|row| rows.get(row).copied())
            .ok_or_else(|| "vertical move left the document".to_owned())
    }

    fn column_zero(&self) -> Result<(Vec<usize>, usize), String> {
        let (rows, _, row) = self.boundaries(true);
        if self.focus != rows[row] {
            return Err("vertical fixture must start at column zero".to_owned());
        }
        Ok((rows, row))
    }

    fn step(&self, forward: bool) -> Result<usize, String> {
        if forward {
            Ok(self.focus + 1)
        } else {
            self.focus
                .checked_sub(1)
                .ok_or("moved before the document".into())
        }
    }

    fn apply(&mut self, command: &str, value: &str) -> Result<(), String> {
        let mut target = None;
        let extend = command.contains("shift");
        match command {
            "command-keycode" | "command-shift-keycode" => match value {
                "51" => {
                    let (starts, _, row) = self.boundaries(true);
                    self.replace(starts[row], self.focus, "")?;
                }
                "124" => {
                    let (_, ends, row) = self.boundaries(true);
                    target = Some(ends[row]);
                }
                "125" => target = Some(self.text.len()),
                "126" => target = Some(0),
                _ => return Err(format!("unexpected Command key {value}")),
            },
            "control-chord" | "control-shift-chord" => {
                let (starts, ends, paragraph) = self.boundaries(false);
                match value {
                    "a" => target = Some(starts[paragraph]),
                    "e" => target = Some(ends[paragraph]),
                    "b" | "f" => target = Some(self.step(value == "f")?),
                    "p" | "n" => {
                        let (rows, row) = self.column_zero()?;
                        target = Some(Self::row(&rows, row, if value == "p" { -1 } else { 1 })?);
                    }
                    "h" => {
                        let start = self.step(false)?;
                        self.replace(start, self.focus, "")?;
                    }
                    "d" => self.replace(self.focus, self.focus + 1, "")?,
                    _ => return Err(format!("unexpected Control alias {value}")),
                }
            }
            "option-keycode" | "option-shift-keycode" => {
                let (starts, ends, paragraph) = self.boundaries(false);
                match value {
                    "126" => {
                        let at_start = usize::from(self.focus == starts[paragraph]);
                        target = Some(starts[paragraph.saturating_sub(at_start)]);
                    }
                    "125" => {
                        let at_end = usize::from(self.focus == ends[paragraph]);
                        target = Some(ends[(paragraph + at_end).min(ends.len() - 1)]);
                    }
                    _ => return Err(format!("unexpected Option key {value}")),
                }
            }
            "keycode" | "shift-keycode" => match value {
                "36" => self.replace(self.focus, self.focus, "\u{2028}")?,
                "123" | "124" => target = Some(self.step(value == "124")?),
                "125" => {
                    let (rows, row) = self.column_zero()?;
                    target = Some(Self::row(&rows, row, 1)?);
                }
                _ => return Err(format!("unexpected key {value}")),
            },
            "type" => self.replace(self.focus, self.focus, value)?,
            _ => return Err(format!("unexpected fixture action {command} {value}")),
        }
        if let Some(target) = target {
            if target > self.text.len() {
                return Err(format!("caret {target} is outside the document"));
            }
            self.focus = target;
            if !extend {
                self.anchor = target;
            }
        }
        Ok(())
    }
}

/// The desktop the probe sees: System Events actions edit the oracle's
/// document, and the pasteboard, its inspector, and pbpaste follow it.
#[derive(Default)]
struct Fake {
    clock: f64,
    commands: Vec<(Vec<String>, Option<String>)>,
    actions: Vec<(String, String)>,
    editor: Editor,
    pasteboard: String,
    generation: i64,
    /// What pbpaste prints instead of the pasteboard.
    pasted: Option<Vec<u8>>,
    ignored: Option<Ignored>,
    swap_vertical: bool,
    textedit: VecDeque<Option<u32>>,
    polls: HashMap<u32, VecDeque<Option<i32>>>,
    waits: HashMap<u32, VecDeque<Option<i32>>>,
    wait_calls: Vec<(u32, f64)>,
    signals: Vec<(u32, Signal, bool)>,
}

fn ran(code: i32, stdout: impl Into<Vec<u8>>, stderr: &str) -> Ran {
    Ran {
        code,
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

impl Fake {
    fn osascript(&mut self, arguments: &[String]) -> Ran {
        let command = arguments[0].as_str();
        match command {
            "preflight" => return ran(0, "accessibility-enabled", ""),
            "textedit-pid" => {
                let pid = self.textedit.pop_front().flatten();
                return ran(
                    0,
                    pid.map_or("absent".to_owned(), |pid| pid.to_string()),
                    "",
                );
            }
            _ => {}
        }
        let (window, value) = (arguments[2].as_str(), arguments[3].clone());
        self.actions.push((command.to_owned(), value.clone()));
        if window != TITLE {
            return ran(1, "", &format!("unexpected window {window}"));
        }
        if self
            .ignored
            .as_ref()
            .is_some_and(|ignored| ignored(command, &value))
        {
            return ran(0, "native-action-delivered", "");
        }
        let mut value = value.as_str();
        if self.swap_vertical && command.starts_with("control") {
            value = match value {
                "p" => "n",
                "n" => "p",
                other => other,
            };
        }
        let editor = &mut self.editor;
        let result = match (command, value) {
            ("chord", "a") => {
                (editor.anchor, editor.focus) = (0, editor.text.len());
                Ok(())
            }
            ("chord", "v") => {
                let (start, end) = editor.selection();
                editor.replace(start, end, &self.pasteboard)
            }
            ("chord", "c") => {
                self.pasteboard = editor.selected();
                self.generation += 1;
                Ok(())
            }
            ("chord", "z") => match editor.history.pop() {
                Some((text, anchor, focus)) => {
                    (editor.text, editor.anchor, editor.focus) = (text, anchor, focus);
                    editor.undo_count += 1;
                    Ok(())
                }
                None => Err("nothing to undo".to_owned()),
            },
            _ => editor.apply(command, value),
        };
        match result {
            Ok(()) => ran(0, "native-action-delivered", ""),
            Err(message) => ran(1, "", &message),
        }
    }

    fn inspector(&mut self, arguments: &[String]) -> Ran {
        let count = self.generation;
        match arguments[0].as_str() {
            "state" => ran(0, json!({"change_count": count}).to_string(), ""),
            "plain" if self.pasteboard == arguments[1] => ran(
                0,
                json!({"change_count": count, "plain_exact": true}).to_string(),
                "",
            ),
            "plain" => ran(
                1,
                "",
                "Native clipboard differs from the expected document state",
            ),
            other => panic!("unexpected inspector command {other}"),
        }
    }

    fn action_names(&self) -> Vec<String> {
        self.actions
            .iter()
            .map(|(command, value)| format!("{command} {value}"))
            .collect()
    }
}

impl System for Fake {
    fn run(&mut self, arguments: &[String], input: Option<&str>, _timeout: f64) -> Result<Ran> {
        self.commands
            .push((arguments.to_vec(), input.map(str::to_owned)));
        Ok(match arguments[0].as_str() {
            "/usr/bin/osascript" => self.osascript(&arguments[2..]),
            "/usr/bin/env" if arguments[2] == "/usr/bin/pbcopy" => {
                self.pasteboard = input.unwrap_or_default().to_owned();
                self.generation += 1;
                ran(0, "", "")
            }
            "/usr/bin/env" if arguments[2] == "/usr/bin/pbpaste" => ran(
                0,
                self.pasted
                    .clone()
                    .unwrap_or_else(|| self.pasteboard.clone().into_bytes()),
                "",
            ),
            "/bin/ps" => ran(
                0,
                "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit\n",
                "",
            ),
            helper if helper.ends_with("/macos-clipboard") => self.inspector(&arguments[1..]),
            _ => panic!("unexpected command {arguments:?}"),
        })
    }

    fn spawn(&mut self, _arguments: &[String], _stdout: File, _stderr: File) -> Result<u32> {
        unimplemented!("the replay starts no native hosts")
    }

    fn poll(&mut self, pid: u32) -> Option<i32> {
        self.polls
            .get_mut(&pid)
            .and_then(VecDeque::pop_front)
            .flatten()
    }

    fn wait(&mut self, pid: u32, timeout: f64) -> Result<Option<i32>> {
        self.wait_calls.push((pid, timeout));
        Ok(self
            .waits
            .get_mut(&pid)
            .and_then(VecDeque::pop_front)
            .flatten())
    }

    fn signal(&mut self, pid: u32, signal: Signal, group: bool) {
        self.signals.push((pid, signal, group));
    }

    fn monotonic(&self) -> f64 {
        self.clock
    }

    fn sleep(&mut self, seconds: f64) -> Result<()> {
        self.clock += seconds;
        Ok(())
    }
}

struct Replay {
    probe: Probe<Fake>,
    _work: TempDir,
    _output: TempDir,
}

impl Replay {
    fn new(fake: Fake) -> Self {
        let work = TempDir::new("textloom-macos-test-work-").unwrap();
        let output = TempDir::new("textloom-macos-test-output-").unwrap();
        let probe = Probe::new(
            fake,
            PathBuf::from("unused"),
            output.path().to_path_buf(),
            work.path(),
            "candidate",
        )
        .unwrap();
        Self {
            probe,
            _work: work,
            _output: output,
        }
    }

    fn with_text(text: &str, caret: usize) -> Self {
        let mut fake = Fake::default();
        fake.editor.load(text, caret);
        Self::new(fake)
    }

    fn fake(&self) -> &Fake {
        &self.probe.system
    }

    fn notes(&self) -> Vec<Value> {
        self.probe.observations["checks"]
            .as_array()
            .unwrap()
            .clone()
    }
}

/// The caret after "a e\u{301}" in the Unicode fixture.
const AFTER_COMBINING: usize = 3;

#[test]
fn fixture_uses_public_utf8_clipboard_input_and_native_paste() {
    let mut replay = Replay::new(Fake::default());
    replay
        .probe
        .shortcut_fixture(101, SHORTCUT_UNICODE)
        .unwrap();
    let steps: Vec<String> = replay
        .fake()
        .commands
        .iter()
        .map(|(arguments, input)| match arguments[0].as_str() {
            "/usr/bin/osascript" => format!("{} {}", arguments[2], arguments[5]),
            "/usr/bin/env" => format!("{} {input:?}", arguments[1..].join(" ")),
            _ => arguments[1..].join(" "),
        })
        .collect();
    let input = format!("{:?}", Some(SHORTCUT_UNICODE));
    assert_eq!(
        steps,
        [
            format!("LC_ALL=en_US.UTF-8 /usr/bin/pbcopy {input}"),
            "chord a".to_owned(),
            "chord v".to_owned(),
            "state".to_owned(),
            "chord a".to_owned(),
            "chord c".to_owned(),
            format!("plain {SHORTCUT_UNICODE}"),
            "LC_ALL=en_US.UTF-8 /usr/bin/pbpaste -Prefer txt None".to_owned(),
        ]
    );
    assert_eq!(replay.fake().editor.text.concat(), SHORTCUT_UNICODE);
}

#[test]
fn selection_check_preserves_selection_and_requires_exact_copy_before_success() {
    let mut replay = Replay::with_text(SHORTCUT_UNICODE, AFTER_COMBINING);
    replay
        .probe
        .shortcut_check(
            101,
            "selection",
            "e\u{301}",
            &[("control-shift-chord", "b")],
            None,
        )
        .unwrap();
    assert_eq!(
        replay.fake().action_names(),
        ["control-shift-chord b", "chord c"]
    );
    let notes = replay.notes();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["name"], "editable: shortcut selection");
    assert_eq!(notes[0]["plain_exact"], true);
    assert_eq!(notes[0]["plain_utf8_exact"], true);
}

#[test]
fn copy_requires_fresh_generation_and_codepoint_exact_unicode_with_trailing_lf() {
    let expected = format!("e\u{301}{WOMAN_TECHNOLOGIST}\n");
    for actual in [
        expected.clone(),
        format!("\u{e9}{WOMAN_TECHNOLOGIST}\n"),
        expected.trim_end_matches('\n').to_owned(),
        expected.replace('\n', "\r\n"),
        expected.replace('\n', "\r"),
    ] {
        let mut fake = Fake {
            generation: 7,
            pasted: Some(actual.clone().into_bytes()),
            ..Fake::default()
        };
        fake.editor.load(&expected, 0);
        fake.editor.focus = fake.editor.text.len();
        let mut replay = Replay::new(fake);
        let result = replay.probe.copy(101, TITLE, &expected, false, false);
        if actual == expected {
            assert_eq!(result.unwrap()["plain_utf8_exact"], true);
        } else {
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains("exact Unicode fixture"),
                "{actual:?}: {error}"
            );
        }
        let fake = replay.fake();
        assert_eq!(fake.action_names(), ["chord c"]);
        let inspected: Vec<_> = fake
            .commands
            .iter()
            .filter(|(arguments, _)| arguments[0].ends_with("/macos-clipboard"))
            .collect();
        assert_eq!(inspected[1].0[1..], ["plain".to_owned(), expected.clone()]);
        assert!(fake.generation > 7, "the copy published a fresh generation");
        assert_eq!(fake.commands.last().unwrap().0, strings(&PASTE));
    }
}

#[test]
fn binary_runner_preserves_unicode_crlf_cr_and_lf_without_a_desktop() {
    let payload = format!("e\u{301}{WOMAN_TECHNOLOGIST}\r\nx\ry\n").into_bytes();
    let directory = TempDir::new("textloom-macos-test-").unwrap();
    let path = directory.path().join("payload");
    fs::write(&path, &payload).unwrap();
    let mut machine = Machine::new(Arc::default());
    let result = machine
        .run(
            &["/bin/cat".to_owned(), path.display().to_string()],
            None,
            10.0,
        )
        .unwrap();
    assert_eq!(result.code, 0);
    assert_eq!(result.stdout, payload);
    let echoed = machine
        .run(&["/bin/cat".to_owned()], Some(SHORTCUT_UNICODE), 10.0)
        .unwrap();
    assert_eq!(echoed.stdout, SHORTCUT_UNICODE.as_bytes());
}

#[test]
fn runner_reports_timeouts_and_interruptions() {
    let interrupted = Arc::new(AtomicBool::new(false));
    let mut machine = Machine::new(Arc::clone(&interrupted));
    let sleep = ["/bin/sleep".to_owned(), "5".to_owned()];
    let error = machine.run(&sleep, None, 0.1).err().unwrap();
    assert!(error.to_string().contains("timed out"), "{error}");
    // A background process holding stdout open cannot outlast the deadline either.
    let started = Instant::now();
    let held = [
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "sleep 5 & echo done".to_owned(),
    ];
    let error = machine.run(&held, None, 0.5).err().unwrap();
    assert!(error.to_string().contains("did not close"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(3));
    interrupted.store(true, Ordering::SeqCst);
    let error = machine.sleep(1.0).unwrap_err();
    assert_eq!(error.to_string(), "Hosted native probe interrupted");
    // Reported once, so cleanup can run afterwards.
    machine.sleep(0.0).unwrap();
}

#[test]
fn deletion_checks_full_document_then_single_undo_exact_restoration() {
    let mut replay = Replay::with_text(SHORTCUT_UNICODE, AFTER_COMBINING);
    let after = format!("a {WOMAN_TECHNOLOGIST} z");
    replay
        .probe
        .shortcut_check(
            101,
            "deletion",
            &after,
            &[("control-chord", "h")],
            Some(SHORTCUT_UNICODE),
        )
        .unwrap();
    assert_eq!(
        replay.fake().action_names(),
        [
            "control-chord h",
            "chord a",
            "chord c",
            "chord z",
            "chord a",
            "chord c"
        ]
    );
    let notes = replay.notes();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["name"], "editable: shortcut deletion");
    assert_eq!(notes[0]["plain_exact"], true);
    assert_eq!(notes[0]["undo_plain_exact"], true);
}

#[test]
fn failed_edit_or_undo_assertion_never_records_success() {
    for fail_undo in [false, true] {
        let mut fake = Fake::default();
        fake.editor.load(SHORTCUT_UNICODE, AFTER_COMBINING);
        fake.ignored = Some(if fail_undo {
            Box::new(|command, value| command == "chord" && value == "z")
        } else {
            Box::new(|command, _| command == "control-chord")
        });
        let mut replay = Replay::new(fake);
        let error = replay
            .probe
            .shortcut_check(
                101,
                "deletion",
                "a e\u{301} z",
                &[("control-chord", "d")],
                Some(SHORTCUT_UNICODE),
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("Shortcut deletion: Clipboard plain failed: "),
            "{error}"
        );
        let expected: &[&str] = if fail_undo {
            &[
                "control-chord d",
                "chord a",
                "chord c",
                "chord z",
                "chord a",
                "chord c",
            ]
        } else {
            &["control-chord d", "chord a", "chord c"]
        };
        assert_eq!(replay.fake().action_names(), expected);
        assert!(replay.notes().is_empty());
    }
}

fn replay(fake: Fake) -> (Replay, Result<()>) {
    let mut replay = Replay::new(fake);
    let result = replay.probe.shortcuts(101);
    (replay, result)
}

#[test]
fn authored_sequences_select_exact_fixtures_and_undo_all_deletions() {
    let (replay, result) = replay(Fake::default());
    result.unwrap();
    let fake = replay.fake();
    assert_eq!(fake.editor.text.concat(), SHORTCUT_UNICODE);
    assert_eq!(fake.editor.undo_count, 3);
    assert_eq!(replay.notes().len(), 25);
    // Keep the extension bounded; no per-case app launches or new sleeps.
    assert!(
        fake.actions.len() <= 160,
        "{} native actions",
        fake.actions.len()
    );
    for (command, value) in &fake.actions {
        if command.starts_with("control") {
            assert!(
                "aebfpnhd".contains(value.as_str()),
                "Control-{value} may be OS-reserved"
            );
        }
    }
}

#[test]
fn exact_fixture_oracle_rejects_each_missing_native_shortcut() {
    let mut missing: Vec<(String, Ignored)> = Vec::new();
    for letter in ["a", "e", "b", "f", "p", "n", "h", "d"] {
        missing.push((
            format!("Control-{letter}"),
            Box::new(move |command: &str, value: &str| {
                command.starts_with("control") && value == letter
            }),
        ));
    }
    for key in ["125", "126"] {
        missing.push((
            format!("Option-{key}"),
            Box::new(move |command: &str, value: &str| {
                command.starts_with("option") && value == key
            }),
        ));
    }
    missing.push((
        "Shift-Control".into(),
        Box::new(|command: &str, _: &str| command == "control-shift-chord"),
    ));
    missing.push((
        "Shift-Option".into(),
        Box::new(|command: &str, _: &str| command == "option-shift-keycode"),
    ));
    missing.push((
        "Command-Backspace".into(),
        Box::new(|command: &str, value: &str| command == "command-keycode" && value == "51"),
    ));
    for (name, ignored) in missing {
        let (_, result) = replay(Fake {
            ignored: Some(ignored),
            ..Fake::default()
        });
        assert!(result.is_err(), "the replay passed without {name}");
    }
}

#[test]
fn fixture_oracle_rejects_swapped_control_p_and_n() {
    let (_, result) = replay(Fake {
        swap_vertical: true,
        ..Fake::default()
    });
    assert!(result.is_err());
}

#[test]
fn added_system_events_actions_deliver_only_the_requested_modifiers() {
    for (command, modifiers) in [
        ("control-chord", "control down"),
        ("control-shift-chord", "control down, shift down"),
        ("command-keycode", "command down"),
        ("command-shift-keycode", "command down, shift down"),
        ("shift-keycode", "shift down"),
        ("option-keycode", "option down"),
        ("option-shift-keycode", "option down, shift down"),
    ] {
        let body = APPLESCRIPT
            .split(&format!("commandName is \"{command}\" then\n"))
            .nth(1)
            .unwrap();
        let body = body.split("else").next().unwrap();
        let operation = if command.ends_with("chord") {
            "keystroke"
        } else {
            "key code"
        };
        assert!(body.contains(operation), "{command}");
        assert!(
            body.contains(&format!("using {{{modifiers}}}")),
            "{command}"
        );
    }
}

#[test]
fn document_urls_match_textedit_documents() {
    assert_eq!(
        file_uri(Path::new("/tmp/a b/c~d.rtf")),
        "file:///tmp/a%20b/c~d.rtf"
    );
    assert_eq!(
        file_uri(Path::new("/tmp/caf\u{e9}")),
        "file:///tmp/caf%C3%A9"
    );
    let replay = Replay::new(Fake::default());
    let (absolute, resolved) = &replay.probe.document_urls["textloom-html-import"];
    assert!(absolute.starts_with("file:///") && absolute.ends_with("/textloom-html-import.rtf"));
    assert!(resolved.ends_with("/textloom-html-import.rtf"));
}

#[test]
fn native_exit_race_still_reaps_all_children() {
    for during_kill in [false, true] {
        let mut fake = Fake::default();
        for pid in [101, 102] {
            fake.polls.insert(pid, VecDeque::from([None, Some(0)]));
        }
        let first = if during_kill {
            vec![None, Some(-9)]
        } else {
            vec![Some(0)]
        };
        fake.waits.insert(101, first.into());
        fake.waits.insert(102, VecDeque::from([Some(0)]));
        let mut replay = Replay::new(fake);
        replay.probe.hosts = vec![101, 102];
        let error = replay.probe.cleanup().unwrap_err().to_string();
        assert!(error.contains("did not all close"), "{error}");
        let fake = replay.fake();
        let mut signals = vec![(101, Signal::Terminate, true)];
        if during_kill {
            signals.push((101, Signal::Kill, true));
        }
        signals.push((102, Signal::Terminate, true));
        assert_eq!(fake.signals, signals);
        let waits = if during_kill { 2 } else { 1 };
        assert_eq!(
            fake.wait_calls
                .iter()
                .filter(|(pid, _)| *pid == 101)
                .count(),
            waits
        );
        assert_eq!(
            fake.wait_calls
                .iter()
                .filter(|call| *call == &(102, 3.0))
                .count(),
            1
        );
        let cleanup = &replay.probe.observations["cleanup"];
        assert_eq!(cleanup["native_processes_reaped"], true);
        assert_eq!(cleanup["native_sessions_closed_gracefully"], false);
        assert!(!replay.probe.passed());
    }
}

#[test]
fn textedit_exit_race_still_confirms_absence() {
    for during_kill in [false, true] {
        // TextEdit exits after the signal, or had already exited before it.
        let answers = if during_kill {
            vec![Some(103), None, None, None]
        } else {
            vec![None, None, None]
        };
        let fake = Fake {
            textedit: answers.into(),
            ..Fake::default()
        };
        let mut replay = Replay::new(fake);
        replay.probe.textedit_pid = Some(103);
        replay.probe.cleanup().unwrap();
        assert_eq!(replay.fake().signals, [(103, Signal::Terminate, false)]);
        assert!(replay.fake().textedit.is_empty());
        assert_eq!(
            replay.probe.observations["cleanup"]["textedit_process_absent"],
            true
        );
    }
}
