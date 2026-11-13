import argparse
import contextlib
import errno
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import fixtures
import lab
import tools


class LabPathTests(unittest.TestCase):
    def paths(self, **settings):
        import json
        from pathlib import Path
        import subprocess
        import sys
        env = {key: value for key, value in os.environ.items()
               if key not in ("FL_BENCH_ROOT", "FL_BENCH_VENV")}
        env.update(settings)
        probe = """import json, tempfile
from pathlib import Path
import lab, live, tools
with tempfile.TemporaryDirectory() as folder:
    work = Path(folder)
    pooch = tools.Pooch("fetchloom").command(dict(id="s1", files=[]), work)[0]
    zenodo = live.command("zenodo_get", dict(native="1", select=[]), work, "fetchloom")[0]
    print(json.dumps([str(lab.ROOT), lab.environment()["TMPDIR"],
                      lab.environment()["PATH"].split(":")[0], pooch, live.PYTHON, zenodo]))
"""
        return json.loads(subprocess.check_output([sys.executable, "-c", probe],
                                                  cwd=Path(lab.__file__).parent, env=env, text=True))

    def test_lab_paths_default_to_the_documented_locations(self):
        self.assertEqual(self.paths(), ["/srv/fl-bench", "/srv/fl-bench/scratch",
                                        "/opt/fl-bench/venv/bin", "/opt/fl-bench/venv/bin/python",
                                        "/opt/fl-bench/venv/bin/python", "/opt/fl-bench/venv/bin/zenodo_get"])

    def test_lab_paths_follow_the_environment_overrides(self):
        self.assertEqual(self.paths(FL_BENCH_ROOT="/data/lab", FL_BENCH_VENV="/data/venv"),
                         ["/data/lab", "/data/lab/scratch", "/data/venv/bin",
                          "/data/venv/bin/python", "/data/venv/bin/python", "/data/venv/bin/zenodo_get"])

    def test_lab_path_aliases_resolve_before_lifecycle_and_output_checks(self):
        from pathlib import Path
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            actual = root / "actual"
            actual.mkdir()
            alias = root / "alias"
            alias.symlink_to(actual, target_is_directory=True)
            paths = self.paths(FL_BENCH_ROOT=str(alias), FL_BENCH_VENV=str(alias / "venv"))
            self.assertEqual(paths[0], str(actual))
            self.assertEqual(paths[1], str(actual / "scratch"))
            self.assertEqual(paths[2], str(actual / "venv" / "bin"))
            self.assertTrue((alias / "results").resolve().is_relative_to(Path(paths[0])))


class FixtureTests(unittest.TestCase):
    def test_seed_controls_entropy(self):
        a = fixtures.content(17, "a", 4096)
        self.assertEqual(len(a), 4096)
        self.assertEqual(a, fixtures.content(17, "a", 4096))
        self.assertNotEqual(a, fixtures.content(18, "a", 4096))
        self.assertNotEqual(a, fixtures.content(17, "b", 4096))
        self.assertGreater(len(set(a)), 240)

    def test_cases_use_only_design_profiles(self):
        cases = fixtures.case_specs(["lan", "home"], ["s1", "s3", "s7"], False)
        self.assertEqual([(c["profile"], c["id"]) for c in cases],
                         [("lan", "s1"), ("lan", "s3"), ("home", "s1"), ("home", "s7")])
        self.assertEqual(cases[0]["size"], 1073741824)
        self.assertEqual(cases[1]["count"], 10000)

    def test_the_compressed_archive_and_the_large_no_op_run_on_lan(self):
        cases = fixtures.case_specs(["lan", "home"], ["s8", "s9"], False)
        self.assertEqual([(c["profile"], c["id"], c["base"]) for c in cases], [("lan", "s8", "s8"), ("lan", "s9", "s3")])
        [tiny] = fixtures.case_specs(["lan"], ["s3"], False)
        self.assertEqual((cases[1]["count"], cases[1]["size"]), (tiny["count"], tiny["size"]))
        self.assertEqual(cases[1]["size"], 4096)

    def test_shaping_splits_roundtrip_and_limits_server(self):
        client, server = lab.shaping("home")
        self.assertIn("10ms", client)
        self.assertIn("10ms", server)
        self.assertNotIn("rate", client)
        self.assertEqual(server[-2:], ["rate", "100mbit"])

    def test_forgetting_connections_resets_only_the_lab(self):
        from unittest import mock
        server_flush = ["ip", "-n", lab.NS, "tcp_metrics", "flush", "all"]
        for shown, wanted in (("10.0.0.5 age 3sec rtt 90us\n10.77.0.2 age 1sec rtt 300us\n",
                               [server_flush, ["ip", "tcp_metrics", "flush", lab.SERVER_ADDRESS]]),
                              ("10.0.0.5 age 3sec rtt 90us\n", [server_flush])):
            with mock.patch.object(lab, "call") as call, \
                 mock.patch.object(lab.subprocess, "check_output", return_value=shown):
                lab.forget_connections()
            self.assertEqual([c.args[0] for c in call.call_args_list], wanted)



class MeasurementTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("FL_BENCH_LAB_CHECKS") == "1", "needs a set-up lab")
    def test_a_profile_without_a_rate_drops_the_last_cap(self):
        import subprocess
        def server_shaping():
            return subprocess.check_output(["ip", "netns", "exec", lab.NS, "tc", "qdisc", "show", "dev",
                                            lab.SERVER, "root"], text=True).split()
        for name in ("poor", "lan", "home", "far", "lan"):
            lab.profile(name)
            rate = lab.PROFILES[name][2]
            shown = server_shaping()
            self.assertEqual("rate" in shown, bool(rate), (name, shown))
            if rate:
                self.assertIn(f"{rate}Mbit", shown, (name, shown))

    @unittest.skipUnless(os.environ.get("FL_BENCH_LAB_CHECKS") == "1", "needs a set-up lab")
    def test_server_answers_a_later_if_modified_since_with_not_modified(self):
        # curl -z sends the local file's time, which is when it was downloaded, not the server's time
        from email.utils import formatdate
        import time
        import urllib.error
        import urllib.request
        path = lab.ROOT / "www" / "if-modified-since"
        path.write_text("fl-bench\n")
        try:
            os.utime(path, (time.time() - 3600, time.time() - 3600))
            request = urllib.request.Request("https://lab.flb/if-modified-since",
                                             headers={"If-Modified-Since": formatdate(time.time() - 60, usegmt=True)})
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            with self.assertRaises(urllib.error.HTTPError) as caught:
                opener.open(request, timeout=5).close()
            self.assertEqual(caught.exception.code, 304)
        finally:
            path.unlink()

    def test_environment_inherits_no_keys_or_settings(self):
        from unittest import mock
        inherited = dict(PATH="/usr/bin", LANG="C.UTF-8", HF_TOKEN="hf_x", HUGGING_FACE_HUB_TOKEN="hf_y",
                         ZENODO_TOKEN="z", KAGGLE_KEY="k", KAGGLE_CONFIG_DIR="/k", NETRC="/n",
                         DATAVERSE_API_TOKEN="d", AWS_SECRET_ACCESS_KEY="a", FETCHLOOM_JOBS="1",
                         XDG_CONFIG_HOME="/c", DBUS_SESSION_BUS_ADDRESS="unix:path=/b")
        with mock.patch.dict(lab.os.environ, inherited, clear=True):
            env = lab.environment()
        self.assertEqual({key for key in inherited if key in env}, {"PATH", "LANG"})

    def test_exclusive_lock_rejects_overlapping_runs(self):
        from pathlib import Path
        import shutil
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        try:
            with lab.exclusive(root / "lock"):
                with self.assertRaises(RuntimeError):
                    lab.exclusive(root / "lock")
            with lab.exclusive(root / "lock"):
                pass
        finally:
            shutil.rmtree(root)

    def test_checker_rejects_missing_and_wrong_files(self):
        import run
        from pathlib import Path
        import shutil
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        try:
            path = root / "value"
            path.write_bytes(b"abc")
            self.assertEqual(run.verify([(path, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")]), [])
            self.assertEqual(len(run.verify([(path, "0" * 64)])), 1)
            self.assertEqual(len(run.verify([(root / "missing", "0" * 64)])), 1)
        finally:
            shutil.rmtree(root)

    def test_a_run_reports_how_its_command_ended_and_leaves_only_its_log(self):
        import run
        import os
        from pathlib import Path
        import shutil
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        try:
            # a signal reads as its negative number, as in subprocess, and a missing tool as 127
            for argv, code in [(["/bin/true"], 0), (["/bin/false"], 1), (["bash", "-c", "exit 3"], 3),
                               (["bash", "-c", "kill -INT $$"], -2), (["no-such-benchmark-tool"], 127)]:
                with self.subTest(argv=argv):
                    result = run.measure(argv, root, os.environ, root / "log", 5)
                    self.assertEqual(result["exit_code"], code)
                    self.assertFalse(result["timeout"])
                    self.assertFalse(result["interruption_sent"])
            self.assertEqual(sorted(path.name for path in root.iterdir()), ["log"])
        finally:
            shutil.rmtree(root)

    def test_peak_memory_excludes_the_harness(self):
        import run
        from pathlib import Path
        import shutil
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        ballast = bytearray(os.urandom(200 * 1024 * 1024))
        try:
            small = run.measure(["/bin/true"], root, os.environ, root / "log", 5)
            self.assertLess(small["maxrss_bytes"], 32 * 1024 * 1024)
            large = run.measure(["python3", "-c", "b = bytearray(100 * 1024 * 1024); b[::4096] = b'x' * len(b[::4096])"],
                                root, os.environ, root / "log", 30)
            self.assertGreater(large["maxrss_bytes"], 100 * 1024 * 1024)
            self.assertLess(large["maxrss_bytes"], 160 * 1024 * 1024)
        finally:
            del ballast
            shutil.rmtree(root)

    def test_preparation_timeout_kills_the_whole_group(self):
        import subprocess
        from pathlib import Path
        from unittest import mock
        import shutil
        import tools
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        child = root / "child"
        try:
            class Slow(tools.Tool):
                def command(self, scenario, workdir):
                    return tools.script(workdir / "command.sh", [tools.shell(["bash", "-c", 'echo $$ > "$0"; exec sleep 60', child])])
            with mock.patch.object(lab, "PREPARE_TIMEOUT", 0.3):
                with self.assertRaises(subprocess.TimeoutExpired):
                    Slow("fetchloom").untimed({}, root)
            with self.assertRaises(ProcessLookupError):
                os.kill(int(child.read_text()), 0)
        finally:
            shutil.rmtree(root)

    def test_preparation_passes_input_and_reports_failure(self):
        import subprocess
        from pathlib import Path
        import shutil
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        try:
            with (root / "out").open("w") as out:
                lab.run_group(["cat"], input="batch\n", text=True, stdout=out)
            self.assertEqual((root / "out").read_text(), "batch\n")
            with self.assertRaises(subprocess.CalledProcessError):
                lab.run_group(["false"])
        finally:
            shutil.rmtree(root)

    def test_datalad_clones_outside_the_clock_even_for_a_second_project(self):
        from pathlib import Path
        from unittest import mock
        import shutil
        import tools
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        try:
            tool = tools.Datalad("fetchloom")
            work = root / "work"
            prepared = root / "prepared" / "datalad"
            calls = []
            def record(argv, **_):
                calls.append([str(x) for x in argv])
            with mock.patch.object(tools.lab, "run_group", record):
                tool.prepare(dict(id="s1", prepared=str(root / "prepared")), work)
                self.assertEqual(calls, [["datalad", "clone", str(prepared), str(work / "project")]])
                self.assertNotIn("datalad clone", tools.shell(tool.command(dict(id="s1"), work)) + (work / "command.sh").read_text())
                tools.reset(work)
                calls.clear()
                (work / "project").mkdir(parents=True, exist_ok=True)
                tool.prepare(dict(id="s5", prepared=str(root / "prepared")), work)
                self.assertEqual(calls[0], ["datalad", "clone", str(prepared), str(work / "project")])
                self.assertEqual(calls[-1], ["datalad", "clone", str(work / "first-project"), str(work / "project")])
                tool.command(dict(id="s5"), work)
                self.assertNotIn("datalad clone", (work / "command.sh").read_text())
        finally:
            shutil.rmtree(root)

    def test_free_space_is_read_once_it_stops_changing(self):
        from unittest import mock
        import run
        readings = iter([100, 150, 180, 180, 999])
        with mock.patch.object(run, "free_bytes", lambda _: next(readings)), \
             mock.patch.object(run.lab, "flush"), \
             mock.patch.object(run.time, "sleep", lambda _: None):
            self.assertEqual(run.settled_free_bytes("/"), 180)

    def test_dvc_keeps_its_site_cache_in_the_measured_folder(self):
        from pathlib import Path
        import tools
        work = lab.ROOT / "fs" / "ext4" / "work"
        env = tools.Dvc("fetchloom").env(work)
        self.assertTrue(Path(env["DVC_SITE_CACHE_DIR"]).is_relative_to(work))

    def test_each_case_draws_its_own_reproducible_orders(self):
        import run
        tools = ["a", "b", "c", "d", "e", "f", "g"]
        def orders(fs, profile, scenario):
            rng = run.case_rng(1729, fs, dict(profile=profile, id=scenario))
            drawn = []
            for _ in range(6):
                order = list(tools)
                rng.shuffle(order)
                drawn.append(order)
            return drawn
        self.assertEqual(orders("ext4", "lan", "s1"), orders("ext4", "lan", "s1"))
        self.assertNotEqual(orders("ext4", "lan", "s1"), orders("ext4", "lan", "s3"))
        self.assertNotEqual(orders("ext4", "lan", "s1"), orders("ext4", "home", "s1"))

    def test_access_counts_request_bytes_and_file_only_bytes(self):
        import run
        result = run.parse_access([
            '{"request":"HEAD /a HTTP/2.0","status":200,"bytes":0}',
            '{"request":"GET /a HTTP/2.0","status":206,"bytes":13}',
            '{"request":"GET /other HTTP/1.1","status":200,"bytes":7}',
        ], {"/a"})
        self.assertEqual(result, {"requests": 3, "bytes_served": 20, "file_bytes_served": 13})


class MetadataTests(unittest.TestCase):
    def test_native_inventory_keeps_installed_and_missing_packages(self):
        import run
        versions = run.native_versions(("python3", "fl-bench-package-missing"))
        self.assertRegex(versions[0], r"^python3\t3\.\d+\.")
        self.assertEqual(versions[1], "fl-bench-package-missing\tnot installed")

    def test_native_inventory_does_not_count_removed_packages_as_installed(self):
        import subprocess
        from unittest import mock
        import run
        answer = subprocess.CompletedProcess([], 0, "nginx\tconfig-files\t1.24.0\n", "")
        with mock.patch.object(run.subprocess, "run", return_value=answer):
            self.assertEqual(run.native_versions(("nginx",)), ["nginx\tnot installed"])

    def test_a_missing_python_environment_is_a_recorded_fact(self):
        from pathlib import Path
        from unittest import mock
        import run
        with tempfile.TemporaryDirectory() as folder, \
             mock.patch.object(lab, "VENV", Path(folder) / "absent"):
            self.assertEqual(run.python_packages(), ["no python env"])

    def test_metadata_does_not_require_a_python_environment(self):
        from pathlib import Path
        from types import SimpleNamespace
        from unittest import mock
        import run
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "owned.json").write_text('{"image_gib": 12}')
            args = SimpleNamespace(fs=[], seed=1729, quick=True, profiles=["lan"])
            with mock.patch.object(lab, "ROOT", root), \
                 mock.patch.object(lab, "VENV", root / "absent"):
                meta = run.metadata(args, {})
            self.assertEqual(meta["installed_packages"], ["no python env"])
            self.assertEqual(meta["tool_versions"], {})
            self.assertEqual(meta["lab_layout"], {"image_gib": 12})

    def test_a_broken_python_environment_keeps_its_error(self):
        from pathlib import Path
        from unittest import mock
        import run
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "bin").mkdir()
            (root / "bin" / "python").symlink_to("/bin/false")
            with mock.patch.object(lab, "VENV", root):
                [error] = run.python_packages()
            self.assertIn("python env unavailable", error)
            self.assertIn("exit status 1", error)


class ToolSelectionTests(unittest.TestCase):
    def test_unselected_adapters_are_not_constructed(self):
        from unittest import mock
        import tools
        class Unselected(tools.Tool):
            name = "unselected"
            def __init__(self, _):
                raise AssertionError("an unselected adapter was constructed")
        with mock.patch.object(tools, "CLASSES", (tools.Fetchloom, Unselected)):
            adapters = tools.available("fetchloom", [("aa", {"FETCHLOOM_EXP_NONE": "1"})],
                                       only=("fetchloom",))
        self.assertEqual(list(adapters), ["fetchloom", "fetchloom+aa"])

    def test_native_preparation_runs_only_the_selected_client(self):
        from pathlib import Path
        import sys
        from unittest import mock
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            fetchloom = root / "fetchloom"
            fetchloom.write_text(f"#!{sys.executable}\nfrom pathlib import Path\nimport sys\n"
                                 "assert sys.argv[1:] == ['-q', '--yes', 'lock']\n"
                                 "Path('data.lock').write_text('locked')\n")
            fetchloom.chmod(0o755)
            with mock.patch.object(lab, "ROOT", root):
                case = fixtures.prepare_native(dict(key="case", files=[]), fetchloom, ["fetchloom"])
            prepared = Path(case["prepared"])
            self.assertEqual(sorted(path.name for path in prepared.iterdir()),
                             ["fetchloom", "fetchloom-prepare.log"])
            self.assertEqual((prepared / "fetchloom" / "data.lock").read_text(), "locked")
            self.assertTrue((prepared / "fetchloom" / ".ready").is_file())

    def test_live_versions_query_only_selected_services(self):
        from unittest import mock
        import live
        def version(argv, **_):
            self.assertEqual(argv[0], live.PYTHON)
            self.assertIn("version('zenodo_get')", argv[-1])
            return "3.1.0\n"
        with mock.patch.object(live.subprocess, "check_output", version):
            self.assertEqual(live.versions(["zenodo"]), {"zenodo_get": "3.1.0"})


class ScenarioTests(unittest.TestCase):
    def test_the_downloaders_and_pooch_unpack_the_compressed_archive(self):
        from pathlib import Path
        import tools
        case = dict(id="s8", members=[], files=[dict(name="members.tar.gz", sha256="0" * 64, size=1,
                                                     url="https://lab.flb/key/members.tar.gz")])
        with tempfile.TemporaryDirectory() as folder:
            work = Path(folder)
            for adapter in (tools.Curl, tools.Wget, tools.Aria2c):
                adapter("unused").command(case, work)
                self.assertEqual((work / "command.sh").read_text().splitlines()[-1],
                                 "tar -xzf data/members.tar.gz -C data", adapter.name)
            tools.Pooch("unused").command(case, work)
            self.assertIn("processor=pooch.Untar(extract_dir='unpacked')", (work / "retrieve.py").read_text())
        self.assertEqual([adapter.name for adapter in tools.CLASSES if not adapter("unused").supports(case)],
                         ["dvc", "datalad"])

    def test_the_large_no_op_skips_work_as_the_small_one_does(self):
        from pathlib import Path
        from unittest import mock
        import tools
        files = [dict(name="file-00000.bin", sha256="0" * 64, size=1, url="https://lab.flb/key/file-00000.bin")]
        with tempfile.TemporaryDirectory() as folder:
            work = Path(folder)
            for adapter, flag in ((tools.Curl, "-z"), (tools.Wget, "-nc"), (tools.Aria2c, "--check-integrity=true")):
                adapter("unused").command(dict(id="s9", files=files, members=[]), work)
                self.assertIn(flag, (work / "command.sh").read_text().split(), adapter.name)
            seen = []
            class Probe(tools.Tool):
                name = "probe"
                def command(self, scenario, workdir):
                    seen.append(scenario["id"])
                    return ["true"]
            with mock.patch.object(tools.lab, "run_group"):
                Probe("unused").prepare(dict(id="s9"), work / "work")
            self.assertEqual(seen, ["s3"])


class VariantTests(unittest.TestCase):
    def test_a_variant_names_only_distinct_fetchloom_settings(self):
        import tools
        self.assertEqual(tools.variant("lean-2:FETCHLOOM_EXP_A=1,FETCHLOOM_EXP_B=x=y"),
                         ("lean-2", {"FETCHLOOM_EXP_A": "1", "FETCHLOOM_EXP_B": "x=y"}))
        for bad in ("FETCHLOOM_EXP_A=1", "Lean:FETCHLOOM_EXP_A=1", "lean:PATH=/x", "lean:FETCHLOOM_EXP_A",
                    "lean:FETCHLOOM_CACHE=/elsewhere", "lean:FETCHLOOM_EXP_A=1,FETCHLOOM_EXP_A=2"):
            with self.assertRaises(ValueError, msg=bad):
                tools.variant(bad)

    def test_a_variant_syncs_the_plain_builds_project_with_its_settings(self):
        from pathlib import Path
        import tools
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            prepared = root / "prepared"
            (prepared / "fetchloom").mkdir(parents=True)
            for name in ("data.toml", "data.lock"):
                (prepared / "fetchloom" / name).write_text(name)
            adapters = tools.available("/bin/fetchloom", [("lean", {"FETCHLOOM_EXP_LEAN": "1"})])
            plain, lean = adapters["fetchloom"], adapters["fetchloom+lean"]
            self.assertEqual(lean.prepared_as, "fetchloom")
            work = root / "work"
            (work / "project").mkdir(parents=True)
            lean.populate(dict(prepared=str(prepared)), work)
            self.assertEqual((work / "project" / "data.lock").read_text(), "data.lock")
            self.assertEqual(lean.command({}, work), plain.command({}, work))
            self.assertEqual(lean.env(work)["FETCHLOOM_EXP_LEAN"], "1")
            self.assertEqual(lean.env(work)["FETCHLOOM_CACHE"], plain.env(work)["FETCHLOOM_CACHE"])
            self.assertNotIn("FETCHLOOM_EXP_LEAN", plain.env(work))


def stand_in(path, program):
    """``path`` as an executable python script running ``program``."""
    path.write_text(f"#!{sys.executable}\n{program}")
    path.chmod(0o755)
    return path


class ToolTests(unittest.TestCase):
    FILES = [dict(name=f"file-0000{index}.bin", sha256="0" * 64, size=1, url=f"https://lab.flb/key/file-0000{index}.bin")
             for index in range(2)]

    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.work = Path(folder.name)

    def lines(self, adapter, scenario, files=None, members=()):
        adapter("unused").command(dict(id=scenario, files=files or self.FILES[:1], members=list(members)), self.work)
        return (self.work / "command.sh").read_text().splitlines()

    def test_a_version_the_tool_cannot_give_is_the_reason_why(self):
        class Missing(tools.Tool):
            name = "no-such-benchmark-tool"

        self.assertEqual(Missing("unused").version(), "[Errno 2] No such file or directory: 'no-such-benchmark-tool'")
        failing = stand_in(self.work / "failing", "raise SystemExit(3)\n")

        class Failing(tools.Tool):
            name = str(failing)

        self.assertEqual(Failing("unused").version(), f"Command '['{failing}', '--version']' returned non-zero exit status 3.")
        fetchloom = stand_in(self.work / "fetchloom", "print('fetchloom 0.1.0')\n")
        self.assertEqual(tools.Fetchloom(fetchloom).version(), "fetchloom 0.1.0")
        (self.work / "bin").mkdir()
        stand_in(self.work / "bin/python", "print(' 1.8.2 ')\n")
        with mock.patch.object(lab, "VENV", self.work):
            self.assertEqual(tools.Pooch("unused").version(), "1.8.2")

    def test_each_downloader_fetches_every_file_and_resumes_the_interrupted_one(self):
        curl = self.lines(tools.Curl, "s1", self.FILES)[2].split()
        # one transfer each, one after another in a single parallel curl
        self.assertEqual(curl.count("--next"), 1)
        self.assertEqual(curl.count("--output"), 2)
        # each continues the file the interrupted first attempt left, and only then
        resumed = self.lines(tools.Curl, "s7")[2].split()
        self.assertEqual(resumed[resumed.index("-C") + 1], "-")
        self.assertIn("-c", self.lines(tools.Wget, "s7")[2].split())
        for adapter, flag in [(tools.Curl, "-C"), (tools.Wget, "-c")]:
            with self.subTest(adapter.name):
                self.assertNotIn(flag, self.lines(adapter, "s1")[2].split())

    def test_the_zip_is_unpacked_and_each_member_checked_where_its_tool_puts_it(self):
        zipped = [dict(name="members.zip", sha256="0" * 64, size=1, url="https://lab.flb/key/members.zip")]
        members = [dict(name="file-00000.bin", sha256="1" * 64)]
        for adapter in (tools.Curl, tools.Wget, tools.Aria2c):
            with self.subTest(adapter.name):
                self.assertEqual(self.lines(adapter, "s4", zipped, members)[-1], "unzip -q data/members.zip -d data")
        case = dict(id="s4", files=zipped, members=members)
        self.assertEqual(tools.Fetchloom("unused").outputs(case, self.work),
                         [(self.work / "project/data/f00000/file-00000.bin", "1" * 64)])
        self.assertEqual(tools.Pooch("unused").outputs(case, self.work),
                         [(self.work / "cache/pooch/unpacked/file-00000.bin", "1" * 64)])
        plain = dict(id="s1", files=self.FILES, members=[])
        self.assertEqual(tools.Pooch("unused").outputs(plain, self.work),
                         [(self.work / "cache/pooch" / item["name"], item["sha256"]) for item in self.FILES])
        self.assertEqual(tools.Curl("unused").outputs(plain, self.work),
                         [(self.work / "project/data" / item["name"], item["sha256"]) for item in self.FILES])

    def test_a_second_project_is_set_up_after_the_untimed_first(self):
        populated = []

        class Probe(tools.Tool):
            name = "probe"

            def populate(self, scenario, workdir):
                populated.append(scenario["id"])
                (workdir / "project" / scenario["id"]).write_text("")

            def command(self, scenario, workdir):
                return ["true"]

        with mock.patch.object(tools.lab, "run_group") as run:
            Probe("unused").prepare(dict(id="s5"), self.work)
        # the cache comes from an untimed run of s2, then a project of its own takes s5's files
        self.assertEqual(populated, ["s2", "s5"])
        self.assertEqual(run.call_count, 1)
        self.assertEqual(sorted(path.name for path in (self.work / "first-project").iterdir()), ["s2"])
        self.assertEqual(sorted(path.name for path in (self.work / "project").iterdir()), ["s5"])

    def test_dvc_pulls_into_a_copy_of_its_prepared_project_with_its_cache_in_the_work_folder(self):
        prepared = self.work / "prepared"
        (prepared / "dvc" / ".dvc").mkdir(parents=True)
        (prepared / "dvc" / "data.dvc").write_text("outs: []\n")
        workdir = self.work / "run"
        dvc = tools.Dvc("unused")
        with mock.patch.object(tools.subprocess, "run") as configured:
            dvc.prepare(dict(id="s1", prepared=str(prepared)), workdir)
        self.assertEqual((workdir / "project/data.dvc").read_text(), "outs: []\n")
        # the cache is in the measured folder, so its disk use counts as fetchloom's does
        self.assertEqual(configured.call_args.args[0], ["dvc", "config", "cache.dir", str(workdir / "cache/dvc")])
        self.assertEqual((configured.call_args.kwargs["cwd"], configured.call_args.kwargs["check"]), (workdir / "project", True))
        self.assertEqual(dvc.cwd(workdir), workdir / "project")
        self.assertEqual(dvc.command(dict(id="s1"), workdir), ["dvc", "pull", "-j", "4"])


class LabHelperTests(unittest.TestCase):
    def test_cpu_lists_name_available_cpus(self):
        self.assertEqual(lab.cpu_list(" ", available=[3, 1]), (1, 3))
        with self.assertRaisesRegex(ValueError, "no CPUs are available"):
            lab.cpu_list("", available=[])
        with mock.patch.object(lab.os, "sched_getaffinity", return_value={0, 1}):
            self.assertEqual(lab.cpu_choice("0-1"), (0, 1))
            with self.assertRaisesRegex(argparse.ArgumentTypeError, r"CPUs are not available: \[5\]"):
                lab.cpu_choice("1,5")

    def test_a_group_left_behind_is_killed_then_given_up_on(self):
        clock = iter(range(0, 100))
        with mock.patch.object(lab.time, "monotonic", side_effect=lambda: next(clock)), \
             mock.patch.object(lab.time, "sleep"), mock.patch.object(lab.os, "killpg") as kill:
            lab.end_group(4242)
        # probed until the deadline, killed, then probed for five seconds more
        self.assertEqual(kill.call_args_list[1], mock.call(4242, lab.signal.SIGKILL))
        self.assertTrue(all(call == mock.call(4242, 0) for call in kill.call_args_list[2:]))
        with mock.patch.object(lab.os, "killpg", side_effect=[None, ProcessLookupError]) as kill, \
             mock.patch.object(lab.time, "sleep"):
            lab.end_group(4242)
        self.assertEqual(kill.call_args_list, [mock.call(4242, 0), mock.call(4242, lab.signal.SIGKILL)])

    def test_a_group_whose_command_ran_past_its_time_is_still_reaped(self):
        # the group is gone by the time it is killed, as when its command ended on its own
        with mock.patch.object(lab.os, "killpg", side_effect=ProcessLookupError), self.assertRaises(subprocess.TimeoutExpired):
            lab.run_group([sys.executable, "-c", "import time; time.sleep(.3)"], timeout=.05)

    def test_commands_write_scratch_files_under_the_lab_once_it_has_a_scratch_folder(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(lab, "ROOT", Path(folder)), \
             mock.patch.object(lab.subprocess, "run") as run:
            lab.call(["ip", "link"])
            self.assertNotIn("env", run.call_args.kwargs)
            (Path(folder) / "scratch").mkdir()
            lab.call(["ip", "link"])
            self.assertEqual(run.call_args.kwargs["env"]["TMPDIR"], str(Path(folder) / "scratch"))
            self.assertEqual(run.call_args.args[0], ["ip", "link"])

    def test_the_lab_refuses_to_run_without_root(self):
        with mock.patch.object(lab.os, "geteuid", return_value=1000), \
             self.assertRaisesRegex(SystemExit, "the lab needs root for namespaces, mounts and drop_caches"):
            lab.require_root()
        with mock.patch.object(lab.os, "geteuid", return_value=0):
            self.assertIsNone(lab.require_root())

    def test_the_lab_writes_out_its_root_and_the_test_filesystems_mounted_under_it(self):
        root = Path("/srv/fl-bench")
        with mock.patch.object(lab, "ROOT", root), mock.patch.object(lab.os.path, "ismount", side_effect=lambda path: path.name == "xfs"):
            self.assertEqual(lab.filesystems(), [root, root / "fs" / "xfs"])
        # syncfs of no file fails as the kernel says, and touches no filesystem
        with self.assertRaises(OSError) as raised:
            lab._syncfs(-1)
        self.assertEqual(raised.exception.errno, errno.EBADF)
        with tempfile.TemporaryFile() as written:
            self.assertIsNone(lab._syncfs(written.fileno()))

    def test_dropping_caches_writes_out_the_lab_first_then_reads_the_given_files_back(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            drop, served = root / "drop_caches", root / "served.bin"
            served.write_bytes(b"abc")
            flushed = []
            with mock.patch.object(lab, "require_root"), mock.patch.object(lab, "DROP", drop), \
                 mock.patch.object(lab, "filesystems", return_value=[root]), \
                 mock.patch.object(lab, "flush", side_effect=lambda *paths: flushed.append((paths, drop.exists()))):
                lab.drop_caches([served])
                # the kernel drops only clean pages, so the writes go out before the drop
                self.assertEqual((flushed, drop.read_text()), ([((root,), False)], "3\n"))
                with self.assertRaises(FileNotFoundError):
                    lab.drop_caches([root / "gone.bin"])


class LabLifecycleTests(unittest.TestCase):
    """setup, teardown and profiles, with the machine paths moved to a scratch folder and ip and tc
    stood in."""

    def setUp(self):
        self.fresh()

    def fresh(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root, self.ca, self.hosts = Path(folder.name) / "lab", Path(folder.name) / "fl-bench.crt", Path(folder.name) / "hosts"
        self.hosts.write_text("127.0.0.1 localhost\n")

    @contextlib.contextmanager
    def machine(self, networks="", link=1, shown=None):
        """the lab's paths in the scratch folder; ip lists ``networks``, ``link`` is the exit of a
        look for the client link, and tc shows ``shown`` for each device."""
        def show(argv, **_):
            return shown[argv[-2]] if argv[-5:-2] == ["qdisc", "show", "dev"] else networks
        with mock.patch.object(lab, "ROOT", self.root), mock.patch.object(lab, "CA", self.ca), \
             mock.patch.object(lab, "HOSTS", self.hosts), mock.patch.object(lab, "require_root"), \
             mock.patch.object(lab.subprocess, "check_output", side_effect=show), \
             mock.patch.object(lab.subprocess, "run", return_value=subprocess.CompletedProcess([], link, "", "")):
            yield

    def test_setup_refuses_to_replace_what_it_did_not_make(self):
        for name, made, networks, link, said in [
            ("a folder with files", lambda: (self.root / "results").mkdir(parents=True), "", 1, "refusing to overwrite unowned {root}$"),
            ("a ca", lambda: self.ca.write_text("ca"), "", 1, "lab.flb or its CA already exists without an ownership marker"),
            ("a hosts line", lambda: self.hosts.write_text(lab.HOST_LINE), "", 1, "lab.flb or its CA already exists"),
            ("a namespace", lambda: None, f"{lab.NS} (id: 0)\n", 1, "unowned benchmark network already exists"),
            ("a link", lambda: None, "", 0, "unowned benchmark network already exists"),
        ]:
            with self.subTest(name):
                self.fresh()
                made()
                with self.machine(networks, link), mock.patch.object(lab, "call") as call, \
                     self.assertRaisesRegex(SystemExit, said.format(root=self.root)):
                    lab.setup()
                call.assert_not_called()
                self.assertFalse((self.root / "owned.json").exists())

    def test_a_setup_step_that_fails_tears_down_the_lab_it_replaced_and_what_it_made(self):
        self.root.mkdir()
        (self.root / "owned.json").write_text("{}")
        seen = []

        def teardown():
            seen.append(json.loads((self.root / "owned.json").read_text()))
            shutil.rmtree(self.root)

        with self.machine(), mock.patch.object(lab, "teardown", side_effect=teardown), \
             mock.patch.object(lab, "call", side_effect=subprocess.CalledProcessError(1, "openssl")) as call, \
             self.assertRaises(subprocess.CalledProcessError):
            lab.setup(image_gib=3)
        # the old lab first, then the new one, whose marker lets teardown know it is the lab's
        self.assertEqual(seen, [{}, {"image_gib": 3}])
        self.assertEqual(call.call_args.args[0][:2], ["openssl", "req"])
        self.assertEqual(self.hosts.read_text(), "127.0.0.1 localhost\n")

    def test_teardown_removes_what_setup_made_and_leaves_the_rest_of_the_machine(self):
        with self.machine(), contextlib.redirect_stdout(io.StringIO()) as said:
            lab.teardown()
        self.assertEqual(said.getvalue(), "lab is already absent\n")
        (self.root / "fs/ext4").mkdir(parents=True)
        (self.root / "owned.json").write_text("{}")
        self.ca.write_text("ca")
        self.hosts.write_text("127.0.0.1 localhost\n" + lab.HOST_LINE + "10.0.0.9 other\n")
        steps = []

        def ran(argv, **_):
            steps.append(argv)
            return subprocess.CompletedProcess(argv, 0, "4242 4243\n" if argv[:3] == ["ip", "netns", "pids"] else "", "")

        with self.machine(), mock.patch.object(lab.subprocess, "run", side_effect=ran), \
             mock.patch.object(lab, "call", side_effect=lambda argv, **_: steps.append(argv)), \
             mock.patch.object(lab.os.path, "ismount", side_effect=lambda path: Path(path).name == "ext4"), \
             mock.patch.object(lab.os, "kill", side_effect=[None, ProcessLookupError]) as kill, \
             mock.patch.object(lab.time, "sleep"), contextlib.redirect_stdout(io.StringIO()) as said:
            lab.teardown()
        self.assertEqual(steps, [["umount", self.root / "fs/ext4"], ["ip", "netns", "pids", lab.NS], ["ip", "netns", "del", lab.NS],
                                 ["ip", "link", "del", lab.CLIENT], ["update-ca-certificates"]])
        self.assertEqual(kill.call_args_list, [mock.call(4242, 15), mock.call(4243, 15)])
        self.assertEqual(self.hosts.read_text(), "127.0.0.1 localhost\n10.0.0.9 other\n")
        self.assertEqual((self.ca.exists(), self.root.exists()), (False, False))
        self.assertEqual(said.getvalue(), "lab removed, including its results; copy wanted results before teardown\n")

    def test_a_profile_starts_from_no_shaping_and_must_take_at_both_ends(self):
        with self.machine(), mock.patch.object(lab, "call") as call, \
             self.assertRaisesRegex(RuntimeError, "run lab.py setup before changing a profile"):
            lab.profile("far")
        call.assert_not_called()
        self.root.mkdir()
        (self.root / "owned.json").write_text("{}")
        server = ["ip", "netns", "exec", lab.NS]
        plain = "qdisc netem 8001: root refcnt 2 limit 1000 delay 75ms loss 1%"
        for name, shown, said in [("far", {lab.CLIENT: plain, lab.SERVER: plain + " rate 50Mbit"}, None),
                                  ("far", {lab.CLIENT: plain, lab.SERVER: plain}, f"{lab.SERVER} did not take the far profile: {plain}"),
                                  # a cap the last profile set and this one leaves out
                                  ("lan", {lab.CLIENT: plain, lab.SERVER: plain + " rate 50Mbit"}, f"{lab.SERVER} did not take the lan profile")]:
            steps = []
            with self.subTest(name=name, said=said), self.machine(shown=shown), \
                 mock.patch.object(lab.subprocess, "run", side_effect=lambda argv, **_: steps.append(argv)), \
                 mock.patch.object(lab, "call", side_effect=lambda argv, **_: steps.append(argv)):
                if said:
                    with self.assertRaisesRegex(RuntimeError, said):
                        lab.profile(name)
                else:
                    lab.profile(name)
                    self.assertEqual(steps, [["tc", "qdisc", "del", "dev", lab.CLIENT, "root"],
                                             ["tc", "qdisc", "add", "dev", lab.CLIENT, "root", "netem", "delay", "75ms", "loss", "1%"],
                                             [*server, "tc", "qdisc", "del", "dev", lab.SERVER, "root"],
                                             [*server, "tc", "qdisc", "add", "dev", lab.SERVER, "root", "netem", "delay", "75ms",
                                              "loss", "1%", "rate", "50mbit"]])

    def test_the_command_sets_up_tears_down_or_changes_the_profile_under_the_lock(self):
        for argv, calls in [(["setup", "--image-gib", "3"], [mock.call.setup(3, (1,))]),
                            (["teardown"], [mock.call.teardown()]),
                            (["profile", "--profile", "far"], [mock.call.exclusive(), mock.call.exclusive().__enter__(),
                                                               mock.call.profile("far"), mock.call.exclusive().__exit__(None, None, None)])]:
            stood = mock.MagicMock()
            with self.subTest(argv[0]), mock.patch.object(sys, "argv", ["lab.py", *argv, "--cpus", "1"]), \
                 mock.patch.object(lab.os, "sched_getaffinity", return_value={0, 1}), \
                 mock.patch.multiple(lab, setup=stood.setup, teardown=stood.teardown, profile=stood.profile, exclusive=stood.exclusive):
                lab.main()
            self.assertEqual(stood.mock_calls, calls)


if __name__ == "__main__":
    unittest.main()
