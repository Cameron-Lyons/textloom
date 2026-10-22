//! Windows checks for the Textloom native host, in the `winvm` VM.
//!
//! The VM must be up (`cargo winvm up`). Keys and clicks reach Windows as
//! hardware input from QEMU's PS/2 keyboard and USB tablet; editor state is
//! read through UI Automation. Results accumulate in results.json under the
//! VM's state directory, with screenshots per check.

use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::native_report::{self, Clipboard, Expected, Mode, Os};
use crate::winvm::{Qmp, Vm};
use crate::{Context, Result, bail, repo_root};

pub const USAGE: &str = "\
usage: winvm-checks build                  cross-compile the host, the winit harness, and the report validator for x86_64-pc-windows-gnu
       winvm-checks deploy                 copy them and the guest helpers into the VM
       winvm-checks explore                start the host and print its UI Automation tree
       winvm-checks [--runs N] CHECK...    run checks (\"all\" for every check)
       winvm-checks list

The VM must be up (cargo winvm up). Keys and clicks reach Windows as hardware
input from QEMU's PS/2 keyboard and USB tablet; editor state is read through
UI Automation. Results accumulate in results.json under the VM's state
directory (TLQA_WINDOWS_OUT overrides it), with screenshots per check.";

const TARGET: &str = "x86_64-pc-windows-gnu";
const TEXT_FIELDS: [&str; 3] = ["Document", "Find", "Replace"];
/// The taskbar along the bottom of the 1280x800 screen.
const TASKBAR: Rect = [0, 752, 1280, 48];

/// x, y, width, height in screen pixels.
pub type Rect = [i64; 4];

fn sleep(seconds: f64) {
    thread::sleep(Duration::from_secs_f64(seconds));
}

fn host_exe() -> PathBuf {
    repo_root()
        .join("target")
        .join(TARGET)
        .join("release/textloom-native-example.exe")
}

fn winit_exe() -> PathBuf {
    repo_root()
        .join("target/winit-harness")
        .join(TARGET)
        .join("release/tlqa-winit-harness.exe")
}

fn validator_exe() -> PathBuf {
    repo_root()
        .join("target/qa-tools")
        .join(TARGET)
        .join("release/check-native-report.exe")
}

fn git_revision() -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root())
        .output()?;
    if !output.status.success() {
        bail!("git rev-parse HEAD failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn cargo_build(
    manifest: &Path,
    target_dir: &Path,
    extra: &[&str],
    revision: Option<&str>,
) -> Result<()> {
    let mut command = Command::new("cargo");
    command.args([
        "build",
        "--locked",
        "--release",
        "--target",
        TARGET,
        "--manifest-path",
    ]);
    command
        .arg(manifest)
        .arg("--target-dir")
        .arg(target_dir)
        .args(extra);
    if let Some(revision) = revision {
        command.env("TEXTLOOM_QA_REVISION", revision);
    }
    let status = command.status().context("cargo")?;
    if !status.success() {
        bail!("cargo build of {} failed", manifest.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------- text offsets

/// The UTF-16 offset UI Automation uses for a character index.
pub fn utf16(text: &str, index: usize) -> usize {
    text.chars().take(index).map(char::len_utf16).sum()
}

/// The character index of a UTF-16 offset.
pub fn codepoints(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (index, character) in text.chars().enumerate() {
        if units >= offset {
            return index;
        }
        units += character.len_utf16();
    }
    text.chars().count()
}

/// The number of characters in `text`.
fn length(text: &str) -> usize {
    text.chars().count()
}

/// The characters from `start` up to `end`.
fn slice(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

/// The characters from `start` on.
fn from(text: &str, start: usize) -> String {
    text.chars().skip(start).collect()
}

/// The character index of `needle` at or after character `start`.
fn find(text: &str, needle: &str, start: usize) -> Option<usize> {
    let byte = text
        .char_indices()
        .nth(start)
        .map_or(text.len(), |(byte, _)| byte);
    let found = byte + text.get(byte..)?.find(needle)?;
    Some(length(&text[..found]))
}

fn field<T: DeserializeOwned>(value: &Value, key: &str) -> Result<T> {
    serde_json::from_value(value.get(key).cloned().unwrap_or(Value::Null))
        .context(format!("guest field {key}"))
}

fn text(value: &Value) -> Result<String> {
    field(value, "doc")
}

/// A field of a guest object, or null.
fn take(mut value: Value, key: &str) -> Value {
    value.get_mut(key).map(Value::take).unwrap_or_default()
}

/// A text field's text, selection (as character indexes), focus, and caret box.
#[derive(Clone, Debug)]
struct TextState {
    text: String,
    selection: Option<[usize; 2]>,
    focused: bool,
    caret_box: Option<Rect>,
    rect: Option<Rect>,
}

#[derive(Deserialize)]
struct GuestTextState {
    text: String,
    selection: Option<Vec<usize>>,
    focused: bool,
    caret_box: Option<Vec<i64>>,
    rect: Option<Vec<i64>>,
}

impl TextState {
    fn selection(&self) -> Result<[usize; 2]> {
        self.selection
            .ok_or_else(|| "the text field has no selection".into())
    }
}

#[derive(Clone, Copy)]
enum Caret {
    Start,
    End,
}

pub struct Checks {
    vm: Vm,
    out: PathBuf,
}

impl Checks {
    pub fn from_env() -> Self {
        let vm = Vm::from_env();
        let out =
            env::var_os("TLQA_WINDOWS_OUT").map_or_else(|| vm.state.join("results"), PathBuf::from);
        Self { vm, out }
    }

    /// Runs PowerShell with qa.ps1 loaded; returns the JSON from its last JSON line.
    fn guest(&self, script: &str) -> Result<Value> {
        let result = self
            .vm
            .run(&format!(". C:\\textloom-qa\\qa.ps1\n{script}"), false, 180)?;
        if result.exit != 0 || result.timed_out {
            bail!(
                "guest job failed ({}): {}",
                result.exit,
                crate::tail(&result.output, 3000)
            );
        }
        for line in result.output.lines().rev() {
            if line.trim_start().starts_with(['{', '[']) {
                return Ok(serde_json::from_str(line)?);
            }
        }
        Ok(Value::Null)
    }

    /// Runs one check's session: a host started in the VM, input through QMP,
    /// screenshots under OUT/<name>.
    fn session<T>(&self, name: &str, body: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
        let out = self.out.join(name);
        fs::create_dir_all(&out).context(out.display())?;
        let mut monitor = self.vm.qmp(30.0)?;
        let size = monitor.size()?;
        let mut session = Session {
            c: self,
            out,
            monitor,
            size,
        };
        let result = body(&mut session);
        // Windows switches input methods system-wide; leave US English for the next check.
        let closed = self.guest("Set-InputMethod 'en-US' | Out-Null; Stop-TextloomHost; Stop-WinitHarness; Out-Json @{}");
        let value = result?;
        closed?;
        Ok(value)
    }

    fn record(&self, name: &str, result: &Value) -> Result<()> {
        fs::create_dir_all(&self.out)?;
        let path = self.out.join("results.json");
        let mut results: Map<String, Value> = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Map::new(),
            Err(error) => return Err(error.into()),
        };
        results.insert(name.to_owned(), result.clone());
        let mut bytes = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
        let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, formatter);
        serde::Serialize::serialize(&results, &mut serializer)?;
        fs::write(&path, bytes).context(path.display())
    }
}

struct Session<'c> {
    c: &'c Checks,
    out: PathBuf,
    monitor: Qmp,
    size: (u32, u32),
}

impl<'c> Session<'c> {
    fn guest(&self, script: &str) -> Result<Value> {
        self.c.guest(script)
    }

    fn start(&mut self, arguments: &[&str]) -> Result<i64> {
        // Close the Start menu or any other transient shell UI first.
        self.monitor.chord("esc")?;
        let quoted: Vec<String> = arguments
            .iter()
            .map(|argument| format!("'{}'", argument.replace('\'', "''")))
            .collect();
        let started = self.guest(&format!(
            "$id = Start-TextloomHost @({}); Out-Json @{{ pid = $id }}",
            quoted.join(", ")
        ))?;
        let pid = field(&started, "pid")?;
        sleep(1.5);
        self.input_method("en-US")?;
        Ok(pid)
    }

    fn winit(&mut self, arguments: &[&str]) -> Result<Winit<'c>> {
        self.monitor.chord("esc")?;
        let quoted: Vec<String> = arguments
            .iter()
            .map(|argument| format!("'{argument}'"))
            .collect();
        let started = self.guest(&format!(
            "Out-Json (Start-WinitHarness @({}))",
            quoted.join(", ")
        ))?;
        let origin = field(&started, "origin")?;
        sleep(1.0);
        Ok(Winit { c: self.c, origin })
    }

    fn running(&self) -> Result<bool> {
        field(&self.guest("Out-Json (Get-HostExit)")?, "running")
    }

    fn state_of(&self, name: &str) -> Result<TextState> {
        let state: GuestTextState =
            serde_json::from_value(self.guest(&format!("Out-Json (Get-TextState '{name}')"))?)?;
        let rect = |values: Option<Vec<i64>>| values.and_then(|values| Rect::try_from(values).ok());
        let selection = state
            .selection
            .and_then(|offsets| <[usize; 2]>::try_from(offsets).ok())
            .map(|offsets| offsets.map(|offset| codepoints(&state.text, offset)));
        Ok(TextState {
            selection,
            focused: state.focused,
            caret_box: rect(state.caret_box),
            rect: rect(state.rect),
            text: state.text,
        })
    }

    fn state(&self) -> Result<TextState> {
        self.state_of("Document")
    }

    /// Polls until `predicate` holds; the software renderer can take a few frames to apply input.
    fn wait_state(
        &self,
        timeout: f64,
        predicate: impl Fn(&TextState) -> bool,
    ) -> Result<TextState> {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        loop {
            let state = self.state()?;
            if predicate(&state) || Instant::now() > deadline {
                return Ok(state);
            }
            sleep(0.3);
        }
    }

    fn wait(&self, predicate: impl Fn(&TextState) -> bool) -> Result<TextState> {
        self.wait_state(6.0, predicate)
    }

    fn rect(&self, name: &str, kind: &str) -> Result<Rect> {
        field(
            &self.guest(&format!(
                "Out-Json @{{ rect = Get-ElementRect '{name}' '{kind}' }}"
            ))?,
            "rect",
        )
    }

    fn click_element(&mut self, name: &str, offset: Option<(i64, i64)>, kind: &str) -> Result<()> {
        let kind = if kind.is_empty() && TEXT_FIELDS.contains(&name) {
            "Edit"
        } else {
            kind
        };
        let [x, y, width, height] = self.rect(name, kind)?;
        let (dx, dy) = offset.unwrap_or((width / 2, height / 2));
        self.monitor
            .click(x + dx, y + dy, "left", Some(self.size))?;
        sleep(0.5);
        Ok(())
    }

    fn focus_document(&mut self) -> Result<()> {
        // Near the top-left of the document, inside its first line.
        self.click_element("Document", Some((40, 20)), "")
    }

    /// The toolbar's paragraph style box, which follows the caret's paragraph.
    fn paragraph_style(&self) -> Result<Value> {
        Ok(take(
            self.guest("Out-Json @{ value = Get-ParagraphStyle }")?,
            "value",
        ))
    }

    fn window_rect(&self) -> Result<Rect> {
        field(
            &self.guest(
                "Out-Json @{ rect = Get-Rect (Get-HostWindow).Current.BoundingRectangle }",
            )?,
            "rect",
        )
    }

    /// The status row, which shows "IME composing" while a composition is open.
    fn status_band(&self) -> Result<Rect> {
        let [x, _, width, _] = self.window_rect()?;
        let [_, y, _, height] = self.rect("Ready", "Text")?;
        Ok([x, y - 2, width, height + 4])
    }

    fn kinds(&self) -> Result<Vec<Value>> {
        Ok(listed(self.guest("Out-Json (Get-ParagraphKinds)")?)
            .into_iter()
            .map(|kind| take(kind, "kind"))
            .collect())
    }

    fn status(&self) -> Result<Value> {
        self.guest("Out-Json (Get-Status)")
    }

    fn composing(&self) -> Result<bool> {
        Ok(listed(self.status()?)
            .iter()
            .any(|text| text == "IME composing"))
    }

    /// Selects the nth occurrence of `find`, or puts the caret at its start or end.
    fn select_in(
        &self,
        needle: &str,
        occurrence: usize,
        caret: Option<Caret>,
        field: &str,
    ) -> Result<(usize, usize)> {
        let text = self.state_of(field)?.text;
        let mut start = None;
        for _ in 0..occurrence {
            let after = start.map_or(0, |start| start + 1);
            start = Some(
                find(&text, needle, after)
                    .ok_or_else(|| format!("{needle:?} is not in the {field}"))?,
            );
        }
        let mut start = start.ok_or("no occurrence was requested")?;
        let mut end = start + length(needle);
        match caret {
            Some(Caret::Start) => end = start,
            Some(Caret::End) => start = end,
            None => {}
        }
        self.guest(&format!(
            "Select-HostRange {} {} '{field}'; Out-Json @{{}}",
            utf16(&text, start),
            utf16(&text, end)
        ))?;
        sleep(0.3);
        Ok((start, end))
    }

    fn select(&self, needle: &str, caret: Option<Caret>) -> Result<(usize, usize)> {
        self.select_in(needle, 1, caret, "Document")
    }

    fn keys(&mut self, chords: &[&str]) -> Result<()> {
        for chord in chords {
            self.monitor.chord(chord)?;
            sleep(0.15);
        }
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> Result<()> {
        self.monitor.type_text(text)?;
        sleep(0.3);
        Ok(())
    }

    fn input_method(&self, language: &str) -> Result<String> {
        let layout = field(
            &self.guest(&format!(
                "Out-Json @{{ layout = Set-InputMethod '{language}' }}"
            ))?,
            "layout",
        )?;
        // Keys sent sooner can still be translated with the previous layout.
        sleep(1.5);
        Ok(layout)
    }

    fn toplevels(&self) -> Result<Value> {
        self.guest("Out-Json (Get-Toplevels)")
    }

    fn screenshot(&mut self, label: &str) -> Result<PathBuf> {
        let path = self.out.join(format!("{label}.png"));
        self.monitor.screenshot(&path)
    }
}

/// PowerShell prints a one-item list as the item alone.
fn listed(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        Value::Null => Vec::new(),
        item => vec![item],
    }
}

/// The winit harness: one editor through textloom's winit adapter, drawn as
/// 10x24 px cells from (20, 20) in its client area, logging JSON events.
struct Winit<'c> {
    c: &'c Checks,
    origin: [i64; 2],
}

impl Winit<'_> {
    fn events(&self, after: i64) -> Result<Vec<Value>> {
        field(
            &self.c.guest(&format!(
                "Out-Json @{{ events = @(Get-WinitEvents {after}) }}"
            ))?,
            "events",
        )
    }

    fn state(&self) -> Result<Value> {
        let events = self.events(0)?;
        let last = events
            .into_iter()
            .rev()
            .find_map(|mut event| event.get_mut("state").map(Value::take));
        last.ok_or_else(|| "the winit harness logged no state".into())
    }

    fn last(&self, kind: &str, after: i64) -> Result<Option<Value>> {
        Ok(self
            .events(after)?
            .into_iter()
            .rev()
            .find(|event| event["event"] == kind))
    }

    fn seq(&self) -> Result<i64> {
        Ok(self
            .events(0)?
            .last()
            .and_then(|event| event["seq"].as_i64())
            .unwrap_or(0))
    }

    /// Screen rectangle of a cell, in physical pixels at 100% scale.
    fn cell(&self, line: i64, column: i64) -> Rect {
        [
            self.origin[0] + 20 + column * 10,
            self.origin[1] + 20 + line * 24,
            10,
            24,
        ]
    }

    fn focus(&self, s: &mut Session) -> Result<()> {
        // Below the text, so the click only focuses the window.
        s.monitor.click(
            self.origin[0] + 400,
            self.origin[1] + 300,
            "left",
            Some(s.size),
        )?;
        sleep(0.8);
        Ok(())
    }
}

// ---------------------------------------------------------------------------- setup commands

fn build() -> Result<()> {
    let root = repo_root();
    let revision = git_revision()?;
    let target = root.join("target");
    cargo_build(
        &root.join("examples/native-editor/Cargo.toml"),
        &target,
        &[],
        Some(&revision),
    )?;
    cargo_build(
        &root.join("scripts/windows-vm/winit-harness/Cargo.toml"),
        &target.join("winit-harness"),
        &[],
        None,
    )?;
    let validator = ["--bin", "check-native-report"];
    cargo_build(
        &root.join("scripts/qa-tools/Cargo.toml"),
        &target.join("qa-tools"),
        &validator,
        None,
    )?;
    for path in [host_exe(), winit_exe(), validator_exe()] {
        println!("{}", path.display());
    }
    Ok(())
}

fn deploy(c: &Checks) -> Result<()> {
    for path in [host_exe(), winit_exe(), validator_exe()] {
        if !path.exists() {
            bail!(
                "{} is missing; run `cargo winvm-checks build`",
                path.display()
            );
        }
    }
    for path in [
        host_exe(),
        winit_exe(),
        validator_exe(),
        repo_root().join("scripts/windows-vm/guest/qa.ps1"),
    ] {
        c.vm.share(&path, None)?;
    }
    let result = c.vm.run(
        "$ErrorActionPreference = 'Stop'\n\
         New-Item -ItemType Directory -Force C:\\textloom-qa\\host, C:\\textloom-qa\\winit | Out-Null\n\
         Get-Process textloom-native-example, tlqa-winit-harness -ErrorAction SilentlyContinue | Stop-Process -Force\n\
         Get-HostFile qa.ps1 C:\\textloom-qa\\qa.ps1\n\
         Get-HostFile textloom-native-example.exe C:\\textloom-qa\\host\\textloom-native-example.exe\n\
         Get-HostFile tlqa-winit-harness.exe C:\\textloom-qa\\winit\\tlqa-winit-harness.exe\n\
         Get-HostFile check-native-report.exe C:\\textloom-qa\\check-native-report.exe\n\
         Copy-Item C:\\textloom-qa\\mesa\\*.dll C:\\textloom-qa\\host -Force\n",
        false,
        300,
    )?;
    if result.exit != 0 || result.timed_out {
        bail!(
            "deploying failed ({}): {}",
            result.exit,
            crate::tail(&result.output, 3000)
        );
    }
    println!("Deployed the host, the winit harness, the report validator, and qa.ps1.");
    Ok(())
}

fn pretty(value: &Value) -> Result<String> {
    let mut bytes = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    serde::Serialize::serialize(
        value,
        &mut serde_json::Serializer::with_formatter(&mut bytes, formatter),
    )?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn explore(c: &Checks) -> Result<()> {
    c.session("explore", |s| {
        s.start(&[])?;
        s.focus_document()?;
        s.screenshot("start")?;
        println!("{}", pretty(&s.guest("Out-Json (Get-Tree 6)")?)?);
        let state = s.state()?;
        println!(
            "{}",
            json!({"text": state.text, "selection": state.selection, "focused": state.focused,
                               "caret_box": state.caret_box, "rect": state.rect})
        );
        println!("{}", Value::Array(s.kinds()?));
        println!("{}", s.status()?);
        println!("{}", s.toplevels()?);
        Ok(())
    })
}

// ---------------------------------------------------------------------------- checks

fn all_true(expectations: &Value) -> bool {
    expectations
        .as_object()
        .is_some_and(|map| map.values().all(|value| value == true))
}

fn renderer_smoke(c: &Checks) -> Result<Value> {
    let revision = git_revision()?;
    let mut observed = Map::new();
    for mode in Mode::ALL {
        let name = mode.name();
        let extra = match mode {
            Mode::Editable => "'--clipboard-self-test'",
            Mode::ReadOnly => "'--read-only'",
            Mode::Disabled => "'--disabled'",
        };
        let report = format!("C:\\textloom-qa\\smoke-{name}.json");
        // The host refuses to replace an existing report.
        let result = c.guest(&format!(
            r#"$env:PATH = "C:\msys64\ucrt64\bin;$env:PATH"; $env:GALLIUM_DRIVER = 'llvmpipe'
$env:GLUTIN_WGL_OPENGL_DLL = 'C:\textloom-qa\host\opengl32.dll'
Remove-Item -LiteralPath {report} -ErrorAction SilentlyContinue
$out = & C:\textloom-qa\host\textloom-native-example.exe --smoke-test --rich-clipboard {extra} --qa-report {report} 2>&1 | ForEach-Object {{ "$_" }}
Send-HostFile {report} smoke-{name}.json
Out-Json @{{ exit = $LASTEXITCODE; output = ($out -join "`n") }}"#
        ))?;
        let output = result["output"].as_str().unwrap_or_default();
        let expected = Expected {
            revision: revision.clone(),
            os: Os::Windows,
            mode,
            interaction: false,
            clipboard: Clipboard::Rich,
        };
        let validation =
            native_report::check_file(&c.vm.inbox().join(format!("smoke-{name}.json")), &expected);
        let message = match &validation {
            Ok(_) => expected.summary(),
            Err(error) => format!("Native report validation failed: {error}"),
        };
        observed.insert(
            name.to_owned(),
            json!({
                "rendered 20 frames": output.contains("Rendered 20 native frames"),
                "clipboard roundtrip": mode != Mode::Editable
                    || output.contains("Native rich clipboard roundtrip and undo/redo passed"),
                "report valid": validation.is_ok(),
                "validator": crate::tail(&message, 500),
            }),
        );
    }
    let passed = observed.values().all(|mode| {
        mode.as_object().is_some_and(|mode| {
            mode.iter()
                .all(|(key, value)| key == "validator" || value == true)
        })
    });
    Ok(json!({"passed": passed, "observed": observed}))
}

/// Presses undo until the document text is `text`; returns the number of presses.
fn undo_until(s: &mut Session, text: &str, chord: &str, limit: usize) -> Result<Option<usize>> {
    let mut current = s.state()?.text;
    for steps in 0..=limit {
        if current == text {
            return Ok(Some(steps));
        }
        s.keys(&[chord])?;
        let previous = current;
        current = s.wait_state(3.0, |state| state.text != previous)?.text;
    }
    Ok(None)
}

fn german_dead_keys_and_altgr(c: &Checks) -> Result<Value> {
    c.session("german", |s| {
        s.start(&[])?;
        s.focus_document()?;
        s.select("writing", Some(Caret::Start))?;
        let before = s.state()?;
        let offset = before.selection()?[0];
        let layout = s.input_method("de-DE")?;
        // ´ is a dead key at the US = position. winit 0.30 loses the first dead key
        // after a layout switch (it prepares the new layout on that key press and
        // consumes the key's state; see RELEASING.md), so that is recorded but not
        // required here.
        s.keys(&["equal", "e"])?;
        let first = slice(&s.wait(|state| state.text != before.text)?.text, offset, offset + 1);
        let at = utf16(&before.text, offset);
        s.guest(&format!("Select-HostRange {at} {}; Out-Json @{{}}", at + 1))?;
        s.keys(&["equal", "e"])?;
        let after_dead = s.wait(|state| slice(&state.text, offset, offset + 1) != first || first == "é")?;
        s.keys(&["alt_r+q", "alt_r+e"])?;
        let after_altgr = s.wait(|state| length(&state.text) >= length(&after_dead.text) + 2)?;
        s.screenshot("typed")?;
        // QWERTZ: the physical Y key is Z, and Latin logical keys select shortcuts.
        let undo_steps = undo_until(s, &before.text, "ctrl+y", 6)?;
        let restored = s.state()?;
        // Windows also accepts Ctrl+Alt for AltGr. egui-winit drops typed text while
        // Ctrl is held, so egui never types it, and egui's default quit shortcut,
        // Ctrl+Q matched ignoring Alt, takes this chord and closes the window.
        s.keys(&["ctrl+alt+q"])?;
        sleep(1.0);
        let survived = s.running()?;
        let after_ctrl_alt = if survived { Some(s.state()?) } else { None };
        let expectations = json!({
            "the German layout is active": layout.ends_with("0407"),
            "dead acute + e types é": slice(&after_dead.text, offset, offset + 1) == "é"
                && length(&after_dead.text) == length(&before.text) + 1,
            "AltGr+Q and AltGr+E type @€": slice(&after_altgr.text, offset, offset + 3) == "é@€",
            "Ctrl+physical Y (Z on QWERTZ) undoes the typing": undo_steps.is_some(),
            "undo restores the caret": restored.selection == before.selection,
            "the document keeps focus": after_altgr.focused,
            "Ctrl+Alt+Q leaves the window open": survived,
        });
        Ok(json!({
            "passed": all_true(&expectations), "expectations": expectations, "layout": layout,
            "first dead key after the switch": first, "typed": slice(&after_altgr.text, offset, offset + 3),
            "Ctrl+Alt+Q typed": after_ctrl_alt.map(|state| slice(&state.text, offset, offset + 1)),
            "undo_steps": undo_steps,
        }))
    })
}

const CLIPBOARD: &str = "Out-Json @{ text = (Get-Clipboard -Raw) }";

fn windows_clipboard_keys(c: &Checks) -> Result<Value> {
    c.session("clipboard-keys", |s| {
        s.start(&[])?;
        s.focus_document()?;
        s.select("format", None)?;
        let before = s.state()?;
        let [start, end] = before.selection()?;
        s.keys(&["ctrl+insert"])?;
        sleep(0.5);
        let copied = take(s.guest(CLIPBOARD)?, "text");
        s.keys(&["shift+delete"])?;
        let after_cut = s.wait(|state| state.text != before.text)?;
        let cut = take(s.guest(CLIPBOARD)?, "text");
        s.keys(&["shift+insert"])?;
        let after_paste = s.wait(|state| state.text != after_cut.text)?;
        s.keys(&["ctrl+z"])?;
        let undo_paste = s.wait(|state| state.text != after_paste.text)?.text;
        s.keys(&["ctrl+z"])?;
        let undo_cut = s.wait(|state| state.text != undo_paste)?.text;
        let expectations = json!({
            "Ctrl+Insert copies the selection": copied == "format",
            "Shift+Delete removes the selection":
                after_cut.text == slice(&before.text, 0, start) + &from(&before.text, end),
            "Shift+Delete puts it on the clipboard": cut == "format",
            "Shift+Insert pastes it back": after_paste.text == before.text,
            "one undo removes the paste": undo_paste == after_cut.text,
            "a second undo restores the cut text": undo_cut == before.text,
        });
        let around = |text: &str| slice(text, start.saturating_sub(8), end + 8);
        let observed = json!({
            "after cut": around(&after_cut.text), "after paste": around(&after_paste.text),
            "undo 1": around(&undo_paste), "undo 2": around(&undo_cut),
        });
        Ok(json!({"passed": all_true(&expectations), "expectations": expectations, "observed": observed}))
    })
}

fn azerty_digit_row_shortcuts(c: &Checks) -> Result<Value> {
    c.session("azerty", |s| {
        s.start(&[])?;
        s.focus_document()?;
        let layout = s.input_method("fr-FR")?;
        let mut observed = Map::new();
        for (label, chord) in [
            ("Ctrl+1", "ctrl+1"),
            ("Ctrl+0", "ctrl+0"),
            ("Ctrl+3", "ctrl+3"),
            ("Ctrl+0 again", "ctrl+0"),
            ("Ctrl+Shift+7", "ctrl+shift+7"),
            ("Ctrl+Shift+8", "ctrl+shift+8"),
        ] {
            s.select("Select", Some(Caret::End))?;
            s.keys(&[chord])?;
            sleep(0.8);
            observed.insert(label.to_owned(), s.paragraph_style()?);
        }
        let (start, _) = s.select("Select", Some(Caret::Start))?;
        let before = s.state()?;
        s.keys(&["1", "2"])?;
        let typed = slice(&s.wait(|state| length(&state.text) >= length(&before.text) + 2)?.text, start, start + 2);
        let style = |label: &str| observed[label].clone();
        let expectations = json!({
            "the French layout is active": layout.ends_with("040C"),
            "Ctrl+physical 1 makes Heading 1": style("Ctrl+1") == "Heading 1",
            "Ctrl+physical 0 makes body text": style("Ctrl+0") == "Body",
            "Ctrl+physical 3 makes Heading 3": style("Ctrl+3") == "Heading 3",
            "Ctrl+physical 0 restores body text": style("Ctrl+0 again") == "Body",
            "Ctrl+Shift+physical 7 makes a numbered list": style("Ctrl+Shift+7") == "Numbered list",
            "Ctrl+Shift+physical 8 makes bullets": style("Ctrl+Shift+8") == "Bullets",
            "unmodified digit-row keys type &é": typed == "&é",
        });
        Ok(json!({"passed": all_true(&expectations), "expectations": expectations, "observed": observed, "typed": typed}))
    })
}

// ---------------------------------------------------------------------------- IME

/// Bounding box of what appeared between two screenshots, outside `ignore`.
///
/// IME candidate lists are not top-level windows or UI Automation elements, so
/// they are found as the union of the regions that changed.
pub fn popup_rect(before: &Path, after: &Path, ignore: &[Rect]) -> Result<Option<Rect>> {
    let mut command = Command::new("magick");
    command.arg(before).arg(after).args([
        "-compose",
        "difference",
        "-composite",
        "-colorspace",
        "gray",
        "-threshold",
        "6%",
        "-fill",
        "black",
    ]);
    for [x, y, width, height] in ignore {
        command.args([
            "-draw".to_owned(),
            format!("rectangle {x},{y} {},{}", x + width, y + height),
        ]);
    }
    command.args([
        "-morphology",
        "Close",
        "Disk:6",
        "-define",
        "connected-components:verbose=true",
        "-define",
        "connected-components:area-threshold=400",
        "-connected-components",
        "8",
        "null:",
    ]);
    let output = command.output().context("magick")?;
    if !output.status.success() {
        bail!("magick failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let mut bounds: Option<[i64; 4]> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5
            || !(fields[4].starts_with("gray(255") || fields[4].starts_with("graya(255"))
        {
            continue;
        }
        let numbers: Vec<i64> = fields[1]
            .split(['x', '+'])
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()
            .context(fields[1])?;
        let [width, height, x, y] =
            <[i64; 4]>::try_from(numbers).map_err(|_| format!("bad geometry {}", fields[1]))?;
        let [left, top, right, bottom] = bounds.unwrap_or([x, y, x + width, y + height]);
        bounds = Some([
            left.min(x),
            top.min(y),
            right.max(x + width),
            bottom.max(y + height),
        ]);
    }
    Ok(bounds.map(|[left, top, right, bottom]| [left, top, right - left, bottom - top]))
}

/// The full-width row of a text line, which changes as a composition is typed.
fn line_band(s: &Session, line: Option<Rect>) -> Result<Rect> {
    let [x, _, width, _] = s.window_rect()?;
    let [_, y, _, height] = line.ok_or("the caret has no box")?;
    Ok([x, y - 4, width, height + 8])
}

/// A panel starts near the composition and sits just below or above its line without covering it.
pub fn beside(panel: Option<Rect>, line: Option<Rect>) -> bool {
    let (Some([x, y, _, height]), Some([line_x, line_y, _, line_height])) = (panel, line) else {
        return false;
    };
    let slack = 40;
    let below = (-3..=slack).contains(&(y - (line_y + line_height)));
    let above = (-3..=slack).contains(&(line_y - (y + height)));
    (line_x - slack..=line_x + slack).contains(&x) && (below || above)
}

pub fn on_screen(panel: Option<Rect>) -> bool {
    panel.is_some_and(|[x, y, width, height]| {
        x >= 0 && y >= 0 && x + width <= 1280 && y + height <= TASKBAR[1]
    })
}

fn wait_composing(s: &Session, composing: bool, timeout: f64) -> Result<bool> {
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    loop {
        let state = s.composing()?;
        if state == composing || Instant::now() > deadline {
            return Ok(state);
        }
        sleep(0.3);
    }
}

/// Switches to an IME and makes sure it composes: a probe key either composes
/// (then Escape cancels it) or types a letter (then Backspace removes it and
/// `toggle` switches the IME's input mode).
fn ime_on(s: &mut Session, language: &str, toggle: &str) -> Result<String> {
    let layout = s.input_method(language)?;
    for _ in 0..3 {
        s.keys(&["n"])?;
        if wait_composing(s, true, 2.0)? {
            s.keys(&["esc"])?;
            wait_composing(s, false, 4.0)?;
            return Ok(layout);
        }
        s.keys(&["backspace"])?;
        sleep(0.4);
        s.keys(&[toggle])?;
        sleep(0.8);
    }
    bail!("the {language} IME did not start composing")
}

fn japanese_on(s: &mut Session) -> Result<String> {
    // Alt+` turns the Microsoft Japanese IME's kana input on and off on a US keyboard.
    ime_on(s, "ja-JP", "alt+grave_accent")
}

fn pinyin_on(s: &mut Session) -> Result<String> {
    // Shift switches Microsoft Pinyin between Chinese and English input.
    ime_on(s, "zh-CN", "shift")
}

/// Converts the typed reading, opens the candidate list, and returns its box.
fn open_candidates(s: &mut Session, ignore: &[Rect]) -> Result<Option<Rect>> {
    s.keys(&["spc"])?;
    sleep(1.0);
    let converted = s.screenshot("converted")?;
    s.keys(&["spc"])?;
    sleep(1.5);
    let listed = s.screenshot("candidates")?;
    popup_rect(&converted, &listed, ignore)
}

fn japanese_ime_replacement(c: &Checks) -> Result<Value> {
    c.session("japanese", |s| {
        s.start(&[])?;
        s.focus_document()?;
        japanese_on(s)?;
        let (start, end) = s.select("format", None)?;
        let before = s.state()?;
        let line = before.caret_box;
        let ignore = [TASKBAR, line_band(s, line)?, s.status_band()?];
        let base = s.screenshot("base")?;
        s.type_text("nihongo")?;
        let composing = wait_composing(s, true, 4.0)?;
        let during = s.state()?;
        let preedit = s.screenshot("preedit")?;
        let suggestions = popup_rect(&base, &preedit, &ignore)?;
        let candidates = open_candidates(s, &ignore)?;
        // The second Space moved to candidate 2; go back to the first and commit it.
        s.keys(&["up"])?;
        sleep(0.5);
        s.keys(&["ret"])?;
        let committed = s.wait(|state| state.text != before.text)?;
        let still_composing = wait_composing(s, false, 4.0)?;
        s.keys(&["ctrl+z"])?;
        let undone = s.wait(|state| state.text == before.text)?;
        s.keys(&["ctrl+y"])?;
        let redone = s.wait(|state| state.text != before.text)?;
        let expected = slice(&before.text, 0, start) + "日本語" + &from(&before.text, end);
        let expectations = json!({
            "typing composes": composing,
            "the suggestion list opens beside the composition": beside(suggestions, line),
            "the candidate list opens beside the composition": beside(candidates, line),
            "the candidate list stays on screen": on_screen(candidates),
            "Enter commits 日本語 over the selection once": committed.text == expected,
            "the caret follows the committed text": committed.selection == Some([start + 3, start + 3]),
            "the composition ends": !still_composing,
            "one undo restores the word and its selection":
                undone.text == before.text && undone.selection == Some([start, end]),
            "redo reapplies the commit": redone.text == expected,
        });
        Ok(json!({
            "passed": all_true(&expectations), "expectations": expectations, "composition line": line,
            "suggestions": suggestions, "candidates": candidates,
            "accessible text while composing": slice(&during.text, start, start + 8),
            "accessible caret while composing": during.caret_box,
        }))
    })
}

fn chinese_pinyin(c: &Checks) -> Result<Value> {
    c.session("pinyin", |s| {
        s.start(&[])?;
        s.focus_document()?;
        pinyin_on(s)?;
        let (start, _) = s.select("writing", Some(Caret::Start))?;
        let before = s.state()?;
        let line = before.caret_box;
        let ignore = [TASKBAR, line_band(s, line)?, s.status_band()?];
        let base = s.screenshot("base")?;
        s.type_text("nihao")?;
        let composing = wait_composing(s, true, 4.0)?;
        sleep(0.8);
        let listed = s.screenshot("candidates")?;
        let candidates = popup_rect(&base, &listed, &ignore)?;
        s.keys(&["spc"])?;
        let committed = s.wait(|state| state.text != before.text)?;
        let still_composing = wait_composing(s, false, 4.0)?;
        s.keys(&["ctrl+z"])?;
        let undone = s.wait(|state| state.text == before.text)?;
        let expected = slice(&before.text, 0, start) + "你好" + &from(&before.text, start);
        let expectations = json!({
            "typing composes": composing,
            "the candidate list opens beside the composition": beside(candidates, line),
            "the candidate list stays on screen": on_screen(candidates),
            "Space commits 你好 at the caret once": committed.text == expected,
            "the composition ends": !still_composing,
            "one undo removes it": undone.text == before.text && undone.selection == Some([start, start]),
        });
        Ok(json!({
            "passed": all_true(&expectations), "expectations": expectations, "composition line": line,
            "candidates": candidates,
        }))
    })
}

fn ime_commit_once(c: &Checks) -> Result<Value> {
    let scenarios = ["click in the document", "click in Find", "Escape"];
    let mut outcomes = Map::new();
    c.session("commit-once", |s| {
        for scenario in scenarios {
            s.start(&[])?;
            s.focus_document()?;
            japanese_on(s)?;
            let (start, _) = s.select("writing", Some(Caret::Start))?;
            let before = s.state()?;
            s.type_text("nihongo")?;
            let composing = wait_composing(s, true, 4.0)?;
            match scenario {
                "click in the document" => {
                    let [_, _, width, height] = before.rect.ok_or("the document has no bounds")?;
                    s.click_element("Document", Some((width / 2, height - 30)), "")?;
                }
                "click in Find" => s.click_element("Find", None, "")?,
                _ => s.keys(&["esc"])?,
            }
            sleep(1.2);
            let after = s.state()?;
            s.screenshot(&scenario.replace(' ', "-"))?;
            let count = |text: &str| text.matches("にほんご").count() as i64;
            let mut outcome = json!({
                "composing before": composing,
                "insertions": count(&after.text) - count(&before.text),
                "at the original caret": slice(&after.text, start, start + 4) == "にほんご",
                "still composing": s.composing()?,
            });
            if scenario == "click in Find" {
                outcome["Find text"] = s.state_of("Find")?.text.into();
            }
            outcomes.insert(scenario.to_owned(), outcome);
        }
        Ok(())
    })?;
    let [document, find, escape] = scenarios.map(|name| &outcomes[name]);
    let expectations = json!({
        "every scenario composed": outcomes.values().all(|outcome| outcome["composing before"] == true),
        "a click in the document commits the preedit once where it was typed":
            document["insertions"] == 1 && document["at the original caret"] == true,
        "a click in Find commits it once in the document":
            find["insertions"] == 1 && find["at the original caret"] == true && find["Find text"] == "",
        "Escape cancels the composition": escape["insertions"] == 0,
        "no composition is left open": !outcomes.values().any(|outcome| outcome["still composing"] == true),
    });
    Ok(
        json!({"passed": all_true(&expectations), "expectations": expectations, "outcomes": outcomes}),
    )
}

fn ime_long_document(c: &Checks) -> Result<Value> {
    c.session("long-document", |s| {
        s.start(&[])?;
        s.focus_document()?;
        s.guest(r#"Set-Clipboard -Value ((1..300 | ForEach-Object { "Line $_ of a long document" }) -join "`r`n"); Out-Json @{}"#)?;
        let original = s.state()?.text;
        s.keys(&["ctrl+end", "ret", "ctrl+v"])?;
        let pasted = s.wait_state(15.0, |state| state.text.contains("Line 300 of a long document"))?;
        s.keys(&["ctrl+end"])?;
        sleep(1.0);
        japanese_on(s)?;
        let before = s.state()?;
        let line = before.caret_box;
        // The document element spans all 300 lines; the visible part lies between
        // its label and the bottom of the window.
        let window = s.window_rect()?;
        let label = s.rect("Document", "Text")?;
        let label_bottom = label[1] + label[3];
        let visible = [window[0], label_bottom, window[2], (window[1] + window[3]).min(TASKBAR[1]) - label_bottom];
        let ignore = [TASKBAR, line_band(s, line)?, s.status_band()?];
        s.type_text("nihongo")?;
        let composing = wait_composing(s, true, 4.0)?;
        let candidates = open_candidates(s, &ignore)?;
        s.keys(&["up"])?;
        sleep(0.5);
        s.keys(&["ret"])?;
        let committed = s.wait(|state| state.text != before.text)?;
        let expectations = json!({
            "300 lines were pasted": pasted.text.matches("of a long document").count() == 300 && pasted.text != original,
            "the caret line is visible": line.is_some_and(|[_, y, _, height]| {
                visible[1] <= y && y + height <= visible[1] + visible[3]
            }),
            "typing composes": composing,
            "the candidate list opens beside the composition": beside(candidates, line),
            "the candidate list stays on screen": on_screen(candidates),
            "the commit lands at the end": committed.text.ends_with("日本語"),
        });
        Ok(json!({
            "passed": all_true(&expectations), "expectations": expectations, "caret line": line,
            "visible document": visible, "candidates": candidates,
        }))
    })
}

// ---------------------------------------------------------------------------- screen reader

/// Lets any process take the foreground once, as on a CI runner.
///
/// Windows lets only the foreground process grant that, so a visible helper is
/// clicked through QMP (real input brings it forward) and then calls
/// AllowSetForegroundWindow(ASFW_ANY) until it is stopped.
fn grant_foreground(c: &Checks, monitor: &mut Qmp, size: (u32, u32)) -> Result<()> {
    let helper = c.guest(
        r#"$code = @'
Add-Type -Namespace TextloomFocus -Name Native -MemberDefinition '[DllImport("user32.dll")] public static extern bool AllowSetForegroundWindow(int process);'
$host.UI.RawUI.WindowTitle = 'Textloom QA focus helper'
while ($true) { [TextloomFocus.Native]::AllowSetForegroundWindow(-1) | Out-Null; Start-Sleep -Milliseconds 100 }
'@
$process = Start-Process powershell.exe -PassThru -ArgumentList ('-NoProfile -EncodedCommand ' + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code)))
Set-Content C:\textloom-qa\focus-helper.pid $process.Id
$deadline = [DateTime]::UtcNow.AddSeconds(15)
do { Start-Sleep -Milliseconds 300; $window = @(Get-Toplevels | Where-Object { $_.title -eq 'Textloom QA focus helper' })[0] }
until ($window -or [DateTime]::UtcNow -gt $deadline)
Out-Json @{ rect = $window.rect }"#,
    )?;
    let [x, y, width, _]: Rect = field(&helper, "rect")?;
    monitor.click(x + width / 2, y + 12, "left", Some(size))?;
    sleep(1.0);
    Ok(())
}

fn stop_foreground_grant(c: &Checks) -> Result<()> {
    c.guest(
        "Get-Content C:\\textloom-qa\\focus-helper.pid -ErrorAction SilentlyContinue | ForEach-Object { \
         Stop-Process -Id $_ -Force -ErrorAction SilentlyContinue }; Out-Json @{}",
    )
    .map(drop)
}

fn read_json_file(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).context(path.display())?;
    Ok(serde_json::from_slice(
        bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes),
    )?)
}

fn nvda_reader_probe(c: &Checks) -> Result<Value> {
    let revision = git_revision()?;
    let root = repo_root();
    for path in [
        "scripts/native-qa/windows_reader.ps1",
        "scripts/native-qa/nvda/textloomCapture.py",
    ] {
        c.vm.share(&root.join(path), None)?;
    }
    for name in ["windows-reader-evidence.json", "windows-reader-native.json"] {
        let _ = fs::remove_file(c.vm.inbox().join(name));
    }
    {
        let mut monitor = c.vm.qmp(30.0)?;
        let size = monitor.size()?;
        grant_foreground(c, &mut monitor, size)?;
    }
    // The probe runs from a copy of the repository layout it expects: it loads
    // its NVDA plugin from next to itself. It refuses to run outside a
    // disposable CI runner; this VM is restored from a snapshot.
    let result = c.vm.run(
        &format!(
            r#"$root = 'C:\textloom-qa\repo'
Remove-Item -Recurse -Force $root, C:\textloom-qa\reader -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$root\scripts\native-qa\nvda" | Out-Null
Get-HostFile windows_reader.ps1 "$root\scripts\native-qa\windows_reader.ps1"
Get-HostFile textloomCapture.py "$root\scripts\native-qa\nvda\textloomCapture.py"
Set-Location $root
$env:GITHUB_ACTIONS = 'true'; $env:RUNNER_OS = 'Windows'
$env:TEXTLOOM_QA_REVISION = '{revision}'
$env:PATH = "C:\msys64\ucrt64\bin;$env:PATH"; $env:GALLIUM_DRIVER = 'llvmpipe'; $env:LIBGL_ALWAYS_SOFTWARE = 'true'
$env:GLUTIN_WGL_OPENGL_DLL = 'C:\textloom-qa\host\opengl32.dll'
& 'C:\Program Files\PowerShell\7\pwsh.exe' -NoProfile -File scripts\native-qa\windows_reader.ps1 -NativeExecutable C:\textloom-qa\host\textloom-native-example.exe -ReportValidator C:\textloom-qa\check-native-report.exe -OutputDirectory C:\textloom-qa\reader 2>&1 | ForEach-Object {{ "$_" }}
$code = $LASTEXITCODE
foreach ($name in 'windows-reader-evidence.json', 'windows-reader-native.json') {{
    if (Test-Path "C:\textloom-qa\reader\$name") {{ Send-HostFile "C:\textloom-qa\reader\$name" $name }} }}
exit $code"#
        ),
        false,
        420,
    );
    let stopped = stop_foreground_grant(c);
    let result = result?;
    stopped?;
    let log_directory = c.out.join("nvda-reader-probe");
    fs::create_dir_all(&log_directory)?;
    let log = log_directory.join("probe.log");
    fs::write(&log, &result.output)?;
    let evidence_path = c.vm.inbox().join("windows-reader-evidence.json");
    let evidence = if evidence_path.exists() {
        read_json_file(&evidence_path)?
    } else {
        json!({})
    };
    let checks = evidence["checks"].as_object().cloned().unwrap_or_default();
    // After NVDA exits, a desktop process can briefly keep NVDA's IAccessible2 proxy
    // loaded, so deleting the probe's temporary NVDA copy fails. That is recorded but
    // does not decide the result, which the evidence written before it does.
    let temporary_copy_locked = result.output.contains("IAccessible2proxy.dll' is denied");
    let expectations = json!({
        "the probe finished": !result.timed_out && (result.exit == 0 || temporary_copy_locked),
        "the probe's evidence says it succeeded": evidence["succeeded"] == true,
        "all 15 probe checks passed": checks.len() == 15 && checks.values().all(|value| value == true),
        "owned processes were cleaned up": evidence["cleanup_succeeded"] == true,
    });
    let failed: Vec<&String> = checks
        .iter()
        .filter(|(_, value)| *value != true)
        .map(|(name, _)| name)
        .collect();
    Ok(json!({
        "passed": all_true(&expectations), "expectations": expectations,
        "failed probe checks": failed,
        "temporary NVDA copy left behind": temporary_copy_locked,
        "probe checks": checks.len(), "log": log,
    }))
}

// ---------------------------------------------------------------------------- winit adapter

fn winit_windows_clipboard_keys(c: &Checks) -> Result<Value> {
    c.session("winit-clipboard", |s| {
        let w = s.winit(&[])?;
        w.focus(s)?;
        let fixture = text(&w.state()?)?;
        let start = w.seq()?;
        s.keys(&["ctrl+a", "ctrl+insert"])?;
        sleep(1.5);
        let copy = w.last("clipboard_write", start)?;
        let copied = s.guest(CLIPBOARD)?["text"].as_str().unwrap_or_default().replace("\r\n", "\n");
        let start = w.seq()?;
        s.keys(&["shift+delete"])?;
        sleep(1.5);
        let cut = w.last("clipboard_write", start)?;
        let after_cut = text(&w.state()?)?;
        let start = w.seq()?;
        s.keys(&["shift+insert"])?;
        sleep(3.0);
        let paste = w.last("paste", start)?;
        let after_paste = text(&w.state()?)?;
        s.keys(&["ctrl+z"])?;
        sleep(1.0);
        let undone = text(&w.state()?)?;
        let wrote = |event: &Option<Value>, action: &str| {
            event.as_ref().is_some_and(|event| event["action"] == action && event["text"] == fixture.as_str())
        };
        let expectations = json!({
            "Ctrl+Insert copies the whole document": wrote(&copy, "copy"),
            "the Windows clipboard holds it": copied == fixture,
            "Shift+Delete cuts it": wrote(&cut, "cut") && after_cut.is_empty(),
            "Shift+Insert requests a paste that restores it once": paste.is_some() && after_paste == fixture,
            "one undo removes the paste": undone.is_empty(),
        });
        Ok(json!({
            "passed": all_true(&expectations), "expectations": expectations,
            "pasted clipboard text": paste.map(|paste| take(paste, "clipboard_text")),
        }))
    })
}

fn winit_altgr_ctrl_alt(c: &Checks) -> Result<Value> {
    c.session("winit-altgr", |s| {
        let w = s.winit(&["--caret-end"])?;
        w.focus(s)?;
        let layout = s.input_method("de-DE")?;
        let before = text(&w.state()?)?;
        s.keys(&["alt_r+q", "ctrl+alt+q", "alt_r+e"])?;
        sleep(1.0);
        let after = text(&w.state()?)?;
        let expectations = json!({
            "the German layout is active": layout.ends_with("0407"),
            "AltGr+Q, Ctrl+Alt+Q, and AltGr+E type @@€": after == before.clone() + "@@€",
        });
        Ok(json!({"passed": all_true(&expectations), "expectations": expectations, "typed": from(&after, length(&before))}))
    })
}

fn winit_russian_shortcuts(c: &Checks) -> Result<Value> {
    c.session("winit-russian", |s| {
        let w = s.winit(&["--caret-end"])?;
        w.focus(s)?;
        let layout = s.input_method("ru-RU")?;
        let fixture = text(&w.state()?)?;
        let start = w.seq()?;
        s.keys(&["ctrl+a"])?;
        sleep(0.6);
        let mut selected: Vec<i64> = field(&w.state()?, "selection")?;
        selected.sort_unstable();
        s.keys(&["ctrl+c"])?;
        sleep(1.5);
        let copy = w.last("clipboard_write", start)?;
        s.keys(&["ctrl+end"])?;
        sleep(0.4);
        s.keys(&["ctrl+v"])?;
        sleep(3.0);
        let pasted = text(&w.state()?)?;
        s.keys(&["ctrl+z"])?;
        sleep(1.0);
        let undone = text(&w.state()?)?;
        s.keys(&["c"])?;
        sleep(0.6);
        let typed = from(&text(&w.state()?)?, length(&undone));
        let expectations = json!({
            "the Russian layout is active": layout.ends_with("0419"),
            "Ctrl+physical A (ф) selects all": selected == [0, length(&fixture) as i64],
            "Ctrl+physical C (с) copies the document": copy.is_some_and(|copy| copy["text"] == fixture.as_str()),
            "Ctrl+physical V (м) pastes it once": pasted == fixture.repeat(2),
            "Ctrl+physical Z (я) undoes the paste in one step": undone == fixture,
            "physical C types с": typed == "с",
        });
        Ok(json!({"passed": all_true(&expectations), "expectations": expectations, "typed": typed}))
    })
}

fn winit_ime_first_composition(c: &Checks) -> Result<Value> {
    c.session("winit-ime", |s| {
        let w = s.winit(&["--caret-end"])?;
        w.focus(s)?;
        s.input_method("ja-JP")?;
        let before = w.state()?;
        let synced = |events: Vec<Value>| -> Vec<Value> {
            events.into_iter().filter(|event| event["event"] == "sync_ime").collect()
        };
        let caret: Vec<f64> = synced(w.events(0)?)
            .last()
            .map(|event| field(event, "painted_caret"))
            .ok_or("the winit harness synced no IME area")??;
        let [caret_x, caret_y, ..] = caret[..] else {
            bail!("the painted caret has no position");
        };
        let line = w.cell(((caret_y - 20.0) / 24.0).round_ties_even() as i64, ((caret_x - 20.0) / 10.0).round_ties_even() as i64);
        let ignore = [TASKBAR, [w.origin[0], line[1] - 4, 800, line[3] + 8]];
        // The IME starts in direct input; if a probe key types a letter, Alt+` turns kana input on.
        s.keys(&["n"])?;
        sleep(0.8);
        if !truthy(&w.state()?["composition"]) {
            s.keys(&["backspace", "alt+grave_accent"])?;
            sleep(0.8);
            s.keys(&["n"])?;
            sleep(0.8);
        }
        s.type_text("ihongo")?;
        sleep(0.8);
        let composing = truthy(&w.state()?["composition"]);
        let candidates = open_candidates(s, &ignore)?;
        let areas: Vec<Value> = synced(w.events(0)?).into_iter().map(|event| take(event, "rect")).collect();
        s.keys(&["up"])?;
        sleep(0.5);
        s.keys(&["ret"])?;
        sleep(1.0);
        let committed = text(&w.state()?)?;
        let expectations = json!({
            "typing composes": composing,
            "the IME area follows the caret": areas.last().and_then(|area| area[1].as_f64()) == Some(caret_y),
            "the candidate list opens beside the caret": beside(candidates, Some(line)),
            "the candidate list stays on screen": on_screen(candidates),
            "Enter commits 日本語 at the caret": committed == text(&before)? + "日本語",
        });
        Ok(json!({
            "passed": all_true(&expectations), "expectations": expectations, "caret line": line,
            "candidates": candidates,
        }))
    })
}

/// Whether a logged value is set: not null, false, zero, or empty.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

const NVDA_PROFILE: &str = "schemaVersion = 24
[general]
language = en
saveConfigurationOnExit = False
askToExit = False
playStartAndExitSounds = False
showWelcomeDialogAtStartup = False
[update]
autoCheck = False
startupNotification = False
allowUsageStats = False
askedAllowUsageStats = True
[addonStore]
automaticUpdates = disabled
";

/// Bytes in a second of 44.1 kHz 16-bit stereo.
const SECOND: u64 = 176_400;

/// Seconds of 16-bit stereo PCM whose RMS over 20 ms windows exceeds the noise floor.
pub fn audible_seconds(data: &[u8]) -> f64 {
    let (rate, channels, window, threshold) = (44_100.0, 2.0, 0.02, 500.0);
    let samples: Vec<f64> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| f64::from(i16::from_le_bytes(pair)))
        .collect();
    let step = ((rate * window) as usize) * channels as usize;
    let loud = samples
        .chunks_exact(step)
        .filter(|chunk| {
            (chunk.iter().map(|value| value * value).sum::<f64>() / chunk.len() as f64).sqrt()
                > threshold
        })
        .count();
    (loud as f64 * window * 100.0).round() / 100.0
}

fn write_wav(path: &Path, data: &[u8]) -> Result<()> {
    let data = &data[..data.len() - data.len() % 4];
    let length = u32::try_from(data.len()).map_err(|_| "the clip is too long")?;
    let mut file = File::create(path).context(path.display())?;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + length).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&2u16.to_le_bytes())?; // channels
    file.write_all(&44_100u32.to_le_bytes())?;
    file.write_all(&(SECOND as u32).to_le_bytes())?;
    file.write_all(&4u16.to_le_bytes())?; // block alignment
    file.write_all(&16u16.to_le_bytes())?; // bits per sample
    file.write_all(b"data")?;
    file.write_all(&length.to_le_bytes())?;
    file.write_all(data)?;
    Ok(())
}

fn read_at(file: &mut File, offset: u64, length: u64) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    file.seek(SeekFrom::Start(offset))?;
    file.take(length).read_to_end(&mut data)?;
    Ok(data)
}

fn nvda_audible_speech(c: &Checks) -> Result<Value> {
    let recording = c.vm.audio();
    if !recording.exists() {
        bail!(
            "guest audio is not recorded; start the VM with `cargo winvm up` (the default --audio wav)"
        );
    }
    let (mark, end, caret) = c.session("nvda-audible", |s| {
        s.start(&[])?;
        s.focus_document()?;
        s.select("TextLoom", Some(Caret::Start))?;
        s.guest(&format!(
            "New-Item -ItemType Directory -Force C:\\textloom-qa\\nvda-audible | Out-Null\n\
             Set-Content -LiteralPath C:\\textloom-qa\\nvda-audible\\nvda.ini -Encoding ascii -Value @'\n{NVDA_PROFILE}'@\n\
             Start-Process \"$env:ProgramFiles\\NVDA\\nvda.exe\" -ArgumentList '--config-path=C:\\textloom-qa\\nvda-audible', '--lang=en'\n\
             Start-Sleep -Seconds 8; Out-Json @{{ running = [bool](Get-Process nvda -ErrorAction SilentlyContinue) }}"
        ))?;
        // A real click refocuses the editor, so NVDA tracks it.
        s.focus_document()?;
        s.select("TextLoom", Some(Caret::Start))?;
        sleep(3.0);
        let mark = fs::metadata(&recording)?.len();
        s.keys(&["down"])?;
        sleep(6.0);
        let end = fs::metadata(&recording)?.len();
        let caret = s.state()?.selection;
        s.guest(
            "Start-Process \"$env:ProgramFiles\\NVDA\\nvda.exe\" -ArgumentList '-q'; Start-Sleep -Seconds 3\n\
             Get-Process nvda -ErrorAction SilentlyContinue | Stop-Process -Force; Out-Json @{}",
        )?;
        Ok((mark, end, caret))
    })?;
    let mut file = File::open(&recording)?;
    // Frames are 4 bytes after the 44-byte header.
    let quiet_start = mark.saturating_sub(3 * SECOND).max(44);
    let quiet = read_at(
        &mut file,
        quiet_start - (quiet_start - 44) % 4,
        (3 * SECOND).min(mark.saturating_sub(44)),
    )?;
    let data = read_at(
        &mut file,
        mark - mark.saturating_sub(44) % 4,
        end.saturating_sub(mark),
    )?;
    let clip = c.out.join("nvda-audible/speech-after-down.wav");
    write_wav(&clip, &data)?;
    let seconds = audible_seconds(&data);
    let baseline = audible_seconds(&quiet);
    let expectations = json!({
        "Down moved the caret to the next line": caret.is_some_and(|[start, _]| start > 0),
        "the sound card was quiet before the key": baseline < 0.3,
        "the sound card played at least a second of speech after it": seconds >= 1.0,
    });
    Ok(json!({
        "passed": all_true(&expectations), "expectations": expectations, "audible seconds": seconds,
        "audible seconds before the key": baseline,
        "recorded seconds": (data.len() as f64 / SECOND as f64 * 100.0).round() / 100.0, "clip": clip,
        "note": "Listen to the clip to confirm it says the line; the check only measures sound.",
    }))
}

// ---------------------------------------------------------------------------- running

struct Check {
    name: &'static str,
    description: &'static str,
    run: fn(&Checks) -> Result<Value>,
}

const CHECKS: &[Check] = &[
    Check {
        name: "renderer-smoke",
        description: "The host renders 20 frames with Mesa in each mode, as the hosted Mesa job does.",
        run: renderer_smoke,
    },
    Check {
        name: "german-dead-keys-and-altgr",
        description: "German layout: dead acute + e, AltGr+Q/E, Ctrl+Alt+Q (Windows' AltGr), and undo with QWERTZ Ctrl+Z.",
        run: german_dead_keys_and_altgr,
    },
    Check {
        name: "windows-clipboard-keys",
        description: "Ctrl+Insert copies, Shift+Delete cuts, and Shift+Insert pastes, with exact text and undo.",
        run: windows_clipboard_keys,
    },
    Check {
        name: "azerty-digit-row-shortcuts",
        description: "French AZERTY: Ctrl+physical 1/3/0 and Ctrl+Shift+physical 7/8 change the paragraph kind; plain keys type &é.",
        run: azerty_digit_row_shortcuts,
    },
    Check {
        name: "japanese-ime-replacement",
        description: "Microsoft Japanese IME replaces a selected word: suggestion and candidate lists beside it, one commit, undo/redo.",
        run: japanese_ime_replacement,
    },
    Check {
        name: "chinese-pinyin",
        description: "Microsoft Pinyin: nihao shows candidates beside the composition; Space commits 你好 at the caret; undo.",
        run: chinese_pinyin,
    },
    Check {
        name: "ime-commit-once",
        description: "Mid-composition, a click in the document or in Find commits the Japanese preedit once; Escape cancels it.",
        run: ime_commit_once,
    },
    Check {
        name: "ime-long-document",
        description: "Composing on the last line of a 300-line document keeps the caret visible and the candidate list beside it.",
        run: ime_long_document,
    },
    Check {
        name: "nvda-reader-probe",
        description: "The hosted NVDA 2026.2 reader probe (scripts/native-qa/windows_reader.ps1), run in the VM with PowerShell 7.",
        run: nvda_reader_probe,
    },
    Check {
        name: "winit-windows-clipboard-keys",
        description: "winit adapter: Ctrl+A then Ctrl+Insert copies, Shift+Delete cuts, Shift+Insert pastes; one undo removes the paste.",
        run: winit_windows_clipboard_keys,
    },
    Check {
        name: "winit-altgr-ctrl-alt",
        description: "winit adapter, German layout: AltGr+Q, Ctrl+Alt+Q, and AltGr+E type @@€ (Ctrl+Alt is AltGr on Windows).",
        run: winit_altgr_ctrl_alt,
    },
    Check {
        name: "winit-russian-shortcuts",
        description: "winit adapter, Russian layout: Ctrl with physical A, C, V, Z selects all, copies, pastes once, and undoes; plain C types с.",
        run: winit_russian_shortcuts,
    },
    Check {
        name: "winit-ime-first-composition",
        description: "winit adapter: the first Japanese composition after launch has the caret's IME area; candidates open beside it.",
        run: winit_ime_first_composition,
    },
    Check {
        name: "nvda-audible-speech",
        description: "Installed NVDA with its default voice reads the next line after Down; the VM's sound card plays the speech.",
        run: nvda_audible_speech,
    },
];

fn run_checks(c: &Checks, checks: &[&Check], runs: usize) -> Result<()> {
    for run in 1..=runs {
        for check in checks {
            let label = if runs == 1 {
                check.name.to_owned()
            } else {
                format!("{}#run{run}", check.name)
            };
            println!("== {label}");
            let started = Instant::now();
            let mut result = match panic::catch_unwind(AssertUnwindSafe(|| (check.run)(c))) {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => json!({"passed": false, "error": error.to_string()}),
                Err(_) => {
                    json!({"passed": false, "error": "the check panicked; see the message above"})
                }
            };
            result["seconds"] = ((started.elapsed().as_secs_f64() * 10.0).round() / 10.0).into();
            c.record(&label, &result)?;
            println!(
                "{}",
                result.to_string().chars().take(3000).collect::<String>()
            );
        }
    }
    Ok(())
}

fn dispatch(arguments: Vec<String>) -> Result<(), String> {
    let mut arguments = crate::cli::Arguments::new(arguments);
    let runs = match arguments.option("--runs")? {
        Some(runs) => runs
            .parse()
            .map_err(|_| "--runs takes a whole number".to_owned())?,
        None => 1,
    };
    let names = arguments.finish()?;
    let c = Checks::from_env();
    let outcome = match names
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["build"] => build(),
        ["deploy"] => deploy(&c),
        ["explore"] => explore(&c),
        ["list"] => {
            for check in CHECKS {
                println!("{}: {}", check.name, check.description);
            }
            Ok(())
        }
        [] => {
            println!("{USAGE}");
            Ok(())
        }
        names => {
            let selected: Vec<&Check> = if names == ["all"] {
                CHECKS.iter().collect()
            } else {
                let unknown: Vec<&str> = names
                    .iter()
                    .copied()
                    .filter(|name| !CHECKS.iter().any(|check| check.name == *name))
                    .collect();
                if !unknown.is_empty() {
                    return Err(format!("unknown checks: {}", unknown.join(", ")));
                }
                names
                    .iter()
                    .filter_map(|name| CHECKS.iter().find(|check| check.name == *name))
                    .collect()
            };
            run_checks(&c, &selected, runs)
        }
    };
    outcome.map_err(|error| error.to_string())
}

/// The `winvm-checks` command.
pub fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match dispatch(arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("winvm-checks: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TempDir;

    const TEXT: &str =
        "Unicode: café, \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}.\nLists";

    #[test]
    fn utf16_offsets_count_surrogate_pairs() {
        let lists = find(TEXT, "Lists", 0).unwrap();
        assert_eq!(utf16(TEXT, lists), lists + 4);
        assert_eq!(codepoints(TEXT, utf16(TEXT, lists)), lists);
    }

    #[test]
    fn offsets_round_trip_at_every_index() {
        for index in 0..=length(TEXT) {
            assert_eq!(codepoints(TEXT, utf16(TEXT, index)), index);
        }
    }

    #[test]
    fn character_slices_and_searches() {
        assert_eq!(slice(TEXT, 12, 14), "é,");
        assert_eq!(from(TEXT, length(TEXT) - 5), "Lists");
        assert_eq!(find(TEXT, "é", 0), Some(12));
        assert_eq!(find("aXbX", "X", 2), Some(3));
        assert_eq!(find("aXb", "X", 2), None);
        assert_eq!(find("ab", "X", 10), None);
    }

    const LINE: Rect = [100, 300, 5, 15];

    #[test]
    fn panel_below_or_above_the_line() {
        assert!(beside(Some([110, 320, 140, 100]), Some(LINE)));
        assert!(beside(Some([110, 190, 140, 100]), Some(LINE)));
    }

    #[test]
    fn panel_covering_or_far_from_the_line() {
        assert!(!beside(Some([110, 305, 140, 100]), Some(LINE)));
        assert!(!beside(Some([400, 320, 140, 100]), Some(LINE)));
        assert!(!beside(Some([110, 500, 140, 100]), Some(LINE)));
        assert!(!beside(None, Some(LINE)));
        assert!(!beside(Some([110, 320, 140, 100]), None));
    }

    #[test]
    fn on_screen_excludes_the_taskbar() {
        assert!(on_screen(Some([0, 0, 1280, 752])));
        assert!(!on_screen(Some([0, 700, 100, 100])));
        assert!(!on_screen(None));
    }

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()
    }

    #[test]
    fn silence_and_a_one_second_tone() {
        let silence = pcm(&[0; 44_100 * 2]);
        let tone = pcm(&[3000, 3000, -3000, -3000].repeat(22_050));
        assert_eq!(audible_seconds(&silence), 0.0);
        let seconds = audible_seconds(&[silence, tone].concat());
        assert!((seconds - 1.0).abs() <= 0.05, "{seconds}");
    }

    #[test]
    fn clips_are_playable_wav_files() {
        let directory = TempDir::new("textloom-checks-test-").unwrap();
        let path = directory.path().join("clip.wav");
        write_wav(&path, &pcm(&[1, 2, 3, 4, 5])).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + 8);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
    }

    #[test]
    fn checks_have_unique_names() {
        for (index, check) in CHECKS.iter().enumerate() {
            assert!(
                CHECKS[..index].iter().all(|other| other.name != check.name),
                "{}",
                check.name
            );
        }
    }

    #[test]
    fn listed_values_and_truthiness() {
        assert_eq!(listed(json!("IME composing")), [json!("IME composing")]);
        assert_eq!(listed(json!(["Ready", "IME composing"])).len(), 2);
        assert!(listed(Value::Null).is_empty());
        assert!(truthy(&json!({"text": ""})) && !truthy(&json!(null)) && !truthy(&json!("")));
    }

    #[test]
    fn finds_a_new_panel_and_ignores_masked_rows() {
        let magick = env::var_os("PATH").is_some_and(|path| {
            env::split_paths(&path).any(|directory| directory.join("magick").is_file())
        });
        if !magick {
            eprintln!("skipped: ImageMagick is not installed");
            return;
        }
        let directory = TempDir::new("textloom-checks-test-").unwrap();
        let (before, after) = (
            directory.path().join("before.png"),
            directory.path().join("after.png"),
        );
        let draw = |arguments: &[&str]| {
            assert!(
                Command::new("magick")
                    .args(arguments)
                    .status()
                    .unwrap()
                    .success()
            )
        };
        draw(&["-size", "400x300", "xc:white", before.to_str().unwrap()]);
        draw(&[
            before.to_str().unwrap(),
            "-fill",
            "black",
            "-draw",
            "rectangle 50,40 149,119",
            "-draw",
            "rectangle 0,200 399,215",
            after.to_str().unwrap(),
        ]);
        assert_eq!(
            popup_rect(&before, &after, &[[0, 196, 400, 24]]).unwrap(),
            Some([50, 40, 100, 80])
        );
        assert_eq!(popup_rect(&before, &before, &[]).unwrap(), None);
    }
}
