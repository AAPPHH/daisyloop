import json
import math
import os
import re
import numpy as np
from scipy.signal import lfilter
from rawstore import RawStore

CONFIG = {
    "ffill_s": 10,
    "taus": [5, 30, 120, 600, 1800],
    "deriv_lags": [10, 30, 120, 600],
    "tsc_cap": 3600,
    "tsc_thr_w": 50.0,
    "tsc_lag_s": 30,
    "vrm_frac": 0.07,
    "amb_rise_per_s": 1.0 / 3600.0,
    "water_floor_rise_per_s": 1.0 / 3600.0,
    "dedupe_corr": 0.999,
    "dedupe_stride": 20,
    "g_assumed_w_per_k": 25.0,
    "heat_flag_w": {"idle": 80.0, "heavy": 100.0, "dominance": 30.0},
    "exclude": [
        r"^/usbhid", r"^/vram", r"/data/(21|22|31|32|2|3)$", r"/factor/", r"/level/", r"/timing/",
        r"^/gpu-nvidia/0/load/7$", r"^/gpu-nvidia/0/smalldata/2$", r"^/gpu-intel",
    ],
    "exclude_labels": [r"Distance to TjMax", r"Thermal Sensor", r"Resolution"],
    "zero_nan": [r"^/nvme/\d+/temperature/", r"^/ssd/\d+/temperature/"],
    "p_cores": 8,
    "spec_path": os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "models", "feature_spec_rich.json"),
    "cache_dir": RawStore().dir,
}

GROUPS = [
    "water_fan_pump", "cpu_raw", "gpu_raw", "board_raw", "mem_storage_raw", "core_agg", "heat_proxy",
    "ewm_water", "ewm_power", "ewm_temp", "deriv", "integral", "delta", "fan_pump_terms", "regime", "time_since",
    "ambient", "time_of_day", "mask",
]


class Node:
    def __init__(self, name, op, ins=(), p=None, group="_", out=False):
        self.name, self.op, self.ins, self.p, self.group, self.out = name, op, list(ins), p or {}, group, out

    def to_dict(self):
        return {"name": self.name, "op": self.op, "in": self.ins, "p": self.p, "group": self.group, "out": self.out}

    @staticmethod
    def from_dict(d):
        return Node(d["name"], d["op"], d["in"], d["p"], d["group"], d["out"])


class Spec:
    def __init__(self, nodes, meta=None):
        self.nodes = nodes
        self.meta = meta or {}
        self.by = {n.name: n for n in nodes}

    def outputs(self):
        return [n.name for n in self.nodes if n.out]

    def groups(self):
        return {n.name: n.group for n in self.nodes if n.out}

    def raw_channels(self):
        return [n.p["channel"] for n in self.nodes if n.op == "raw"]

    def subset(self, names):
        need, stack = set(), list(names)
        while stack:
            k = stack.pop()
            if k not in need:
                need.add(k)
                stack.extend(self.by[k].ins)
        keep = [Node(n.name, n.op, n.ins, n.p, n.group, n.name in names) for n in self.nodes if n.name in need]
        return Spec(keep, dict(self.meta))

    def save(self, path):
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        with open(path, "w") as f:
            json.dump({"meta": self.meta, "nodes": [n.to_dict() for n in self.nodes]}, f, indent=0)

    @staticmethod
    def load(path):
        with open(path) as f:
            d = json.load(f)
        return Spec([Node.from_dict(x) for x in d["nodes"]], d["meta"])


class RawSelector:
    def __init__(self, cfg=CONFIG):
        self.cfg = cfg

    def select(self, meta, X):
        cfg = self.cfg
        chans = meta["channels"]
        dropped = {}
        cand = []
        for i, c in enumerate(chans):
            ch, lab = c["channel"], c["label"]
            v = X[:, i]
            f = v[np.isfinite(v)]
            if len(f) == 0 or float(f.std()) < 1e-9:
                dropped[ch] = "constant"
            elif any(re.search(p, ch) for p in cfg["exclude"]) or any(re.search(p, lab) for p in cfg["exclude_labels"]):
                dropped[ch] = "rule"
            else:
                cand.append(i)
        prio = {"d5next": 0, "nvml": 1, "lhm": 2}
        cand.sort(key=lambda i: (prio[chans[i]["source"]], chans[i]["channel"]))
        sub = np.asarray(X[:: cfg["dedupe_stride"]])
        kept = []
        for i in cand:
            a = sub[:, i]
            dup = None
            for j in kept:
                b = sub[:, j]
                m = np.isfinite(a) & np.isfinite(b)
                if m.sum() < 200:
                    continue
                aa, bb = a[m], b[m]
                if aa.std() < 1e-9 or bb.std() < 1e-9:
                    continue
                if abs(np.corrcoef(aa, bb)[0, 1]) > cfg["dedupe_corr"] and chans[i]["kind"] == chans[j]["kind"]:
                    dup = j
                    break
            if dup is None:
                kept.append(i)
            else:
                dropped[chans[i]["channel"]] = "duplicate_of:" + chans[dup]["channel"]
        return [chans[i] for i in kept], dropped


class SpecBuilder:
    def __init__(self, meta, kept, cfg=CONFIG):
        self.cfg = cfg
        self.nodes = []
        self.names = set()
        self.kept = {c["channel"]: c for c in kept}
        self.all = {c["channel"]: c for c in meta["channels"]}

    def add(self, name, op, ins=(), p=None, group="_", out=False):
        assert name not in self.names, name
        self.names.add(name)
        self.nodes.append(Node(name, op, ins, p, group, out))
        return name

    def raw(self, name, channel, group="_", out=True):
        zn = any(re.search(p, channel) for p in self.cfg["zero_nan"])
        return self.add(name, "raw", (), {"channel": channel, "ffill": self.cfg["ffill_s"], "zero_nan": zn}, group, out)

    def lin(self, name, ins, coef, off=0.0, group="_", out=False):
        return self.add(name, "lin", ins, {"coef": list(coef), "off": off}, group, out)

    def ewm(self, x, tau, group, out=True):
        return self.add(f"{x}.ewm{tau}", "ewm", [x], {"tau": float(tau)}, group, out)

    def diff(self, x, n, group, out=True):
        return self.add(f"{x}.d{n}", "diff", [x], {"n": n}, group, out)

    def agg(self, name, op, ins, group, out=True):
        return self.add(name, op, ins, {}, group, out)

    def core_groups(self, kind, pattern, label_re):
        res = {"P": [], "E": []}
        for ch, c in self.all.items():
            if c["kind"] != kind or not ch.startswith(pattern):
                continue
            m = re.search(label_re, c["label"])
            if m:
                res["P" if m.group(1) == "P-Core" else "E"].append((int(m.group(2)), ch))
        return {k: [ch for _, ch in sorted(v)] for k, v in res.items()}

    def build(self):
        cfg = self.cfg
        taus = cfg["taus"]
        K = self.kept
        has = lambda ch: ch in self.all
        self.raw("water", "d5next/water/temp", "water_fan_pump")
        for nm, ch in [("fan_duty", "d5next/fan/duty"), ("fan_rpm", "d5next/fan/rpm"), ("fan_pow", "d5next/fan/power"),
                       ("pump_duty", "d5next/pump/duty"), ("pump_rpm", "d5next/pump/rpm"), ("pump_pow", "d5next/pump/power"),
                       ("fan_cur", "d5next/fan/current"), ("pump_cur", "d5next/pump/current")]:
            self.raw(nm, ch, "water_fan_pump")
        percore = re.compile(r"^/intelcpu/0/(load|clock|voltage)/\d+$")
        used = {"d5next/water/temp", "d5next/fan/duty", "d5next/fan/rpm", "d5next/fan/power", "d5next/pump/duty", "d5next/pump/rpm",
                "d5next/pump/power", "d5next/fan/current", "d5next/pump/current"}
        keys = [("cpu_pkg_w", "/intelcpu/0/power/0", "cpu_raw"), ("gpu_w", "nvml/0/power", "gpu_raw"), ("gpu_temp", "nvml/0/temp", "gpu_raw"),
                ("cpu_core_t", "/intelcpu/0/temperature/0", "cpu_raw"), ("cpu_pkg_t", "/intelcpu/0/temperature/26", "cpu_raw"),
                ("gpu_hot", "/gpu-nvidia/0/temperature/2", "gpu_raw"), ("gpu_mem", "/gpu-nvidia/0/temperature/3", "gpu_raw"),
                ("sys_t", "/lpc/nct6687dr/0/temperature/1", "board_raw"), ("vrm_t", "/lpc/nct6687dr/0/temperature/2", "board_raw"),
                ("chip_t", "/lpc/nct6687dr/0/temperature/3", "board_raw"), ("dimm_t", "/memory/dimm/1/temperature/0", "mem_storage_raw"),
                ("ssd_t", "/ssd/0/temperature/0", "mem_storage_raw"), ("nvme_t", "/nvme/1/temperature/0", "mem_storage_raw"),
                ("gpu_util", "nvml/0/util_gpu", "gpu_raw"), ("cpu_load", "/intelcpu/0/load/0", "cpu_raw")]
        for nm, ch, g in keys:
            self.raw(nm, ch, g)
            used.add(ch)
        for ch, c in K.items():
            if ch in used or percore.match(ch):
                continue
            if ch.startswith("/intelcpu"):
                g = "cpu_raw"
            elif ch.startswith("/gpu-nvidia") or ch.startswith("nvml"):
                g = "gpu_raw"
            elif ch.startswith("/lpc") or ch.startswith("d5next"):
                g = "board_raw"
            else:
                g = "mem_storage_raw"
            self.raw("r:" + ch, ch, g)
        # per-core intermediates for group aggregates
        agg_sets = {}
        for kind, pat, lre in [("temperature", "/intelcpu/0/temperature/", r"(P-Core|E-Core) #(\d+)$"),
                               ("clock", "/intelcpu/0/clock/", r"(P-Core|E-Core) #(\d+)$")]:
            agg_sets[kind] = self.core_groups(kind, pat, lre)
        loads = sorted([(int(re.search(r"#(\d+)$", c["label"]).group(1)), ch) for ch, c in self.all.items()
                        if ch.startswith("/intelcpu/0/load/") and re.search(r"CPU Core #\d+$", c["label"])])
        agg_sets["load"] = {"P": [ch for n, ch in loads if n <= cfg["p_cores"]], "E": [ch for n, ch in loads if n > cfg["p_cores"]]}
        volts = {"P": [], "E": []}
        for ch, c in self.all.items():
            m = re.search(r"(P-Core|E-Core) #(\d+)$", c["label"])
            if c["kind"] == "voltage" and ch.startswith("/intelcpu/0/voltage/") and m:
                volts["P" if m.group(1) == "P-Core" else "E"].append((int(m.group(2)), ch))
        agg_sets["voltage"] = {k: [ch for _, ch in sorted(v)] for k, v in volts.items()}
        agg_out = {}
        for kind, sets in agg_sets.items():
            for cls, chs in sets.items():
                ins = []
                for ch in chs:
                    nm = "c:" + ch
                    if nm not in self.names:
                        self.raw(nm, ch, "_", out=False)
                    ins.append(nm)
                if not ins:
                    continue
                for op in ("gmax", "gmean", "gmin", "gspread"):
                    nm = f"{kind}_{cls}_{op[1:]}"
                    self.agg(nm, op, ins, "core_agg")
                agg_out[(kind, cls)] = ins
            allc = [x for cls in sets for x in agg_out.get((kind, cls), [])]
            if allc:
                for op in ("gmax", "gmean", "gspread"):
                    self.agg(f"{kind}_all_{op[1:]}", op, allc, "core_agg")
        self.lin("vrm_est_w", ["cpu_pkg_w"], [cfg["vrm_frac"]], 0.0, "heat_proxy", True)
        self.lin("p_total", ["cpu_pkg_w", "gpu_w", "pump_pow", "vrm_est_w"], [1, 1, 1, 1], 0.0, "heat_proxy", True)
        self.lin("p_cpu_gpu", ["cpu_pkg_w", "gpu_w"], [1, 1], 0.0, "heat_proxy", True)
        self.lin("p_gpu_minus_cpu", ["gpu_w", "cpu_pkg_w"], [1, -1], 0.0, "heat_proxy", True)
        self.add("amb_board", "gmin", ["dimm_t", "ssd_t", "nvme_t"], {}, "_", False)
        self.add("amb_board_floor", "leakymin", ["amb_board"], {"rise": cfg["amb_rise_per_s"]}, "ambient", True)
        self.add("water_floor", "leakymin", ["water"], {"rise": cfg["water_floor_rise_per_s"]}, "ambient", True)
        self.lin("amb_est", ["amb_board_floor", "water_floor"], [0.5, 0.5], 0.0, "ambient", True)
        self.lin("water_minus_amb", ["water", "amb_est"], [1, -1], 0.0, "ambient", True)
        self.lin("water_minus_wfloor", ["water", "water_floor"], [1, -1], 0.0, "ambient", True)
        for src, grp in [("water", "ewm_water"), ("p_total", "ewm_power"), ("cpu_pkg_w", "ewm_power"), ("gpu_w", "ewm_power"),
                         ("cpu_core_t", "ewm_temp"), ("gpu_temp", "ewm_temp"), ("cpu_load", "ewm_power"), ("gpu_util", "ewm_power"),
                         ("fan_duty", "ewm_water"), ("pump_duty", "ewm_water"), ("fan_rpm", "ewm_water")]:
            for t in taus:
                self.ewm(src, t, grp)
        for src in ["water", "p_total", "cpu_pkg_w", "gpu_w", "cpu_core_t", "gpu_temp", "fan_duty"]:
            for n in cfg["deriv_lags"]:
                self.diff(f"{src}.ewm5", n, "deriv")
        for src in ["water", "cpu_core_t", "gpu_temp"]:
            for n in (300, 900, 1800):
                self.diff(f"{src}.ewm30", n, "deriv")
        for src in ["p_total", "gpu_w", "cpu_pkg_w"]:
            for t in taus[1:]:
                self.add(f"{src}.int{t}", "scale", [f"{src}.ewm{t}"], {"c": float(t)}, "integral", True)
        self.lin("p_removed", ["water_minus_amb"], [cfg["g_assumed_w_per_k"]], 0.0, "_", False)
        self.lin("p_excess", ["p_total", "p_removed"], [1, -1], 0.0, "integral", True)
        for t in taus[1:]:
            self.add(f"p_excess.int{t}", "scale", [self.ewm("p_excess", t, "integral", True)], {"c": float(t)}, "integral", True)
        for nm, a, b in [("water_m_sys", "water", "sys_t"), ("water_m_chip", "water", "chip_t"), ("water_m_vrm", "water", "vrm_t"),
                         ("water_m_dimm", "water", "dimm_t"), ("water_m_ssd", "water", "ssd_t"), ("cpucore_m_water", "cpu_core_t", "water"),
                         ("cpupkg_m_water", "cpu_pkg_t", "water"), ("gpu_m_water", "gpu_temp", "water"), ("gpuhot_m_water", "gpu_hot", "water"),
                         ("gpumem_m_water", "gpu_mem", "water"), ("cpucore_m_gpu", "cpu_core_t", "gpu_temp")]:
            self.lin(nm, [a, b], [1, -1], 0.0, "delta", True)
        if ("temperature", "P") in agg_out:
            self.lin("delta_P_mean_m_water", ["temperature_P_mean", "water"], [1, -1], 0.0, "delta", True)
            self.lin("delta_E_mean_m_water", ["temperature_E_mean", "water"], [1, -1], 0.0, "delta", True)
            self.lin("delta_PE_mean", ["temperature_P_mean", "temperature_E_mean"], [1, -1], 0.0, "delta", True)
        for t in (30, 600):
            for nm in ("cpucore_m_water", "gpu_m_water", "water_m_sys"):
                self.ewm(nm, t, "delta")
        self.lin("fan_frac", ["fan_duty"], [0.01], 0.0, "_", False)
        self.lin("fan_rpm_n", ["fan_rpm"], [1.0 / 1470.0], 0.0, "_", False)
        self.lin("pump_rpm_n", ["pump_rpm"], [1.0 / 4800.0], 0.0, "_", False)
        self.add("fan_rpm3", "pow", ["fan_rpm_n"], {"e": 3}, "fan_pump_terms", True)
        self.add("fan_rpm2", "pow", ["fan_rpm_n"], {"e": 2}, "fan_pump_terms", True)
        self.add("pump_rpm3", "pow", ["pump_rpm_n"], {"e": 3}, "fan_pump_terms", True)
        self.add("fan_x_dT", "mul", ["fan_frac", "water_minus_amb"], {}, "fan_pump_terms", True)
        self.add("fanrpm_x_dT", "mul", ["fan_rpm_n", "water_minus_amb"], {}, "fan_pump_terms", True)
        self.add("pump_x_dT", "mul", ["pump_rpm_n", "water_minus_amb"], {}, "fan_pump_terms", True)
        self.add("fan_x_ptotal", "mul", ["fan_frac", "p_total"], {}, "fan_pump_terms", True)
        self.add("fan_sat", "gt", ["fan_duty"], {"c": 98.0}, "fan_pump_terms", True)
        self.lin("fan_duty_minus_rpm", ["fan_frac", "fan_rpm_n"], [1, -1], 0.0, "fan_pump_terms", True)
        for t in (30, 600):
            self.ewm("fan_x_dT", t, "fan_pump_terms")
            self.ewm("fan_rpm3", t, "fan_pump_terms")
        h = cfg["heat_flag_w"]
        self.add("reg_idle", "gt", ["p_total.ewm30"], {"c": h["idle"], "inv": True}, "regime", True)
        self.lin("gpu_dom", ["gpu_w.ewm30", "cpu_pkg_w.ewm30"], [1, -1], 0.0, "_", False)
        self.lin("cpu_dom", ["cpu_pkg_w.ewm30", "gpu_w.ewm30"], [1, -1], 0.0, "_", False)
        self.add("reg_gpu_dom", "gt", ["gpu_dom"], {"c": h["dominance"]}, "regime", True)
        self.add("reg_cpu_dom", "gt", ["cpu_dom"], {"c": h["dominance"]}, "regime", True)
        self.add("reg_gpu_heavy", "gt", ["gpu_w.ewm30"], {"c": h["heavy"]}, "regime", True)
        self.add("reg_cpu_heavy", "gt", ["cpu_pkg_w.ewm30"], {"c": h["heavy"]}, "regime", True)
        self.add("reg_pump_low", "gt", ["pump_duty"], {"c": 60.0, "inv": True}, "regime", True)
        self.add("reg_fan_below", "gt", ["fan_duty"], {"c": 95.0, "inv": True}, "regime", True)
        for nm, src in [("tsc_ptotal", "p_total.ewm5"), ("tsc_gpu", "gpu_w.ewm5"), ("tsc_cpu", "cpu_pkg_w.ewm5")]:
            self.add(nm, "tsc", [src], {"thr": cfg["tsc_thr_w"], "n": cfg["tsc_lag_s"], "cap": cfg["tsc_cap"]}, "time_since", True)
        self.add("tsc_water", "tsc", ["water.ewm5"], {"thr": 0.15, "n": 60, "cap": cfg["tsc_cap"]}, "time_since", True)
        self.add("hour_sin", "hour_sin", [], {}, "time_of_day", True)
        self.add("hour_cos", "hour_cos", [], {}, "time_of_day", True)
        self.add("mask_d5", "mask", ["water"], {}, "mask", True)
        self.add("mask_nvml", "mask", ["gpu_w"], {}, "mask", True)
        self.add("mask_lhm", "mask", ["cpu_pkg_w"], {}, "mask", True)
        self.add("mask_pump_pow", "mask", ["pump_pow"], {}, "mask", True)
        return Spec(self.nodes)


class Ops:
    @staticmethod
    def ffill(x, lim, zero_nan):
        x = np.where(x == 0, np.nan, x) if zero_nan else x
        n = len(x)
        idx = np.where(np.isfinite(x), np.arange(n), -1)
        np.maximum.accumulate(idx, out=idx)
        age = np.arange(n) - idx
        ok = (idx >= 0) & (age <= lim)
        return np.where(ok, x[np.maximum(idx, 0)], np.nan)

    @staticmethod
    def runs(fin):
        d = np.diff(np.r_[0, fin.astype(np.int8), 0])
        return np.where(d == 1)[0], np.where(d == -1)[0]


class BatchEngine:
    def __init__(self, spec):
        self.spec = spec

    def run(self, X, ix, k0, keep=None):
        v = {}
        T = X.shape[0]
        for n in self.spec.nodes:
            a = [v[i] for i in n.ins]
            p = n.p
            if n.op == "raw":
                col = ix.get(p["channel"])
                x = np.full(T, np.nan) if col is None else np.asarray(X[:, col], dtype=np.float64)
                v[n.name] = Ops.ffill(x, p["ffill"], p["zero_nan"])
            elif n.op == "lin":
                y = np.full(T, float(p["off"]))
                for c, s in zip(p["coef"], a):
                    y = y + c * s
                v[n.name] = y
            elif n.op == "scale":
                v[n.name] = a[0] * p["c"]
            elif n.op == "mul":
                v[n.name] = a[0] * a[1]
            elif n.op == "pow":
                v[n.name] = a[0] ** p["e"]
            elif n.op in ("gmax", "gmin", "gmean", "gspread"):
                M = np.vstack(a)
                with np.errstate(invalid="ignore"):
                    if n.op == "gmax":
                        y = M.max(0)
                    elif n.op == "gmin":
                        y = M.min(0)
                    elif n.op == "gmean":
                        y = M.sum(0) / M.shape[0]
                    else:
                        y = M.max(0) - M.min(0)
                v[n.name] = np.where(np.isfinite(M).all(0), y, np.nan)
            elif n.op == "ewm":
                v[n.name] = self.ewm(a[0], p["tau"])
            elif n.op == "diff":
                y = np.full(T, np.nan)
                y[p["n"]:] = a[0][p["n"]:] - a[0][: T - p["n"]]
                v[n.name] = y
            elif n.op == "gt":
                x = a[0]
                r = (x < p["c"]) if p.get("inv") else (x > p["c"])
                v[n.name] = np.where(np.isfinite(x), r.astype(np.float64), np.nan)
            elif n.op == "mask":
                v[n.name] = np.isfinite(a[0]).astype(np.float64)
            elif n.op == "leakymin":
                v[n.name] = self.leakymin(a[0], p["rise"])
            elif n.op == "tsc":
                v[n.name] = self.tsc(a[0], p)
            elif n.op in ("hour_sin", "hour_cos"):
                ang = 2 * np.pi * (((k0 + np.arange(T)) % 86400) / 86400.0)
                v[n.name] = np.sin(ang) if n.op == "hour_sin" else np.cos(ang)
            else:
                raise KeyError(n.op)
        return v

    @staticmethod
    def ewm(x, tau):
        alpha = 1.0 - math.exp(-1.0 / tau)
        warm = max(1, int(tau))
        out = np.full(len(x), np.nan)
        s, e = Ops.runs(np.isfinite(x))
        for i, j in zip(s, e):
            seg = x[i:j]
            y = lfilter([alpha], [1.0, -(1.0 - alpha)], seg, zi=[(1.0 - alpha) * seg[0]])[0]
            y[: warm - 1] = np.nan
            out[i:j] = y
        return out

    @staticmethod
    def leakymin(x, rise):
        out = np.full(len(x), np.nan)
        m = math.nan
        for t, xv in enumerate(x.tolist()):
            if math.isnan(xv):
                m = m + rise if not math.isnan(m) else math.nan
            else:
                m = xv if math.isnan(m) else min(xv, m + rise)
            out[t] = m
        return out

    @staticmethod
    def tsc(x, p):
        n, cap, thr = p["n"], p["cap"], p["thr"]
        T = len(x)
        d = np.full(T, np.nan)
        d[n:] = x[n:] - x[: T - n]
        ch = np.isfinite(d) & (np.abs(d) > thr)
        last = np.where(ch, np.arange(T), -1)
        np.maximum.accumulate(last, out=last)
        return np.where(last >= 0, np.minimum(np.arange(T) - last, cap), cap).astype(np.float64)


class StreamEngine:
    def __init__(self, spec):
        self.spec = spec
        self.state = {n.name: {} for n in spec.nodes}
        self.hist = {n.name: [] for n in spec.nodes}
        self.t = 0

    def step(self, row, k):
        v = {}
        for n in self.spec.nodes:
            a = [v[i] for i in n.ins]
            p, s = n.p, self.state[n.name]
            nan = math.nan
            if n.op == "raw":
                x = row.get(p["channel"], nan)
                if x is None or (p["zero_nan"] and x == 0):
                    x = nan
                if not math.isnan(x):
                    s["last"], s["age"] = x, 0
                    y = x
                else:
                    s["age"] = s.get("age", 10**9) + 1
                    y = s["last"] if "last" in s and s["age"] <= p["ffill"] else nan
            elif n.op == "lin":
                y = p["off"]
                for c, x in zip(p["coef"], a):
                    y = y + c * x
            elif n.op == "scale":
                y = a[0] * p["c"]
            elif n.op == "mul":
                y = a[0] * a[1]
            elif n.op == "pow":
                y = a[0] ** p["e"]
            elif n.op in ("gmax", "gmin", "gmean", "gspread"):
                if any(math.isnan(x) for x in a):
                    y = nan
                elif n.op == "gmax":
                    y = max(a)
                elif n.op == "gmin":
                    y = min(a)
                elif n.op == "gmean":
                    y = sum(a) / len(a)
                else:
                    y = max(a) - min(a)
            elif n.op == "ewm":
                x = a[0]
                if math.isnan(x):
                    s["n"], s["y"], y = 0, nan, nan
                else:
                    al = 1.0 - math.exp(-1.0 / p["tau"])
                    s["n"] = s.get("n", 0) + 1
                    s["y"] = x if s["n"] == 1 else al * x + (1.0 - al) * s["y"]
                    y = s["y"] if s["n"] >= max(1, int(p["tau"])) else nan
            elif n.op == "diff":
                h = self.hist[n.ins[0]]
                y = a[0] - h[-p["n"] - 1] if len(h) > p["n"] else nan
            elif n.op == "gt":
                y = nan if math.isnan(a[0]) else float((a[0] < p["c"]) if p.get("inv") else (a[0] > p["c"]))
            elif n.op == "mask":
                y = 0.0 if math.isnan(a[0]) else 1.0
            elif n.op == "leakymin":
                m = s.get("m", nan)
                if math.isnan(a[0]):
                    m = m + p["rise"] if not math.isnan(m) else nan
                else:
                    m = a[0] if math.isnan(m) else min(a[0], m + p["rise"])
                s["m"], y = m, m
            elif n.op == "tsc":
                h = self.hist[n.ins[0]]
                c = s.get("c", p["cap"])
                if len(h) > p["n"] and not math.isnan(a[0]) and not math.isnan(h[-p["n"] - 1]) and abs(a[0] - h[-p["n"] - 1]) > p["thr"]:
                    c = 0
                else:
                    c = min(c + 1, p["cap"])
                s["c"], y = c, float(c)
            elif n.op in ("hour_sin", "hour_cos"):
                ang = 2 * math.pi * ((k % 86400) / 86400.0)
                y = math.sin(ang) if n.op == "hour_sin" else math.cos(ang)
            else:
                raise KeyError(n.op)
            v[n.name] = y
            self.hist[n.name].append(y)
            if len(self.hist[n.name]) > 4000:
                del self.hist[n.name][:1000]
        return v


class FeatureFrame:
    def __init__(self, cfg=CONFIG):
        self.cfg = cfg
        self.dir = cfg["cache_dir"]

    def build_spec(self, path=None):
        meta, X = RawStore(self.cfg).load()
        kept, dropped = RawSelector(self.cfg).select(meta, X)
        spec = SpecBuilder(meta, kept, self.cfg).build()
        spec.meta = {
            "version": 1, "ffill_s": self.cfg["ffill_s"], "kept_raw": [c["channel"] for c in kept], "dropped_raw": dropped,
            "config": {k: self.cfg[k] for k in ("taus", "deriv_lags", "vrm_frac", "g_assumed_w_per_k", "heat_flag_w", "p_cores")},
            "inferred": ["amb_board_floor", "water_floor", "amb_est", "vrm_est_w", "water_minus_amb", "water_minus_wfloor"],
        }
        spec.save(path or self.cfg["spec_path"])
        return spec

    def compute(self, spec, X=None, ix=None, k0=None):
        if X is None:
            meta, X = RawStore(self.cfg).load()
            ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
            k0 = meta["k0"]
        v = BatchEngine(spec).run(X, ix, k0)
        names = spec.outputs()
        F = np.empty((X.shape[0], len(names)), dtype=np.float32)
        for j, nm in enumerate(names):
            F[:, j] = v[nm]
        return names, F, v

    def save(self, names, F, tag="rich"):
        np.save(os.path.join(self.dir, f"F_{tag}.npy"), F)
        with open(os.path.join(self.dir, f"F_{tag}.json"), "w") as f:
            json.dump(names, f)

    def load(self, tag="rich"):
        with open(os.path.join(self.dir, f"F_{tag}.json")) as f:
            names = json.load(f)
        return names, np.load(os.path.join(self.dir, f"F_{tag}.npy"), mmap_mode="r")


PHYS = {
    "m": 1.61, "gn_frac": 0.0, "rpm_full": 1460.0, "pump_full": 4800.0, "eta_cpu": 0.96, "eta_gpu": 1.0,
    "lags": [11, 22, 33], "taus": [5, 30, 120, 600], "slopes": [10, 30, 120], "fan_lag": 22, "fan_slope": 30,
    "pint": False, "pump_low_rpm": 3500.0,
    "pint_names": ["ph.q.l11", "ph.q.l22", "ph.q.l33", "ph.q.e5", "ph.q.e30", "ph.q.e120", "ph.q.e600", "ph.fx", "ph.fx.e30", "ph.fx.e120", "water_minus_amb", "water.e5.d10", "water.e5.d30", "water.e5.d120"],
}


class PhysSpec:
    def __init__(self, rich, cfg=PHYS):
        self.rich, self.cfg, self.new = rich, cfg, []

    def add(self, name, op, ins, p=None, out=True):
        self.new.append(Node(name, op, ins, p or {}, "phys", out))
        return name

    def lin(self, name, ins, coef, off=0.0, out=True):
        return self.add(name, "lin", ins, {"coef": coef, "off": off}, out)

    def ewm(self, x, tau, out=True):
        return self.add(f"{x}.e{tau}", "ewm", [x], {"tau": tau}, out)

    def lag(self, x, n):
        d = self.add(f"{x}.d{n}", "diff", [x], {"n": n}, False)
        return self.lin(f"{x}.l{n}", [x, d], [1.0, -1.0])

    def slope(self, x, n):
        return self.add(f"{x}.d{n}", "diff", [x], {"n": n})

    def build(self):
        c = self.cfg
        q = self.lin("ph.q", ["cpu_pkg_w", "gpu_w", "pump_pow"], [c["eta_cpu"], c["eta_gpu"], 1.0])
        for n in c["lags"]:
            self.lag(q, n)
        for t in c["taus"]:
            self.ewm(q, t)
        for s in ("cpu_pkg_w", "gpu_w"):
            for t in c["taus"][:2]:
                self.ewm(s, t)
        w5 = self.ewm("water", 5, False)
        for n in c["slopes"]:
            self.slope(w5, n)
        fr = self.add("ph.fr", "scale", ["fan_rpm"], {"c": 1.0 / c["rpm_full"]}, False)
        fm = self.add("ph.fm", "pow", [fr], {"e": c["m"]}, False)
        gf = self.lin("ph.gf", [fm], [1.0 - c["gn_frac"]], c["gn_frac"], False)
        fx = self.add("ph.fx", "mul", [gf, "water_minus_amb"])
        self.ewm(fx, 30)
        self.ewm(fx, 120)
        gl = self.lag(gf, c["fan_lag"])
        self.add("ph.fxl", "mul", [gl, "water_minus_amb"])
        gs = self.slope(gf, c["fan_slope"])
        self.add("ph.fxd", "mul", [gs, "water_minus_amb"])
        pr = self.add("ph.pr", "scale", ["pump_rpm"], {"c": 1.0 / c["pump_full"]}, False)
        pg = self.lin("ph.pg", [pr], [1.0], -1.0)
        self.add("ph.px", "mul", [pg, fx])
        self.add("ph.pgx", "mul", [pg, "water_minus_amb"])
        for s in ("cpu_core_t", "gpu_temp", "cpucore_m_water", "gpu_m_water"):
            self.ewm(s, 5)
        if c["pint"]:
            pl = self.add("ph.pl", "gt", ["pump_rpm"], {"c": c["pump_low_rpm"], "inv": True})
            for n in c["pint_names"]:
                self.add(f"{n}*pl", "mul", [n, pl])
        return Spec(list(self.rich.nodes) + self.new, dict(self.rich.meta))

    def names(self):
        base = ["cpu_pkg_w", "gpu_w", "pump_pow", "water", "water_minus_amb", "cpu_core_t", "gpu_temp", "cpucore_m_water", "gpu_m_water"]
        return base + [n.name for n in self.new if n.out]


if __name__ == "__main__":
    ff = FeatureFrame()
    spec = ff.build_spec()
    names, F, _ = ff.compute(spec)
    ff.save(names, F)
    print(len(spec.nodes), len(names), F.shape)
