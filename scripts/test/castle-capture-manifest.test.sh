#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
python3 - "$repo_root/scripts/castle-capture-manifest.py" <<'PY'
import importlib.util
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("capture_manifest", sys.argv[1])
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)

class CaptureManifestTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.fixture = self.directory / "capital.vhc"
        header = b"VHCAST03" + struct.pack("<IIqIIIqqqqqqIII", 3, 36, 1, 0, 0, 0,
                                             0, 1, 0, 0, 0, 0, 63, 69, 63)
        self.fixture.write_bytes(header + bytes(2 * 63 * 69 * 63))
        self.commit = "a" * 40
        Path(str(self.fixture) + ".json").write_text(json.dumps({
            "source_commit": self.commit, "sha256": capture.sha256(self.fixture)}))
        self.report = self.directory / "exterior.txt"
        self.report.write_text(f"source_commit={self.commit}\nsource_dirty=false\nworldgen=36\nseed=1\nactual_facing=0\nreview_turn=0\nscene_mode=0\n")
        # A synthetic header is enough for this manifest reader; the GPU harness
        # separately requires a completed, fully decodable image before reporting it.
        (self.directory / "exterior.png").write_bytes(
            b"\x89PNG\r\n\x1a\n" + bytes(8) + struct.pack(">II", 1280, 720))

    def test_indexes_matching_source_without_machine_paths(self):
        result = capture.manifest(self.directory)
        self.assertEqual(result["captures"][0]["artifact"], "exterior.png")
        self.assertNotIn(str(self.directory), json.dumps(result))

    def test_rejects_fixture_changed_after_server_export(self):
        self.fixture.write_bytes(self.fixture.read_bytes()[:-2] + b"\x01\x00")
        with self.assertRaisesRegex(ValueError, "server export digest"):
            capture.manifest(self.directory)

    def test_rejects_uncommitted_or_different_source(self):
        original = self.report.read_text()
        for changed in [original.replace("source_dirty=false", "source_dirty=true"),
                        original.replace(self.commit, "b" * 40)]:
            self.report.write_text(changed)
            with self.assertRaises(ValueError):
                capture.manifest(self.directory)

    def test_rejects_trace_holes_and_nonfinite_positions(self):
        path = self.directory / "route.tsv"
        header = "tick\tlocal_feet_x\tlocal_feet_y\tlocal_feet_z\n"
        for row in ["1\t1\t2\t3", "0\tNaN\t2\t3", "0\t1\t2", ""]:
            path.write_text(header + row)
            with self.assertRaises(ValueError):
                capture.trace_points(path)
        path.write_text(header + "0\t31.5\t0\t62.5\n1\t31.5\t0\t62.49\n")
        self.assertEqual(capture.trace_points(path)[-1], [31.5, 0, 62.49])
        for row in ["0\t1\t2\t3", "0\t31.5\t0\t62.5\n1\t32.5\t0\t62.5"]:
            path.write_text(header + row)
            with self.assertRaisesRegex(ValueError, "canonical gate"):
                capture.trace_points(path)

unittest.main(argv=[sys.argv[0]])
PY
