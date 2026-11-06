import json
import math
import os
import re
import sys
import tempfile
import time
import numpy as np
from scipy.optimize import least_squares
from export import Exporter
from features import FeatureFrame, Ops, Spec, CONFIG as FCFG
from models import CONFIG as MCFG, Metrics, local_idx
from rawstore import RawStore

HERE = os.path.dirname(os.path.abspath(__file__))
MODELS = os.path.join(HERE, "..", "models")
CFG = {
    "store_dir": FCFG["cache_dir"],
    "spec": os.path.join(MODELS, "feature_spec_deploy.json"),
    "deployed": os.path.join(MODELS, "model_deploy.json"),
    "out_dir": os.path.join(MODELS, "steptrain"),
    "phase_channel": "daisyloop/step/phase",
    "windows": [],
    "window": {"merge_s": 120, "pre_s": 300, "post_s": 900, "holdout_frac": 0.2, "embargo_s": 900},
    "blocks": {k: MCFG["blocks"][k] for k in ("train", "val", "test", "lhm_missing", "oor_pump20")},
    "stride": MCFG["stride"],
    "horizons": MCFG["horizons"],
    "series": MCFG["series"],
    "step_share": 0.05,
    "augment": {"target": "water_120", "every": {"fan": 2, "pump": 1}, "share": 0.2, "horizon_s": 120},
    "lhm_aug": 0.3,
    "seed": 0,
    "ray": {"num_cpus": 4, "memory": 6e9},
    "xgb": dict(MCFG["xgb"], max_depth=3, min_child_weight=100, eta=0.03, nthread=4),
    "finetune": {"max_depth": 5, "chunk_s": 300, "val_every": 4, "eta": 0.1, "min_child_weight": 2, "rounds": 800, "every": 10, "patience": 6, "guard": 0.02, "min_gain": 0.0005},
    "maps": {"bins": 5.0, "min_n": 30},
    "boot": {"block_s": 600, "n": 400},
    "gate": {
        "target": "water_120",
        "rmse_tolerance": 0.0,
        "required": ["fan", "pump"],
        "context_s": 2000,
        "anchor_s": 60,
        "rollout_s": 120,
        "min_load_w": 40.0,
        "z": 1.96,
        "se_inflate": 10.0,
        "dwell_s": 120,
        "fan": {"channel": "fan", "grid": [27.0, 40.0, 55.0, 75.0, 100.0], "min_effect_k": 0.1, "min_ratio": 0.3, "monotone_tol_frac": 0.25, "min_span": 40.0, "min_levels": 3, "min_drop": 0.25},
        "pump": {"channel": "pump", "grid": [15.0, 40.0, 70.0, 100.0], "min_effect_k": 0.01, "min_ratio": 0.1, "min_plant_k": 0.02, "min_span": 50.0, "min_levels": 2},
        "text": [
            "ship only if all of the following hold, otherwise the artifact is written as candidate_* and not as model_deploy.json",
            "1. fan and pump excitation: enough distinct duty levels held long enough in the step window",
            "2. grey-box fit of the thermal.md plant on the window with the dead time as a parameter: fan exponent m and pump coefficient gamma have the physical sign and a one-sided 95 % bound (standard error inflated for autocorrelation) that excludes zero, and the fan conductance at the lowest level is at least min_drop below full fan; for the pump the fitted gamma is not significantly negative (more flow never cools worse) and the fitted net effect of pump 100 % against the floor on the 2 min water forecast (pump heat minus the gain in conductance) is at least min_plant_k in magnitude, because the sign to expect comes from it",
            "3. counterfactual of the retrained trees as the planner uses them: at anchor rows of the window the recorded history, then the channel held at each grid level for rollout_s, all features recomputed, forecast read at the end of the rollout: fan effect lower level minus full fan on the 2 min water forecast is positive, at least min_effect_k and min_ratio times the effect the fitted plant implies, the block bootstrap interval is above zero and the curve is monotone within monotone_tol_frac of its range; the pump effect has the sign the fitted plant implies, at least min_effect_k, interval excluding zero",
            "4. water 2 min RMSE of the retrained model on the old test block (not used for training or early stopping) is not worse than the deployed model's by more than rmse_tolerance",
        ],
    },
    "plant": {
        "c1": 7100.0, "c2": 4600.0, "k": 24.0, "a": 23.8, "m": 1.61, "gamma": 0.03, "ta": 25.0, "eta_cpu": 0.96, "eta_gpu": 1.0,
        "fan_full_rpm": 1460.0, "fan_per_duty": 14.7, "spool_s": 4.5, "pump_cal": [[0.0, 2000.0], [19.8, 2570.0], [100.0, 4800.0]],
        "pump_power": [[0.0, 4.26], [19.8, 4.26], [100.0, 18.32]], "dead": [[2400.0, 33.0], [2570.0, 31.0], [4800.0, 11.0]],
        "sensor_k": 0.005, "quant": 0.01, "load_noise_w": 8.0, "rpm_noise": 2.0,
    },
    "inject": {
        "base_span": ("2026-11-04 07:40", "2026-11-04 09:24"),
        "gap_s": 3600,
        "rest_s": 900,
        "temps": [[r"^/intelcpu/0/temperature/", 0.2, "cpu"], [r"^nvml/0/temp$", 0.062, "gpu"], [r"^/gpu-nvidia/0/temperature/2$", 0.038, "gpu"]],
        "loads": {"/intelcpu/0/load/0": ["cpu", 15.0, 250.0], "nvml/0/util_gpu": ["gpu", 0.0, 320.0], "/gpu-nvidia/0/load/3": ["gpu", 0.0, 320.0]},
        "protocols": {
            "constant": {
                "fan": [[100, 300, 72], [75, 480, 72], [55, 480, 72], [40, 480, 72], [27, 480, 72], [100, 300, 72]],
                "pump": [[15, 300, 190], [100, 180, 190], [15, 180, 190], [100, 180, 190], [15, 180, 190]],
            },
            "matched": {
                "fan": [[100, 300, 180], [75, 480, 115], [55, 480, 70], [40, 480, 48], [27, 480, 45], [100, 300, 180]],
                "pump": [[15, 300, 190], [100, 180, 190], [15, 180, 190], [100, 180, 190], [15, 180, 190]],
            },
        },
        "cpu_share": 0.55,
    },
}


class Plant:
    def __init__(self, cfg=CFG["plant"], fan_response=True):
        self.c = dict(cfg)
        if not fan_response:
            self.c["m"] = 0.0

    def pump(self, duty):
        c = self.c
        cal, pw = np.array(c["pump_cal"]).T, np.array(c["pump_power"]).T
        return np.interp(duty, *cal), np.interp(duty, *pw)

    def fan(self, duty):
        c = self.c
        target = np.asarray(duty, dtype=np.float64) * c["fan_per_duty"]
        a = 1.0 - math.exp(-1.0 / c["spool_s"])
        out = np.empty(len(target))
        r = target[0]
        for i, t in enumerate(target):
            r += (t - r) * a
            out[i] = r
        return out

    def run(self, fan_duty, pump_duty, p_cpu, p_gpu, rng):
        c = self.c
        n = len(fan_duty)
        frpm = self.fan(fan_duty)
        prpm, ppow = self.pump(np.asarray(pump_duty, dtype=np.float64))
        q = c["eta_cpu"] * p_cpu + c["eta_gpu"] * p_gpu + ppow
        tau = np.interp(prpm, *np.array(c["dead"]).T)
        qd = np.interp(np.arange(n) - tau, np.arange(n), q)
        G = c["a"] * np.power(np.maximum(frpm, 0.0) / c["fan_full_rpm"], c["m"]) * (1.0 + c["gamma"] * (prpm / 4800.0 - 1.0))
        g0 = G[0]
        t1 = t2 = c["ta"] + qd[0] / max(g0, 1e-6)
        out = np.empty(n)
        for i in range(n):
            out[i] = t1
            f = c["k"] * (t1 - t2)
            t1 += (qd[i] - G[i] * (t1 - c["ta"]) - f) / c["c1"]
            t2 += f / c["c2"]
        water = np.round((out + rng.normal(0.0, c["sensor_k"], n)) / c["quant"]) * c["quant"]
        return {"water": water, "fan_rpm": np.round(frpm + rng.normal(0.0, c["rpm_noise"], n)), "pump_rpm": np.round(prpm + rng.normal(0.0, c["rpm_noise"], n)), "pump_pow": ppow}


class Injector:
    def __init__(self, cfg=CFG):
        self.cfg = cfg

    def schedule(self, steps, rng):
        d = np.concatenate([np.full(int(s[1]), float(s[0])) for s in steps])
        p = np.concatenate([np.full(int(s[1]), float(s[2])) for s in steps])
        return d, p

    def segments(self, protocol):
        ic = self.cfg["inject"]
        fan, pump = ic["protocols"][protocol]["fan"], ic["protocols"][protocol]["pump"]
        rest = [100, ic["rest_s"], 60]
        return [("fan", fan), ("rest", [rest]), ("pump", pump), ("rest", [rest])]

    def build(self, protocol, fan_response, dest, seed=0):
        cfg = self.cfg
        ic, rng = cfg["inject"], np.random.default_rng(seed)
        meta, X = RawStore(dict(cfg_store(cfg["store_dir"]))).load()
        chans = Spec.load(cfg["spec"]).raw_channels()
        ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
        keep = [meta["channels"][ix[c]] for c in chans]
        T0 = X.shape[0]
        a, b = (local_idx(meta, s) for s in ic["base_span"])
        base = np.asarray(X[a:b][:, [ix[c["channel"]] for c in keep]], dtype=np.float64)
        for j in range(base.shape[1]):
            base[:, j] = Ops.ffill(base[:, j], 10 ** 9, False)
            base[:, j] = np.where(np.isfinite(base[:, j]), base[:, j], np.nanmedian(base[:, j]) if np.isfinite(base[:, j]).any() else np.nan)
        col = {c["channel"]: i for i, c in enumerate(keep)}
        plant = Plant(cfg["plant"], fan_response)
        fan_d, pump_d, pc, pg, phase = [], [], [], [], []
        for name, steps in self.segments(protocol):
            d, pw = self.schedule(steps, rng)
            fan_d.append(d if name != "pump" else np.full(len(d), 100.0))
            pump_d.append(np.full(len(d), 19.8) if name != "pump" else np.concatenate([np.full(int(s[1]), float(s[0])) for s in steps]))
            pw = pw + rng.normal(0.0, cfg["plant"]["load_noise_w"], len(pw))
            pc.append(np.clip(pw * ic["cpu_share"], 5.0, None))
            pg.append(np.clip(pw * (1.0 - ic["cpu_share"]), 3.0, None))
            ph = np.full(len(d), np.nan)
            if name != "rest":
                ph = np.concatenate([np.full(int(s[1]), i + 1.0) for i, s in enumerate(steps)])
            phase.append(ph)
        fan_d, pump_d, pc, pg, phase = (np.concatenate(v) for v in (fan_d, pump_d, pc, pg, phase))
        n = len(fan_d)
        assert n <= b - a, (n, b - a)
        sim = plant.run(fan_d, pump_d, pc, pg, rng)
        seg = base[:n].copy()
        w_base, c_base, g_base = seg[:, col["d5next/water/temp"]].copy(), seg[:, col["/intelcpu/0/power/0"]].copy(), seg[:, col["nvml/0/power"]].copy()
        w_fill = np.where(np.isfinite(w_base), w_base, np.nanmedian(w_base))
        shift = {"cpu": pc - np.nan_to_num(c_base, nan=float(np.nanmedian(c_base))), "gpu": pg - np.nan_to_num(g_base, nan=float(np.nanmedian(g_base)))}
        for pat, k, who in ic["temps"]:
            for ch, i in col.items():
                if re.search(pat, ch):
                    seg[:, i] = seg[:, i] + (sim["water"] - w_fill) + k * shift[who]
        for ch, (who, lo, hi) in ic["loads"].items():
            p = {"cpu": pc, "gpu": pg}[who]
            seg[:, col[ch]] = np.clip(100.0 * (p - lo) / (hi - lo), 0.0, 100.0)
        seg[:, col["d5next/water/temp"]] = sim["water"]
        seg[:, col["d5next/fan/duty"]] = fan_d
        seg[:, col["d5next/fan/rpm"]] = sim["fan_rpm"]
        seg[:, col["d5next/pump/duty"]] = pump_d
        seg[:, col["d5next/pump/rpm"]] = sim["pump_rpm"]
        seg[:, col["d5next/pump/power"]] = sim["pump_pow"]
        seg[:, col["/intelcpu/0/power/0"]] = pc
        seg[:, col["nvml/0/power"]] = pg
        seg[:, col["/gpu-nvidia/0/power/0"]] = pg
        gap = np.full((ic["gap_s"], len(keep)), np.nan)
        real = np.asarray(X[:, [ix[c["channel"]] for c in keep]], dtype=np.float32)
        step_cols = np.full((T0 + ic["gap_s"] + n, 1), np.nan, dtype=np.float32)
        step_cols[T0 + ic["gap_s"]:, 0] = phase
        out = np.vstack([real, gap.astype(np.float32), seg.astype(np.float32)])
        out = np.hstack([out, step_cols])
        os.makedirs(dest, exist_ok=True)
        np.save(os.path.join(dest, "raw.npy"), out)
        channels = keep + [{"channel": cfg["phase_channel"], "source": "daisyloop", "label": "step test phase", "kind": "other", "unit": "", "role": "input"}]
        with open(os.path.join(dest, "meta.json"), "w") as f:
            json.dump({"rows": int(meta.get("rows", 0)), "k0": meta["k0"], "k1": meta["k0"] + out.shape[0] - 1, "channels": channels}, f)
        return {"rows": out.shape[0], "injected_from": T0 + ic["gap_s"], "seconds": n, "water_max": float(np.nanmax(sim["water"])), "water_min": float(np.nanmin(sim["water"]))}


def cfg_store(path):
    return {"cache_dir": path, "chunk_s": 86400}


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
    NAMES = ["C", "A", "m", "gamma", "Ta", "tau", "f1", "K", "tau_hi"]
    LO = [3.0, 1.0, 0.0, -1.0, 10.0, 0.0, 0.3, 5.0, 0.0]
    HI = [40.0, 80.0, 3.5, 1.0, 30.0, 60.0, 1.0, 80.0, 60.0]
    X0 = [11.7, 20.0, 1.0, 0.0, 25.0, 20.0, 0.6, 24.0, 12.0]
    SCALE = [5.0, 10.0, 1.0, 0.2, 5.0, 10.0, 0.3, 10.0, 10.0]

    def __init__(self, cfg=CFG):
        self.full = cfg["plant"]["fan_full_rpm"]
        self.pump_lo, self.pump_hi = cfg["plant"]["pump_cal"][1][1], cfg["plant"]["pump_cal"][-1][1]

    def conductance(self, th, frpm, prpm):
        return th[1] * np.power(np.maximum(frpm, 0.0) / self.full, th[2]) * (1.0 + th[3] * (prpm / 4800.0 - 1.0))

    def simulate(self, th, w, q, frpm, prpm):
        C, A, m, gam, Ta, tau, f1, K, tau_hi = th
        n = len(w)
        qd = np.interp(np.arange(n) - np.interp(prpm, [self.pump_lo, self.pump_hi], [tau, tau_hi]), np.arange(n), q)
        G = self.conductance(th, frpm, prpm)
        c1, c2 = C * 1000.0 * f1, C * 1000.0 * (1.0 - f1) + 1e-9
        t1 = t2 = w[0]
        out = np.empty(n)
        for i in range(n):
            out[i] = t1
            f = K * (t1 - t2)
            t1 += (qd[i] - G[i] * (t1 - Ta) - f) / c1
            t2 += f / c2
        return out

    def fit(self, segs, fixed=None):
        lo, hi, x0 = list(self.LO), list(self.HI), list(self.X0)
        for k, v in (fixed or {}).items():
            i = self.NAMES.index(k)
            lo[i], hi[i], x0[i] = v, v + 1e-9, v

        def res(th):
            return np.concatenate([self.simulate(th, s["w"], s["q"], s["f"], s["p"]) - s["w"] for s in segs])

        r = least_squares(res, x0, bounds=(lo, hi), x_scale=self.SCALE)
        return r.x, float(r.fun @ r.fun), len(r.fun)


class StepTrainer:
    def __init__(self, cfg=CFG):
        self.cfg = cfg
        self.out = cfg["out_dir"]

    def load(self):
        cfg = self.cfg
        meta, X = RawStore(cfg_store(cfg["store_dir"])).load()
        self.meta = meta
        self.spec = Spec.load(cfg["spec"])
        self.deployed = Forest(json.load(open(cfg["deployed"])))
        self.feats = self.deployed.features
        self.chans = self.spec.raw_channels() + [cfg["phase_channel"]]
        ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
        have = [c for c in self.chans if c in ix]
        self.X = np.asarray(X[:, [ix[c] for c in have]])
        self.ix = {c: i for i, c in enumerate(have)}
        self.k0 = meta["k0"]
        self.T = self.X.shape[0]
        self.names, self.F, _ = FeatureFrame().compute(self.spec, self.X, self.ix, self.k0)
        self.col = {n: i for i, n in enumerate(self.names)}
        self.fi = [self.col[n] for n in self.feats]
        return self

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

    def dataset(self):
        cfg, w = self.cfg, self.cfg["window"]
        T, st = self.T, cfg["stride"]
        self.rows = np.arange(0, T, st)
        self.Y = {}
        ser = {k: Ops.ffill(np.asarray(self.X[:, self.ix[ch]], dtype=np.float64), FCFG["ffill_s"], False) for k, ch in cfg["series"].items()}
        for k in ser:
            for h in cfg["horizons"]:
                y = np.full(T, np.nan)
                y[: T - h] = ser[k][h:] - ser[k][: T - h]
                self.Y[f"{k}_{h}"] = y[self.rows]
        blocks = {n: np.zeros(len(self.rows), bool) for n in list(cfg["blocks"]) + ["step_train", "step_hold", "step_active"]}
        for n, spans in cfg["blocks"].items():
            for a, z in spans:
                blocks[n] |= (self.rows >= max(0, local_idx(self.meta, a))) & (self.rows < local_idx(self.meta, z))
        self.wins = self.windows()
        ph = np.isfinite(self.X[:, self.ix[cfg["phase_channel"]]]) if cfg["phase_channel"] in self.ix else np.zeros(T, bool)
        for a, b in self.wins:
            lo, hi = max(0, a - w["pre_s"]), min(T, b + w["post_s"])
            hold = int(hi - w["holdout_frac"] * (hi - lo))
            blocks["step_train"] |= (self.rows >= lo) & (self.rows < hold - w["embargo_s"])
            blocks["step_hold"] |= (self.rows >= hold) & (self.rows < hi)
            blocks["step_active"] |= (self.rows >= a) & (self.rows < b)
        self.blocks = blocks
        return self

    def lhm_features(self):
        b = self.blocks["lhm_missing"]
        tr = self.blocks["train"]
        Fb = self.F[self.rows][:, self.fi]
        miss = np.isfinite(Fb[b]).mean(0) < 0.05 if b.any() else np.zeros(len(self.feats), bool)
        have = np.isfinite(Fb[tr]).mean(0) > 0.9
        return [n for n, m, h in zip(self.feats, miss, have) if m and h]

    def train(self):
        import xgboost as xgb
        cfg = self.cfg
        Fb = self.F[self.rows][:, self.fi]
        lhm = set(self.lhm_features())
        lci = [i for i, n in enumerate(self.feats) if n in lhm]
        mci = [i for i, n in enumerate(self.feats) if n.startswith("mask_lhm")]
        self.boosters, self.best, self.weights = {}, {}, {}
        for tg, y in self.Y.items():
            ok = np.isfinite(y)
            tr = np.where(self.blocks["train"] & ok)[0]
            sp = np.where(self.blocks["step_train"] & ok)[0]
            va = np.where(self.blocks["val"] & ok)[0]
            s = cfg["step_share"]
            ws = s / (1.0 - s) * len(tr) / max(len(sp), 1)
            idx = np.concatenate([tr, sp])
            wt = np.concatenate([np.ones(len(tr)), np.full(len(sp), ws)])
            self.weights[tg] = {"train_rows": int(len(tr)), "step_rows": int(len(sp)), "step_row_weight": float(ws), "step_share": s}
            Xtr, ytr = Fb[idx], y[idx]
            ex = tg == cfg["augment"]["target"] and len(self.aug["y"]) > 0
            if ex:
                sh = cfg["augment"]["share"]
                chs, cnt = np.unique(self.aug["ch"], return_counts=True)
                per = dict(zip(chs, sh / (1.0 - sh) * len(tr) / len(chs) / cnt))
                wa = np.array([per[k] for k in self.aug["ch"]])
                Xtr, ytr, wt = np.vstack([Xtr, self.aug["F"]]), np.concatenate([ytr, self.aug["y"]]), np.concatenate([wt, wa])
                self.weights[tg].update(aug_rows={k: int(v) for k, v in zip(chs, cnt)}, aug_row_weight={k: float(v) for k, v in per.items()})
            rng = np.random.default_rng(cfg["seed"])
            pick = rng.random(len(Xtr)) < cfg["lhm_aug"]
            Xa = Xtr[pick].copy()
            Xa[:, lci] = np.nan
            Xa[:, mci] = 0.0
            p = dict(cfg["xgb"])
            rounds, early = p.pop("rounds"), p.pop("early")
            p["seed"] = cfg["seed"]
            dtr = xgb.DMatrix(np.vstack([Xtr, Xa]), np.concatenate([ytr, ytr[pick]]), weight=np.concatenate([wt, wt[pick]]))
            dva = xgb.DMatrix(Fb[va], y[va])
            m = xgb.train(p, dtr, rounds, evals=[(dva, "val")], early_stopping_rounds=early, verbose_eval=False)
            m = m[: int(m.best_iteration) + 1]
            self.boosters[tg], self.weights[tg]["finetune"] = self.finetune(m, Fb, y, sp, va, (self.aug["F"], self.aug["y"]) if ex else None)
            self.best[tg] = self.boosters[tg].num_boosted_rounds() - 1
        return self

    def finetune(self, base, Fb, y, sp, va, extra=None):
        import xgboost as xgb
        c = self.cfg["finetune"]
        held = (self.rows[sp] // c["chunk_s"]) % c["val_every"] == c["val_every"] - 1
        fit, sv = sp[~held], sp[held]
        rm = lambda b, ix: float(np.sqrt(np.mean((b.predict(xgb.DMatrix(Fb[ix])) - y[ix]) ** 2)))
        old0, step0 = rm(base, va), rm(base, sv)
        p = dict(self.cfg["xgb"], eta=c["eta"], min_child_weight=c["min_child_weight"], max_depth=c["max_depth"], seed=self.cfg["seed"])
        p.pop("rounds"), p.pop("early")
        dfit = xgb.DMatrix(np.vstack([Fb[fit]] + ([extra[0]] if extra else [])), np.concatenate([y[fit]] + ([extra[1]] if extra else [])))
        keep, best, left, trace = base, step0, c["patience"], []
        cur = base
        for r in range(0, c["rounds"], c["every"]):
            cur = xgb.train(p, dfit, c["every"], xgb_model=cur)
            old, st = rm(cur, va), rm(cur, sv)
            trace.append([cur.num_boosted_rounds(), old, st])
            if old > old0 * (1.0 + c["guard"]):
                break
            if st < best * (1.0 - c["min_gain"]):
                keep, best, left = cur, st, c["patience"]
            else:
                left -= 1
                if left <= 0:
                    break
        return keep, {"base_rounds": base.num_boosted_rounds(), "added": keep.num_boosted_rounds() - base.num_boosted_rounds(), "old_val_rmse_before": old0, "old_val_rmse_after": rm(keep, va), "step_val_rmse_before": step0, "step_val_rmse_after": best, "fit_rows": int(len(fit)), "val_rows": int(len(sv))}

    def deploy_model(self, identified):
        out = {"features": self.feats, "identified": identified, "targets": {}}
        for tg, b in self.boosters.items():
            base = float(json.loads(b.save_config())["learner"]["learner_model_param"]["base_score"].strip("[]"))
            trees = [Exporter.flat(json.loads(s)) for s in b.get_dump(dump_format="json")[: self.best[tg] + 1]]
            out["targets"][tg] = {"base": base, "trees": trees}
        return out

    def check_export(self, model):
        import xgboost as xgb
        f = Forest(model)
        ix = np.where(self.blocks["test"])[0][::10]
        Fb = self.F[self.rows][:, self.fi]
        worst = 0.0
        pred = f.predict(Fb[ix])
        for tg, b in self.boosters.items():
            ref = b.predict(xgb.DMatrix(Fb[ix]), iteration_range=(0, self.best[tg] + 1))
            worst = max(worst, float(np.abs(ref - pred[tg]).max()))
        return worst

    def evaluate(self, new):
        Fb = self.F[self.rows][:, self.fi]
        m = Metrics(MCFG)
        pn, pd = new.predict(Fb), self.deployed.predict(Fb)
        out = {}
        for blk in ("test", "step_hold", "lhm_missing", "oor_pump20"):
            for tg, y in self.Y.items():
                ix = np.where(self.blocks[blk] & np.isfinite(y) & np.isfinite(pn[tg]) & np.isfinite(pd[tg]))[0]
                if len(ix) < 50:
                    continue
                out[f"{tg}|{blk}"] = m.compare(self.rows[ix], y[ix].astype(np.float64), {"new": pn[tg][ix], "deployed": pd[tg][ix]})
        return out

    def maps(self):
        b, n = self.cfg["maps"]["bins"], self.cfg["maps"]["min_n"]
        out = {}
        for ch in ("fan", "pump"):
            d = self.X[:, self.ix[f"d5next/{ch}/duty"]]
            res = {}
            for what in ("rpm", "power"):
                key = f"d5next/{ch}/{what}"
                if key not in self.ix:
                    continue
                y = self.X[:, self.ix[key]]
                ok = np.isfinite(d) & np.isfinite(y)
                xs, ys = [], []
                for lo in np.arange(0, 105, b):
                    s = ok & (d >= lo) & (d < lo + b)
                    if s.sum() > n:
                        xs.append(float(np.median(d[s])))
                        ys.append(float(np.median(y[s])))
                res[what] = (xs, ys)
            out[ch] = res
        return out

    def labels(self):
        out = []
        for a, b in self.wins:
            span = {c: float(np.nanmax(self.X[a:b, self.ix[f"d5next/{c}/duty"]]) - np.nanmin(self.X[a:b, self.ix[f"d5next/{c}/duty"]])) for c in ("fan", "pump")}
            out.append(max(span, key=span.get))
        return out

    def excitation(self, ch):
        g = self.cfg["gate"]
        act = np.zeros(self.T, bool)
        for (a, b), who in zip(self.wins, self.labels()):
            if who == ch:
                act[a:b] = True
        d = np.round(self.X[act, self.ix[f"d5next/{ch}/duty"]] / 5.0) * 5.0
        d = d[np.isfinite(d)]
        u, c = np.unique(d, return_counts=True)
        held = u[c >= g["dwell_s"]]
        return {"levels_held": held.tolist(), "span": float(held.max() - held.min()) if len(held) else 0.0}

    def segments(self):
        segs = []
        for (a, b), who in zip(self.wins, self.labels()):
            sl = slice(a, b)
            g = lambda c: self.X[sl, self.ix[c]].astype(np.float64)
            w = g("d5next/water/temp")
            q = 0.96 * np.nan_to_num(g("/intelcpu/0/power/0")) + np.nan_to_num(g("nvml/0/power")) + np.nan_to_num(g("d5next/pump/power"))
            ok = np.isfinite(w) & np.isfinite(g("d5next/fan/rpm")) & np.isfinite(g("d5next/pump/rpm"))
            if ok.mean() > 0.9:
                w = np.where(np.isfinite(w), w, np.nanmedian(w))
                segs.append({"w": w, "q": q, "f": np.nan_to_num(g("d5next/fan/rpm")), "p": np.nan_to_num(g("d5next/pump/rpm"), nan=2570.0), "pw": np.nan_to_num(g("d5next/pump/power")), "a": a, "b": b, "who": who})
        return segs

    def structural(self):
        g = self.cfg["gate"]
        gb = GreyBox(self.cfg)
        segs = self.segments()
        th, sse, n = gb.fit(segs)
        out = {"theta": dict(zip(gb.NAMES, th.tolist())), "fit_rmse_k": float(np.sqrt(sse / n)), "n": n}
        for k in ("m", "gamma"):
            _, sse0, _ = gb.fit(segs, {k: 0.0})
            out[f"lr_{k}"] = float(n * math.log(max(sse0, sse) / sse) / g["se_inflate"] ** 2)
        act = np.concatenate([np.arange(a, b) for a, b in self.wins])
        out["lowest_rpm_ratio"] = max(float(np.nanmin(self.X[act, self.ix["d5next/fan/rpm"]])) / self.cfg["plant"]["fan_full_rpm"], 0.05)
        out["g_ratio_at_lowest"] = float(out["lowest_rpm_ratio"] ** out["theta"]["m"])
        out["lr_threshold"] = g["z"] ** 2
        return out

    def plant_effect(self, ch, st, grid, t, maps):
        th = np.array([st["theta"][k] for k in GreyBox.NAMES])
        gb = GreyBox(self.cfg)
        g = lambda c: self.X[t, self.ix[c]].astype(np.float64)
        w, fr, pr = g("d5next/water/temp"), g("d5next/fan/rpm"), g("d5next/pump/rpm")
        sc = 120.0 / (th[0] * 1000.0)
        rpm = lambda c, d: float(np.interp(d, *maps[c]["rpm"]))
        if ch == "fan":
            hi = gb.conductance(th, np.full(len(t), rpm("fan", grid[-1])), pr)
            lo = gb.conductance(th, np.full(len(t), rpm("fan", grid[0])), pr)
            return float(np.mean((hi - lo) * (w - th[4])) * sc)
        hi = gb.conductance(th, fr, np.full(len(t), rpm("pump", grid[-1])))
        lo = gb.conductance(th, fr, np.full(len(t), rpm("pump", grid[0])))
        dp = float(np.interp(grid[-1], *maps["pump"]["power"]) - np.interp(grid[0], *maps["pump"]["power"]))
        return float(np.mean(dp - (hi - lo) * (w - th[4])) * sc)

    def feat_at(self, r, ch, d, maps):
        g = self.cfg["gate"]
        a0 = max(0, int(r) - g["context_s"])
        Xs = self.X[a0 : int(r) + 1].copy()
        h = int(r) - g["rollout_s"] - a0 + 1
        Xs[h:, self.ix[f"d5next/{ch}/duty"]] = d
        for what, (xs, ys) in maps[ch].items():
            key = f"d5next/{ch}/{what}"
            if key in self.ix:
                Xs[h:, self.ix[key]] = np.interp(d, xs, ys)
        names, F, _ = self.ff.compute(self.spec, Xs, self.ix, self.k0 + a0)
        self.pos = getattr(self, "pos", None) or [names.index(n) for n in self.feats]
        return F[-1, self.pos]

    def counter(self, new, ch, grid, maps):
        g = self.cfg["gate"]
        mine = np.zeros(self.T, bool)
        for (a, b), who in zip(self.wins, self.labels()):
            if who == ch:
                mine[a:b] = True
        ts = self.rows[np.where(mine[self.rows])[0]]
        heat = np.nan_to_num(self.X[ts, self.ix["/intelcpu/0/power/0"]].astype(np.float64)) + np.nan_to_num(self.X[ts, self.ix["nvml/0/power"]].astype(np.float64))
        ts = ts[heat > g["min_load_w"]]
        ts = ts[:: max(1, g["anchor_s"] // self.cfg["stride"])]
        ts = ts[ts + g["rollout_s"] < self.T]
        out = np.array([new.predict(np.array([self.feat_at(t + g["rollout_s"], ch, d, maps) for t in ts]), [g["target"]])[g["target"]] for d in grid])
        return out, ts

    def augment(self):
        c, g, ft = self.cfg["augment"], self.cfg["gate"], self.cfg["finetune"]
        th = np.array([self.st["theta"][k] for k in GreyBox.NAMES])
        gb, mp = GreyBox(self.cfg), self.mp
        y = self.Y[c["target"]]
        F, Y, C = [], [], []
        for sg in self.segments():
            ch, a, H, roll = sg["who"], sg["a"], c["horizon_s"], g["rollout_s"]
            rec = gb.simulate(th, sg["w"], sg["q"], sg["f"], sg["p"])
            rpm = lambda k, d: float(np.interp(d, *mp[k]["rpm"]))
            for r in range(a + roll + 1, sg["b"] - H - 1):
                ri = r // self.cfg["stride"]
                if r % self.cfg["stride"] or (ri % c["every"][ch]) or not self.blocks["step_train"][ri] or not np.isfinite(y[ri]) or (r // ft["chunk_s"]) % ft["val_every"] == ft["val_every"] - 1:
                    continue
                for d in g[ch]["grid"]:
                    f, p, q = sg["f"].copy(), sg["p"].copy(), sg["q"].copy()
                    i0, i1 = r - a - roll, r - a
                    if ch == "fan":
                        f[i0:] = rpm("fan", d)
                    else:
                        p[i0:] = rpm("pump", d)
                        q[i0:] += float(np.interp(d, *mp["pump"]["power"])) - sg["pw"][i0:]
                    alt = gb.simulate(th, sg["w"], q, f, p)
                    F.append(self.feat_at(r, ch, d, mp))
                    C.append(ch)
                    Y.append(y[ri] + (alt[i1 + H] - alt[i1]) - (rec[i1 + H] - rec[i1]))
        return {"F": np.array(F, dtype=np.float32), "y": np.array(Y, dtype=np.float64), "ch": np.array(C)}

    def bootstrap(self, t, delta):
        b, n = self.cfg["boot"]["block_s"], self.cfg["boot"]["n"]
        blk = (t // b).astype(np.int64)
        u, inv = np.unique(blk, return_inverse=True)
        s, c = np.bincount(inv, weights=delta), np.bincount(inv).astype(np.float64)
        rng = np.random.default_rng(1)
        d = rng.integers(0, len(u), (n, len(u)))
        m = s[d].sum(1) / c[d].sum(1)
        return [float(np.quantile(m, 0.025)), float(np.quantile(m, 0.975))]

    def identify(self, new, maps):
        g = self.cfg["gate"]
        st = self.st
        out = {"structural": st}
        for ch in ("fan", "pump"):
            c = g[ch]
            ex = self.excitation(ch)
            grid = c["grid"]
            pred, t = self.counter(new, ch, grid, maps)
            curve = pred.mean(1).tolist()
            plant = self.plant_effect(ch, st, grid, t, maps)
            if ch == "fan":
                delta = pred[0] - pred[-1]
                expect = 1.0
                tol = c["monotone_tol_frac"] * (max(curve) - min(curve))
                mono = all(curve[i] >= curve[i + 1] - tol for i in range(len(curve) - 1))
                phys = st["lr_m"] >= st["lr_threshold"] and st["theta"]["m"] > 0 and st["g_ratio_at_lowest"] <= 1.0 - c["min_drop"]
            else:
                delta = pred[-1] - pred[0]
                expect = float(np.sign(plant))
                mono = True
                phys = not (st["theta"]["gamma"] < 0 and st["lr_gamma"] >= st["lr_threshold"]) and abs(plant) >= c["min_plant_k"]
            ci = self.bootstrap(t, delta)
            eff = float(delta.mean())
            sign_ok = eff * expect > 0 and (ci[0] > 0 if expect > 0 else ci[1] < 0)
            res = {"excitation": ex, "grid": grid, "curve_k": curve, "effect_k": eff, "effect_ci": ci, "plant_effect_k": plant, "model_to_plant": eff / plant if plant else None, "expected_sign": expect,
                   "monotone": mono, "physical_fit_ok": bool(phys), "rows": int(len(t)), "min_ratio": c["min_ratio"], "excited": len(ex["levels_held"]) >= c["min_levels"] and ex["span"] >= c["min_span"]}
            res["identified"] = bool(res["excited"] and phys and sign_ok and abs(eff) >= c["min_effect_k"] and abs(eff) >= c["min_ratio"] * abs(plant) and mono)
            out[ch] = res
        return out

    def gate(self, ident, ev):
        g = self.cfg["gate"]
        k = f"{g['target']}|test"
        r = ev[k]["rmse"]
        rmse_ok = r["new"] <= r["deployed"] * (1.0 + g["rmse_tolerance"])
        flags = {c: ident[c]["identified"] for c in ("fan", "pump")}
        reasons = [f"{c} response not identified" for c in g["required"] if not flags[c]]
        if not rmse_ok:
            reasons.append(f"{g['target']} test RMSE {r['new']:.4f} worse than deployed {r['deployed']:.4f}")
        return {"ship": not reasons, "reasons": reasons, "identified": flags, "rmse": {"new": r["new"], "deployed": r["deployed"], "persistence": r["persist"]}, "criteria": g["text"]}

    def run(self):
        t0 = time.time()
        self.load().dataset()
        self.ff, self.mp = FeatureFrame(), self.maps()
        self.st = self.structural()
        self.aug = self.augment()
        self.train()
        new = Forest(self.deploy_model({"fan": False, "pump": False}))
        ident = self.identify(new, self.mp)
        ev = self.evaluate(new)
        verdict = self.gate(ident, ev)
        model = self.deploy_model(verdict["identified"])
        worst = self.check_export(model)
        assert worst < 1e-4, worst
        os.makedirs(self.out, exist_ok=True)
        name = "" if verdict["ship"] else "candidate_"
        with open(os.path.join(self.out, f"{name}model_deploy.json"), "w") as f:
            json.dump(model, f, separators=(",", ":"))
        sub = self.spec.subset(set(self.feats))
        sub.meta["deployed_outputs"] = self.feats
        sub.save(os.path.join(self.out, f"{name}feature_spec_deploy.json"))
        report = {"gate": verdict, "identification": ident, "evaluation": ev, "weights": self.weights, "windows": [[int(a), int(b)] for a, b in self.wins], "export_max_abs_diff": worst,
                  "best_iter": self.best, "seconds": round(time.time() - t0), "lhm_features": self.lhm_features()}
        with open(os.path.join(self.out, "report.json"), "w") as f:
            json.dump(report, f, indent=1)
        return report


class Cluster:
    def __init__(self, cfg=CFG):
        import ray
        self.ray, self.res = ray, cfg["ray"]
        ray.init(address="auto", logging_level=40, runtime_env={"py_executable": sys.executable, "env_vars": {"PYTHONPATH": HERE}})

    def run(self, fn, *args):
        return self.ray.get(self.ray.remote(num_cpus=self.res["num_cpus"], memory=self.res["memory"])(fn).remote(*args))


def train_task(cfg):
    return StepTrainer(cfg).run()


def dry_task(cfg, protocol, fan_response, root):
    store, out = os.path.join(root, "store"), os.path.join(root, "out")
    info = Injector(cfg).build(protocol, fan_response, store)
    cfg = dict(cfg, store_dir=store, out_dir=out)
    return info, StepTrainer(cfg).run()


class DryRun:
    def __init__(self, cfg=CFG):
        self.cfg = cfg

    def run(self, root=None):
        root = root or tempfile.mkdtemp(prefix="steptrain-dry-")
        cluster = Cluster(self.cfg)
        res = {}
        for protocol in self.cfg["inject"]["protocols"]:
            for fr in (True, False):
                info, rep = cluster.run(dry_task, self.cfg, protocol, fr, os.path.join(root, f"{protocol}-{'fan' if fr else 'nofan'}"))
                res[f"{protocol}|{'fan' if fr else 'nofan'}"] = {"inject": info, "gate": rep["gate"], "fan": rep["identification"]["fan"], "pump": rep["identification"]["pump"], "structural": rep["identification"]["structural"]}
        with open(os.path.join(root, "dryrun.json"), "w") as f:
            json.dump(res, f, indent=1)
        return root, res


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "dryrun":
        root, res = DryRun().run()
        print(root, {k: v["gate"]["ship"] for k, v in res.items()})
    else:
        r = Cluster().run(train_task, CFG)
        print(json.dumps(r["gate"], indent=1))
