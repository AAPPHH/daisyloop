import glob
import json
import os
import shutil
import sqlite3
import sys
import tempfile
import unittest
import duckdb
sys.path.insert(0, os.path.dirname(__file__))
from lake import CONFIG, Lake, LakeError

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


class LakeResilience(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.dir = os.path.join(self.tmp, "lake")
        shutil.copytree(FIXTURE, self.dir)
        with open(os.path.join(self.dir, "expected.json")) as f:
            self.expected = json.load(f)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def lake(self, **extra):
        return Lake({**CONFIG, "data_dir": self.dir, "retry_wait": 0.01, "lock_timeout": 0.2, **extra})

    def total(self, lake):
        return lake.execute("select count(*) from telemetry").fetchone()[0]

    def hot(self, kind=".arrows"):
        return sorted(glob.glob(os.path.join(self.dir, "hot", "telemetry-*" + kind)))

    def test_locked_catalog_raises_instead_of_an_empty_view(self):
        holder = sqlite3.connect(os.path.join(self.dir, "catalog.db"), isolation_level=None)
        holder.execute("begin exclusive")
        try:
            with self.assertRaises(sqlite3.OperationalError):
                self.lake()
        finally:
            holder.execute("rollback")
            holder.close()

    def test_missing_catalog_or_table_raises(self):
        with self.assertRaises(LakeError):
            Lake({**CONFIG, "data_dir": os.path.join(self.tmp, "nowhere")})
        with self.assertRaises(LakeError):
            Lake({**CONFIG, "data_dir": self.dir, "namespace": "other"})

    def test_rename_between_glob_and_read_is_retried(self):
        lake = self.lake()
        before = self.total(lake)
        real = Lake.read_stream.__func__
        calls = []

        def racing(cls, paths):
            if not calls:
                calls.append(paths)
                raise FileNotFoundError(paths[0])
            return real(cls, paths)

        Lake.read_stream = classmethod(racing)
        try:
            self.assertEqual(self.total(lake.refresh()), before)
        finally:
            Lake.read_stream = classmethod(real)
        self.assertTrue(calls)

    def test_unsynced_tail_is_not_shown_and_damage_keeps_the_verified_prefix(self):
        lake = self.lake()
        before = self.total(lake)
        path = self.hot()[-1]
        with open(path, "ab") as f:
            f.write(bytes(37))
        self.assertEqual(self.total(lake.refresh()), before)
        with open(path, "rb") as f:
            data = bytearray(f.read())
        data[len(data) // 2] ^= 0x40
        with open(path, "wb") as f:
            f.write(data)
        after = self.total(lake.refresh())
        self.assertLess(after, before)

    def test_damaged_sidecar_is_a_flaw_but_a_crash_tail_is_not(self):
        data = bytes(range(256)) * 2048
        self.assertIsNotNone(Lake.verified(data, b"")[1])
        self.assertIsNotNone(Lake.verified(data, bytes(64))[1])
        self.assertIsNotNone(Lake.verified(data, bytes(range(64)))[1])
        self.assertIsNone(Lake.verified(data[:768], b"")[1])
        self.assertIsNotNone(Lake.verified(data[:1920], b"")[1])

    def test_refresh_after_fetchone_and_time_range(self):
        lake = self.lake()
        n = self.total(lake)
        lake.execute("select channel from telemetry").fetchone()
        self.assertEqual(self.total(lake.refresh()), n)
        lake.execute("select channel from telemetry").fetchone()
        self.assertLess(self.total(lake.refresh(t0=0, t1=1)), n)
        self.assertEqual(self.total(lake.refresh()), n)


if __name__ == "__main__":
    unittest.main()
