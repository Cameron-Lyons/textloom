#!/usr/bin/env python3
"""Windows checks for the Textloom native host, in the winvm.py VM.

usage: checks.py build                  cross-compile the host and the winit harness for x86_64-pc-windows-gnu
       checks.py deploy                 copy them and the guest helpers into the VM
       checks.py explore                start the host and print its UI Automation tree
       checks.py [--runs N] CHECK...    run checks ("all" for every check)
       checks.py list

The VM must be up (winvm.py up). Keys and clicks reach Windows as hardware
input from QEMU's PS/2 keyboard and USB tablet; editor state is read through
UI Automation. Results accumulate in results.json under the VM's state
directory, with screenshots per check.
"""

import json
import os
import subprocess
import sys
import time
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import winvm  # noqa: E402

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
TARGET = "x86_64-pc-windows-gnu"
HOST_EXE = REPO / "target" / TARGET / "release" / "textloom-native-example.exe"
WINIT_EXE = REPO / "target" / "winit-harness" / TARGET / "release" / "tlqa-winit-harness.exe"
OUT = Path(os.environ.get("TLQA_WINDOWS_OUT", winvm.STATE / "results"))
RESULTS = OUT / "results.json"
TEXT_FIELDS = ("Document", "Find", "Replace")
CHECKS = {}


def check(name):
    def register(function):
        CHECKS[name] = function
        return function
    return register


def utf16(text, index):
    """The UTF-16 offset UI Automation uses for a Python string index."""
    return len(text[:index].encode("utf-16-le")) // 2


def codepoints(text, offset):
    """The Python string index of a UTF-16 offset."""
    units = 0
    for index, character in enumerate(text):
        if units >= offset:
            return index
        units += 2 if ord(character) > 0xFFFF else 1
    return len(text)


def guest(script, admin=False, timeout=180):
    """Run PowerShell with qa.ps1 loaded; return the JSON object from its last output line."""
    result = winvm.run(". C:\\textloom-qa\\qa.ps1\n" + script, admin, timeout)
    lines = [line for line in result["output"].splitlines() if line.strip()]
    if result["exit"] != 0 or result.get("timed_out"):
        raise RuntimeError(f"guest job failed ({result['exit']}): {result['output'][-3000:]}")
    for line in reversed(lines):
        if line.lstrip().startswith(("{", "[")):
            return json.loads(line)
    return None


class Session:
    """One check: a host started in the VM, input through QMP, screenshots under OUT/<name>."""

    def __init__(self, name):
        self.name = name
        self.out = OUT / name
        self.out.mkdir(parents=True, exist_ok=True)
        self.monitor = winvm.qmp()
        self.size = self.monitor.size()

    def close(self):
        try:
            # Windows switches input methods system-wide; leave US English for the next check.
            guest("Set-InputMethod 'en-US' | Out-Null; Stop-TextloomHost; Stop-WinitHarness; Out-Json @{}")
        finally:
            self.monitor.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

    def start(self, *arguments):
        # Close the Start menu or any other transient shell UI first.
        self.monitor.chord("esc")
        quoted = ", ".join("'" + argument.replace("'", "''") + "'" for argument in arguments)
        self.pid = guest(f"$id = Start-TextloomHost @({quoted}); Out-Json @{{ pid = $id }}")["pid"]
        time.sleep(1.5)
        self.input_method("en-US")
        return self

    def winit(self, *arguments):
        self.monitor.chord("esc")
        return Winit(self, arguments)

    def running(self):
        return guest("Out-Json (Get-HostExit)")["running"]

    def state(self, name="Document"):
        """Text, selection (as Python string indexes), focus, and caret box of a text field."""
        state = guest(f"Out-Json (Get-TextState '{name}')")
        if state.get("selection"):
            state["selection"] = [codepoints(state["text"], offset) for offset in state["selection"]]
        return state

    def wait_state(self, predicate, timeout=6.0, name="Document"):
        """Poll until predicate(state) holds; the software renderer can take a few frames to apply input."""
        deadline = time.monotonic() + timeout
        while True:
            state = self.state(name)
            if predicate(state) or time.monotonic() > deadline:
                return state
            time.sleep(0.3)

    def rect(self, name, kind=""):
        return guest(f"Out-Json @{{ rect = Get-ElementRect '{name}' '{kind}' }}")["rect"]

    def click_element(self, name, dx=None, dy=None, kind=""):
        if not kind and name in TEXT_FIELDS:
            kind = "Edit"
        x, y, width, height = self.rect(name, kind)
        self.monitor.click(x + (width // 2 if dx is None else dx), y + (height // 2 if dy is None else dy), size=self.size)
        time.sleep(0.5)

    def focus_document(self):
        # Near the top-left of the document, inside its first line.
        self.click_element("Document", dx=40, dy=20)

    def paragraph_style(self):
        """The toolbar's paragraph style box, which follows the caret's paragraph."""
        return guest("Out-Json @{ value = Get-ParagraphStyle }")["value"]

    def window_rect(self):
        return guest("Out-Json @{ rect = Get-Rect (Get-HostWindow).Current.BoundingRectangle }")["rect"]

    def status_band(self):
        """The status row, which shows "IME composing" while a composition is open."""
        x, _, width, _ = self.window_rect()
        _, y, _, height = self.rect("Ready", "Text")
        return [x, y - 2, width, height + 4]

    def kinds(self):
        return [paragraph["kind"] for paragraph in guest("Out-Json (Get-ParagraphKinds)")]

    def status(self):
        return guest("Out-Json (Get-Status)")

    def select(self, find, occurrence=1, caret=None, field="Document"):
        """Select the nth occurrence of `find`, or put the caret at its start or end."""
        text = self.state(field)["text"]
        start = -1
        for _ in range(occurrence):
            start = text.index(find, start + 1)
        end = start + len(find)
        if caret == "start":
            end = start
        elif caret == "end":
            start = end
        guest(f"Select-HostRange {utf16(text, start)} {utf16(text, end)} '{field}'; Out-Json @{{}}")
        time.sleep(0.3)
        return start, end

    def keys(self, *chords, pause=0.15):
        for chord in chords:
            self.monitor.chord(chord)
            time.sleep(pause)

    def type(self, text):
        self.monitor.type(text)
        time.sleep(0.3)

    def input_method(self, language):
        layout = guest(f"Out-Json @{{ layout = Set-InputMethod '{language}' }}")["layout"]
        # Keys sent sooner can still be translated with the previous layout.
        time.sleep(1.5)
        return layout

    def toplevels(self):
        return guest("Out-Json (Get-Toplevels)")

    def screenshot(self, label):
        return self.monitor.screenshot(self.out / f"{label}.png")


class Winit:
    """The winit harness: one editor through textloom's winit adapter, drawn as
    10x24 px cells from (20, 20) in its client area, logging JSON events."""

    def __init__(self, session, arguments):
        self.s = session
        quoted = ", ".join("'" + argument + "'" for argument in arguments)
        started = guest(f"Out-Json (Start-WinitHarness @({quoted}))")
        self.origin = started["origin"]
        time.sleep(1.0)

    def events(self, after=0):
        return guest(f"Out-Json @{{ events = @(Get-WinitEvents {after}) }}")["events"]

    def state(self):
        return [event for event in self.events() if "state" in event][-1]["state"]

    def last(self, kind, after=0):
        found = [event for event in self.events(after) if event.get("event") == kind]
        return found[-1] if found else None

    def seq(self):
        events = self.events()
        return events[-1]["seq"] if events else 0

    def cell(self, line, column):
        """Screen rectangle of a cell, in physical pixels at 100% scale."""
        return [self.origin[0] + 20 + column * 10, self.origin[1] + 20 + line * 24, 10, 24]

    def focus(self):
        # Below the text, so the click only focuses the window.
        self.s.monitor.click(self.origin[0] + 400, self.origin[1] + 300, size=self.s.size)
        time.sleep(0.8)


def record(name, result):
    OUT.mkdir(parents=True, exist_ok=True)
    results = json.loads(RESULTS.read_text()) if RESULTS.exists() else {}
    results[name] = result
    RESULTS.write_text(json.dumps(results, indent=1, ensure_ascii=False))


# --------------------------------------------------------------------------- setup commands

def build():
    revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO, capture_output=True, text=True,
                              check=True).stdout.strip()
    subprocess.run(["cargo", "build", "--locked", "--release", "--target", TARGET, "--manifest-path",
                    str(REPO / "examples/native-editor/Cargo.toml"), "--target-dir", str(REPO / "target")],
                   check=True, env=dict(os.environ, TEXTLOOM_QA_REVISION=revision))
    subprocess.run(["cargo", "build", "--locked", "--release", "--target", TARGET, "--manifest-path",
                    str(HERE / "winit-harness/Cargo.toml"), "--target-dir", str(REPO / "target" / "winit-harness")],
                   check=True)
    print(HOST_EXE)
    print(WINIT_EXE)


def deploy():
    for path in (HOST_EXE, WINIT_EXE):
        if not path.exists():
            winvm.fail(f"{path} is missing; run `checks.py build`")
    for path in (HOST_EXE, WINIT_EXE, HERE / "guest" / "qa.ps1"):
        winvm.share(path)
    winvm.run(
        "$ErrorActionPreference = 'Stop'\n"
        "New-Item -ItemType Directory -Force C:\\textloom-qa\\host, C:\\textloom-qa\\winit | Out-Null\n"
        "Get-Process textloom-native-example, tlqa-winit-harness -ErrorAction SilentlyContinue | Stop-Process -Force\n"
        "Get-HostFile qa.ps1 C:\\textloom-qa\\qa.ps1\n"
        "Get-HostFile textloom-native-example.exe C:\\textloom-qa\\host\\textloom-native-example.exe\n"
        "Get-HostFile tlqa-winit-harness.exe C:\\textloom-qa\\winit\\tlqa-winit-harness.exe\n"
        "Copy-Item C:\\textloom-qa\\mesa\\*.dll C:\\textloom-qa\\host -Force\n", timeout=300)
    print("Deployed the host, the winit harness, and qa.ps1.")


def explore():
    with Session("explore") as s:
        s.start()
        s.focus_document()
        s.screenshot("start")
        print(json.dumps(guest("Out-Json (Get-Tree 6)"), indent=1, ensure_ascii=False))
        print(json.dumps(s.state(), ensure_ascii=False))
        print(json.dumps(s.kinds(), ensure_ascii=False))
        print(json.dumps(s.status(), ensure_ascii=False))
        print(json.dumps(s.toplevels(), ensure_ascii=False))


# --------------------------------------------------------------------------- checks

@check("renderer-smoke")
def renderer_smoke():
    """The host renders 20 frames with Mesa in each mode, as the hosted Mesa job does."""
    revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO, capture_output=True, text=True,
                              check=True).stdout.strip()
    observed = {}
    for mode in ("editable", "read-only", "disabled"):
        extra = {"editable": "'--clipboard-self-test'", "read-only": "'--read-only'", "disabled": "'--disabled'"}[mode]
        report = f"C:\\textloom-qa\\smoke-{mode}.json"
        result = guest(
            "$env:PATH = \"C:\\msys64\\ucrt64\\bin;$env:PATH\"; $env:GALLIUM_DRIVER = 'llvmpipe'\n"
            "$env:GLUTIN_WGL_OPENGL_DLL = 'C:\\textloom-qa\\host\\opengl32.dll'\n"
            # The host refuses to replace an existing report.
            f"Remove-Item -LiteralPath {report} -ErrorAction SilentlyContinue\n"
            f"$out = & C:\\textloom-qa\\host\\textloom-native-example.exe --smoke-test --rich-clipboard {extra} "
            f"--qa-report {report} 2>&1 | ForEach-Object {{ \"$_\" }}\n"
            f"Send-HostFile {report} smoke-{mode}.json\n"
            "Out-Json @{ exit = $LASTEXITCODE; output = ($out -join \"`n\") }", timeout=180)
        validation = subprocess.run(
            [sys.executable, "-I", str(REPO / "scripts/check_native_report.py"), str(winvm.INBOX / f"smoke-{mode}.json"),
             "--revision", revision, "--os", "windows", "--mode", mode], capture_output=True, text=True)
        observed[mode] = {
            "rendered 20 frames": "Rendered 20 native frames" in result["output"],
            "clipboard roundtrip": mode != "editable" or "Native rich clipboard roundtrip and undo/redo passed" in result["output"],
            "report valid": validation.returncode == 0,
            "validator": (validation.stdout + validation.stderr).strip()[-500:],
        }
    passed = all(value for mode in observed.values() for key, value in mode.items() if key != "validator")
    return {"passed": passed, "observed": observed}


def undo_until(s, text, chord="ctrl+z", limit=6):
    """Press undo until the document text is `text`; returns the number of presses, or None."""
    current = s.state()["text"]
    for steps in range(limit + 1):
        if current == text:
            return steps
        s.keys(chord)
        previous = current
        current = s.wait_state(lambda state: state["text"] != previous, timeout=3)["text"]
    return None


@check("german-dead-keys-and-altgr")
def german_dead_keys_and_altgr():
    """German layout: dead acute + e, AltGr+Q/E, Ctrl+Alt+Q (Windows' AltGr), and undo with QWERTZ Ctrl+Z."""
    with Session("german") as s:
        s.start()
        s.focus_document()
        s.select("writing", caret="start")
        before = s.state()
        offset = before["selection"][0]
        layout = s.input_method("de-DE")
        # ´ is a dead key at the US = position. winit 0.30 loses the first dead key
        # after a layout switch (it prepares the new layout on that key press and
        # consumes the key's state; see RELEASING.md), so that is recorded but not
        # required here.
        s.keys("equal", "e")
        first = s.wait_state(lambda state: state["text"] != before["text"])["text"][offset:offset + 1]
        guest(f"Select-HostRange {utf16(before['text'], offset)} {utf16(before['text'], offset) + 1}; Out-Json @{{}}")
        s.keys("equal", "e")
        after_dead = s.wait_state(lambda state: state["text"][offset:offset + 1] != first or first == "é")
        s.keys("alt_r+q", "alt_r+e")
        after_altgr = s.wait_state(lambda state: len(state["text"]) >= len(after_dead["text"]) + 2)
        s.screenshot("typed")
        # QWERTZ: the physical Y key is Z, and Latin logical keys select shortcuts.
        undo_steps = undo_until(s, before["text"], chord="ctrl+y")
        restored = s.state()
        # Windows also accepts Ctrl+Alt for AltGr. egui-winit drops typed text while
        # Ctrl is held, so egui never types it, and egui's default quit shortcut,
        # Ctrl+Q matched ignoring Alt, takes this chord and closes the window.
        s.keys("ctrl+alt+q")
        time.sleep(1.0)
        survived = s.running()
        after_ctrl_alt = s.state() if survived else None
        expectations = {
            "the German layout is active": layout.endswith("0407"),
            "dead acute + e types é": after_dead["text"][offset:offset + 1] == "é"
                and len(after_dead["text"]) == len(before["text"]) + 1,
            "AltGr+Q and AltGr+E type @€": after_altgr["text"][offset:offset + 3] == "é@€",
            "Ctrl+physical Y (Z on QWERTZ) undoes the typing": undo_steps is not None,
            "undo restores the caret": restored["selection"] == before["selection"],
            "the document keeps focus": after_altgr["focused"],
            "Ctrl+Alt+Q leaves the window open": survived,
        }
        return {"passed": all(expectations.values()), "expectations": expectations, "layout": layout,
                "first dead key after the switch": first, "typed": after_altgr["text"][offset:offset + 3],
                "Ctrl+Alt+Q typed": after_ctrl_alt["text"][offset:offset + 1] if after_ctrl_alt else None,
                "undo_steps": undo_steps}


@check("windows-clipboard-keys")
def windows_clipboard_keys():
    """Ctrl+Insert copies, Shift+Delete cuts, and Shift+Insert pastes, with exact text and undo."""
    clipboard = "Out-Json @{ text = (Get-Clipboard -Raw) }"
    with Session("clipboard-keys") as s:
        s.start()
        s.focus_document()
        s.select("format")
        before = s.state()
        start, end = before["selection"]
        s.keys("ctrl+insert")
        time.sleep(0.5)
        copied = guest(clipboard)["text"]
        s.keys("shift+delete")
        after_cut = s.wait_state(lambda state: state["text"] != before["text"])
        cut = guest(clipboard)["text"]
        s.keys("shift+insert")
        after_paste = s.wait_state(lambda state: state["text"] != after_cut["text"])
        s.keys("ctrl+z")
        undo_paste = s.wait_state(lambda state: state["text"] != after_paste["text"])["text"]
        s.keys("ctrl+z")
        undo_cut = s.wait_state(lambda state: state["text"] != undo_paste)["text"]
        expectations = {
            "Ctrl+Insert copies the selection": copied == "format",
            "Shift+Delete removes the selection": after_cut["text"] == before["text"][:start] + before["text"][end:],
            "Shift+Delete puts it on the clipboard": cut == "format",
            "Shift+Insert pastes it back": after_paste["text"] == before["text"],
            "one undo removes the paste": undo_paste == after_cut["text"],
            "a second undo restores the cut text": undo_cut == before["text"],
        }
        around = lambda text: text[max(start - 8, 0):end + 8]
        observed = {"after cut": around(after_cut["text"]), "after paste": around(after_paste["text"]),
                    "undo 1": around(undo_paste), "undo 2": around(undo_cut)}
        return {"passed": all(expectations.values()), "expectations": expectations, "observed": observed}


@check("azerty-digit-row-shortcuts")
def azerty_digit_row_shortcuts():
    """French AZERTY: Ctrl+physical 1/3/0 and Ctrl+Shift+physical 7/8 change the paragraph kind; plain keys type &é."""
    with Session("azerty") as s:
        s.start()
        s.focus_document()
        layout = s.input_method("fr-FR")
        observed = {}
        for label, chord in (("Ctrl+1", "ctrl+1"), ("Ctrl+0", "ctrl+0"), ("Ctrl+3", "ctrl+3"), ("Ctrl+0 again", "ctrl+0"),
                             ("Ctrl+Shift+7", "ctrl+shift+7"), ("Ctrl+Shift+8", "ctrl+shift+8")):
            s.select("Select", caret="end")
            s.keys(chord)
            time.sleep(0.8)
            observed[label] = s.paragraph_style()
        start, _ = s.select("Select", caret="start")
        before = s.state()
        s.keys("1", "2")
        typed = s.wait_state(lambda state: len(state["text"]) >= len(before["text"]) + 2)["text"][start:start + 2]
        expectations = {
            "the French layout is active": layout.endswith("040C"),
            "Ctrl+physical 1 makes Heading 1": observed["Ctrl+1"] == "Heading 1",
            "Ctrl+physical 0 makes body text": observed["Ctrl+0"] == "Body",
            "Ctrl+physical 3 makes Heading 3": observed["Ctrl+3"] == "Heading 3",
            "Ctrl+physical 0 restores body text": observed["Ctrl+0 again"] == "Body",
            "Ctrl+Shift+physical 7 makes a numbered list": observed["Ctrl+Shift+7"] == "Numbered list",
            "Ctrl+Shift+physical 8 makes bullets": observed["Ctrl+Shift+8"] == "Bullets",
            "unmodified digit-row keys type &é": typed == "&é",
        }
        return {"passed": all(expectations.values()), "expectations": expectations, "observed": observed, "typed": typed}


# --------------------------------------------------------------------------- IME

TASKBAR = (0, 752, 1280, 48)


def popup_rect(before, after, ignore=()):
    """Bounding box of what appeared between two screenshots, outside `ignore`.

    IME candidate lists are not top-level windows or UI Automation elements, so
    they are found as the union of the regions that changed."""
    draw = []
    for x, y, width, height in ignore:
        draw += ["-draw", f"rectangle {x},{y} {x + width},{y + height}"]
    output = subprocess.run(
        ["magick", str(before), str(after), "-compose", "difference", "-composite", "-colorspace", "gray",
         "-threshold", "6%", "-fill", "black", *draw, "-morphology", "Close", "Disk:6",
         "-define", "connected-components:verbose=true", "-define", "connected-components:area-threshold=400",
         "-connected-components", "8", "null:"], capture_output=True, text=True, check=True).stdout
    boxes = []
    for line in output.splitlines()[1:]:
        fields = line.split()
        if len(fields) < 5 or not fields[4].startswith(("gray(255", "graya(255")):
            continue
        width, height, x, y = (int(value) for value in fields[1].replace("x", "+").split("+"))
        boxes.append((x, y, x + width, y + height))
    if not boxes:
        return None
    left, top = min(box[0] for box in boxes), min(box[1] for box in boxes)
    right, bottom = max(box[2] for box in boxes), max(box[3] for box in boxes)
    return [left, top, right - left, bottom - top]


def line_band(s, box):
    """The full-width row of a text line, which changes as a composition is typed."""
    x, y, width, height = s.window_rect()
    return [x, box[1] - 4, width, box[3] + 8]


def beside(panel, line, slack=40):
    """A panel starts near the composition and sits just below or above its line without covering it."""
    if not panel or not line:
        return False
    x, y, width, height = panel
    line_x, line_y, _, line_height = line
    below = -3 <= y - (line_y + line_height) <= slack
    above = -3 <= line_y - (y + height) <= slack
    return line_x - slack <= x <= line_x + slack and (below or above)


def on_screen(panel):
    return bool(panel) and panel[0] >= 0 and panel[1] >= 0 and panel[0] + panel[2] <= 1280 and panel[1] + panel[3] <= TASKBAR[1]


def wait_composing(s, composing=True, timeout=4.0):
    deadline = time.monotonic() + timeout
    while True:
        state = "IME composing" in s.status()
        if state == composing or time.monotonic() > deadline:
            return state
        time.sleep(0.3)


def ime_on(s, language, toggle):
    """Switch to an IME and make sure it composes: a probe key either composes
    (then Escape cancels it) or types a letter (then Backspace removes it and
    `toggle` switches the IME's input mode)."""
    layout = s.input_method(language)
    for _ in range(3):
        s.keys("n")
        if wait_composing(s, timeout=2.0):
            s.keys("esc")
            wait_composing(s, composing=False)
            return layout
        s.keys("backspace")
        time.sleep(0.4)
        s.keys(toggle)
        time.sleep(0.8)
    raise RuntimeError(f"the {language} IME did not start composing")


def japanese_on(s):
    # Alt+` turns the Microsoft Japanese IME's kana input on and off on a US keyboard.
    return ime_on(s, "ja-JP", "alt+grave_accent")


def pinyin_on(s):
    # Shift switches Microsoft Pinyin between Chinese and English input.
    return ime_on(s, "zh-CN", "shift")


@check("japanese-ime-replacement")
def japanese_ime_replacement():
    """Microsoft Japanese IME replaces a selected word: suggestion and candidate lists beside it, one commit, undo/redo."""
    with Session("japanese") as s:
        s.start()
        s.focus_document()
        japanese_on(s)
        start, end = s.select("format")
        before = s.state()
        line = before["caret_box"]
        ignore = [TASKBAR, line_band(s, line), s.status_band()]
        base = s.screenshot("base")
        s.type("nihongo")
        composing = wait_composing(s)
        during = s.state()
        preedit = s.screenshot("preedit")
        suggestions = popup_rect(base, preedit, ignore)
        s.keys("spc")
        time.sleep(1.0)
        converted = s.screenshot("converted")
        s.keys("spc")
        time.sleep(1.5)
        listed = s.screenshot("candidates")
        candidates = popup_rect(converted, listed, ignore)
        # The second Space moved to candidate 2; go back to the first and commit it.
        s.keys("up")
        time.sleep(0.5)
        s.keys("ret")
        committed = s.wait_state(lambda state: state["text"] != before["text"])
        still_composing = wait_composing(s, composing=False)
        s.keys("ctrl+z")
        undone = s.wait_state(lambda state: state["text"] == before["text"])
        s.keys("ctrl+y")
        redone = s.wait_state(lambda state: state["text"] != before["text"])
        expected = before["text"][:start] + "日本語" + before["text"][end:]
        expectations = {
            "typing composes": composing,
            "the suggestion list opens beside the composition": beside(suggestions, line),
            "the candidate list opens beside the composition": beside(candidates, line),
            "the candidate list stays on screen": on_screen(candidates),
            "Enter commits 日本語 over the selection once": committed["text"] == expected,
            "the caret follows the committed text": committed["selection"] == [start + 3, start + 3],
            "the composition ends": not still_composing,
            "one undo restores the word and its selection": undone["text"] == before["text"]
                and undone["selection"] == [start, end],
            "redo reapplies the commit": redone["text"] == expected,
        }
        return {"passed": all(expectations.values()), "expectations": expectations, "composition line": line,
                "suggestions": suggestions, "candidates": candidates,
                "accessible text while composing": during["text"][start:start + 8],
                "accessible caret while composing": during["caret_box"]}


@check("chinese-pinyin")
def chinese_pinyin():
    """Microsoft Pinyin: nihao shows candidates beside the composition; Space commits 你好 at the caret; undo."""
    with Session("pinyin") as s:
        s.start()
        s.focus_document()
        pinyin_on(s)
        start, _ = s.select("writing", caret="start")
        before = s.state()
        line = before["caret_box"]
        ignore = [TASKBAR, line_band(s, line), s.status_band()]
        base = s.screenshot("base")
        s.type("nihao")
        composing = wait_composing(s)
        time.sleep(0.8)
        listed = s.screenshot("candidates")
        candidates = popup_rect(base, listed, ignore)
        s.keys("spc")
        committed = s.wait_state(lambda state: state["text"] != before["text"])
        still_composing = wait_composing(s, composing=False)
        s.keys("ctrl+z")
        undone = s.wait_state(lambda state: state["text"] == before["text"])
        expected = before["text"][:start] + "你好" + before["text"][start:]
        expectations = {
            "typing composes": composing,
            "the candidate list opens beside the composition": beside(candidates, line),
            "the candidate list stays on screen": on_screen(candidates),
            "Space commits 你好 at the caret once": committed["text"] == expected,
            "the composition ends": not still_composing,
            "one undo removes it": undone["text"] == before["text"] and undone["selection"] == [start, start],
        }
        return {"passed": all(expectations.values()), "expectations": expectations, "composition line": line,
                "candidates": candidates}


@check("ime-commit-once")
def ime_commit_once():
    """Mid-composition, a click in the document or in Find commits the Japanese preedit once; Escape cancels it."""
    outcomes = {}
    with Session("commit-once") as s:
        for scenario in ("click in the document", "click in Find", "Escape"):
            s.start()
            s.focus_document()
            japanese_on(s)
            start, _ = s.select("writing", caret="start")
            before = s.state()
            s.type("nihongo")
            composing = wait_composing(s)
            if scenario == "click in the document":
                x, y, width, height = before["rect"]
                s.click_element("Document", dx=width // 2, dy=height - 30)
            elif scenario == "click in Find":
                s.click_element("Find")
            else:
                s.keys("esc")
            time.sleep(1.2)
            after = s.state()
            s.screenshot(scenario.replace(" ", "-"))
            outcome = {
                "composing before": composing,
                "insertions": after["text"].count("にほんご") - before["text"].count("にほんご"),
                "at the original caret": after["text"][start:start + 4] == "にほんご",
                "still composing": "IME composing" in s.status(),
            }
            if scenario == "click in Find":
                outcome["Find text"] = s.state("Find")["text"]
            outcomes[scenario] = outcome
    document, find, escape = (outcomes[name] for name in ("click in the document", "click in Find", "Escape"))
    expectations = {
        "every scenario composed": all(outcome["composing before"] for outcome in outcomes.values()),
        "a click in the document commits the preedit once where it was typed":
            document["insertions"] == 1 and document["at the original caret"],
        "a click in Find commits it once in the document":
            find["insertions"] == 1 and find["at the original caret"] and find["Find text"] == "",
        "Escape cancels the composition": escape["insertions"] == 0,
        "no composition is left open": not any(outcome["still composing"] for outcome in outcomes.values()),
    }
    return {"passed": all(expectations.values()), "expectations": expectations, "outcomes": outcomes}


@check("ime-long-document")
def ime_long_document():
    """Composing on the last line of a 300-line document keeps the caret visible and the candidate list beside it."""
    with Session("long-document") as s:
        s.start()
        s.focus_document()
        guest("Set-Clipboard -Value ((1..300 | ForEach-Object { \"Line $_ of a long document\" }) -join \"`r`n\"); Out-Json @{}")
        original = s.state()["text"]
        s.keys("ctrl+end", "ret", "ctrl+v")
        pasted = s.wait_state(lambda state: "Line 300 of a long document" in state["text"], timeout=15)
        s.keys("ctrl+end")
        time.sleep(1.0)
        japanese_on(s)
        before = s.state()
        line = before["caret_box"]
        # The document element spans all 300 lines; the visible part lies between
        # its label and the bottom of the window.
        window = s.window_rect()
        label = s.rect("Document", "Text")
        visible = [window[0], label[1] + label[3], window[2], min(window[1] + window[3], TASKBAR[1]) - label[1] - label[3]]
        ignore = [TASKBAR, line_band(s, line), s.status_band()]
        s.type("nihongo")
        composing = wait_composing(s)
        s.keys("spc")
        time.sleep(1.0)
        converted = s.screenshot("converted")
        s.keys("spc")
        time.sleep(1.5)
        listed = s.screenshot("candidates")
        candidates = popup_rect(converted, listed, ignore)
        s.keys("up")
        time.sleep(0.5)
        s.keys("ret")
        committed = s.wait_state(lambda state: state["text"] != before["text"])
        expectations = {
            "300 lines were pasted": pasted["text"].count("of a long document") == 300 and pasted["text"] != original,
            "the caret line is visible": bool(line) and visible[1] <= line[1]
                and line[1] + line[3] <= visible[1] + visible[3],
            "typing composes": composing,
            "the candidate list opens beside the composition": beside(candidates, line),
            "the candidate list stays on screen": on_screen(candidates),
            "the commit lands at the end": committed["text"].endswith("日本語"),
        }
        return {"passed": all(expectations.values()), "expectations": expectations, "caret line": line,
                "visible document": visible, "candidates": candidates}


# --------------------------------------------------------------------------- screen reader

def grant_foreground(monitor, size):
    """Let any process take the foreground once, as on a CI runner.

    Windows lets only the foreground process grant that, so a visible helper is
    clicked through QMP (real input brings it forward) and then calls
    AllowSetForegroundWindow(ASFW_ANY) until it is stopped."""
    rect = guest(r'''
$code = @'
Add-Type -Namespace TextloomFocus -Name Native -MemberDefinition '[DllImport("user32.dll")] public static extern bool AllowSetForegroundWindow(int process);'
$host.UI.RawUI.WindowTitle = 'Textloom QA focus helper'
while ($true) { [TextloomFocus.Native]::AllowSetForegroundWindow(-1) | Out-Null; Start-Sleep -Milliseconds 100 }
'@
$process = Start-Process powershell.exe -PassThru -ArgumentList ('-NoProfile -EncodedCommand ' + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code)))
Set-Content C:\textloom-qa\focus-helper.pid $process.Id
$deadline = [DateTime]::UtcNow.AddSeconds(15)
do { Start-Sleep -Milliseconds 300; $window = @(Get-Toplevels | Where-Object { $_.title -eq 'Textloom QA focus helper' })[0] }
until ($window -or [DateTime]::UtcNow -gt $deadline)
Out-Json @{ rect = $window.rect }''')["rect"]
    x, y, width, _ = rect
    monitor.click(x + width // 2, y + 12, size=size)
    time.sleep(1.0)


def stop_foreground_grant():
    guest("Get-Content C:\\textloom-qa\\focus-helper.pid -ErrorAction SilentlyContinue | ForEach-Object { "
          "Stop-Process -Id $_ -Force -ErrorAction SilentlyContinue }; Out-Json @{}")


@check("nvda-reader-probe")
def nvda_reader_probe():
    """The hosted NVDA 2026.2 reader probe (scripts/native-qa/windows_reader.ps1), run in the VM with PowerShell 7."""
    revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO, capture_output=True, text=True,
                              check=True).stdout.strip()
    for path in ("scripts/native-qa/windows_reader.ps1", "scripts/native-qa/nvda/textloomCapture.py",
                 "scripts/check_native_report.py"):
        winvm.share(REPO / path)
    for name in ("windows-reader-evidence.json", "windows-reader-native.json"):
        (winvm.INBOX / name).unlink(missing_ok=True)
    with winvm.qmp() as monitor:
        grant_foreground(monitor, monitor.size())
    try:
        # The probe runs from a copy of the repository layout it expects: it calls
        # `python scripts/check_native_report.py` and loads its NVDA plugin next to itself.
        result = winvm.run(
            "$root = 'C:\\textloom-qa\\repo'\n"
            "Remove-Item -Recurse -Force $root, C:\\textloom-qa\\reader -ErrorAction SilentlyContinue\n"
            "New-Item -ItemType Directory -Force \"$root\\scripts\\native-qa\\nvda\" | Out-Null\n"
            "Get-HostFile windows_reader.ps1 \"$root\\scripts\\native-qa\\windows_reader.ps1\"\n"
            "Get-HostFile textloomCapture.py \"$root\\scripts\\native-qa\\nvda\\textloomCapture.py\"\n"
            "Get-HostFile check_native_report.py \"$root\\scripts\\check_native_report.py\"\n"
            "Set-Location $root\n"
            # The probe refuses to run outside a disposable CI runner; this VM is restored from a snapshot.
            "$env:GITHUB_ACTIONS = 'true'; $env:RUNNER_OS = 'Windows'\n"
            f"$env:TEXTLOOM_QA_REVISION = '{revision}'\n"
            "$env:PATH = \"C:\\msys64\\ucrt64\\bin;$env:PATH\"; $env:GALLIUM_DRIVER = 'llvmpipe'; $env:LIBGL_ALWAYS_SOFTWARE = 'true'\n"
            "$env:GLUTIN_WGL_OPENGL_DLL = 'C:\\textloom-qa\\host\\opengl32.dll'\n"
            "& 'C:\\Program Files\\PowerShell\\7\\pwsh.exe' -NoProfile -File scripts\\native-qa\\windows_reader.ps1 "
            "-NativeExecutable C:\\textloom-qa\\host\\textloom-native-example.exe -OutputDirectory C:\\textloom-qa\\reader 2>&1 | "
            "ForEach-Object { \"$_\" }\n"
            "$code = $LASTEXITCODE\n"
            "foreach ($name in 'windows-reader-evidence.json', 'windows-reader-native.json') {\n"
            "    if (Test-Path \"C:\\textloom-qa\\reader\\$name\") { Send-HostFile \"C:\\textloom-qa\\reader\\$name\" $name } }\n"
            "exit $code", timeout=420)
    finally:
        stop_foreground_grant()
    (OUT / "nvda-reader-probe").mkdir(parents=True, exist_ok=True)
    (OUT / "nvda-reader-probe" / "probe.log").write_text(result["output"])
    evidence_path = winvm.INBOX / "windows-reader-evidence.json"
    evidence = json.loads(evidence_path.read_text(encoding="utf-8-sig")) if evidence_path.exists() else {}
    checks = evidence.get("checks") or {}
    # After NVDA exits, a desktop process can briefly keep NVDA's IAccessible2 proxy
    # loaded, so deleting the probe's temporary NVDA copy fails. That is recorded but
    # does not decide the result, which the evidence written before it does.
    temporary_copy_locked = "IAccessible2proxy.dll' is denied" in result["output"]
    expectations = {
        "the probe finished": not result.get("timed_out") and (result["exit"] == 0 or temporary_copy_locked),
        "the probe's evidence says it succeeded": evidence.get("succeeded") is True,
        "all 15 probe checks passed": len(checks) == 15 and all(value is True for value in checks.values()),
        "owned processes were cleaned up": evidence.get("cleanup_succeeded") is True,
    }
    return {"passed": all(expectations.values()), "expectations": expectations,
            "failed probe checks": [name for name, value in checks.items() if value is not True],
            "temporary NVDA copy left behind": temporary_copy_locked,
            "probe checks": len(checks), "log": str(OUT / "nvda-reader-probe" / "probe.log")}


# --------------------------------------------------------------------------- winit adapter

@check("winit-windows-clipboard-keys")
def winit_windows_clipboard_keys():
    """winit adapter: Ctrl+A then Ctrl+Insert copies, Shift+Delete cuts, Shift+Insert pastes; one undo removes the paste."""
    clipboard = "Out-Json @{ text = (Get-Clipboard -Raw) }"
    with Session("winit-clipboard") as s:
        w = s.winit()
        w.focus()
        fixture = w.state()["doc"]
        start = w.seq()
        s.keys("ctrl+a", "ctrl+insert")
        time.sleep(1.5)
        copy = w.last("clipboard_write", start)
        copied = guest(clipboard)["text"]
        start = w.seq()
        s.keys("shift+delete")
        time.sleep(1.5)
        cut = w.last("clipboard_write", start)
        after_cut = w.state()["doc"]
        start = w.seq()
        s.keys("shift+insert")
        time.sleep(3.0)
        paste = w.last("paste", start)
        after_paste = w.state()["doc"]
        s.keys("ctrl+z")
        time.sleep(1.0)
        undone = w.state()["doc"]
        expectations = {
            "Ctrl+Insert copies the whole document": bool(copy) and copy["action"] == "copy" and copy["text"] == fixture,
            "the Windows clipboard holds it": (copied or "").replace("\r\n", "\n") == fixture,
            "Shift+Delete cuts it": bool(cut) and cut["action"] == "cut" and cut["text"] == fixture and after_cut == "",
            "Shift+Insert requests a paste that restores it once": bool(paste) and after_paste == fixture,
            "one undo removes the paste": undone == "",
        }
        return {"passed": all(expectations.values()), "expectations": expectations,
                "pasted clipboard text": paste and paste.get("clipboard_text")}


@check("winit-altgr-ctrl-alt")
def winit_altgr_ctrl_alt():
    """winit adapter, German layout: AltGr+Q, Ctrl+Alt+Q, and AltGr+E type @@€ (Ctrl+Alt is AltGr on Windows)."""
    with Session("winit-altgr") as s:
        w = s.winit("--caret-end")
        w.focus()
        layout = s.input_method("de-DE")
        before = w.state()["doc"]
        s.keys("alt_r+q", "ctrl+alt+q", "alt_r+e")
        time.sleep(1.0)
        after = w.state()["doc"]
        expectations = {
            "the German layout is active": layout.endswith("0407"),
            "AltGr+Q, Ctrl+Alt+Q, and AltGr+E type @@€": after == before + "@@€",
        }
        return {"passed": all(expectations.values()), "expectations": expectations, "typed": after[len(before):]}


@check("winit-russian-shortcuts")
def winit_russian_shortcuts():
    """winit adapter, Russian layout: Ctrl with physical A, C, V, Z selects all, copies, pastes once, and undoes; plain C types с."""
    with Session("winit-russian") as s:
        w = s.winit("--caret-end")
        w.focus()
        layout = s.input_method("ru-RU")
        fixture = w.state()["doc"]
        start = w.seq()
        s.keys("ctrl+a")
        time.sleep(0.6)
        selected = sorted(w.state()["selection"])
        s.keys("ctrl+c")
        time.sleep(1.5)
        copy = w.last("clipboard_write", start)
        s.keys("ctrl+end")
        time.sleep(0.4)
        s.keys("ctrl+v")
        time.sleep(3.0)
        pasted = w.state()["doc"]
        s.keys("ctrl+z")
        time.sleep(1.0)
        undone = w.state()["doc"]
        s.keys("c")
        time.sleep(0.6)
        typed = w.state()["doc"][len(undone):]
        expectations = {
            "the Russian layout is active": layout.endswith("0419"),
            "Ctrl+physical A (ф) selects all": selected == [0, len(fixture)],
            "Ctrl+physical C (с) copies the document": bool(copy) and copy["text"] == fixture,
            "Ctrl+physical V (м) pastes it once": pasted == fixture + fixture,
            "Ctrl+physical Z (я) undoes the paste in one step": undone == fixture,
            "physical C types с": typed == "с",
        }
        return {"passed": all(expectations.values()), "expectations": expectations, "typed": typed}


@check("winit-ime-first-composition")
def winit_ime_first_composition():
    """winit adapter: the first Japanese composition after launch has the caret's IME area; candidates open beside it."""
    with Session("winit-ime") as s:
        w = s.winit("--caret-end")
        w.focus()
        s.input_method("ja-JP")
        before = w.state()
        caret = [event for event in w.events() if event.get("event") == "sync_ime"][-1]["painted_caret"]
        line = w.cell(round((caret[1] - 20) / 24), round((caret[0] - 20) / 10))
        ignore = [TASKBAR, [w.origin[0], line[1] - 4, 800, line[3] + 8]]
        # The IME starts in direct input; if a probe key types a letter, Alt+` turns kana input on.
        s.keys("n")
        time.sleep(0.8)
        if not w.state()["composition"]:
            s.keys("backspace", "alt+grave_accent")
            time.sleep(0.8)
            s.keys("n")
            time.sleep(0.8)
        s.type("ihongo")
        time.sleep(0.8)
        composing = w.state()["composition"]
        s.keys("spc")
        time.sleep(1.0)
        converted = s.screenshot("converted")
        s.keys("spc")
        time.sleep(1.5)
        listed = s.screenshot("candidates")
        candidates = popup_rect(converted, listed, ignore)
        areas = [event["rect"] for event in w.events() if event.get("event") == "sync_ime"]
        s.keys("up")
        time.sleep(0.5)
        s.keys("ret")
        time.sleep(1.0)
        committed = w.state()["doc"]
        expectations = {
            "typing composes": bool(composing),
            "the IME area follows the caret": areas[-1][1] == caret[1],
            "the candidate list opens beside the caret": beside(candidates, line),
            "the candidate list stays on screen": on_screen(candidates),
            "Enter commits 日本語 at the caret": committed == before["doc"] + "日本語",
        }
        return {"passed": all(expectations.values()), "expectations": expectations, "caret line": line,
                "candidates": candidates}


NVDA_PROFILE = """schemaVersion = 24
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
"""


def audible_seconds(data, rate=44100, channels=2, window=0.02, threshold=500):
    """Seconds of 16-bit PCM whose RMS over 20 ms windows exceeds the noise floor."""
    import array
    samples = array.array("h", data[:len(data) - len(data) % 2])
    step = int(rate * window) * channels
    loud = 0
    for start in range(0, len(samples) - step + 1, step):
        chunk = samples[start:start + step]
        if (sum(value * value for value in chunk) / len(chunk)) ** 0.5 > threshold:
            loud += 1
    return round(loud * window, 2)


@check("nvda-audible-speech")
def nvda_audible_speech():
    """Installed NVDA with its default voice reads the next line after Down; the VM's sound card plays the speech."""
    import wave
    recording = winvm.STATE / "audio.wav"
    if not recording.exists():
        raise RuntimeError("guest audio is not recorded; start the VM with `winvm.py up` (the default --audio wav)")
    with Session("nvda-audible") as s:
        s.start()
        s.focus_document()
        s.select("TextLoom", caret="start")
        guest("New-Item -ItemType Directory -Force C:\\textloom-qa\\nvda-audible | Out-Null\n"
              f"Set-Content -LiteralPath C:\\textloom-qa\\nvda-audible\\nvda.ini -Encoding ascii -Value @'\n{NVDA_PROFILE}'@\n"
              "Start-Process \"$env:ProgramFiles\\NVDA\\nvda.exe\" -ArgumentList '--config-path=C:\\textloom-qa\\nvda-audible', '--lang=en'\n"
              "Start-Sleep -Seconds 8; Out-Json @{ running = [bool](Get-Process nvda -ErrorAction SilentlyContinue) }")
        # A real click refocuses the editor, so NVDA tracks it.
        s.focus_document()
        s.select("TextLoom", caret="start")
        time.sleep(3)
        mark = recording.stat().st_size
        s.keys("down")
        time.sleep(6)
        end = recording.stat().st_size
        caret = s.state()["selection"]
        guest("Start-Process \"$env:ProgramFiles\\NVDA\\nvda.exe\" -ArgumentList '-q'; Start-Sleep -Seconds 3\n"
              "Get-Process nvda -ErrorAction SilentlyContinue | Stop-Process -Force; Out-Json @{}")
    second = 176400  # bytes of 44.1 kHz 16-bit stereo
    with recording.open("rb") as file:
        file.seek(max(44, mark - 3 * second) - (max(44, mark - 3 * second) - 44) % 4)
        quiet = file.read(min(3 * second, mark - 44))
        file.seek(mark - (mark - 44) % 4)
        data = file.read(end - mark)
    clip = OUT / "nvda-audible" / "speech-after-down.wav"
    with wave.open(str(clip), "wb") as output:
        output.setnchannels(2)
        output.setsampwidth(2)
        output.setframerate(44100)
        output.writeframes(data[:len(data) - len(data) % 4])
    seconds = audible_seconds(data)
    baseline = audible_seconds(quiet)
    expectations = {
        "Down moved the caret to the next line": bool(caret) and caret[0] > 0,
        "the sound card was quiet before the key": baseline < 0.3,
        "the sound card played at least a second of speech after it": seconds >= 1.0,
    }
    return {"passed": all(expectations.values()), "expectations": expectations, "audible seconds": seconds,
            "audible seconds before the key": baseline,
            "recorded seconds": round(len(data) / 176400, 2), "clip": str(clip),
            "note": "Listen to the clip to confirm it says the line; the check only measures sound."}


def run_checks(names, runs):
    for run in range(1, runs + 1):
        for name in names:
            label = name if runs == 1 else f"{name}#run{run}"
            print(f"== {label}", flush=True)
            started = time.monotonic()
            try:
                result = CHECKS[name]()
            except Exception as error:  # noqa: BLE001
                result = {"passed": False, "error": str(error), "trace": traceback.format_exc()[-3000:]}
            result["seconds"] = round(time.monotonic() - started, 1)
            record(label, result)
            print(json.dumps(result, ensure_ascii=False)[:3000], flush=True)


def main():
    arguments = sys.argv[1:]
    runs = 1
    if "--runs" in arguments:
        position = arguments.index("--runs")
        runs = int(arguments[position + 1])
        del arguments[position:position + 2]
    if arguments in (["build"], ["deploy"], ["explore"]):
        {"build": build, "deploy": deploy, "explore": explore}[arguments[0]]()
    elif arguments == ["list"]:
        for name, function in CHECKS.items():
            print(f"{name}: {(function.__doc__ or '').strip()}")
    elif arguments:
        names = list(CHECKS) if arguments == ["all"] else arguments
        unknown = [name for name in names if name not in CHECKS]
        if unknown:
            winvm.fail("unknown checks: " + ", ".join(unknown))
        run_checks(names, runs)
    else:
        print(__doc__)


if __name__ == "__main__":
    main()
