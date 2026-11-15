"""the workflows pin each tool's version once, in their env, agree on the tools they share and run
on named images."""

import itertools
from pathlib import Path
import re
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = sorted((ROOT / ".github/workflows").glob("*.yml"))


def pins(path):
    """the PIN_ variables a workflow's env sets, name to value."""
    return dict(re.findall(r"^  (PIN_\w+): (\S+)$", path.read_text(encoding="utf-8"), re.MULTILINE))


def jobs(name):
    """each job of the workflow ``name``, its id to its text, split at the two-space keys under jobs:."""
    text = (ROOT / ".github/workflows" / name).read_text(encoding="utf-8").split("\njobs:\n", 1)[1]
    parts = re.split(r"(?m)^  ([\w-]+):\n", text)
    return dict(zip(parts[1::2], parts[2::2]))


class WorkflowTests(unittest.TestCase):
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

    def test_jobs_run_on_named_images(self):
        """a -latest label moves to a new image without a change here, so a job never uses one, nor
        does a dispatch option that picks a job's runner."""
        for path in WORKFLOWS:
            with self.subTest(workflow=path.name):
                self.assertNotRegex(path.read_text(encoding="utf-8"), r"(?m)^\s+(?:runs-on|options): .*-latest\b")

    def test_checkouts_name_a_full_ref(self):
        """actions/checkout takes a branch over a tag of the same short name, so a ref such as
        v0.1.0 would publish a branch someone pushed under that name."""
        for path in WORKFLOWS:
            for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                if re.match(r"\s+ref: ", line):
                    with self.subTest(workflow=path.name, line=number):
                        self.assertRegex(line, r"^\s+ref: refs/(?:heads|tags)/")


    def test_release_packages_are_checked_before_they_are_published_or_signed(self):
        """python/check_release.py verifies each package's attestation, which takes attestations:
        read, before pypi gets the wheels or the apt key signs the debian packages."""
        for workflow, job, then in [("publish.yml", "pypi", "uses: pypa/gh-action-pypi-publish@"),
                                    ("pages.yml", "pages", "gpg --batch --yes --armor --detach-sign")]:
            with self.subTest(workflow=workflow):
                text = jobs(workflow)[job]
                self.assertIn("\n      attestations: read\n", text)
                self.assertLess(text.index("python3 python/check_release.py "), text.index(then))

    def test_the_apt_key_never_shares_a_job_with_the_docs_toolchain(self):
        """the site's toolchain comes from pypi and runs with the runner's sudo, so its job holds no
        secret and no right to deploy; the signing job makes apt/ afresh, since its signed Release
        lists every package found there."""
        site, pages = jobs("pages.yml")["site"], jobs("pages.yml")["pages"]
        self.assertIn("python python/build_site.py", site)
        self.assertNotRegex(site, r"secrets\.|environment:|: write\b")
        self.assertIn("secrets.APT_SIGNING_KEY", pages)
        for toolchain in ["setup-uv", "docs-site", "build_site.py"]:
            with self.subTest(toolchain=toolchain):
                self.assertNotIn(toolchain, pages)
        self.assertRegex(pages, r"\n +mkdir site/apt\n")


    def test_the_crates_token_reaches_a_publish_that_builds_nothing(self):
        """cargo publish builds the crate first unless told not to, which runs every build script
        and proc macro of the tree with the token in their environment."""
        given = [step for step in jobs("publish.yml")["crates"].split("\n      - ")[1:] if "secrets." in step]
        self.assertEqual(len(given), 1)
        self.assertRegex(given[0], r"\n        run: cargo publish --locked --no-verify(?: #[^\n]*)?\n")


    def test_packages_are_kept_before_unpinned_tools_install(self):
        """apt and brew install whatever version they hold, so in a build job such a step after the
        build comes after the packages are uploaded, when it can no longer change them."""
        build = jobs("artifacts.yml")["build"]
        after = build[build.index("&compile"):]
        installs = list(re.finditer(r"apt-install\.sh [^\n]*|brew install [^\n]*", after))
        self.assertTrue(installs)
        for install in installs:
            with self.subTest(install=install[0]):
                self.assertGreater(install.start(), after.index("name: packages-"))


    def test_install_action_never_falls_back(self):
        """for a tool or version it has no checked release of, install-action otherwise turns to
        cargo-binstall, which takes a binary from wherever it finds one."""
        for path in WORKFLOWS:
            lines = path.read_text(encoding="utf-8").splitlines()
            for number, line in enumerate(lines):
                if "uses: taiki-e/install-action@" in line:
                    step = itertools.takewhile(lambda following: following.startswith(" " * 8), lines[number + 1:])
                    with self.subTest(workflow=path.name, line=number + 1):
                        self.assertIn(" " * 10 + "fallback: none", list(step))


if __name__ == "__main__":
    unittest.main()
