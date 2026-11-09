import ctypes
import glob
import json
import math
import multiprocessing
import os
import queue
import signal
import subprocess
import sys
import threading
import time
from collections import deque

HERE = os.path.dirname(os.path.abspath(__file__))
LOCAL = os.environ.get("LOCALAPPDATA", ".")
CFG = {
    "hot_dir": os.path.join(LOCAL, "daisyloop", "hot"),
    "log_dir": os.path.join(LOCAL, "daisyloop", "steprun"),
    "abort_exe": [os.path.join(HERE, "..", "tools", "d5_probe", "target", "release", "d5_abort.exe"), r"C:\clones\daisyloop\tools\d5_probe\target\release\d5_abort.exe"],
    "abort_timeout_s": 150,
    "pipe": {"name": r"\\.\pipe\daisyloop", "version": 5, "timeout_s": 8.0, "busy_retries": 5},
    "channels": {
        "water": ["d5next/water/temp"],
        "fan_rpm": ["d5next/fan/rpm"],
        "fan_duty": ["d5next/fan/duty"],
        "pump_rpm": ["d5next/pump/rpm"],
        "pump_duty": ["d5next/pump/duty"],
        "cpu_power": ["/intelcpu/0/power/0"],
        "gpu_power": ["nvml/0/power", "/gpu-nvidia/0/power/0"],
        "gpu_limit": ["nvml/0/power_limit"],
        "cpu_temp": ["/intelcpu/0/temperature/26"],
        "gpu_temp": ["nvml/0/temp"],
        "phase": ["daisyloop/step/phase"],
        "step_duty": ["daisyloop/step/duty"],
    },
    "history_s": 300,
    "limits": {"water_c": 44.0, "rise_k_per_min": 1.5, "rise_hold_s": 120, "rise_window_s": 60, "gpu_c": 80.0, "pump_rpm_min": 1500.0, "stale_s": 10.0, "stale_lhm_s": 15.0, "cpu_hold_c": 95.0, "cpu_hold_s": 10, "cpu_c": 100.0},
    "safety_channels": ["water", "cpu_temp", "gpu_temp", "pump_rpm"],
    "lhm_keys": ["cpu_temp"],
    "release_wait_s": 15,
    "status_every_s": 30,
    "load": {
        "cpu_threads": 8, "cpu_max_w": 100.0, "cpu_range_w": 120.0, "cpu_ki": 0.004, "cpu_soft_c": 90.0,
        "gpu_max_w": 320.0, "gpu_range_w": 300.0, "gpu_ki": 0.002, "gpu_soft_c": 75.0, "gpu_size": 2048,
        "cap_down_w": 10.0, "cap_up_w": 2.0, "cap_hyst_c": 5.0, "cpu_min_w": 0.0, "gpu_off_s": 20,
        "period_s": 0.1, "job": True,
    },
    "foreign_max_w": 90.0,
    "start_wait_s": 30,
    "gone_s": 6,
    "overrun_s": 120,
    "start_retries": 4,
    "start_retry_s": 10,
    "stages": {
        "duty": {"band": [30.0, 31.0], "parts": "duty"},
        "fan": {"band": [27.0, 31.0], "parts": "fan"},
        "pump": {"band": [27.0, 33.0], "parts": "pump"},
    },
    "rehearse": {"watts": 250.0, "secs": 120, "every_s": 5},
    "early_tol_s": 20,
    "heat": {"target_c": 30.0, "watts": 100.0, "max_s": 3600},
    "cool": {"to_c": 31.0, "max_s": 3600},
    "controls": {"fan": "d5.fan", "pump": "d5.pump"},
}


def stale_keys(cfg, tele):
    lim = cfg["limits"]
    return [k for k in cfg["safety_channels"] if tele.age(k) > (lim["stale_lhm_s"] if k in cfg["lhm_keys"] else lim["stale_s"])]


class Clock:
    def now(self):
        return time.time()

    def sleep(self, secs):
        time.sleep(secs)


class Log:
    def __init__(self, directory, clock):
        self.clock = clock
        self.lock = threading.Lock()
        self.path = None
        self.q = queue.SimpleQueue()
        if directory:
            os.makedirs(directory, exist_ok=True)
            self.path = os.path.join(directory, time.strftime("steprun-%Y%m%d-%H%M%S.log"))
        threading.Thread(target=self.console, daemon=True, name="console").start()

    def console(self):
        while True:
            line = self.q.get()
            try:
                print(line, flush=True)
            except Exception:
                pass

    def __call__(self, text):
        try:
            line = f"{time.strftime('%H:%M:%S', time.localtime(self.clock.now()))} {text}"
            self.q.put(line)
            if self.path:
                with self.lock:
                    with open(self.path, "a", encoding="utf-8") as f:
                        f.write(line + "\n")
        except Exception:
            pass

    def flush(self, secs=2.0):
        end = time.time() + secs
        while not self.q.empty() and time.time() < end:
            time.sleep(0.05)


class Telemetry:
    def __init__(self, cfg, clock):
        self.cfg, self.clock = cfg, clock
        self.lock = threading.Lock()
        self.latest = {}
        self.hist = {}
        self.names = {n for alts in cfg["channels"].values() for n in alts}

    def push(self, ts, channel, value):
        with self.lock:
            cur = self.latest.get(channel)
            if cur is not None and ts < cur[0]:
                return
            self.latest[channel] = (ts, value)
            h = self.hist.setdefault(channel, deque())
            h.append((ts, value))
            while h and ts - h[0][0] > self.cfg["history_s"]:
                h.popleft()

    def pick(self, key):
        with self.lock:
            found = [(self.latest[n], n) for n in self.cfg["channels"][key] if n in self.latest]
        return max(found, key=lambda x: x[0][0])[1] if found else None

    def age(self, key):
        n = self.pick(key)
        return math.inf if n is None else max(0.0, self.clock.now() - self.latest[n][0])

    def value(self, key, max_age=None):
        n = self.pick(key)
        if n is None or (max_age is not None and self.age(key) > max_age):
            return None
        v = self.latest[n][1]
        return v if math.isfinite(v) else None

    def series(self, key, secs):
        n = self.pick(key)
        if n is None:
            return []
        t0 = self.clock.now() - secs
        with self.lock:
            return [p for p in self.hist.get(n, ()) if p[0] >= t0]


class Tail(Telemetry):
    def __init__(self, cfg, clock):
        super().__init__(cfg, clock)
        self.path, self.data, self.done = None, b"", 0
        self.stop = threading.Event()
        self.error = None

    def poll(self):
        files = sorted(glob.glob(os.path.join(self.cfg["hot_dir"], "telemetry-*.arrows")))
        if not files:
            return
        if files[-1] != self.path:
            self.path, self.data, self.done = files[-1], b"", 0
        with open(self.path, "rb") as f:
            f.seek(len(self.data))
            new = f.read()
        if new:
            self.data += new
            self.parse()

    def parse(self):
        import pyarrow as pa
        import pyarrow.ipc as ipc
        try:
            reader = ipc.open_stream(pa.BufferReader(self.data))
            for i, b in enumerate(reader):
                if i >= self.done:
                    self.take(b)
                    self.done = i + 1
        except (pa.ArrowInvalid, OSError):
            pass

    def take(self, b):
        import numpy as np
        import pyarrow as pa
        ts = b.column("ts").cast(pa.int64()).to_numpy() / 1e6
        if ts.size == 0 or ts.max() < self.clock.now() - self.cfg["history_s"]:
            return
        col = b.column("channel")
        idx = col.indices.to_numpy(zero_copy_only=False)
        val = b.column("value").to_numpy(zero_copy_only=False)
        for k, name in enumerate(col.dictionary.to_pylist()):
            if name in self.names:
                for r in np.nonzero(idx == k)[0]:
                    self.push(float(ts[r]), name, float(val[r]))

    def loop(self):
        while not self.stop.is_set():
            try:
                self.poll()
                self.error = None
            except Exception as e:
                self.error = repr(e)
            self.stop.wait(0.5)

    def start(self):
        threading.Thread(target=self.loop, daemon=True, name="tail").start()
        return self


class PipeError(Exception):
    pass


class Refused(Exception):
    pass


class Status:
    def __init__(self, d):
        self.d = d

    def control(self, cid):
        return next((c for c in self.d.get("controls", []) if c["id"] == cid), None)

    def drive(self, cid):
        c = self.control(cid)
        return c["drive"] if c else None

    @property
    def step(self):
        return self.d.get("step_test")


class Pipe:
    def __init__(self, cfg):
        self.cfg = cfg["pipe"]
        self.n = 0
        self.lock = threading.Lock()

    def exchange(self, req):
        last = None
        for _ in range(self.cfg["busy_retries"]):
            try:
                f = open(self.cfg["name"], "r+b", buffering=0)
                break
            except OSError as e:
                last = e
                if getattr(e, "winerror", None) != 231 and e.errno != 22:
                    raise
                time.sleep(0.2)
        else:
            raise last
        with f:
            f.write(json.dumps(req).encode() + b"\n")
            buf = b""
            while b"\n" not in buf:
                chunk = f.read(65536)
                if not chunk:
                    raise OSError("pipe closed")
                buf += chunk
        return json.loads(buf.split(b"\n", 1)[0])

    def call(self, body):
        with self.lock:
            self.n += 1
            rid = self.n
        req = {"v": self.cfg["version"], "id": rid, "body": body}
        out = {}

        def run():
            try:
                out["r"] = self.exchange(req)
            except Exception as e:
                out["e"] = e

        t = threading.Thread(target=run, daemon=True)
        t.start()
        t.join(self.cfg["timeout_s"])
        if t.is_alive():
            raise PipeError("pipe timeout")
        if "e" in out:
            e = out["e"]
            raise PipeError(f"{type(e).__name__}: {e}")
        r = out["r"]
        if r.get("v") != self.cfg["version"] or r.get("id") != rid:
            raise PipeError(f"protocol mismatch: {r.get('v')}")
        b = r["body"]
        if b["type"] == "error":
            raise Refused(b["message"])
        return b

    def snapshot(self):
        return Status(self.call({"type": "snapshot"}))

    def command(self, cmd):
        self.call({"type": "command", **cmd})


class Runbook:
    def __init__(self, table=None):
        from stepsim import SIM
        self.table = table or SIM["inject"]["protocols"]["runbook"]
        self.idle_w = SIM["inject"]["idle_w"]
        table = self.table
        self.entries = {k: [(i, e[1]) for i, e in enumerate(table) if e[0] == k] for k in ("fan", "pump")}

    def part(self, kind, which):
        i, steps = self.entries[kind][which]
        nxt = self.table[i + 1] if i + 1 < len(self.table) and self.table[i + 1][0] == "rest" else None
        return {"kind": kind, "steps": steps, "rest": int(nxt[1]) if nxt else 0}

    def parts(self, name):
        return {"duty": [self.part("fan", 0), self.part("pump", 0)], "fan": [self.part("fan", -1)], "pump": [self.part("pump", -1)]}[name]


class Split:
    def __init__(self, cfg):
        self.cfg = cfg
        self.cpu_cap = cfg["cpu_max_w"]
        self.gpu_cap = cfg["gpu_max_w"]

    @staticmethod
    def cap(cap, top, temp, soft, c):
        if temp is None or temp >= soft:
            return max(c["cpu_min_w"], cap - c["cap_down_w"])
        if temp < soft - c["cap_hyst_c"]:
            return min(top, cap + c["cap_up_w"])
        return cap

    def step(self, total, cpu_t, gpu_t, gpu_w, gpu_limit=None):
        c = self.cfg
        self.cpu_cap = self.cap(self.cpu_cap, c["cpu_max_w"], cpu_t, c["cpu_soft_c"], c)
        top = min(c["gpu_max_w"], gpu_limit) if gpu_limit else c["gpu_max_w"]
        self.gpu_cap = self.cap(self.gpu_cap, top, gpu_t, c["gpu_soft_c"], c)
        self.gpu_cap = min(self.gpu_cap, top)
        gpu_goal = min(self.gpu_cap, max(0.0, total - self.cpu_cap))
        cpu_goal = min(self.cpu_cap, max(0.0, total - (gpu_goal if gpu_goal > 0 else (gpu_w or 0.0))))
        return cpu_goal, gpu_goal


class Pwm:
    def __init__(self, ki, rng):
        self.ki, self.rng, self.duty = ki, rng, 0.0

    def update(self, goal, measured):
        if goal <= 0 or measured is None:
            self.duty = 0.0
        else:
            if self.duty == 0.0:
                self.duty = min(1.0, 0.5 * goal / self.rng)
            self.duty = min(1.0, max(0.0, self.duty + self.ki * (goal - measured)))
        return self.duty


class Win:
    k32 = ctypes.WinDLL("kernel32", use_last_error=True) if os.name == "nt" else None

    @classmethod
    def setup(cls):
        k = cls.k32
        from ctypes import wintypes as w
        k.OpenProcess.restype = w.HANDLE
        k.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
        k.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
        k.CreateJobObjectW.restype = w.HANDLE
        k.CreateJobObjectW.argtypes = [ctypes.c_void_p, w.LPCWSTR]
        k.SetInformationJobObject.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD]
        k.AssignProcessToJobObject.argtypes = [w.HANDLE, w.HANDLE]
        k.CloseHandle.argtypes = [w.HANDLE]

    @classmethod
    def parent_handle(cls, pid):
        cls.setup()
        return cls.k32.OpenProcess(0x00100000, False, pid)

    @classmethod
    def alive(cls, handle):
        return handle is not None and cls.k32.WaitForSingleObject(handle, 0) == 0x102


class Job:
    def __init__(self):
        from ctypes import wintypes as w
        Win.setup()

        class Basic(ctypes.Structure):
            _fields_ = [("a", ctypes.c_int64), ("b", ctypes.c_int64), ("flags", w.DWORD), ("minws", ctypes.c_size_t), ("maxws", ctypes.c_size_t), ("active", w.DWORD), ("aff", ctypes.c_size_t), ("prio", w.DWORD), ("sched", w.DWORD)]

        class Extended(ctypes.Structure):
            _fields_ = [("basic", Basic), ("io", ctypes.c_uint64 * 6), ("pm", ctypes.c_size_t), ("jm", ctypes.c_size_t), ("ppeak", ctypes.c_size_t), ("jpeak", ctypes.c_size_t)]

        k = Win.k32
        self.handle = k.CreateJobObjectW(None, None)
        info = Extended()
        info.basic.flags = 0x2000
        if not k.SetInformationJobObject(self.handle, 9, ctypes.byref(info), ctypes.sizeof(info)):
            raise OSError("job object limit not set")

    def add(self, pid):
        k = Win.k32
        h = k.OpenProcess(0x0101, False, pid)
        if not h or not k.AssignProcessToJobObject(self.handle, h):
            raise OSError(f"pid {pid} not added to the job")
        k.CloseHandle(h)


def cpu_worker(duty, stop, parent, period):
    for k in ("OMP_NUM_THREADS", "MKL_NUM_THREADS", "OPENBLAS_NUM_THREADS"):
        os.environ[k] = "1"
    import numpy as np
    a = np.random.rand(160, 160).astype(np.float32)
    b = np.random.rand(160, 160).astype(np.float32)
    ph = Win.parent_handle(parent)
    while not stop.value and Win.alive(ph):
        t0 = time.perf_counter()
        d = duty.value
        while d > 0.001 and time.perf_counter() - t0 < d * period:
            a @ b
        rest = period - (time.perf_counter() - t0)
        if rest > 0:
            time.sleep(rest)


def gpu_worker(duty, stop, parent, period, size, ready):
    import torch
    ph = Win.parent_handle(parent)
    a = b = None
    try:
        a = torch.randn(size, size, device="cuda", dtype=torch.float16)
        b = torch.randn(size, size, device="cuda", dtype=torch.float16)
        a @ b
        torch.cuda.synchronize()
        ready.value = 1
        while not stop.value and Win.alive(ph):
            t0 = time.perf_counter()
            d = duty.value
            while d > 0.001 and time.perf_counter() - t0 < d * period:
                a @ b
                torch.cuda.synchronize()
            rest = period - (time.perf_counter() - t0)
            if rest > 0:
                time.sleep(rest)
    finally:
        del a, b
        torch.cuda.synchronize()
        torch.cuda.empty_cache()


class Load:
    def __init__(self, cfg, tele=None):
        self.cfg = cfg["load"]
        self.tele = tele
        ctx = multiprocessing.get_context("spawn")
        self.ctx = ctx
        self.duty = {"cpu": ctx.RawValue("d", 0.0), "gpu": ctx.RawValue("d", 0.0)}
        self.stop = {"cpu": ctx.RawValue("i", 0), "gpu": ctx.RawValue("i", 0)}
        self.ready = ctx.RawValue("i", 0)
        self.halted = False
        self.procs = {"cpu": [], "gpu": []}
        self.pwm = {"cpu": Pwm(self.cfg["cpu_ki"], self.cfg["cpu_range_w"]), "gpu": Pwm(self.cfg["gpu_ki"], self.cfg["gpu_range_w"])}
        self.split = Split(self.cfg)
        self.threads = self.cfg["cpu_threads"]
        self.total = None
        self.gpu_idle = 0
        self.job = Job() if self.cfg["job"] and os.name == "nt" else None
        self.lock = threading.Lock()

    def set_threads(self, n):
        self.threads = int(n)

    def set_total(self, watts):
        if self.halted and watts is not None:
            return
        self.total = watts
        if watts is None:
            self.zero()

    def zero(self):
        for k in self.duty:
            self.duty[k].value = 0.0
            self.pwm[k].duty = 0.0

    def spawn(self, kind):
        self.stop[kind].value = 0
        self.ready.value = 0
        if kind == "cpu":
            args = (self.duty["cpu"], self.stop["cpu"], os.getpid(), self.cfg["period_s"])
            procs = [self.ctx.Process(target=cpu_worker, args=args, daemon=True) for _ in range(self.threads)]
        else:
            args = (self.duty["gpu"], self.stop["gpu"], os.getpid(), self.cfg["period_s"], self.cfg["gpu_size"], self.ready)
            procs = [self.ctx.Process(target=gpu_worker, args=args, daemon=True)]
        for p in procs:
            p.start()
            if self.job:
                self.job.add(p.pid)
        self.procs[kind] = procs

    def release(self, kind):
        self.duty[kind].value = 0.0
        self.stop[kind].value = 1
        for p in self.procs[kind]:
            p.join(2.0)
            if p.is_alive():
                p.terminate()
                p.join(1.0)
        self.procs[kind] = []

    def pids(self):
        return [p.pid for ps in self.procs.values() for p in ps]

    def own_watts(self):
        return self.duty["cpu"].value * self.cfg["cpu_range_w"] + self.duty["gpu"].value * self.cfg["gpu_range_w"]

    def report(self):
        return f"duty cpu {self.duty['cpu'].value:.2f} gpu {self.duty['gpu'].value:.2f} gpu_ready {self.ready.value}"

    def regulate(self):
        with self.lock:
            if self.halted:
                return
            if self.total is None:
                self.zero()
                self.gpu_idle += 1
            else:
                t = self.tele
                cpu_w, gpu_w = t.value("cpu_power", 5), t.value("gpu_power", 5)
                cpu_goal, gpu_goal = self.split.step(self.total, t.value("cpu_temp", 5), t.value("gpu_temp", 5), gpu_w, t.value("gpu_limit", 30))
                if cpu_goal > 0 and not self.procs["cpu"]:
                    self.spawn("cpu")
                if gpu_goal > 0 and not self.procs["gpu"]:
                    self.spawn("gpu")
                self.gpu_idle = 0 if gpu_goal > 0 else self.gpu_idle + 1
                self.duty["cpu"].value = self.pwm["cpu"].update(cpu_goal, cpu_w)
                self.duty["gpu"].value = self.pwm["gpu"].update(gpu_goal, gpu_w) if self.ready.value else 0.0
            if self.procs["gpu"] and self.gpu_idle >= self.cfg["gpu_off_s"]:
                self.release("gpu")

    def halt(self):
        self.halted = True
        self.total = None
        self.zero()
        got = self.lock.acquire(timeout=5.0)
        try:
            self.release("cpu")
            self.release("gpu")
        finally:
            if got:
                self.lock.release()


class Abort:
    def __init__(self, cfg, load, pipe, runner, log, clock):
        self.cfg, self.load, self.pipe, self.runner, self.log, self.clock = cfg, load, pipe, runner, log, clock
        self.lock = threading.Lock()
        self.fired = None
        self.done = threading.Event()
        self.tested = []

    def released(self, status):
        ids = self.tested or list(self.cfg["controls"].values())
        return status.step is None and all(status.drive(c) == "firmware" for c in ids)

    def verify(self):
        end = self.clock.now() + self.cfg["release_wait_s"]
        while True:
            try:
                if self.released(self.pipe.snapshot()):
                    return True
            except (PipeError, Refused) as e:
                self.log(f"abort: status failed: {e}")
                return False
            if self.clock.now() >= end:
                return False
            self.clock.sleep(1.0)

    def say(self, text):
        try:
            self.log(text)
        except Exception:
            pass

    def fire(self, reason):
        with self.lock:
            if self.fired:
                return
            self.fired = reason
        try:
            self.load.halt()
        except Exception as e:
            self.say(f"abort: load halt failed: {e!r}")
        ok = False
        try:
            try:
                self.pipe.command({"cmd": "stop_step_test"})
                note = "StopStepTest acknowledged"
            except Refused as e:
                note = f"StopStepTest refused: {e}"
            self.say(f"ABORT {reason}: load stopped, {note}")
            ok = self.verify()
        except PipeError as e:
            self.say(f"ABORT {reason}: load stopped, pipe failed: {e}")
        except Exception as e:
            self.say(f"ABORT {reason}: load stopped, error {e!r}")
        try:
            if ok:
                self.say("abort: tested channels are on firmware")
            else:
                self.say("abort: firmware not confirmed, running d5_abort")
                self.say(f"abort: d5_abort verdict: {self.runner()}")
        except Exception as e:
            self.say(f"abort: d5_abort failed to run: {e!r}")
        finally:
            self.done.set()


class AbortRunner:
    def __init__(self, cfg):
        self.cfg = cfg

    def __call__(self):
        exe = next((p for p in self.cfg["abort_exe"] if os.path.exists(p)), None)
        if exe is None:
            return "FAILED d5_abort.exe not found"
        flags = getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)
        try:
            r = subprocess.run([exe], capture_output=True, text=True, timeout=self.cfg["abort_timeout_s"], creationflags=flags)
        except Exception as e:
            return f"FAILED {type(e).__name__}: {e}"
        lines = [l for l in (r.stdout + r.stderr).splitlines() if l.strip()]
        hit = [l for l in lines if l.startswith(("OK", "FAILED"))]
        return f"exit {r.returncode}: " + (hit[-1] if hit else (lines[-1] if lines else "no output"))


class Watchdog:
    def __init__(self, cfg, tele, abort, clock):
        self.cfg, self.tele, self.abort, self.clock = cfg, tele, abort, clock
        self.rise_since = None
        self.cpu_since = None
        self.stopped = threading.Event()

    def slope(self):
        lim = self.cfg["limits"]
        pts = self.tele.series("water", lim["rise_window_s"])
        if len(pts) < 10 or pts[-1][0] - pts[0][0] < lim["rise_window_s"] / 2:
            return None
        t0 = pts[0][0]
        xs, ys = [p[0] - t0 for p in pts], [p[1] for p in pts]
        mx, my = sum(xs) / len(xs), sum(ys) / len(ys)
        den = sum((x - mx) ** 2 for x in xs)
        return 60.0 * sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / den if den else None

    def check(self):
        lim, t = self.cfg["limits"], self.tele
        hits = []
        stale = stale_keys(self.cfg, t)
        if stale:
            hits.append(f"telemetry stale (D5 over {lim['stale_s']:.0f} s, LHM over {lim['stale_lhm_s']:.0f} s): {','.join(stale)}")
        water, cpu, gpu, pump = (t.value(k) for k in ("water", "cpu_temp", "gpu_temp", "pump_rpm"))
        if water is not None and water >= lim["water_c"]:
            hits.append(f"water {water:.2f} C >= {lim['water_c']}")
        now = self.clock.now()
        if cpu is not None and cpu >= lim["cpu_hold_c"]:
            self.cpu_since = self.cpu_since or now
        else:
            self.cpu_since = None
        if cpu is not None and (cpu >= lim["cpu_c"] or (self.cpu_since and now - self.cpu_since >= lim["cpu_hold_s"])):
            hits.append(f"CPU package {cpu:.1f} C (>= {lim['cpu_c']} at once or >= {lim['cpu_hold_c']} for {lim['cpu_hold_s']} s)")
        if gpu is not None and gpu >= lim["gpu_c"]:
            hits.append(f"GPU {gpu:.1f} C >= {lim['gpu_c']}")
        if pump is not None and pump < lim["pump_rpm_min"]:
            hits.append(f"pump {pump:.0f} rpm < {lim['pump_rpm_min']:.0f}")
        s = self.slope()
        if s is not None and s > lim["rise_k_per_min"]:
            self.rise_since = self.rise_since or now
            if now - self.rise_since > lim["rise_hold_s"]:
                hits.append(f"water rising {s:.2f} K/min for {now - self.rise_since:.0f} s")
        else:
            self.rise_since = None
        return hits

    def step(self):
        hits = self.check()
        if hits:
            self.abort.fire("; ".join(hits))
        return hits

    def loop(self):
        while not self.stopped.is_set():
            try:
                self.step()
            except Exception as e:
                self.abort.fire(f"watchdog error {type(e).__name__}: {e}")
            self.stopped.wait(1.0)

    def start(self):
        threading.Thread(target=self.loop, daemon=True, name="watchdog").start()


class Aborted(Exception):
    pass


class Session:
    def __init__(self, cfg, tele, pipe, load, runner, log, clock, runbook=None):
        self.cfg, self.tele, self.pipe, self.load, self.log, self.clock = cfg, tele, pipe, load, log, clock
        self.rb = runbook or Runbook()
        self.abort = Abort(cfg, load, pipe, runner, log, clock)
        self.dog = Watchdog(cfg, tele, self.abort, clock)
        self.last_status = 0.0
        self.phase = None

    def fmt(self, v, spec):
        return "-" if v is None else format(v, spec)

    def status_line(self):
        t, f = self.tele, self.fmt
        s = self.dog.slope()
        return (f"status water {f(t.value('water'), '.2f')} C ({f(s, '+.2f')} K/min) cpu {f(t.value('cpu_temp'), '.0f')} C {f(t.value('cpu_power'), '.0f')} W "
                f"gpu {f(t.value('gpu_temp'), '.0f')} C {f(t.value('gpu_power'), '.0f')} W fan {f(t.value('fan_rpm'), '.0f')} rpm/{f(t.value('fan_duty'), '.0f')} % "
                f"pump {f(t.value('pump_rpm'), '.0f')} rpm/{f(t.value('pump_duty'), '.0f')} % phase {f(t.value('phase', 5), '.0f')} load {self.load.total} W own {self.load.own_watts():.0f} W")

    def tick(self):
        self.clock.sleep(1.0)
        if self.abort.fired:
            raise Aborted(self.abort.fired)
        now = self.clock.now()
        if now - self.last_status >= self.cfg["status_every_s"]:
            self.last_status = now
            self.log(self.status_line())
        self.load.regulate()

    def fresh(self):
        return stale_keys(self.cfg, self.tele)

    def refusals(self, stage, go, value=None, inner=False):
        out = []
        if stage in ("duty", "fan", "pump", "heat", "rehearse") and not go and not inner:
            out.append("the word go is missing")
        stale = self.fresh()
        if stale:
            out.append(f"telemetry not fresh: {','.join(stale)}")
        water = self.tele.value("water")
        if stage in self.cfg["stages"] and not inner:
            lo, hi = self.cfg["stages"][stage]["band"]
            if water is None or not lo <= water <= hi:
                out.append(f"water {self.fmt(water, '.2f')} C outside the band {lo} to {hi} C")
        if stage == "heat" and (water is None or water >= (value or self.cfg["heat"]["target_c"])):
            out.append(f"water {self.fmt(water, '.2f')} C already at the target")
        if stage in ("duty", "fan", "pump"):
            try:
                st = self.pipe.snapshot()
            except (PipeError, Refused) as e:
                return out + [f"pipe: {e}"]
            if st.step is not None:
                out.append("a step test is already running")
            for cid in self.cfg["controls"].values():
                c = st.control(cid)
                if c is None or c["drive"] != "firmware":
                    out.append(f"{cid} is not on firmware ({None if c is None else c['drive']})")
            for part in self.rb.parts(self.cfg["stages"][stage]["parts"]):
                c = st.control(self.cfg["controls"][part["kind"]])
                if c is not None and not c["allowed"]:
                    out.append(f"writes not allowed for {c['id']}")
                if c is not None and c["mode"] == "model":
                    out.append(f"{c['id']} is in model mode")
        if stage in ("duty", "fan", "pump", "heat", "rehearse"):
            cpu, gpu = self.tele.value("cpu_power", 10), self.tele.value("gpu_power", 10)
            foreign = None if cpu is None or gpu is None else cpu + gpu - self.load.own_watts()
            if foreign is None:
                out.append("CPU or GPU power unknown")
            elif foreign > self.cfg["foreign_max_w"]:
                out.append(f"foreign load {foreign:.0f} W above {self.cfg['foreign_max_w']:.0f} W")
        return out

    def start_test(self, cid):
        for i in range(self.cfg["start_retries"]):
            try:
                self.pipe.command({"cmd": "start_step_test", "id": cid, "confirmed": True})
                self.log(f"StartStepTest {cid} acknowledged")
                return
            except Refused as e:
                if "Schreibpause" not in str(e) or i == self.cfg["start_retries"] - 1:
                    raise
                self.log(f"start waits: {e}")
                for _ in range(self.cfg["start_retry_s"]):
                    self.tick()

    def watts_of(self, steps, phase):
        w = steps[phase - 1][2] if 1 <= phase <= len(steps) else 0.0
        return None if w <= self.rb.idle_w else float(w)

    def follow(self, part):
        steps = part["steps"]
        cid = self.cfg["controls"][part["kind"]]
        total = sum(s[1] for s in steps)
        t0 = self.clock.now()
        self.phase, seen, gone, t_seen = None, False, 0, None
        while True:
            self.tick()
            p = self.tele.value("phase", 5)
            if p is not None:
                if not seen:
                    t_seen = self.clock.now()
                seen, gone = True, 0
                if int(p) != self.phase:
                    self.phase = int(p)
                    w = self.watts_of(steps, self.phase)
                    self.load.set_total(w)
                    d = steps[self.phase - 1][0] if self.phase <= len(steps) else None
                    n = None
                    try:
                        n = self.pipe.snapshot().step
                    except (PipeError, Refused) as e:
                        self.log(f"status unavailable: {e}")
                    ov = None if n is None else n.get("overridden")
                    self.log(f"phase {self.phase} of {len(steps)} {part['kind']} duty {d} % load {'off' if w is None else f'{w:.0f} W'} overridden {ov}")
                    if self.phase == 1 and n is not None:
                        if n is None or n.get("phases") != len(steps) or n.get("id") != cid:
                            self.abort.fire(f"daemon step plan {n} differs from the table ({len(steps)} phases)")
                            raise Aborted(self.abort.fired)
            elif seen:
                gone += 1
                if gone >= self.cfg["gone_s"]:
                    break
            elif self.clock.now() - t0 > self.cfg["start_wait_s"]:
                self.abort.fire("step test did not start within the wait time")
                raise Aborted(self.abort.fired)
            if self.clock.now() - t0 > total + self.cfg["overrun_s"]:
                self.abort.fire("step test ran longer than its plan")
                raise Aborted(self.abort.fired)
        self.load.set_total(None)
        ran = self.clock.now() - t_seen - gone
        if ran < total - self.cfg["early_tol_s"]:
            self.log(f"{part['kind']} test ended early: {ran:.0f} s of {total} s")
            self.abort.fire(f"{part['kind']} test ended early after {ran:.0f} s of {total} s")
            raise Aborted(self.abort.fired)
        self.log(f"{part['kind']} test finished after {ran:.0f} s, load off")

    def wait(self, secs, text):
        self.log(f"{text} {secs:.0f} s, load off")
        end = self.clock.now() + secs
        while self.clock.now() < end:
            self.tick()

    def confirm_firmware(self, ids):
        st = self.pipe.snapshot()
        bad = [c for c in ids if st.drive(c) != "firmware"]
        if bad:
            self.abort.fire(f"not back on firmware: {','.join(bad)}")
            raise Aborted(self.abort.fired)
        self.log(f"{','.join(ids)} on firmware")

    def run_parts(self, stage):
        parts = self.rb.parts(self.cfg["stages"][stage]["parts"])
        for i, part in enumerate(parts):
            cid = self.cfg["controls"][part["kind"]]
            if i:
                bad = self.refusals(stage, True, inner=True)
                if bad:
                    self.log("refused to continue: " + "; ".join(bad))
                    return 2
            self.abort.tested = [cid]
            self.log(f"stage {stage}: starting {cid}, {len(part['steps'])} phases")
            self.start_test(cid)
            self.follow(part)
            self.confirm_firmware([cid])
            if part["rest"] and (i < len(parts) - 1 or stage == "fan"):
                self.wait(part["rest"], "rest")
        self.abort.tested = []
        return 0

    def run_rehearse(self):
        r = self.cfg["rehearse"]
        self.log(f"rehearse: {r['watts']:.0f} W for {r['secs']} s, no step test, no D5 command")
        self.load.set_total(r["watts"])
        for i in range(r["secs"]):
            self.tick()
            if i % r["every_s"] == 0:
                t = self.tele
                self.log(f"rehearse {i}s cpu {self.fmt(t.value('cpu_power'), '.0f')} W {self.fmt(t.value('cpu_temp'), '.0f')} C gpu {self.fmt(t.value('gpu_power'), '.0f')} W {self.fmt(t.value('gpu_temp'), '.0f')} C {self.load.report()}")
        self.load.halt()
        self.log("rehearse done, load off")

    def run_heat(self, value):
        target = value or self.cfg["heat"]["target_c"]
        self.load.set_total(self.cfg["heat"]["watts"])
        t0 = self.clock.now()
        self.log(f"heat to {target} C with {self.cfg['heat']['watts']} W")
        while self.tele.value("water") is not None and self.tele.value("water") < target:
            self.tick()
            if self.clock.now() - t0 > self.cfg["heat"]["max_s"]:
                self.abort.fire("heat took longer than its limit")
                raise Aborted(self.abort.fired)
        self.load.halt()
        self.log(f"heat done: water {self.fmt(self.tele.value('water'), '.2f')} C, load off; start step 3 within 2 min")

    def run_cool(self):
        t0 = self.clock.now()
        to = self.cfg["cool"]["to_c"]
        self.log(f"cool: watching until water is below {to} C, load off")
        while self.tele.value("water") is None or self.tele.value("water") >= to:
            self.tick()
            if self.clock.now() - t0 > self.cfg["cool"]["max_s"]:
                self.log("cool: limit reached, water still not below target")
                return
        self.log(f"cool done: water {self.tele.value('water'):.2f} C")
        try:
            st = self.pipe.snapshot()
            self.log("channels: " + ", ".join(f"{c}={st.drive(c)}" for c in self.cfg["controls"].values()))
        except (PipeError, Refused) as e:
            self.log(f"pipe: {e}")

    def show(self):
        t = self.tele
        for k in self.cfg["channels"]:
            self.log(f"{k} {self.fmt(t.value(k), '.2f')} age {t.age(k):.1f} s")
        try:
            st = self.pipe.snapshot()
            for c in st.d["controls"]:
                self.log(f"pipe {c['id']} mode {c['mode']} drive {c['drive']} allowed {c['allowed']} rpm {c['actual_rpm']} duty {c['duty']}")
            self.log(f"pipe step {st.step} alarms {[a['kind'] for a in st.d['alarms']]}")
        except (PipeError, Refused) as e:
            self.log(f"pipe failed: {e}" + (" (access denied: the pipe is open to interactive logon sessions only)" if "Permission" in str(e) or "denied" in str(e) else ""))

    def run(self, stage, go=False, value=None):
        if stage == "status":
            self.show()
            return 0
        bad = self.refusals(stage, go, value)
        if bad:
            self.log(f"refused {stage}: " + "; ".join(bad))
            return 2
        self.dog.start()
        try:
            if stage in self.cfg["stages"]:
                return self.run_parts(stage)
            elif stage == "rehearse":
                self.run_rehearse()
            elif stage == "heat":
                self.run_heat(value)
            elif stage == "cool":
                self.run_cool()
        except Aborted:
            return 1
        except KeyboardInterrupt:
            self.abort.fire("ctrl-c")
            return 1
        except BaseException as e:
            self.abort.fire(f"error {type(e).__name__}: {e}")
            raise
        finally:
            self.dog.stopped.set()
            self.load.halt()
            if self.abort.fired:
                try:
                    self.abort.done.wait(self.cfg["release_wait_s"] + self.cfg["abort_timeout_s"] + 30)
                except KeyboardInterrupt:
                    self.log("second Ctrl-C: not waiting for the abort verdict")
        return 0


def wait_fresh(tele, cfg, clock, secs=30):
    end = clock.now() + secs
    while clock.now() < end:
        if not stale_keys(cfg, tele):
            return True
        clock.sleep(0.5)
    return False


def interrupt(*args):
    raise KeyboardInterrupt


def main(argv):
    if not argv:
        print("usage: steprun.py duty|fan|pump|cool|heat|rehearse|status [go] [heat target C]")
        return 2
    stage, words = argv[0], argv[1:]
    value = next((float(w) for w in words if w.replace(".", "", 1).isdigit()), None)
    clock = Clock()
    tele = Tail(CFG, clock).start()
    log = Log(CFG["log_dir"], clock)
    wait_fresh(tele, CFG, clock)
    load = Load(CFG, tele)
    session = Session(CFG, tele, Pipe(CFG), load, AbortRunner(CFG), log, clock)
    signal.signal(getattr(signal, "SIGBREAK", signal.SIGTERM), interrupt)
    log(f"steprun {stage} {' '.join(words)}")
    try:
        return session.run(stage, "go" in words, value)
    finally:
        load.halt()
        tele.stop.set()
        log.flush()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
