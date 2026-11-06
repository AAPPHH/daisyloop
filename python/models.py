import datetime as dt
import json
import math
import os
import time
import numpy as np
from features import FeatureFrame, Ops, PHYS, PhysSpec, Spec, CONFIG as FCFG
from rawstore import RawStore

HERE = os.path.dirname(os.path.abspath(__file__))
CONFIG = {
    "horizons": [30, 120, 300, 900],
    "series": {"water": "d5next/water/temp", "cpu": "/intelcpu/0/temperature/0", "gpu": "nvml/0/temp"},
    "stride": 5,
    "blocks": {
        "train": [("2026-11-01 10:34", "2026-11-02 06:00")],
        "val": [("2026-11-02 06:45", "2026-11-02 14:00")],
        "test": [("2026-11-02 14:45", "2026-11-02 22:10")],
        "lhm_missing": [("2026-11-03 08:42", "2026-11-03 13:12")],
        "oor_pump20": [("2026-11-04 07:40", "2026-11-04 09:24"), ("2026-11-04 14:31", "2026-11-04 14:57")],
    },
    "forced": ["fan_duty", "fan_rpm", "pump_duty", "pump_rpm"],
    "small": [
        "water", "water.ewm30", "water.ewm600", "water.ewm5.d30", "water.ewm5.d120", "p_total.ewm5", "p_total.ewm30", "p_total.ewm120",
        "p_total.ewm600", "cpu_pkg_w.ewm5", "gpu_w.ewm5", "fan_duty", "fan_rpm", "pump_duty", "cpu_core_t.ewm5", "gpu_temp.ewm5",
        "amb_est", "water_minus_amb", "p_excess.ewm120", "tsc_ptotal",
    ],
    "xgb": {"rounds": 600, "early": 40, "eta": 0.04, "max_depth": 5, "min_child_weight": 40, "subsample": 0.7, "colsample_bytree": 0.5,
            "lambda": 10.0, "tree_method": "hist", "nthread": 2, "objective": "reg:squarederror"},
    "boot_block_s": 600,
    "boot_n": 1000,
    "runs_log": os.path.join(HERE, "..", "models", "runs.jsonl"),
    "out_dir": os.path.join(HERE, "..", "models"),
    "cache": os.path.join(FCFG["cache_dir"], "D5s.npz"),
    "ray_env": {"PYTHONPATH": os.environ.get("PYTHONPATH", "") or HERE},
    "ray_res": {"num_cpus": 2, "memory": 700e6},
}


def local_idx(meta, s):
    return int(dt.datetime.strptime(s, "%Y-%m-%d %H:%M").astimezone().timestamp()) - meta["k0"]


class Dataset:
    def __init__(self, cfg=CONFIG):
        self.cfg = cfg

    def build(self):
        cfg = self.cfg
        sd = cfg.get("store_dir")
        meta, X = (RawStore({"cache_dir": sd}) if sd else RawStore()).load()
        ix = {c["channel"]: i for i, c in enumerate(meta["channels"]) }
        ff = FeatureFrame(dict(FCFG, cache_dir=sd)) if sd else FeatureFrame()
        if cfg.get("phys"):
            ps = PhysSpec(Spec.load(FCFG["spec_path"]), dict(PHYS, **cfg.get("phys_cfg", {})))
            spec = ps.build()
            if cfg.get("phys_only"):
                spec = spec.subset(set(ps.names()))
            names, F, _ = ff.compute(spec, X, ix, meta["k0"])
        else:
            names, F = ff.load()
        T = F.shape[0]
        extra = {}
        if cfg.get("future_fan"):
            pc = dict(PHYS, **cfg.get("phys_cfg", {}))
            fan = Ops.ffill(np.asarray(X[:, ix["d5next/fan/rpm"]], dtype=np.float64), FCFG["ffill_s"], False)
            wma = np.asarray(F[:, names.index("water_minus_amb")], dtype=np.float64)
            for k in cfg["future_fan"]:
                fut = np.full(T, np.nan)
                fut[: T - k] = fan[k:]
                extra[f"ph.fxf{k}"] = (pc["gn_frac"] + (1 - pc["gn_frac"]) * (fut / pc["rpm_full"]) ** pc["m"]) * wma
        ser = {k: Ops.ffill(np.asarray(X[:, ix[ch]], dtype=np.float64), FCFG["ffill_s"], False) for k, ch in cfg["series"].items()}
        rows = np.arange(0, T, cfg["stride"])
        Y, tn = [], []
        for k in cfg["series"]:
            for h in cfg["horizons"]:
                y = np.full(T, np.nan)
                y[: T - h] = ser[k][h:] - ser[k][: T - h]
                Y.append(y[rows])
                tn.append(f"{k}_{h}")
        block = np.full(T, -1, dtype=np.int8)
        bn = list(cfg["blocks"])
        for b, spans in enumerate(cfg["blocks"].values()):
            for a, z in spans:
                block[max(0, local_idx(meta, a)): local_idx(meta, z)] = b
        names = list(names) + list(extra)
        Fs = np.asarray(F[rows])
        if extra:
            Fs = np.hstack([Fs, np.column_stack([v[rows] for v in extra.values()]).astype(np.float32)])
        np.savez(cfg["cache"], F=Fs, Y=np.array(Y).T.astype(np.float32), t=rows, block=block[rows], names=np.array(names),
                 tnames=np.array(tn), bnames=np.array(bn), k0=meta["k0"], raw=np.array([ser[k][rows] for k in cfg["series"]]).T.astype(np.float32))

    def load(self):
        z = np.load(self.cfg["cache"])
        self.F, self.Y, self.t, self.block = z["F"], z["Y"], z["t"], z["block"]
        self.names, self.tnames, self.bnames, self.k0 = list(z["names"]), list(z["tnames"]), list(z["bnames"]), int(z["k0"])
        self.col = {n: i for i, n in enumerate(self.names)}
        return self

    def sel(self, block, target):
        y = self.Y[:, self.tnames.index(target)]
        return np.where((self.block == self.bnames.index(block)) & np.isfinite(y))[0]

    def lhm_cols(self):
        b = self.bnames.index("lhm_missing")
        miss = np.isfinite(self.F[self.block == b]).mean(0) < 0.05
        have = np.isfinite(self.F[self.block == self.bnames.index("train")]).mean(0) > 0.9
        return [n for n, m, h in zip(self.names, miss, have) if m and h]

    def X(self, idx, feats=None):
        if feats is None:
            return self.F[idx]
        return self.F[idx][:, [self.col[n] for n in feats]]


class Physics:
    def __init__(self, target, cfg=CONFIG, fan_term=False):
        self.target, self.cfg, self.fan_term = target, cfg, fan_term
        self.theta = None

    def inputs(self, D, idx):
        g = lambda n: D.F[idx, D.col[n]].astype(np.float64)
        return {"T0": g("water.ewm5") if self.target.startswith("water") else g({"cpu": "cpu_core_t.ewm5", "gpu": "gpu_temp.ewm5"}[self.target.split("_")[0]]),
                "water": g("water.ewm5"), "cpu": g("cpu_pkg_w.ewm5"), "gpu": g("gpu_w.ewm5"), "pump": g("pump_pow"), "fan": g("fan_duty") / 100.0,
                "ambient": g("amb_est")}

    def predict_h(self, th, inp, h):
        if self.target.startswith("water"):
            C, G0, G1, Ta, a, b, q0 = th
            G = G0 + G1 * inp["fan"]
            Q = a * inp["cpu"] + b * inp["gpu"] + inp["pump"] + q0
            Tinf = Ta + Q / G
            return (Tinf - inp["T0"]) * (1 - np.exp(-h * G / (C * 1.0)))
        tau, R, b0 = th
        P = inp["cpu"] if self.target.startswith("cpu") else inp["gpu"]
        return (inp["water"] + R * P + b0 - inp["T0"]) * (1 - np.exp(-h / tau))

    def x0(self):
        if self.target.startswith("water"):
            return np.array([12000.0, 25.0, 0.0 if not self.fan_term else 3.0, 26.0, 0.9, 0.9, 0.0]), ([2000, 5, 0, 15, 0.3, 0.3, -100], [80000, 120, 1e-6 if not self.fan_term else 60, 35, 2.0, 2.0, 100])
        return np.array([30.0, 0.15, 5.0]), ([2, 0.0, -20], [600, 1.0, 40])

    def fit(self, D, tr):
        from scipy.optimize import least_squares
        horizons = [int(n.split("_")[1]) for n in D.tnames if n.startswith(self.target.split("_")[0])]
        tg = self.target.split("_")[0]
        idx = tr[::2]
        inp = self.inputs(D, idx)
        ys = {h: D.Y[idx, D.tnames.index(f"{tg}_{h}")].astype(np.float64) for h in horizons}
        ok = np.ones(len(idx), bool)
        for v in inp.values():
            ok &= np.isfinite(v)
        for y in ys.values():
            ok &= np.isfinite(y)
        inp = {k: v[ok] for k, v in inp.items()}
        ys = {h: y[ok] for h, y in ys.items()}
        x0, (lo, hi) = self.x0()

        def res(th):
            return np.concatenate([(self.predict_h(th, inp, h) - ys[h]) / max(1.0, math.sqrt(h) / 5) for h in horizons])

        self.theta = least_squares(res, x0, bounds=(lo, hi), loss="soft_l1", f_scale=0.3, x_scale=np.maximum(np.abs(x0), 1)).x
        return self

    def predict(self, D, idx, h):
        inp = self.inputs(D, idx)
        return self.predict_h(self.theta, inp, h)


class Metrics:
    def __init__(self, cfg=CONFIG, seed=0):
        self.cfg, self.rng = cfg, np.random.default_rng(seed)

    @staticmethod
    def rmse(e):
        return float(np.sqrt(np.mean(e ** 2)))

    def boot(self, t, errs, n=None):
        n = n or self.cfg["boot_n"]
        b = (t // self.cfg["boot_block_s"]).astype(np.int64)
        u, inv = np.unique(b, return_inverse=True)
        cnt = np.bincount(inv).astype(np.float64)
        S = {k: np.bincount(inv, weights=e ** 2) for k, e in errs.items()}
        draws = self.rng.integers(0, len(u), (n, len(u)))
        tot = cnt[draws].sum(1)
        return {k: np.sqrt(s[draws].sum(1) / tot) for k, s in S.items()}

    def compare(self, t, y, preds):
        errs = {k: y - p for k, p in preds.items()}
        errs["persist"] = y.copy()
        bs = self.boot(t, errs)
        out = {"n": int(len(y)), "rmse": {k: self.rmse(e) for k, e in errs.items()}}
        out["rmse_ci"] = {k: [float(np.quantile(v, .025)), float(np.quantile(v, .975))] for k, v in bs.items()}
        out["skill_vs_persist"] = {}
        for k in preds:
            s = 1 - bs[k] / bs["persist"]
            out["skill_vs_persist"][k] = [float(1 - out["rmse"][k] / out["rmse"]["persist"]), float(np.quantile(s, .025)), float(np.quantile(s, .975))]
        out["skill_vs"] = {}
        for k in preds:
            for j in preds:
                if k != j:
                    s = 1 - bs[k] / bs[j]
                    out["skill_vs"][f"{k}|{j}"] = [float(1 - out["rmse"][k] / out["rmse"][j]), float(np.quantile(s, .025)), float(np.quantile(s, .975))]
        return out


class RunLog:
    def __init__(self, cfg=CONFIG):
        self.path = cfg["runs_log"]

    def write(self, **kw):
        os.makedirs(os.path.dirname(os.path.abspath(self.path)), exist_ok=True)
        kw["ts"] = time.time()
        kw["iso"] = dt.datetime.now().astimezone().isoformat(timespec="seconds")
        with open(self.path, "a") as f:
            f.write(json.dumps(kw) + "\n")


def augment(D, X, y, feats, frac, seed):
    rng = np.random.default_rng(seed)
    cols = set(D.lhm_cols())
    pick = rng.random(len(X)) < frac
    Xa = X[pick].copy()
    ci = [i for i, n in enumerate(feats) if n in cols]
    Xa[:, ci] = np.nan
    for i, n in enumerate(feats):
        if n.startswith("mask_lhm"):
            Xa[:, i] = 0.0
    return np.vstack([X, Xa]), np.concatenate([y, y[pick]])


def xgb_task(job):
    import xgboost as xgb
    cfg = job.get("cfg", CONFIG)
    D = Dataset(cfg).load()
    feats = job["feats"] or D.names
    out = {}
    for tg in job["targets"]:
        tr, va = D.sel("train", tg), D.sel("val", tg)
        p = dict(cfg["xgb"])
        p.update(job.get("params", {}))
        rounds, early = p.pop("rounds"), p.pop("early")
        p["seed"] = job.get("seed", 0)
        ti = D.tnames.index(tg)
        Xtr, ytr = D.X(tr, feats), D.Y[tr, ti]
        if job.get("lhm_aug"):
            Xtr, ytr = augment(D, Xtr, ytr, feats, job["lhm_aug"], job.get("seed", 0))
        dtr, dva = xgb.DMatrix(Xtr, ytr), xgb.DMatrix(D.X(va, feats), D.Y[va, ti])
        m = xgb.train(p, dtr, rounds, evals=[(dva, "val")], early_stopping_rounds=early, verbose_eval=False)
        r = {"best_iter": int(m.best_iteration), "preds": {}}
        for b in D.bnames:
            ix = D.sel(b, tg)
            r["preds"][b] = m.predict(xgb.DMatrix(D.X(ix, feats)), iteration_range=(0, m.best_iteration + 1)) if len(ix) else np.zeros(0)
        g = m.get_score(importance_type="gain")
        r["gain"] = {n: float(g.get(f"f{i}", 0.0)) for i, n in enumerate(feats)}
        if job.get("save"):
            m.save_model(os.path.join(cfg["out_dir"], f"{job['tag']}_{tg}.json"))
        out[tg] = r
    return {"tag": job["tag"], "res": out}


def physics_task(job):
    D = Dataset(CONFIG).load()
    out = {}
    for fan in (False, True):
        for tg in ("water", "cpu", "gpu"):
            if tg != "water" and fan:
                continue
            ph = Physics(tg, CONFIG, fan).fit(D, D.sel("train", f"{tg}_300"))
            r = {"theta": ph.theta.tolist(), "preds": {}}
            for h in CONFIG["horizons"]:
                t = f"{tg}_{h}"
                r["preds"][t] = {b: ph.predict(D, D.sel(b, t), h) for b in D.bnames}
            out[f"{tg}{'_fan' if fan else ''}"] = r
    return out


def build_task(cfg):
    Dataset(cfg).build()
    return cfg["cache"]


LOCAL = os.path.join(os.environ.get("LOCALAPPDATA", "."), "daisyloop-ml")
FRESH = dict(CONFIG)
FRESH.update({
    "horizons": [30, 60, 90, 120, 300, 900],
    "store_dir": os.path.join(LOCAL, "cache_fresh"),
    "cache": os.path.join(LOCAL, "cache_fresh", "D5f.npz"),
    "phys": True,
    "blocks": dict(CONFIG["blocks"], fresh0=[("2026-11-04 15:10", "2026-11-04 19:30")], fresh1=[("2026-11-04 19:30", "2026-11-05 08:00")],
                   fresh2=[("2026-11-05 08:45", "2026-11-05 18:40")]),
    "runs_log": os.path.join(HERE, "..", "models", "runs_linear.jsonl"),
})


LIN = {
    "ridge": {"alpha": [0.1, 1.0, 10.0, 100.0, 1000.0, 10000.0]},
    "svr": {"C": [0.001, 0.01, 0.1], "epsilon": [0.005, 0.01, 0.02]},
    "huber": {"alpha": [1.0, 100.0, 3000.0], "epsilon": [1.35]},
}
LIN_TARGETS = ["water_30", "water_60", "water_90", "water_120", "cpu_30", "cpu_120", "gpu_30", "gpu_120"]
LIN_SETS = ["a44", "phys", "rich"]
LIN_CLIP = 5.0


class Prep:
    def __init__(self, cols):
        self.cols = list(cols)

    def fit(self, F, rows):
        X = F[rows][:, self.cols].astype(np.float64)
        ok = np.isfinite(X)
        n = np.maximum(ok.sum(0), 1)
        self.mean = np.where(ok, X, 0.0).sum(0) / n
        self.std = np.sqrt(np.where(ok, (X - self.mean) ** 2, 0.0).sum(0) / n)
        self.std = np.where(self.std < 1e-9, 1.0, self.std)
        return self

    def z(self, X, clip=None):
        Z = np.nan_to_num((X - self.mean) / self.std, nan=0.0, posinf=0.0, neginf=0.0)
        return np.clip(Z, -clip, clip) if clip else Z

    def transform(self, F, rows, clip=None):
        return self.z(F[rows][:, self.cols].astype(np.float64), clip)


class LinearFit:
    def __init__(self, family, params):
        self.family, self.params = family, params

    def make(self):
        from sklearn.linear_model import HuberRegressor, Ridge
        from sklearn.svm import LinearSVR
        p = self.params
        if self.family == "ridge":
            return Ridge(alpha=p["alpha"])
        if self.family == "svr":
            return LinearSVR(C=p["C"], epsilon=p["epsilon"], loss="epsilon_insensitive", dual=True, tol=1e-3, max_iter=1000, random_state=0)
        return HuberRegressor(epsilon=p["epsilon"], alpha=p["alpha"], max_iter=200)

    @staticmethod
    def grid(family):
        g = LIN[family]
        keys = list(g)
        out = [{}]
        for k in keys:
            out = [dict(o, **{k: v}) for o in out for v in g[k]]
        return out

    def fit(self, Z, y):
        import warnings
        warnings.simplefilter("ignore")
        m = self.make().fit(Z, y)
        self.w, self.b = np.asarray(m.coef_, dtype=np.float64), float(np.ravel(m.intercept_)[0])
        return self

    def predict(self, Z):
        return Z @ self.w + self.b


def lin_rows(D, blocks, tg):
    return np.concatenate([D.sel(b, tg) for b in blocks]) if blocks else np.zeros(0, dtype=np.int64)


def linear_task(job):
    cfg = job["cfg"]
    D = Dataset(cfg).load()
    feats = list(job["feats"])
    if job.get("drop_lhm"):
        lhm = set(D.lhm_cols())
        feats = [n for n in feats if n not in lhm and not n.startswith("mask_lhm")]
    cols = [D.col[n] for n in feats]
    clip = job.get("clip", LIN_CLIP)
    out = {"feats": feats, "clip": clip, "targets": {}}
    for tg in job["targets"]:
        fit_rows = lin_rows(D, job["fit"], tg)
        prep = Prep(cols).fit(D.F, fit_rows)
        Ztr, ytr = prep.transform(D.F, fit_rows, clip), D.Y[fit_rows, D.tnames.index(tg)].astype(np.float64)
        if job.get("val"):
            vr = lin_rows(D, job["val"], tg)
            Zv, yv = prep.transform(D.F, vr, clip), D.Y[vr, D.tnames.index(tg)].astype(np.float64)
            res = []
            for p in LinearFit.grid(job["family"]):
                f = LinearFit(job["family"], p).fit(Ztr, ytr)
                res.append((float(np.sqrt(np.mean((f.predict(Zv) - yv) ** 2))), p))
            best = min(res, key=lambda r: r[0])[1]
        else:
            res, best = [], job["params"][tg]
        f = LinearFit(job["family"], best).fit(Ztr, ytr)
        r = {"params": best, "grid": res, "preds": {}, "mean": prep.mean, "std": prep.std, "w": f.w, "b": f.b}
        for b in D.bnames:
            ix = D.sel(b, tg)
            r["preds"][b] = f.predict(prep.transform(D.F, ix, clip)) if len(ix) else np.zeros(0)
        out["targets"][tg] = r
    return {"tag": job["tag"], "res": out}


def xgb_fit_task(job):
    import xgboost as xgb
    cfg = job["cfg"]
    D = Dataset(cfg).load()
    feats = job["feats"]
    out = {}
    for tg in job["targets"]:
        tr = lin_rows(D, job["fit"], tg)
        p = dict(cfg["xgb"], **job["params"])
        p.pop("rounds"), p.pop("early")
        p["seed"] = 0
        ti = D.tnames.index(tg)
        Xtr, ytr = augment(D, D.X(tr, feats), D.Y[tr, ti], feats, job["lhm_aug"], 0)
        m = xgb.train(p, xgb.DMatrix(Xtr, ytr), job["rounds"][tg])
        out[tg] = {"preds": {b: m.predict(xgb.DMatrix(D.X(D.sel(b, tg), feats))) if len(D.sel(b, tg)) else np.zeros(0) for b in D.bnames}}
        if job.get("save"):
            m.save_model(os.path.join(cfg["out_dir"], f"{job['tag']}_{tg}.json"))
    return {"tag": job["tag"], "res": out}


VARIANTS = {"m07": {"m": 0.7}, "m25": {"m": 2.5}, "gn20": {"gn_frac": 0.2}, "ff": {}}
for _k, _v in VARIANTS.items():
    VARIANTS[_k] = dict(FRESH, cache=os.path.join(LOCAL, "cache_fresh", f"D5f_{_k}.npz"), phys_cfg=_v, phys_only=True, future_fan=[15, 30] if _k == "ff" else None)
