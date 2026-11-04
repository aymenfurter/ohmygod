import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/release-notes.py"


class ReleaseValidation(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        self.records = []
        for target, suffix in [
            ("aarch64-apple-darwin", "macos-arm64.dmg"),
            ("x86_64-apple-darwin", "macos-x64.dmg"),
            ("x86_64-pc-windows-msvc", "windows-x64.exe"),
        ]:
            name = f"OH-MY-GOD_1.2.3_{suffix}"
            data = target.encode()
            (self.dist / name).write_bytes(data)
            path = self.dist / f"{name}.json"
            record = {"version": "1.2.3", "target": target, "commit": "abc123",
                      "file": name, "sha256": hashlib.sha256(data).hexdigest()}
            path.write_text(json.dumps(record))
            self.records.append(path)

    def run_release(self):
        return subprocess.run([sys.executable, str(SCRIPT), "1.2.3", "abc123"],
                              cwd=self.root, text=True, capture_output=True)

    def test_complete_release_has_three_private_download_links_and_checksums(self):
        result = self.run_release()
        self.assertEqual(result.returncode, 0, result.stderr)
        notes = (self.dist / "release-notes.md").read_text()
        self.assertEqual(notes.count("[Download]"), 3)
        self.assertIn("private repository", notes)
        self.assertIn("not code-signed", notes)
        self.assertEqual(len((self.dist / "SHA256SUMS").read_text().splitlines()), 3)

    def test_missing_platform_fails(self):
        self.records[0].unlink()
        self.assertNotEqual(self.run_release().returncode, 0)
        self.assertFalse((self.dist / "release-notes.md").exists())

    def test_mixed_version_revision_filename_and_checksum_fail(self):
        path = self.records[0]
        original = json.loads(path.read_text())
        for key in ["version", "commit", "file", "sha256"]:
            with self.subTest(key=key):
                record = {**original, key: "wrong"}
                path.write_text(json.dumps(record))
                self.assertNotEqual(self.run_release().returncode, 0)
                self.assertFalse((self.dist / "release-notes.md").exists())

    def test_changed_installer_fails(self):
        record = json.loads(self.records[0].read_text())
        (self.dist / record["file"]).write_bytes(b"changed")
        self.assertNotEqual(self.run_release().returncode, 0)
