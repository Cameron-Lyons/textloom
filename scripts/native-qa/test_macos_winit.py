"""Headless cleanup regressions for the hosted Mac winit probe."""

import itertools
import signal
import unittest
from unittest.mock import Mock, patch

import macos_winit


class CleanupTests(unittest.TestCase):
    def probe(self):
        # Avoid constructing executable/report paths or starting a real child.
        probe = macos_winit.Probe.__new__(macos_winit.Probe)
        probe.child = Mock(pid=101)
        probe.observations = {}
        return probe

    def test_zombie_group_permission_error_is_reaped_before_success(self):
        probe = self.probe()
        probe.child.poll.side_effect = [None, 0]
        with (patch.object(macos_winit.os, "killpg", side_effect=[
                None, PermissionError(1, "Operation not permitted"), ProcessLookupError()
              ]) as killpg,
              patch.object(macos_winit.time, "monotonic", side_effect=itertools.count(0, 0.1)),
              patch.object(macos_winit.time, "sleep")):
            probe.cleanup()
        self.assertEqual(killpg.call_args_list, [
            unittest.mock.call(101, signal.SIGTERM),
            unittest.mock.call(101, 0),
            unittest.mock.call(101, 0),
        ])
        self.assertEqual(probe.child.poll.call_count, 2)
        probe.child.wait.assert_called_once_with(timeout=1)
        self.assertIn("cleanup", probe.observations)

    def test_termination_permission_race_still_requires_group_absence(self):
        probe = self.probe()
        with (patch.object(macos_winit.os, "killpg", side_effect=[
                PermissionError(1, "Operation not permitted"), ProcessLookupError()
              ]),
              patch.object(macos_winit.time, "monotonic", side_effect=itertools.count(0, 0.1)),
              patch.object(macos_winit.time, "sleep")):
            probe.cleanup()
        probe.child.wait.assert_called_once_with(timeout=1)
        self.assertIn("cleanup", probe.observations)

    def test_live_unsignalable_group_fails_after_bounded_cleanup(self):
        probe = self.probe()
        with (patch.object(macos_winit.os, "killpg",
                           side_effect=PermissionError(1, "Operation not permitted")) as killpg,
              patch.object(macos_winit.time, "monotonic", side_effect=itertools.count()),
              patch.object(macos_winit.time, "sleep")):
            with self.assertRaisesRegex(macos_winit.ProbeFailure, "survived bounded cleanup"):
                probe.cleanup()
        self.assertEqual([call.args[1] for call in killpg.call_args_list if call.args[1]],
                         [signal.SIGTERM, signal.SIGKILL])
        probe.child.wait.assert_not_called()
        self.assertNotIn("cleanup", probe.observations)

    def test_reaped_leader_does_not_hide_surviving_group_members(self):
        probe = self.probe()
        probe.child.poll.return_value = 0
        with (patch.object(macos_winit.os, "killpg"),
              patch.object(macos_winit.time, "monotonic", side_effect=itertools.count()),
              patch.object(macos_winit.time, "sleep")):
            with self.assertRaisesRegex(macos_winit.ProbeFailure, "survived bounded cleanup"):
                probe.cleanup()
        probe.child.wait.assert_not_called()
        self.assertNotIn("cleanup", probe.observations)


if __name__ == "__main__":
    unittest.main()
