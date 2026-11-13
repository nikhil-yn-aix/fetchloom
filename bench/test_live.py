import tempfile
import os
import unittest


class LiveTests(unittest.TestCase):
    def test_live_checker_excludes_only_native_checksum_reports(self):
        from pathlib import Path
        import shutil
        import live
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        (root / "generated").mkdir(parents=True)
        try:
            (root / "value").write_bytes(b"abc")
            (root / "generated" / "checksums123.json").write_text("{}")
            # what a client keeps for itself is no part of what it fetched
            for cache in (".cache", "sub/.fetchloom"):
                (root / cache).mkdir(parents=True)
                (root / cache / "entry").write_bytes(b"kept")
            actual = live.fingerprint(root, client="datahugger")
            self.assertEqual(actual, {"value": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"})
            self.assertEqual(len(live.fingerprint(root)), 2)
            # a client that answers with one file, as kagglehub does, is that file alone
            self.assertEqual(live.fingerprint(root / "value"), {"value": actual["value"]})
        finally:
            shutil.rmtree(root)
    def test_live_comparison_reports_layout_and_content_separately(self):
        import live
        self.assertEqual(live.compare({"a": "1"}, {"x/a": "1"})["status"], "layout-difference")
        self.assertEqual(live.compare({"a": "1"}, {"a": "2"})["status"], "content-difference")
        self.assertEqual(live.compare({"a": "1"}, {"a": "1"})["status"], "identical")
        self.assertEqual(live.compare({}, {})["status"], "empty")
        self.assertEqual(live.compare({"a": "1", "b": "1"}, {"a": "1"})["status"], "content-difference")

@unittest.skipUnless(os.environ.get("FL_BENCH_LIVE_CHECKS") == "1", "requires public-service access")
class NativeIntegrityTests(unittest.TestCase):
    def check_client(self, service, message):
        import live
        import tools
        import lab
        import run
        from pathlib import Path
        import shutil
        dataset = live.catalog(service, True)
        dataset["expected_sha256"] = {item["name"]: "0" * 64 for item in dataset["files"]}
        work = lab.ROOT / "fs" / "ext4" / "negative-check"
        tools.reset(work)
        try:
            for name in ("home", "cache", "scratch"):
                (work / name).mkdir()
            binary = os.environ.get("FL_BENCH_FETCHLOOM", "/home/fl/bin/fetchloom")
            env = tools.Tool(binary).env(work)
            env["KAGGLEHUB_CACHE"] = str(work / "cache" / "kaggle")
            result = run.measure(live.command(dataset["tool"], dataset, work, Path(binary)),
                                 work, env, work / "log", 120)
            self.assertFalse(result["timeout"])
            self.assertEqual(result["exit_code"], 1)
            self.assertIn(message, (work / "log").read_text())
        finally:
            shutil.rmtree(work)

    def test_hf_rejects_a_wrong_expected_hash(self):
        self.check_client("huggingface", "computed checksum did NOT match")

    def test_kaggle_rejects_a_wrong_expected_hash(self):
        self.check_client("kaggle", "wrong SHA-256")

class RecordTests(unittest.TestCase):
    def dataset(self):
        return dict(service="zenodo", tool="zenodo_get", ref="zenodo:1", native="1", select=[],
                    quick=True, files=[dict(name="input.bin", size=3)], bytes=3,
                    expected_sha256={"input.bin": "digest"})

    def test_metadata_precedes_the_first_measured_command(self):
        import contextlib
        import io
        import json
        from pathlib import Path
        import sys
        from unittest import mock
        import live
        import lab
        import run
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "owned.json").write_text("{}")
            out = root / "results"
            calls = []
            def measured(argv, work, env, logfile, *args, **kwargs):
                meta_path = out / "meta.json"
                self.assertTrue(meta_path.is_file(), "live metadata must exist before measurement")
                meta = json.loads(meta_path.read_text())
                self.assertEqual(meta["datasets"][0]["expected_sha256"], {"input.bin": "digest"})
                self.assertTrue(meta["orders"])
                self.assertTrue(meta["commands"])
                calls.append(argv)
                if len(calls) == 2:
                    raise KeyboardInterrupt
                return dict(wall_seconds=1, exit_code=0, timeout=False)
            metadata = dict(commands={}, orders=[], cases=[], tool_versions={})
            with mock.patch.object(sys, "argv", ["live.py", "--fetchloom", "/unused", "--quick", "--services", "zenodo", "--out", str(out)]), \
                 mock.patch.object(lab, "ROOT", root), mock.patch.object(lab, "require_root"), \
                 mock.patch.object(lab, "pin_server", return_value={}), \
                 mock.patch.object(lab, "drop_caches"), mock.patch.object(run, "metadata", return_value=metadata), \
                 mock.patch.object(live, "versions", return_value={}), \
                 mock.patch.object(live, "catalog", return_value=self.dataset()), \
                 mock.patch.object(live, "reference", return_value={"input.bin": "digest"}), \
                 mock.patch.object(run, "measure", side_effect=measured), \
                 mock.patch.object(run, "free_bytes", return_value=100), mock.patch.object(live.lab, "flush"), \
                 mock.patch.object(live, "fingerprint", return_value={"input.bin": "digest"}), \
                 contextlib.redirect_stdout(io.StringIO()), self.assertRaises(KeyboardInterrupt):
                live.main()
            rows = [json.loads(line) for line in (out / "runs.jsonl").read_text().splitlines()]
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["status"], "passed")
            self.assertEqual(len(json.loads((out / "meta.json").read_text())["commands"]), 2)

    def campaign(self, root, argv, **patches):
        """live.main with the lab, the services and the measuring stood in; its exit and output."""
        import contextlib
        import io
        import sys
        from unittest import mock
        import lab
        import live
        import run
        metadata = dict(commands={}, orders=[], cases=[], tool_versions={})
        stood = dict(pin_server=mock.patch.object(lab, "pin_server", return_value={}),
                     drop_caches=mock.patch.object(lab, "drop_caches"), flush=mock.patch.object(lab, "flush"),
                     metadata=mock.patch.object(run, "metadata", return_value=metadata),
                     versions=mock.patch.object(live, "versions", return_value={}),
                     free_bytes=mock.patch.object(run, "free_bytes", return_value=100))
        stood.update(patches)
        with contextlib.ExitStack() as stack:
            for patch in stood.values():
                stack.enter_context(patch)
            stack.enter_context(mock.patch.object(sys, "argv", ["live.py", "--fetchloom", "/unused", *argv]))
            stack.enter_context(mock.patch.object(lab, "ROOT", root))
            stack.enter_context(mock.patch.object(lab, "require_root"))
            shown = stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            told = stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            try:
                code = live.main()
            except SystemExit as stopped:
                code = stopped.code
        return code, shown.getvalue(), told.getvalue()

    def test_a_campaign_skips_a_service_it_cannot_reach_and_compares_each_round(self):
        import json
        from pathlib import Path
        from unittest import mock
        import live
        import run

        def catalog(service, quick):
            if service == "kaggle":
                raise ValueError("the public API returned no matching files")
            return self.dataset()

        def measured(argv, work, env, logfile, *args, **kwargs):
            # the native client fails; fetchloom passes
            return dict(wall_seconds=1, exit_code=int("zenodo_get" in argv[0]), timeout=False)

        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            (root / "owned.json").write_text("{}")
            out = root / "results"
            code, shown, _ = self.campaign(root, ["--quick", "--services", "kaggle,zenodo", "--out", str(out)],
                                           catalog=mock.patch.object(live, "catalog", side_effect=catalog),
                                           reference=mock.patch.object(live, "reference", return_value={"input.bin": "digest"}),
                                           measure=mock.patch.object(run, "measure", side_effect=measured),
                                           fingerprint=mock.patch.object(live, "fingerprint", return_value={"input.bin": "digest"}))
            rows = [json.loads(line) for line in (out / "runs.jsonl").read_text().splitlines()]
            meta = json.loads((out / "meta.json").read_text())
        self.assertEqual(code, 1)
        reason = "public metadata or reference unavailable: ValueError: the public API returned no matching files"
        self.assertEqual(rows[0], dict(kind="live", service="kaggle", status="skipped", reason=reason, quick=True))
        self.assertTrue(shown.startswith(f"kaggle: skipped: {reason}\n"))
        self.assertEqual(sorted((row["tool"], row["status"]) for row in rows[1:3]), [("fetchloom", "passed"), ("zenodo_get", "failed")])
        self.assertEqual((rows[3]["kind"], rows[3]["service"], rows[3]["round"], rows[3]["comparison"]["status"]),
                         ("comparison", "zenodo", 1, "identical"))
        self.assertEqual(len(rows), 4)
        self.assertEqual([dataset["service"] for dataset in meta["datasets"]], ["zenodo"])
        self.assertEqual([order["tools"] for order in meta["orders"]], [[row["tool"] for row in rows[1:3]]])

    def test_a_campaign_that_cannot_be_kept_apart_never_starts(self):
        from pathlib import Path
        from unittest import mock
        import live
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            out = root / "results"
            for changes, said in [(dict(out=root.parent / "elsewhere"), f"write results under {root}"),
                                  (dict(), "run lab.py setup first"),
                                  (dict(owned=True, runs=True), "output already contains runs")]:
                with self.subTest(said):
                    if changes.get("owned"):
                        (root / "owned.json").write_text("{}")
                    if changes.get("runs"):
                        out.mkdir()
                        (out / "runs.jsonl").write_text("")
                    with mock.patch.object(live, "catalog") as catalog:
                        code, _, told = self.campaign(root, ["--out", str(changes.get("out", out))])
                    self.assertEqual((code, told.splitlines()[-1]), (2, f"live.py: error: {said}"))
                    catalog.assert_not_called()

    def test_checker_exception_is_a_failed_record(self):
        import json
        from pathlib import Path
        from unittest import mock
        import live
        import lab
        import run
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            work = root / "work"
            meta = dict(commands={})
            with mock.patch.object(lab, "drop_caches"), \
                 mock.patch.object(run, "measure", return_value=dict(wall_seconds=1, exit_code=0, timeout=False)), \
                 mock.patch.object(run, "free_bytes", return_value=100), mock.patch.object(live.lab, "flush"), \
                 mock.patch.object(live, "fingerprint", side_effect=OSError("checker read failed")):
                row = live.one_run("fetchloom", self.dataset(), work, root, 1, Path("/unused"), meta, 30)
            self.assertEqual(row["status"], "failed")
            self.assertIn("checker read failed", row["error"])
            self.assertEqual(json.loads((root / "runs.jsonl").read_text())["status"], "failed")


class RequestTests(unittest.TestCase):
    def test_a_request_names_the_benchmark_and_its_json_is_read(self):
        import http.server
        import threading
        import urllib.request
        from unittest import mock
        import live
        agents = []

        class Answer(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                agents.append(self.headers["User-Agent"])
                body = b'{"files": [{"key": "iris.csv"}]}'
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_):
                pass

        server = http.server.HTTPServer(("127.0.0.1", 0), Answer)
        threading.Thread(target=server.serve_forever, kwargs={"poll_interval": .05}, daemon=True).start()
        # a direct opener, so no proxy of the machine stands between
        direct = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            with mock.patch.object(live.urllib.request, "urlopen", direct.open):
                answer = live.get_json(f"http://127.0.0.1:{server.server_address[1]}/api/records/1")
        finally:
            server.shutdown()
            server.server_close()
        self.assertEqual((answer, agents), ({"files": [{"key": "iris.csv"}]}, ["fetchloom-benchmark/1"]))


class CatalogTests(unittest.TestCase):
    """each service's entry, from answers the rust source tests recorded from the services themselves."""

    def catalog(self, service, quick, answers, extra=(), edit=None):
        import json
        from pathlib import Path
        from unittest import mock
        import live
        sources = Path(live.__file__).resolve().parents[1] / "tests/fixtures/sources"
        asked = []

        def get_json(url):
            asked.append(url)
            answer = json.loads((sources / answers[url]).read_text())
            if edit:
                edit(answer)
            return answer + list(extra) if isinstance(answer, list) else answer

        with mock.patch.object(live, "get_json", side_effect=get_json):
            try:
                return live.catalog(service, quick)
            finally:
                self.assertEqual(asked, list(answers))

    def test_zenodo_lists_the_record_files_with_their_md5(self):
        entry = self.catalog("zenodo", True, {"https://zenodo.org/api/records/3608990": "zenodo/iris.json"})
        self.assertEqual(entry, dict(service="zenodo", select=[], quick=True, ref="zenodo:3608990", native="3608990",
                                     tool="zenodo_get", bytes=4177, files=[dict(
                                         name="iris.csv", size=4177, checksum="md5:395d6abd001abf3760a1a794ef4c453c",
                                         url="https://zenodo.org/api/records/3608990/files/iris.csv/content")]))
        # the full run selects one file, which the iris record does not have
        with self.assertRaisesRegex(ValueError, "the public API returned no matching files"):
            self.catalog("zenodo", False, {"https://zenodo.org/api/records/8164711": "zenodo/iris.json"})

    def test_hugging_face_selects_by_pattern_at_one_revision_with_each_lfs_sha256(self):
        revision = "e6281661ce1c48d982bc483cf8a173c1bbeb5d31"
        tree = f"https://huggingface.co/api/datasets/stanfordnlp/imdb/tree/{revision}?recursive=true&expand=true"
        # a partitioned parquet dataset is a folder named like a file, which is no file to fetch
        partitioned = dict(type="directory", oid="0" * 40, size=0, path="plain_text/partitioned.parquet")
        entry = self.catalog("huggingface", False, {tree: "hf/imdb-tree.json"}, extra=[partitioned])
        base = f"https://huggingface.co/datasets/stanfordnlp/imdb/resolve/{revision}/plain_text"
        self.assertEqual((entry["ref"], entry["revision"], entry["select"]),
                         (f"hf:datasets/stanfordnlp/imdb@{revision}", revision, ["plain_text/*.parquet"]))
        self.assertEqual(entry["files"], [
            dict(name="plain_text/test-00000-of-00001.parquet", size=20470363, url=f"{base}/test-00000-of-00001.parquet",
                 checksum="sha256:b52e26e2f872d282ffac460bf9770b25ac6f102cda0e6ca7158df98c94e8b3da"),
            dict(name="plain_text/train-00000-of-00001.parquet", size=20979968, url=f"{base}/train-00000-of-00001.parquet",
                 checksum="sha256:db47d16b5c297cc0dd625e519c81319c24c9149e70e8496de5475f6fa928342c"),
            dict(name="plain_text/unsupervised-00000-of-00001.parquet", size=41996509, url=f"{base}/unsupervised-00000-of-00001.parquet",
                 checksum="sha256:74d14fbfcbb39fb7d299c38ca9f0ae6d231bf97108da85d620027ba437b6d52e"),
        ])
        self.assertEqual(entry["bytes"], 20470363 + 20979968 + 41996509)
        # a smoke run asks for the repository's newest revision first, then the tree at it
        with self.assertRaisesRegex(ValueError, "no matching files"):
            self.catalog("huggingface", True, {"https://huggingface.co/api/datasets/scikit-learn/iris": "hf/imdb-revision.json",
                                               f"https://huggingface.co/api/datasets/scikit-learn/iris/tree/{revision}"
                                               "?recursive=true&expand=true": "hf/imdb-tree.json"})

    def test_kaggle_takes_the_selected_file_of_the_version(self):
        entry = self.catalog("kaggle", True, {"https://www.kaggle.com/api/v1/datasets/list/uciml/iris?datasetVersionNumber=2":
                                              "kaggle/iris-files.json"})
        self.assertEqual((entry["ref"], entry["native"], entry["tool"]), ("kaggle:uciml/iris@2", "uciml/iris/versions/2", "kagglehub"))
        self.assertEqual(entry["files"], [dict(name="Iris.csv", size=5107, checksum=None,
                                               url="https://api.kaggle.com/v1/datasets/download/uciml/iris/Iris.csv"
                                                   "?datasetVersionNumber=2&raw=true")])

    def test_figshare_takes_every_file_and_a_smoke_run_only_those_under_a_kib(self):
        # the article recorded is another, with figshare's answer shape
        answers = {"https://api.figshare.com/v2/articles/5616403": "figshare/article.json"}
        entry = self.catalog("figshare", False, answers)
        self.assertEqual((entry["ref"], entry["native"], entry["select"], entry["bytes"]),
                         ("figshare:5616403", "10.6084/m9.figshare.5616403", [], 158574 + 162562))
        self.assertEqual(entry["files"][0], dict(name="labt_a_2743468_sm3772.docx", size=158574,
                                                 url="https://ndownloader.figshare.com/files/70482987",
                                                 checksum="md5:4adbc16263f40b9f4eb22a52bf94372f"))
        with self.assertRaisesRegex(ValueError, "no matching files"):
            self.catalog("figshare", True, answers)

    def test_dataverse_takes_original_files_by_folder(self):
        base = "https://dataverse.harvard.edu"
        answers = {base + "/api/datasets/:persistentId/?persistentId=doi:10.7910/DVN/TJCLKP": "dataverse/dataset.json"}
        entry = self.catalog("dataverse", False, answers)
        self.assertEqual(entry["version"], "5.0")
        self.assertEqual(entry["files"], [
            dict(name="data/2026-06-24.tsv", size=25314, url=base + "/api/access/datafile/14027661?format=original", checksum=None),
            dict(name="code/language.py", size=590, url=base + "/api/access/datafile/6867336?format=original", checksum=None),
            dict(name="data/primary/primary-data.zip", size=276577, url=base + "/api/access/datafile/14027662?format=original",
                 checksum=None),
        ])
        # a file restricted to some users is left out, as it cannot be fetched without an account

        def restrict(answer):
            answer["data"]["latestVersion"]["files"][2]["restricted"] = True

        open_files = self.catalog("dataverse", False, answers, edit=restrict)["files"]
        self.assertEqual([item["name"] for item in open_files], ["data/2026-06-24.tsv", "code/language.py"])
        smoke = self.catalog("dataverse", True, answers)
        self.assertEqual(([item["name"] for item in smoke["files"]], smoke["select"], smoke["max_file_size"]),
                         (["code/language.py"], ["**/language.py"], 1024))


class ClientTests(unittest.TestCase):
    def setUp(self):
        from pathlib import Path
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.work = Path(folder.name)

    def test_the_reference_copy_must_have_the_size_and_checksum_the_api_gave(self):
        import io
        from unittest import mock
        import live

        def request(url):
            return io.BytesIO(b"abc")

        abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        for checksum, size, said in [("md5:900150983cd24fb0d6963f7d28e17f72", 3, None), (None, 3, None),
                                     ("md5:" + "0" * 32, 3, "reference checksum mismatch for x"),
                                     (None, 4, "reference size differs from API for x")]:
            with self.subTest(checksum=checksum, size=size), mock.patch.object(live, "request", side_effect=request):
                dataset = dict(files=[dict(name="x", size=size, url="https://example.org/x", checksum=checksum)])
                folder = self.work / f"{checksum}-{size}"
                if said is None:
                    self.assertEqual(live.reference(dataset, folder), {"x": abc})
                else:
                    with self.assertRaisesRegex(ValueError, said):
                        live.reference(dataset, folder)

    def test_each_client_gets_the_selection_and_the_check_it_offers(self):
        import lab
        import live
        output = str(self.work / "output")
        zenodo = dict(service="zenodo", ref="zenodo:1", native="1", select=["*.csv", "*.md"])
        self.assertEqual(live.command("fetchloom", zenodo, self.work, "/bin/fetchloom"),
                         ["/bin/fetchloom", "-q", "--yes", "get", "zenodo:1", "--no-extract", "--dir", output,
                          "--select", "*.csv", "--select", "*.md"])
        self.assertEqual(live.command("zenodo_get", zenodo, self.work, "/bin/fetchloom"),
                         [str(lab.VENV / "bin" / "zenodo_get"), "1", "-o", output, "--glob", "*.csv", "--glob", "*.md"])
        hf = dict(service="huggingface", native="scikit-learn/iris", revision="abc", select=["Iris.csv"],
                  expected_sha256={"Iris.csv": "f" * 64})
        self.assertEqual(live.command("hf", hf, self.work, "/bin/fetchloom"), ["bash", str(self.work / "command.sh")])
        # hf download checks no hash of its own, so the command checks each file after it
        self.assertEqual((self.work / "command.sh").read_text().splitlines()[-1], f"sha256sum -c {self.work / 'SHA256SUMS'}")
        self.assertEqual((self.work / "SHA256SUMS").read_text(), f"{'f' * 64}  {output}/Iris.csv\n")
        kaggle = dict(service="kaggle", native="uciml/iris/versions/2", select=["Iris.csv"], expected_sha256={"Iris.csv": "e" * 64})
        self.assertEqual(live.command("kagglehub", kaggle, self.work, "/bin/fetchloom"), [live.PYTHON, str(self.work / "download.py")])
        script = (self.work / "download.py").read_text()
        self.assertIn("kagglehub.dataset_download('uciml/iris/versions/2', path='Iris.csv')", script)
        self.assertIn(f"if actual != {'e' * 64!r}:\n    raise ValueError('downloaded file has wrong SHA-256')", script)
        for service, prelude in [("figshare", False), ("dataverse", True)]:
            with self.subTest(service):
                dataset = dict(service=service, native="10.1/x", select=[], max_file_size=1024)
                live.command("datahugger", dataset, self.work, "/bin/fetchloom")
                script = (self.work / "download.py").read_text()
                self.assertTrue(script.endswith(f"datahugger.get('10.1/x', {output!r}, checksum=True, unzip=False, max_file_size=1024)\n"))
                # dataverse refuses python's default user agent
                self.assertEqual("'User-Agent': 'fetchloom-benchmark/1 datahugger/0.13'" in script, prelude)

    def test_each_python_client_reports_its_version_once_or_why_it_has_none(self):
        import subprocess
        from unittest import mock
        import live
        asked = []

        def version(argv, **_):
            asked.append(argv)
            if "hf_xet" in argv[-1]:
                raise subprocess.CalledProcessError(1, argv)
            return "1.0\n"

        with mock.patch.object(live.subprocess, "check_output", side_effect=version):
            found = live.versions(["figshare", "dataverse", "huggingface"])
        # figshare and dataverse share datahugger, which is asked once
        self.assertEqual(found, {"datahugger": "1.0", "huggingface_hub": "1.0",
                                 "hf_xet": f"unavailable: {subprocess.CalledProcessError(1, asked[-1])}"})
        self.assertEqual(len(asked), 3)
        with mock.patch.object(live.subprocess, "check_output", side_effect=FileNotFoundError):
            self.assertEqual(live.versions(["zenodo"]), {"zenodo_get": "no python env"})

    def test_an_anonymous_kaggle_refusal_is_a_skip_not_a_failure(self):
        import json
        from unittest import mock
        import lab
        import live
        import run
        dataset = dict(service="kaggle", native="uciml/iris/versions/2", select=["Iris.csv"], quick=True,
                       expected_sha256={"Iris.csv": "e" * 64}, bytes=5107)

        said = ["kagglehub.exceptions.UnauthenticatedError: 401 Client Error\n"]

        def refused(argv, work, env, logfile, *args, **kwargs):
            logfile.write_text(said[-1])
            return dict(wall_seconds=1, exit_code=1, timeout=False)

        with mock.patch.object(lab, "drop_caches"), mock.patch.object(run, "measure", side_effect=refused), \
             mock.patch.object(run, "free_bytes", return_value=100), mock.patch.object(live.lab, "flush"):
            row = live.one_run("kagglehub", dataset, self.work / "work", self.work, 1, "/bin/fetchloom", dict(commands={}), 30)
            self.assertEqual((row["status"], row["reason"]), ("skipped", "kagglehub refused anonymous access; see command log"))
            # any other failure of the same client is a failure
            said.append("OSError: [Errno 28] No space left on device\n")
            row = live.one_run("kagglehub", dataset, self.work / "work", self.work, 2, "/bin/fetchloom", dict(commands={}), 30)
            self.assertEqual((row["status"], "reason" in row), ("failed", False))
        self.assertEqual([json.loads(line)["status"] for line in (self.work / "runs.jsonl").read_text().splitlines()],
                         ["skipped", "failed"])


if __name__ == "__main__":
    unittest.main()
