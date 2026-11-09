import datetime as dt
import math
import numpy as np
from features import BatchEngine, FeatureFrame, Spec, StreamEngine, CONFIG
from rawstore import RawStore, CONFIG as RCFG
import os
import pytest

pytestmark = pytest.mark.skipif(not os.path.exists(os.path.join(RCFG["cache_dir"], "meta.json")), reason="needs the local raw store cache")

SPEC = Spec.load(CONFIG["spec_path"])


def data():
    meta, X = RawStore().load()
    return meta, np.asarray(X), {c["channel"]: i for i, c in enumerate(meta["channels"])}


def ts(meta, s):
    return int(dt.datetime.strptime(s, "%Y-%m-%d %H:%M").astimezone().timestamp()) - meta["k0"]


def close(a, b, tol=1e-9):
    a, b = np.asarray(a, dtype=np.float64), np.asarray(b, dtype=np.float64)
    both_nan = np.isnan(a) & np.isnan(b)
    ok = both_nan | (np.abs(a - b) <= tol * (1 + np.abs(b)))
    return bool(ok.all())


def test_stream_equals_batch():
    meta, X, ix = data()
    a, b = ts(meta, "2026-11-04 07:00"), ts(meta, "2026-11-04 09:30")
    sub = X[a:b]
    k0 = meta["k0"] + a
    batch = BatchEngine(SPEC).run(sub, ix, k0)
    eng = StreamEngine(SPEC)
    names = SPEC.outputs()
    cols = {c: i for c, i in ix.items()}
    out = {n: [] for n in names}
    for t in range(sub.shape[0]):
        row = {c: float(sub[t, i]) for c, i in cols.items() if not math.isnan(sub[t, i])}
        v = eng.step(row, k0 + t)
        for n in names:
            out[n].append(v[n])
    bad = [n for n in names if not close(out[n], batch[n])]
    assert not bad, bad[:10]


def test_causality():
    meta, X, ix = data()
    X = X[175000:]
    k0 = meta["k0"] + 175000
    ref = BatchEngine(SPEC).run(X, ix, k0)
    rng = np.random.default_rng(0)
    names = SPEC.outputs()
    for cut in rng.integers(3000, X.shape[0] - 100, 4):
        Y = X.copy()
        Y[cut + 1:] = rng.normal(50, 30, Y[cut + 1:].shape).astype(np.float32)
        out = BatchEngine(SPEC).run(Y, ix, k0)
        bad = [n for n in names if not close(out[n][: cut + 1], ref[n][: cut + 1], 1e-9)]
        assert not bad, (cut, bad[:10])


def test_gap_semantics():
    meta, X, ix = data()
    ff = FeatureFrame()
    names, F, v = ff.compute(SPEC, X, ix, meta["k0"])
    i = names.index("mask_lhm")
    a, b = ts(meta, "2026-11-03 09:00"), ts(meta, "2026-11-03 12:00")
    assert F[a:b, i].max() == 0.0
    j = names.index("p_total")
    assert np.isnan(F[a:b, j]).all()
    w = names.index("water.ewm600")
    assert np.isfinite(F[a + 1000:b, w]).all()


if __name__ == "__main__":
    for f in (test_stream_equals_batch, test_causality, test_gap_semantics):
        f()
        print("ok", f.__name__)
