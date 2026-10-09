#!/usr/bin/env python3
"""Validate native rendering and clipboard observations without certifying manual QA."""

import argparse
import json
import math
from pathlib import Path
import sys


# Exact keys and scalar types prevent a report from silently including document
# text, clipboard payloads, font paths, or composition strings in CI artifacts.
SCHEMA = {
    "schema_version": int,
    "manual_signoff": str,
    "document_content_included": bool,
    "host": {
        "name": str,
        "egui_api_version": str,
        "eframe_api_version": str,
        "os": str,
        "architecture": str,
        "source_revision": str,
    },
    "session": {
        "started_unix_seconds": int,
        "ended_unix_seconds": int,
        "graceful_exit": bool,
        "smoke_test_requested": bool,
        "rich_clipboard_requested": bool,
        "clipboard_self_test_requested": bool,
        "clipboard_self_test_passed": bool,
        "rendered_frames": int,
        "regular_font_supplied": bool,
        "bold_font_supplied": bool,
    },
    "display": {
        "pixels_per_point": (int, float),
        "minimum_pixels_per_point": (int, float),
        "maximum_pixels_per_point": (int, float),
        "native_pixels_per_point": (int, float, type(None)),
    },
    "host_input_events": dict.fromkeys(
        ("text", "key_press", "paste", "copy", "cut", "ime_preedit",
         "ime_commit", "ime_delete_surrounding", "accessibility_action",
         "window_focus_gained", "window_focus_lost"), int
    ),
    "observed_state_changes": dict.fromkeys(
        ("document_revision", "selection", "typing_style", "composition",
         "focus_gains", "focus_losses", "enabled", "read_only", "snapshot_restores"), int
    ),
    "errors": dict.fromkeys(
        ("editing", "accessibility", "host_command", "clipboard_self_test"), int
    ),
    "final_widget": dict.fromkeys(
        ("enabled", "read_only", "focused", "window_focused",
         "widget_focus_retained", "composition_active"), bool
    ),
    "final_document": {
        **dict.fromkeys(
            ("paragraphs", "text_utf8_bytes_excluding_paragraph_breaks", "revision",
             "tlfr_bytes", "html_bytes", "undo_steps", "redo_steps"), int
        ),
        "tlfr_round_trip_equal": bool,
    },
    "final_selection": {
        "anchor": {"paragraph": int, "byte": int},
        "focus": {"paragraph": int, "byte": int},
    },
    "final_typing_style": dict.fromkeys(
        ("bold", "italic", "underline", "strikethrough", "code", "explicit_foreground"), bool
    ),
}


def check_schema(value, schema, path="report"):
    if isinstance(schema, dict):
        if type(value) is not dict or value.keys() != schema.keys():
            raise ValueError(f"{path}: unexpected or missing fields")
        for key, spec in schema.items():
            check_schema(value[key], spec, f"{path}.{key}")
        return
    expected = schema if isinstance(schema, tuple) else (schema,)
    if type(value) not in expected:
        raise ValueError(f"{path}: incorrect scalar type")
    if (type(value) is int and value < 0) or (
        type(value) is float and (value < 0 or not math.isfinite(value))
    ):
        raise ValueError(f"{path}: expected a finite nonnegative number")


def validate(report, revision, os_name, mode):
    """Reject stale, malformed, failed, or content-bearing renderer evidence."""
    check_schema(report, SCHEMA)

    def require(condition, message):
        if not condition:
            raise ValueError(message)

    require(report["schema_version"] == 1, "unsupported report schema version")
    require(report["manual_signoff"] == "not_recorded", "smoke report claims manual signoff")
    require(not report["document_content_included"], "report claims document content")
    host = report["host"]
    require(host["name"] == "textloom-native-example", "unexpected native host")
    require(
        host["egui_api_version"] == host["eframe_api_version"] == "0.36",
        "unexpected host API versions",
    )
    require(host["architecture"] in ("x86_64", "aarch64"), "unexpected runner architecture")
    require(host["source_revision"] == revision, "source revision does not match this run")
    require(host["os"] == os_name, "host OS does not match this run")
    session = report["session"]
    require(
        session["graceful_exit"] and session["smoke_test_requested"],
        "missing graceful smoke completion",
    )
    require(session["rendered_frames"] >= 20, "fewer than 20 native frames")
    require(session["rich_clipboard_requested"], "native rich clipboard was not requested")
    require(
        session["clipboard_self_test_requested"] == (mode == "editable"),
        "unexpected clipboard self-test request for this mode",
    )
    require(
        session["clipboard_self_test_passed"] == (mode == "editable"),
        "missing or unexpected clipboard self-test completion",
    )
    require(all(count == 0 for count in report["errors"].values()), "native host reported errors")
    require(report["final_document"]["tlfr_round_trip_equal"], "TLFR snapshot roundtrip failed")
    widget = report["final_widget"]
    require(not widget["composition_active"], "composition remains active after smoke test")
    require(widget["enabled"] == (mode != "disabled"), "unexpected widget enabled state")
    require(widget["read_only"] == (mode == "read-only"), "unexpected widget read-only state")
    require(
        widget["focused"] == (widget["window_focused"] and widget["widget_focus_retained"]),
        "inconsistent focus observations",
    )
    if mode == "disabled":
        require(
            not widget["widget_focus_retained"] and not widget["focused"],
            "disabled widget retained focus",
        )
        require(
            report["observed_state_changes"]["focus_gains"] == 0,
            "disabled widget gained focus",
        )
    display = report["display"]
    require(
        0 < display["minimum_pixels_per_point"]
        <= display["pixels_per_point"] <= display["maximum_pixels_per_point"],
        "invalid rendered display scale",
    )


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON field")
        result[key] = value
    return result


def reject_constant(_value):
    raise ValueError("nonfinite JSON number")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--os", required=True, choices=("linux", "macos", "windows"))
    parser.add_argument("--mode", required=True, choices=("editable", "read-only", "disabled"))
    arguments = parser.parse_args()
    try:
        report = json.loads(
            arguments.report.read_text(encoding="utf-8"),
            object_pairs_hook=unique_object,
            parse_constant=reject_constant,
        )
        validate(report, arguments.revision, arguments.os, arguments.mode)
    except (OSError, UnicodeError, ValueError) as error:
        print(f"Native report validation failed: {error}", file=sys.stderr)
        return 1
    print(f"Validated {arguments.os} {arguments.mode} rendering observations; manual QA remains separate")
    return 0


if __name__ == "__main__":
    sys.exit(main())
