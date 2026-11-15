"""the workflows pin each tool's version once, in their env, agree on the tools they share and run
on named images, and keep the supply-chain rules docs/dev/engineering/ci.md lists."""

import itertools
import os
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
        seen, tools = {}, {}
        for path in WORKFLOWS:
            for name, value in pins(path).items():
                with self.subTest(workflow=path.name, pin=name):
                    self.assertEqual(seen.setdefault(name, value), value)
                for tool, _, version in (part.rpartition("@") for part in value.split(",")):
                    if tool:
                        with self.subTest(workflow=path.name, tool=tool):
                            self.assertEqual(tools.setdefault(tool, version), version)
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
        """each package's attestation is verified, which takes attestations: read, before pypi gets
        the wheels or the apt key signs the debian packages."""
        for workflow, job, check, then in [
            ("publish.yml", "pypi", "python3 python/check_release.py ", "uses: pypa/gh-action-pypi-publish@"),
            ("pages.yml", "pages", "gh attestation verify ", "gpg --batch --yes --armor --detach-sign"),
        ]:
            with self.subTest(workflow=workflow):
                text = jobs(workflow)[job]
                self.assertIn("\n      attestations: read\n", text)
                self.assertLess(text.index(check), text.index(then))

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

    def test_the_apt_key_job_runs_no_repository_file(self):
        """a script pushed to main could put a stand-in gpg on the path, or LD_PRELOAD in the
        environment, of the step that signs, so the job holding the key checks out nothing and runs
        no action of this repository, which $/ would fetch without a checkout."""
        pages = jobs("pages.yml")["pages"]
        self.assertNotRegex(pages, r"uses: (?:actions/checkout@|\./|\$/)")
        self.assertNotRegex(pages, r"python3? python/|\.github/scripts/")

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


    def test_a_weekly_run_audits_the_workflows_online(self):
        """ci.yml's zizmor runs offline, which leaves out the audits that ask github whether a
        pinned commit belongs to its action and whether an action has a known advisory."""
        text = (ROOT / ".github/workflows/deep.yml").read_text(encoding="utf-8")
        self.assertIn("\n  schedule:\n", text)
        self.assertRegex(text, r"\npermissions:\n  contents: read\n\n")
        audit = jobs("deep.yml")["workflows"]
        self.assertIn("if: github.event_name == 'schedule' ||", audit)
        self.assertIn("GH_TOKEN: ${{ github.token }}", audit)
        self.assertRegex(audit, r"\n        run: zizmor --strict-collection \.github\n")


    def test_dependabot_updates_every_lockfile_a_week_after_each_release(self):
        """bench/uv.lock is left out: its pins are the tools the benchmark measures."""
        lockfiles = {"Cargo.lock": "cargo", "package-lock.json": "npm", "uv.lock": "uv"}
        expected = {("github-actions", "/")}
        for folder, children, files in os.walk(ROOT):
            children[:] = [child for child in children
                           if child not in ("target", "node_modules", "site") and child[:1] != "." or child == ".github"]
            relative = Path(folder).relative_to(ROOT).as_posix()
            place = "/" if relative == "." else "/" + relative
            if place != "/bench":
                expected |= {(lockfiles[name], place) for name in set(files) & set(lockfiles)}
        entries = re.split(r"(?m)^  - ", (ROOT / ".github/dependabot.yml").read_text(encoding="utf-8"))[1:]
        updated = {(re.search(r"^package-ecosystem: (\S+)", entry, re.MULTILINE)[1],
                    re.search(r"^    directory: (\S+)", entry, re.MULTILINE)[1]) for entry in entries}
        self.assertEqual(updated, expected)
        for entry in entries:
            with self.subTest(entry=entry.split("\n", 1)[0]):
                self.assertRegex(entry, r"\n    cooldown:\n      default-days: 7\n")


    def test_cargo_deny_refuses_yanked_crates(self):
        """cargo-deny only warns about a release its authors withdrew unless told to refuse it."""
        deny = tomllib.loads((ROOT / ".config/deny.toml").read_text(encoding="utf-8"))
        self.assertEqual(deny["advisories"].get("yanked"), "deny")


if __name__ == "__main__":
    unittest.main()
