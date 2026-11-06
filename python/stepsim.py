import json
import math
import os
import re
import numpy as np
from features import Ops, Spec, CONFIG as FCFG
from models import local_idx
from rawstore import RawStore

SIM = {
    "plant": {
        "c1": 7100.0, "c2": 4600.0, "k": 24.0, "a": 23.8, "m": 1.61, "gamma": 0.03, "ta": 25.0, "eta_cpu": 0.96, "eta_gpu": 1.0,
        "fan_full_rpm": 1460.0, "fan_per_duty": 14.7, "spool_s": 4.5, "pump_cal": [[0.0, 2000.0], [19.8, 2570.0], [100.0, 4800.0]],
        "pump_power": [[0.0, 4.26], [19.8, 4.26], [100.0, 18.32]], "dead": [[2400.0, 33.0], [2570.0, 31.0], [4800.0, 11.0]],
        "sensor_k": 0.005, "quant": 0.01, "load_noise_w": 8.0, "rpm_noise": 2.0, "firmware_fan": 100.0, "firmware_pump": 19.8,
        "bound_fan": [[35.0, 20.0], [38.0, 54.4], [42.0, 100.0]], "bound_pump": [[35.0, 15.0], [36.0, 100.0]], "start_water": 30.5, "drift_shape": "lin",
    },
    "inject": {
        "base_span": ("2026-11-04 07:40", "2026-11-04 09:24"),
        "gap_s": 3600,
        "temps": [[r"^/intelcpu/0/temperature/", 0.2, "cpu"], [r"^nvml/0/temp$", 0.062, "gpu"], [r"^/gpu-nvidia/0/temperature/2$", 0.038, "gpu"]],
        "loads": {"/intelcpu/0/load/0": ["cpu", 15.0, 250.0], "nvml/0/util_gpu": ["gpu", 0.0, 320.0], "/gpu-nvidia/0/load/3": ["gpu", 0.0, 320.0]},
        "cpu_share": 0.55,
        "idle_w": 70.0,
        "restart_gap": None,
        "protocols": {
            "runbook": [
                ["cold", 900, 70],
                ["fan", [[0, 120, 70]]],
                ["rest", 180, 70],
                ["pump", [[15, 120, 70]]],
                ["rest", 300, 70],
                ["fan", [[100, 300, 250], [75, 480, 100], [55, 480, 75], [40, 480, 70], [27, 480, 70], [100, 300, 250], [100, 300, 125]]],
                ["rest", 300, 70],
                ["pump", [[15, 300, 200], [100, 180, 200], [15, 180, 200], [100, 180, 200], [15, 180, 200]]],
                ["rest", 900, 70],
            ],
        },
    },
}


class Plant:
    DRIFT = {"lin": lambda x: x, "quad": lambda x: x * x, "sat": lambda x: 1.0 - math.exp(-x / 0.25), "bump": lambda x: math.sin(math.pi * x)}

    def __init__(self, cfg=SIM["plant"], fan_response=True, drift_k=0.0):
        self.c = dict(cfg)
        self.drift_k = drift_k
        if not fan_response:
            self.c["m"] = 0.0

    @staticmethod
    def curve(table, x):
        return float(np.interp(x, [r[0] for r in table], [r[1] for r in table]))

    def pump(self, duty):
        c = self.c
        return float(np.interp(duty, *np.array(c["pump_cal"]).T)), float(np.interp(duty, *np.array(c["pump_power"]).T))

    def dead(self, prpm):
        return float(np.interp(prpm, *np.array(self.c["dead"]).T))

    def conductance(self, frpm, prpm):
        c = self.c
        return c["a"] * max(frpm, 0.0) ** c["m"] / c["fan_full_rpm"] ** c["m"] * (1.0 + c["gamma"] * (prpm / 4800.0 - 1.0))

    def run(self, fan_cmd, pump_cmd, p_cpu, p_gpu, rng):
        c = self.c
        n = len(fan_cmd)
        a = 1.0 - math.exp(-1.0 / c["spool_s"])
        qtot = np.empty(n)
        out, fd_rec, pd_rec = np.empty(n), np.empty(n), np.empty(n)
        frpm, prpm_rec, ppow_rec, wread = np.empty(n), np.empty(n), np.empty(n), np.empty(n)
        qload = c["eta_cpu"] * p_cpu + c["eta_gpu"] * p_gpu
        r = (c["firmware_fan"] if np.isnan(fan_cmd[0]) else fan_cmd[0]) * c["fan_per_duty"]
        t1 = t2 = w_prev = c["start_water"]
        for i in range(n):
            fd = c["firmware_fan"] if np.isnan(fan_cmd[i]) else max(fan_cmd[i], self.curve(c["bound_fan"], w_prev) if w_prev >= c["bound_fan"][0][0] else 0.0)
            pd = c["firmware_pump"] if np.isnan(pump_cmd[i]) else max(pump_cmd[i], self.curve(c["bound_pump"], w_prev) if w_prev >= c["bound_pump"][0][0] else 0.0)
            r += (fd * c["fan_per_duty"] - r) * a
            prpm, ppow = self.pump(pd)
            qtot[i] = qload[i] + ppow
            x = i - self.dead(prpm)
            j = int(math.floor(x))
            qd = qtot[0] if j < 0 else qtot[i] if j >= i else qtot[j] + (qtot[j + 1] - qtot[j]) * (x - j)
            ta = c["ta"] + self.drift_k * self.DRIFT[c["drift_shape"]](i / n)
            G = self.conductance(r, prpm)
            out[i] = t1
            fd_rec[i], pd_rec[i], frpm[i], prpm_rec[i], ppow_rec[i] = fd, pd, r, prpm, ppow
            f = c["k"] * (t1 - t2)
            t1 += (qd - G * (t1 - ta) - f) / c["c1"]
            t2 += f / c["c2"]
            w_prev = out[i]
        water = np.round((out + rng.normal(0.0, c["sensor_k"], n)) / c["quant"]) * c["quant"]
        return {"water": water, "fan_duty": fd_rec, "pump_duty": pd_rec, "fan_rpm": np.maximum(np.round(frpm + rng.normal(0.0, c["rpm_noise"], n)), 0.0), "pump_rpm": np.round(prpm_rec + rng.normal(0.0, c["rpm_noise"], n)), "pump_pow": ppow_rec}

    def effect(self, w, prpm, rpm_lo, rpm_hi, horizon=120.0):
        c = self.c
        g = lambda r: c["a"] * (r / c["fan_full_rpm"]) ** c["m"] * (1.0 + c["gamma"] * (prpm / 4800.0 - 1.0))
        return float(np.mean((g(rpm_hi) - g(rpm_lo)) * (w - c["ta"])) * horizon / c["c1"])


class Injector:
    def __init__(self, cfg, sim=SIM):
        self.cfg, self.sim = cfg, sim

    def schedule(self, protocol):
        ic = self.sim["inject"]
        fan, pump, load, phase, active = [], [], [], [], []
        self.fan_test = None
        for name, *rest in ic["protocols"][protocol]:
            if name in ("fan", "pump"):
                steps = rest[0]
                if name == "fan" and len(steps) > 1:
                    self.fan_test = sum(len(v) for v in fan)
                for i, (d, secs, w) in enumerate(steps):
                    secs = int(secs)
                    fan.append(np.full(secs, float(d) if name == "fan" else np.nan))
                    pump.append(np.full(secs, float(d) if name == "pump" else np.nan))
                    load.append(np.full(secs, float(w)))
                    phase.append(np.full(secs, i + 1.0))
            else:
                secs, w = int(rest[0]), float(rest[1])
                fan.append(np.full(secs, np.nan))
                pump.append(np.full(secs, np.nan))
                load.append(np.full(secs, w))
                phase.append(np.full(secs, np.nan))
        return tuple(np.concatenate(v) for v in (fan, pump, load, phase))

    def build(self, protocol, fan_response, dest, seed=0, drift_k=0.0):
        cfg, ic = self.cfg, self.sim["inject"]
        rng = np.random.default_rng(seed)
        meta, X = RawStore({"cache_dir": cfg["store_dir"], "chunk_s": 86400}).load()
        chans = Spec.load(cfg["deployed_spec"]).raw_channels()
        ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
        keep = [meta["channels"][ix[c]] for c in chans if c in ix]
        T0 = X.shape[0]
        a, b = (local_idx(meta, s) for s in ic["base_span"])
        base = np.asarray(X[a:b][:, [ix[c["channel"]] for c in keep]], dtype=np.float64)
        for j in range(base.shape[1]):
            base[:, j] = Ops.ffill(base[:, j], 10 ** 9, False)
            base[:, j] = np.where(np.isfinite(base[:, j]), base[:, j], np.nanmedian(base[:, j]) if np.isfinite(base[:, j]).any() else np.nan)
        col = {c["channel"]: i for i, c in enumerate(keep)}
        fan_cmd, pump_cmd, load, phase = self.schedule(protocol)
        n = len(fan_cmd)
        base = np.concatenate([base, base[::-1]] * (n // len(base) + 1))[:n]
        load = np.maximum(load + rng.normal(0.0, self.sim["plant"]["load_noise_w"], n), ic["idle_w"] * 0.8)
        pc = np.clip(load * ic["cpu_share"], 5.0, None)
        pg = np.clip(load * (1.0 - ic["cpu_share"]), 3.0, None)
        sim = Plant(self.sim["plant"], fan_response, drift_k).run(fan_cmd, pump_cmd, pc, pg, rng)
        rg = ic["restart_gap"]
        seg = base.copy()
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
        for ch, v in (("d5next/water/temp", sim["water"]), ("d5next/fan/duty", sim["fan_duty"]), ("d5next/fan/rpm", sim["fan_rpm"]), ("d5next/pump/duty", sim["pump_duty"]),
                      ("d5next/pump/rpm", sim["pump_rpm"]), ("d5next/pump/power", sim["pump_pow"]), ("/intelcpu/0/power/0", pc), ("nvml/0/power", pg), ("/gpu-nvidia/0/power/0", pg)):
            seg[:, col[ch]] = v
        if rg:
            seg[self.fan_test - rg[1] - rg[0]:self.fan_test - rg[1], :] = np.nan
        gap = np.full((ic["gap_s"], len(keep)), np.nan)
        real = np.asarray(X[:, [ix[c["channel"]] for c in keep]], dtype=np.float32)
        step_cols = np.full((T0 + ic["gap_s"] + n, 1), np.nan, dtype=np.float32)
        step_cols[T0 + ic["gap_s"]:, 0] = phase
        out = np.hstack([np.vstack([real, gap.astype(np.float32), seg.astype(np.float32)]), step_cols])
        os.makedirs(dest, exist_ok=True)
        np.save(os.path.join(dest, "raw.npy"), out)
        channels = keep + [{"channel": cfg["phase_channel"], "source": "daisyloop", "label": "step test phase", "kind": "other", "unit": "", "role": "input"}]
        with open(os.path.join(dest, "meta.json"), "w") as f:
            json.dump({"rows": int(meta.get("rows", 0)), "k0": meta["k0"], "k1": meta["k0"] + out.shape[0] - 1, "channels": channels}, f)
        return {"rows": out.shape[0], "injected_from": T0 + ic["gap_s"], "seconds": n, "water_max": float(np.nanmax(sim["water"])), "water_min": float(np.nanmin(sim["water"])), "sim": sim, "phase": phase, "fan_cmd": fan_cmd, "pump_cmd": pump_cmd, "load": load}
