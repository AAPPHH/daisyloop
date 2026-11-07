import copy
import json
import numpy as np
import pyarrow.parquet as pq
from metaset import CONFIG, Evaluator, Hash, Metaset

NAMES = ["water", "fan_rpm", "pump_rpm", "p_total", "water.ewm5.d120", "p_gpu_minus_cpu", "p_total.ewm5.d30", "p_excess", "extra"]
T = 12000


def make(seed=0, n=T):
    ms = make_full(seed)
    if n < T:
        ms.F, ms.Y, ms.X, ms.t, ms.T = ms.F[:n], ms.Y[:n].copy(), ms.X[:n], ms.t[:n], n
        ms.Y[-120:] = np.nan
        ms.step_mask = ms.step_mask[:n]
    return ms


def make_full(seed=0):
    n = T
    rng = np.random.default_rng(seed)
    ms = Metaset(copy.deepcopy(CONFIG))
    ms.cfg["select"]["cap"] = 20.0
    ms.cfg["strata"]["min_groups"] = 3
    ms.k0, ms.T = 1_000_000, n
    ms.t = ms.k0 + np.arange(n, dtype=np.int64)
    seg = (np.arange(n) // 2000) % 3
    water = 31.0 + 3.0 * seg + np.cumsum(rng.normal(0, 0.002, n))
    fan = np.where(seg == 0, 1460.0, np.where(seg == 1, 700.0, 1000.0)) + (np.arange(n) % 700 < 20) * 300.0
    pump = np.where(seg == 2, 2570.0, 4820.0) + 0.0 * water
    p = np.where(seg == 1, 250.0, 60.0) + rng.normal(0, 1.0, n) + (np.arange(n) % 900 < 30) * 80.0
    F = np.column_stack([water, fan, pump, p, np.gradient(water) * 120, rng.normal(0, 40, n), np.gradient(p) * 30, p - 25 * (water - 28.0), rng.normal(size=n)]).astype(np.float32)
    ms.F, ms.names = F, NAMES
    ms.col = {k: i for i, k in enumerate(NAMES)}
    ms.yn = [f"{k}_{h}" for k in ("water", "cpu", "gpu") for h in (30, 60, 90, 120)]
    ms.Y = np.column_stack([np.r_[water[h:] - water[:-h], np.full(h, np.nan)] if k == "water" else rng.normal(0, 1, n) for k in ("water", "cpu", "gpu") for h in (30, 60, 90, 120)]).astype(np.float32)
    ms.X = np.full((n, 1), np.nan, np.float32)
    ms.X[5000:5300, 0] = 1.0
    ms.ix = {ms.cfg["phase_channel"]: 0}
    ms.step_mask = ms.steps()
    return ms


def test_hash_uniform_and_deterministic():
    u = Hash.uniform(np.arange(100000, dtype=np.int64), 0)
    assert u.min() >= 0 and u.max() < 1 and abs(u.mean() - 0.5) < 0.01
    assert (u == Hash.uniform(np.arange(100000, dtype=np.int64), 0)).all()
    assert (u != Hash.uniform(np.arange(100000, dtype=np.int64), 1)).any()


def test_selection_reduces_and_weights_reproduce_count():
    ms = make()
    sel = ms.build()
    elig = int(ms.eligible().sum())
    assert 0 < len(sel.idx) < 0.5 * elig
    assert abs(float(sel.weight.sum(dtype=np.float64)) / elig - 1.0) < 0.1
    assert (sel.weight >= 1.0 - 1e-6).all()


def test_every_cell_present_and_step_rows_kept_fully():
    ms = make()
    sel = ms.build()
    rows = np.flatnonzero(ms.eligible())
    full_cells = set(ms.bins(rows)[1].tolist())
    assert full_cells <= set(sel.cell.tolist())
    steps = np.flatnonzero(ms.step_mask & ms.eligible())
    assert set(steps.tolist()) <= set(sel.idx.tolist())
    assert (sel.weight[sel.step] == 1.0).all()


def test_deterministic_and_incremental_nested():
    a, b = make(), make()
    assert (a.build().idx == b.build().idx).all()
    small = make(n=T // 2)
    big = make()
    early = small.build()
    late = big.build()
    cut = small.t[-1] - 4 * big.cfg["block_s"]
    old = set(early.idx[small.t[early.idx] < cut].tolist())
    new = set(late.idx.tolist())
    assert len(old) > 0
    assert (new & set(np.flatnonzero(big.t < cut).tolist())) <= old


def test_budget_raises_selectivity(tmp_path):
    ms = make()
    full = ms.build()
    tight = ms.build(rows_max=int(len(full.idx) * 0.8))
    assert tight.level < 1.0 and len(tight.idx) < len(full.idx)
    assert tight.counts["expected"] <= len(full.idx) * 0.8 + 1


def test_budget_level_stores_ks_and_warns(tmp_path, caplog):
    ms = make()
    ms.cfg["parquet"] = dict(ms.cfg["parquet"], probe_rows=500, row_group=2000)
    path = str(tmp_path / "m.parquet")
    ms.cfg["out_path"] = path
    assert "ks" not in ms.build().counts
    ms.cfg["budget_bytes"] = 150_000
    ms.cfg["eval"]["ks_max"] = 0.0
    with caplog.at_level("WARNING"):
        sel, info = ms.run(path=path)
    assert sel.level < 1.0
    meta = json.loads(pq.ParquetFile(path).schema_arrow.metadata[b"metaset"])
    assert meta["level"] == sel.level and set(meta["counts"]["ks"]) == set(ms.cfg["eval"]["ks"])
    assert all(0.0 <= v <= 1.0 for v in meta["counts"]["ks"].values())
    assert len([r for r in caplog.records if "weighted KS" in r.getMessage()]) == 1


def test_run_fits_byte_budget_and_zstd(tmp_path):
    ms = make()
    ms.cfg["parquet"] = dict(ms.cfg["parquet"], probe_rows=500, row_group=2000)
    path = str(tmp_path / "m.parquet")
    ms.cfg["out_path"] = path
    ms.cfg["budget_bytes"] = 400_000
    sel, info = ms.run(path=path)
    assert info["size"] <= 400_000
    f = pq.ParquetFile(path)
    assert f.metadata.row_group(0).column(0).compression == "ZSTD"
    meta = json.loads(f.schema_arrow.metadata[b"metaset"])
    assert meta["counts"]["kept"] == f.metadata.num_rows and meta["level"] <= 1.0
    assert {"t", "group", "cell", "stratum", "weight", "y_water_120", "f_water"} <= set(f.schema_arrow.names)


def test_strata_merge_rare_cells():
    ms = make()
    ms.cfg["strata"]["min_groups"] = 6
    sel = ms.build()
    g = ms.t[sel.idx] // ms.cfg["block_s"]
    short = [s for s in np.unique(sel.stratum) if len(np.unique(g[sel.stratum == s])) < 6]
    assert len(short) <= 1
    assert len(np.unique(sel.stratum)) < len(np.unique(sel.cell))


def test_ks_distance():
    x = np.random.default_rng(0).normal(size=5000)
    assert Evaluator.ks(x, np.ones(5000), x) < 1e-9
    assert Evaluator.ks(x + 3.0, np.ones(5000), x) > 0.85
    d = np.array([0.0, 1.0] * 500)
    assert Evaluator.ks(d, np.ones(1000), d) < 1e-9
    assert Evaluator.ks(np.array([0.0, 1.0]), np.array([3.0, 1.0]), d) > 0.2
