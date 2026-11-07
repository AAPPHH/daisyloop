import copy
import itertools
import json
import math
import os
import shutil
import sys
import tempfile
import time
import numpy as np
from scipy.optimize import least_squares
from scipy.signal import lfilter
from export import LinearExporter
from features import FeatureFrame, Node, Ops, PHYS, PhysSpec, Spec, CONFIG as FCFG
from models import CONFIG as MCFG, FRESH, LIN_CLIP, LinearFit, Metrics, PINT, Prep, local_idx
from metastore import HistoryStore
from rawstore import RawStore
from stepsim import Injector, Plant, SIM

HERE = os.path.dirname(os.path.abspath(__file__))
MODELS = os.path.join(HERE, "..", "models")
ALT = {"m": 1.2, "a": 30.0, "c1": 8500.0, "k": 15.0, "gamma": 0.1, "ta": 22.0, "spool_s": 8.0}
WRONG = {"recirc12": {"m": 1.2, "ta_fan": 10.0, "q0": 0.0}, "recirc161": {"m": 1.61, "ta_fan": 10.0, "q0": 0.0}, "air": {"m": 0.0, "ta_fan": 8.0, "q0": 40.0}}
CFG = {
    "source": "history",
    "store_dir": FCFG["cache_dir"],
    "deployed": os.path.join(MODELS, "model_deploy.json"),
    "deployed_spec": os.path.join(MODELS, "feature_spec_deploy.json"),
    "out_dir": os.path.join(MODELS, "steptrain"),
    "phase_channel": "daisyloop/step/phase",
    "windows": [],
    "window": {"merge_s": 120, "pre_s": 300, "post_s": 900, "embargo_s": 900, "ctx_pre_s": 300, "ctx_post_s": 300, "gap_s": 10, "piece_min_s": 120},
    "blocks": FRESH["blocks"],
    "folds": {"fit": ["train", "oor_pump20", "fresh0", "step_train"], "select": ["val", "fresh1"], "final": ["test", "fresh2", "step_hold"]},
    "stride": MCFG["stride"],
    "series": {"water": MCFG["series"]["water"]},
    "horizons": [30, 60, 90, 120],
    "seed": 0,
    "threads": 1,
    "step_share": 0.05,
    "families": {"ridge": {"alpha": [100.0, 1000.0, 10000.0, 100000.0]}, "svr": {"C": [0.0001, 0.0003, 0.001], "epsilon": [0.01]}},
    "select_targets": ["water_30", "water_60", "water_120"],
    "trees": {"targets": ["water_30", "water_60", "water_120"], "rounds": 600, "early": 40, "params": {"eta": 0.03, "max_depth": 3, "min_child_weight": 100, "subsample": 0.7, "colsample_bytree": 0.5, "lambda": 10.0, "tree_method": "hist", "objective": "reg:squarederror"}},
    "boot": {"block_s": 600, "n": 400},
    "workers": 5,
    "blas_threads": 1,
    "gate": {
        "grey": {"hold": {"fan": [6, 7], "pump": [4, 5]}, "se_inflate": 10.0, "z": 1.96, "full_s": 120.0, "fit_rmse_max": 0.12, "c_range": [8.0, 16.0], "ahead": {"h": 120, "step": 10, "gain": 1.5}, "null_bounds": {"C": [8.0, 16.0], "Ta": [18.0, 30.0]}, "boot": {"block_s": 240, "n": 5000, "seed": 0, "q": 0.025}, "loo": True},
        "fan": {"grid_lo_duty": 27.0, "min_levels": 3, "min_span": 40.0, "min_drop": 0.25, "dwell_s": 120},
        "pump": {"dwell_s": 120, "why": "pump coefficient not identified: the protocol does not separate it from pump heat and dead time (dry run: truth 0 gives a fitted 0.14, flag fired in 2 of 5 runs only at truth 0.6), the pump stays on the firmware PID"},
        "hist": {"block_s": 60, "warm_s": 300, "min_rpm": 1400.0, "min_pump_rpm": 4000.0, "max_fan_range": 40.0, "max_pump_range": 100.0, "min_blocks": 100, "min_span_k": 3.0, "a_range": [20.0, 28.0], "h": 120, "step": 10, "gain": 1.5, "bias_max": 0.10, "bias_rpm": 1400.0, "bins": [0.0, 600.0, 1000.0, 1400.0, 1e9]},
        "rmse_targets": ["water_30", "water_120"],
        "hold_targets": ["water_120"],
        "required": ["fan"],
        "z": 1.96,
        "text": [
            "these criteria were fixed after the first real refusal of an earlier version, they are post hoc and were not pre-registered; the thresholds of this version were fixed before its first run",
            "ship only if all of the following hold, otherwise the artifact is written as candidate_* and not as model_deploy.json",
            "1. fan excitation: enough distinct duty levels held long enough in the step windows",
            "2. full-fan conductance A is estimated from the earlier half of the non-step history (steady 60 s blocks above 1400 rpm and 4000 pump rpm, slope of block heat minus C times the water slope against block water temperature, no step rows, only time before the first step window) and must lie in 20 to 28 W/K, otherwise refuse; the grey box holds A fixed at it in the full fit",
            "2b. grey-box fit of the thermal.md plant with the dead time as a parameter, a linear ambient drift and a constant unmodelled heat input q0 (0 to 120 W) on the rows that are finite, outside gaps and outside the hold-out spans: fit RMSE at most grey.fit_rmse_max = 0.12 K (absolute sanity limit)",
            "3. fan exponent m is positive with a one-sided 95 % likelihood-ratio bound (variance inflated for autocorrelation) against the nested model without fan response but with ambient drift (A free in it), and the conductance of the lowest held running duty level (its recorded rpm, not the 0 % test) is at least min_drop below full fan",
            "3b. fitted heat capacity C within 8 to 16 kJ/K (the independently known value is 11.7)",
            "4. check on fan phases 6 and 7, which stay out of the fit and the response term and run at constant full fan: the 120 s ahead forecast, re-initialised from the measured water every 10 s with both lumps set to the measured water, is strictly better than persistence, and the lower bound (2.5 %) of the RMSE ratio no-fan refit over full model, from a circular 240 s block bootstrap (seed 0, 5000 draws), is above 1.0; the no-fan reference refit has q0 and drift free, A free, C bounded to 8 to 16 kJ/K and Ta to 18 to 30 C. The forecast uses the logged power, pump rpm and fan rpm inside the horizon (the planner compares rpm choices under the same load, so this is a physics test, not a planner skill score); the variants with power held at the origin value and with power and pump held are reported, not gated. The free-run hold-out RMSE and the leave-one-fan-phase-out ranges of C, m, q0 and Ta (their fits include the hold-out rows) are reported, not gated",
            "5. water 30 s and 120 s RMSE of the model on the final fold (test, fresh2, held-out step rows) is not worse than the deployed model's by more than z times the block-bootstrap standard deviation of the paired difference, and on the held-out step rows the 120 s forecast of water is not worse than persistence by more than the same band",
            "6. on the later half of the non-step history (not used for A, no step rows) the 120 s re-initialised forecast has RMSE at least 1.5 times better than persistence and an absolute mean bias above 1400 rpm of at most 0.10 K; RMSE and bias per rpm bin (0-600, 600-1000, 1000-1400, 1400+) are reported",
            "the pump flag is not part of the gate: identified.pump is always false, the planner then does not use the model for the pump channel",
        ],
    },
    "plant": SIM["plant"],
    "sim": SIM,
    "dry": {"base": {"fan": {"fan": True, "drift": 0.0}, "nofan": {"fan": False, "drift": 0.0}, "drift_nofan": {"fan": False, "drift": 5.5}, "bump_nofan": {"fan": False, "drift": 5.5, "plant": {"drift_shape": "bump"}},
                     "drift_fan": {"fan": True, "drift": 5.5}, "alt_fan": {"fan": True, "drift": 0.0, "plant": ALT}, "alt_nofan": {"fan": False, "drift": 3.0, "plant": ALT},
                     "wrong_recirc12": {"fan": True, "drift": 0.0, "plant": WRONG["recirc12"]}, "wrong_recirc161": {"fan": True, "drift": 0.0, "plant": WRONG["recirc161"]}, "wrong_air": {"fan": True, "drift": 0.0, "plant": WRONG["air"]}},
            "starts": [30.0, 30.5, 31.0], "restart_gap": [120, 30], "grid": {"fan": "all", "nofan": [30.5], "drift_nofan": [30.5], "bump_nofan": [30.5], "alt_fan": [30.5], "alt_nofan": [30.5], "wrong_recirc12": [30.5], "wrong_recirc161": [30.5], "wrong_air": [30.5]},
            "seeds": 10, "need": 58, "protocol": "runbook", "factor": 1.5},
}


class Forest:
    def __init__(self, model):
        self.features = model["features"]
        self.identified = model.get("identified", {"fan": False, "pump": False})
        self.targets = {tg: (m["base"], [{k: np.array(t[k]) for k in "ftlrmv"} for t in m["trees"]]) for tg, m in model["targets"].items()}

    @staticmethod
    def walk(t, X):
        n = len(X)
        i = np.zeros(n, dtype=np.int64)
        rows = np.arange(n)
        thr = t["t"].astype(np.float32)
        while True:
            f = t["f"][i]
            leaf = f < 0
            if leaf.all():
                return t["v"][i]
            xv = X[rows, np.where(leaf, 0, f)]
            left = np.where(np.isnan(xv), t["m"][i] == 1, xv < thr[i])
            i = np.where(leaf, i, np.where(left, t["l"][i], t["r"][i]))

    def predict(self, X, targets=None):
        X = np.asarray(X, dtype=np.float32)
        out = {}
        for tg, (base, trees) in self.targets.items():
            if targets is None or tg in targets:
                s = np.full(len(X), base, dtype=np.float64)
                for t in trees:
                    s += self.walk(t, X)
                out[tg] = s
        return out




class LinearDeployed:
    def __init__(self, model):
        self.m = model
        self.features = model["features"]
        self.identified = model.get("identified", {"fan": False, "pump": False})
        self.targets = model["targets"]
        self.free = np.array([n in model.get("free", []) for n in self.features])

    @staticmethod
    def dot(t, V, clip, free):
        z = np.where(np.isfinite(V), (V - np.array(t["lin"]["mean"])) / np.array(t["lin"]["std"]), 0.0)
        return t["base"] + np.where(free, z, np.clip(z, -clip, clip)) @ np.array(t["lin"]["w"])

    def term(self, X, tg):
        r = self.m.get("response")
        series, h = tg.split("_")
        if not r or series not in r["gain"]:
            return 0.0
        f = self.features
        u = np.clip((X[:, f.index(r["pump_feat"])] - r["pump"][0]) / (r["pump"][1] - r["pump"][0]), 0.0, 1.0)
        dead = r["dead"][0] + (r["dead"][1] - r["dead"][0]) * u
        return r["gain"][series] * r["k"] * np.maximum(float(h) - dead, 0.0) * X[:, f.index(r["feat"])]

    def predict(self, X, targets=None):
        X = np.asarray(X, dtype=np.float64)
        m, clip = self.m, self.m["clip"]
        need = np.zeros(len(self.features), bool)
        for t in m["targets"].values():
            need |= np.array(t["lin"]["w"]) != 0.0
        primary = np.isfinite(X[:, need]).all(axis=1)
        fb = m.get("fallback")
        if fb:
            idx = [self.features.index(n) if n in self.features else -1 for n in fb["features"]]
            V = np.where(np.array(idx) >= 0, X[:, idx], np.nan)
        out = {}
        for tg in m["targets"]:
            if targets is not None and tg not in targets:
                continue
            p = self.dot(m["targets"][tg], X, clip, self.free)
            if fb and not primary.all():
                p = np.where(primary, p, self.dot(fb["targets"][tg], V, clip, self.free[idx]))
            out[tg] = p - self.term(X, tg)
        return out


def load_deployed(path):
    m = json.load(open(path))
    return LinearDeployed(m) if m.get("kind") == "linear" else Forest(m)


class GreyBox:
    NAMES = ["C", "A", "m", "gamma", "Ta", "tau", "f1", "K", "tau_hi", "drift", "q0"]
    LO = [3.0, 1.0, 0.0, -1.0, 10.0, 5.0, 0.3, 5.0, 3.0, -8.0, 0.0]
    HI = [40.0, 80.0, 3.5, 1.0, 30.0, 60.0, 1.0, 80.0, 60.0, 8.0, 120.0]
    X0 = [11.7, 20.0, 1.0, 0.0, 25.0, 20.0, 0.6, 24.0, 12.0, 0.0, 60.0]
    SCALE = [5.0, 10.0, 1.0, 0.2, 5.0, 10.0, 0.3, 10.0, 10.0, 2.0, 20.0]

    def __init__(self, cfg=CFG):
        p = cfg["plant"]
        self.full, self.pump_full = p["fan_full_rpm"], 4800.0
        self.pump_lo, self.pump_hi = p["pump_cal"][1][1], p["pump_cal"][-1][1]

    def conductance(self, th, f, p):
        return th[1] * np.power(np.maximum(f, 0.0) / self.full, th[2]) * (1.0 + th[3] * (p / self.pump_full - 1.0))

    def parts(self, th, s):
        C, A, m, gam, Ta, tau, f1, K, tau_hi, drift, q0 = th
        w, p = s["w"], s["p"]
        n = len(w)
        qd = np.interp(np.arange(n) - np.interp(p, [self.pump_lo, self.pump_hi], [tau, tau_hi]), np.arange(n), s["q"]) + q0
        return qd, self.conductance(th, s["f"], p), Ta + drift * s["t"], C * 1000.0 * f1, C * 1000.0 * (1.0 - f1) + 1e-9, K

    def ahead(self, th, s, h, step):
        return self.ahead_full(th, s, h, step)[:2]

    def ahead_full(self, th, s, h, step, tkey="hold", okey=None, hold_q=False, hold_p=False):
        C, A, m, gam, Ta, tau, f1, K, tau_hi, drift, q0 = th
        w, f, p, q = s["w"], s["f"], s["p"], s["q"]
        n = len(w)
        ar = np.arange(n)
        ta = Ta + drift * s["t"]
        c1, c2 = C * 1000.0 * f1, C * 1000.0 * (1.0 - f1) + 1e-9
        a = np.arange(0, n - h, step)
        a = a[s[tkey][a + h]]
        if okey:
            a = a[s[okey][a]]
        t = w[a].copy()
        u = t.copy()
        for j in range(h):
            i = a + j
            pj = p[a] if hold_p else p[i]
            x = i - np.interp(pj, [self.pump_lo, self.pump_hi], [tau, tau_hi])
            qd = np.interp(np.minimum(x, a) if hold_q else x, ar, q) + q0
            G = A * np.power(np.maximum(f[i], 0.0) / self.full, m) * (1.0 + gam * (pj / self.pump_full - 1.0))
            fl = K * (t - u)
            t = t + (qd - G * (t - ta[i]) - fl) / c1
            u = u + fl / c2
        return t - w[a + h], w[a] - w[a + h], a

    def simulate(self, th, s):
        qd, G, ta, c1, c2, K = (v.tolist() if isinstance(v, np.ndarray) else v for v in self.parts(th, s))
        w = s["w"]
        n = len(w)
        t1 = t2 = w[0]
        out = [0.0] * n
        for i in range(n):
            out[i] = t1
            f = K * (t1 - t2)
            t1 += (qd[i] - G[i] * (t1 - ta[i]) - f) / c1
            t2 += f / c2
        return np.array(out)

    def residual(self, th, segs, key):
        return np.nan_to_num(np.concatenate([(self.simulate(th, s) - s["w"])[s[key]] for s in segs]), nan=1e3, posinf=1e3, neginf=-1e3)

    def fit(self, segs, fixed=None, x0=None, key="fit", bounds=None):
        lo, hi, st = list(self.LO), list(self.HI), list(self.X0 if x0 is None else x0)
        for k, (a, b) in (bounds or {}).items():
            i = self.NAMES.index(k)
            lo[i], hi[i] = a, b
        for k, v in (fixed or {}).items():
            i = self.NAMES.index(k)
            lo[i], hi[i], st[i] = v, v + 1e-9, v
        st = list(np.clip(st, lo, hi))
        r = least_squares(lambda th: self.residual(th, segs, key), st, bounds=(lo, hi), x_scale=self.SCALE, ftol=1e-7, xtol=1e-7, max_nfev=60)
        self.info = {"nfev": int(r.nfev), "status": int(r.status)}
        return r.x, float(r.fun @ r.fun), len(r.fun)

    def rmse(self, th, segs, key):
        r = self.residual(th, segs, key)
        return float(np.sqrt(np.mean(r ** 2))) if len(r) else float("nan")

    def effect(self, th, w, p, lo_rpm, hi_rpm, horizon):
        g = lambda r: th[1] * (r / self.full) ** th[2] * (1.0 + th[3] * (p / self.pump_full - 1.0))
        return float(np.mean((g(hi_rpm) - g(lo_rpm)) * (w - th[4])) * horizon / (th[0] * 1000.0 * th[6]))


class Store:
    def __init__(self, cfg=CFG):
        self.cfg = cfg

    def load(self, channels):
        cfg = self.cfg
        want = list(dict.fromkeys(list(channels) + [cfg["phase_channel"]]))
        src = cfg.get("source", "history")
        meta, X = RawStore({"cache_dir": cfg["store_dir"], "chunk_s": 86400}).load() if src == "raw" else HistoryStore().load(want) if src == "history" else src.load(want)
        self.meta, self.k0 = meta, meta["k0"]
        ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
        have = [c for c in want if c in ix]
        self.X = np.asarray(X[:, [ix[c] for c in have]])
        self.ix = {c: i for i, c in enumerate(have)}
        self.T = self.X.shape[0]
        self.wins = self.windows()
        return self

    def col(self, c):
        return self.X[:, self.ix[c]].astype(np.float64)

    def windows(self):
        cfg, w = self.cfg, self.cfg["window"]
        if cfg["windows"]:
            return [(local_idx(self.meta, a) if isinstance(a, str) else int(a), local_idx(self.meta, b) if isinstance(b, str) else int(b)) for a, b in cfg["windows"]]
        if cfg["phase_channel"] not in self.ix:
            raise ValueError(f"no step test window: channel {cfg['phase_channel']} is not in the lake data and cfg windows is empty")
        s, e = Ops.runs(np.isfinite(self.X[:, self.ix[cfg["phase_channel"]]]))
        out = []
        for a, b in zip(s, e):
            if out and a - out[-1][1] <= w["merge_s"]:
                out[-1] = (out[-1][0], int(b))
            else:
                out.append((int(a), int(b)))
        if not out:
            raise ValueError("no step test window: the phase channel has no values")
        return out

    def spans(self):
        w, who, ph = self.cfg["window"], self.labels(), self.col(self.cfg["phase_channel"])
        out = []
        for i, (a, b) in enumerate(self.wins):
            pre = min(w["ctx_pre_s"], (a - self.wins[i - 1][1]) // 2) if i else w["ctx_pre_s"]
            post = min(w["ctx_post_s"], (self.wins[i + 1][0] - b) // 2) if i + 1 < len(self.wins) else w["post_s"]
            lo, hi = max(0, a - pre), min(self.T, b + post)
            hit = np.where(np.nan_to_num(ph[lo:hi], nan=-1.0) >= self.cfg["gate"]["grey"]["hold"][who[i]][0])[0]
            out.append((lo, lo + int(hit[0]) if len(hit) else hi, hi))
        return out

    def labels(self):
        out = []
        for a, b in self.wins:
            score = {}
            for c in ("fan", "pump"):
                d = self.col(f"d5next/{c}/duty")
                seg, pre = d[a:b], d[max(0, a - 120):a]
                score[c] = float(np.nanmax(seg) - np.nanmin(seg)) + (abs(float(np.nanmedian(seg)) - float(np.nanmedian(pre))) if np.isfinite(pre).any() else 0.0)
            out.append(max(score, key=score.get))
        return out


class Identifier:
    def __init__(self, cfg, st):
        self.cfg, self.st, self.g = cfg, st, cfg["gate"]
        self.gb = GreyBox(cfg)

    NAMES = ("d5next/water/temp", "d5next/fan/rpm", "d5next/pump/rpm", "/intelcpu/0/power/0", "nvml/0/power", "d5next/pump/power")

    def columns(self):
        st = self.st
        return {c: st.col(c) for c in self.NAMES + ("d5next/fan/duty", "d5next/pump/duty", self.cfg["phase_channel"])}

    def cut(self, cols, lo, hi):
        w = self.cfg["window"]
        bad = ~np.all([np.isfinite(cols[c][lo:hi]) for c in self.NAMES], axis=0)
        gs, ge = Ops.runs(bad)
        gap = np.zeros(hi - lo, bool)
        for x, y in zip(gs, ge):
            gap[x:y] |= y - x >= w["gap_s"]
        out = []
        for x, y in zip(*Ops.runs(~gap)):
            good = np.where(~bad[x:y])[0]
            if len(good) < w["piece_min_s"] or len(good) < 0.9 * (y - x):
                continue
            x, y = x + int(good[0]), x + int(good[-1]) + 1
            out.append((lo + x, lo + y, ~bad[x:y]))
        return out

    def segments(self):
        st = self.st
        who = st.labels()
        cols = self.columns()
        idx0 = st.wins[0][0]
        segs = []
        for i, (lo, hold, hi) in enumerate(st.spans()):
            for x, y, ok in self.cut(cols, lo, hi):
                segs.append(self.piece(cols, who[i], x, y, hold, st.wins[i], idx0, ok))
        return segs

    def history(self):
        st, hc = self.st, self.g["hist"]
        end = st.wins[0][0] - self.cfg["window"]["pre_s"]
        cols = self.columns()
        pieces = self.cut(cols, 0, end) if end > 0 else []
        if not pieces:
            return None
        mid = (pieces[0][0] + pieces[-1][1]) // 2
        sim = self.cfg.get("truth")
        if sim is not None:
            sc = dict(cols)
            ff = lambda v: Ops.ffill(v, 10 ** 9, False)[:end]
            fill = lambda v: np.where(np.isfinite(v), v, np.nanmedian(v) if np.isfinite(v).any() else 0.0)
            p = Plant(sim.c, True, 0.0).run(ff(cols["d5next/fan/duty"]), ff(cols["d5next/pump/duty"]), fill(ff(cols["/intelcpu/0/power/0"])), fill(ff(cols["nvml/0/power"])), np.random.default_rng(self.cfg["seed"]))
            for k, v in (("d5next/water/temp", "water"), ("d5next/fan/rpm", "fan_rpm"), ("d5next/pump/rpm", "pump_rpm"), ("d5next/pump/power", "pump_pow")):
                sc[k] = np.r_[p[v], np.full(len(cols[k]) - end, np.nan)]
            cols = sc
        segs = []
        for x, y, ok in pieces:
            s = self.piece(cols, "fan", x, y, y, st.wins[0], 0, ok)
            s["t"] = np.zeros(y - x)
            s["later"] = s["all"] & (np.arange(x, y) >= mid)
            s["early"] = s["all"] & (np.arange(x, y) < mid)
            segs.append(s)
        return segs

    def conductance(self, hs):
        hc, pc = self.g["hist"], self.cfg["plant"]
        c1, c2, kk, dead, B = pc["c1"], pc["c2"], pc["k"], pc["dead"][-1][1], int(hc["block_s"])
        xs, ys = [], []
        for s in hs:
            n = len(s["w"])
            qd = np.interp(np.arange(n) - dead, np.arange(n), s["q"])
            k = n // B
            if not k:
                continue
            sh = (k, B)
            r = lambda v: v[: k * B].reshape(sh)
            ok = (np.arange(k) * B >= hc["warm_s"]) & r(s["early"]).all(1) & (r(s["f"]).mean(1) >= hc["min_rpm"]) & (np.ptp(r(s["f"]), 1) <= hc["max_fan_range"]) & (r(s["p"]).mean(1) >= hc["min_pump_rpm"]) & (np.ptp(r(s["p"]), 1) <= hc["max_pump_range"])
            tt = np.arange(B) - (B - 1) / 2.0
            w = r(s["w"])
            lag = kk / c2
            t2 = lfilter([lag], [1.0, lag - 1.0], s["w"][:-1], zi=[(1.0 - lag) * s["w"][0]])[0]
            t2 = np.r_[s["w"][0], t2]
            slope = lambda v: (v * tt).sum(1) / (tt * tt).sum()
            xs.append(w.mean(1)[ok])
            ys.append((r(qd).mean(1) - c1 * slope(w) - c2 * slope(r(t2)))[ok])
        x, y = (np.concatenate(v) if v else np.zeros(0) for v in (xs, ys))
        if len(x) < hc["min_blocks"] or np.ptp(x) < hc["min_span_k"]:
            return {"a": None, "n": int(len(x)), "ok": False, "span_k": float(np.ptp(x)) if len(x) else 0.0}
        a = float(np.cov(x, y)[0, 1] / np.var(x, ddof=1))
        return {"a": a, "n": int(len(x)), "ok": bool(hc["a_range"][0] <= a <= hc["a_range"][1]), "span_k": float(np.ptp(x))}

    def piece(self, cols, who, lo, hi, hold, win, idx0, ok):
        sl = slice(lo, hi)
        keep = np.where(ok)[0]
        fill = lambda v: np.interp(np.arange(hi - lo), keep, v[sl][keep])
        late = np.arange(lo, hi) >= hold
        q = 0.96 * fill(cols["/intelcpu/0/power/0"]) + fill(cols["nvml/0/power"]) + fill(cols["d5next/pump/power"])
        return {"w": fill(cols["d5next/water/temp"]), "q": q, "f": fill(cols["d5next/fan/rpm"]), "p": fill(cols["d5next/pump/rpm"]), "t": (np.arange(lo, hi) - idx0) / 3600.0, "who": who,
                "fit": ~late & ok, "hold": late & ok, "all": ok.copy(), "a": win[0], "b": win[1], "lo": lo, "hi": hi, "phase": cols[self.cfg["phase_channel"]][sl], "duty": cols[f"d5next/{who}/duty"][sl]}

    def excitation(self, who):
        c = self.g[who]
        use = [s for s in self.segs if s["who"] == who]
        d, r = (np.concatenate([s[k][np.isfinite(s["phase"])] for s in use] or [np.zeros(0)]) for k in ("duty", "f"))
        keep = np.isfinite(d)
        d, r = np.round(d[keep] / 5.0) * 5.0, r[keep]
        u, n = np.unique(d, return_counts=True)
        held = u[n >= c["dwell_s"]]
        return {"levels_held": held.tolist(), "span": float(held.max() - held.min()) if len(held) else 0.0, "level_rpm": {str(float(l)): float(np.median(r[d == l])) for l in held}}

    def run(self):
        gc = self.g["grey"]
        gb = self.gb
        self.segs = segs = self.segments()
        if not segs:
            raise ValueError("no usable step window")
        fan_s = [s for s in segs if s["who"] == "fan"]
        hist = self.history()
        a_est = self.conductance(hist) if hist else {"a": None, "n": 0, "ok": False, "span_k": 0.0}
        a_est["source"] = "history"
        if self.cfg.get("a_fixed") is not None:
            a_est.update(a=float(self.cfg["a_fixed"]), ok=bool(self.g["hist"]["a_range"][0] <= self.cfg["a_fixed"] <= self.g["hist"]["a_range"][1]), source="fixed by cfg a_fixed")
        th, sse, n = gb.fit(segs, {"A": a_est["a"]} if a_est["a"] is not None else None)
        th0, sse0, _ = gb.fit(segs, {"m": 0.0}, th)
        thb, _, _ = gb.fit(segs, {"m": 0.0}, th, bounds=gc["null_bounds"])
        nfev_null = dict(gb.info)
        lr = lambda s0: float(n * math.log(max(s0, sse) / sse) / gc["se_inflate"] ** 2)
        out = {"theta_fit": dict(zip(gb.NAMES, th.tolist())), "fit_rmse_k": float(np.sqrt(sse / n)), "n": int(n), "lr_threshold": gc["z"] ** 2, "lr_m": lr(sse0), "theta_null": dict(zip(gb.NAMES, th0.tolist())), "theta_null_bounded": dict(zip(gb.NAMES, thb.tolist())), "null_bounded_fit": nfev_null, "a_est": a_est}
        ex = self.excitation("fan")
        low = [r for l, r in ex["level_rpm"].items() if float(l) > 0.0]
        out["lowest_rpm_ratio"] = min(low) / gb.full if low else None
        out["g_ratio_at_lowest"] = float(out["lowest_rpm_ratio"] ** th[2]) if low else None
        hs = [s for s in fan_s if s["hold"].any()]
        out["hold_fan"] = {"rmse_full": gb.rmse(th, hs, "hold") if hs else None, "rmse_null": gb.rmse(th0, hs, "hold") if hs else None, "rows": int(sum(s["hold"].sum() for s in hs)), "ahead": self.ahead(th, thb, hs)}
        out["hist"] = self.check(th, hist)
        if gc["loo"]:
            out["loo"] = self.loo(th, fan_s)
        out.update(fan=self.fan(out, th, fan_s, ex), pump=self.pump())
        out["response"] = self.response(out["theta_fit"])
        return out

    def ahead(self, th, thb, hs):
        a = self.g["grey"]["ahead"]
        rm = lambda v: float(np.sqrt(np.mean(v ** 2)))
        out = None
        for name, kw in (("logged", {}), ("held_power", {"hold_q": True}), ("held_power_pump", {"hold_q": True, "hold_p": True})):
            e = [(self.gb.ahead_full(th, s, a["h"], a["step"], **kw), self.gb.ahead_full(thb, s, a["h"], a["step"], **kw)[0]) for s in hs]
            if not hs or not sum(len(x[0][0]) for x in e):
                return None
            ef, pe, en = (np.concatenate(v) for v in ([x[0][0] for x in e], [x[0][1] for x in e], [x[1] for x in e]))
            ci, med = self.boot(ef, en)
            r = {"full": rm(ef), "null": rm(en), "persist": rm(pe), "n": int(len(ef)), "ratio": rm(en) / rm(ef), "ratio_ci": ci, "ratio_median": med}
            if out is None:
                out = dict(r, variants={})
            else:
                out["variants"][name] = r
        return out

    def boot(self, ef, en):
        b, n = self.g["grey"]["boot"], len(ef)
        L = max(1, int(round(b["block_s"] / self.g["grey"]["ahead"]["step"])))
        st = np.random.default_rng(b["seed"]).integers(0, n, (b["n"], -(-n // L)))
        idx = ((st[:, :, None] + np.arange(L)) % n).reshape(b["n"], -1)[:, :n]
        ratio = np.sqrt(np.mean(en[idx] ** 2, 1) / np.mean(ef[idx] ** 2, 1))
        return [float(np.quantile(ratio, b["q"])), float(np.quantile(ratio, 1.0 - b["q"]))], float(np.median(ratio))

    def check(self, th, hist):
        hc = self.g["hist"]
        if not hist:
            return None
        e = [self.gb.ahead_full(th, s, hc["h"], hc["step"], "all", "later") + (s,) for s in hist]
        if not sum(len(x[0]) for x in e):
            return None
        ef, pe = (np.concatenate([x[k] for x in e]) for k in (0, 1))
        rpm = np.concatenate([x[3]["f"][x[2]] for x in e])
        rm = lambda v: float(np.sqrt(np.mean(v ** 2))) if len(v) else None
        bins = {}
        for lo, hi in zip(hc["bins"][:-1], hc["bins"][1:]):
            m = (rpm >= lo) & (rpm < hi)
            bins[f"{lo:.0f}-{hi:.0f}" if hi < 1e8 else f"{lo:.0f}+"] = {"n": int(m.sum()), "rmse": rm(ef[m]), "persist": rm(pe[m]), "bias": float(np.mean(ef[m])) if m.any() else None}
        hi = rpm >= hc["bias_rpm"]
        return {"n": int(len(ef)), "rmse": rm(ef), "persist": rm(pe), "gain": rm(pe) / rm(ef), "bias_hi": float(np.mean(ef[hi])) if hi.any() else None, "bins": bins}

    def loo(self, th, fan_s):
        gb, out = self.gb, {"phases": {}}
        for ph in range(1, 8):
            for s in self.segs:
                out_ph = (s["who"] == "fan") & (s["phase"] == ph)
                s["lo_fit"], s["lo_hold"] = s["all"] & ~out_ph, s["all"] & out_ph
            t, _, _ = gb.fit(self.segs, {"A": th[1]}, th, key="lo_fit")
            ls = [s for s in fan_s if s["lo_hold"].any()]
            out["phases"][str(ph)] = {**{k: float(t[gb.NAMES.index(k)]) for k in ("C", "m", "q0", "Ta")}, "left_out_rmse": gb.rmse(t, ls, "lo_hold") if ls else None}
        out["range"] = {k: [min(v[k] for v in out["phases"].values()), max(v[k] for v in out["phases"].values())] for k in ("C", "m", "q0", "Ta")}
        return out

    def fan(self, o, th, ss, ex):
        c, gc = self.g["fan"], self.g["grey"]
        h = o["hold_fan"]
        why = []
        if not (len(ex["levels_held"]) >= c["min_levels"] and ex["span"] >= c["min_span"]):
            why.append("fan not excited enough")
        ae = o["a_est"]
        if not ae["ok"]:
            why.append("full-fan conductance estimate from history " + ("not available" if ae["a"] is None else f"{ae['a']:.1f} W/K") + f" (n {ae['n']}) outside {self.g['hist']['a_range'][0]} to {self.g['hist']['a_range'][1]} W/K")
        if not o["fit_rmse_k"] <= gc["fit_rmse_max"]:
            why.append(f"grey-box fit RMSE {o['fit_rmse_k']:.3f} K above {gc['fit_rmse_max']}")
        if not gc["c_range"][0] <= th[0] <= gc["c_range"][1]:
            why.append(f"fitted heat capacity {th[0]:.1f} kJ/K outside {gc['c_range'][0]} to {gc['c_range'][1]}")
        if not (th[2] > 0 and o["lr_m"] >= o["lr_threshold"]):
            why.append("fan exponent not significant against no fan response with ambient drift")
        if o["g_ratio_at_lowest"] is None or not o["g_ratio_at_lowest"] <= 1.0 - c["min_drop"]:
            why.append("conductance drop at the lowest level too small")
        a = h.get("ahead")
        if a is None:
            why.append("no hold-out fan phases")
        else:
            if not a["ratio_ci"][0] > 1.0:
                why.append(f"{gc['ahead']['h']} s forecast on the hold-out phases ({a['full']:.3f} K): lower bound of the no-fan/full RMSE ratio {a['ratio_ci'][0]:.2f} (ratio {a['ratio']:.2f}, no-fan {a['null']:.3f} K) not above 1.0")
            if not a["full"] < a["persist"]:
                why.append(f"{gc['ahead']['h']} s forecast on the hold-out phases ({a['full']:.3f} K) not better than persistence ({a['persist']:.3f} K)")
        hh, hc = o["hist"], self.g["hist"]
        if hh is None:
            why.append("history forecast: no later-half history")
        else:
            if not hh["persist"] >= hc["gain"] * hh["rmse"]:
                why.append(f"history forecast {hc['h']} s on the later half ({hh['rmse']:.3f} K) not {hc['gain']} times better than persistence ({hh['persist']:.3f} K)")
            if not (hh["bias_hi"] is not None and abs(hh["bias_hi"]) <= hc["bias_max"]):
                why.append(f"history forecast bias above {hc['bias_rpm']:.0f} rpm {'n/a' if hh['bias_hi'] is None else format(hh['bias_hi'], '.4f')} K exceeds {hc['bias_max']} K")
        hs = [s for s in ss if s["hold"].any()]
        eff = None
        if hs:
            w, p = np.concatenate([s["w"][s["hold"]] for s in hs]), np.concatenate([s["p"][s["hold"]] for s in hs])
            eff = self.gb.effect(th, w, p, c["grid_lo_duty"] * self.cfg["plant"]["fan_per_duty"], self.gb.full, gc["full_s"])
        tr, truth = self.cfg.get("truth"), None
        if tr is not None and hs:
            truth = tr.effect(w, p, c["grid_lo_duty"] * self.cfg["plant"]["fan_per_duty"], self.gb.full, gc["full_s"])
        return {"identified": not why, "reasons": why, "excitation": ex, "effect_k": eff, "truth_k": truth, "m": float(th[2])}

    def pump(self):
        return {"identified": False, "reasons": [self.g["pump"]["why"]], "excitation": self.excitation("pump")}

    def response(self, th):
        gb = self.gb
        return {"feat": "step.fx", "pump_feat": "pump_rpm", "k": th["A"] / (th["C"] * 1000.0 * th["f1"]), "m": th["m"], "gamma": th["gamma"], "ta": th["Ta"], "pump": [gb.pump_lo, gb.pump_hi],
                "dead": [th["tau"], th["tau_hi"]], "gain": {"water": 1.0}}


class Response:
    def __init__(self, r):
        self.r = r

    def nodes(self):
        r, full = self.r, CFG["plant"]["fan_full_rpm"]
        n = lambda name, op, ins, p, out=False: Node(name, op, ins, p, "step", out)
        return [n("step.fr", "scale", ["fan_rpm"], {"c": 1.0 / full}), n("step.fm", "pow", ["step.fr"], {"e": r["m"]}), n("step.pr", "scale", ["pump_rpm"], {"c": 1.0 / 4800.0}),
                n("step.pf", "lin", ["step.pr"], {"coef": [r["gamma"]], "off": 1.0 - r["gamma"]}), n("step.wt", "lin", ["water"], {"coef": [1.0], "off": -r["ta"]}),
                n("step.fp", "mul", ["step.fm", "step.pf"], {}), n("step.fx", "mul", ["step.fp", "step.wt"], {}, True)]

    def term(self, prpm, fx, tg):
        r = self.r
        series, h = tg.split("_")
        u = np.clip((prpm - r["pump"][0]) / (r["pump"][1] - r["pump"][0]), 0.0, 1.0)
        dead = r["dead"][0] + (r["dead"][1] - r["dead"][0]) * u
        return r["gain"][series] * r["k"] * np.maximum(float(h) - dead, 0.0) * fx

    def block(self):
        return {k: self.r[k] for k in ("feat", "pump_feat", "k", "pump", "dead", "gain")}


class Corpus:
    FANPUMP = ("ph.fx", "ph.gf", "ph.px", "ph.pg")

    def __init__(self, cfg):
        self.cfg, self.st = cfg, None
        self.rich = Spec.load(FCFG["spec_path"])
        ps = PhysSpec(self.rich, dict(PHYS, pint=True))
        self.full = ps.build()
        self.backbone = [n for n in dict.fromkeys(ps.names()) if not n.startswith(self.FANPUMP)]
        self.free = [n for n in self.backbone if n == "ph.pl" or n.endswith("*pl")]
        self.dep = load_deployed(cfg["deployed"])
        self.dspec = Spec.load(cfg["deployed_spec"])

    def build(self, resp):
        cfg, st = self.cfg, self.st
        spec = Spec(list(self.full.nodes) + resp.nodes(), dict(self.full.meta))
        self.need = list(dict.fromkeys(self.backbone + self.dep.features + ["step.fx", "pump_rpm"]))
        names, F, _ = FeatureFrame().compute(spec.subset(set(self.need)), st.X, st.ix, st.k0)
        self.col = {n: i for i, n in enumerate(names)}
        T, stride = st.T, cfg["stride"]
        self.rows = np.arange(0, T, stride)
        self.F = F[self.rows]
        dn, DF, _ = FeatureFrame().compute(self.dspec.subset(set(self.dep.features)), st.X, st.ix, st.k0)
        dc = {n: i for i, n in enumerate(dn)}
        self.D = DF[self.rows][:, [dc[n] for n in self.dep.features]]
        self.Y = {}
        for k, ch in cfg["series"].items():
            s = Ops.ffill(st.col(ch), FCFG["ffill_s"], False)
            for h in cfg["horizons"]:
                y = np.full(T, np.nan)
                y[: T - h] = s[h:] - s[: T - h]
                self.Y[f"{k}_{h}"] = y[self.rows]
        blocks = {n: np.zeros(len(self.rows), bool) for n in cfg["blocks"]}
        for n, spans in cfg["blocks"].items():
            for a, z in spans:
                blocks[n] |= (self.rows >= max(0, local_idx(st.meta, a))) & (self.rows < local_idx(st.meta, z))
        blocks.update(self.step_blocks(st, self.rows))
        self.blocks = blocks
        return self

    def step_blocks(self, st, rows):
        w = self.cfg["window"]
        out = {n: np.zeros(len(rows), bool) for n in ("step_train", "step_hold", "step_active")}
        train, zone = np.zeros(len(rows), bool), np.zeros(len(rows), bool)
        for (a, b), (lo, hold, hi) in zip(st.wins, st.spans()):
            train |= (rows >= max(0, a - w["pre_s"])) & (rows < min(st.T, b + w["post_s"]))
            if hold < hi:
                zone |= (rows >= hold - w["embargo_s"]) & (rows < hi)
                out["step_hold"] |= (rows >= hold) & (rows < hi)
            out["step_active"] |= (rows >= a) & (rows < b)
        out["step_train"] = train & ~zone
        return out

    def lhm_cols(self):
        b, tr = self.blocks["lhm_missing"], self.blocks["train"]
        if not b.any():
            return []
        j = [self.col[n] for n in self.backbone]
        miss = np.isfinite(self.F[b][:, j]).mean(0) < 0.05
        have = np.isfinite(self.F[tr][:, j]).mean(0) > 0.9
        return [n for n, m, h in zip(self.backbone, miss, have) if m and h]

    def idx(self, folds, tg):
        m = np.zeros(len(self.rows), bool)
        for b in folds:
            m |= self.blocks[b]
        return np.where(m & np.isfinite(self.Y[tg]) & np.isfinite(self.F[:, self.col["step.fx"]]))[0]


class Backbone:
    def __init__(self, cfg, co, resp):
        self.cfg, self.co, self.resp = cfg, co, resp

    def ytrain(self, rows, tg):
        co = self.co
        return co.Y[tg][rows].astype(np.float64) + self.resp.term(co.F[rows, co.col["pump_rpm"]].astype(np.float64), co.F[rows, co.col["step.fx"]].astype(np.float64), tg)

    def weights(self, rows):
        co, s = self.co, self.cfg["step_share"]
        st = co.blocks["step_train"][rows]
        if not st.any():
            return None
        return np.where(st, s / (1.0 - s) * (~st).sum() / st.sum(), 1.0)

    def grid(self, fam):
        g = self.cfg["families"][fam]
        return [dict(zip(g, v)) for v in itertools.product(*g.values())]

    def prepare(self, feats, rows, clip=LIN_CLIP):
        co = self.co
        cols = [co.col[n] for n in feats]
        free = np.array([n in co.free for n in feats])
        prep = Prep(cols).fit(co.F, rows)
        return prep, free

    def fit_one(self, fam, params, prep, free, rows, tg):
        co = self.co
        f = LinearFit(fam, params).fit(prep.transform(co.F, rows, LIN_CLIP, free), self.ytrain(rows, tg), self.weights(rows))
        return f

    def predict(self, f, prep, free, rows, tg):
        co = self.co
        return f.predict(prep.transform(co.F, rows, LIN_CLIP, free)) - (self.ytrain(rows, tg) - co.Y[tg][rows])

    def run(self, feats, targets):
        co, cfg = self.co, self.cfg
        fit_f, sel_f, fin_f = (cfg["folds"][k] for k in ("fit", "select", "final"))
        res = {"select": {}, "params": {}}
        for fam in cfg["families"]:
            res["select"][fam] = {}
            res["params"][fam] = {}
            for tg in targets:
                fr, sr = co.idx(fit_f, tg), co.idx(sel_f, tg)
                prep, free = self.prepare(feats, fr)
                best = None
                for p in self.grid(fam):
                    f = self.fit_one(fam, p, prep, free, fr, tg)
                    e = float(np.sqrt(np.mean((self.predict(f, prep, free, sr, tg) - co.Y[tg][sr]) ** 2)))
                    if best is None or e < best[0]:
                        best = (e, p)
                res["select"][fam][tg] = best[0]
                res["params"][fam][tg] = best[1]
        persist = {tg: float(np.sqrt(np.mean(co.Y[tg][co.idx(sel_f, tg)] ** 2))) for tg in targets}
        res["persist_select"] = persist
        res["score"] = {fam: float(np.mean([res["select"][fam][tg] / persist[tg] for tg in cfg["select_targets"]])) for fam in cfg["families"]}
        res["family"] = min(res["score"], key=res["score"].get)
        return res

    def final(self, fam, params, feats, targets, subset=None):
        co, cfg = self.co, self.cfg
        rows_f = cfg["folds"]["fit"] + cfg["folds"]["select"]
        out = {}
        for tg in targets:
            rows = co.idx(rows_f, tg)
            prep, free = self.prepare(feats, rows)
            f = self.fit_one(fam, params[tg], prep, free, rows, tg)
            out[tg] = (f, prep, free, rows)
        return out

    def model_block(self, fitted, feats, allfeats):
        co = self.co
        out = {}
        for tg, (f, prep, free, rows) in fitted.items():
            w, mean, std = np.zeros(len(allfeats)), np.zeros(len(allfeats)), np.ones(len(allfeats))
            for j, n in enumerate(feats):
                k = allfeats.index(n)
                w[k], mean[k], std[k] = f.w[j], prep.mean[j], prep.std[j]
            out[tg] = {"base": float(f.b), "lin": {"w": w.tolist(), "mean": mean.tolist(), "std": std.tolist()}}
        return out


class Trees:
    def __init__(self, cfg, co):
        self.cfg, self.co = cfg, co

    def run(self):
        import xgboost as xgb
        co, cfg, tc = self.co, self.cfg, self.cfg["trees"]
        fj = [co.col[n] for n in co.dep.features]
        out = {}
        for tg in tc["targets"]:
            fr, sr = co.idx(cfg["folds"]["fit"], tg), co.idx(cfg["folds"]["select"], tg)
            p = dict(tc["params"], nthread=cfg["threads"], seed=cfg["seed"])
            m = xgb.train(p, xgb.DMatrix(co.F[fr][:, fj], co.Y[tg][fr]), tc["rounds"], evals=[(xgb.DMatrix(co.F[sr][:, fj], co.Y[tg][sr]), "v")], early_stopping_rounds=tc["early"], verbose_eval=False)
            n = int(m.best_iteration) + 1
            sel = float(np.sqrt(np.mean((m.predict(xgb.DMatrix(co.F[sr][:, fj]), iteration_range=(0, n)) - co.Y[tg][sr]) ** 2)))
            ar = co.idx(cfg["folds"]["fit"] + cfg["folds"]["select"], tg)
            m2 = xgb.train(p, xgb.DMatrix(co.F[ar][:, fj], co.Y[tg][ar]), n)
            out[tg] = {"select_rmse": sel, "rounds": n, "model": m2}
        return out


class StepExporter(LinearExporter):
    def __init__(self, cfg, co, resp, feats, fb_feats):
        self.cfg = cfg["export"]
        self.co, self.resp, self.feats, self.fb_feats = co, resp, feats, fb_feats

    def spec(self):
        full = Spec(list(self.co.full.nodes) + self.resp.nodes(), dict(self.co.full.meta))
        sub = full.subset(set(self.model_features))
        sub.meta["deployed_outputs"] = self.model_features
        sub.save(self.cfg["spec_out"])
        return sub

    def build_model(self, flags, bb, main, fb):
        co = self.co
        allf = list(dict.fromkeys(self.feats + ["step.fx", "pump_rpm"]))
        self.model_features = allf
        out = {"kind": "linear", "features": allf, "identified": flags, "clip": LIN_CLIP, "free": [n for n in allf if n in co.free], "response": self.resp.block(),
               "targets": bb.model_block(main, self.feats, allf), "fallback": {"features": self.fb_feats, "targets": bb.model_block(fb, self.fb_feats, self.fb_feats)}}
        with open(self.cfg["model_out"], "w") as f:
            json.dump(out, f, separators=(",", ":"))
        self.out = out
        return out

    def run(self):
        spec = self.spec()
        gold = self.golden(spec, self.out) if self.cfg.get("golden_out") else {}
        return {"nodes": len(spec.nodes), "raw": len(spec.raw_channels()), **gold}


class Evaluator:
    def __init__(self, cfg, co):
        self.cfg, self.co = cfg, co

    def paired(self, rows, tg, preds, base):
        co = self.co
        y = co.Y[tg][rows].astype(np.float64)
        errs = {k: y - p for k, p in preds.items()}
        errs["persist"] = y.copy()
        t = co.rows[rows]
        b = self.cfg["boot"]
        bs = Metrics({"boot_block_s": b["block_s"], "boot_n": b["n"]}, seed=1).boot(t, errs)
        rm = {k: float(np.sqrt(np.mean(e ** 2))) for k, e in errs.items()}
        z = self.cfg["gate"]["z"]
        out = {"n": int(len(y)), "rmse": rm}
        for ref in base:
            d = bs["new"] - bs[ref]
            sd = float(np.std(d))
            out[f"vs_{ref}"] = {"diff": rm["new"] - rm[ref], "band": z * sd, "ok": bool(rm["new"] - rm[ref] <= z * sd), "ci": [float(np.quantile(d, 0.025)), float(np.quantile(d, 0.975))]}
        return out

    def run(self, bb, fitted, others, trees, targets):
        co, cfg = self.co, self.cfg
        fj = [co.col[n] for n in co.dep.features]
        out = {"final": {}, "step_hold": {}, "trees": {}}
        for tg in targets:
            for name, folds in (("final", cfg["folds"]["final"]), ("step_hold", ["step_hold"])):
                rows = co.idx(folds, tg)
                if len(rows) < 50:
                    continue
                f, prep, free, _ = fitted[tg]
                preds = {"new": bb.predict(f, prep, free, rows, tg)}
                for fam, fit in others.items():
                    g, pr, fr, _ = fit[tg]
                    preds[fam] = bb.predict(g, pr, fr, rows, tg)
                base = ["persist"]
                if tg in co.dep.targets:
                    preds["deployed"] = co.dep.predict(co.D[rows], [tg])[tg]
                    base.append("deployed")
                if name == "final" and tg in trees:
                    import xgboost as xgb
                    preds["trees"] = trees[tg]["model"].predict(xgb.DMatrix(co.F[rows][:, fj]))
                    base.append("trees")
                out[name][tg] = self.paired(rows, tg, preds, base)
        return out


class StepTrainer:
    def __init__(self, cfg=CFG):
        self.cfg = cfg
        self.out = cfg["out_dir"]

    def clear(self):
        for pre in ("", "candidate_"):
            for n in ("model_deploy.json", "feature_spec_deploy.json", "golden_deploy.json"):
                if os.path.exists(os.path.join(self.out, pre + n)):
                    os.remove(os.path.join(self.out, pre + n))

    def gate(self, ident, ev):
        g = self.cfg["gate"]
        flags = {c: bool(ident[c]["identified"]) for c in ("fan", "pump")}
        reasons = [f"{c} response not identified: " + "; ".join(ident[c]["reasons"]) for c in g["required"] if not flags[c]]
        checks = {}
        for tg in g["rmse_targets"]:
            v = ev["final"][tg]["vs_deployed"]
            checks[f"{tg}|deployed"] = v
            if not v["ok"]:
                reasons.append(f"{tg} final-fold RMSE {ev['final'][tg]['rmse']['new']:.4f} worse than deployed {ev['final'][tg]['rmse']['deployed']:.4f} beyond the noise band {v['band']:.4f}")
        for tg in g["hold_targets"]:
            if tg in ev["step_hold"]:
                v = ev["step_hold"][tg]["vs_persist"]
                checks[f"{tg}|step_hold|persist"] = v
                if not v["ok"]:
                    reasons.append(f"{tg} on held-out step rows worse than persistence beyond the noise band")
        return {"ship": not reasons, "reasons": reasons, "identified": flags, "weak": {"pump": not flags["pump"]}, "checks": checks, "criteria": g["text"]}

    def run(self):
        t0 = time.time()
        cfg = self.cfg
        co = Corpus(cfg)
        st = Store(cfg).load(co.full.raw_channels())
        co.st = st
        ident = Identifier(cfg, st).run()
        resp = Response(ident["response"])
        co.build(resp)
        targets = [f"{s}_{h}" for s in cfg["series"] for h in cfg["horizons"]]
        bb = Backbone(cfg, co, resp)
        sel = bb.run(co.backbone, targets)
        fam = sel["family"]
        fitted = {f: bb.final(f, sel["params"][f], co.backbone, targets) for f in cfg["families"]}
        lhm = set(co.lhm_cols())
        fbf = [n for n in co.backbone if n not in lhm]
        fb = bb.final(fam, sel["params"][fam], fbf, targets)
        trees = Trees(cfg, co).run()
        ev = Evaluator(cfg, co).run(bb, fitted[fam], {f: v for f, v in fitted.items() if f != fam}, trees, targets)
        verdict = self.gate(ident, ev)
        name = "" if verdict["ship"] else "candidate_"
        os.makedirs(self.out, exist_ok=True)
        ex = dict(cfg.get("export", {}), spec_out=os.path.join(self.out, f"{name}feature_spec_deploy.json"), model_out=os.path.join(self.out, f"{name}model_deploy.json"), store={"cache_dir": cfg["store_dir"], "chunk_s": 86400})
        if cfg.get("golden"):
            ex.update(golden_out=os.path.join(self.out, f"{name}golden_deploy.json"), golden_span=("2026-11-04 07:00", "2026-11-04 09:00"), golden_every=10)
        sx = StepExporter(dict(cfg, export=ex), co, resp, co.backbone, fbf)
        self.clear()
        try:
            model = sx.build_model(verdict["identified"], bb, fitted[fam], fb)
            gold = sx.run()
        except BaseException:
            self.clear()
            raise
        report = {"gate": verdict, "identification": ident, "selection": {k: v for k, v in sel.items()}, "trees_select": {t: {"select_rmse": v["select_rmse"], "rounds": v["rounds"]} for t, v in trees.items()},
                  "evaluation": ev, "clipped": self.clipped(bb, fitted[fam], targets), "family": fam, "windows": [[int(a), int(b)] for a, b in st.wins], "hold_rows": int(co.blocks["step_hold"].sum()), "overlap_rows": int((co.blocks["step_hold"] & co.blocks["step_train"]).sum()), "golden": gold, "seconds": round(time.time() - t0)}
        with open(os.path.join(self.out, "report.json"), "w") as f:
            json.dump(report, f, indent=1, default=float)
        return report

    def clipped(self, bb, fitted, targets):
        co, out = bb.co, {}
        for tg in ("water_30", "water_120"):
            f, prep, free, _ = fitted[tg]
            out[tg] = {b: prep.clipped(co.F, np.where(co.blocks[b])[0], LIN_CLIP, free) for b in ("fresh1", "fresh2", "step_hold", "oor_pump20") if co.blocks[b].any()}
        return out


def train_task(cfg):
    return StepTrainer(cfg).run()


def dry_cases(d):
    out = {}
    for name, starts in d["grid"].items():
        for st in d["starts"] if starts == "all" else starts:
            for gap in (False, True):
                spec = copy.deepcopy(d["base"][name])
                spec["plant"] = dict(spec.get("plant", {}), start_water=st)
                spec["inject"] = {"restart_gap": d["restart_gap"] if gap else None}
                out[f"{name}_{st}" + ("_gap" if gap else "")] = spec
    return out


def dry_task(job):
    cfg, case, seed, root = job["cfg"], job["case"], job["seed"], job["root"]
    spec = {**cfg["dry"]["base"], **dry_cases(cfg["dry"])}[case]
    fr, drift = spec["fan"], spec["drift"]
    sim = copy.deepcopy(cfg["sim"])
    sim["plant"].update(spec.get("plant", {}))
    sim["inject"].update(spec.get("inject", {}))
    dest = os.path.join(root, f"{case}-{seed}")
    info = Injector(cfg, sim).build(cfg["dry"]["protocol"], fr, os.path.join(dest, "store"), seed, drift)
    truth = Plant(sim["plant"], fr, drift)
    c = dict(cfg, source="raw", store_dir=os.path.join(dest, "store"), out_dir=os.path.join(dest, "out"), truth=truth, seed=seed, gate=dict(cfg["gate"], grey=dict(cfg["gate"]["grey"], loo=False)))
    try:
        rep = StepTrainer(c).run()
    finally:
        shutil.rmtree(os.path.join(dest, "store"), ignore_errors=True)
    f = rep["identification"]["fan"]
    ratio = f["effect_k"] / f["truth_k"] if f.get("truth_k") else None
    return {"case": case, "seed": seed, "ship": rep["gate"]["ship"], "identified": rep["gate"]["identified"], "reasons": rep["gate"]["reasons"], "fan_effect_k": f["effect_k"], "truth_k": f.get("truth_k"), "ratio": ratio,
            "m": f["m"], "gamma": rep["identification"]["theta_fit"]["gamma"], "q0": rep["identification"]["theta_fit"]["q0"], "c": rep["identification"]["theta_fit"]["C"], "fit_rmse": rep["identification"]["fit_rmse_k"], "hold": rep["identification"]["hold_fan"], "family": rep["family"], "seconds": rep["seconds"], "water_max": info["water_max"],
            "fan_reasons": f["reasons"], "a_est": rep["identification"]["a_est"], "hist": rep["identification"]["hist"]}


class Runner:
    def __init__(self, cfg=CFG):
        self.cfg = cfg

    def map(self, fn, jobs):
        for k in ("OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS"):
            os.environ[k] = str(self.cfg["blas_threads"])
        if self.cfg["workers"] <= 1 or len(jobs) <= 1:
            return [fn(j) for j in jobs]
        import multiprocessing as mp
        with mp.get_context("spawn").Pool(min(self.cfg["workers"], len(jobs))) as p:
            return p.map(fn, jobs, chunksize=1)


class DryRun:
    def __init__(self, cfg=CFG):
        self.cfg = cfg

    def run(self, root=None, cases=None, seeds=None):
        d = self.cfg["dry"]
        root = root or tempfile.mkdtemp(prefix="steptrain-dry-")
        jobs = [{"cfg": {k: v for k, v in self.cfg.items() if k != "truth"}, "case": c, "seed": s, "root": root} for c in (cases or list(dry_cases(d))) for s in range(seeds or d["seeds"])]
        res = Runner(self.cfg).map(dry_task, jobs)
        with open(os.path.join(root, "dryrun.json"), "w") as f:
            json.dump(res, f, indent=1, default=float)
        return root, res

    @staticmethod
    def summary(res, factor):
        out = {}
        rng = lambda v: [min(v), max(v)] if v else None
        for case in dict.fromkeys(r["case"] for r in res):
            rs = [r for r in res if r["case"] == case]
            ok = [bool(r["ratio"] is not None and 1.0 / factor <= r["ratio"] <= factor) for r in rs]
            out[case] = {"n": len(rs), "ship": sum(r["ship"] for r in rs), "ship_ok": sum(bool(r["ship"] and o) for r, o in zip(rs, ok)), "fan_id": sum(r["identified"]["fan"] for r in rs), "ratio_ok": sum(ok),
                         "fit": rng([r["fit_rmse"] for r in rs]), "hold": rng([r["hold"]["rmse_full"] for r in rs if r["hold"]["rmse_full"] is not None]), "ratio": rng([r["ratio"] for r in rs if r["ratio"]]),
                         "q0": rng([r["q0"] for r in rs]), "c": rng([r["c"] for r in rs]), "ship_wo6": sum(bool(r["ship"] or (r["fan_reasons"] and all(x.startswith("history forecast") for x in r["fan_reasons"]) and len(r["reasons"]) == 1)) for r in rs),
                         "a_est": rng([r["a_est"]["a"] for r in rs if r["a_est"]["a"] is not None]), "hist_gain": rng([r["hist"]["gain"] for r in rs if r["hist"]]), "hist_bias_hi": rng([r["hist"]["bias_hi"] for r in rs if r["hist"] and r["hist"]["bias_hi"] is not None]), "ratio_ci_lo": rng([r["hold"]["ahead"]["ratio_ci"][0] for r in rs if r["hold"].get("ahead")]), "ahead_gain": rng([r["hold"]["ahead"]["null"] / r["hold"]["ahead"]["full"] for r in rs if r["hold"].get("ahead")])}
        return out

    @staticmethod
    def verdict(summary, need=58):
        out = {}
        fan = [s for c, s in summary.items() if c.startswith("fan")]
        for c, s in summary.items():
            if c.startswith(("nofan", "drift", "bump", "alt_nofan")):
                out[c] = s["ship"] == 0
            elif c.startswith("alt_fan"):
                out[c] = s["ship"] == s["ship_ok"]
        if fan:
            out["fan"] = sum(s["ship_ok"] for s in fan) >= min(need, sum(s["n"] for s in fan))
        return out


def main(argv, cfg=CFG):
    if argv and argv[0] == "dryrun":
        cfg = dict(cfg, store_dir=FRESH["store_dir"])
        if len(argv) > 4:
            cfg["a_fixed"] = float(argv[4])
        cases = argv[2].split(",") if len(argv) > 2 else None
        root, res = DryRun(cfg).run(argv[1] if len(argv) > 1 else None, cases, int(argv[3]) if len(argv) > 3 else None)
        sm = DryRun.summary(res, cfg["dry"]["factor"])
        out = {"root": root, "summary": sm, "criteria": DryRun.verdict(sm, cfg["dry"]["need"])}
    else:
        out = train_task(cfg)["gate"]
    print(json.dumps(out, indent=1))
    return out


if __name__ == "__main__":
    main(sys.argv[1:])
