"""the workflows pin each tool's version once, in their env, and agree on the tools they share."""

from pathlib import Path
import re
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = sorted((ROOT / ".github/workflows").glob("*.yml"))


def pins(path):
    """the PIN_ variables a workflow's env sets, name to value."""
    return dict(re.findall(r"^  (PIN_\w+): (\S+)$", path.read_text(encoding="utf-8"), re.MULTILINE))


class PinTests(unittest.TestCase):
    def test_workflows_that_install_one_tool_pin_one_version(self):
        seen = {}
        for path in WORKFLOWS:
            for name, value in pins(path).items():
                with self.subTest(workflow=path.name, pin=name):
                    self.assertEqual(seen.setdefault(name, value), value)
        nextest = tomllib.loads((ROOT / ".config/nextest.toml").read_text(encoding="utf-8"))
        self.assertEqual(seen["PIN_NEXTEST"], "cargo-nextest@" + nextest["nextest-version"]["required"])

    def test_steps_take_versions_from_the_pins(self):
        for path in WORKFLOWS:
            for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                with self.subTest(workflow=path.name, line=number):
                    self.assertNotRegex(line, r"^\s+(?:tool|version|checksum): [^$]*\d", "name a PIN_ variable")


if __name__ == "__main__":
    unittest.main()
