# SPDX-License-Identifier: GPL-2.0-or-later
# Copyright (c) 2026 Textloom contributors
"""Original, isolated NVDA QA plugin; never generates speech or edits text.

The driver sends native keys. This plugin observes NVDA's synthesis queue and
reads its focused object's TextInfo. Only the owned editor's public fixture is
captured. Requests and results stay in the private profile; there is no server.
"""

import json
import os
from pathlib import Path
import threading
import time

import api
import config
import core
import globalPluginHandler
from NVDAState import WritePaths
from speech.extensions import pre_speechQueued
import textInfos
import wx


MAX_TEXT = 8192
MAX_EVENTS = 5000


class GlobalPlugin(globalPluginHandler.GlobalPlugin):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._root = Path(WritePaths.configDir)
        self._target = int(os.environ["TEXTLOOM_NVDA_TARGET_PID"])
        self._output = self._root / "reader-events.jsonl"
        self._request = self._root / "reader-request.json"
        self._last_request = 0
        self._sequence = 0
        self._lock = threading.Lock()
        self._stopped = False
        self._timer = wx.PyTimer(self._poll)
        pre_speechQueued.register(self._speech)
        core.postNvdaStartup.register(self._ready)
        self._timer.Start(100)

    def _write(self, kind, **values):
        with self._lock:
            if self._stopped:
                return
            self._sequence += 1
            if self._sequence > MAX_EVENTS:
                self._timer.Stop()
                self._stopped = True
                return
            event = {"sequence": self._sequence, "kind": kind,
                     "monotonic_seconds": time.monotonic(), **values}
            with self._output.open("a", encoding="utf-8") as stream:
                stream.write(json.dumps(event, ensure_ascii=True) + "\n")

    def _ready(self):
        self._write("ready", reader_pid=os.getpid(), target_pid=self._target,
                    synth=config.conf["speech"]["synth"])

    def _focus(self):
        obj = api.getFocusObject()
        if obj is None or obj.processID != self._target:
            raise RuntimeError("owned editor is not NVDA's focus object")
        return obj

    @staticmethod
    def _bounded(value):
        if not isinstance(value, str) or len(value) > MAX_TEXT:
            raise ValueError("capture text exceeds fixture limit")
        return value

    def _speech(self, speechSequence, **kwargs):
        # Filtering by NVDA's own focus avoids capturing other applications.
        try:
            obj = self._focus()
        except RuntimeError:
            return
        parts = [self._bounded(part) for part in speechSequence if isinstance(part, str)]
        if parts:
            self._write("speech_queued", target_pid=obj.processID,
                        text=self._bounded(" ".join(parts)))

    def event_gainFocus(self, obj, nextHandler):
        if obj.processID == self._target:
            self._write("focus", target_pid=obj.processID,
                        name=self._bounded(obj.name or ""), role=str(obj.role))
        nextHandler()

    def _snapshot(self, request_id):
        obj = self._focus()
        entire = obj.makeTextInfo(textInfos.POSITION_ALL)
        selection = obj.makeTextInfo(textInfos.POSITION_SELECTION)
        caret = obj.makeTextInfo(textInfos.POSITION_CARET)
        self._write("snapshot", request_id=request_id, target_pid=obj.processID,
                    name=self._bounded(obj.name or ""), role=str(obj.role),
                    provider=type(entire).__module__ + "." + type(entire).__name__,
                    text=self._bounded(entire.text),
                    selected_text=self._bounded(selection.text),
                    selection_collapsed=selection.isCollapsed,
                    caret_at_start=caret.compareEndPoints(entire, "startToStart") == 0,
                    caret_at_end=caret.compareEndPoints(entire, "startToEnd") == 0)

    def _poll(self):
        if not self._request.exists():
            return
        request_id = None
        try:
            if self._request.stat().st_size > 1024:
                raise ValueError("request exceeds limit")
            request = json.loads(self._request.read_text(encoding="utf-8"))
            request_id = request["id"]
            if type(request_id) is not int or request_id <= self._last_request:
                return
            self._last_request = request_id
            operation = request["operation"]
            if operation == "snapshot":
                self._snapshot(request_id)
            elif operation == "barrier":
                self._write("barrier", request_id=request_id)
            elif operation == "quit":
                self._write("quit_requested", request_id=request_id)
                core.callLater(0, core.triggerNVDAExit)
            else:
                raise ValueError("unknown request operation")
        except Exception as error:
            # Exception messages can contain provider/document text.
            self._write("capture_error", request_id=request_id,
                        error_type=type(error).__name__)

    def terminate(self):
        self._timer.Stop()
        pre_speechQueued.unregister(self._speech)
        core.postNvdaStartup.unregister(self._ready)
        self._write("terminated")
        self._stopped = True
        super().terminate()
