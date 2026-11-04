import json
import os
import pickle
import numpy as np
from features import BatchEngine, FeatureFrame, Spec, CONFIG as FCFG
from models import CONFIG, Dataset, Metrics
from rawstore import RawStore

CFG = {
    "load_w": [100.0, 150.0],
    "fan_band": [20.0, 95.0],
    "shifts": [-10.0, -20.0, -40.0],
    "targets": ["water_120", "water_300", "water_900"],
    "warm_s": 3600,
    "bins": 5.0,
}


class Excitation:
    def __init__(self):
        meta, X = RawStore().load()
        self.ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
        self.meta, self.X = meta, np.asarray(X)
        spec = Spec.load(FCFG["spec_path"])
        self.names, self.F, self.v = FeatureFrame().compute(spec, self.X, self.ix, meta["k0"])
        self.col = {n: i for i, n in enumerate(self.names)}

    def f(self, n):
        return self.F[:, self.col[n]].astype(np.float64)

    def inventory(self):
        fan, p, w = self.f("fan_duty"), self.f("p_total"), self.f("water")
        lo, hi = CFG["fan_band"]
        out = {"seconds_total_d5": int(np.isfinite(w).sum()), "seconds_lhm": int(np.isfinite(p).sum())}
        for L in CFG["load_w"]:
            load = p > L
            out[f"load>{int(L)}W_s"] = int(load.sum())
            out[f"load>{int(L)}W_fan>=99_s"] = int((load & (fan >= 99)).sum())
            out[f"load>{int(L)}W_fan_in_band_s"] = int((load & (fan >= lo) & (fan <= hi)).sum())
            out[f"load>{int(L)}W_fan<80_s"] = int((load & (fan < 80)).sum())
            q = np.nanquantile(fan[load], [0.01, 0.05, 0.5]).tolist() if load.sum() else None
            out[f"load>{int(L)}W_fan_q01_q05_q50"] = q
        band = (fan >= lo) & (fan <= hi) & np.isfinite(p)
        out["band_seconds_with_lhm"] = int(band.sum())
        out["band_power_quantiles_w"] = np.nanquantile(p[band], [0.05, 0.5, 0.95]).tolist() if band.sum() else None
        return out

    def duty_predictability(self):
        import xgboost as xgb
        D = Dataset(CONFIG).load()
        cols = list(D.names)
        cols = [n for n in cols if "fan_" not in n and "fan_duty" not in n and "fan_rpm" not in n and "fan_pow" not in n and "fan_cur" not in n]
        y = D.F[:, D.col["fan_duty"]]
        tr = np.where((D.block == D.bnames.index("train")) & np.isfinite(y))[0]
        te = np.where((D.block == D.bnames.index("test")) & np.isfinite(y))[0]
        ci = [D.col[n] for n in cols]
        m = xgb.train({"max_depth": 5, "eta": 0.05, "subsample": 0.7, "colsample_bytree": 0.5, "nthread": 2, "min_child_weight": 20},
                      xgb.DMatrix(D.F[tr][:, ci], y[tr]), 300)
        p = m.predict(xgb.DMatrix(D.F[te][:, ci]))
        r = y[te] - p
        band = (y[te] >= CFG["fan_band"][0]) & (y[te] <= CFG["fan_band"][1])
        return {"r2_test": float(1 - r.var() / y[te].var()), "resid_std": float(r.std()), "duty_std": float(y[te].std()),
                "band_n": int(band.sum()), "band_resid_std": float(r[band].std()) if band.sum() else None, "n_feat": len(cols)}

    def structural(self, seconds=120):
        w, p = self.f("water.ewm5"), self.f("p_total.ewm30")
        fan, amb, pump = self.f("fan_duty") / 100.0, self.f("amb_est"), self.f("pump_pow")
        T = len(w)
        dT = np.full(T, np.nan)
        dT[:-seconds] = (w[seconds:] - w[:-seconds]) / seconds
        gpu, cpu = self.f("gpu_w.ewm30"), self.f("cpu_pkg_w.ewm30")
        dt = w - 26.0
        rows = np.where(np.isfinite(dT) & np.isfinite(p) & np.isfinite(dt) & np.isfinite(fan) & (p > CFG["load_w"][0]))[0][::5]
        A = np.column_stack([p[rows], dt[rows], fan[rows] * dt[rows], np.ones(len(rows))])
        y = dT[rows]
        beta = np.linalg.lstsq(A, y, rcond=None)[0]
        blk = rows // 600
        u, inv = np.unique(blk, return_inverse=True)
        rng = np.random.default_rng(0)
        bs = []
        for _ in range(500):
            pick = rng.integers(0, len(u), len(u))
            sel = np.concatenate([np.where(inv == k)[0] for k in pick])
            bs.append(np.linalg.lstsq(A[sel], y[sel], rcond=None)[0])
        bs = np.array(bs)
        corr = np.corrcoef(A[:, 1:3].T)[0, 1]
        return {"n": int(len(rows)), "beta_names": ["P_total", "dT(water-26)", "fan*dT", "const"], "beta": beta.tolist(),
                "ci95": np.quantile(bs, [0.025, 0.975], axis=0).T.tolist(), "corr_dT_fanxdT": float(corr),
                "implied_G1_over_C_per_s": float(-beta[2]), "note": "fan*dT coefficient expected negative (more fan, more cooling)"}

    def duty_maps(self):
        fan = self.X[:, self.ix["d5next/fan/duty"]]
        out = {}
        b = CFG["bins"]
        edges = np.arange(0, 105, b)
        for ch in ("rpm", "power", "current"):
            y = self.X[:, self.ix[f"d5next/fan/{ch}"]]
            ok = np.isfinite(fan) & np.isfinite(y)
            xs, ys = [], []
            for lo in edges:
                m = ok & (fan >= lo) & (fan < lo + b)
                if m.sum() > 30:
                    xs.append(float(np.median(fan[m])))
                    ys.append(float(np.median(y[m])))
            out[ch] = (xs, ys)
        return out

    def counterfactual(self, models, shifts=None):
        import xgboost as xgb
        D = Dataset(CONFIG).load()
        maps = self.duty_maps()
        bi = D.bnames.index("test")
        shifts = shifts or CFG["shifts"]
        idx_all = np.where(D.block == bi)[0]
        g0, g1 = int(D.t[idx_all[0]]) - CFG["warm_s"], int(D.t[idx_all[-1]]) + 1
        spec = Spec.load(FCFG["spec_path"])
        sub = self.X[g0:g1].copy()
        k0 = self.meta["k0"] + g0

        def feats_for(Xs):
            nm, F, _ = FeatureFrame().compute(spec, Xs, self.ix, k0)
            return {n: i for i, n in enumerate(nm)}, F

        col, Fb = feats_for(sub)
        rows = D.t[idx_all] - g0
        load = Fb[rows, col["p_total.ewm30"]] > CFG["load_w"][1]
        fan_obs = Fb[rows, col["fan_duty"]]
        res = {"rows_load>150": int(load.sum()), "rows_load>150_fan<=80_observed": int((load & (fan_obs <= 80)).sum()), "shifts": {}}
        m = Metrics(CONFIG)
        for sh in shifts:
            Xs = sub.copy()
            d = Xs[:, self.ix["d5next/fan/duty"]]
            nd = np.clip(d + sh, 5.0, 100.0)
            Xs[:, self.ix["d5next/fan/duty"]] = nd
            for ch in ("rpm", "power", "current"):
                xs, ys = maps[ch]
                Xs[:, self.ix[f"d5next/fan/{ch}"]] = np.interp(nd, xs, ys)
            c2, F2 = feats_for(Xs)
            for name, (path_fmt, feats) in models.items():
                for tg in CFG["targets"]:
                    bst = xgb.Booster()
                    bst.load_model(path_fmt.format(tg=tg))
                    a = bst.predict(xgb.DMatrix(Fb[rows][:, [col[n] for n in feats]]))
                    b = bst.predict(xgb.DMatrix(F2[rows][:, [c2[n] for n in feats]]))
                    dlt = (b - a)[load]
                    t = D.t[idx_all][load]
                    blk = (t // 600)
                    u, inv = np.unique(blk, return_inverse=True)
                    rng = np.random.default_rng(1)
                    means = []
                    for _ in range(500):
                        pick = rng.integers(0, len(u), len(u))
                        sel = np.concatenate([np.where(inv == k)[0] for k in pick])
                        means.append(dlt[sel].mean())
                    res["shifts"].setdefault(str(sh), {})[f"{name}|{tg}"] = {"mean_dwater_K": float(dlt.mean()), "ci95": np.quantile(means, [0.025, 0.975]).tolist(),
                                                                         "frac_rows_positive": float((dlt > 1e-4).mean()), "frac_rows_zero": float((np.abs(dlt) <= 1e-4).mean())}
        return res

    def run(self, models, out):
        r = {"inventory": self.inventory(), "duty_predictability": self.duty_predictability(), "structural": self.structural(),
             "counterfactual": self.counterfactual(models)}
        with open(out, "w") as f:
            json.dump(r, f, indent=1)
        return r
