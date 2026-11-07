import copy
import os
import shutil
import tempfile
import numpy as np
import pytest
import metastore
from lake import CONFIG as LCFG, RangeLake, LakeError
from metastore import CONFIG, BlockScore, HistoryStore, StoreError, Thinner

K0 = 1_700_000_000
T = 14400
NAMES = ["d5next/water/temp", "d5next/fan/rpm", "d5next/pump/rpm", "/intelcpu/0/power/0", "nvml/0/power", "daisyloop/step/phase", "x/noise", "x/flat", "x/int"]
KINDS = ["temperature", "rpm", "rpm", "power", "power", "other", "load", "other", "temperature"]


def cfg_for(tmp, **tol):
    c = copy.deepcopy(CONFIG)
    c.update(dir=os.path.join(tmp, "store"), log=os.path.join(tmp, "u.log"), day_s=3600, block_s=120, chunk_s=600, lag_s=0, context_s=0)
    c["tol"]["sample_chunks"] = 2
    c["tol"]["preset"] = dict(zip(NAMES, [0.01, 1.0, 5.0, 0.5, 0.5, 0.1, 0.5, 0.1, 0.5]), **tol)
    c["thin"].update(hold_blocks=1, rare_blocks=2)
    c["cache_dir"] = os.path.join(tmp, "cache")
    c["d5"]["width"] = 20
    return c


def synth(seed=0):
    rng = np.random.default_rng(seed)
    t = np.arange(T)
    X = np.full((T, len(NAMES)), np.nan, np.float32)
    seg = (t // 1800) % 3
    X[:, 0] = 31.0 + 3.0 * seg + np.cumsum(rng.normal(0, 0.002, T))
    X[:, 1] = np.where(seg == 0, 1460.0, np.where(seg == 1, 700.0, 1000.0)) + rng.normal(0, 3, T)
    X[:, 2] = 4800.0
    X[:, 3] = np.where(seg == 1, 250.0, 60.0) + rng.normal(0, 1.0, T)
    X[:, 4] = 40.0 + 5 * np.sin(t / 700.0)
    X[6000:6300, 5] = 1.0
    X[:, 6] = np.clip(rng.normal(20, 4, T), 0, 100)
    X[:, 7] = 5.0
    X[:, 8] = np.round(50 + 5 * np.sin(t / 300.0))
    for c in range(len(NAMES)):
        X[rng.random(T) < 0.01, c] = np.nan
    X[3000:3100, 3] = np.nan
    X[9000:, 7] = np.nan
    return X


class FakeReader:
    def __init__(self, X, floor=None):
        self.X, self.avail, self.floor = X, T, floor
        self.reads = []
        self.with_d5 = False

    def read(self, a, b):
        if self.floor is not None and a < self.floor():
            raise AssertionError(f"read of {a} older than allowed {self.floor()}")
        self.reads.append((a, b))
        i0, i1 = a - K0, min(b, K0 + self.avail) - K0
        sub = self.X[i0:i1]
        r, c = np.nonzero(np.isfinite(sub))
        rows = {"k": (r + i0 + K0).astype(np.int64), "channel": np.array(NAMES, dtype=object)[c], "v": sub[r, c].astype(np.float64)}
        return rows, {n: ("src", n, k, "u", "state") for n, k in zip(NAMES, KINDS)}

    def read_d5(self, a, b, width):
        if not self.with_d5:
            return np.zeros(0, np.int64), np.zeros((0, width), np.uint8), 0
        if not hasattr(self, "D5"):
            rng = np.random.default_rng(5)
            m = np.cumsum(rng.integers(-1, 2, (T, width)), axis=0).astype(np.uint8)
            m[:, 3] = (np.arange(T) * 10).astype(np.uint8)
            m[500:900] = m[500]
            self.D5 = m
        i0, i1 = a - K0, min(b, K0 + self.avail) - K0
        t = (np.arange(i0, i1) + K0) * 1_000_000 + (np.arange(i0, i1) * 37) % 900_000
        return t.astype(np.int64), self.D5[i0:i1], 0

    def latest(self, a=None):
        return K0 + self.avail - 1

    def earliest(self):
        return K0


def build(tmp, X, **kw):
    c = cfg_for(tmp, **kw)
    st = HistoryStore(c)
    st.update(FakeReader(X))
    return st


@pytest.fixture()
def tmp():
    d = tempfile.mkdtemp()
    yield d
    shutil.rmtree(d, ignore_errors=True)


def slack(X):
    return 2.0 ** -22 * np.nanmax(np.abs(X), axis=0)


def test_reconstruction_within_tolerance_and_gaps_exact(tmp):
    X = synth()
    st = build(tmp, X)
    meta, R = st.load()
    assert meta["k0"] == K0 and meta["k1"] == K0 + T - 1 and R.shape == X.shape
    order = [NAMES.index(c["channel"]) for c in meta["channels"]]
    tol = np.array(meta["tol"])
    for j, i in enumerate(order):
        fin = np.isfinite(X[:, i])
        assert (np.isfinite(R[:, j]) == fin)[np.flatnonzero(fin)[0]: np.flatnonzero(fin)[-1] + 1].all()
        err = np.abs(R[fin, j].astype(np.float64) - X[fin, i])
        assert err.max() <= tol[j] + slack(X)[i], NAMES[i]
    assert st.size() < X.nbytes / 4


def test_metadata_matches_rawstore_layout(tmp):
    meta, _ = build(tmp, synth()).load()
    assert [c["channel"] for c in meta["channels"]] == sorted(NAMES)
    assert set(meta["channels"][0]) == {"channel", "source", "label", "kind", "unit", "role"}


def test_ten_daily_updates_equal_one_batch_and_read_only_newer(tmp):
    X = synth(1)
    _, R1 = build(os.path.join(tmp, "a"), X).load()
    c = cfg_for(os.path.join(tmp, "b"))
    rd = FakeReader(X)
    for n in range(1, 11):
        rd.avail = T * n // 10
        st = HistoryStore(c)
        rd.floor = lambda st=st: (st.watermark + 1 - 3600) if st.watermark is not None else K0
        st.update(rd)
    meta, R2 = HistoryStore(c).load()
    assert (np.isfinite(R1) == np.isfinite(R2)).all()
    fin = np.isfinite(R1)
    assert np.abs(R1[fin].astype(np.float64) - R2[fin]).max() <= max(meta["tol"]) * 1e-6 + 1e-9
    assert min(a for a, _ in rd.reads) == K0 and len(rd.reads) >= 10
    assert all(b - a <= 600 for a, b in rd.reads)


def test_old_access_is_detected(tmp):
    X = synth()
    st = build(tmp, X)
    rd = FakeReader(X, floor=lambda: st.watermark + 1)
    rd.avail = T
    assert st.update(rd)["new_s"] == 0
    with pytest.raises(AssertionError):
        rd.read(K0, K0 + 10)


def test_update_with_no_new_rows_is_a_noop_and_resume_after_reload(tmp):
    X = synth()
    st = build(tmp, X)
    size, wm = st.size(), st.watermark
    again = HistoryStore(st.cfg)
    assert again.update(FakeReader(X))["new_s"] == 0
    assert again.size() == size and again.watermark == wm


def test_new_channel_appears_later(tmp):
    X = synth()
    X[:7000, 6] = np.nan
    c = cfg_for(tmp)
    st = HistoryStore(c)
    rd = FakeReader(X)
    rd.avail = 5000
    rd.read = (lambda f: lambda a, b: (lambda rb: ({k: v[rb[0]["channel"] != NAMES[6]] for k, v in rb[0].items()}, {k: v for k, v in rb[1].items() if k != NAMES[6]}))(f(a, b)))(rd.read)
    st.update(rd)
    assert len(st.chan) == len(NAMES) - 1


def test_thinning_fits_budget_keeps_step_rows_and_bounds_error(tmp):
    X = synth()
    st = build(tmp, X)
    full = st.size()
    meta0, R0 = st.load()
    th = Thinner(st, budget=int(full * 0.7))
    out = th.enforce()
    assert out["thinned_blocks"] > 0 and st.size() <= th.budget
    meta, R = HistoryStore(st.cfg).load()
    tol = np.array(meta["tol"])
    B = st.cfg["block_s"]
    sm = BlockScore(st).compute()
    t = np.arange(T) + K0
    mult = np.array([th.mult.get(int(x) // B, 1.0) for x in t])
    for j, c in enumerate(meta["channels"]):
        i = NAMES.index(c["channel"])
        fin = np.isfinite(X[:, i])
        assert (np.isfinite(R[:, j]) == np.isfinite(R0[:, j])).all()
        err = np.abs(R[fin, j].astype(np.float64) - X[fin, i])
        assert (err <= mult[fin] * tol[j] + slack(X)[i]).all(), c["channel"]
    step_blocks = np.flatnonzero(sm["step"]) + sm["b0"]
    assert len(step_blocks) and all(th.mult.get(int(b), 1.0) == 1.0 for b in step_blocks)
    assert max(th.mult.values()) > 1.0


def test_thinning_refuses_when_only_step_rows_are_left(tmp):
    X = synth()
    st = build(tmp, X)
    with pytest.raises(StoreError):
        Thinner(st, budget=1000).enforce()


def test_update_after_thinning_stays_within_tolerance(tmp):
    X = synth(2)
    c = cfg_for(tmp)
    st = HistoryStore(c)
    rd = FakeReader(X)
    rd.avail = T // 2
    st.update(rd)
    Thinner(st, budget=int(st.size() * 0.8)).enforce()
    rd.avail = T
    st = HistoryStore(c)
    st.update(rd)
    meta, R = st.load()
    mult = np.array([Thinner(st).mult.get(int(x) // c["block_s"], 1.0) for x in np.arange(T) + K0])
    tol = np.array(meta["tol"])
    for j, ch in enumerate(meta["channels"]):
        i = NAMES.index(ch["channel"])
        fin = np.isfinite(X[:, i])
        assert (np.abs(R[fin, j].astype(np.float64) - X[fin, i]) <= mult[fin] * tol[j] + slack(X)[i]).all()


def protected_share(th):
    bs = BlockScore(th.st).compute()
    prot = th.protected(bs)
    return prot[~bs["step"]].mean(), bs, prot


def test_d5_stored_exactly_with_duplicate_runs_collapsed_and_incremental(tmp):
    X = synth(3)
    c = cfg_for(tmp)
    rd = FakeReader(X)
    rd.with_d5 = True
    for n in (4, 10):
        rd.avail = T * n // 10
        st = HistoryStore(c)
        info = st.update(rd)
    t, M = st.reports()
    i = np.arange(len(t))
    assert len(t) == T * 10 // 10 and (np.diff(t) > 0).all() and info["bytes_d5"] > 0 and info["bytes_telemetry"] > 0
    assert (M == rd.D5[: len(t)]).all() and (t == (i + K0) * 1_000_000 + (i * 37) % 900_000).all()
    day = st.d5_day(int(t[0] // 1_000_000 // c["day_s"]))
    raw = open(st.files("d5")[int(t[0] // 1_000_000 // c["day_s"])], "rb").read()
    assert int.from_bytes(raw[8:16], "little") < int.from_bytes(raw[:8], "little")
    assert info["bytes"] == info["bytes_telemetry"] + info["bytes_d5"]
    t2, M2 = st.reports(K0 + 100, K0 + 199)
    assert len(t2) == 100 and (M2 == rd.D5[100:200]).all()


def test_load_is_a_memmap_cached_until_the_store_changes(tmp):
    X = synth(4)
    st = build(tmp, X)
    meta, R = st.load()
    assert isinstance(R, np.memmap) and R.mode == "c"
    cache = os.path.join(tmp, "cache")
    names = sorted(os.listdir(cache))
    mt = {n: os.stat(os.path.join(cache, n)).st_mtime_ns for n in names}
    meta2, R2 = HistoryStore(st.cfg).load()
    assert sorted(os.listdir(cache)) == names and mt == {n: os.stat(os.path.join(cache, n)).st_mtime_ns for n in names}
    assert (np.isfinite(R) == np.isfinite(R2)).all()
    sub, S = st.load(channels=[NAMES[0]], k0=K0 + 5000, k1=K0 + 5999)
    assert S.shape == (1000, 1) and np.array_equal(S[:, 0], R[5000:6000, meta["channels"].index(next(c for c in meta["channels"] if c["channel"] == NAMES[0]))], equal_nan=True)
    Thinner(st, budget=int(st.size() * 0.8)).enforce()
    del R, R2
    _, R3 = HistoryStore(st.cfg).load()
    assert sorted(os.listdir(cache)) != names


def test_stage_one_protects_about_a_fifth_of_non_step_blocks(tmp):
    st = build(tmp, synth())
    share, bs, prot = protected_share(Thinner(st))
    assert 0.17 <= share <= 0.23 and not (prot & bs["step"]).any()


def test_ladder_continues_oldest_first_beyond_max_mult_and_records_bound(tmp):
    X = synth()
    st = build(tmp, X)
    th = Thinner(st, budget=int(st.size() * 0.45))
    th.cfg.update(max_mult=2.0, ladder_max=64.0)
    out = th.enforce()
    assert out["ladder"] and st.size() <= th.budget
    assert 2.0 < max(th.mult.values()) <= 64.0
    ids = sorted(b for b, m in th.mult.items() if m > 2.0)
    low = sorted(b for b, m in th.mult.items() if m <= 2.0)
    assert ids and (not low or ids[0] <= low[len(low) // 2])
    meta, R = HistoryStore(st.cfg).load()
    tol = np.array(meta["tol"])
    mult = np.array([th.mult.get(int(x) // st.cfg["block_s"], 1.0) for x in np.arange(T) + K0])
    for j, ch in enumerate(meta["channels"]):
        i = NAMES.index(ch["channel"])
        fin = np.isfinite(X[:, i])
        assert (np.abs(R[fin, j].astype(np.float64) - X[fin, i]) <= mult[fin] * tol[j] + slack(X)[i]).all(), ch["channel"]
    sm = BlockScore(st).compute()
    assert all(th.mult.get(int(b), 1.0) == 1.0 for b in np.flatnonzero(sm["step"]) + sm["b0"])
    with pytest.raises(StoreError):
        Thinner(st, budget=1000).enforce()


def test_thinning_overshoot_stays_small(tmp):
    st = build(tmp, synth())
    th = Thinner(st, budget=int(st.size() * 0.8))
    out = th.enforce()
    assert 0.0 <= (th.budget * st.cfg["safety"] - out["size_after"]) / th.budget < 0.05


def test_range_lake_retries_a_file_that_disappears_and_gives_up_cleanly(tmp):
    lake = RangeLake(dict(LCFG, data_dir=tmp, retry_wait=0.001, retries=3))
    lake.location = lambda t: (_ for _ in ()).throw(IndexError())
    calls = []
    real = lake.hot_files

    def racing(table, t0, t1):
        calls.append(1)
        if len(calls) == 1:
            raise FileNotFoundError("gone")
        return real(table, t0, t1)

    lake.hot_files = racing
    lake.refresh(0, 10)
    assert len(calls) == 2 and lake.con.execute("select count(*) from telemetry").fetchone()[0] == 0
    lake.hot_files = lambda *a: (_ for _ in ()).throw(FileNotFoundError("gone"))
    with pytest.raises(LakeError):
        lake.refresh(0, 10)


def test_failures_before_main_are_logged(tmp):
    c = cfg_for(tmp)
    c["data_dir"] = os.path.join(tmp, "nolake")
    with pytest.raises(Exception):
        metastore.main(["bogus"], c)
    assert "bogus failed" in open(c["log"]).read()
