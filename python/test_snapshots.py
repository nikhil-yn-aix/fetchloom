"""snapshot coverage is the union of native and capability-specific runs."""

import unittest

from check_snapshots import require_referenced


class SnapshotTests(unittest.TestCase):
    def test_a_reflink_only_snapshot_is_kept_when_another_run_references_it(self):
        require_referenced({"ordinary.snap", "reflink.snap"}, [{"ordinary.snap"}, {"reflink.snap"}])

    def test_unreferenced_unknown_and_missing_reports_are_rejected(self):
        for expected, reports in [({"unused.snap"}, [set()]), ({"known.snap"}, [{"other.snap"}]), ({"known.snap"}, []), (set(), [set()])]:
            with self.subTest(expected=expected, reports=reports), self.assertRaises(ValueError):
                require_referenced(expected, reports)
