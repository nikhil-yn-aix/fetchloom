"""fl added to a built wheel by the command artifacts.yml runs, with every other file kept."""

import base64
import glob
import hashlib
import shlex
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DIST_INFO = "fetchloom-0.1.0.dist-info"


def record_line(name, data):
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode("ascii")
    return f"{name},sha256={digest},{len(data)}\n"


class WheelScripts(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / "dist").mkdir()
        self.wheel = self.root / "dist" / "fetchloom-0.1.0-py3-none-manylinux2014_x86_64.whl"
        self.members = {
            "fetchloom/__init__.py": (b"import json\n", 0o644),
            "fetchloom/__main__.py": (b"def main():\n    return 0\n", 0o644),
            "fetchloom-0.1.0.data/scripts/fetchloom": (b"\x7fELF the executable", 0o755),
            f"{DIST_INFO}/METADATA": (b"Metadata-Version: 2.4\nName: fetchloom\nVersion: 0.1.0\n", 0o644),
            f"{DIST_INFO}/WHEEL": (b"Wheel-Version: 1.0\nRoot-Is-Purelib: false\n", 0o644),
        }

    def build(self, extra=None):
        members = {**self.members, **(extra or {})}
        record = "".join(record_line(name, data) for name, (data, _) in members.items())
        record += f"{DIST_INFO}/RECORD,,\n"
        members[f"{DIST_INFO}/RECORD"] = (record.encode("ascii"), 0o644)
        with zipfile.ZipFile(self.wheel, "w") as wheel:
            for name, (data, mode) in members.items():
                info = zipfile.ZipInfo(name, date_time=(2026, 11, 11, 0, 0, 0))
                info.external_attr = mode << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                wheel.writestr(info, data)
        return record

    def run_step(self):
        text = (ROOT / ".github/workflows/artifacts.yml").read_text(encoding="utf-8")
        steps = [line.strip() for line in text.splitlines()
                 if "python/wheel_scripts.py" in line and not line.strip().startswith("#")]
        self.assertEqual(len(steps), 1, "artifacts.yml must add the scripts to the wheels once")
        words = shlex.split(steps[0])
        after = words[words.index("python/wheel_scripts.py") + 1:]
        wheels = [path for word in after for path in sorted(glob.glob(word, root_dir=self.root))]
        return subprocess.run([sys.executable, str(ROOT / "python/wheel_scripts.py"), *wheels],
                              cwd=self.root, capture_output=True, text=True, timeout=30)

    def test_fl_is_added_and_every_other_file_kept(self):
        record = self.build()
        result = self.run_step()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        points = b"[console_scripts]\nfl=fetchloom.__main__:main\n"
        with zipfile.ZipFile(self.wheel) as wheel:
            self.assertEqual(wheel.read(f"{DIST_INFO}/entry_points.txt"), points)
            listed = record.replace(f"{DIST_INFO}/RECORD,,",
                                    record_line(f"{DIST_INFO}/entry_points.txt", points) + f"{DIST_INFO}/RECORD,,")
            self.assertEqual(wheel.read(f"{DIST_INFO}/RECORD").decode("ascii"), listed)
            for name, (data, mode) in self.members.items():
                with self.subTest(name):
                    self.assertEqual(wheel.read(name), data)
                    self.assertEqual(wheel.getinfo(name).external_attr >> 16, mode)
            self.assertEqual(wheel.namelist()[-1], f"{DIST_INFO}/RECORD")
        self.assertEqual([path.name for path in (self.root / "dist").iterdir()], [self.wheel.name])

    def test_a_wheel_that_already_has_entry_points_is_left_alone(self):
        self.build({f"{DIST_INFO}/entry_points.txt": (b"[console_scripts]\nother=other:main\n", 0o644)})
        before = self.wheel.read_bytes()
        result = self.run_step()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("entry_points.txt", result.stderr)
        self.assertEqual(self.wheel.read_bytes(), before)
        self.assertEqual([path.name for path in (self.root / "dist").iterdir()], [self.wheel.name])
