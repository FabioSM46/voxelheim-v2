"""Small exporter boundary checks, run remotely by the manual workflow."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("exporter", Path(__file__).parents[1] / "export-hoard-evidence.py")
EXPORTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPORTER)


class EvidenceBoundary(unittest.TestCase):
    def setUp(self):
        os.environ.update(HOARD_SOURCE_COMMIT="a" * 40, GITHUB_RUN_ID="123", GITHUB_RUN_ATTEMPT="1", GITHUB_SERVER_URL="https://github.com", GITHUB_REPOSITORY="example/renamed-project")

    def exported(self, raw, code):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "public.json"
            passed = EXPORTER.export(raw, target, code)
            return passed, json.loads(target.read_text())

    def test_raw_diagnostics_never_leave_runner(self):
        raw = 'private diagnostic sentinel\nHOARD_EVIDENCE {"Version":1,"Success":true,"Members":[]}\n'
        passed, report = self.exported(raw, 0)
        self.assertTrue(passed)
        self.assertNotIn("private diagnostic sentinel", json.dumps(report))
        self.assertEqual(report["source_commit"], "a" * 40)
        self.assertEqual(report["run_url"], "https://github.com/example/renamed-project/actions/runs/123/attempts/1")

    def test_failure_and_missing_report_fail_closed(self):
        for raw, code in [("private diagnostic", 0), ('HOARD_EVIDENCE {"Version":1,"Success":true}', 1), ('HOARD_EVIDENCE {"Version":1,"Success":false}', 0)]:
            passed, report = self.exported(raw, code)
            self.assertFalse(passed)
            self.assertFalse(report["success"])

    def test_paths_and_credential_like_prose_rejected(self):
        for prose in ["/private/example", "ticket=synthetic", "contact@example.com"]:
            raw = "HOARD_EVIDENCE " + json.dumps({"Version": 1, "Success": True, "Notes": [prose]})
            passed, report = self.exported(raw, 0)
            self.assertFalse(passed)
            self.assertEqual(report["report_status"], "rejected-scenario-report")
            self.assertIsNone(report["observations"])

    def test_duplicate_reports_are_ambiguous(self):
        row = 'HOARD_EVIDENCE {"Version":1,"Success":true}\n'
        passed, report = self.exported(row + row, 0)
        self.assertFalse(passed)
        self.assertEqual(report["report_status"], "ambiguous-scenario-report")


if __name__ == "__main__":
    unittest.main()
