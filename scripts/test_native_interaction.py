import importlib.util
from pathlib import Path
import re
import signal
import subprocess
import sys
import unittest
from unittest.mock import Mock, patch


SPEC = importlib.util.spec_from_file_location(
    "macos_interaction", Path(__file__).parent / "native-qa" / "macos_interaction.py"
)
macos_interaction = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(macos_interaction)


class ShortcutFixture:
    """Small fixture oracle, not a replacement for the hosted native run.

    Use explicit grapheme tokens and column-zero vertical moves so the authored
    sequences can be checked without macOS, font metrics, or editor bindings.
    """

    def __init__(self, test, ignored=None):
        self.test = test
        self.ignored = ignored
        self.actions = []
        self.copy_actions = 0
        self.fixture_actions = 0
        self.history = []
        self.undo_count = 0

    def tokens(self, text):
        return re.findall("e\u0301|👩🏽‍💻|.", text, re.DOTALL)

    def load(self, _pid, text):
        self.fixture_actions += 4  # Native Select All, Paste, Select All, Copy.
        self.text = self.tokens(text)
        self.anchor = self.focus = len(self.text)
        self.history = []

    def boundaries(self, soft=False):
        breaks = {"\n", "\u2028"} if soft else {"\n"}
        ends = [index for index, token in enumerate(self.text) if token in breaks]
        starts = [0] + [index + 1 for index in ends]
        row = max(index for index, start in enumerate(starts) if start <= self.focus)
        return starts, ends + [len(self.text)], row

    def replace(self, start, end, text):
        self.history.append((self.text[:], self.anchor, self.focus))
        self.text[start:end] = self.tokens(text)
        self.anchor = self.focus = start + len(self.tokens(text))

    def action(self, _pid, window, command, value):
        self.test.assertEqual(window, macos_interaction.TITLE)
        self.actions.append((command, value))
        if self.ignored and self.ignored(command, value):
            return
        target = None
        extend = "shift" in command
        if command in ("command-keycode", "command-shift-keycode"):
            if value == "51":
                starts, _, row = self.boundaries(soft=True)
                self.replace(starts[row], self.focus, "")
            elif value == "124":
                _, ends, row = self.boundaries(soft=True)
                target = ends[row]
            else:
                self.test.assertIn(value, ("125", "126"))
                target = 0 if value == "126" else len(self.text)
        elif command in ("control-chord", "control-shift-chord"):
            starts, ends, paragraph = self.boundaries()
            if value in "ae":
                target = starts[paragraph] if value == "a" else ends[paragraph]
            elif value in "bf":
                target = self.focus + (-1 if value == "b" else 1)
            elif value in "pn":
                rows, _, row = self.boundaries(soft=True)
                self.test.assertEqual(self.focus, rows[row], "vertical fixture must start at column zero")
                target = rows[row + (-1 if value == "p" else 1)]
            elif value == "h":
                self.replace(self.focus - 1, self.focus, "")
            elif value == "d":
                self.replace(self.focus, self.focus + 1, "")
            else:
                self.test.fail(f"Unexpected Control alias {value}")
        elif command in ("option-keycode", "option-shift-keycode"):
            starts, ends, paragraph = self.boundaries()
            if value == "126":
                target = starts[max(0, paragraph - (self.focus == starts[paragraph]))]
            else:
                self.test.assertEqual(value, "125")
                target = ends[min(len(ends) - 1, paragraph + (self.focus == ends[paragraph]))]
        elif command in ("keycode", "shift-keycode"):
            if value == "36":
                self.replace(self.focus, self.focus, "\u2028")
            elif value in ("123", "124"):
                target = self.focus + (-1 if value == "123" else 1)
            else:
                self.test.assertEqual(value, "125")
                rows, _, row = self.boundaries(soft=True)
                self.test.assertEqual(self.focus, rows[row])
                target = rows[row + 1]
        elif command == "type":
            self.replace(self.focus, self.focus, value)
        elif command == "chord" and value == "z":
            self.text, self.anchor, self.focus = self.history.pop()
            self.undo_count += 1
        else:
            self.test.fail(f"Unexpected fixture action {command} {value}")
        if target is not None:
            self.test.assertTrue(0 <= target <= len(self.text))
            self.focus = target
            if not extend:
                self.anchor = target

    def copy(self, _pid, window, expected, *, select_all=True):
        self.test.assertEqual(window, macos_interaction.TITLE)
        self.copy_actions += 2 if select_all else 1
        if select_all:
            self.anchor, self.focus = 0, len(self.text)
        start, end = sorted((self.anchor, self.focus))
        actual = "".join(self.text[start:end]).replace("\u2028", "\n")
        self.test.assertEqual(actual, expected)
        return {"plain_exact": True}


class MacosShortcutTests(unittest.TestCase):
    def probe(self):
        probe = macos_interaction.Probe(Path("unused"), "candidate")
        for method in ("save", "run", "action", "copy", "note"):
            setattr(probe, method, Mock())
        return probe

    def test_fixture_uses_public_utf8_clipboard_input_and_native_paste(self):
        probe = self.probe()
        trace = Mock()
        for name in ("run", "action", "copy"):
            trace.attach_mock(getattr(probe, name), name)
        text = macos_interaction.SHORTCUT_UNICODE
        probe.shortcut_fixture(101, text)
        self.assertEqual(trace.mock_calls, [
            unittest.mock.call.run(["/usr/bin/env", "LC_ALL=en_US.UTF-8", "/usr/bin/pbcopy"], input_text=text),
            unittest.mock.call.action(101, macos_interaction.TITLE, "chord", "a"),
            unittest.mock.call.action(101, macos_interaction.TITLE, "chord", "v"),
            unittest.mock.call.copy(101, macos_interaction.TITLE, text),
        ])

    def test_selection_check_preserves_selection_and_requires_exact_copy_before_success(self):
        probe = self.probe()
        probe.copy.return_value = {"plain_exact": True}
        probe.shortcut_check(101, "selection", "e\u0301", ("control-shift-chord", "b"))
        probe.action.assert_called_once_with(101, macos_interaction.TITLE, "control-shift-chord", "b")
        probe.copy.assert_called_once_with(101, macos_interaction.TITLE, "e\u0301", select_all=False)
        probe.note.assert_called_once_with("editable: shortcut selection", plain_exact=True)

    def test_copy_requires_fresh_generation_and_codepoint_exact_unicode_with_trailing_lf(self):
        expected = "e\u0301👩🏽‍💻\n"
        for actual in (
            expected, "é👩🏽‍💻\n", expected.rstrip("\n"),
            expected.replace("\n", "\r\n"), expected.replace("\n", "\r"),
        ):
            with self.subTest(actual=actual):
                probe = self.probe()
                # Use the real copy runner, with only the OS boundaries mocked.
                probe.copy = macos_interaction.Probe.copy.__get__(probe)
                probe.clipboard = Mock(side_effect=[{"change_count": 7}, {"plain_exact": True}])
                probe.run.return_value.stdout = actual.encode("utf-8")
                if actual == expected:
                    result = probe.copy(101, macos_interaction.TITLE, expected, select_all=False)
                    self.assertTrue(result["plain_utf8_exact"])
                else:
                    with self.assertRaisesRegex(RuntimeError, "exact Unicode fixture"):
                        probe.copy(101, macos_interaction.TITLE, expected, select_all=False)
                probe.clipboard.assert_any_call("plain", expected, changed_from=7)
                probe.run.assert_called_once_with(
                    ["/usr/bin/env", "LC_ALL=en_US.UTF-8", "/usr/bin/pbpaste", "-Prefer", "txt"],
                    binary_output=True,
                )
                probe.action.assert_called_once_with(101, macos_interaction.TITLE, "chord", "c")

    def test_binary_runner_preserves_unicode_crlf_cr_and_lf_without_a_desktop(self):
        payload = "e\u0301👩🏽‍💻\r\nx\ry\n".encode("utf-8")
        probe = macos_interaction.Probe(Path("unused"), "candidate")
        result = probe.run(
            [sys.executable, "-c", f"import sys; sys.stdout.buffer.write({payload!r})"],
            binary_output=True,
        )
        self.assertEqual(result.stdout, payload)

    def test_deletion_checks_full_document_then_single_undo_exact_restoration(self):
        probe = self.probe()
        probe.copy.side_effect = [{"plain_exact": True}, {"plain_exact": True}]
        trace = Mock()
        for name in ("action", "copy", "note"):
            trace.attach_mock(getattr(probe, name), name)
        probe.shortcut_check(101, "deletion", "after", ("control-chord", "h"), undo_to="before")
        self.assertEqual(trace.mock_calls, [
            unittest.mock.call.action(101, macos_interaction.TITLE, "control-chord", "h"),
            unittest.mock.call.copy(101, macos_interaction.TITLE, "after", select_all=True),
            unittest.mock.call.action(101, macos_interaction.TITLE, "chord", "z"),
            unittest.mock.call.copy(101, macos_interaction.TITLE, "before"),
            unittest.mock.call.note("editable: shortcut deletion", plain_exact=True, undo_plain_exact=True),
        ])

    def test_failed_edit_or_undo_assertion_never_records_success(self):
        for fail_undo in (False, True):
            with self.subTest(fail_undo=fail_undo):
                probe = self.probe()
                failure = RuntimeError("clipboard differs")
                probe.copy.side_effect = [{"plain_exact": True}, failure] if fail_undo else failure
                with self.assertRaisesRegex(RuntimeError, "Shortcut deletion: clipboard differs"):
                    probe.shortcut_check(101, "deletion", "after", ("control-chord", "d"), undo_to="before")
                self.assertEqual(probe.copy.call_count, 2 if fail_undo else 1)
                self.assertEqual(probe.action.call_count, 2 if fail_undo else 1)
                probe.note.assert_not_called()

    def replay(self, ignored=None):
        probe = self.probe()
        fixture = ShortcutFixture(self, ignored)
        probe.shortcut_fixture = fixture.load
        probe.action = fixture.action
        probe.copy = fixture.copy
        probe.shortcuts(101)
        return probe, fixture

    def test_authored_sequences_select_exact_fixtures_and_undo_all_deletions(self):
        probe, fixture = self.replay()
        self.assertEqual("".join(fixture.text), macos_interaction.SHORTCUT_UNICODE)
        self.assertEqual(fixture.undo_count, 3)
        self.assertEqual(probe.note.call_count, 25)
        # Keep the extension bounded; no per-case app launches or new sleeps.
        self.assertLessEqual(len(fixture.actions) + fixture.copy_actions + fixture.fixture_actions, 160)
        for command, value in fixture.actions:
            if command.startswith("control"):
                self.assertIn(value, "aebfpnhd")  # Never OS-reserved Control-arrow.

    def test_exact_fixture_oracle_rejects_each_missing_native_shortcut(self):
        missing = [
            (f"Control-{letter}", lambda command, value, letter=letter:
             command.startswith("control") and value == letter)
            for letter in "aebfpnhd"
        ] + [
            (f"Option-{value}", lambda command, key, value=value:
             command.startswith("option") and key == value)
            for value in ("125", "126")
        ] + [
            ("Shift-Control", lambda command, _value: command == "control-shift-chord"),
            ("Shift-Option", lambda command, _value: command == "option-shift-keycode"),
            ("Command-Backspace", lambda command, value: command == "command-keycode" and value == "51"),
        ]
        for name, ignored in missing:
            with self.subTest(missing=name):
                with self.assertRaises((RuntimeError, AssertionError)):
                    self.replay(ignored)

    def test_fixture_oracle_rejects_swapped_control_p_and_n(self):
        original_action = ShortcutFixture.action

        def swapped_action(fixture, pid, window, command, value):
            if command.startswith("control"):
                value = {"p": "n", "n": "p"}.get(value, value)
            return original_action(fixture, pid, window, command, value)

        with patch.object(ShortcutFixture, "action", swapped_action):
            with self.assertRaises(RuntimeError):
                self.replay()

    def test_added_system_events_actions_deliver_only_the_requested_modifiers(self):
        for command, modifiers in (
            ("control-chord", "control down"),
            ("control-shift-chord", "control down, shift down"),
            ("command-keycode", "command down"),
            ("command-shift-keycode", "command down, shift down"),
            ("shift-keycode", "shift down"),
            ("option-keycode", "option down"),
            ("option-shift-keycode", "option down, shift down"),
        ):
            with self.subTest(command=command):
                body = macos_interaction.APPLESCRIPT.split(f'commandName is "{command}" then\n', 1)[1]
                body = body.split("else", 1)[0]
                operation = "keystroke" if command.endswith("chord") else "key code"
                self.assertIn(operation, body)
                self.assertIn("using {" + modifiers + "}", body)

    def test_editable_invokes_shortcuts_before_closing_its_owned_window(self):
        probe = self.probe()
        probe.start = Mock(return_value=(Mock(pid=101), Path("unused-report")))
        for name in ("clipboard", "diagnostic", "open_textedit", "external_copy", "focus", "shortcuts", "finish"):
            setattr(probe, name, Mock(return_value={}))
        probe.clipboard.return_value = {"change_count": 1}
        probe.copy.return_value = {"plain_exact": True}
        trace = Mock()
        trace.attach_mock(probe.shortcuts, "shortcuts")
        trace.attach_mock(probe.finish, "finish")
        probe.editable()
        self.assertEqual(trace.mock_calls, [
            unittest.mock.call.shortcuts(101),
            unittest.mock.call.finish("editable", probe.start.return_value[0], Path("unused-report")),
        ])


@unittest.skipUnless(hasattr(signal, "SIGKILL"), "POSIX process cleanup")
class MacosCleanupTests(unittest.TestCase):
    def probe(self):
        probe = macos_interaction.Probe(Path("unused"), "candidate")
        probe.save = Mock()
        probe.handles = [Mock(), Mock()]
        return probe

    def test_native_exit_race_still_reaps_all_children_and_closes_handles(self):
        for during_kill in (False, True):
            with self.subTest(during_kill=during_kill):
                probe = self.probe()
                first = Mock(pid=101)
                second = Mock(pid=102)
                for child in (first, second):
                    child.poll.side_effect = [None, 0]
                first.wait.side_effect = (
                    [subprocess.TimeoutExpired("native", 3), 0] if during_kill else [0]
                )
                probe.hosts = [first, second]
                signals = (
                    [None, ProcessLookupError(), None]
                    if during_kill else [ProcessLookupError(), None]
                )
                with patch.object(macos_interaction.os, "killpg", side_effect=signals):
                    with self.assertRaisesRegex(RuntimeError, "did not all close"):
                        probe.cleanup()
                self.assertEqual(first.wait.call_count, 2 if during_kill else 1)
                second.wait.assert_called_once_with(timeout=3)
                for handle in probe.handles:
                    handle.close.assert_called_once_with()
                self.assertTrue(probe.observations["cleanup"]["native_processes_reaped"])
                self.assertFalse(probe.observations["cleanup"]["native_sessions_closed_gracefully"])
                self.assertFalse(probe.observations["passed"])

    def test_textedit_exit_race_still_closes_handles(self):
        for during_kill in (False, True):
            with self.subTest(during_kill=during_kill):
                probe = self.probe()
                probe.textedit_pid = 103
                probe.run = Mock(return_value=Mock(returncode=0, stdout="/Applications/TextEdit"))
                probe.textedit = Mock(side_effect=[103, None, None] if during_kill else [None, None, None])
                signals = [None, ProcessLookupError()] if during_kill else [ProcessLookupError()]
                with (
                    patch.object(macos_interaction.os, "kill", side_effect=signals),
                    patch.object(macos_interaction.time, "monotonic", side_effect=[0, 4, 5, 9]),
                ):
                    probe.cleanup()
                for handle in probe.handles:
                    handle.close.assert_called_once_with()
                self.assertTrue(probe.observations["cleanup"]["textedit_process_absent"])
                probe.save.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
