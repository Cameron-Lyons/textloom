import importlib.util
from pathlib import Path
import signal
import subprocess
import unittest
from unittest.mock import Mock, patch


SPEC = importlib.util.spec_from_file_location(
    "macos_interaction", Path(__file__).parent / "native-qa" / "macos_interaction.py"
)
macos_interaction = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(macos_interaction)


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
