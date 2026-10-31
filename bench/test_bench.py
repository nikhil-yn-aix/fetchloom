import unittest

import fixtures
import lab


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


if __name__ == "__main__":
    unittest.main()
