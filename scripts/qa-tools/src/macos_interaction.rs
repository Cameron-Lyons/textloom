//! The hosted macOS interaction probe. On a disposable GitHub Actions macOS
//! runner it sends native keys through System Events to the native host and
//! TextEdit, checks clipboard generations and representations with a small
//! AppKit inspector, and exercises undo/redo, focus, editing guards, and the
//! Mac editing shortcuts. See `scripts/native-qa/README.md`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use std::{env, thread};

use serde_json::{Map, Value, json};

use crate::native_report::{self, Clipboard, Expected, Mode, Os};
use crate::{Context, Result, TempDir, bail, repo_root, tail};

pub const TITLE: &str = "TextLoom native example";
pub const ORIGINAL: &str = "TextLoom\nSelect text to format it, or start writing.\n\
    Unicode: caf\u{e9}, \u{65e5}\u{672c}\u{8a9e}, \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}.\n\
    Lists, undo, search, and rich snapshots.\n\
    Enter continues a list; Enter on an empty item exits it.";
pub const EXTERNAL: &str =
    "external caf\u{e9} \u{65e5}\u{672c}\u{8a9e}\u{1f469}\u{1f3fd}\u{200d}\u{1f4bb}\nsecond";
pub const SHORTCUT_NAVIGATION: &str = "top\ntip\nabc\n\nend";
pub const SHORTCUT_UNICODE: &str = "a e\u{301}\u{1f469}\u{1f3fd}\u{200d}\u{1f4bb} z";
const MARKER: &str = "Native rich clipboard roundtrip and undo/redo passed";
const PASTE: [&str; 5] = [
    "/usr/bin/env",
    "LC_ALL=en_US.UTF-8",
    "/usr/bin/pbpaste",
    "-Prefer",
    "txt",
];
const COPY: [&str; 3] = ["/usr/bin/env", "LC_ALL=en_US.UTF-8", "/usr/bin/pbcopy"];

pub const APPLESCRIPT: &str = r#"
on run argv
    set commandName to item 1 of argv
    tell application "System Events"
        if commandName is "preflight" then
            if not UI elements enabled then error "System Events Accessibility is unavailable"
            return "accessibility-enabled"
        end if
        if commandName is "textedit-pid" then
            set matches to application processes whose bundle identifier is "com.apple.TextEdit"
            if (count of matches) is 0 then return "absent"
            if (count of matches) is not 1 then error "Ambiguous TextEdit processes"
            return (unix id of item 1 of matches) as text
        end if
        set targetPID to (item 2 of argv) as integer
        set windowName to item 3 of argv
        set expectedDocument to item 5 of argv
        set resolvedDocument to item 6 of argv
        set targetProcess to first application process whose unix id is targetPID
        tell targetProcess
            set candidates to {}
            set windowDescriptions to {}
            repeat with candidate in windows
                set candidateName to name of candidate
                set candidateDocument to ""
                try
                    set candidateDocument to (value of attribute "AXDocument" of candidate) as text
                end try
                set end of windowDescriptions to candidateName & " [" & candidateDocument & "]"
                if expectedDocument is not "" then
                    if candidateDocument is expectedDocument or candidateDocument is resolvedDocument then
                        set end of candidates to contents of candidate
                    end if
                else if candidateName contains windowName then
                    set end of candidates to contents of candidate
                end if
            end repeat
            if (count of candidates) is not 1 then
                set AppleScript's text item delimiters to "; "
                set inventory to windowDescriptions as text
                set AppleScript's text item delimiters to ""
                error "Target window count " & (count of candidates) & " for " & commandName & " / " & windowName & ": " & inventory
            end if
            set targetWindow to item 1 of candidates
            set frontmost to true
            perform action "AXRaise" of targetWindow
        end tell
        delay 0.15
        if (unix id of first application process whose frontmost is true) is not targetPID then
            error "Target PID did not become foreground"
        end if
        tell targetProcess
            if expectedDocument is not "" then
                set foregroundDocument to (value of attribute "AXDocument" of window 1) as text
                if foregroundDocument is not expectedDocument and foregroundDocument is not resolvedDocument then
                    error "Wrong foreground document"
                end if
            else if not ((name of window 1) contains windowName) then
                error "Wrong foreground window"
            end if
            if commandName is "diagnostic" then
                set focusedRole to "unavailable"
                set focusedDescription to "unavailable"
                set focusedValue to "unavailable"
                try
                    set focusedElement to value of attribute "AXFocusedUIElement" of targetProcess
                    set focusedRole to (value of attribute "AXRole" of focusedElement) as text
                    set focusedDescription to (value of attribute "AXDescription" of focusedElement) as text
                end try
                try
                    set focusedValue to (value of attribute "AXValue" of focusedElement) as text
                    if (count of characters of focusedValue) > 2048 then
                        set focusedValue to (characters 1 thru 2048 of focusedValue) as text
                    end if
                end try
                return "PID=" & targetPID & linefeed & "window=" & (name of window 1) & linefeed & "focused_role=" & focusedRole & linefeed & "focused_description=" & focusedDescription & linefeed & "fixture_focused_value=" & focusedValue
            else if commandName is "focus" then
                return "foreground-confirmed"
            else if commandName is "chord" then
                keystroke (item 4 of argv) using {command down}
            else if commandName is "shift-chord" then
                keystroke (item 4 of argv) using {command down, shift down}
            else if commandName is "control-chord" then
                keystroke (item 4 of argv) using {control down}
            else if commandName is "control-shift-chord" then
                keystroke (item 4 of argv) using {control down, shift down}
            else if commandName is "keycode" then
                key code ((item 4 of argv) as integer)
            else if commandName is "command-keycode" then
                key code ((item 4 of argv) as integer) using {command down}
            else if commandName is "command-shift-keycode" then
                key code ((item 4 of argv) as integer) using {command down, shift down}
            else if commandName is "shift-keycode" then
                key code ((item 4 of argv) as integer) using {shift down}
            else if commandName is "option-keycode" then
                key code ((item 4 of argv) as integer) using {option down}
            else if commandName is "option-shift-keycode" then
                key code ((item 4 of argv) as integer) using {option down, shift down}
            else if commandName is "type" then
                keystroke (item 4 of argv)
            else if commandName is "close" then
                perform action "AXPress" of (first button of targetWindow whose subrole is "AXCloseButton")
            else
                error "Unknown native action"
            end if
        end tell
        delay 0.2
        return "native-action-delivered"
    end tell
end run
"#;

/// A System Events action and its key or text.
type Key = (&'static str, &'static str);

const START: Key = ("command-keycode", "126"); // Command-Up: document start.
const END: Key = ("command-keycode", "125"); // Command-Down: document end.
const DOWN: Key = ("keycode", "125");
const LEFT: Key = ("keycode", "123");
const SELECT_START: Key = ("command-shift-keycode", "126");
const SELECT_END: Key = ("command-shift-keycode", "125");

/// What a command printed and how it exited.
pub struct Ran {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Ran {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Terminate,
    Kill,
}

/// The processes, clock, and signals the probe uses, so its sequences can be
/// replayed against a fixture without a desktop.
pub trait System {
    /// Runs a command to completion, failing if it outlives `timeout` seconds.
    fn run(&mut self, arguments: &[String], input: Option<&str>, timeout: f64) -> Result<Ran>;
    /// Starts a native host in its own process group.
    fn spawn(&mut self, arguments: &[String], stdout: File, stderr: File) -> Result<u32>;
    /// The exit code of a started process, once it has exited.
    fn poll(&mut self, pid: u32) -> Option<i32>;
    /// Waits up to `timeout` seconds for a started process to exit.
    fn wait(&mut self, pid: u32, timeout: f64) -> Result<Option<i32>>;
    /// Signals a process, or the process group it leads. It may already have
    /// exited; callers check the outcome afterwards.
    fn signal(&mut self, pid: u32, signal: Signal, group: bool);
    fn monotonic(&self) -> f64;
    fn sleep(&mut self, seconds: f64) -> Result<()>;
}

/// The real processes, interrupted by SIGINT or SIGTERM so cleanup still runs.
pub struct Machine {
    started: Instant,
    children: HashMap<u32, Child>,
    exited: HashMap<u32, i32>,
    interrupted: Arc<AtomicBool>,
}

impl Machine {
    pub fn new(interrupted: Arc<AtomicBool>) -> Self {
        Self {
            started: Instant::now(),
            children: HashMap::new(),
            exited: HashMap::new(),
            interrupted,
        }
    }

    fn check_interrupted(&self) -> Result<()> {
        // Reported once, so the cleanup that follows can still run.
        if self.interrupted.swap(false, Ordering::SeqCst) {
            bail!("Hosted native probe interrupted");
        }
        Ok(())
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| -status.signal().unwrap_or(0))
}

fn read_all(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = sender.send(bytes);
    });
    receiver
}

impl System for Machine {
    fn run(&mut self, arguments: &[String], input: Option<&str>, timeout: f64) -> Result<Ran> {
        self.check_interrupted()?;
        let (program, rest) = arguments.split_first().ok_or("empty command")?;
        let mut child = Command::new(program)
            .args(rest)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(program)?;
        let writer = input.zip(child.stdin.take()).map(|(input, mut stdin)| {
            let input = input.to_owned();
            thread::spawn(move || {
                // A command can exit without reading all of its input.
                let _ = stdin.write_all(input.as_bytes());
            })
        });
        let stdout = read_all(child.stdout.take());
        let stderr = read_all(child.stderr.take());
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            let stop = if Instant::now() >= deadline {
                Some(format!("{program} timed out after {timeout} s"))
            } else {
                self.check_interrupted()
                    .err()
                    .map(|error| error.to_string())
            };
            if let Some(message) = stop {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{message}");
            }
            thread::sleep(Duration::from_millis(5));
        };
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        // A process the command started can keep its pipes open after it exits.
        let collect = |pipe: mpsc::Receiver<Vec<u8>>| {
            pipe.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| format!("{program} output did not close within {timeout} s"))
        };
        Ok(Ran {
            code: exit_code(status),
            stdout: collect(stdout)?,
            stderr: collect(stderr)?,
        })
    }

    fn spawn(&mut self, arguments: &[String], stdout: File, stderr: File) -> Result<u32> {
        use std::os::unix::process::CommandExt;
        let (program, rest) = arguments.split_first().ok_or("empty command")?;
        let child = Command::new(program)
            .args(rest)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .process_group(0)
            .spawn()
            .context(program)?;
        let pid = child.id();
        self.children.insert(pid, child);
        Ok(pid)
    }

    fn poll(&mut self, pid: u32) -> Option<i32> {
        if let Some(&code) = self.exited.get(&pid) {
            return Some(code);
        }
        let code = match self.children.get_mut(&pid)?.try_wait() {
            Ok(None) => return None,
            Ok(Some(status)) => exit_code(status),
            Err(_) => -1,
        };
        self.children.remove(&pid);
        self.exited.insert(pid, code);
        Some(code)
    }

    fn wait(&mut self, pid: u32, timeout: f64) -> Result<Option<i32>> {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        loop {
            if let Some(code) = self.poll(pid) {
                return Ok(Some(code));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            self.check_interrupted()?;
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn signal(&mut self, pid: u32, signal: Signal, group: bool) {
        use rustix::process::{Pid, Signal as Raw, kill_process, kill_process_group};
        let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
            return;
        };
        let signal = match signal {
            Signal::Terminate => Raw::TERM,
            Signal::Kill => Raw::KILL,
        };
        let _ = if group {
            kill_process_group(pid, signal)
        } else {
            kill_process(pid, signal)
        };
    }

    fn monotonic(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    fn sleep(&mut self, seconds: f64) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs_f64(seconds);
        loop {
            self.check_interrupted()?;
            let now = Instant::now();
            if now >= deadline {
                return Ok(());
            }
            thread::sleep((deadline - now).min(Duration::from_millis(50)));
        }
    }
}

/// A percent-encoded `file://` URI, as TextEdit reports a document in AXDocument.
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/_.-~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn change_count(observation: &Map<String, Value>) -> Result<i64> {
    observation
        .get("change_count")
        .and_then(Value::as_i64)
        .ok_or_else(|| "clipboard observation has no change_count".into())
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

pub struct Probe<S> {
    system: S,
    root: PathBuf,
    output: PathBuf,
    revision: String,
    helper: PathBuf,
    import_file: PathBuf,
    external_file: PathBuf,
    /// Each private TextEdit document's URI as given and with symlinks resolved.
    document_urls: HashMap<String, (String, String)>,
    textedit_pid: Option<u32>,
    hosts: Vec<u32>,
    observations: Map<String, Value>,
}

impl<S: System> Probe<S> {
    pub fn new(
        system: S,
        root: PathBuf,
        output: PathBuf,
        work: &Path,
        revision: &str,
    ) -> Result<Self> {
        let import_file = work.join("textloom-html-import.rtf");
        let external_file = work.join("textloom-external-source.rtf");
        let resolved = fs::canonicalize(work).context(work.display())?;
        let mut document_urls = HashMap::new();
        for path in [&import_file, &external_file] {
            let name = path.file_name().expect("fixture files have names");
            document_urls.insert(
                stem(path),
                (
                    file_uri(&std::path::absolute(path)?),
                    file_uri(&resolved.join(name)),
                ),
            );
        }
        let observations = serde_json::from_value(json!({
            "source_revision": revision,
            "manual_signoff": "not_recorded",
            "scope": "hosted macOS native keys and TextEdit clipboard; no IME or reader signoff",
            "checks": [],
            "cleanup": {},
            "passed": false,
        }))?;
        Ok(Self {
            system,
            root,
            output,
            revision: revision.to_owned(),
            helper: work.join("macos-clipboard"),
            import_file,
            external_file,
            document_urls,
            textedit_pid: None,
            hosts: Vec::new(),
            observations,
        })
    }

    pub fn note(&mut self, name: &str, details: Value) -> Result<()> {
        let mut check = Map::new();
        check.insert("name".to_owned(), name.into());
        if let Value::Object(details) = details {
            check.extend(details);
        }
        if let Some(Value::Array(checks)) = self.observations.get_mut("checks") {
            checks.push(Value::Object(check));
        }
        println!("{name}");
        self.save()
    }

    pub fn save(&self) -> Result<()> {
        let mut observations = Value::Object(self.observations.clone());
        observations.sort_all_objects();
        let path = self.output.join("macos-interaction.json");
        fs::write(&path, serde_json::to_string_pretty(&observations)? + "\n")
            .context(path.display())
    }

    fn observe(&mut self, key: &str, value: Value) {
        self.observations.insert(key.to_owned(), value);
    }

    fn observe_cleanup(&mut self, key: &str, value: bool) {
        if let Some(Value::Object(cleanup)) = self.observations.get_mut("cleanup") {
            cleanup.insert(key.to_owned(), value.into());
        }
    }

    fn cleanup_flag(&self, key: &str) -> bool {
        self.observations
            .get("cleanup")
            .and_then(|cleanup| cleanup.get(key))
            == Some(&Value::Bool(true))
    }

    pub fn passed(&self) -> bool {
        self.observations.get("passed") == Some(&Value::Bool(true))
    }

    fn run(
        &mut self,
        arguments: &[String],
        input: Option<&str>,
        timeout: f64,
        check: bool,
    ) -> Result<Ran> {
        let result = self.system.run(arguments, input, timeout)?;
        if check && result.code != 0 {
            let name = Path::new(&arguments[0])
                .file_name()
                .map(|name| name.to_string_lossy());
            bail!(
                "{} exited {}: {}{}",
                name.unwrap_or_default(),
                result.code,
                tail(&result.stderr(), 4000),
                tail(&result.stdout(), 1000)
            );
        }
        Ok(result)
    }

    fn apple(&mut self, arguments: &[&str], check: bool) -> Result<Ran> {
        let mut command = strings(&["/usr/bin/osascript", "-"]);
        command.extend(strings(arguments));
        self.run(&command, Some(APPLESCRIPT), 10.0, check)
    }

    fn textedit(&mut self) -> Result<Option<u32>> {
        let value = self.apple(&["textedit-pid"], true)?.stdout();
        match value.trim() {
            "absent" => Ok(None),
            pid => Ok(Some(
                pid.parse()
                    .map_err(|_| format!("unexpected TextEdit PID {pid:?}"))?,
            )),
        }
    }

    fn documents(&self, window: &str) -> (String, String) {
        self.document_urls.get(window).cloned().unwrap_or_default()
    }

    fn action(&mut self, pid: u32, window: &str, command: &str, value: &str) -> Result<()> {
        self.observe(
            "last_native_action",
            json!({"pid": pid, "window": window, "command": command, "value": value}),
        );
        self.save()?;
        let (document, resolved) = self.documents(window);
        let deadline = self.system.monotonic() + 5.0;
        loop {
            let result = self.apple(
                &[
                    command,
                    &pid.to_string(),
                    window,
                    value,
                    &document,
                    &resolved,
                ],
                false,
            )?;
            if result.code == 0 {
                return Ok(());
            }
            let stderr = result.stderr();
            if !stderr.contains("Target window count 0") || self.system.monotonic() >= deadline {
                bail!(
                    "Native {command} on PID {pid} / {window}: {}",
                    tail(&stderr, 4000)
                );
            }
            self.system.sleep(0.2)?;
        }
    }

    fn focus(&mut self, pid: u32, window: &str, host: bool) -> Result<()> {
        let deadline = self.system.monotonic() + 15.0;
        let mut last_error = "target window did not appear".to_owned();
        while self.system.monotonic() < deadline {
            if host && let Some(code) = self.system.poll(pid) {
                bail!("Native process exited early ({code})");
            }
            let (document, resolved) = self.documents(window);
            let result = self.apple(
                &["focus", &pid.to_string(), window, "", &document, &resolved],
                false,
            )?;
            if result.code == 0 {
                return self.system.sleep(0.35);
            }
            last_error = result.stderr().trim().to_owned();
            self.system.sleep(0.25)?;
        }
        bail!("Could not focus PID-checked target window: {last_error}")
    }

    fn clipboard(
        &mut self,
        command: &str,
        arguments: &[&str],
        changed_from: Option<i64>,
    ) -> Result<Map<String, Value>> {
        let deadline = self.system.monotonic() + 5.0;
        let mut last_error = "clipboard did not converge".to_owned();
        let mut helper = vec![self.helper.display().to_string(), command.to_owned()];
        helper.extend(strings(arguments));
        while self.system.monotonic() < deadline {
            let result = self.run(&helper, None, 10.0, false)?;
            if result.code == 0 {
                let observation: Map<String, Value> = serde_json::from_slice(&result.stdout)?;
                if changed_from.is_none_or(|generation| {
                    change_count(&observation).is_ok_and(|count| count > generation)
                }) {
                    return Ok(observation);
                }
                last_error = "native copy did not publish a fresh clipboard generation".to_owned();
            } else {
                last_error = result.stderr().trim().to_owned();
            }
            self.system.sleep(0.15)?;
        }
        bail!("Clipboard {command} failed: {last_error}")
    }

    fn copy(
        &mut self,
        pid: u32,
        window: &str,
        expected: &str,
        fallback: bool,
        select_all: bool,
    ) -> Result<Map<String, Value>> {
        let generation = change_count(&self.clipboard("state", &[], None)?)?;
        if select_all {
            self.action(pid, window, "chord", "a")?;
        }
        self.action(pid, window, "chord", "c")?;
        let command = if fallback { "fallback" } else { "plain" };
        let mut result = self.clipboard(command, &[expected], Some(generation))?;
        // The inspector's Swift String == accepts canonical equivalence. Compare
        // raw UTF-8 so normalization and line-ending changes cannot pass as
        // exact restores. Pin only the clipboard subprocess encoding.
        let pasted = self.run(&strings(&PASTE), None, 10.0, true)?;
        if pasted.stdout != expected.as_bytes() {
            bail!("Native clipboard differs from the exact Unicode fixture");
        }
        result.insert("plain_utf8_exact".to_owned(), true.into());
        Ok(result)
    }

    fn external_copy(&mut self) -> Result<Map<String, Value>> {
        let pid = self.textedit_pid.ok_or("TextEdit is not running")?;
        let window = stem(&self.external_file);
        self.focus(pid, &window, false)?;
        let generation = change_count(&self.clipboard("state", &[], None)?)?;
        self.action(pid, &window, "chord", "a")?;
        self.action(pid, &window, "chord", "c")?;
        self.clipboard("external-fixture", &[], Some(generation))
    }

    fn diagnostic(&mut self, label: &str) -> Result<()> {
        let command = vec![
            self.helper.display().to_string(),
            "diagnose".to_owned(),
            self.output.display().to_string(),
            label.to_owned(),
        ];
        let result = self.run(&command, None, 20.0, false)?;
        let path = self
            .output
            .join(format!("macos-fixture-{label}.clipboard.json"));
        fs::write(&path, result.stdout() + &result.stderr()).context(path.display())?;
        if let Some(pid) = self.textedit_pid {
            let window = stem(&self.import_file);
            let (document, resolved) = self.documents(&window);
            let result = self.apple(
                &[
                    "diagnostic",
                    &pid.to_string(),
                    &window,
                    "",
                    &document,
                    &resolved,
                ],
                false,
            )?;
            let text: String = (result.stdout() + &result.stderr())
                .chars()
                .take(8192)
                .collect();
            let path = self.output.join(format!("macos-fixture-{label}.ax.txt"));
            fs::write(&path, text).context(path.display())?;
        }
        Ok(())
    }

    fn start(&mut self, mode: Mode) -> Result<(u32, PathBuf)> {
        let name = mode.name();
        let stdout_path = self.output.join(format!("macos-{name}.stdout.log"));
        let stdout = File::create(&stdout_path).context(stdout_path.display())?;
        let stderr_path = self.output.join(format!("macos-{name}.stderr.log"));
        let stderr = File::create(&stderr_path).context(stderr_path.display())?;
        let report = self.output.join(format!("macos-{name}.json"));
        let mut arguments = vec![
            self.root
                .join("target/debug/textloom-native-example")
                .display()
                .to_string(),
            "--rich-clipboard".to_owned(),
            "--qa-report".to_owned(),
            report.display().to_string(),
        ];
        arguments.push(
            match mode {
                Mode::Editable => "--clipboard-self-test",
                Mode::ReadOnly => "--read-only",
                Mode::Disabled => "--disabled",
            }
            .to_owned(),
        );
        let pid = self.system.spawn(&arguments, stdout, stderr)?;
        self.hosts.push(pid);
        if mode == Mode::Editable {
            let deadline = self.system.monotonic() + 20.0;
            while !fs::read_to_string(&stdout_path)?
                .lines()
                .any(|line| line == MARKER)
            {
                if self.system.poll(pid).is_some() || self.system.monotonic() >= deadline {
                    bail!("Native clipboard fixture did not complete successfully");
                }
                self.system.sleep(0.2)?;
            }
        }
        self.focus(pid, TITLE, true)?;
        self.note(
            &format!("{name}: native window and foreground PID verified"),
            json!({"pid": pid}),
        )?;
        Ok((pid, report))
    }

    fn finish(&mut self, mode: Mode, pid: u32, report: &Path) -> Result<()> {
        let name = mode.name();
        self.action(pid, TITLE, "close", "")?;
        match self.system.wait(pid, 10.0)? {
            None => bail!("{name}: native host did not exit within 10 s"),
            Some(0) => {}
            Some(code) => bail!("{name}: native host exited {code}"),
        }
        let expected = Expected {
            revision: self.revision.clone(),
            os: Os::Macos,
            mode,
            interaction: true,
            clipboard: Clipboard::Rich,
        };
        let observation = native_report::check_file(report, &expected)
            .map_err(|error| format!("Native report validation failed: {error}"))?;
        println!("{}", expected.summary());
        let events = &observation.host_input_events;
        let changes = &observation.observed_state_changes;
        if events.key_press == 0 {
            bail!("{name}: no native keyboard events observed");
        }
        if mode == Mode::Editable {
            let counts = [
                events.text,
                events.paste,
                events.copy,
                events.window_focus_lost,
                events.window_focus_gained,
            ];
            if counts.contains(&0) {
                bail!("Editable session did not observe actual input and focus transitions");
            }
            if changes.document_revision == 0 {
                bail!("Editable session did not observe document mutation");
            }
        } else if changes.document_revision != 0 || observation.final_document.undo_steps != 0 {
            bail!("{name}: guarded native input changed the document/history");
        } else if mode == Mode::ReadOnly && [events.paste, events.copy, events.cut].contains(&0) {
            bail!("Read-only guards were not exercised through native clipboard events");
        }
        let file = report
            .file_name()
            .map(|file| file.to_string_lossy().into_owned());
        self.note(
            &format!("{name}: strict QA and real input counters passed"),
            json!({"report": file}),
        )
    }

    fn open_textedit(&mut self, path: &Path) -> Result<()> {
        let open = strings(&["/usr/bin/open", "-b", "com.apple.TextEdit"]);
        self.run(
            &[open, vec![path.display().to_string()]].concat(),
            None,
            10.0,
            true,
        )?;
        let deadline = self.system.monotonic() + 15.0;
        let pid = loop {
            if let Some(pid) = self.textedit_pid {
                break pid;
            }
            if self.system.monotonic() >= deadline {
                bail!("TextEdit did not launch");
            }
            self.textedit_pid = self.textedit()?;
            if self.textedit_pid.is_none() {
                self.system.sleep(0.2)?;
            }
        };
        let window = stem(path);
        self.focus(pid, &window, false)?;
        self.note(
            "TextEdit private document and foreground PID verified",
            json!({"pid": pid, "window": window}),
        )
    }

    fn shortcut_fixture(&mut self, pid: u32, text: &str) -> Result<()> {
        // Only deterministic public text goes through the runner's pasteboard.
        self.run(&strings(&COPY), Some(text), 10.0, true)?;
        self.action(pid, TITLE, "chord", "a")?;
        self.action(pid, TITLE, "chord", "v")?;
        self.copy(pid, TITLE, text, false, true)?;
        Ok(())
    }

    fn shortcut_check(
        &mut self,
        pid: u32,
        name: &str,
        expected: &str,
        actions: &[Key],
        undo_to: Option<&str>,
    ) -> Result<()> {
        let result = self
            .shortcut_attempt(pid, expected, actions, undo_to)
            .map_err(|error| format!("Shortcut {name}: {error}"))?;
        self.note(&format!("editable: shortcut {name}"), Value::Object(result))
    }

    fn shortcut_attempt(
        &mut self,
        pid: u32,
        expected: &str,
        actions: &[Key],
        undo_to: Option<&str>,
    ) -> Result<Map<String, Value>> {
        for (command, value) in actions {
            self.action(pid, TITLE, command, value)?;
        }
        // Navigation must copy its selection, never replace it with Select All.
        let mut result = self.copy(pid, TITLE, expected, false, undo_to.is_some())?;
        if let Some(undo_to) = undo_to {
            self.action(pid, TITLE, "chord", "z")?;
            let restored = self.copy(pid, TITLE, undo_to, false, true)?;
            result.insert(
                "undo_plain_exact".to_owned(),
                restored.get("plain_exact").cloned().into(),
            );
        }
        Ok(result)
    }

    fn shortcuts(&mut self, pid: u32) -> Result<()> {
        // Keep lines short and vertical moves at column zero to avoid font metrics.
        // Shift-Return creates a real soft break; exported plain text uses LF.
        // No Control-arrow chords: macOS reserves them for Mission Control.
        // START then DOWN reaches the start of the soft line.
        self.shortcut_fixture(pid, "top\nabc\n\nend")?;
        self.action(pid, TITLE, START.0, START.1)?;
        self.action(pid, TITLE, "command-keycode", "124")?;
        self.action(pid, TITLE, "shift-keycode", "36")?;
        self.action(pid, TITLE, "type", "tip")?;
        self.copy(pid, TITLE, SHORTCUT_NAVIGATION, false, true)?;

        // Set up and witness each alias with independent native keys. Pairing
        // aliases could hide a regression such as Control-P/N being swapped.
        self.shortcut_check(
            pid,
            "Control-A reaches the paragraph start across a soft break",
            SHORTCUT_NAVIGATION,
            &[START, DOWN, ("control-chord", "a"), SELECT_END],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Control-E reaches the paragraph end",
            "top\ntip",
            &[START, DOWN, ("control-chord", "e"), SELECT_START],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Control-A selects to the paragraph start",
            "top\n",
            &[START, DOWN, ("control-shift-chord", "a")],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Control-E selects to the paragraph end",
            "tip",
            &[START, DOWN, ("control-shift-chord", "e")],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Control-P moves to the previous visual line",
            SHORTCUT_NAVIGATION,
            &[START, DOWN, ("control-chord", "p"), SELECT_END],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Control-N moves to the next visual line",
            "tip\nabc\n\nend",
            &[START, ("control-chord", "n"), SELECT_END],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Control-P selects the previous visual line",
            "top\n",
            &[START, DOWN, ("control-shift-chord", "p")],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Control-N selects the next visual line",
            "top\n",
            &[START, ("control-shift-chord", "n")],
            None,
        )?;

        self.shortcut_check(
            pid,
            "Option-Up reaches the paragraph start across a soft break",
            SHORTCUT_NAVIGATION,
            &[START, DOWN, ("option-keycode", "126"), SELECT_END],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Option-Down reaches the paragraph end across a soft break",
            "top\ntip",
            &[
                START,
                ("keycode", "124"),
                ("option-keycode", "125"),
                SELECT_START,
            ],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Option-Down selects to the paragraph end",
            "top\ntip",
            &[START, ("option-shift-keycode", "125")],
            None,
        )?;
        // Continue selections to check their anchors through empty paragraphs.
        self.shortcut_check(
            pid,
            "Shift-Option-Down extends into the next paragraph",
            "top\ntip\nabc",
            &[("option-shift-keycode", "125")],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Option-Down extends through an empty paragraph",
            "top\ntip\nabc\n",
            &[("option-shift-keycode", "125")],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Option-Down retains the anchor across paragraphs",
            SHORTCUT_NAVIGATION,
            &[("option-shift-keycode", "125")],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Option-Up selects to the paragraph start",
            "end",
            &[END, ("option-shift-keycode", "126")],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Option-Up extends through an empty paragraph",
            "\nend",
            &[("option-shift-keycode", "126")],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Option-Up retains the anchor across paragraphs",
            "abc\n\nend",
            &[("option-shift-keycode", "126")],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Option-Up reaches the document start",
            SHORTCUT_NAVIGATION,
            &[("option-shift-keycode", "126")],
            None,
        )?;
        self.copy(pid, TITLE, SHORTCUT_NAVIGATION, false, true)?;
        self.shortcut_check(
            pid,
            "Command-Backspace preserves the previous soft line / undo",
            "top\n\nabc\n\nend",
            &[
                START,
                DOWN,
                ("command-keycode", "124"),
                ("command-keycode", "51"),
            ],
            Some(SHORTCUT_NAVIGATION),
        )?;

        // END and three LEFTs put the caret after the combining grapheme.
        self.shortcut_fixture(pid, SHORTCUT_UNICODE)?;
        self.shortcut_check(
            pid,
            "Control-F moves over a whole ZWJ emoji",
            "\u{1f469}\u{1f3fd}\u{200d}\u{1f4bb}",
            &[
                END,
                LEFT,
                LEFT,
                LEFT,
                ("control-chord", "f"),
                ("shift-keycode", "123"),
            ],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Control-F selects a whole ZWJ emoji",
            "\u{1f469}\u{1f3fd}\u{200d}\u{1f4bb}",
            &[END, LEFT, LEFT, LEFT, ("control-shift-chord", "f")],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Control-B moves over a combining grapheme",
            "e\u{301}",
            &[
                END,
                LEFT,
                LEFT,
                LEFT,
                ("control-chord", "b"),
                ("shift-keycode", "124"),
            ],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Shift-Control-B selects a combining grapheme",
            "e\u{301}",
            &[END, LEFT, LEFT, LEFT, ("control-shift-chord", "b")],
            None,
        )?;
        self.shortcut_check(
            pid,
            "Control-H deletes a combining grapheme / undo",
            "a \u{1f469}\u{1f3fd}\u{200d}\u{1f4bb} z",
            &[END, LEFT, LEFT, LEFT, ("control-chord", "h")],
            Some(SHORTCUT_UNICODE),
        )?;
        self.shortcut_check(
            pid,
            "Control-D deletes a ZWJ emoji / undo",
            "a e\u{301} z",
            &[END, LEFT, LEFT, LEFT, ("control-chord", "d")],
            Some(SHORTCUT_UNICODE),
        )
    }

    fn editable(&mut self) -> Result<()> {
        let (pid, report) = self.start(Mode::Editable)?;
        let fixture = self.clipboard("native-fixture", &[], None)?;
        self.note(
            "editable: native fixture representations verified",
            Value::Object(fixture),
        )?;
        self.diagnostic("native-offer")?;
        let import_file = self.import_file.clone();
        let import = stem(&import_file);
        self.open_textedit(&import_file)?;
        let textedit = self.textedit_pid.ok_or("TextEdit is not running")?;
        self.action(textedit, &import, "chord", "a")?;
        self.diagnostic("textedit-before-paste")?;
        self.action(textedit, &import, "chord", "v")?;
        self.diagnostic("textedit-after-paste")?;
        let generation = change_count(&self.clipboard("state", &[], None)?)?;
        self.action(textedit, &import, "chord", "a")?;
        self.action(textedit, &import, "chord", "c")?;
        self.diagnostic("textedit-after-copy")?;
        let imported = self.clipboard("textedit-import", &[], Some(generation))?;
        self.note(
            "editable: TextEdit imported native HTML",
            Value::Object(imported),
        )?;
        self.action(textedit, &import, "chord", "s")?;
        let external_file = self.external_file.clone();
        self.open_textedit(&external_file)?;
        let external = self.external_copy()?;
        self.note(
            "editable: TextEdit external rich source verified",
            Value::Object(external),
        )?;
        self.focus(pid, TITLE, true)?;
        self.action(pid, TITLE, "chord", "a")?;
        self.action(pid, TITLE, "chord", "v")?;
        let fallback = self.copy(pid, TITLE, EXTERNAL, true, true)?;
        self.note(
            "editable: external rich source used plain fallback",
            Value::Object(fallback),
        )?;
        self.action(pid, TITLE, "chord", "z")?;
        self.copy(pid, TITLE, ORIGINAL, false, true)?;
        self.action(pid, TITLE, "shift-chord", "z")?;
        self.copy(pid, TITLE, EXTERNAL, false, true)?;
        self.note(
            "editable: native replacement undo and redo restored exact Unicode text",
            Value::Null,
        )?;
        self.action(pid, TITLE, "keycode", "124")?;
        self.action(pid, TITLE, "type", "x")?;
        self.focus(textedit, &stem(&external_file), false)?;
        self.focus(pid, TITLE, true)?;
        self.action(pid, TITLE, "type", "y")?;
        self.copy(pid, TITLE, &format!("{EXTERNAL}xy"), false, true)?;
        self.action(pid, TITLE, "chord", "z")?;
        self.copy(pid, TITLE, &format!("{EXTERNAL}x"), false, true)?;
        self.action(pid, TITLE, "chord", "z")?;
        self.copy(pid, TITLE, EXTERNAL, false, true)?;
        self.note(
            "editable: native typing and focus-separated undo groups passed",
            Value::Null,
        )?;
        self.shortcuts(pid)?;
        self.finish(Mode::Editable, pid, &report)
    }

    fn guarded(&mut self, mode: Mode) -> Result<()> {
        let (pid, report) = self.start(mode)?;
        if mode == Mode::ReadOnly {
            self.copy(pid, TITLE, ORIGINAL, false, true)?;
        }
        self.external_copy()?;
        self.focus(pid, TITLE, true)?;
        let generation = change_count(&self.clipboard("state", &[], None)?)?;
        self.action(pid, TITLE, "chord", "a")?;
        self.action(pid, TITLE, "type", "x")?;
        for key in ["51", "117", "36"] {
            self.action(pid, TITLE, "keycode", key)?;
        }
        self.action(pid, TITLE, "chord", "v")?;
        self.action(pid, TITLE, "chord", "x")?;
        self.action(pid, TITLE, "chord", "z")?;
        self.action(pid, TITLE, "shift-chord", "z")?;
        if mode == Mode::ReadOnly {
            self.clipboard("plain", &[ORIGINAL], Some(generation))?;
            self.copy(pid, TITLE, ORIGINAL, false, true)?;
            self.note(
                "read-only: copy/cut available and native editing attempts preserved exact document",
                Value::Null,
            )?;
        } else {
            self.action(pid, TITLE, "chord", "c")?;
            let state = self.clipboard("plain", &[EXTERNAL], None)?;
            if change_count(&state)? != generation {
                bail!("Disabled editor changed the external clipboard");
            }
            self.note(
                "disabled: native editing/copy/cut attempts preserved external clipboard",
                Value::Null,
            )?;
        }
        self.finish(mode, pid, &report)
    }

    /// Closes every native host and the probe's TextEdit, then checks they are gone.
    pub fn cleanup(&mut self) -> Result<()> {
        let mut clean = true;
        for pid in self.hosts.clone() {
            if self.system.poll(pid).is_none() {
                clean = false;
                self.system.signal(pid, Signal::Terminate, true);
                if self.system.wait(pid, 3.0)?.is_none() {
                    self.system.signal(pid, Signal::Kill, true);
                    self.system.wait(pid, 3.0)?;
                }
            }
        }
        let reaped = self
            .hosts
            .clone()
            .into_iter()
            .all(|pid| self.system.poll(pid).is_some());
        self.observe_cleanup("native_processes_reaped", reaped);
        self.observe_cleanup("native_sessions_closed_gracefully", clean);
        let absent = match self.textedit_pid {
            None => true,
            Some(pid) => {
                let result = self.run(
                    &strings(&["/bin/ps", "-p", &pid.to_string(), "-o", "comm="]),
                    None,
                    10.0,
                    false,
                )?;
                if result.code == 0 {
                    if Path::new(result.stdout().trim())
                        .file_name()
                        .is_none_or(|name| name != "TextEdit")
                    {
                        bail!("TextEdit PID identity changed before cleanup");
                    }
                    for signal in [Signal::Terminate, Signal::Kill] {
                        self.system.signal(pid, signal, false);
                        let deadline = self.system.monotonic() + 3.0;
                        while self.system.monotonic() < deadline && self.textedit()? == Some(pid) {
                            self.system.sleep(0.15)?;
                        }
                        if self.textedit()? != Some(pid) {
                            break;
                        }
                    }
                }
                self.textedit()?.is_none()
            }
        };
        self.observe_cleanup("textedit_process_absent", absent);
        let required = [
            "native_processes_reaped",
            "native_sessions_closed_gracefully",
            "textedit_process_absent",
        ];
        if !required.iter().all(|key| self.cleanup_flag(key)) {
            self.observe("passed", false.into());
            self.save()?;
            bail!("Native/TextEdit processes did not all close and disappear");
        }
        self.save()
    }

    pub fn execute(&mut self) -> Result<()> {
        self.apple(&["preflight"], true)?;
        if self.textedit()?.is_some() {
            bail!("Refusing to operate on an existing TextEdit process");
        }
        let source = self.root.join("scripts/native-qa/macos_clipboard.swift");
        let build = vec![
            "/usr/bin/xcrun".to_owned(),
            "swiftc".to_owned(),
            source.display().to_string(),
            "-o".to_owned(),
            self.helper.display().to_string(),
        ];
        let result = self.run(&build, None, 60.0, true)?;
        let log = self.output.join("macos-helper-build.log");
        fs::write(&log, result.stdout() + &result.stderr()).context(log.display())?;
        let (import_file, external_file) = (
            self.import_file.display().to_string(),
            self.external_file.display().to_string(),
        );
        self.clipboard("prepare", &[&import_file, &external_file], None)?;
        self.note(
            "preflight: isolated hosted desktop and System Events Accessibility verified",
            Value::Null,
        )?;
        self.editable()?;
        self.guarded(Mode::ReadOnly)?;
        self.guarded(Mode::Disabled)?;
        self.observe("passed", true.into());
        Ok(())
    }
}

/// The `macos-interaction` command.
pub fn main() -> ExitCode {
    let hosted = env::var("GITHUB_ACTIONS").is_ok_and(|value| value == "true")
        && env::var("RUNNER_OS").is_ok_and(|value| value == "macOS");
    if !cfg!(target_os = "macos") || !hosted {
        eprintln!(
            "This clipboard/input probe runs only on an isolated hosted macOS Actions runner"
        );
        return ExitCode::FAILURE;
    }
    let revision = env::var("TEXTLOOM_QA_REVISION").unwrap_or_default();
    let root = repo_root();
    let full = revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !full || !root.join("target/debug/textloom-native-example").is_file() {
        eprintln!("Missing full 40-hex QA revision or built native executable");
        return ExitCode::FAILURE;
    }
    match run_probe(root, &revision) {
        Ok(true) => {
            println!(
                "macOS native TextEdit, keyboard, undo/redo, focus, and editing guards passed; manual signoff remains separate"
            );
            ExitCode::SUCCESS
        }
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("macOS native interaction failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_probe(root: PathBuf, revision: &str) -> Result<bool> {
    let output = root.join("native-qa-observations");
    fs::create_dir_all(&output).context(output.display())?;
    let interrupted = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&interrupted))?;
    }
    let parent = env::var_os("RUNNER_TEMP").map_or_else(env::temp_dir, PathBuf::from);
    let work = TempDir::new_in(&parent, "textloom-macos-interaction-").context(parent.display())?;
    let mut probe = Probe::new(
        Machine::new(interrupted),
        root,
        output,
        work.path(),
        revision,
    )?;
    if let Err(error) = probe.execute() {
        probe.observe("failure", error.to_string().into());
        probe.observe("passed", false.into());
        eprintln!("macOS native interaction failed: {error}");
    }
    if let Err(error) = probe.cleanup() {
        probe.observe("cleanup_failure", error.to_string().into());
        probe.observe("passed", false.into());
        let _ = probe.save();
        eprintln!("macOS native cleanup failed: {error}");
    }
    Ok(probe.passed())
}

#[cfg(test)]
mod tests;
