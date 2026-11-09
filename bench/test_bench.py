import os
import tempfile
import unittest

import fixtures
import lab


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

    def test_process_exit_codes_are_distinct(self):
        import run
        import os
        from pathlib import Path
        import shutil
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        try:
            passed = run.measure(["/bin/true"], root, os.environ, root / "log", 2)
            self.assertEqual(passed["exit_code"], 0)
            failed = run.measure(["/bin/false"], root, os.environ, root / "log", 2)
            self.assertEqual(failed["exit_code"], 1)
            self.assertFalse(passed["timeout"])
            self.assertFalse(passed["interruption_sent"])
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
            self.assertEqual(run.measure(["bash", "-c", "kill -INT $$"], root, os.environ, root / "log", 5)["exit_code"], -2)
            self.assertEqual(run.measure(["bash", "-c", "exit 3"], root, os.environ, root / "log", 5)["exit_code"], 3)
            self.assertEqual(run.measure(["no-such-benchmark-tool"], root, os.environ, root / "log", 5)["exit_code"], 127)
            self.assertEqual(sorted(path.name for path in root.iterdir()), ["log"])
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
            with mock.patch.object(lab, "PREPARE_TIMEOUT", 1):
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

    def test_a_comparison_pairs_passed_rounds_and_skips_warmups(self):
        import ab
        def row(tool, number, wall, status="passed", warmup=False):
            return dict(tool=tool, filesystem="ext4", profile="lan", scenario="s3", round=number,
                        warmup=warmup, status=status, wall_seconds=wall, user_seconds=1, sys_seconds=1,
                        maxrss_bytes=100, disk_bytes=10)
        rows = [row("fetchloom", 0, 1.0, warmup=True), row("fetchloom+lean", 0, 99.0, warmup=True),
                row("fetchloom", 1, 2.0), row("fetchloom+lean", 1, 1.0),
                row("fetchloom", 2, 4.0), row("fetchloom+lean", 2, 3.0),
                row("fetchloom", 3, 1.0), row("fetchloom+lean", 3, 9.0, status="failed"),
                row("curl", 1, 0.1)]
        [result] = ab.compare(rows)
        self.assertEqual(result["case"], "ext4/lan/s3")
        self.assertEqual((result["rounds"], result["wins"]), (2, 2))
        self.assertEqual(result["failed"], {"base": 0, "variant": 1})
        self.assertAlmostEqual(result["ratios"]["wall"], (0.5 * 0.75) ** 0.5)
        self.assertEqual(result["ratios"]["memory"], 1.0)
        self.assertIn("fetchloom+lean", ab.table([result]))


if __name__ == "__main__":
    unittest.main()
