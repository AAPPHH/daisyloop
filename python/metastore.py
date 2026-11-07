import bisect
import datetime
import hashlib
import json
import math
import os
import shutil
import struct
import sys
import time
import warnings

LOCAL = os.environ.get("LOCALAPPDATA", ".")
DIR = os.path.join(LOCAL, "daisyloop-ml", "metastore")


def log_line(cfg, text):
    try:
        os.makedirs(os.path.dirname(cfg["log"]), exist_ok=True)
        with open(cfg["log"], "a") as f:
            f.write(f"{datetime.datetime.now().isoformat(timespec='seconds')} {text}\n")
    except OSError:
        pass


try:
    import numpy as np
    import pandas as pd
    import pyarrow as pa
    import pyarrow.parquet as pq
except BaseException as e:
    if __name__ == "__main__":
        log_line({"log": os.path.join(DIR, "update.log")}, f"import failed {type(e).__name__}: {e}")
    raise

CONFIG = {
    "dir": DIR,
    "data_dir": os.environ.get("DAISYLOOP_DATA_DIR") or os.path.join(LOCAL, "daisyloop"),
    "log": os.path.join(DIR, "update.log"),
    "cache_dir": os.path.join(DIR, "cache"),
    "d5": {"table": "d5_raw", "width": 162, "level": 22},
    "budget_bytes": 2_000_000_000,
    "safety": 0.97,
    "chunk_s": 3600,
    "context_s": 0,
    "lag_s": 120,
    "day_s": 86400,
    "block_s": 900,
    "parquet": {"level": 22, "row_group": 2_000_000},
    "tol": {
        "rel_q": 0.5, "rel_sigma": 0.1, "q_pct": 10.0, "rel_float": 1e-6, "other_range_frac": 1e-3, "min_samples": 50, "sample_chunks": 8, "door_frac": 0.85,
        "floor": {"temperature": 0.01, "rpm": 1.0, "duty": 0.1, "power": 0.01, "voltage": 0.001, "current": 0.001, "load": 0.1, "clock": 1.0, "flow": 0.01, "other": 0.0},
        "preset": {"d5next/water/temp": 0.005},
    },
    "signals": {"water": "d5next/water/temp", "fan": "d5next/fan/rpm", "pump": "d5next/pump/rpm", "cpu_w": "/intelcpu/0/power/0", "gpu_w": "nvml/0/power", "phase": "daisyloop/step/phase"},
    "score": {"lag_s": 30, "fan": 1460.0 * 0.05, "pump": 4800.0 * 0.05, "p_total": 40.0, "slope_lag_s": 120, "slope": 0.3, "step_pre_s": 300, "step_post_s": 900, "step_gap_s": 120},
    "cells": [("water", [30.0, 32.0, 34.0, 36.0, 38.0]), ("fan", [100.0, 650.0, 1000.0, 1400.0]), ("pump", [3500.0]), ("p_total", [50.0, 100.0, 200.0, 300.0])],
    "thin": {"max_mult": 64.0, "ladder_max": 1024.0, "step": 2.0, "gain": 0.4, "gain_min": 0.01, "rare_blocks": 6, "protect_frac": 0.2, "need_frac": 0.6, "tries": 40, "hold_blocks": 2, "escalate": True},
}
POINT_COLS = ["c", "t", "v"]
GAP_COLS = ["c", "s", "n"]


class StoreError(RuntimeError):
    pass


class Tolerance:
    def __init__(self, cfg):
        self.cfg = cfg["tol"]

    def derive(self, name, kind, x):
        c = self.cfg
        if name in c["preset"]:
            return float(c["preset"][name])
        floor = c["floor"].get(kind, c["floor"]["other"])
        x = x[np.isfinite(x)]
        if len(x) < c["min_samples"]:
            return float(max(floor, c["rel_float"] * (np.abs(x).max() if len(x) else 0.0), 1e-9))
        d = np.diff(x)
        nz = np.abs(d[d != 0])
        q = float(np.percentile(nz, c["q_pct"])) if len(nz) else 0.0
        sig = 1.4826 * float(np.median(np.abs(d - np.median(d)))) / math.sqrt(2.0)
        tol = max(floor, c["rel_q"] * q, c["rel_sigma"] * sig)
        if kind not in c["floor"] or kind == "other":
            lo, hi = np.percentile(x, [0.1, 99.9])
            tol = max(tol, c["other_range_frac"] * float(hi - lo))
        return float(max(tol, c["rel_float"] * float(np.abs(x).max()), 1e-9))


class Door:
    NAMES = ("has", "at", "av", "lo", "hi", "lt", "lv")

    def __init__(self, n=0):
        self.has = np.zeros(n, bool)
        self.at = np.zeros(n, np.int64)
        self.lt = np.zeros(n, np.int64)
        self.av, self.lv = np.zeros(n), np.zeros(n)
        self.lo, self.hi = np.full(n, -np.inf), np.full(n, np.inf)

    @property
    def n(self):
        return len(self.has)

    def grow(self, n):
        if n <= self.n:
            return
        add = Door(n - self.n)
        for k in self.NAMES:
            setattr(self, k, np.concatenate([getattr(self, k), getattr(add, k)]))

    def arrays(self):
        return {k: getattr(self, k) for k in self.NAMES}

    @classmethod
    def from_arrays(cls, d):
        out = cls(len(d["has"]))
        for k in cls.NAMES:
            setattr(out, k, np.array(d[k]))
        return out

    def run(self, M, t0, tol):
        pts, gaps = [], []
        ok_rows = np.flatnonzero(np.isfinite(M).any(1))
        for i in ok_rows:
            tt = t0 + int(i)
            v = M[i]
            idx = np.flatnonzero(np.isfinite(v))
            x = v[idx].astype(np.float64)
            h = self.has[idx]
            if not h.all():
                ni = idx[~h]
                pts.append((ni, np.full(len(ni), tt, np.int64), x[~h]))
                self.has[ni] = True
                self.at[ni], self.av[ni] = tt, x[~h]
                self.lo[ni], self.hi[ni] = -np.inf, np.inf
            ci, cx = idx[h], x[h]
            if len(ci):
                gl = tt - self.lt[ci]
                gm = gl > 1
                if gm.any():
                    gaps.append((ci[gm], self.lt[ci][gm] + 1, gl[gm] - 1))
                s = (cx - self.av[ci]) / (tt - self.at[ci])
                br = ~((s >= self.lo[ci]) & (s <= self.hi[ci]))
                if br.any():
                    b = ci[br]
                    pts.append((b, self.lt[b].copy(), self.lv[b].copy()))
                    self.at[b], self.av[b] = self.lt[b], self.lv[b]
                    self.lo[b], self.hi[b] = -np.inf, np.inf
                dt = (tt - self.at[ci]).astype(np.float64)
                tl = tol[ci]
                self.lo[ci] = np.maximum(self.lo[ci], (cx - tl - self.av[ci]) / dt)
                self.hi[ci] = np.minimum(self.hi[ci], (cx + tl - self.av[ci]) / dt)
            self.lt[idx], self.lv[idx] = tt, x
        return self.pack(pts, 3, (np.int32, np.int64, np.float32)), self.pack(gaps, 3, (np.int32, np.int64, np.int64))

    @staticmethod
    def pack(parts, n, types):
        if not parts:
            return tuple(np.zeros(0, t) for t in types)
        return tuple(np.concatenate([p[j] for p in parts]).astype(types[j]) for j in range(n))

    def flush(self):
        idx = np.flatnonzero(self.has & (self.lt > self.at))
        return idx.astype(np.int32), self.lt[idx].copy(), self.lv[idx].astype(np.float32)


class Curves:
    def __init__(self, ids, pts, gaps):
        self.pts, self.gaps = {}, {}
        for src, dst in ((pts, self.pts), (gaps, self.gaps)):
            c = src[0]
            order = np.argsort(c, kind="stable")
            cs = c[order]
            cut = np.flatnonzero(np.r_[True, cs[1:] != cs[:-1], True]) if len(cs) else np.zeros(0, np.int64)
            for a, b in zip(cut[:-1], cut[1:]):
                dst[int(cs[a])] = tuple(col[order[a:b]] for col in src[1:])

    def at(self, c, grid, masked=True):
        out = np.full(len(grid), np.nan)
        if c not in self.pts:
            return out
        t, v = self.pts[c]
        o = np.argsort(t, kind="stable")
        out = np.interp(grid, t[o], v[o].astype(np.float64), left=np.nan, right=np.nan)
        if masked and c in self.gaps:
            s, n = self.gaps[c]
            d = np.zeros(len(grid) + 1, np.int32)
            a = np.clip(s - grid[0], 0, len(grid))
            b = np.clip(s + n - grid[0], 0, len(grid))
            np.add.at(d, a, 1)
            np.add.at(d, b, -1)
            out[np.cumsum(d[:-1]) > 0] = np.nan
        return out


class Edges:
    def __init__(self, days, prev, nxt, first, last):
        self.days, self.prev, self.nxt, self.first, self.last = days, prev, nxt, first, last

    def around(self, day):
        i = bisect.bisect_left(self.days, day)
        if i < len(self.days) and self.days[i] == day:
            return self.prev[day], self.nxt[day]
        return (self.prev[self.days[i]] if i < len(self.days) else self.last), (self.nxt[self.days[i - 1]] if i > 0 else self.first)


class LakeReader:
    def __init__(self, lake=None, cfg=CONFIG):
        from lake import RangeLake, CONFIG as LCFG
        self.lake = lake or RangeLake(dict(LCFG, data_dir=cfg["data_dir"]))

    def read(self, a, b):
        con = self.lake.refresh(a, b).con
        w = "ts >= to_timestamp(?) and ts < to_timestamp(?)"
        rows = con.execute(f"select cast(floor(epoch(ts)) as bigint) k, channel, avg(value) v from telemetry where {w} group by all", [float(a), float(b)]).fetchnumpy()
        meta = con.execute(f"select channel, any_value(source), any_value(label), any_value(kind), any_value(unit), any_value(role) from telemetry where {w} group by channel", [float(a), float(b)]).fetchall()
        return rows, {m[0]: m[1:] for m in meta}

    def read_d5(self, a, b, width):
        con = self.lake.refresh(a, b, ("d5_raw",)).con
        r = con.execute("select cast(epoch_us(ts) as bigint) t, report from d5_raw where ts >= to_timestamp(?) and ts < to_timestamp(?) order by ts", [float(a), float(b)]).fetchnumpy()
        ok = np.fromiter((len(x) == width for x in r["report"]), bool, len(r["t"]))
        t = r["t"][ok].astype(np.int64)
        return t, (np.frombuffer(b"".join(r["report"][ok]), np.uint8).reshape(len(t), width) if len(t) else np.zeros((0, width), np.uint8)), int((~ok).sum())

    def edge(self, fn, a=None):
        con = self.lake.refresh(a, None).con
        w, args = ("where ts >= to_timestamp(?)", [float(a)]) if a is not None else ("", [])
        v = con.execute(f"select {fn}(epoch(ts)) from telemetry {w}", args).fetchone()[0]
        return None if v is None else int(v)

    def latest(self, a=None):
        return self.edge("max", a)

    def earliest(self):
        return self.edge("min")


class HistoryStore:
    META = ("channel", "source", "label", "kind", "unit", "role")

    def __init__(self, cfg=CONFIG):
        self.cfg = cfg
        self.dir = cfg["dir"]
        self.chan, self.k0, self.watermark = [], None, None
        self.door = Door()
        self.tolerance = Tolerance(cfg)
        self.path = lambda n: os.path.join(self.dir, n)
        self.restore()

    def restore(self):
        if not os.path.exists(self.path("channels.json")):
            return
        with open(self.path("channels.json")) as f:
            m = json.load(f)
        self.chan, self.k0, self.watermark = m["channels"], m["k0"], m["watermark"]
        with np.load(self.path("state.npz")) as z:
            self.door = Door.from_arrays({k: z[k] for k in Door.NAMES})
        self.door.grow(len(self.chan))

    def commit(self):
        os.makedirs(self.dir, exist_ok=True)
        tmp = self.path("channels.json.tmp")
        with open(tmp, "w") as f:
            json.dump({"channels": self.chan, "k0": self.k0, "watermark": self.watermark}, f)
        np.savez(self.path("state.tmp.npz"), **self.door.arrays())
        os.replace(self.path("state.tmp.npz"), self.path("state.npz"))
        os.replace(tmp, self.path("channels.json"))

    @property
    def index(self):
        return {c["channel"]: c["id"] for c in self.chan}

    def tol(self):
        return self.cfg["tol"]["door_frac"] * np.array([c["tol"] for c in self.chan] + [1.0] * (self.door.n - len(self.chan)))

    EXT = {"points": ".parquet", "gaps": ".parquet", "d5": ".bin"}

    def files(self, kind):
        if not os.path.isdir(self.dir):
            return {}
        pre, ext = kind + "-", self.EXT[kind]
        return {int(n[len(pre):-len(ext)]): self.path(n) for n in sorted(os.listdir(self.dir)) if n.startswith(pre) and n.endswith(ext)}

    def size(self, kinds=("points", "gaps", "d5")):
        return sum(os.path.getsize(p) for k in kinds for p in self.files(k).values())

    def pack_d5(self, t, M):
        o = np.argsort(t, kind="stable")
        t, M = t[o], M[o]
        _, first = np.unique(t, return_index=True)
        t, M = t[first], M[first]
        u = np.r_[True, (M[1:] != M[:-1]).any(1)]
        B = M[u]
        D = B.copy()
        D[1:] = B[1:] - B[:-1]
        dt = np.r_[t[:1], np.diff(t)].astype("<i8")
        body = dt.view(np.uint8).reshape(len(t), 8).T.tobytes() + np.packbits(u).tobytes() + np.ascontiguousarray(D.T).tobytes()
        return struct.pack("<QQQQ", len(t), len(B), M.shape[1], len(body)) + pa.Codec("zstd", compression_level=self.cfg["d5"]["level"]).compress(body, asbytes=True)

    @staticmethod
    def unpack_d5(raw):
        n, nu, w, size = struct.unpack("<QQQQ", raw[:32])
        body = np.frombuffer(pa.Codec("zstd").decompress(raw[32:], decompressed_size=size, asbytes=True), np.uint8)
        a, b = 8 * n, 8 * n + (n + 7) // 8
        t = np.cumsum(np.ascontiguousarray(body[:a].reshape(8, n).T).view("<i8").reshape(n))
        u = np.unpackbits(body[a:b])[:n].astype(bool)
        B = np.cumsum(body[b:].reshape(w, nu).T, axis=0, dtype=np.uint8)
        return t, B[np.cumsum(u) - 1]

    def d5_day(self, day):
        if day not in self.files("d5"):
            return None
        with open(self.files("d5")[day], "rb") as f:
            return self.unpack_d5(f.read())

    def append_d5(self, t, M):
        ds = self.cfg["day_s"]
        days = t // 1_000_000 // ds
        for day in np.unique(days):
            m = days == day
            cur = [t[m], M[m]]
            old = self.d5_day(int(day))
            if old is not None:
                cur = [np.concatenate([o, c]) for o, c in zip(old, cur)]
            os.makedirs(self.dir, exist_ok=True)
            path = self.path(f"d5-{int(day)}.bin")
            with open(path + ".tmp", "wb") as f:
                f.write(self.pack_d5(*cur))
            os.replace(path + ".tmp", path)

    def reports(self, k0=None, k1=None):
        w = self.cfg["d5"]["width"]
        parts = [self.d5_day(d) for d in self.files("d5")]
        t = np.concatenate([p[0] for p in parts]) if parts else np.zeros(0, np.int64)
        M = np.concatenate([p[1] for p in parts]) if parts else np.zeros((0, w), np.uint8)
        m = np.ones(len(t), bool)
        if k0 is not None:
            m &= t >= k0 * 1_000_000
        if k1 is not None:
            m &= t < (k1 + 1) * 1_000_000
        return t[m], M[m]

    def write(self, table, path, cols, enc):
        pc = self.cfg["parquet"]
        os.makedirs(self.dir, exist_ok=True)
        tmp = path + ".tmp"
        pq.write_table(table, tmp, compression="zstd", compression_level=pc["level"], row_group_size=pc["row_group"], use_dictionary=[c for c in cols if c not in enc],
                       column_encoding={c: e for c, e in enc.items()}, write_statistics=False)
        os.replace(tmp, path)

    def read(self, kind, day, cols=None):
        return pq.read_table(self.files(kind)[day], columns=cols)

    def save_points(self, day, c, t, v):
        o = np.lexsort((t, c))
        tab = pa.table({"c": pa.array(c[o].astype(np.uint16)), "t": pa.array(t[o].astype(np.int64)), "v": pa.array(v[o].astype(np.float32))})
        self.write(tab, self.path(f"points-{day}.parquet"), POINT_COLS, {"t": "DELTA_BINARY_PACKED", "v": "BYTE_STREAM_SPLIT"})

    def save_gaps(self, day, c, s, n):
        o = np.lexsort((s, c))
        tab = pa.table({"c": pa.array(c[o].astype(np.uint16)), "s": pa.array(s[o].astype(np.int64)), "n": pa.array(n[o].astype(np.int32))})
        self.write(tab, self.path(f"gaps-{day}.parquet"), GAP_COLS, {"s": "DELTA_BINARY_PACKED", "n": "DELTA_BINARY_PACKED"})

    def day_arrays(self, kind, day):
        if day not in self.files(kind):
            return None
        t = self.read(kind, day)
        return tuple(t.column(n).to_numpy().astype(np.int64 if n != "v" else np.float32) for n in t.column_names)

    def append(self, pts, gaps):
        ds = self.cfg["day_s"]
        for kind, data, save in (("points", pts, self.save_points), ("gaps", gaps, self.save_gaps)):
            if not len(data[0]):
                continue
            days = data[1] // ds
            for day in np.unique(days):
                m = days == day
                cur = [x[m] for x in data]
                old = self.day_arrays(kind, int(day))
                if old is not None:
                    cur = [np.concatenate([o, c]) for o, c in zip(old, cur)]
                key = cur[0].astype(np.int64) * (1 << 40) + cur[1]
                _, first = np.unique(key, return_index=True)
                save(int(day), *[x[first] for x in cur])

    @staticmethod
    def by_channel(rows, into=None):
        o = np.argsort(rows["k"], kind="stable")
        out = {} if into is None else into
        for n, g in pd.Series(rows["v"][o]).groupby(rows["channel"][o]):
            out.setdefault(n, []).append(g.to_numpy())
        return out

    def sample_tol(self, reader, a, b):
        t = self.cfg["tol"]
        got = {}
        span = max((b - a) // t["sample_chunks"], 1)
        for j in range(t["sample_chunks"]):
            lo = a + j * span
            self.by_channel(reader.read(lo, min(lo + self.cfg["chunk_s"], b))[0], got)
        return got

    def register(self, meta, sample=None):
        new = []
        for n in sorted(meta):
            if n in self.index:
                continue
            x = np.concatenate((sample or {}).get(n) or [np.zeros(0)])
            c = dict(zip(self.META, (n,) + tuple(meta[n])))
            c["id"], c["tol"] = len(self.chan), self.tolerance.derive(n, c["kind"], x)
            self.chan.append(c)
            new.append(c)
        self.door.grow(len(self.chan))
        return new

    def matrix(self, rows, a, b):
        M = np.full((b - a, len(self.chan)), np.nan, np.float32)
        if len(rows["k"]):
            codes, uniq = pd.factorize(rows["channel"])
            ix = self.index
            col = np.array([ix[u] for u in uniq])[codes]
            M[rows["k"] - a, col] = rows["v"]
        return M

    def update(self, reader=None):
        cfg = self.cfg
        reader = reader or LakeReader(cfg=cfg)
        start = None if self.watermark is None else self.watermark + 1
        latest = reader.latest(start)
        info = {"new_s": 0, "points": 0, "gaps": 0, "d5_rows": 0, "d5_skipped": 0, "new_channels": 0}
        if latest is None:
            return self.finish(info)
        end = latest - cfg["lag_s"]
        if start is None:
            start = reader.earliest()
        if end < start:
            return self.finish(info)
        first_build = self.watermark is None
        all_p, all_g, all_d = [], [], []
        step = cfg["chunk_s"]
        total = [0, 0, 0, 0]
        w5 = cfg["d5"]["width"]

        def flush():
            if all_p:
                pts = tuple(np.concatenate([p[j] for p in all_p]) for j in range(3))
                gaps = tuple(np.concatenate([g[j] for g in all_g]) for j in range(3))
                self.append(pts, gaps)
                total[0] += len(pts[0])
                total[1] += len(gaps[0])
                all_p.clear()
                all_g.clear()
            if all_d:
                t5, M5 = np.concatenate([x[0] for x in all_d]), np.concatenate([x[1] for x in all_d])
                self.append_d5(t5, M5)
                total[2] += len(t5)
                all_d.clear()
        a = start
        while a <= end:
            b = min((a // step + 1) * step, end + 1)
            rows, meta = reader.read(a - cfg["context_s"], b)
            keep = rows["k"] >= a
            rows = {k: v[keep] for k, v in rows.items()}
            missing = {n: m for n, m in meta.items() if n not in self.index}
            if missing:
                sample = self.sample_tol(reader, a, end + 1) if first_build and a == start else self.by_channel(rows)
                info["new_channels"] += len(self.register(missing, sample))
            if self.k0 is None and len(rows["k"]):
                self.k0 = int(rows["k"].min())
            p, g = self.door.run(self.matrix(rows, a, b), a, self.tol())
            all_p.append(p)
            all_g.append(g)
            if hasattr(reader, "read_d5"):
                t5, M5, skipped = reader.read_d5(a, b, w5)
                all_d.append((t5, M5))
                total[3] += skipped
            if b % cfg["day_s"] == 0:
                flush()
            a = b
        flush()
        self.watermark = end
        info.update(new_s=end - start + 1, points=total[0], gaps=total[1], d5_rows=total[2], d5_skipped=total[3])
        return self.finish(info)

    def finish(self, info):
        if self.chan:
            self.commit()
        info.update(watermark=self.watermark, bytes=self.size(), bytes_telemetry=self.size(("points", "gaps")), bytes_d5=self.size(("d5",)), channels=len(self.chan))
        return info

    def curves(self, names=None):
        ix = self.index
        want = None if names is None else {ix[n] for n in names if n in ix}
        parts = {"points": [], "gaps": []}
        for kind, cols in (("points", ["c", "t", "v"]), ("gaps", ["c", "s", "n"])):
            for day in self.files(kind):
                tab = self.read(kind, day, cols)
                c = tab.column("c").to_numpy()
                m = np.isin(c, list(want)) if want is not None else slice(None)
                parts[kind].append([tab.column(n).to_numpy()[m] for n in cols])
        p = [np.concatenate([x[j] for x in parts["points"]]) if parts["points"] else np.zeros(0) for j in range(3)]
        g = [np.concatenate([x[j] for x in parts["gaps"]]) if parts["gaps"] else np.zeros(0) for j in range(3)]
        fi, ft, fv = self.door.flush()
        if want is not None:
            m = np.isin(fi, list(want))
            fi, ft, fv = fi[m], ft[m], fv[m]
        p = [np.concatenate([p[0], fi]), np.concatenate([p[1], ft]), np.concatenate([p[2], fv])]
        return Curves(ix, p, g)

    def edges(self):
        days = sorted(self.files("points"))
        n = len(self.chan)
        prev, last_t, last_v = {}, np.full(n, -1, np.int64), np.zeros(n)
        first = {}
        for d in days:
            tab = self.read("points", d)
            c, t, v = (tab.column(k).to_numpy() for k in POINT_COLS)
            prev[d] = (last_t.copy(), last_v.copy())
            u, a = np.unique(c, return_index=True)
            e = np.r_[a[1:], len(c)] - 1
            first[d] = (u, t[a], v[a].astype(np.float64))
            last_t[u], last_v[u] = t[e], v[e]
        nxt, nt, nv = {}, np.full(n, -1, np.int64), np.zeros(n)
        for d in reversed(days):
            nxt[d] = (nt.copy(), nv.copy())
            u, ft, fv = first[d]
            nt[u], nv[u] = ft, fv
        return Edges(days, prev, nxt, (nt, nv), (last_t, last_v))

    def signature(self, names, a, b):
        stat = [(k, d, os.stat(p).st_size, os.stat(p).st_mtime_ns) for k in ("points", "gaps") for d, p in self.files(k).items()]
        req = hashlib.sha1(json.dumps([names, a, b]).encode()).hexdigest()[:12]
        return req, hashlib.sha1(json.dumps([stat, self.watermark, self.k0, [c["tol"] for c in self.chan], self.door.lt.tolist(), self.door.at.tolist()]).encode()).hexdigest()[:12]

    def build_cache(self, path, names, a, b):
        ix = self.index
        cols = np.array([ix[n] for n in names])
        D = self.cfg["day_s"]
        X = np.lib.format.open_memmap(path + ".tmp", mode="w+", dtype=np.float32, shape=(b - a + 1, len(names)))
        E = self.edges()
        fi, ft, fv = self.door.flush()
        gdays, pdays = sorted(self.files("gaps")), self.files("points")
        carry = [np.zeros(0, np.int64)] * 3
        gi = 0
        rows = 0
        for day in range(a // D, b // D + 1):
            lo, hi = max(day * D, a), min((day + 1) * D, b + 1)
            while gi < len(gdays) and gdays[gi] <= day:
                tab = self.read("gaps", gdays[gi])
                new = [tab.column(k).to_numpy().astype(np.int64) for k in GAP_COLS]
                carry = [np.concatenate([x, y]) for x, y in zip(carry, new)]
                keep = carry[1] + carry[2] > lo
                carry = [x[keep] for x in carry]
                gi += 1
            gm = np.isin(carry[0], cols) & (carry[1] < hi)
            g = [x[gm] for x in carry]
            ptab = pq.read_table(pdays[day]) if day in pdays else None
            pc, pt, pv = ([ptab.column(k).to_numpy() for k in POINT_COLS] if ptab is not None else [np.zeros(0, np.int64)] * 3)
            pr, nx = E.around(day)
            hp, hn = np.flatnonzero(pr[0] >= 0), np.flatnonzero(nx[0] >= 0)
            c = np.concatenate([pc, hp, hn, fi])
            t = np.concatenate([pt, pr[0][hp], nx[0][hn], ft])
            v = np.concatenate([pv.astype(np.float64), pr[1][hp], nx[1][hn], fv.astype(np.float64)])
            cv = Curves(None, [c, t, v], g)
            grid = np.arange(lo, hi, dtype=np.int64)
            for j, ch in enumerate(cols):
                X[lo - a: hi - a, j] = cv.at(int(ch), grid)
            rows += int(np.isin(pc, cols).sum())
            carry = [x[carry[1] + carry[2] > hi] for x in carry]
        X.flush()
        del X
        os.replace(path + ".tmp", path)
        return rows

    def load(self, channels=None, k0=None, k1=None):
        if self.watermark is None:
            raise StoreError("the history store is empty")
        names = sorted(c["channel"] for c in self.chan) if channels is None else sorted(set(channels) & set(self.index))
        a, b = max(self.k0, k0 if k0 is not None else self.k0), min(self.watermark, k1 if k1 is not None else self.watermark)
        req, sig = self.signature(names, int(a), int(b))
        cd = self.cfg["cache_dir"]
        os.makedirs(cd, exist_ok=True)
        path, info = os.path.join(cd, f"load-{req}-{sig}.npy"), os.path.join(cd, f"load-{req}-{sig}.json")
        if not (os.path.exists(path) and os.path.exists(info)):
            rows = self.build_cache(path, names, int(a), int(b))
            with open(info, "w") as f:
                json.dump({"rows": rows}, f)
            for n in os.listdir(cd):
                if n.startswith(f"load-{req}-") and not n.startswith(f"load-{req}-{sig}."):
                    try:
                        os.remove(os.path.join(cd, n))
                    except OSError:
                        pass
        with open(info) as f:
            rows = json.load(f)["rows"]
        by = {c["channel"]: c for c in self.chan}
        meta = {"rows": rows, "k0": int(a), "k1": int(b), "channels": [{k: by[n][k] for k in self.META} for n in names], "tol": [by[n]["tol"] for n in names]}
        return meta, np.load(path, mmap_mode="c")

    def tolerances(self):
        return {c["channel"]: c["tol"] for c in self.chan}


def main(argv, cfg=CONFIG):
    cmd = argv[0] if argv else "update"
    t0 = time.time()
    try:
        import psutil
        psutil.Process().nice(psutil.BELOW_NORMAL_PRIORITY_CLASS)
        st = HistoryStore(cfg)
        if cmd == "update":
            info = st.update()
            info.update(Thinner(st).enforce())
            text = "update ok " + " ".join(f"{k}={v}" for k, v in info.items() if not isinstance(v, (dict, list)))
        else:
            raise StoreError(f"unknown command {cmd}")
    except BaseException as e:
        log_line(cfg, f"{cmd} failed {type(e).__name__}: {e} seconds={time.time() - t0:.0f}")
        raise
    log_line(cfg, f"{text} seconds={time.time() - t0:.0f}")
    print(text)


class BlockScore:
    def __init__(self, st):
        self.st = st

    def signals(self, cv, grid):
        ix, sig = self.st.index, self.st.cfg["signals"]
        return {k: (cv.at(ix[n], grid) if n in ix else np.full(len(grid), np.nan)) for k, n in sig.items()}

    def compute(self):
        st = self.st
        cfg, sc = st.cfg, st.cfg["score"]
        B, D = cfg["block_s"], cfg["day_s"]
        cv = st.curves(list(cfg["signals"].values()))
        b0, b1 = st.k0 // B, (st.watermark + 1) // B
        nb = b1 - b0
        score, med, fin = np.zeros(nb), np.full((nb, len(cfg["cells"])), np.nan), []
        L, H = sc["lag_s"], sc["slope_lag_s"]
        pad = max(L, H)
        for d0 in range(b0 * B // D * D, b1 * B, D):
            lo, hi = max(d0, b0 * B), min(d0 + D, b1 * B)
            if hi <= lo:
                continue
            s = self.signals(cv, np.arange(lo - pad, hi, dtype=np.int64))
            fin.append(np.isfinite(s["phase"][pad:]))
            p = s["cpu_w"] + s["gpu_w"]
            ch = lambda x, n: np.nan_to_num(np.abs(x[pad:] - x[pad - n: len(x) - n]))
            comp = np.column_stack([ch(s["fan"], L) / sc["fan"], ch(s["pump"], L) / sc["pump"], ch(p, L) / sc["p_total"], ch(s["water"], H) / sc["slope"]])
            nbk = (hi - lo) // B
            a = (lo - b0 * B) // B
            score[a: a + nbk] = comp.sum(1).reshape(nbk, B).mean(1)
            vals = {"water": s["water"], "fan": s["fan"], "pump": s["pump"], "p_total": p}
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")
                for j, (key, _) in enumerate(cfg["cells"]):
                    med[a: a + nbk, j] = np.nanmedian(vals[key][pad:].reshape(nbk, B), axis=1)
        on = np.flatnonzero(np.concatenate(fin)) if fin else np.zeros(0, np.int64)
        step = np.zeros(nb, bool)
        if len(on):
            cut = np.flatnonzero(np.diff(on) > sc["step_gap_s"])
            for u, w in zip(np.r_[on[0], on[cut + 1]], np.r_[on[cut], on[-1]]):
                x0, x1 = b0 * B + u - sc["step_pre_s"], b0 * B + w + sc["step_post_s"]
                step[max(x0 // B - b0, 0): max(x1 // B - b0 + 1, 0)] = True
        code = np.zeros(nb, np.int64)
        for j, (_, edges) in enumerate(cfg["cells"]):
            bj = np.where(np.isfinite(med[:, j]), 1 + np.searchsorted(edges, np.nan_to_num(med[:, j]), side="right"), 0)
            code = code * (len(edges) + 2) + bj
        _, inv, cnt = np.unique(code, return_inverse=True, return_counts=True)
        return {"b0": b0, "score": score, "step": step, "cell": code, "cell_n": cnt[inv]}


class Thinner:
    def __init__(self, st, budget=None):
        self.st, self.cfg = st, st.cfg["thin"]
        self.budget = st.cfg["budget_bytes"] if budget is None else budget
        self.path = st.path("blocks.json")
        self.mult = {}
        if os.path.exists(self.path):
            with open(self.path) as f:
                self.mult = {int(k): v for k, v in json.load(f)["mult"].items()}

    def save(self):
        tmp = self.path + ".tmp"
        with open(tmp, "w") as f:
            json.dump({"block_s": self.st.cfg["block_s"], "mult": {str(k): v for k, v in sorted(self.mult.items())}}, f)
        os.replace(tmp, self.path)

    def density(self):
        B = self.st.cfg["block_s"]
        out = {}
        for d in self.st.files("points"):
            t = self.st.read("points", d, ["t"]).column("t").to_numpy()
            u, n = np.unique(t // B, return_counts=True)
            out.update(zip(u.tolist(), n.tolist()))
        return out

    def thin_day(self, day, spans, prev, nxt, tol):
        st = self.st
        c, t, v = st.day_arrays("points", day)
        hp, hn = np.flatnonzero(prev[0] >= 0), np.flatnonzero(nxt[0] >= 0)
        cv = Curves(None, [np.concatenate([c, hp, hn]), np.concatenate([t, prev[0][hp], nxt[0][hn]]), np.concatenate([v.astype(np.float64), prev[1][hp], nxt[1][hn]])], [np.zeros(0, np.int64)] * 3)
        g = st.day_arrays("gaps", day)
        if g is not None:
            cv.gaps = Curves(None, [np.zeros(0, np.int64)] * 3, list(g)).gaps
        removed = np.zeros(len(c), bool)
        add = []
        n = len(st.chan)
        for a, b, mo, mn in spans:
            grid = np.arange(a, b, dtype=np.int64)
            ok = np.flatnonzero(st.door.has[:n] & (st.door.at[:n] >= b))
            if not len(ok):
                continue
            M = np.full((len(grid), len(ok)), np.nan, np.float32)
            for j, ch in enumerate(ok):
                col = cv.at(int(ch), grid)
                col[[0, -1]] = cv.at(int(ch), grid[[0, -1]], masked=False)
                M[:, j] = col
            door = Door(len(ok))
            p, _ = door.run(M, a, (mn - mo) * tol[ok])
            fi, ft, fv = door.flush()
            removed |= np.isin(c, ok) & (t >= a) & (t < b)
            add.append((ok[np.concatenate([p[0], fi])], np.concatenate([p[1], ft]), np.concatenate([p[2], fv])))
        if not add:
            return 0
        keep = ~removed
        nc = np.concatenate([c[keep]] + [x[0] for x in add])
        nt = np.concatenate([t[keep]] + [x[1] for x in add])
        nv = np.concatenate([v[keep]] + [x[2] for x in add])
        st.save_points(day, nc, nt, nv)
        return len(c) - len(nc)

    def apply(self, picks, cap):
        st = self.st
        B, D, cfg = st.cfg["block_s"], st.cfg["day_s"], self.cfg
        E = self.st.edges()
        tol = st.tol()
        by_day = {}
        for blk in sorted(picks):
            by_day.setdefault(blk * B // D, []).append(blk)
        removed = 0
        for day, blks in by_day.items():
            spans = []
            for blk in blks:
                mo = self.mult.get(blk, 1.0)
                mn = min(mo * cfg["step"], cap)
                if spans and spans[-1][1] == blk * B and spans[-1][2] == mo and spans[-1][3] == mn:
                    spans[-1][1] = (blk + 1) * B
                else:
                    spans.append([blk * B, (blk + 1) * B, mo, mn])
            removed += self.thin_day(day, spans, E.prev[day], E.nxt[day], tol)
            for blk in blks:
                self.mult[blk] = min(self.mult.get(blk, 1.0) * cfg["step"], cap)
        self.save()
        return removed

    def protected(self, bs):
        cfg = self.cfg
        step = bs["step"]
        rare = ~step & (bs["cell_n"] < cfg["rare_blocks"])
        free = np.flatnonzero(~step & ~rare)
        top = free[np.argsort(-bs["score"][free], kind="stable")][: max(int(cfg["protect_frac"] * (~step).sum()) - int(rare.sum()), 0)]
        out = rare.copy()
        out[top] = True
        return out

    def enforce(self):
        st, cfg = self.st, self.cfg
        if st.watermark is None or st.size() <= self.budget:
            return {"thinned_blocks": 0}
        target = self.budget * st.cfg["safety"]
        bs = BlockScore(st).compute()
        B = st.cfg["block_s"]
        hold = (st.watermark + 1) // B - cfg["hold_blocks"]
        stage, total, gain = 0, 0, cfg["gain"]
        ids = np.arange(len(bs["score"])) + bs["b0"]
        stages = [("protected", cfg["max_mult"]), ("unprotected", cfg["max_mult"]), ("ladder", cfg["ladder_max"])]
        stages = stages if cfg["escalate"] else stages[:1]
        for _ in range(cfg["tries"]):
            size = st.size()
            if size <= target:
                break
            dens = self.density()
            bpp = sum(os.path.getsize(p) for p in st.files("points").values()) / max(sum(dens.values()), 1)
            name, cap = stages[stage]
            prot = bs["step"] | (self.protected(bs) if stage == 0 else False)
            m = np.array([self.mult.get(int(b), 1.0) for b in ids])
            cand = np.flatnonzero(~prot & (m < cap) & (ids < hold))
            cand = cand[np.argsort(ids[cand])] if name == "ladder" else cand[np.lexsort((ids[cand], -bs["cell_n"][cand], bs["score"][cand]))]
            need, picks, raw = (size - target) * cfg["need_frac"], [], 0.0
            for i in cand:
                picks.append(int(ids[i]))
                raw += dens.get(int(ids[i]), 0) * bpp
                if raw * gain >= need:
                    break
            if not picks:
                if stage + 1 < len(stages):
                    stage += 1
                    continue
                raise StoreError(f"store is {size} bytes, budget {self.budget}: no block left to thin without touching step rows")
            total += len(picks)
            self.apply(picks, cap)
            if raw > 0:
                gain = min(1.0, max(cfg["gain_min"], (size - st.size()) / raw))
        size = st.size()
        if size > self.budget:
            raise StoreError(f"store is {size} bytes after thinning, budget {self.budget}")
        return {"thinned_blocks": total, "escalated": stage > 0, "ladder": stage > 1, "size_after": size}

    def tolerance_at(self, channel, t):
        return self.st.tolerances()[channel] * self.mult.get(int(t) // self.st.cfg["block_s"], 1.0)


if __name__ == "__main__":
    main(sys.argv[1:])
