import copy
import ctypes
import io
import os
import subprocess
import sys
import time
import pyarrow as pa
import pyarrow.ipc as ipc
import pytest
import steprun as S

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = {"water": 30.5, "fan_rpm": 1460.0, "fan_duty": 100.0, "pump_rpm": 2570.0, "pump_duty": 20.0, "cpu_power": 30.0, "gpu_power": 20.0, "cpu_temp": 40.0, "gpu_temp": 40.0}


class FakeClock:
    def __init__(self):
        self.t = 1_000_000.0
        self.hooks = []

    def now(self):
        return self.t

    def sleep(self, secs):
        self.t += secs
        for h in self.hooks:
            h()


class Events(list):
    pass


class Feed:
    def __init__(self, cfg, tele, clock, vals=None):
        self.cfg, self.tele, self.clock = cfg, tele, clock
        self.vals = dict(BASE, **(vals or {}))
        self.on = True
        self.phase = None
        clock.hooks.append(self.push)
        self.push()

    def push(self):
        if not self.on:
            return
        for k, v in self.vals.items():
            if v is not None:
                self.tele.push(self.clock.now(), self.cfg["channels"][k][0], v)
        if self.phase is not None:
            self.tele.push(self.clock.now(), self.cfg["channels"]["phase"][0], self.phase)


class FakePipe:
    def __init__(self, events, clock, drive="firmware", allowed=True):
        self.events, self.clock = events, clock
        self.drive = {"d5.fan": drive, "d5.pump": drive}
        self.allowed = allowed
        self.fail = False
        self.stop_works = True
        self.commands = []
        self.step = None
        self.started_at = None

    def snapshot(self):
        if self.fail:
            raise S.PipeError("fake pipe down")
        controls = [{"id": k, "drive": v, "allowed": self.allowed, "mode": "pid", "actual_rpm": 1000, "duty": 20} for k, v in self.drive.items()]
        return S.Status({"controls": controls, "step_test": self.step, "alarms": []})

    def command(self, cmd):
        if self.fail:
            raise S.PipeError("fake pipe down")
        self.commands.append(cmd)
        if cmd["cmd"] == "stop_step_test":
            self.events.append("pipe.stop")
            if self.stop_works:
                self.drive = {k: "firmware" for k in self.drive}
                self.step = None
        else:
            self.started_at = self.clock.now()
            self.events.append("pipe.start")


class FakeLoad:
    def __init__(self, events, clock):
        self.events, self.clock = events, clock
        self.total = None
        self.calls = []
        self.threads = 8

    def set_total(self, w):
        self.total = w
        self.calls.append((self.clock.now(), w))

    def regulate(self):
        pass

    def own_watts(self):
        return 0.0

    def report(self):
        return "duty fake"

    def halt(self):
        self.total = None
        self.events.append("load.halt")


class Rig:
    def __init__(self, vals=None, drive="firmware", allowed=True, runbook=None):
        self.cfg = copy.deepcopy(S.CFG)
        self.clock = FakeClock()
        self.events = Events()
        self.logs = []
        self.tele = S.Telemetry(self.cfg, self.clock)
        self.feed = Feed(self.cfg, self.tele, self.clock, vals)
        self.pipe = FakePipe(self.events, self.clock, drive, allowed)
        self.load = FakeLoad(self.events, self.clock)
        self.runner_calls = 0
        self.session = S.Session(self.cfg, self.tele, self.pipe, self.load, self.runner, self.logs.append, self.clock, runbook)
        self.session.dog.start = lambda: None
        self.clock.hooks.append(self.session.dog.step)

    def runner(self):
        self.runner_calls += 1
        self.events.append("runner")
        return "OK fake"


def warm(rig, secs=3):
    for _ in range(secs):
        rig.clock.sleep(1.0)


LIMIT_CASES = [
    ("water", {"water": 44.0}),
    ("cpu", {"cpu_temp": 100.0}),
    ("gpu", {"gpu_temp": 80.0}),
    ("pump", {"pump_rpm": 1499.0}),
]


@pytest.mark.parametrize("name,vals", LIMIT_CASES)
def test_every_limit_stops_load_before_anything_else(name, vals):
    rig = Rig()
    warm(rig)
    rig.session.abort.tested = ["d5.fan"]
    assert rig.session.dog.check() == []
    rig.feed.vals.update(vals)
    rig.feed.push()
    hits = rig.session.dog.step()
    assert hits
    assert rig.events[0] == "load.halt" and rig.events[1] == "pipe.stop"
    assert rig.runner_calls == 0 and rig.session.abort.fired


def test_just_below_the_limits_does_not_fire():
    rig = Rig({"water": 43.9, "cpu_temp": 94.9, "gpu_temp": 79.9, "pump_rpm": 1500.0})
    warm(rig)
    assert rig.session.dog.step() == [] and not rig.events


def test_stale_telemetry_fires_after_ten_seconds():
    rig = Rig()
    warm(rig)
    rig.feed.on = False
    rig.clock.t += 9.0
    assert rig.session.dog.step() == []
    rig.clock.t += 2.0
    hits = rig.session.dog.step()
    assert hits and "stale" in hits[0] and "water" in hits[0]
    assert rig.events[0] == "load.halt"


def test_cpu_95_needs_ten_seconds_and_lhm_staleness_is_fifteen():
    rig = Rig({"cpu_temp": 96.0})
    for _ in range(8):
        rig.clock.sleep(1.0)
    assert not rig.session.abort.fired
    for _ in range(4):
        rig.clock.sleep(1.0)
    assert rig.session.abort.fired and "CPU package" in rig.session.abort.fired
    rig = Rig({"cpu_temp": 94.0})
    warm(rig)
    rig.feed.vals["cpu_temp"] = None
    rig.clock.t += 14.0
    rig.feed.vals.update(water=30.5)
    rig.tele.push(rig.clock.now(), rig.cfg["channels"]["water"][0], 30.5)
    for k in ("gpu_temp", "pump_rpm"):
        rig.tele.push(rig.clock.now(), rig.cfg["channels"][k][0], 40.0 if k == "gpu_temp" else 2570.0)
    assert rig.session.dog.check() == []
    rig.clock.t += 2.0
    for k, v in (("water", 30.5), ("gpu_temp", 40.0), ("pump_rpm", 2570.0)):
        rig.tele.push(rig.clock.now(), rig.cfg["channels"][k][0], v)
    assert any("cpu_temp" in h for h in rig.session.dog.check())


def test_missing_channel_counts_as_stale():
    rig = Rig({"cpu_temp": None})
    warm(rig)
    assert any("cpu_temp" in h for h in rig.session.dog.check())


def run_rise(rate, secs, stop=None):
    rig = Rig()
    for i in range(secs):
        rig.feed.vals["water"] = 30.0 + rate * min(i, stop or secs) / 60.0
        rig.clock.sleep(1.0)
        if rig.session.abort.fired:
            return i, rig
    return None, rig


def test_fast_rise_fires_only_after_two_minutes_over_the_rate():
    i, rig = run_rise(2.0, 400)
    assert i is not None and 120 < i <= 200 and "K/min" in rig.session.abort.fired
    assert rig.events[0] == "load.halt"


def test_slow_rise_and_short_burst_do_not_fire():
    assert run_rise(1.2, 400)[0] is None
    assert run_rise(2.0, 300, stop=100)[0] is None


def test_failed_pipe_goes_straight_to_d5_abort():
    rig = Rig({"water": 45.0})
    rig.pipe.fail = True
    warm(rig)
    assert rig.events[0] == "load.halt" and rig.events[-1] == "runner"
    assert rig.runner_calls == 1 and any("d5_abort verdict" in l for l in rig.logs)


def test_channel_not_on_firmware_after_fifteen_seconds_runs_d5_abort():
    rig = Rig({"water": 45.0})
    rig.pipe.stop_works = False
    rig.pipe.drive["d5.fan"] = "test"
    rig.session.abort.tested = ["d5.fan"]
    t0 = rig.clock.now()
    rig.session.dog.step()
    assert rig.events == ["load.halt", "pipe.stop", "runner"]
    assert rig.clock.now() - t0 >= 15


def test_release_inside_the_wait_does_not_run_d5_abort():
    rig = Rig({"water": 45.0})
    rig.pipe.stop_works = False
    rig.pipe.drive["d5.fan"] = "duty"
    rig.session.abort.tested = ["d5.fan"]

    def late():
        if rig.clock.now() > t0 + 5:
            rig.pipe.drive["d5.fan"] = "firmware"

    t0 = rig.clock.now()
    rig.clock.hooks.append(late)
    rig.session.dog.step()
    assert rig.events == ["load.halt", "pipe.stop"]


def test_abort_fires_once():
    rig = Rig({"water": 45.0})
    warm(rig, 5)
    assert rig.events.count("load.halt") == 1 and rig.events.count("pipe.stop") == 1


def fan_part_feed(rig):
    steps = rig.session.rb.parts("fan")[0]["steps"]
    ends, acc = [], 0
    for d, secs, w in steps:
        acc += secs
        ends.append(acc)

    def hook():
        if rig.pipe.started_at is None:
            return
        e = rig.clock.now() - rig.pipe.started_at - 3
        if 0 <= e < ends[-1]:
            ph = next(i for i, x in enumerate(ends) if e < x) + 1
            rig.feed.phase = float(ph)
            rig.pipe.step = {"phases": len(steps), "id": "d5.fan", "phase": ph}
        else:
            rig.feed.phase = None
            rig.pipe.step = None
    rig.clock.hooks.insert(0, hook)
    return steps, ends


def test_fan_stage_follows_the_phase_with_the_loads_of_the_table():
    rig = Rig()
    warm(rig)
    steps, ends = fan_part_feed(rig)
    rig.feed.vals["water"] = 30.5
    assert rig.session.run("fan", True) == 0
    assert [w for t, w in rig.load.calls] == [250.0, 100.0, 75.0, None, None, 250.0, 125.0, None]
    t0 = rig.pipe.started_at + 3
    for k, (t, w) in enumerate(rig.load.calls[:7]):
        assert 0 <= t - (t0 + ([0] + ends)[k]) <= 3
    assert [c["cmd"] for c in rig.pipe.commands] == ["start_step_test"]
    assert rig.pipe.commands[0]["id"] == "d5.fan" and rig.pipe.commands[0]["confirmed"] is True
    assert not rig.session.abort.fired and rig.runner_calls == 0
    assert rig.clock.now() - rig.pipe.started_at >= ends[-1] + 300


def test_table_loads_are_the_runbook_loads():
    rb = S.Runbook()
    assert [s[2] for s in rb.parts("fan")[0]["steps"]] == [250, 100, 75, 70, 70, 250, 125]
    assert [s[2] for s in rb.parts("pump")[0]["steps"]] == [200] * 5
    assert [(p["kind"], len(p["steps"])) for p in rb.parts("duty")] == [("fan", 1), ("pump", 1)]
    assert rb.parts("duty")[0]["rest"] == 180


def test_duty_stage_runs_fan_then_pump_with_the_load_off():
    rig = Rig()
    warm(rig)
    state = {}

    def hook():
        if rig.pipe.started_at is None or not rig.pipe.commands:
            return
        kind = rig.pipe.commands[-1]["id"]
        e = rig.clock.now() - rig.pipe.started_at - 3
        if 0 <= e < 120:
            rig.feed.phase = 1.0
            rig.pipe.step = {"phases": 1, "id": kind}
        else:
            rig.feed.phase = None
            rig.pipe.step = None
        state["ok"] = True

    rig.clock.hooks.insert(0, hook)
    assert rig.session.run("duty", True) == 0
    assert [c["id"] for c in rig.pipe.commands] == ["d5.fan", "d5.pump"]
    assert all(w is None for t, w in rig.load.calls)


def test_a_daemon_plan_with_other_phase_count_aborts():
    rig = Rig()
    warm(rig)
    fan_part_feed(rig)
    rig.clock.hooks.insert(1, lambda: rig.pipe.step and rig.pipe.step.update(phases=5))
    assert rig.session.run("fan", True) == 1
    assert "differs from the table" in rig.session.abort.fired and rig.events[1:3] == ["load.halt", "pipe.stop"]


def test_ctrl_c_is_an_abort():
    rig = Rig({"water": 33.0})
    warm(rig)
    n = {"k": 0}

    def hook():
        n["k"] += 1
        if n["k"] == 10:
            raise KeyboardInterrupt

    rig.clock.hooks.append(hook)
    assert rig.session.run("cool", True) == 1
    assert rig.session.abort.fired == "ctrl-c" and rig.events[0] == "load.halt"


def test_cool_ends_below_the_target_without_any_command():
    rig = Rig({"water": 33.0})
    warm(rig)
    rig.clock.hooks.append(lambda: rig.feed.vals.update(water=rig.feed.vals["water"] - 0.1))
    assert rig.session.run("cool", False) == 0
    assert rig.pipe.commands == [] and rig.tele.value("water") < 31.0


def test_heat_runs_the_light_load_until_the_target_and_stops_it():
    rig = Rig({"water": 28.0})
    warm(rig)
    rig.clock.hooks.append(lambda: rig.feed.vals.update(water=rig.feed.vals["water"] + 0.05))
    assert rig.session.run("heat", True, 30.0) == 0
    assert rig.load.calls[0][1] == 100.0 and rig.events[-1] == "load.halt"
    assert rig.pipe.commands == [] and rig.tele.value("water") >= 30.0


REFUSALS = [
    ("water below the band", dict(vals={"water": 26.9}), True, "outside the band"),
    ("water above the band", dict(vals={"water": 31.1}), True, "outside the band"),
    ("telemetry not fresh", dict(), True, "not fresh"),
    ("channel on duty", dict(drive="duty"), True, "not on firmware"),
    ("writes not allowed", dict(allowed=False), True, "writes not allowed"),
    ("foreign load", dict(vals={"cpu_power": 120.0, "gpu_power": 30.0}), True, "foreign load"),
    ("no go", dict(), False, "go is missing"),
]


@pytest.mark.parametrize("name,kw,go,text", REFUSALS)
def test_stage_refuses_to_start(name, kw, go, text):
    rig = Rig(**kw)
    warm(rig)
    if name == "telemetry not fresh":
        rig.feed.on = False
        rig.clock.t += 12
    assert rig.session.run("fan", go) == 2
    assert text in rig.logs[-1]
    assert rig.pipe.commands == [] and rig.load.calls == []


def test_good_conditions_pass_and_status_acts_on_nothing():
    rig = Rig()
    warm(rig)
    assert rig.session.refusals("fan", True) == []
    assert rig.session.run("status") == 0
    assert rig.pipe.commands == [] and rig.events == []


def test_heat_refuses_when_the_water_is_already_there():
    rig = Rig({"water": 30.2})
    warm(rig)
    assert rig.session.run("heat", True, 30.0) == 2


def test_split_shifts_to_the_gpu_when_the_cpu_runs_hot():
    cfg = copy.deepcopy(S.CFG["load"])
    sp = S.Split(cfg)
    cpu, gpu = sp.step(250.0, 70.0, 50.0, 30.0)
    assert (cpu, gpu) == (100.0, 150.0)
    for _ in range(4):
        cpu, gpu = sp.step(250.0, 91.0, 50.0, 100.0)
    assert cpu == 60.0 and gpu == 190.0
    for _ in range(20):
        cpu, gpu = sp.step(250.0, 91.0, 50.0, 100.0)
    assert cpu == 0.0 and gpu == 250.0
    for _ in range(100):
        cpu, gpu = sp.step(250.0, 70.0, 50.0, 100.0)
    assert (cpu, gpu) == (100.0, 150.0)


def test_split_uses_the_cpu_only_up_to_100_w():
    sp = S.Split(copy.deepcopy(S.CFG["load"]))
    assert sp.step(90.0, 60.0, 40.0, 15.0) == (75.0, 0.0)
    assert sp.step(100.0, 60.0, 40.0, 15.0)[1] == 0.0


def test_pwm_closes_the_loop_on_the_measurement():
    p = S.Pwm(0.004, 120.0)
    assert p.update(0.0, 30.0) == 0.0
    d = p.update(80.0, 30.0)
    assert 0.0 < d <= 1.0
    assert p.update(80.0, 30.0) > d
    assert p.update(80.0, 120.0) < d + 0.5
    assert p.update(80.0, None) == 0.0


def write_stream(path, batches, cut=0):
    sink = io.BytesIO()
    schema = pa.schema([("ts", pa.timestamp("us", tz="UTC")), ("channel", pa.dictionary(pa.int16(), pa.string())), ("value", pa.float64())])
    names = pa.array(["d5next/water/temp", "other", "nvml/0/power"])
    with ipc.new_stream(sink, schema) as w:
        for rows in batches:
            ts = pa.array([int(r[0] * 1e6) for r in rows], pa.timestamp("us", tz="UTC"))
            ch = pa.DictionaryArray.from_arrays(pa.array([r[1] for r in rows], pa.int16()), names)
            w.write_batch(pa.record_batch([ts, ch, pa.array([r[2] for r in rows], pa.float64())], schema=schema))
    data = sink.getvalue()
    return data, len(data) - cut


def test_tail_reads_appended_bytes_and_survives_a_truncated_tail(tmp_path):
    cfg = copy.deepcopy(S.CFG)
    cfg["hot_dir"] = str(tmp_path)
    now = time.time()
    tail = S.Tail(cfg, S.Clock())
    p = tmp_path / "telemetry-00000000000000000001.arrows"
    full, _ = write_stream(p, [[(now - 2, 0, 30.1), (now - 2, 1, 5.0)], [(now - 1, 0, 30.2), (now - 1, 2, 77.0)], [(now, 0, 30.3)]])
    p.write_bytes(full[:-40])
    tail.poll()
    assert tail.value("water") == 30.2 and tail.value("gpu_power") == 77.0
    p.write_bytes(full)
    tail.poll()
    assert tail.value("water") == 30.3 and tail.age("water") < 5
    assert len(tail.series("water", 60)) == 3
    assert "other" not in tail.latest
    q = tmp_path / "telemetry-00000000000000000002.arrows"
    full2, _ = write_stream(q, [[(now + 1, 0, 31.0)]])
    q.write_bytes(full2)
    tail.poll()
    assert tail.value("water") == 31.0 and tail.path == str(q)


def test_pipe_requests_carry_the_version_and_the_command_shapes():
    cfg = copy.deepcopy(S.CFG)
    pipe = S.Pipe(cfg)
    seen = []

    def exchange(req):
        seen.append(req)
        return {"v": 5, "id": req["id"], "body": {"type": "ack"}}

    pipe.exchange = exchange
    pipe.command({"cmd": "start_step_test", "id": "d5.fan", "confirmed": True})
    pipe.command({"cmd": "stop_step_test"})
    assert seen[0] == {"v": 5, "id": 1, "body": {"type": "command", "cmd": "start_step_test", "id": "d5.fan", "confirmed": True}}
    assert seen[1]["body"] == {"type": "command", "cmd": "stop_step_test"}
    pipe.exchange = lambda req: {"v": 1, "id": req["id"], "body": {"type": "ack"}}
    with pytest.raises(S.PipeError):
        pipe.command({"cmd": "stop_step_test"})
    pipe.exchange = lambda req: {"v": 5, "id": req["id"], "body": {"type": "error", "message": "no"}}
    with pytest.raises(S.Refused):
        pipe.snapshot()
    pipe.exchange = lambda req: (_ for _ in ()).throw(PermissionError(13, "denied"))
    with pytest.raises(S.PipeError):
        pipe.snapshot()


def alive(pid):
    k = ctypes.windll.kernel32
    k.OpenProcess.restype = ctypes.c_void_p
    h = k.OpenProcess(0x1000, False, pid)
    if not h:
        return False
    code = ctypes.c_ulong()
    k.GetExitCodeProcess(ctypes.c_void_p(h), ctypes.byref(code))
    k.CloseHandle(ctypes.c_void_p(h))
    return code.value == 259


@pytest.mark.skipif(os.name != "nt", reason="windows only")
@pytest.mark.parametrize("job", [True, False])
def test_load_children_die_with_the_parent(job):
    code = (
        f"import sys, time; sys.path.insert(0, {HERE!r}); import steprun as S\n"
        f"cfg = dict(S.CFG); cfg['load'] = dict(cfg['load'], cpu_threads=1, job={job})\n"
        "L = S.Load(cfg); L.spawn('cpu'); L.duty['cpu'].value = 0.05\n"
        "print(L.pids(), flush=True); time.sleep(60)\n"
    )
    parent = subprocess.Popen([sys.executable, "-c", code], stdout=subprocess.PIPE, text=True)
    try:
        pids = eval(parent.stdout.readline())
        assert len(pids) == 1 and alive(pids[0])
        parent.kill()
        parent.wait()
        end = time.time() + 8
        while alive(pids[0]) and time.time() < end:
            time.sleep(0.1)
        assert not alive(pids[0])
    finally:
        parent.kill()


def test_abort_survives_failing_steps_and_always_signals_done():
    rig = Rig({"water": 45.0})
    rig.pipe.command = lambda cmd: (_ for _ in ()).throw(RuntimeError("boom"))
    rig.load.halt = lambda: (_ for _ in ()).throw(RuntimeError("halt boom"))
    rig.session.abort.runner = lambda: (_ for _ in ()).throw(RuntimeError("runner boom"))
    rig.session.abort.log = lambda t: (_ for _ in ()).throw(OSError("console"))
    rig.session.dog.step()
    assert rig.session.abort.done.is_set()


def test_a_failing_log_never_stops_the_abort():
    rig = Rig({"water": 45.0})
    rig.session.abort.log = lambda t: (_ for _ in ()).throw(OSError("console"))
    rig.session.dog.step()
    assert rig.events[:2] == ["load.halt", "pipe.stop"] and rig.session.abort.done.is_set()


def test_log_writes_the_file_even_when_the_console_fails(tmp_path, monkeypatch):
    log = S.Log(str(tmp_path), S.Clock())
    monkeypatch.setattr("builtins.print", lambda *a, **k: (_ for _ in ()).throw(OSError("blocked")))
    log("hello")
    log.flush()
    assert "hello" in open(log.path, encoding="utf-8").read()


def test_halted_load_does_not_respawn():
    cfg = copy.deepcopy(S.CFG)
    cfg["load"]["job"] = False
    tele = S.Telemetry(cfg, S.Clock())
    load = S.Load(cfg, tele)
    load.halt()
    load.set_total(100.0)
    load.regulate()
    assert load.total is None and load.pids() == [] and load.halted


def test_early_end_is_not_success_and_duty_does_not_go_on_with_the_pump():
    rig = Rig()
    warm(rig)

    def hook():
        if not rig.pipe.commands or rig.pipe.started_at is None:
            return
        e = rig.clock.now() - rig.pipe.started_at - 3
        if 0 <= e < 30:
            rig.feed.phase = 1.0
            rig.pipe.step = {"phases": 1, "id": "d5.fan"}
        else:
            rig.feed.phase = None
            rig.pipe.step = None

    rig.clock.hooks.insert(0, hook)
    assert rig.session.run("duty", True) == 1
    assert [c["id"] for c in rig.pipe.commands if c["cmd"] == "start_step_test"] == ["d5.fan"]
    assert "ended early" in rig.session.abort.fired and "pipe.stop" in rig.events and "load.halt" in rig.events


def test_phase_lines_log_the_overridden_flag():
    rig = Rig()
    warm(rig)
    fan_part_feed(rig)
    rig.session.run("fan", True)
    assert any("overridden" in l and l.startswith("phase 2") for l in rig.logs)


def test_rehearse_runs_250_w_for_120_s_without_water_band_or_commands():
    rig = Rig({"water": 35.0})
    warm(rig)
    t0 = rig.clock.now()
    assert rig.session.run("rehearse", True) == 0
    assert rig.load.calls[0][1] == 250.0 and rig.events[-1] == "load.halt"
    assert 120 <= rig.clock.now() - t0 <= 130 and rig.pipe.commands == []
    assert sum(1 for l in rig.logs if l.startswith("rehearse") and "s cpu" in l) >= 24
    assert S.Session(rig.cfg, rig.tele, rig.pipe, rig.load, rig.runner, rig.logs.append, rig.clock).run("rehearse", False) == 2


def test_pipe_ids_are_local_and_busy_open_error_is_recognised(monkeypatch):
    pipe = S.Pipe(copy.deepcopy(S.CFG))

    def exchange(req):
        pipe.n += 5
        return {"v": 5, "id": req["id"], "body": {"type": "ack"}}

    pipe.exchange = exchange
    pipe.command({"cmd": "stop_step_test"})
    calls = []

    def fake_open(*a, **k):
        calls.append(1)
        if len(calls) < 3:
            raise OSError(22, "Invalid argument")
        raise FileNotFoundError(2, "gone")

    monkeypatch.setattr("builtins.open", fake_open)
    monkeypatch.setattr(S.time, "sleep", lambda s: None)
    pipe2 = S.Pipe(copy.deepcopy(S.CFG))
    with pytest.raises(FileNotFoundError):
        pipe2.exchange({"v": 5, "id": 1, "body": {}})
    assert len(calls) == 3
