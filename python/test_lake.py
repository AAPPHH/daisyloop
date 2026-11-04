import json
import os
import sys
import unittest
import duckdb
sys.path.insert(0, os.path.dirname(__file__))
from lake import CONFIG, Lake

FIXTURE = os.environ.get("DAISYLOOP_FIXTURE") or os.path.join(os.path.dirname(__file__), "..", "target", "test-tmp", "lake-fixture")


class LakeRegression(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(os.path.join(FIXTURE, "expected.json")) as f:
            cls.expected = json.load(f)
        cls.lake = Lake({**CONFIG, "data_dir": os.path.abspath(FIXTURE)})

    def count(self, where, args=None):
        return self.lake.execute(f"select count(*) from telemetry where {where}", args).fetchall()[0][0]

    def test_engine_is_duckdb_2(self):
        self.assertGreaterEqual(int(duckdb.__version__.split(".")[0]), 2)

    def test_total_counts_each_row_once(self):
        self.assertEqual(self.count("true"), self.expected["total"])
        self.assertEqual(self.lake.execute("select count(*) from d5_raw").fetchall()[0][0], self.expected["raw"])

    def test_eq_matches_the_non_pushed_count(self):
        ident = self.expected["long0_id"]
        scanned = sum(1 for (c,) in self.lake.execute("select channel from telemetry").fetchall() if c == ident)
        self.assertEqual(self.count("channel = ?", [ident]), scanned)
        self.assertEqual(self.count("channel in (?, ?)", [ident, "none"]), scanned)
        self.assertEqual(scanned, self.expected["long0"])

    def test_hot_rows_are_visible(self):
        self.assertEqual(self.count("channel = 'hot/only'"), self.expected["hot_only"])

    def test_refresh_is_stable(self):
        before = self.count("true")
        self.lake.refresh()
        self.assertEqual(self.count("true"), before)


if __name__ == "__main__":
    unittest.main()
