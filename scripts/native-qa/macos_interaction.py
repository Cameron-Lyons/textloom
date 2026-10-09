#!/usr/bin/env python3
"""Exercise native macOS input and TextEdit clipboard on an isolated CI desktop.

Only fixture text is used. Native reports remain content free and carry no
manual signoff. This driver refuses to run on a person's macOS session.
"""

import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "native-qa-observations"
TITLE = "TextLoom native example"
ORIGINAL = (
    "TextLoom\nSelect text to format it, or start writing.\n"
    "Unicode: café, 日本語, 👨‍👩‍👧‍👦.\nLists, undo, search, and rich snapshots.\n"
    "Enter continues a list; Enter on an empty item exits it."
)
EXTERNAL = "external café 日本語👩🏽‍💻\nsecond"
MARKER = "Native rich clipboard roundtrip and undo/redo passed"

# Use the runner's pre-authorized osascript -> System Events path. All process,
# window, and input values are argv, never interpolated AppleScript source.
APPLESCRIPT = r'''
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
        set targetProcess to first application process whose unix id is targetPID
        tell targetProcess
            set candidates to windows whose name contains windowName
            if (count of candidates) is not 1 then error "Missing or ambiguous target window"
            set targetWindow to item 1 of candidates
            set frontmost to true
            perform action "AXRaise" of targetWindow
        end tell
        delay 0.15
        if (unix id of first application process whose frontmost is true) is not targetPID then
            error "Target PID did not become foreground"
        end if
        tell targetProcess
            if not ((name of window 1) contains windowName) then error "Wrong foreground window"
            if commandName is "focus" then
                return "foreground-confirmed"
            else if commandName is "chord" then
                keystroke (item 4 of argv) using {command down}
            else if commandName is "shift-chord" then
                keystroke (item 4 of argv) using {command down, shift down}
            else if commandName is "keycode" then
                key code ((item 4 of argv) as integer)
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
'''


class Probe:
    def __init__(self, work, revision):
        self.work = work
        self.revision = revision
        self.helper = work / "macos-clipboard"
        self.import_file = work / "textloom-html-import.rtf"
        self.external_file = work / "textloom-external-source.rtf"
        self.textedit_pid = None
        self.hosts = []
        self.handles = []
        self.observations = {
            "source_revision": revision,
            "manual_signoff": "not_recorded",
            "scope": "hosted macOS native keys and TextEdit clipboard; no IME or reader signoff",
            "checks": [],
            "cleanup": {},
            "passed": False,
        }

    def note(self, name, **details):
        self.observations["checks"].append({"name": name, **details})
        print(name, flush=True)
        self.save()

    def save(self):
        (OUTPUT / "macos-interaction.json").write_text(
            json.dumps(self.observations, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )

    def run(self, arguments, *, input_text=None, timeout=10, check=True):
        result = subprocess.run(
            [str(argument) for argument in arguments], input=input_text,
            capture_output=True, text=True, encoding="utf-8", timeout=timeout,
        )
        if check and result.returncode:
            # Fixture-only diagnostic output is bounded; do not log argv/text.
            raise RuntimeError(
                f"{Path(str(arguments[0])).name} exited {result.returncode}: "
                f"{result.stderr[-4000:]}{result.stdout[-1000:]}"
            )
        return result

    def apple(self, *arguments, check=True):
        return self.run(["/usr/bin/osascript", "-", *arguments], input_text=APPLESCRIPT, check=check)

    def textedit(self):
        value = self.apple("textedit-pid").stdout.strip()
        return None if value == "absent" else int(value)

    def action(self, pid, window, command, value=""):
        self.apple(command, pid, window, value)

    def focus(self, pid, window, host=None):
        deadline = time.monotonic() + 15
        last_error = "target window did not appear"
        while time.monotonic() < deadline:
            if host is not None and host.poll() is not None:
                raise RuntimeError(f"Native process exited early ({host.returncode})")
            result = self.apple("focus", pid, window, check=False)
            if result.returncode == 0:
                # Allow the focus event to reach a rendered frame before input.
                time.sleep(0.35)
                return
            last_error = result.stderr.strip()
            time.sleep(0.25)
        raise RuntimeError(f"Could not focus PID-checked target window: {last_error}")

    def clipboard(self, command, *arguments, changed_from=None):
        deadline = time.monotonic() + 5
        last_error = "clipboard did not converge"
        while time.monotonic() < deadline:
            result = self.run([self.helper, command, *arguments], check=False)
            if result.returncode == 0:
                observation = json.loads(result.stdout)
                if changed_from is None or observation["change_count"] > changed_from:
                    return observation
                last_error = "native copy did not publish a fresh clipboard generation"
            else:
                last_error = result.stderr.strip()
            time.sleep(0.15)
        raise RuntimeError(f"Clipboard {command} failed: {last_error}")

    def copy(self, pid, window, expected, *, fallback=False, select_all=True):
        generation = self.clipboard("state")["change_count"]
        if select_all:
            self.action(pid, window, "chord", "a")
        self.action(pid, window, "chord", "c")
        return self.clipboard("fallback" if fallback else "plain", expected, changed_from=generation)

    def external_copy(self):
        self.focus(self.textedit_pid, self.external_file.stem)
        generation = self.clipboard("state")["change_count"]
        self.action(self.textedit_pid, self.external_file.stem, "chord", "a")
        self.action(self.textedit_pid, self.external_file.stem, "chord", "c")
        return self.clipboard("external-fixture", changed_from=generation)

    def start(self, mode):
        stdout_path = OUTPUT / f"macos-{mode}.stdout.log"
        stdout = stdout_path.open("w", encoding="utf-8")
        stderr = (OUTPUT / f"macos-{mode}.stderr.log").open("w", encoding="utf-8")
        self.handles.extend([stdout, stderr])
        report = OUTPUT / f"macos-{mode}.json"
        arguments = [ROOT / "target/debug/textloom-native-example", "--rich-clipboard", "--qa-report", report]
        if mode == "editable":
            arguments.append("--clipboard-self-test")
        else:
            arguments.append("--read-only" if mode == "read-only" else "--disabled")
        process = subprocess.Popen(arguments, stdout=stdout, stderr=stderr, start_new_session=True)
        self.hosts.append(process)
        if mode == "editable":
            deadline = time.monotonic() + 20
            while MARKER not in stdout_path.read_text(encoding="utf-8").splitlines():
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("Native clipboard fixture did not complete successfully")
                time.sleep(0.2)
        self.focus(process.pid, TITLE, host=process)
        self.note(f"{mode}: native window and foreground PID verified", pid=process.pid)
        return process, report

    def finish(self, mode, process, report):
        self.action(process.pid, TITLE, "close")
        process.wait(timeout=10)
        if process.returncode != 0:
            raise RuntimeError(f"{mode}: native host exited {process.returncode}")
        result = self.run([
            sys.executable, ROOT / "scripts/check_native_report.py", report,
            "--revision", self.revision, "--os", "macos", "--mode", mode, "--interaction",
        ])
        print(result.stdout.strip(), flush=True)
        observation = json.loads(report.read_text(encoding="utf-8"))
        events = observation["host_input_events"]
        changes = observation["observed_state_changes"]
        if events["key_press"] == 0:
            raise RuntimeError(f"{mode}: no native keyboard events observed")
        if mode == "editable":
            if not all(events[key] > 0 for key in ("text", "paste", "copy", "window_focus_lost", "window_focus_gained")):
                raise RuntimeError("Editable session did not observe actual input and focus transitions")
            if changes["document_revision"] == 0:
                raise RuntimeError("Editable session did not observe document mutation")
        elif changes["document_revision"] != 0 or observation["final_document"]["undo_steps"] != 0:
            raise RuntimeError(f"{mode}: guarded native input changed the document/history")
        elif mode == "read-only" and not all(events[key] > 0 for key in ("paste", "copy", "cut")):
            raise RuntimeError("Read-only guards were not exercised through native clipboard events")
        self.note(f"{mode}: strict QA and real input counters passed", report=report.name)

    def open_textedit(self, path):
        self.run(["/usr/bin/open", "-b", "com.apple.TextEdit", path])
        deadline = time.monotonic() + 15
        while self.textedit_pid is None:
            self.textedit_pid = self.textedit()
            if time.monotonic() >= deadline:
                raise RuntimeError("TextEdit did not launch")
            if self.textedit_pid is None:
                time.sleep(0.2)
        self.focus(self.textedit_pid, path.stem)

    def editable(self):
        process, report = self.start("editable")
        self.note("editable: native fixture representations verified", **self.clipboard("native-fixture"))
        self.open_textedit(self.import_file)
        self.action(self.textedit_pid, self.import_file.stem, "chord", "a")
        self.action(self.textedit_pid, self.import_file.stem, "chord", "v")
        generation = self.clipboard("state")["change_count"]
        self.action(self.textedit_pid, self.import_file.stem, "chord", "a")
        self.action(self.textedit_pid, self.import_file.stem, "chord", "c")
        self.note("editable: TextEdit imported native HTML", **self.clipboard("textedit-import", changed_from=generation))
        # Save the already named private file so closing it needs no dialog.
        self.action(self.textedit_pid, self.import_file.stem, "chord", "s")
        self.open_textedit(self.external_file)
        self.note("editable: TextEdit external rich source verified", **self.external_copy())
        self.focus(process.pid, TITLE, host=process)
        self.action(process.pid, TITLE, "chord", "a")
        self.action(process.pid, TITLE, "chord", "v")
        self.note("editable: external rich source used plain fallback", **self.copy(process.pid, TITLE, EXTERNAL, fallback=True))
        self.action(process.pid, TITLE, "chord", "z")
        self.copy(process.pid, TITLE, ORIGINAL)
        self.action(process.pid, TITLE, "shift-chord", "z")
        self.copy(process.pid, TITLE, EXTERNAL)
        self.note("editable: native replacement undo and redo restored exact Unicode text")
        # Copy leaves the entire document selected; Right collapses it at its end.
        self.action(process.pid, TITLE, "keycode", "124")
        self.action(process.pid, TITLE, "type", "x")
        self.focus(self.textedit_pid, self.external_file.stem)
        self.focus(process.pid, TITLE, host=process)
        self.action(process.pid, TITLE, "type", "y")
        self.copy(process.pid, TITLE, EXTERNAL + "xy")
        self.action(process.pid, TITLE, "chord", "z")
        self.copy(process.pid, TITLE, EXTERNAL + "x")
        self.action(process.pid, TITLE, "chord", "z")
        self.copy(process.pid, TITLE, EXTERNAL)
        self.note("editable: native typing and focus-separated undo groups passed")
        self.finish("editable", process, report)

    def guarded(self, mode):
        process, report = self.start(mode)
        if mode == "read-only":
            self.copy(process.pid, TITLE, ORIGINAL)
        self.external_copy()
        self.focus(process.pid, TITLE, host=process)
        generation = self.clipboard("state")["change_count"]
        self.action(process.pid, TITLE, "chord", "a")
        self.action(process.pid, TITLE, "type", "x")
        for key in ("51", "117", "36"):
            self.action(process.pid, TITLE, "keycode", key)
        self.action(process.pid, TITLE, "chord", "v")
        self.action(process.pid, TITLE, "chord", "x")
        self.action(process.pid, TITLE, "chord", "z")
        self.action(process.pid, TITLE, "shift-chord", "z")
        if mode == "read-only":
            self.clipboard("plain", ORIGINAL, changed_from=generation)
            self.copy(process.pid, TITLE, ORIGINAL)
            self.note("read-only: copy/cut available and native editing attempts preserved exact document")
        else:
            self.action(process.pid, TITLE, "chord", "c")
            state = self.clipboard("plain", EXTERNAL)
            if state["change_count"] != generation:
                raise RuntimeError("Disabled editor changed the external clipboard")
            self.note("disabled: native editing/copy/cut attempts preserved external clipboard")
        self.finish(mode, process, report)

    def cleanup(self):
        clean = True
        for process in self.hosts:
            if process.poll() is None:
                clean = False  # failure cleanup is not a successful graceful session
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=3)
        self.observations["cleanup"]["native_processes_reaped"] = all(p.poll() is not None for p in self.hosts)
        self.observations["cleanup"]["native_sessions_closed_gracefully"] = clean
        # TextEdit belongs to this probe only: preflight rejected an existing
        # process. Check executable identity again before signalling its PID.
        if self.textedit_pid is not None:
            result = self.run(["/bin/ps", "-p", self.textedit_pid, "-o", "comm="], check=False)
            if result.returncode == 0:
                if Path(result.stdout.strip()).name != "TextEdit":
                    raise RuntimeError("TextEdit PID identity changed before cleanup")
                os.kill(self.textedit_pid, signal.SIGTERM)
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline and self.textedit() == self.textedit_pid:
                    time.sleep(0.15)
                if self.textedit() == self.textedit_pid:
                    os.kill(self.textedit_pid, signal.SIGKILL)
                    deadline = time.monotonic() + 3
                    while time.monotonic() < deadline and self.textedit() == self.textedit_pid:
                        time.sleep(0.15)
            self.observations["cleanup"]["textedit_process_absent"] = self.textedit() is None
        else:
            self.observations["cleanup"]["textedit_process_absent"] = True
        for handle in self.handles:
            handle.close()
        required = ("native_processes_reaped", "native_sessions_closed_gracefully", "textedit_process_absent")
        if not all(self.observations["cleanup"].get(key) is True for key in required):
            self.observations["passed"] = False
            self.save()
            raise RuntimeError("Native/TextEdit processes did not all close and disappear")
        self.save()

    def execute(self):
        self.apple("preflight")
        if self.textedit() is not None:
            raise RuntimeError("Refusing to operate on an existing TextEdit process")
        result = self.run(["/usr/bin/xcrun", "swiftc", ROOT / "scripts/native-qa/macos_clipboard.swift", "-o", self.helper], timeout=60)
        (OUTPUT / "macos-helper-build.log").write_text(result.stdout + result.stderr, encoding="utf-8")
        self.clipboard("prepare", self.import_file, self.external_file)
        self.note("preflight: isolated hosted desktop and System Events Accessibility verified")
        self.editable()
        self.guarded("read-only")
        self.guarded("disabled")
        self.observations["passed"] = True


def interrupted(_signum, _frame):
    raise RuntimeError("Hosted native probe interrupted")


def main():
    if sys.platform != "darwin" or os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_OS") != "macOS":
        print("This clipboard/input probe runs only on an isolated hosted macOS Actions runner", file=sys.stderr)
        return 1
    revision = os.environ.get("TEXTLOOM_QA_REVISION", "")
    if not re.fullmatch(r"[0-9a-fA-F]{40}", revision) or not (ROOT / "target/debug/textloom-native-example").is_file():
        print("Missing full 40-hex QA revision or built native executable", file=sys.stderr)
        return 1
    OUTPUT.mkdir(parents=True, exist_ok=True)
    signal.signal(signal.SIGTERM, interrupted)
    with tempfile.TemporaryDirectory(prefix="textloom-macos-interaction-", dir=os.environ.get("RUNNER_TEMP")) as directory:
        probe = Probe(Path(directory), revision)
        try:
            probe.execute()
        except (Exception, KeyboardInterrupt) as error:
            probe.observations["failure"] = str(error)
            probe.observations["passed"] = False
            print(f"macOS native interaction failed: {error}", file=sys.stderr)
        finally:
            try:
                probe.cleanup()
            except Exception as error:
                probe.observations["cleanup_failure"] = str(error)
                probe.observations["passed"] = False
                probe.save()
                print(f"macOS native cleanup failed: {error}", file=sys.stderr)
        if not probe.observations["passed"]:
            return 1
    print("macOS native TextEdit, keyboard, undo/redo, focus, and editing guards passed; manual signoff remains separate")
    return 0


if __name__ == "__main__":
    sys.exit(main())
