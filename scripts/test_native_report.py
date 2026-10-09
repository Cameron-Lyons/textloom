"""Regression checks for false-green or content-bearing native smoke evidence."""

import json
import unittest

from check_native_report import SCHEMA, reject_constant, unique_object, validate


def specimen(mode="editable"):
    def empty(spec):
        if isinstance(spec, dict):
            return {key: empty(value) for key, value in spec.items()}
        return (spec[0] if isinstance(spec, tuple) else spec)()

    report = empty(SCHEMA)
    report.update(schema_version=1, manual_signoff="not_recorded")
    report["host"].update(
        name="textloom-native-example", egui_api_version="0.36",
        eframe_api_version="0.36", os="linux", architecture="x86_64",
        source_revision="candidate",
    )
    report["session"].update(graceful_exit=True, smoke_test_requested=True, rendered_frames=21)
    report["display"].update(
        pixels_per_point=2, minimum_pixels_per_point=1,
        maximum_pixels_per_point=2, native_pixels_per_point=None,
    )
    report["final_widget"].update(enabled=mode != "disabled", read_only=mode == "read-only")
    report["final_document"]["tlfr_round_trip_equal"] = True
    return report


class NativeReportTests(unittest.TestCase):
    def test_modes_with_no_manual_signoff(self):
        for mode in ("editable", "read-only", "disabled"):
            with self.subTest(mode=mode):
                validate(specimen(mode), "candidate", "linux", mode)

    def test_stale_incomplete_or_failed_evidence(self):
        for section, key, value in (
            ("host", "source_revision", "old-commit"),
            ("host", "os", "windows"),
            ("session", "rendered_frames", 19),
            ("session", "rendered_frames", True),
            ("session", "graceful_exit", False),
            ("session", "smoke_test_requested", False),
            ("errors", "accessibility", 1),
            ("final_document", "tlfr_round_trip_equal", False),
            ("final_widget", "composition_active", True),
            ("final_widget", "read_only", True),
        ):
            with self.subTest(section=section, key=key):
                report = specimen()
                report[section][key] = value
                with self.assertRaises(ValueError):
                    validate(report, "candidate", "linux", "editable")

    def test_content_and_manual_signoff_are_rejected(self):
        examples = []
        report = specimen()
        report["document_content_included"] = True
        examples.append(report)
        report = specimen()
        report["manual_signoff"] = "passed"
        examples.append(report)
        report = specimen()
        report["final_document"]["text"] = "private document"
        examples.append(report)
        report = specimen()
        report["host_input_events"]["paste"] = "private clipboard"
        examples.append(report)
        for index, report in enumerate(examples):
            with self.subTest(report=index):
                with self.assertRaises(ValueError):
                    validate(report, "candidate", "linux", "editable")

    def test_disabled_retained_focus_and_focus_gain_are_rejected(self):
        for section, key in (
            ("final_widget", "widget_focus_retained"),
            ("observed_state_changes", "focus_gains"),
        ):
            report = specimen("disabled")
            report[section][key] = True if section == "final_widget" else 1
            with self.assertRaises(ValueError):
                validate(report, "candidate", "linux", "disabled")

    def test_missing_evidence_duplicate_fields_and_nonfinite_json(self):
        report = specimen()
        del report["final_widget"]["widget_focus_retained"]
        with self.assertRaises(ValueError):
            validate(report, "candidate", "linux", "editable")
        for source in ('{"errors": 1, "errors": 0}', '{"frames": NaN}'):
            with self.assertRaises(ValueError):
                json.loads(source, object_pairs_hook=unique_object, parse_constant=reject_constant)


if __name__ == "__main__":
    unittest.main()
