"""Regression tests for the Windows VM check harness that need no VM."""

import array
import importlib.util
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path

HARNESS = Path(__file__).resolve().parent / "windows-vm"


def load(name):
    spec = importlib.util.spec_from_file_location(name, HARNESS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


STATE = tempfile.mkdtemp(prefix="textloom-winvm-test-")


def tearDownModule():
    shutil.rmtree(STATE, ignore_errors=True)

os.environ["TEXTLOOM_WINVM"] = STATE
winvm = load("winvm")
checks = load("checks")


class OffsetTests(unittest.TestCase):
    TEXT = "Unicode: café, 👨‍👩‍👧‍👦.\nLists"

    def test_utf16_offsets_count_surrogate_pairs(self):
        lists = self.TEXT.index("Lists")
        self.assertEqual(checks.utf16(self.TEXT, lists), lists + 4)
        self.assertEqual(checks.codepoints(self.TEXT, checks.utf16(self.TEXT, lists)), lists)

    def test_round_trip_at_every_index(self):
        for index in range(len(self.TEXT) + 1):
            self.assertEqual(checks.codepoints(self.TEXT, checks.utf16(self.TEXT, index)), index)


class PlacementTests(unittest.TestCase):
    LINE = [100, 300, 5, 15]

    def test_panel_below_or_above_the_line(self):
        self.assertTrue(checks.beside([110, 320, 140, 100], self.LINE))
        self.assertTrue(checks.beside([110, 190, 140, 100], self.LINE))

    def test_panel_covering_or_far_from_the_line(self):
        self.assertFalse(checks.beside([110, 305, 140, 100], self.LINE))
        self.assertFalse(checks.beside([400, 320, 140, 100], self.LINE))
        self.assertFalse(checks.beside([110, 500, 140, 100], self.LINE))
        self.assertFalse(checks.beside(None, self.LINE))

    def test_on_screen_excludes_the_taskbar(self):
        self.assertTrue(checks.on_screen([0, 0, 1280, 752]))
        self.assertFalse(checks.on_screen([0, 700, 100, 100]))


class AudioTests(unittest.TestCase):
    def test_silence_and_a_one_second_tone(self):
        silence = array.array("h", [0] * 44100 * 2).tobytes()
        tone = array.array("h", [3000, 3000, -3000, -3000] * 22050).tobytes()
        self.assertEqual(checks.audible_seconds(silence), 0)
        self.assertAlmostEqual(checks.audible_seconds(silence + tone), 1.0, delta=0.05)


class JobServerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        winvm.CONFIG.write_text(json.dumps({"port": port, "token": "secret"}))
        threading.Thread(target=winvm.serve, daemon=True).start()
        cls.base = f"http://127.0.0.1:{port}"
        for _ in range(50):
            try:
                winvm.request("GET", "/status", timeout=1)
                return
            except OSError:
                threading.Event().wait(0.1)

    def guest_request(self, path, data=None, method="GET", token="secret"):
        request = urllib.request.Request(self.base + path, data=data, method=method, headers={"X-Token": token})
        with urllib.request.urlopen(request, timeout=40) as response:
            return response.status, response.read()

    def test_a_job_round_trips_through_a_polling_guest(self):
        def guest():
            status, body = self.guest_request("/next")
            job = json.loads(body)
            result = {"exit": 0, "timed_out": False, "output": "ran " + job["script"]}
            self.guest_request("/result/" + job["id"], json.dumps(result).encode("utf-8-sig"), "POST")

        thread = threading.Thread(target=guest)
        thread.start()
        self.assertEqual(winvm.run("Get-Date", timeout=30)["output"], "ran Get-Date")
        thread.join()

    def test_requests_need_the_token(self):
        with self.assertRaises(urllib.error.HTTPError) as raised:
            self.guest_request("/status", token="wrong")
        raised.exception.close()
        self.assertEqual(raised.exception.code, 403)

    def test_shared_names_cannot_leave_the_share(self):
        for name in ("..%2Fconfig.json", ".hidden"):
            with self.assertRaises(urllib.error.HTTPError) as raised:
                self.guest_request("/share/" + name)
            raised.exception.close()
            self.assertEqual(raised.exception.code, 400)

    def test_files_move_through_the_share_and_inbox(self):
        source = Path(STATE) / "source.txt"
        source.write_text("shared")
        self.assertEqual(self.guest_request("/share/" + winvm.share(source))[1], b"shared")
        self.guest_request("/inbox/upload.png", b"PNG", "PUT")
        self.assertEqual((winvm.INBOX / "upload.png").read_bytes(), b"PNG")


@unittest.skipUnless(shutil.which("magick"), "ImageMagick is not installed")
class PopupTests(unittest.TestCase):
    def test_finds_a_new_panel_and_ignores_masked_rows(self):
        directory = Path(tempfile.mkdtemp(dir=STATE))
        before, after = directory / "before.png", directory / "after.png"
        subprocess.run(["magick", "-size", "400x300", "xc:white", str(before)], check=True)
        subprocess.run(["magick", str(before), "-fill", "black", "-draw", "rectangle 50,40 149,119",
                        "-draw", "rectangle 0,200 399,215", str(after)], check=True)
        self.assertEqual(checks.popup_rect(before, after, ignore=[(0, 196, 400, 24)]), [50, 40, 100, 80])
        self.assertIsNone(checks.popup_rect(before, before))


if __name__ == "__main__":
    unittest.main()
