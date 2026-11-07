import json
import os
import sys
import time
import numpy as np
import pyarrow as pa
import pyarrow.parquet as pq
from features import FeatureFrame, PHYS, PhysSpec, Spec, CONFIG as FCFG
from models import LIN_CLIP, LinearFit, Prep
from rawstore import RawStore

OUT = os.path.join(os.environ.get("LOCALAPPDATA", "."), "daisyloop-ml", "metaset")
CONFIG = {
    "out_path": os.path.join(OUT, "metaset.parquet"),
    "report_path": os.path.join(OUT, "report.json"),
    "full_path": os.path.join(OUT, "full_table.parquet"),
    "budget_bytes": 2_000_000_000,
    "block_s": 900,
    "horizons": [30, 60, 90, 120],
    "series": {"water": "d5next/water/temp", "cpu": "/intelcpu/0/temperature/0", "gpu": "nvml/0/temp"},
    "phys": dict(PHYS, pint=True),
    "phase_channel": "daisyloop/step/phase",
    "step": {"pre_s": 300, "post_s": 900},
    "require": ["water_30", "water_120"],
    "cells": [
        ("water", "water", [30.0, 32.0, 34.0, 36.0, 38.0]),
        ("fan", "fan_rpm", [100.0, 650.0, 1000.0, 1400.0]),
        ("pump", "pump_rpm", [3500.0]),
        ("heat", "p_total", [50.0, 100.0, 200.0, 300.0]),
        ("slope", "water.ewm5.d120", [-0.7, -0.15, 0.15, 0.7]),
        ("mix", "p_gpu_minus_cpu", [-30.0, 30.0]),
    ],
    "strata": {"min_groups": 30, "collapse": ["mix", "slope", "heat", "pump", "fan", "water"]},
    "dedup": {"water": 0.1, "fan_rpm": 50.0, "pump_rpm": 200.0, "p_total": 10.0, "water.ewm5.d120": 0.05, "p_gpu_minus_cpu": 25.0},
    "score": {"lag_s": 30, "fan_rpm": 1460.0 * 0.05, "pump_rpm": 4800.0 * 0.05, "p_total": 40.0, "slope": 0.3, "resid": 0.3, "c_kj_per_k": 11.7, "h": 120},
    "select": {"cap": 100.0, "cap_min": 4.0, "boost_score": 3.0, "score_ref": 3.0, "boost_transient": 2.0, "size_exp": 0.5, "rep_quantile": 0.5, "seed": 0},
    "parquet": {"level": 22, "row_group": 50000, "probe_rows": 20000, "safety": 0.92, "shrink": 0.9, "tries": 6},
    "eval": {"holdout": 0.2, "alpha": 1000.0, "seeds": 5, "folds": 5, "min_groups": 5, "min_cell_rows": 30, "ks": ["water", "fan_rpm", "pump_rpm", "p_total"], "ks_max": 0.05, "cover": 0.95, "rmse_tol": 0.05, "targets": ["water_30", "water_120"]},
}
KEEP_STATS = ["t", "group", "cell", "stratum"]


class Hash:
    @staticmethod
    def uniform(t, seed):
        g = np.uint64(0x9E3779B97F4A7C15)
        with np.errstate(over="ignore"):
            z = t.astype(np.uint64) + g * np.uint64(seed + 1)
            z = (z ^ (z >> np.uint64(30))) * np.uint64(0xBF58476D1CE4E5B9)
            z = (z ^ (z >> np.uint64(27))) * np.uint64(0x94D049BB133111EB)
            z = z ^ (z >> np.uint64(31))
        return (z >> np.uint64(11)).astype(np.float64) / float(1 << 53)


class Selection:
    def __init__(self, idx, weight, n_dup, score, transient, step, cell, stratum, cells, level, cap_eff, counts):
        self.idx, self.weight, self.n_dup, self.score, self.transient, self.step = idx, weight, n_dup, score, transient, step
        self.cell, self.stratum, self.cells, self.level, self.cap_eff, self.counts = cell, stratum, cells, level, cap_eff, counts


class Metaset:
    def __init__(self, cfg=CONFIG, store=None):
        self.cfg = cfg
        self.store = store

    def load(self, meta=None, X=None):
        cfg = self.cfg
        if meta is None:
            meta, X = (self.store or RawStore()).load()
        self.meta, self.X = meta, X
        self.k0, self.T = meta["k0"], X.shape[0]
        self.ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
        ps = PhysSpec(Spec.load(FCFG["spec_path"]), cfg["phys"])
        self.ps = ps
        self.names, self.F, _ = FeatureFrame().compute(ps.build(), X, self.ix, self.k0)
        self.col = {n: i for i, n in enumerate(self.names)}
        self.t = self.k0 + np.arange(self.T, dtype=np.int64)
        self.Y, self.yn = self.targets()
        self.step_mask = self.steps()
        return self

    def feat(self, n):
        return np.asarray(self.F[:, self.col[n]], dtype=np.float64)

    def targets(self):
        from features import Ops
        cfg, T = self.cfg, self.T
        cols, names = [], []
        for k, ch in cfg["series"].items():
            s = Ops.ffill(np.asarray(self.X[:, self.ix[ch]], dtype=np.float64), FCFG["ffill_s"], False) if ch in self.ix else np.full(T, np.nan)
            for h in cfg["horizons"]:
                y = np.full(T, np.nan)
                y[: T - h] = s[h:] - s[: T - h]
                cols.append(y.astype(np.float32))
                names.append(f"{k}_{h}")
        return np.column_stack(cols), names

    def steps(self):
        out = np.zeros(self.T, bool)
        ch = self.cfg["phase_channel"]
        if ch not in self.ix:
            return out
        fin = np.isfinite(np.asarray(self.X[:, self.ix[ch]], dtype=np.float64))
        w = self.cfg["step"]
        on = np.flatnonzero(fin)
        if not len(on):
            return out
        gaps = np.flatnonzero(np.diff(on) > 120)
        starts, ends = np.r_[on[0], on[gaps + 1]], np.r_[on[gaps], on[-1]]
        for a, b in zip(starts, ends):
            out[max(0, a - w["pre_s"]): min(self.T, b + w["post_s"])] = True
        return out

    def eligible(self, t_max=None):
        ok = np.isfinite(self.feat("water"))
        for n in self.cfg["require"]:
            ok &= np.isfinite(self.Y[:, self.yn.index(n)])
        if t_max is not None:
            ok &= self.t < t_max - max(self.cfg["horizons"])
        return ok

    def bins(self, rows):
        cols, radix = [], []
        for _, f, edges in self.cfg["cells"]:
            v = self.feat(f)[rows]
            b = np.where(np.isfinite(v), 1 + np.searchsorted(edges, np.nan_to_num(v), side="right"), 0)
            cols.append(b.astype(np.int8))
            radix.append(len(edges) + 2)
        B = np.column_stack(cols)
        code = np.zeros(len(rows), np.int64)
        for j, r in enumerate(radix):
            code = code * r + B[:, j]
        return B, code

    def strata(self, B, group):
        cfg = self.cfg
        names = [c[0] for c in cfg["cells"]]
        radix = np.array([len(c[2]) + 3 for c in cfg["cells"]], np.int64)
        C = B.astype(np.int64)

        def encode(M):
            code = np.zeros(len(M), np.int64)
            for j, r in enumerate(radix):
                code = code * r + M[:, j]
            return code
        for nm in cfg["strata"]["collapse"]:
            _, inv = np.unique(encode(C), return_inverse=True)
            pairs = np.unique(np.column_stack([inv, group]), axis=0)
            ng = np.bincount(pairs[:, 0], minlength=inv.max() + 1)
            C[ng[inv] < cfg["strata"]["min_groups"], names.index(nm)] = radix[names.index(nm)] - 1
        return encode(C)

    def components(self, rows):
        s = self.cfg["score"]
        lag = s["lag_s"]

        def change(n):
            v = self.feat(n)
            d = np.full(self.T, np.nan)
            d[lag:] = np.abs(v[lag:] - v[:-lag])
            return d
        comp = np.column_stack([
            change("fan_rpm") / s["fan_rpm"], change("pump_rpm") / s["pump_rpm"], np.abs(self.feat("p_total.ewm5.d30")) / s["p_total"],
            np.abs(self.feat("water.ewm5.d120")) / s["slope"],
            np.abs(self.Y[:, self.yn.index(f"water_{s['h']}")] - self.feat("p_excess") * s["h"] / (s["c_kj_per_k"] * 1000.0)) / s["resid"],
        ])
        return np.nan_to_num(comp[rows], nan=0.0, posinf=0.0, neginf=0.0)

    def dedup(self, rows, step):
        q = self.cfg["dedup"]
        keys = []
        for f, d in q.items():
            v = self.feat(f)[rows]
            keys.append(np.where(np.isfinite(v), np.floor(np.nan_to_num(v) / d), -1e9).astype(np.int64))
        keys.append((self.t[rows] // self.cfg["block_s"]).astype(np.int64))
        keys.append(np.where(step, self.t[rows], 0).astype(np.int64))
        K = np.column_stack(keys)
        return K

    def representatives(self, rows, score, step):
        K = self.dedup(rows, step)
        order = np.lexsort((self.t[rows], score) + tuple(K[:, j] for j in range(K.shape[1] - 1, -1, -1)))
        Ks = K[order]
        new = np.r_[True, (Ks[1:] != Ks[:-1]).any(1)]
        first = np.flatnonzero(new)
        n = np.diff(np.r_[first, len(order)])
        pos = first + np.floor((n - 1) * self.cfg["select"]["rep_quantile"]).astype(np.int64)
        return order[pos], n

    @staticmethod
    def inclusion(b, cap):
        if len(b) <= cap:
            return np.ones(len(b))
        lo, hi = 0.0, cap / b.min() + 1.0
        for _ in range(60):
            m = 0.5 * (lo + hi)
            if np.minimum(1.0, m * b).sum() > cap:
                hi = m
            else:
                lo = m
        return np.minimum(1.0, 0.5 * (lo + hi) * b)

    def probabilities(self, code, boost, fixed, cap):
        p = np.ones(len(code))
        free = np.flatnonzero(~fixed)
        order = free[np.argsort(code[free], kind="stable")]
        cs = code[order]
        cut = np.r_[0, np.flatnonzero(cs[1:] != cs[:-1]) + 1, len(order)]
        for a, b in zip(cut[:-1], cut[1:]):
            ids = order[a:b]
            p[ids] = self.inclusion(boost[ids], cap)
        return p

    def build(self, t_max=None, level=None, rows_max=None):
        cfg = self.cfg
        sc = cfg["select"]
        rows = np.flatnonzero(self.eligible(t_max))
        comp = self.components(rows)
        score = comp.sum(1)
        transient = (comp[:, :4] >= 1.0).any(1)
        step = self.step_mask[rows]
        rep, n_dup = self.representatives(rows, score, step)
        idx = rows[rep]
        sc_r, tr_r, st_r = score[rep], transient[rep], step[rep]
        B, code = self.bins(idx)
        boost = (1.0 + sc["boost_score"] * np.minimum(sc_r / sc["score_ref"], 1.0) + sc["boost_transient"] * tr_r) * n_dup.astype(np.float64) ** sc["size_exp"]
        u = Hash.uniform(self.t[idx], sc["seed"])
        n_fixed = int(st_r.sum())

        def draw(lv):
            cap = max(sc["cap_min"], sc["cap"] * lv)
            return cap, self.probabilities(code, boost, st_r, cap)
        if level is None:
            level = 1.0
            if rows_max is not None and draw(1.0)[1].sum() > rows_max:
                lo, hi = 0.0, 1.0
                for _ in range(30):
                    m = 0.5 * (lo + hi)
                    if draw(m)[1].sum() > rows_max:
                        hi = m
                    else:
                        lo = m
                level = lo
        cap, p = draw(level)
        keep = u < p
        sel = np.flatnonzero(keep)
        _, cnt = np.unique(self.bins(rows)[1], return_counts=True)
        counts = {"eligible": int(len(rows)), "reps": int(len(idx)), "kept": int(len(sel)), "fixed": n_fixed, "expected": float(p.sum()), "cells": int(len(cnt))}
        return Selection(idx[sel], (n_dup[sel] / p[sel]).astype(np.float32), n_dup[sel].astype(np.int32), sc_r[sel].astype(np.float32), tr_r[sel], st_r[sel], code[sel], self.strata(B[sel], self.t[idx[sel]] // cfg["block_s"]), B[sel], level, cap, counts)

    def assemble(self, sel=None):
        cfg = self.cfg
        if sel is None:
            idx = np.arange(self.T)
            n = self.T
            cols = {"t": self.t, "group": (self.t // cfg["block_s"]).astype(np.int32), "cell": np.zeros(n, np.int64), "stratum": np.zeros(n, np.int64), "weight": np.ones(n, np.float32)}
        else:
            idx = sel.idx
            cols = {"t": self.t[idx], "group": (self.t[idx] // cfg["block_s"]).astype(np.int32), "cell": sel.cell, "stratum": sel.stratum, "weight": sel.weight}
            for j, (nm, _, _) in enumerate(cfg["cells"]):
                cols["c_" + nm] = sel.cells[:, j]
            cols["n_dup"], cols["score"], cols["transient"], cols["step"] = sel.n_dup, sel.score, sel.transient.astype(np.int8), sel.step.astype(np.int8)
        for j, nm in enumerate(self.yn):
            cols["y_" + nm] = self.Y[idx, j]
        ph = cfg["phase_channel"]
        cols["step_phase"] = np.asarray(self.X[idx, self.ix[ph]], dtype=np.float32) if ph in self.ix else np.full(len(idx), np.nan, np.float32)
        Fi = self.F[idx]
        for j, nm in enumerate(self.names):
            cols["f_" + nm] = Fi[:, j]
        meta = {"k0": self.k0, "names": self.names, "targets": self.yn, "cells": [[a, b, c] for a, b, c in cfg["cells"]], "block_s": cfg["block_s"]}
        if sel is not None:
            meta.update({"level": sel.level, "cap": sel.cap_eff, "select": cfg["select"], "dedup": cfg["dedup"], "counts": sel.counts})
        return pa.table({k: pa.array(v) for k, v in cols.items()}).replace_schema_metadata({"metaset": json.dumps(meta)})

    def write(self, table, path):
        pc = self.cfg["parquet"]
        os.makedirs(os.path.dirname(path), exist_ok=True)
        ids = [c for c in table.column_names if c in ("group", "cell", "stratum", "n_dup", "transient", "step") or c.startswith("c_")]
        enc = {c: "BYTE_STREAM_SPLIT" for c in table.column_names if table.schema.field(c).type in (pa.float32(), pa.float64()) or c == "t"}
        enc.pop("t", None)
        pq.write_table(table, path, compression="zstd", compression_level=pc["level"], row_group_size=pc["row_group"], use_dictionary=ids,
                       column_encoding=enc, write_statistics=[c for c in KEEP_STATS if c in table.column_names] or False)
        return os.path.getsize(path)

    def probe(self, table):
        pc = self.cfg["parquet"]
        n = min(pc["probe_rows"], table.num_rows)
        take = np.sort(np.random.default_rng(0).choice(table.num_rows, n, replace=False))
        tmp = self.cfg["out_path"] + ".probe"
        size = self.write(table.take(pa.array(take)), tmp)
        os.remove(tmp)
        return size / n

    def run(self, t_max=None, path=None):
        cfg = self.cfg
        pc = cfg["parquet"]
        path = path or cfg["out_path"]
        sel = self.build(t_max)
        per_row = self.probe(self.assemble(sel))
        rows_max = int(cfg["budget_bytes"] * pc["safety"] / per_row)
        info = {"bytes_per_row_probe": per_row, "rows_max": rows_max}
        for _ in range(pc["tries"]):
            sel = self.build(t_max, rows_max=rows_max)
            size = self.write(self.assemble(sel), path)
            info.update({"size": size, "level": sel.level, "counts": sel.counts})
            if size <= cfg["budget_bytes"]:
                return sel, info
            rows_max = int(rows_max * pc["shrink"])
        raise RuntimeError(f"cannot fit {cfg['budget_bytes']} bytes, last size {size}")


class Evaluator:
    def __init__(self, ms):
        self.ms, self.cfg = ms, ms.cfg["eval"]

    @staticmethod
    def ks(x, w, ref):
        ref = np.sort(ref)
        o = np.argsort(x)
        xs, cw = x[o], np.cumsum(w[o], dtype=np.float64) / w.sum(dtype=np.float64)
        pts = np.unique(np.concatenate([xs, ref]))
        at = np.searchsorted(xs, pts, side="right")
        fm = np.where(at > 0, cw[np.maximum(at - 1, 0)], 0.0)
        return float(np.abs(fm - np.searchsorted(ref, pts, side="right") / len(ref)).max())

    def structure(self, path, sel_rows):
        from sklearn.model_selection import StratifiedGroupKFold
        ms, cfg = self.ms, self.cfg
        t = pq.read_table(path, columns=["t", "group", "cell", "stratum", "weight"] + ["f_" + n for n in cfg["ks"]]).to_pandas()
        out = {"rows": int(len(t))}
        full = np.flatnonzero(ms.eligible())
        _, code_full = ms.bins(full)
        cf, nf = np.unique(code_full, return_counts=True)
        need = set(cf[nf >= cfg["min_cell_rows"]].tolist())
        have = set(t["cell"].unique().tolist())
        out["cells_full"], out["cells_ge30"], out["cells_ge30_present"] = int(len(cf)), len(need), len(need & have)
        gcount = t.groupby("stratum")["group"].nunique()
        big = set(gcount[gcount >= cfg["min_groups"]].index.tolist())
        out["strata"], out["strata_ge5_groups"] = int(len(gcount)), len(big)
        cov = []
        sgk = StratifiedGroupKFold(cfg["folds"], shuffle=True, random_state=0)
        for tr, te in sgk.split(t, t["stratum"], t["group"]):
            cov.append(len(big & set(t["stratum"].iloc[te].unique().tolist())) / max(len(big), 1))
        out["fold_coverage"] = cov
        ks = {}
        for n in cfg["ks"]:
            x = t["f_" + n].to_numpy(np.float64)
            r = ms.feat(n)[full]
            m = np.isfinite(x)
            ks[n] = self.ks(x[m], t["weight"].to_numpy(np.float64)[m], r[np.isfinite(r)])
        out["ks"] = ks
        out["pass"] = {"c2": out["cells_ge30"] == out["cells_ge30_present"], "c3": min(cov) >= cfg["cover"], "c5": max(ks.values()) <= cfg["ks_max"]}
        return out

    def backbone(self):
        ps = self.ms.ps
        names = [n for n in dict.fromkeys(ps.names()) if not n.startswith(("ph.fx", "ph.gf", "ph.px", "ph.pg"))]
        return names, np.array([n == "ph.pl" or n.endswith("*pl") for n in names])

    def fit_rmse(self, feats, free, rows, w, y, test, ytest, total):
        ms, cfg = self.ms, self.cfg
        cols = [ms.col[n] for n in feats]
        X = np.asarray(ms.F[rows][:, cols], dtype=np.float64)
        w = np.asarray(w, dtype=np.float64) * (total / np.sum(w))
        ok = np.isfinite(X)
        n = np.maximum((ok * w[:, None]).sum(0), 1e-12)
        mean = (np.where(ok, X, 0.0) * w[:, None]).sum(0) / n
        std = np.sqrt((np.where(ok, (X - mean) ** 2, 0.0) * w[:, None]).sum(0) / n)
        prep = Prep(cols)
        prep.mean, prep.std = mean, np.where(std < 1e-9, 1.0, std)
        f = LinearFit("ridge", {"alpha": cfg["alpha"]}).fit(prep.z(X, LIN_CLIP, free), y, w)
        Xt = np.asarray(ms.F[test][:, cols], dtype=np.float64)
        return float(np.sqrt(np.mean((f.predict(prep.z(Xt, LIN_CLIP, free)) - ytest) ** 2)))

    def holdout(self, t_cut):
        ms, cfg = self.ms, self.cfg
        feats, free = self.backbone()
        sel = ms.build(t_cut)
        train = np.flatnonzero(ms.eligible(t_cut))
        test = np.flatnonzero(ms.eligible() & (ms.t >= t_cut))
        res = {"train_rows": int(len(train)), "meta_rows": int(len(sel.idx)), "test_rows": int(len(test)), "targets": {}}
        rng = np.random.default_rng(0)
        for tg in cfg["targets"]:
            j = ms.yn.index(tg)
            yt = ms.Y[test, j].astype(np.float64)
            tot = float(len(train))
            full = self.fit_rmse(feats, free, train, np.ones(len(train)), ms.Y[train, j].astype(np.float64), test, yt, tot)
            meta = self.fit_rmse(feats, free, sel.idx, sel.weight, ms.Y[sel.idx, j].astype(np.float64), test, yt, tot)
            rand = []
            for s in range(cfg["seeds"]):
                r = np.sort(np.random.default_rng(s).choice(train, len(sel.idx), replace=False))
                rand.append(self.fit_rmse(feats, free, r, np.ones(len(r)), ms.Y[r, j].astype(np.float64), test, yt, tot))
            persist = float(np.sqrt(np.mean(yt ** 2)))
            res["targets"][tg] = {"full": full, "meta": meta, "random_median": float(np.median(rand)), "random": rand, "persistence": persist, "meta_over_full": meta / full,
                                  "pass_within": meta <= (1 + cfg["rmse_tol"]) * full, "pass_beats_random": meta < float(np.median(rand))}
        res["pass"] = {"c4": all(v["pass_within"] and v["pass_beats_random"] for v in res["targets"].values())}
        return res

    def run(self):
        ms, cfg = self.ms, self.ms.cfg
        t0 = time.time()
        sel, info = ms.run()
        rep = {"build": info, "size_ok": info["size"] <= cfg["budget_bytes"], "full_rows": int(ms.T), "kept_over_full": len(sel.idx) / ms.T,
               "kept_over_eligible": len(sel.idx) / info["counts"]["eligible"], "weight_sum": float(sel.weight.sum(dtype=np.float64))}
        rep["structure"] = self.structure(cfg["out_path"], sel.idx)
        t_cut = ms.k0 + int((1 - self.cfg["holdout"]) * (ms.T - 1))
        rep["holdout"] = self.holdout(t_cut)
        rep["seconds"] = time.time() - t0
        os.makedirs(os.path.dirname(cfg["report_path"]), exist_ok=True)
        with open(cfg["report_path"], "w") as f:
            json.dump(rep, f, indent=1)
        return rep


def main(argv, cfg=CONFIG):
    import psutil
    psutil.Process().nice(psutil.BELOW_NORMAL_PRIORITY_CLASS)
    ms = Metaset(cfg).load()
    if argv and argv[0] == "full":
        size = ms.write(ms.assemble(), cfg["full_path"])
        print(json.dumps({"full_rows": ms.T, "full_bytes": size}))
        return
    print(json.dumps(Evaluator(ms).run(), indent=1))


if __name__ == "__main__":
    main(sys.argv[1:])
