#!/usr/bin/env python3
"""Disposable hosted-macOS winit startup and plain clipboard probe.

Run from the checkout after building scripts/windows-vm/winit-harness into
target. This sends System Events keys to its own naturally foreground window;
it never activates, raises, clicks, or repairs focus. Observations contain only
the public fixtures below. It does not test an IME or establish manual signoff.
"""

import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import sys
import time


FIXTURE = "Первая строка hello\nsecond line"
UNICODE = "café 日本語 e\u0301 👩🏽‍💻\nsecond\n"
SENTINEL = "textloom macOS winit copy must replace this sentinel"
INTERRUPTED = None
APPLE_SCRIPT = r'''
on run argv
    set commandName to item 1 of argv
    tell application "System Events"
        if not UI elements enabled then error "System Events Accessibility is unavailable"
        if commandName is "preflight" then return "accessibility-enabled"
        set targetPID to (item 2 of argv) as integer
        set targets to application processes whose unix id is targetPID
        if (count of targets) is not 1 then return "waiting-for-process"
        set targetProcess to item 1 of targets
        tell targetProcess
            set targetWindows to windows whose name is "tlqa-winit"
            if (count of targetWindows) is not 1 then return "waiting-for-window"
        end tell
        set frontProcess to first application process whose frontmost is true
        if (unix id of frontProcess) is not targetPID then
            if commandName is "inspect" then return "background-pid=" & (unix id of frontProcess)
            error "Owned winit process lost foreground; refusing native keys"
        end if
        tell targetProcess
            if name of window 1 is not "tlqa-winit" then error "Unexpected foreground window"
        end tell
        if commandName is "inspect" then return "natural-foreground"
        set keyNumber to (item 3 of argv) as integer
        if commandName is "plain" then
            key code keyNumber
        else if commandName is "command" then
            key code keyNumber using {command down}
        else if commandName is "command-shift" then
            key code keyNumber using {command down, shift down}
        else
            error "Unknown winit probe action"
        end if
        return "keys-sent"
    end tell
end run
'''


class ProbeFailure(RuntimeError):
    """An unmet native observation or a bounded probe failure."""


def require(condition, message):
    if not condition:
        raise ProbeFailure(message)


def expected_state(text, selection):
    return {"doc": text, "selection": selection, "composition": None}


def check_interrupted():
    require(INTERRUPTED is None, f"Probe interrupted by signal {INTERRUPTED}")


def run(arguments, input_bytes=None):
    check_interrupted()
    result = subprocess.run(
        arguments, input=input_bytes, capture_output=True, timeout=8, check=False,
        env={**os.environ, "LC_ALL": "en_US.UTF-8"},
    )
    require(result.returncode == 0,
            f"{arguments[0]} exited {result.returncode}: "
            f"{result.stderr.decode('utf-8', errors='replace')}")
    check_interrupted()
    return result.stdout


def apple(action, pid=0, key=0):
    return run(["/usr/bin/osascript", "-", action, str(pid), str(key)],
               APPLE_SCRIPT.encode("utf-8")).decode("utf-8").strip()


def clipboard_write(text):
    run(["/usr/bin/pbcopy"], text.encode("utf-8"))


def clipboard_read():
    # Do not decode with replacement, strip newlines, or normalize Unicode.
    return run(["/usr/bin/pbpaste", "-Prefer", "txt"])


class Probe:
    def __init__(self, executable, output, revision):
        self.executable = executable
        self.output = output
        self.report = output / "macos-winit.json"
        self.log = output / "macos-winit.events.jsonl"
        self.stderr = output / "macos-winit.stderr.log"
        self.child = None
        self.observations = {
            "schema_version": 1,
            "revision": revision,
            "os": "macos",
            "os_version": platform.mac_ver()[0],
            "architecture": platform.machine(),
            "runner_environment": os.environ.get("RUNNER_ENVIRONMENT"),
            "binary_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
            "native_input": "osascript/System Events key code",
            "focus_policy": "observe natural activation; no focus repair",
            "manual_signoff": "not_recorded",
            "ime_tested": False,
            "passed": False,
            "checks": [],
        }

    def save(self):
        self.report.write_text(json.dumps(self.observations, ensure_ascii=False,
                                         indent=2) + "\n", encoding="utf-8")

    def events(self):
        check_interrupted()
        require(self.child is not None, "Harness has not been launched")
        require(self.child.poll() is None,
                f"Harness exited early: {self.child.returncode}; see {self.stderr}")
        # The last line may still be in flight. All preceding lines must be JSON.
        lines = self.log.read_bytes().split(b"\n")[:-1]
        events = [json.loads(line) for line in lines]
        for sequence, event in enumerate(events, 1):
            require(event.get("seq") == sequence, "Missing/out-of-order harness JSON event")
            require("error" not in event, f"Harness error: {event}")
        return events

    def wait(self, label, predicate, timeout=8):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            events = self.events()
            if predicate(events):
                return events
            time.sleep(0.05)
        raise ProbeFailure(f"Timed out waiting for {label}; see {self.log}")

    def check(self, label, **evidence):
        self.observations["checks"].append({"name": label, "passed": True, **evidence})
        self.save()

    @staticmethod
    def latest_state(events):
        states = [event["state"] for event in events if "state" in event]
        return states[-1] if states else None

    @staticmethod
    def keys(events, after, letter):
        modifiers = ""
        matching = []
        for event in events:
            native = event.get("input", {})
            if native.get("kind") == "modifiers":
                modifiers = native["state"]
            if (event["seq"] > after and native.get("kind") == "key"
                    and native.get("physical") == f"Code(Key{letter.upper()})"
                    and native.get("state") == "Pressed"):
                # Count first: repeats or unhandled presses can leave the same
                # final state (notably redo), but must still fail the action.
                matching.append((event, modifiers))
        return matching

    def action(self, label, code, letter, state, command=True, shift=False,
               extra=lambda events: True):
        before = self.events()
        after = before[-1]["seq"] if before else 0
        mode = "command-shift" if shift else "command" if command else "plain"
        require(apple(mode, self.child.pid, code) == "keys-sent",
                f"Owned foreground window unavailable for {label}")

        def witnessed(events):
            keys = self.keys(events, after, letter)
            if len(keys) != 1:
                return False
            event, modifiers = keys[0]
            native = event["input"]
            return (native.get("synthetic") is False and native.get("repeat") is False
                    and event.get("outcome", {}).get("handled") is True
                    and ("SUPER" in modifiers) == command
                    and ("SHIFT" in modifiers) == shift
                    and "CONTROL" not in modifiers and "ALT" not in modifiers
                    and self.latest_state(events) == state
                    and extra([event for event in events if event["seq"] > after]))

        self.wait(label, witnessed)
        # Include queued key-up/IME/clipboard work before accepting exact state.
        time.sleep(0.25)
        events = self.events()
        require(witnessed(events), f"{label} changed after initial observation")
        keys = self.keys(events, after, letter)
        require(apple("inspect", self.child.pid) == "natural-foreground",
                f"Owned window lost foreground during {label}")
        self.check(label, after_seq=after, through_seq=events[-1]["seq"],
                   key_seq=keys[0][0]["seq"], state=state)
        return [event for event in events if event["seq"] > after]

    def execute(self):
        require(apple("preflight") == "accessibility-enabled", "Accessibility unavailable")
        self.save()
        with self.log.open("wb") as stdout, self.stderr.open("wb") as stderr:
            self.child = subprocess.Popen(
                [str(self.executable)], stdin=subprocess.DEVNULL,
                stdout=stdout, stderr=stderr, start_new_session=True,
            )
        self.observations["pid"] = self.child.pid
        start = time.monotonic()
        events = self.wait("ready and natural window focus", lambda events: (
            any(event.get("event") == "ready" and event.get("backend") == "macos"
                and event.get("pid") == self.child.pid
                and event.get("revision") == self.observations["revision"]
                and event.get("state") == expected_state(FIXTURE, [0, 0])
                for event in events)
            and any(event.get("event") == "sync_ime"
                    and event.get("window_has_focus") is True
                    and event.get("adapter_focused") is True for event in events)
        ))
        # AX registration can lag winit's ready event. Observe only, never activate.
        deadline = start + 12
        foreground = "not-observed"
        while time.monotonic() < deadline:
            foreground = apple("inspect", self.child.pid)
            self.observations["last_startup_foreground"] = foreground
            if foreground == "natural-foreground":
                break
            time.sleep(0.1)
        require(foreground == "natural-foreground",
                f"winit did not activate naturally: {foreground}")
        self.check("natural-startup-focus", through_seq=events[-1]["seq"],
                   elapsed_ms=round((time.monotonic() - start) * 1000))

        typed = "x" + FIXTURE
        self.action("first-native-key-without-focus-repair", 7, "x",
                    expected_state(typed, [1, 1]), command=False)
        selected = expected_state(typed, [0, len(typed)])
        self.action("command-select-all", 0, "a", selected)
        clipboard_write(UNICODE)
        require(clipboard_read() == UNICODE.encode("utf-8"), "Unicode source clipboard mismatch")
        pasted = expected_state(UNICODE, [len(UNICODE), len(UNICODE)])

        def paste_evidence(events):
            requests = [event for event in events
                        if event.get("outcome", {}).get("clipboard") == "paste_request"]
            pastes = [event for event in events if event.get("event") == "paste"]
            return (len(requests) == 1 and len(pastes) == 1
                    and pastes[0].get("clipboard_text") == UNICODE
                    and pastes[0].get("state") == pasted
                    and pastes[0].get("outcome", {}).get("handled") is True
                    and pastes[0].get("outcome", {}).get("changed") is True)

        self.action("command-paste-unicode", 9, "v", pasted, extra=paste_evidence)
        self.action("one-command-undo-restores-document-and-selection", 6, "z", selected)
        self.action("one-command-shift-redo-restores-paste", 6, "z", pasted, shift=True)
        self.action("command-select-pasted-text", 0, "a",
                    expected_state(UNICODE, [0, len(UNICODE)]))
        clipboard_write(SENTINEL)
        require(clipboard_read() == SENTINEL.encode("utf-8"), "Copy sentinel was not installed")

        def copy_evidence(events):
            writes = [event for event in events if event.get("event") == "clipboard_write"]
            return (len(writes) == 1 and writes[0].get("action") == "copy"
                    and writes[0].get("program") == "/usr/bin/pbcopy"
                    and writes[0].get("ok") is True and writes[0].get("text") == UNICODE
                    and any(event.get("outcome", {}).get("clipboard") == {"copy": UNICODE}
                            for event in events))

        self.action("command-copy-unicode", 8, "c",
                    expected_state(UNICODE, [0, len(UNICODE)]), extra=copy_evidence)
        actual = clipboard_read()
        (self.output / "macos-winit.copied.txt").write_bytes(actual)
        require(actual == UNICODE.encode("utf-8"), "Native copy changed exact Unicode bytes")
        self.check("fresh-native-clipboard-roundtrip", expected=UNICODE,
                   utf8_bytes=len(actual), replaced_sentinel=True)
        self.observations["passed"] = True

    def cleanup(self):
        if self.child is not None:
            # Only the process group created by this probe, never a name/global kill.
            # The leader can exit while its clipboard helper is still running.
            def signal_group(signum):
                try:
                    os.killpg(self.child.pid, signum)
                    return True
                except ProcessLookupError:
                    return False
                except PermissionError:
                    # Darwin can return EPERM for a terminated group whose
                    # leader became a zombie between poll() and killpg(). The
                    # group still exists: keep reaping and require ESRCH before
                    # reporting success. A genuinely unsignalable live group
                    # therefore still fails the bounded cleanup below.
                    return True

            for signum in (signal.SIGTERM, signal.SIGKILL):
                signal_group(signum)
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline:
                    self.child.poll()  # Reap the leader while observing the group.
                    if not signal_group(0):
                        self.child.wait(timeout=1)
                        self.observations["cleanup"] = "owned process group stopped; harness reaped"
                        return
                    time.sleep(0.05)
            raise ProbeFailure("Owned harness process group survived bounded cleanup")


def interrupted(signum, frame):
    # Defer raising until an explicit checkpoint, so a signal cannot interrupt
    # Popen before registration or prevent bounded cleanup of the owned group.
    global INTERRUPTED
    INTERRUPTED = signum


def main():
    # Guard before querying System Events, changing clipboard, or launching a GUI.
    if (sys.platform != "darwin" or os.environ.get("GITHUB_ACTIONS") != "true"
            or os.environ.get("RUNNER_OS") != "macOS"
            or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"):
        print("This input/clipboard probe runs only on a disposable GitHub-hosted macOS runner",
              file=sys.stderr)
        return 1
    revision = os.environ.get("TEXTLOOM_QA_REVISION", "")
    root = Path(__file__).resolve().parents[2]
    executable = root / "target/debug/tlqa-winit-harness"
    if not re.fullmatch(r"[0-9a-fA-F]{40}", revision) or not executable.is_file():
        print("Missing full 40-hex QA revision or built winit harness", file=sys.stderr)
        return 1
    output = root / "native-qa-observations"
    output.mkdir(exist_ok=True)
    probe = Probe(executable, output, revision)
    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGALRM):
        signal.signal(signum, interrupted)
    signal.alarm(90)
    try:
        probe.execute()
        check_interrupted()
    except Exception as error:
        probe.observations.update(passed=False, failure=f"{type(error).__name__}: {error}")
        print(f"macOS winit native probe failed: {error}", file=sys.stderr)
    finally:
        signal.alarm(0)
        try:
            probe.cleanup()
        except Exception as error:
            probe.observations.update(passed=False, cleanup_failure=str(error))
        if INTERRUPTED is not None:
            probe.observations.update(passed=False, interrupted_signal=INTERRUPTED)
        probe.save()
    if probe.observations["passed"]:
        print("macOS winit native startup and Unicode clipboard/history passed; manual signoff remains separate")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
