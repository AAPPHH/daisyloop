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
from export import LinearExporter
from features import FeatureFrame, Node, Ops, PHYS, PhysSpec, Spec, CONFIG as FCFG
from models import CONFIG as MCFG, FRESH, LIN_CLIP, LinearFit, Metrics, PINT, Prep, local_idx
from rawstore import RawStore
from stepsim import Injector, Plant, SIM

HERE = os.path.dirname(os.path.abspath(__file__))
MODELS = os.path.join(HERE, "..", "models")
ALT = {"m": 1.2, "a": 30.0, "c1": 8500.0, "k": 15.0, "gamma": 0.1, "ta": 22.0, "spool_s": 8.0}
CFG = {
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
    "series": MCFG["series"],
    "horizons": [30, 60, 90, 120],
    "seed": 0,
    "threads": 1,
    "step_share": 0.05,
    "families": {"ridge": {"alpha": [100.0, 1000.0, 10000.0, 100000.0]}, "svr": {"C": [0.0001, 0.0003, 0.001], "epsilon": [0.01]}},
    "select_targets": ["water_30", "water_60", "water_120"],
    "trees": {"targets": ["water_30", "water_60", "water_120"], "rounds": 600, "early": 40, "params": {"eta": 0.03, "max_depth": 3, "min_child_weight": 100, "subsample": 0.7, "colsample_bytree": 0.5, "lambda": 10.0, "tree_method": "hist", "objective": "reg:squarederror"}},
    "die_gain": 1.0,
    "boot": {"block_s": 600, "n": 400},
    "workers": 5,
    "blas_threads": 1,
    "gate": {
        "grey": {"hold": {"fan": [6, 7], "pump": [4, 5]}, "se_inflate": 10.0, "z": 1.96, "hold_gain": 1.5, "full_s": 120.0,
                 "band": {"margin": 1.25, "fit_max": 0.0514, "hold_max": 0.0877, "runs": 60}},
        "fan": {"grid_lo_duty": 27.0, "min_levels": 3, "min_span": 40.0, "min_drop": 0.25, "dwell_s": 120},
        "pump": {"dwell_s": 120, "why": "pump coefficient not identified: the protocol does not separate it from pump heat and dead time (dry run: truth 0 gives a fitted 0.14, flag fired in 2 of 5 runs only at truth 0.6), the pump stays on the firmware PID"},
        "rmse_targets": ["water_30", "water_120"],
        "hold_targets": ["water_120", "cpu_120", "gpu_120"],
        "required": ["fan"],
        "z": 1.96,
        "text": [
            "ship only if all of the following hold, otherwise the artifact is written as candidate_* and not as model_deploy.json",
            "1. fan excitation: enough distinct duty levels held long enough in the step windows",
            "2. grey-box fit of the thermal.md plant with the dead time as a parameter and a linear ambient drift on the rows that are finite, outside gaps and outside the hold-out spans: fit RMSE at most grey.fit_rmse_max, the largest RMSE of the correct plant over the dry-run seeds (grey.band) times grey.band.margin",
            "3. fan exponent m is positive with a one-sided 95 % likelihood-ratio bound (variance inflated for autocorrelation) against the nested model without fan response but with ambient drift, and the conductance of the lowest held running duty level (its recorded rpm, not the 0 % test) is at least min_drop below full fan",
            "4. independent check: phases not used in the fit (fan phases 6 and 7, pump phases 4 and 5 and what follows) are predicted with RMSE at most grey.hold_rmse_max (the same band rule) and at least hold_gain times better than by the nested no-fan-plus-drift model refitted on the same fit phases",
            "5. water 30 s and 120 s RMSE of the model on the final fold (test, fresh2, held-out step rows) is not worse than the deployed model's by more than z times the block-bootstrap standard deviation of the paired difference, and on the held-out step rows the 120 s forecasts of water, cpu and gpu are not worse than persistence by more than the same band",
            "the pump flag is not part of the gate: identified.pump is always false, the planner then does not use the model for the pump channel",
        ],
    },
    "plant": SIM["plant"],
    "sim": SIM,
    "dry": {"base": {"fan": {"fan": True, "drift": 0.0}, "nofan": {"fan": False, "drift": 0.0}, "drift_nofan": {"fan": False, "drift": 5.5}, "bump_nofan": {"fan": False, "drift": 5.5, "plant": {"drift_shape": "bump"}},
                     "drift_fan": {"fan": True, "drift": 5.5}, "alt_fan": {"fan": True, "drift": 0.0, "plant": ALT}, "alt_nofan": {"fan": False, "drift": 3.0, "plant": ALT}},
            "starts": [30.0, 30.5, 31.0], "restart_gap": [120, 30], "grid": {"fan": "all", "nofan": [30.5], "drift_nofan": [30.5], "bump_nofan": [30.5], "alt_fan": [30.5], "alt_nofan": [30.5]},
            "seeds": 10, "need": 9, "protocol": "runbook", "factor": 1.5},
}

for _k in ("fit", "hold"):
    CFG["gate"]["grey"][f"{_k}_rmse_max"] = round(CFG["gate"]["grey"]["band"]["margin"] * CFG["gate"]["grey"]["band"][f"{_k}_max"], 3)


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




class GreyBox:
    NAMES = ["C", "A", "m", "gamma", "Ta", "tau", "f1", "K", "tau_hi", "drift"]
    LO = [3.0, 1.0, 0.0, -1.0, 10.0, 5.0, 0.3, 5.0, 3.0, -8.0]
    HI = [40.0, 80.0, 3.5, 1.0, 30.0, 60.0, 1.0, 80.0, 60.0, 8.0]
    X0 = [11.7, 20.0, 1.0, 0.0, 25.0, 20.0, 0.6, 24.0, 12.0, 0.0]
    SCALE = [5.0, 10.0, 1.0, 0.2, 5.0, 10.0, 0.3, 10.0, 10.0, 2.0]

    def __init__(self, cfg=CFG):
        p = cfg["plant"]
        self.full, self.pump_full = p["fan_full_rpm"], 4800.0
        self.pump_lo, self.pump_hi = p["pump_cal"][1][1], p["pump_cal"][-1][1]

    def conductance(self, th, f, p):
        return th[1] * np.power(np.maximum(f, 0.0) / self.full, th[2]) * (1.0 + th[3] * (p / self.pump_full - 1.0))

    def simulate(self, th, s):
        C, A, m, gam, Ta, tau, f1, K, tau_hi, drift = th
        w, p = s["w"], s["p"]
        n = len(w)
        qd = np.interp(np.arange(n) - np.interp(p, [self.pump_lo, self.pump_hi], [tau, tau_hi]), np.arange(n), s["q"]).tolist()
        G = self.conductance(th, s["f"], p).tolist()
        ta = (Ta + drift * s["t"]).tolist()
        c1, c2 = C * 1000.0 * f1, C * 1000.0 * (1.0 - f1) + 1e-9
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

    def fit(self, segs, fixed=None, x0=None, key="fit"):
        lo, hi, st = list(self.LO), list(self.HI), list(self.X0 if x0 is None else x0)
        for k, v in (fixed or {}).items():
            i = self.NAMES.index(k)
            lo[i], hi[i], st[i] = v, v + 1e-9, v
        r = least_squares(lambda th: self.residual(th, segs, key), st, bounds=(lo, hi), x_scale=self.SCALE, ftol=1e-7, xtol=1e-7, max_nfev=60)
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
        meta, X = RawStore({"cache_dir": cfg["store_dir"], "chunk_s": 86400}).load()
        self.meta, self.k0 = meta, meta["k0"]
        ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
        have = [c for c in dict.fromkeys(list(channels) + [cfg["phase_channel"]]) if c in ix]
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

    def segments(self):
        st, w = self.st, self.cfg["window"]
        who = st.labels()
        names = ("d5next/water/temp", "d5next/fan/rpm", "d5next/pump/rpm", "/intelcpu/0/power/0", "nvml/0/power", "d5next/pump/power")
        cols = {c: st.col(c) for c in names + ("d5next/fan/duty", "d5next/pump/duty", self.cfg["phase_channel"])}
        idx0 = st.wins[0][0]
        segs = []
        for i, (lo, hold, hi) in enumerate(st.spans()):
            bad = ~np.all([np.isfinite(cols[c][lo:hi]) for c in names], axis=0)
            gs, ge = Ops.runs(bad)
            gap = np.zeros(hi - lo, bool)
            for x, y in zip(gs, ge):
                gap[x:y] |= y - x >= w["gap_s"]
            ps, pe = Ops.runs(~gap)
            for x, y in zip(ps, pe):
                good = np.where(~bad[x:y])[0]
                if len(good) < w["piece_min_s"] or len(good) < 0.9 * (y - x):
                    continue
                x, y = x + int(good[0]), x + int(good[-1]) + 1
                segs.append(self.piece(cols, who[i], lo + x, lo + y, hold, st.wins[i], idx0, ~bad[x:y]))
        return segs

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
        th, sse, n = gb.fit(segs)
        th0, sse0, _ = gb.fit(segs, {"m": 0.0}, th)
        lr = lambda s0: float(n * math.log(max(s0, sse) / sse) / gc["se_inflate"] ** 2)
        out = {"theta_fit": dict(zip(gb.NAMES, th.tolist())), "fit_rmse_k": float(np.sqrt(sse / n)), "n": int(n), "lr_threshold": gc["z"] ** 2, "lr_m": lr(sse0), "theta_null": dict(zip(gb.NAMES, th0.tolist()))}
        ex = self.excitation("fan")
        low = [r for l, r in ex["level_rpm"].items() if float(l) > 0.0]
        out["lowest_rpm_ratio"] = min(low) / gb.full if low else None
        out["g_ratio_at_lowest"] = float(out["lowest_rpm_ratio"] ** th[2]) if low else None
        hs = [s for s in fan_s if s["hold"].any()]
        out["hold_fan"] = {"rmse_full": gb.rmse(th, hs, "hold") if hs else None, "rmse_null": gb.rmse(th0, hs, "hold") if hs else None, "rows": int(sum(s["hold"].sum() for s in hs))}
        out.update(fan=self.fan(out, th, fan_s, ex), pump=self.pump())
        out["response"] = self.response(out["theta_fit"])
        return out

    def fan(self, o, th, ss, ex):
        c, gc = self.g["fan"], self.g["grey"]
        h = o["hold_fan"]
        why = []
        if not (len(ex["levels_held"]) >= c["min_levels"] and ex["span"] >= c["min_span"]):
            why.append("fan not excited enough")
        if not o["fit_rmse_k"] <= gc["fit_rmse_max"]:
            why.append(f"grey-box fit RMSE {o['fit_rmse_k']:.3f} K above {gc['fit_rmse_max']}")
        if not (th[2] > 0 and o["lr_m"] >= o["lr_threshold"]):
            why.append("fan exponent not significant against no fan response with ambient drift")
        if o["g_ratio_at_lowest"] is None or not o["g_ratio_at_lowest"] <= 1.0 - c["min_drop"]:
            why.append("conductance drop at the lowest level too small")
        if h["rmse_full"] is None:
            why.append("no hold-out fan phases")
        else:
            if not h["rmse_full"] <= gc["hold_rmse_max"]:
                why.append(f"hold-out RMSE {h['rmse_full']:.3f} K above {gc['hold_rmse_max']}")
            if not h["rmse_null"] >= gc["hold_gain"] * h["rmse_full"]:
                why.append("hold-out not predicted better than by no fan response with drift")
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
                "dead": [th["tau"], th["tau_hi"]], "gain": {"water": 1.0, "cpu": self.cfg["die_gain"], "gpu": self.cfg["die_gain"]}}


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
        self.dep = Forest(json.load(open(cfg["deployed"])))

    def build(self, resp):
        cfg, st = self.cfg, self.st
        spec = Spec(list(self.full.nodes) + resp.nodes(), dict(self.full.meta))
        self.need = list(dict.fromkeys(self.backbone + self.dep.features + ["step.fx", "pump_rpm"]))
        names, F, _ = FeatureFrame().compute(spec.subset(set(self.need)), st.X, st.ix, st.k0)
        self.col = {n: i for i, n in enumerate(names)}
        T, stride = st.T, cfg["stride"]
        self.rows = np.arange(0, T, stride)
        self.F = F[self.rows]
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
                    preds["deployed"] = co.dep.predict(co.F[rows][:, fj], [tg])[tg]
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
    c = dict(cfg, store_dir=os.path.join(dest, "store"), out_dir=os.path.join(dest, "out"), truth=truth, seed=seed)
    try:
        rep = StepTrainer(c).run()
    finally:
        shutil.rmtree(os.path.join(dest, "store"), ignore_errors=True)
    f = rep["identification"]["fan"]
    ratio = f["effect_k"] / f["truth_k"] if f.get("truth_k") else None
    return {"case": case, "seed": seed, "ship": rep["gate"]["ship"], "identified": rep["gate"]["identified"], "reasons": rep["gate"]["reasons"], "fan_effect_k": f["effect_k"], "truth_k": f.get("truth_k"), "ratio": ratio,
            "m": f["m"], "gamma": rep["identification"]["theta_fit"]["gamma"], "fit_rmse": rep["identification"]["fit_rmse_k"], "hold": rep["identification"]["hold_fan"], "family": rep["family"], "seconds": rep["seconds"], "water_max": info["water_max"]}


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
                         "fit": rng([r["fit_rmse"] for r in rs]), "hold": rng([r["hold"]["rmse_full"] for r in rs if r["hold"]["rmse_full"] is not None]), "ratio": rng([r["ratio"] for r in rs if r["ratio"]])}
        return out

    @staticmethod
    def band(res):
        fan = [r for r in res if r["case"].startswith("fan_")]
        return {"fit_max": max(r["fit_rmse"] for r in fan), "hold_max": max(r["hold"]["rmse_full"] for r in fan), "runs": len(fan)} if fan else None

    @staticmethod
    def verdict(summary, need=9):
        out = {}
        for c, s in summary.items():
            if c.startswith(("nofan", "drift", "bump", "alt_nofan")):
                out[c] = s["ship"] == 0
            elif c.startswith("alt_fan"):
                out[c] = s["ship"] == s["ship_ok"]
            else:
                out[c] = s["ship_ok"] >= min(need, s["n"])
        return out


def main(argv, cfg=CFG):
    if argv and argv[0] == "dryrun":
        cfg = dict(cfg, store_dir=FRESH["store_dir"])
        cases = argv[2].split(",") if len(argv) > 2 else None
        root, res = DryRun(cfg).run(argv[1] if len(argv) > 1 else None, cases, int(argv[3]) if len(argv) > 3 else None)
        sm = DryRun.summary(res, cfg["dry"]["factor"])
        out = {"root": root, "band": DryRun.band(res), "summary": sm, "criteria": DryRun.verdict(sm, cfg["dry"]["need"])}
    else:
        out = train_task(cfg)["gate"]
    print(json.dumps(out, indent=1))
    return out


if __name__ == "__main__":
    main(sys.argv[1:])
