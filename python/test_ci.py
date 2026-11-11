"""selection checks reject missing tests rather than trusting a successful process."""

import unittest

from ci_tests import require_same, selected


class SelectionTests(unittest.TestCase):
    def test_only_matching_nonignored_cases_count(self):
        def case(ignored=False, status="matches"):
            return {"ignored": ignored, "filter-match": {"status": status}}
        document = {"rust-suites": {"cli": {"testcases": {
            "kept": case(), "live": case(ignored=True), "filtered": case(status="mismatch")}}}}
        self.assertEqual(selected(document), {"cli::kept"})

    def test_empty_missing_and_extra_selections_fail(self):
        for expected, actual in [(set(), set()), ({"one"}, set()), ({"one"}, {"one", "two"})]:
            with self.subTest(expected=expected, actual=actual), self.assertRaises(RuntimeError):
                require_same(expected, actual, "filesystem")
        require_same({"one"}, {"one"}, "filesystem")
