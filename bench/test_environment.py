from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

import lab
import tools


class EnvironmentTests(unittest.TestCase):
    def test_external_client_configuration_is_not_inherited(self):
        inherited = dict(PATH="/usr/bin:/bin", CURL_HOME="/foreign", CURL_CA_BUNDLE="/foreign/ca",
                         WGETRC="/foreign/wgetrc", GIT_DIR="/foreign/git", GIT_CONFIG_COUNT="1",
                         PYTHONPATH="/foreign/modules", PYTHONHOME="/foreign/python",
                         PYTHONUSERBASE="/foreign/user", PYTHONPYCACHEPREFIX="/foreign/bytecode",
                         DATALAD_RUNTIME_MAX_ANNEX_JOBS="1", DVC_CACHE_DIR="/foreign/dvc", POOCH_CACHE="/foreign/pooch")
        with mock.patch.dict(lab.os.environ, inherited, clear=True):
            env = lab.environment()
        for name in inherited.keys() - {"PATH"}:
            self.assertNotIn(name, env)
        self.assertEqual(env["GIT_CONFIG_NOSYSTEM"], "1")
        self.assertEqual(env["GIT_CONFIG_GLOBAL"], "/dev/null")

    @unittest.skipUnless(shutil.which("curl"), "requires curl")
    def test_curl_does_not_read_an_external_config_file(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            foreign = root / "foreign"
            foreign.mkdir()
            (foreign / ".curlrc").write_text('write-out = "foreign-config-read"\n')
            source = root / "source.bin"
            source.write_bytes(b"abc")
            case = dict(id="s1", files=[dict(name="input.bin", url=source.as_uri(),
                        sha256="ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")])
            tool = tools.Curl("unused")
            work = root / "work"
            with mock.patch.dict(lab.os.environ, {"CURL_HOME": str(foreign)}):
                tool.prepare(case, work)
                result = subprocess.run(tool.command(case, work), cwd=work, env=tool.env(work),
                                        capture_output=True, text=True, check=True)
            self.assertNotIn("foreign-config-read", result.stdout)
            self.assertEqual((work / "project" / "data" / "input.bin").read_bytes(), b"abc")

    def test_native_config_disable_flags_are_in_recorded_commands(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            case = dict(id="s1", files=[])
            tools.Curl("unused").command(case, root)
            self.assertIn("curl -q --parallel", (root / "command.sh").read_text())
            tools.Wget("unused").command(case, root)
            self.assertIn("wget --no-config", (root / "command.sh").read_text())



class FlushTests(unittest.TestCase):
    def test_each_filesystem_is_flushed_once_and_no_other(self):
        devices = {"/lab": 1, "/lab/www": 1, "/lab/fs/ext4": 2, "/lab/fs/xfs": 3}
        flushed = []
        with mock.patch.object(lab.os, "stat", side_effect=lambda path: mock.Mock(st_dev=devices[str(path)])), \
             mock.patch.object(lab.os, "open", side_effect=lambda path, flags: devices[str(path)] * 10), \
             mock.patch.object(lab.os, "close"), \
             mock.patch.object(lab, "_syncfs", side_effect=flushed.append), \
             mock.patch.object(lab.os, "sync", side_effect=AssertionError("a global sync waits on every mount")):
            lab.flush("/lab", "/lab/www", "/lab/fs/ext4", "/lab/fs/xfs", "/lab/fs/ext4")
        self.assertEqual(flushed, [10, 20, 30])

    def test_dropping_caches_flushes_only_the_lab(self):
        with tempfile.TemporaryDirectory() as root:
            drop = Path(root) / "drop_caches"
            with mock.patch.object(lab, "ROOT", Path(root)), \
                 mock.patch.object(lab, "DROP", drop), \
                 mock.patch.object(lab, "require_root"), \
                 mock.patch.object(lab, "flush") as flush, \
                 mock.patch.object(lab.os, "sync", side_effect=AssertionError("a global sync waits on every mount")):
                lab.drop_caches([])
            flush.assert_called_once_with(Path(root))
            self.assertEqual(drop.read_text(), "3\n")


class DurableCurlTests(unittest.TestCase):
    def test_durable_curl_flushes_what_it_wrote_after_checking_it(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            source = root / "source.bin"
            source.write_bytes(b"abc")
            case = dict(id="s1", members=[], files=[dict(name="input.bin", url=source.as_uri(),
                        sha256="ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")])
            tool = tools.DurableCurl("unused")
            self.assertEqual(tool.name, "curl+sync")
            self.assertEqual(tool.prepared_as, "curl")
            work = root / "work"
            tool.prepare(case, work)
            subprocess.run(tool.command(case, work), cwd=work, env=tool.env(work), check=True)
            lines = (work / "command.sh").read_text().splitlines()
            self.assertEqual(lines[-1], "sync -f data")
            self.assertTrue(lines[-2].startswith("sha256sum -c"), lines)
            self.assertEqual((work / "project" / "data" / "input.bin").read_bytes(), b"abc")
            self.assertTrue(tool.version().startswith("curl "), tool.version())

    def test_durable_curl_is_one_of_the_compared_tools(self):
        self.assertIn("curl+sync", [cls.name for cls in tools.CLASSES])


if __name__ == "__main__":
    unittest.main()
