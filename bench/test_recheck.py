import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import recheck


class FixtureTests(unittest.TestCase):
    def test_the_shared_files_depend_only_on_scale_and_seed(self):
        made = []
        for _ in range(2):
            with tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                recheck.sources(root, 0.0001, 7, "http://127.0.0.1:9")
                made.append({str(path.relative_to(root)): path.read_bytes() for path in sorted(root.rglob("*"))
                             if path.is_file() and path.name != "sources.json"})
        self.assertEqual(made[0], made[1])
        self.assertEqual(len([name for name in made[0] if name.startswith("src/files/")]), 10)
        self.assertEqual(made[0]["src/links.txt"].decode().splitlines()[0], "http://127.0.0.1:9/links/l00000.bin")

    def test_the_drop_names_one_file_of_every_dataset(self):
        with tempfile.TemporaryDirectory() as folder:
            project = Path(folder)
            (project / "data.toml").write_text('[datasets.p00001]\nref = "local"\n\n[datasets.p00000]\nref = "local"\n')
            self.assertEqual(recheck.drop_paths(project), ["data/p00000/remove.bin", "data/p00001/remove.bin"])

    def test_every_arm_runs_a_fresh_copy_and_the_aa_arm_the_baseline(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "old").write_bytes(b"baseline")
            (root / "new").write_bytes(b"candidate")
            copies = recheck.arms(root / "old", root / "new", root)
            self.assertEqual(list(copies), list(recheck.ARMS))
            self.assertEqual([path.read_bytes() for path in copies.values()], [b"baseline", b"candidate", b"baseline"])
            self.assertTrue(all(path.parent == root / "bin" for path in copies.values()))
            self.assertTrue(all(path.stat().st_mode & 0o100 for path in copies.values()))


class MeasureTests(unittest.TestCase):
    def test_every_arm_runs_once_a_round_and_the_rows_feed_the_summary(self):
        walls = {"fetchloom": 1.0, "fetchloom+candidate": 1.1, "fetchloom+aa": 1.0}

        def measured(argv, cwd, env, logfile, timeout, *, cpus=None):
            return dict(wall_seconds=walls[argv[0]], user_seconds=.5, sys_seconds=.1, exit_code=0, timeout=False,
                        maxrss_bytes=(2 if argv[0] == "fetchloom+candidate" else 1) << 20, cpu_affinity=[0])

        orders = []
        for _ in range(2):
            with tempfile.TemporaryDirectory() as folder:
                out = Path(folder)
                specs = {arm: dict(argv=[arm], cwd=out, env={}) for arm in recheck.ARMS}
                with mock.patch.object(recheck.run, "measure", side_effect=measured):
                    recheck.measure("status", specs, 6, 1729, out, None, 5)
                rows = [json.loads(line) for line in (out / "runs.jsonl").read_text().splitlines()]
            rounds = [[row["tool"] for row in rows if row["round"] == number] for number in range(7)]
            self.assertTrue(all(sorted(order) == sorted(recheck.ARMS) for order in rounds))
            self.assertEqual([row["warmup"] for row in rows].count(True), 3)
            orders.append(rounds)
        self.assertEqual(*orders)
        self.assertGreater(len({tuple(order) for order in orders[0]}), 1)
        text = recheck.summary(rows)
        self.assertIn("| status | candidate | 6 | 1000.0 → 1100.0 ms | 1.100 [1.100, 1.100] | 1.0 → 2.0 | 2.000", text)
        self.assertIn("| status | aa | 6 | 1000.0 → 1000.0 ms | 1.000 [1.000, 1.000] | 1.0 → 1.0 | 1.000", text)


if __name__ == "__main__":
    unittest.main()
