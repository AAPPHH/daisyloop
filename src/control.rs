use crate::{Config, Row};
use hidapi::{HidApi, HidDevice};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Crc16Usb;

impl Crc16Usb {
    pub fn compute(data: &[u8]) -> u16 {
        let mut crc = 0xFFFFu16;
        for &b in data {
            crc ^= b as u16;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
            }
        }
        crc ^ 0xFFFF
    }
    pub fn stored(r: &[u8]) -> u16 {
        u16::from_be_bytes([r[r.len() - 2], r[r.len() - 1]])
    }
    pub fn of_report(r: &[u8]) -> u16 {
        Self::compute(&r[1..r.len() - 2])
    }
    pub fn seal(r: &mut [u8]) {
        let c = Self::of_report(r).to_be_bytes();
        let n = r.len();
        r[n - 2..].copy_from_slice(&c);
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct LoopConfig {
    pub autostart: bool,
    pub target_water: Option<f64>,
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    pub min_setpoint: f64,
    pub max_setpoint: f64,
    pub cpu_guard: bool,
}

impl LoopConfig {
    fn for_spec(spec: &Spec) -> LoopConfig {
        LoopConfig { autostart: false, target_water: None, kp: 0.3, ki: 0.002, kd: 0.0, min_setpoint: spec.min as f64 / 100.0, max_setpoint: spec.max as f64 / 100.0, cpu_guard: spec.name == "fan" }
    }
}

impl Default for LoopConfig {
    fn default() -> Self {
        LoopConfig::for_spec(&Spec::FAN)
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ControlConfig {
    pub fan: LoopConfig,
    pub pump: LoopConfig,
    pub water_limit: f64,
    pub cpu_channel: String,
    pub cpu_guard_start: f64,
    pub cpu_guard_gain: f64,
    pub cpu_guard_max_drop: f64,
    pub water_channel: String,
    pub pump_rpm_channel: String,
    pub stale_secs: f64,
    pub lost_secs: f64,
    pub verify_secs: f64,
    pub history_secs: u64,
}

impl Default for ControlConfig {
    fn default() -> Self {
        ControlConfig {
            fan: LoopConfig::for_spec(&Spec::FAN),
            pump: LoopConfig::for_spec(&Spec::PUMP),
            water_limit: 40.0,
            cpu_channel: "/intelcpu/0/temperature/0".into(),
            cpu_guard_start: 80.0,
            cpu_guard_gain: 0.2,
            cpu_guard_max_drop: 4.0,
            water_channel: "d5next/water/temp".into(),
            pump_rpm_channel: "d5next/pump/rpm".into(),
            stale_secs: 5.0,
            lost_secs: 60.0,
            verify_secs: 10.0,
            history_secs: 6 * 3600,
        }
    }
}

pub trait D5Control {
    fn get(&mut self) -> Result<Vec<u8>, String>;
    fn set(&mut self, report: &[u8]) -> Result<(), String>;
    fn settle(&mut self, _ms: u64) {}
}

pub struct HidControl {
    dev: HidDevice,
}

impl HidControl {
    pub fn open(cfg: &Config) -> Result<HidControl, String> {
        let api = HidApi::new().map_err(|e| e.to_string())?;
        let info = api
            .device_list()
            .find(|d| d.vendor_id() == cfg.d5_vid && d.product_id() == cfg.d5_pid && d.usage_page() == cfg.d5_usage_page)
            .ok_or("D5 Next HID interface not found")?;
        Ok(HidControl { dev: info.open_device(&api).map_err(|e| e.to_string())? })
    }
}

impl D5Control for HidControl {
    fn get(&mut self) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; Guard::LEN];
        buf[0] = Guard::ID;
        let n = self.dev.get_feature_report(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(n);
        Ok(buf)
    }
    fn set(&mut self, report: &[u8]) -> Result<(), String> {
        if report.len() != Guard::LEN || report[0] != Guard::ID {
            return Err("refused: only the 809 byte report 0x03 may be sent".into());
        }
        std::thread::sleep(Duration::from_millis(200));
        self.dev.send_feature_report(report).map_err(|e| e.to_string())
    }
    fn settle(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Fault {
    Foreign(String),
    Refused(String),
    Device(String),
}

impl Fault {
    pub fn message(&self) -> &str {
        match self {
            Fault::Foreign(m) | Fault::Refused(m) | Fault::Device(m) => m,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Normal,
    Safety,
    Recover,
}

pub enum Outcome {
    Skipped(&'static str),
    Sent { prev: Vec<u8>, sent: Vec<u8> },
}

pub fn be16(r: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([r[at], r[at + 1]])
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub name: &'static str,
    pub label: &'static str,
    pub base: usize,
    pub end: usize,
    pub min: u16,
    pub max: u16,
}

impl Spec {
    pub const FAN: Spec = Spec { name: "fan", label: "Fan", base: 0x41, end: 0x96, min: 2800, max: 3600 };
    pub const PUMP: Spec = Spec { name: "pump", label: "Pump", base: 0x96, end: 0xEB, min: 3000, max: 4500 };
    pub const ALL: [Spec; 2] = [Spec::FAN, Spec::PUMP];
    pub fn mode(&self) -> usize {
        self.base
    }
    pub fn setpoint(&self) -> usize {
        self.base + 5
    }
    pub fn other(&self) -> Spec {
        if self.base == Spec::FAN.base { Spec::PUMP } else { Spec::FAN }
    }
    pub fn own(&self) -> Range<usize> {
        self.base..self.end
    }
}

pub struct Guard {
    dev: Box<dyn D5Control>,
    expected: Option<Vec<u8>>,
    last_write_ms: Option<i64>,
}

impl Guard {
    pub const LEN: usize = 0x329;
    pub const ID: u8 = 3;
    pub const CRC: usize = 0x327;
    pub const STEP: u16 = 50;
    pub const DEADBAND: u16 = 10;
    pub const INTERVAL_MS: i64 = 60_000;
    pub const READBACK_MS: i64 = 500;
    pub const WATCH_MS: i64 = 5_000;
    pub const PUMP_TOLERANCE_RPM: f64 = 150.0;
    pub const PUMP_MIN_RPM: f64 = 1800.0;
    pub const PUMP_MIN_MS: i64 = 3_000;

    pub fn new(dev: Box<dyn D5Control>) -> Guard {
        Guard { dev, expected: None, last_write_ms: None }
    }
    pub fn validate(r: &[u8]) -> Result<(), String> {
        if r.len() != Self::LEN || r[0] != Self::ID {
            return Err(format!("bad report length {} id {}", r.len(), r.first().copied().unwrap_or(0)));
        }
        if Crc16Usb::stored(r) != Crc16Usb::of_report(r) {
            return Err("crc mismatch on device report".into());
        }
        Ok(())
    }
    pub fn check_diff(fresh: &[u8], target: &[u8], spec: &Spec) -> Result<(), String> {
        Self::validate(target)?;
        if fresh.len() != target.len() {
            return Err("length differs".into());
        }
        let other = spec.other();
        if target[other.own()] != fresh[other.own()] {
            return Err(format!("{} subgroup would change", other.name));
        }
        if Spec::ALL.iter().any(|s| target[s.mode()] != fresh[s.mode()]) {
            return Err("mode byte would change".into());
        }
        let allowed = [spec.setpoint(), spec.setpoint() + 1, Self::CRC, Self::CRC + 1];
        if let Some(o) = (0..target.len()).find(|o| target[*o] != fresh[*o] && !allowed.contains(o)) {
            return Err(format!("byte {o:#05X} outside the {} set-point field would change", spec.name));
        }
        let sp = be16(target, spec.setpoint());
        if !(spec.min..=spec.max).contains(&sp) {
            return Err(format!("{} set-point {sp} outside [{}, {}]", spec.name, spec.min, spec.max));
        }
        Ok(())
    }
    fn fresh(&mut self) -> Result<Vec<u8>, Fault> {
        let r = self.dev.get().map_err(Fault::Device)?;
        Self::validate(&r).map_err(Fault::Device)?;
        Ok(r)
    }
    pub fn adopt(&mut self) -> Result<Vec<u8>, Fault> {
        let r = self.fresh()?;
        self.expected = Some(r.clone());
        Ok(r)
    }
    pub fn verify(&mut self) -> Result<Vec<u8>, Fault> {
        let r = self.fresh()?;
        match &self.expected {
            Some(e) if *e == r => Ok(r),
            Some(_) => Err(Fault::Foreign("device report differs from the last one we wrote".into())),
            None => Err(Fault::Refused("no adopted report".into())),
        }
    }
    pub fn readback(&mut self, sent: &[u8]) -> Result<(), Fault> {
        let r = self.fresh()?;
        if r == sent {
            Ok(())
        } else {
            Err(Fault::Device("readback differs from what was written".into()))
        }
    }
    pub fn settle(&mut self, ms: u64) {
        self.dev.settle(ms);
    }
    pub fn due(&self, now_ms: i64) -> bool {
        self.last_write_ms.is_none_or(|l| now_ms - l >= Self::INTERVAL_MS)
    }
    pub fn write(&mut self, now_ms: i64, spec: &Spec, want: u16, kind: Kind, pending: &mut dyn FnMut(&[u8])) -> Result<Outcome, Fault> {
        if kind == Kind::Normal && !self.due(now_ms) {
            return Ok(Outcome::Skipped("interval"));
        }
        let fresh = self.fresh()?;
        if kind != Kind::Recover {
            match &self.expected {
                Some(e) if *e == fresh => {}
                Some(_) => return Err(Fault::Foreign("device report differs from the last one we wrote".into())),
                None => return Err(Fault::Refused("no adopted report".into())),
            }
        }
        if fresh[spec.mode()] != 1 {
            return Err(Fault::Refused(format!("{} is not in set-point mode 1", spec.name)));
        }
        let cur = be16(&fresh, spec.setpoint());
        let mut target_sp = want.clamp(spec.min, spec.max);
        if kind == Kind::Normal {
            if !(spec.min..=spec.max).contains(&cur) {
                return Err(Fault::Refused(format!("current {} set-point {cur} outside the allowed range", spec.name)));
            }
            target_sp = target_sp.clamp(cur.saturating_sub(Self::STEP), cur + Self::STEP);
            if target_sp.abs_diff(cur) < Self::DEADBAND {
                return Ok(Outcome::Skipped("deadband"));
            }
        } else if target_sp == cur {
            return Ok(Outcome::Skipped("same"));
        }
        let mut target = fresh.clone();
        target[spec.setpoint()..spec.setpoint() + 2].copy_from_slice(&target_sp.to_be_bytes());
        Crc16Usb::seal(&mut target);
        Self::check_diff(&fresh, &target, spec).map_err(Fault::Refused)?;
        pending(&target);
        self.last_write_ms = Some(now_ms);
        self.dev.set(&target).map_err(Fault::Device)?;
        self.expected = Some(target.clone());
        Ok(Outcome::Sent { prev: fresh, sent: target })
    }
}

#[derive(Default, Clone, Debug)]
pub struct Terms {
    pub p: f64,
    pub i: f64,
    pub d: f64,
    pub error: f64,
    pub drop: f64,
    pub desired: f64,
    pub saturated: bool,
}

pub struct Pid {
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    pub lo: f64,
    pub hi: f64,
    pub slew: f64,
    i: f64,
    out: f64,
    prev: Option<f64>,
    d_f: f64,
    pub terms: Terms,
}

impl Pid {
    pub fn new(cfg: &LoopConfig, spec: &Spec) -> Pid {
        let (lo, hi) = (cfg.min_setpoint.max(spec.min as f64 / 100.0), cfg.max_setpoint.min(spec.max as f64 / 100.0));
        Pid { kp: cfg.kp, ki: cfg.ki, kd: cfg.kd, lo, hi, slew: Guard::STEP as f64 / 100.0 / (Guard::INTERVAL_MS as f64 / 1000.0), i: 0.0, out: lo, prev: None, d_f: 0.0, terms: Terms::default() }
    }
    pub fn init(&mut self, target: f64, meas: f64, out: f64) {
        self.out = out;
        self.prev = Some(meas);
        self.d_f = 0.0;
        let range = self.hi - self.lo;
        self.i = (out - target + self.kp * (meas - target)).clamp(-range, range);
    }
    pub fn force(&mut self, out: f64) {
        self.out = out;
        self.terms.saturated = true;
    }
    pub fn output(&self) -> f64 {
        self.out
    }
    pub fn step(&mut self, target: f64, meas: f64, drop: f64, dt: f64) -> f64 {
        if dt <= 0.0 {
            return self.out;
        }
        let e = meas - target;
        let raw_d = self.prev.map_or(0.0, |p| (meas - p) / dt);
        self.d_f += 0.2 * (raw_d - self.d_f);
        self.prev = Some(meas);
        let (p, d) = (-self.kp * e, -self.kd * self.d_f);
        let di = -self.ki * e * dt;
        let range = self.hi - self.lo;
        let limit = |u: f64, out: f64, slew: f64, lo: f64, hi: f64| u.clamp(lo, hi).clamp(out - slew * dt, out + slew * dt);
        let i_new = (self.i + di).clamp(-range, range);
        let u = target + p + i_new + d - drop;
        let mut u_unlimited = u;
        let mut out = limit(u, self.out, self.slew, self.lo, self.hi);
        let held = u.clamp(self.lo, self.hi);
        if ((u - held) * di > 0.0) || ((u - out).abs() > 1.0 && (u - out) * di > 0.0) {
            u_unlimited = target + p + self.i + d - drop;
            out = limit(u_unlimited, self.out, self.slew, self.lo, self.hi);
        } else {
            self.i = i_new;
        }
        self.out = out;
        self.terms = Terms { p, i: self.i, d, error: e, drop, desired: u_unlimited, saturated: (u_unlimited - out).abs() > 1e-9 };
        out
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Recovery {
    fan_base: Option<u16>,
    pump_base: Option<u16>,
    base_report: String,
    expected: String,
    pending: Option<String>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Recovery {
    fn path(dir: &Path) -> PathBuf {
        dir.join("fan_control.json")
    }
    fn load(dir: &Path) -> Option<Recovery> {
        serde_json::from_slice(&std::fs::read(Self::path(dir)).ok()?).ok()
    }
    fn save(&self, dir: &Path) {
        let tmp = dir.join("fan_control.json.tmp");
        let ok = serde_json::to_vec(self).ok().is_some_and(|b| std::fs::write(&tmp, b).is_ok() && std::fs::rename(&tmp, Self::path(dir)).is_ok());
        if !ok {
            log::error!("control: recovery file could not be written");
        }
    }
    fn clear(dir: &Path) {
        let _ = std::fs::remove_file(Self::path(dir));
    }
    fn owns(&self, fresh: &[u8]) -> bool {
        let f = hex(fresh);
        f == self.base_report || f == self.expected || self.pending.as_deref() == Some(f.as_str())
    }
    fn base(&self, spec: &Spec) -> Option<u16> {
        if spec.name == "fan" { self.fan_base } else { self.pump_base }
    }
    fn set_base(&mut self, spec: &Spec, v: Option<u16>) {
        if spec.name == "fan" {
            self.fan_base = v;
        } else {
            self.pump_base = v;
        }
    }
    fn empty(&self) -> bool {
        self.fan_base.is_none() && self.pump_base.is_none()
    }
}

struct Pending {
    at: i64,
    sent: Vec<u8>,
    prev: Vec<u8>,
    pump_base: Option<f64>,
}

enum Phase {
    Off,
    Taken,
    Idle,
    Sent(Pending),
    Watch(Pending),
    Paused(String),
    Fault(String),
}

#[derive(Default)]
struct Inputs {
    water: Option<(i64, f64)>,
    cpu: Option<(i64, f64)>,
    pump: VecDeque<(i64, f64)>,
}

impl Inputs {
    fn absorb(&mut self, rows: &[Row], now: i64, cfg: &ControlConfig) {
        for r in rows {
            if !r.value.is_finite() {
                continue;
            }
            if r.channel == cfg.water_channel {
                self.water = Some((now, r.value));
            } else if r.channel == cfg.cpu_channel {
                self.cpu = Some((now, r.value));
            } else if r.channel == cfg.pump_rpm_channel {
                self.pump.push_back((now, r.value));
            }
        }
        while self.pump.front().is_some_and(|s| now - s.0 > 30_000) {
            self.pump.pop_front();
        }
    }
    fn pump_baseline(&self, now: i64) -> Option<f64> {
        let v: Vec<f64> = self.pump.iter().rev().filter(|s| now - s.0 <= 10_000).take(5).map(|s| s.1).collect();
        let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |a, &x| (a.0.min(x), a.1.max(x)));
        (v.len() >= 3 && hi - lo <= Guard::PUMP_TOLERANCE_RPM / 2.0).then(|| v.iter().sum::<f64>() / v.len() as f64)
    }
    fn pump_since(&self, at: i64) -> Vec<f64> {
        self.pump.iter().filter(|s| s.0 >= at).map(|s| s.1).collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct LoopStatus {
    pub name: &'static str,
    pub allowed: bool,
    pub state: u8,
    pub phase: String,
    pub setpoint: Option<f64>,
    pub target: f64,
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    pub lo: f64,
    pub hi: f64,
    pub terms: Terms,
    pub output: f64,
}

struct Loop {
    spec: Spec,
    allowed: bool,
    autostart: bool,
    cpu_guard: bool,
    phase: Phase,
    pid: Pid,
    target: f64,
    configured_target: Option<f64>,
    current_centi: Option<u16>,
    last_pid: i64,
    lost: bool,
    over: bool,
    low_since: Option<i64>,
}

impl Loop {
    fn new(spec: Spec, cfg: &LoopConfig, allowed: bool) -> Loop {
        Loop {
            spec,
            allowed,
            autostart: cfg.autostart,
            cpu_guard: cfg.cpu_guard,
            phase: Phase::Off,
            pid: Pid::new(cfg, &spec),
            target: cfg.target_water.unwrap_or(spec.min as f64 / 100.0),
            configured_target: cfg.target_water,
            current_centi: None,
            last_pid: 0,
            lost: false,
            over: false,
            low_since: None,
        }
    }
    fn running(&self) -> bool {
        matches!(self.phase, Phase::Taken | Phase::Idle | Phase::Sent(_) | Phase::Watch(_))
    }
    fn busy(&self) -> bool {
        matches!(self.phase, Phase::Sent(_) | Phase::Watch(_))
    }
    fn floor(&self) -> u16 {
        (self.pid.lo * 100.0).round() as u16
    }
    fn status(&self) -> LoopStatus {
        let (state, phase) = match &self.phase {
            Phase::Off => (0, "off".to_string()),
            Phase::Taken => (1, "running".to_string()),
            Phase::Idle => (1, "running".to_string()),
            Phase::Sent(_) => (1, "write sent".to_string()),
            Phase::Watch(_) => (1, "write verified, watching".to_string()),
            Phase::Paused(w) => (2, format!("paused: {w}")),
            Phase::Fault(w) => (3, format!("fault: {w}")),
        };
        LoopStatus {
            name: self.spec.name,
            allowed: self.allowed,
            state,
            phase,
            setpoint: self.current_centi.map(|c| c as f64 / 100.0),
            target: self.target,
            kp: self.pid.kp,
            ki: self.pid.ki,
            kd: self.pid.kd,
            lo: self.pid.lo,
            hi: self.pid.hi,
            terms: self.pid.terms.clone(),
            output: self.pid.output(),
        }
    }
    fn rows(&self) -> Vec<Row> {
        let s = self.status();
        let (name, label) = (self.spec.name, self.spec.label);
        let row = |ch: &str, text: &str, kind: &'static str, unit: &str, v: f64, role: &'static str| Row::new("daisyloop", format!("daisyloop/{name}/{ch}"), format!("{label} control {text}"), kind, unit, v).role(role);
        let mut rows = vec![row("state", "state", "other", "", s.state as f64, "input")];
        if s.state == 0 {
            return rows;
        }
        if let Some(sp) = s.setpoint {
            rows.push(row("setpoint", "commanded set-point", "temperature", "degC", sp, "input"));
        }
        rows.push(row("target_water", "target water", "temperature", "degC", s.target, "input"));
        for (ch, text, v) in [
            ("pid_p", "P term", s.terms.p),
            ("pid_i", "I term", s.terms.i),
            ("pid_d", "D term", s.terms.d),
            ("pid_error", "error", s.terms.error),
            ("cpu_drop", "CPU guard drop", s.terms.drop),
            ("pid_desired", "unlimited output", s.terms.desired),
            ("pid_output", "limited output", s.output),
        ] {
            rows.push(row(ch, text, "other", "degC", v, "aux"));
        }
        rows
    }
}

pub type Factory = Box<dyn Fn() -> Result<Box<dyn D5Control>, String>>;

pub struct Control {
    cfg: ControlConfig,
    dir: PathBuf,
    factory: Factory,
    guard: Option<Guard>,
    recovery: Option<Recovery>,
    inputs: Inputs,
    loops: Vec<Loop>,
    last_verify: i64,
    misses: u32,
}

impl Control {
    pub fn new(cfg: &Config, factory: Factory) -> Control {
        let flags = [cfg.fan_control_enabled, cfg.pump_control_enabled];
        let loops = Spec::ALL.iter().zip(flags).map(|(s, a)| Loop::new(*s, if s.name == "fan" { &cfg.control.fan } else { &cfg.control.pump }, a)).collect();
        let mut c = Control { cfg: cfg.control.clone(), dir: cfg.data_dir.clone(), factory, guard: None, recovery: None, inputs: Inputs::default(), loops, last_verify: 0, misses: 0 };
        if let Some(rec) = Recovery::load(&c.dir) {
            c.recover(rec);
        }
        c
    }
    pub fn index(&self, name: &str) -> Option<usize> {
        self.loops.iter().position(|l| l.spec.name == name)
    }
    pub fn allowed(&self, name: &str) -> bool {
        self.index(name).is_some_and(|k| self.loops[k].allowed)
    }
    pub fn running(&self, name: &str) -> bool {
        self.index(name).is_some_and(|k| self.loops[k].running())
    }
    pub fn autostart(&self) -> Vec<&'static str> {
        self.loops.iter().filter(|l| l.allowed && l.autostart).map(|l| l.spec.name).collect()
    }
    pub fn statuses(&self) -> Vec<LoopStatus> {
        self.loops.iter().map(|l| l.status()).collect()
    }
    fn recover(&mut self, mut rec: Recovery) {
        let result = (self.factory)().map(Guard::new).map_err(Fault::Device).and_then(|mut g| g.adopt().map(|f| (g, f)));
        let (mut guard, fresh) = match result {
            Ok(x) => x,
            Err(e) => return log::error!("control: recovery could not read the device: {}", e.message()),
        };
        if !rec.owns(&fresh) {
            log::warn!("control: device report is not ours, recovery file dropped, nothing written");
            return Recovery::clear(&self.dir);
        }
        for spec in Spec::ALL {
            let Some(base) = rec.base(&spec) else { continue };
            if !self.allowed(spec.name) {
                log::error!("control: recovery has a {0} base set-point but {0}_control_enabled is false, nothing written", spec.name);
                continue;
            }
            let cur = guard.adopt().map(|r| be16(&r, spec.setpoint()));
            let restored = match cur {
                Ok(c) if c == base => true,
                Err(e) => {
                    log::error!("control: crash recovery could not read the device: {}", e.message());
                    false
                }
                Ok(_) => match guard.write(0, &spec, base, Kind::Recover, &mut |_| {}) {
                    Ok(Outcome::Sent { sent, .. }) => {
                        guard.settle(Guard::READBACK_MS as u64);
                        let ok = guard.readback(&sent).is_ok();
                        if ok {
                            log::warn!("control: crash recovery restored {} set-point {:.2}", spec.name, base as f64 / 100.0);
                        } else {
                            log::error!("control: crash recovery readback failed");
                        }
                        ok
                    }
                    Ok(Outcome::Skipped(_)) => true,
                    Err(e) => {
                        log::error!("control: crash recovery refused: {}", e.message());
                        false
                    }
                },
            };
            if restored {
                rec.set_base(&spec, None);
            }
        }
        if rec.empty() {
            Recovery::clear(&self.dir);
        } else {
            rec.expected = hex(&guard.adopt().unwrap_or_default());
            rec.pending = None;
            rec.save(&self.dir);
        }
    }
    pub fn enable(&mut self, name: &str, now: i64) -> Result<(), String> {
        let k = self.index(name).ok_or("unknown control")?;
        if !self.loops[k].allowed {
            return Err(format!("{name} control is disabled in the config ({name}_control_enabled)"));
        }
        if self.loops[k].running() {
            return Ok(());
        }
        let spec = self.loops[k].spec;
        let stale = (self.cfg.stale_secs * 1000.0) as i64;
        let (water, age) = self.inputs.water.map(|w| (w.1, now - w.0)).ok_or("no water temperature yet")?;
        if age > stale || water >= self.cfg.water_limit {
            return Err("water temperature stale or at the limit".into());
        }
        if spec.name == "pump" && self.inputs.pump.back().is_none_or(|p| now - p.0 > stale || p.1 < Guard::PUMP_MIN_RPM) {
            return Err("pump rpm stale or below the hard minimum".into());
        }
        let fresh = match self.guard.as_mut() {
            Some(g) => g.verify().map_err(|e| e.message().to_string())?,
            None => {
                let mut g = Guard::new((self.factory)()?);
                let f = g.adopt().map_err(|e| e.message().to_string())?;
                self.guard = Some(g);
                f
            }
        };
        let cur = be16(&fresh, spec.setpoint());
        let checked = if fresh[spec.mode()] != 1 {
            Err(format!("{name} is not in set-point mode 1"))
        } else if !(spec.min..=spec.max).contains(&cur) {
            Err(format!("current {name} set-point {cur} outside the allowed range"))
        } else {
            Ok(())
        };
        if let Err(e) = checked {
            if !self.loops.iter().any(|l| l.running()) {
                self.guard = None;
            }
            return Err(e);
        }
        let mut rec = match self.recovery.take().or_else(|| Recovery::load(&self.dir).filter(|r| r.owns(&fresh))) {
            Some(r) => Recovery { expected: hex(&fresh), pending: None, ..r },
            None => Recovery { fan_base: None, pump_base: None, base_report: hex(&fresh), expected: hex(&fresh), pending: None },
        };
        if rec.base(&spec).is_none() {
            rec.set_base(&spec, Some(cur));
        }
        rec.save(&self.dir);
        let base = rec.base(&spec).unwrap_or(cur);
        self.recovery = Some(rec);
        let l = &mut self.loops[k];
        l.target = l.configured_target.unwrap_or(base as f64 / 100.0).clamp(l.pid.lo, l.pid.hi);
        l.pid.init(l.target, water, cur as f64 / 100.0);
        l.current_centi = Some(cur);
        l.phase = Phase::Idle;
        (l.last_pid, l.lost, l.over, l.low_since) = (now, false, false, None);
        self.last_verify = now;
        self.misses = 0;
        log::info!("{name} control enabled, base set-point {:.2}, target water {:.2}", base as f64 / 100.0, l.target);
        Ok(())
    }
    pub fn disable(&mut self, name: &str, now: i64) {
        let Some(k) = self.index(name) else { return };
        if self.loops[k].running() {
            self.restore_base(k, now);
        }
        if !matches!(self.loops[k].phase, Phase::Fault(_)) {
            self.loops[k].phase = Phase::Off;
        }
        if !self.loops.iter().any(|l| l.running()) {
            self.guard = None;
        }
        log::info!("{name} control disabled");
    }
    pub fn shutdown(&mut self, now: i64) {
        for s in Spec::ALL {
            self.disable(s.name, now);
        }
    }
    fn restore_base(&mut self, k: usize, now: i64) {
        let spec = self.loops[k].spec;
        let (Some(mut rec), Some(guard)) = (self.recovery.take(), self.guard.as_mut()) else { return };
        let Some(base) = rec.base(&spec) else {
            self.recovery = Some(rec);
            return;
        };
        let done = match guard.write(now, &spec, base, Kind::Safety, &mut |_| {}) {
            Ok(Outcome::Sent { sent, .. }) => {
                guard.settle(Guard::READBACK_MS as u64);
                match guard.readback(&sent) {
                    Ok(()) => {
                        log::info!("{} base set-point {:.2} restored", spec.name, base as f64 / 100.0);
                        rec.expected = hex(&sent);
                        rec.pending = None;
                        true
                    }
                    Err(e) => {
                        log::error!("control: restore readback failed: {}", e.message());
                        false
                    }
                }
            }
            Ok(Outcome::Skipped(_)) => true,
            Err(Fault::Foreign(m)) => {
                log::error!("control: not restoring, device changed by someone else: {m}");
                rec.fan_base = None;
                rec.pump_base = None;
                true
            }
            Err(e) => {
                log::error!("control: restore failed: {}", e.message());
                false
            }
        };
        if done {
            rec.set_base(&spec, None);
        }
        if rec.empty() {
            Recovery::clear(&self.dir);
        } else {
            rec.save(&self.dir);
            self.recovery = Some(rec);
        }
    }
    pub fn set_pid(&mut self, name: &str, target: f64, kp: f64, ki: f64, kd: f64) -> Result<(), String> {
        let k = self.index(name).ok_or("unknown control")?;
        if !self.loops[k].allowed {
            return Err(format!("{name} control is disabled in the config ({name}_control_enabled)"));
        }
        if ![target, kp, ki, kd].iter().all(|v| v.is_finite()) || kp < 0.0 || ki < 0.0 || kd < 0.0 || kp > 10.0 || ki > 1.0 || kd > 100.0 {
            return Err("pid parameters out of range".into());
        }
        let water = self.inputs.water;
        let l = &mut self.loops[k];
        if target < l.pid.lo || target > l.pid.hi {
            return Err(format!("target water must be within {:.1}..{:.1}", l.pid.lo, l.pid.hi));
        }
        (l.target, l.pid.kp, l.pid.ki, l.pid.kd, l.configured_target) = (target, kp, ki, kd, Some(target));
        if let (true, Some((_, w))) = (l.running(), water) {
            l.pid.init(target, w, l.pid.output());
        }
        log::info!("{name} control: pid set target {target:.2} kp {kp} ki {ki} kd {kd}");
        Ok(())
    }
    fn stop_all(&mut self, phase: impl Fn() -> Phase) {
        for l in self.loops.iter_mut().filter(|l| l.running()) {
            l.phase = phase();
        }
        self.guard = None;
    }
    fn pause(&mut self, why: String) {
        log::error!("control paused: {why}");
        self.stop_all(|| Phase::Paused(why.clone()));
    }
    fn fail(&mut self, now: i64, why: String, restore: Option<(Spec, u16)>) {
        log::error!("control disabled: {why}");
        if let (Some(g), Some((spec, sp))) = (self.guard.as_mut(), restore) {
            match g.write(now, &spec, sp, Kind::Recover, &mut |_| {}) {
                Ok(Outcome::Sent { .. }) => log::error!("control: {} restored to {:.2} once", spec.name, sp as f64 / 100.0),
                Ok(Outcome::Skipped(_)) => {}
                Err(e) => log::error!("control: restore failed: {}", e.message()),
            }
        }
        self.stop_all(|| Phase::Fault(why.clone()));
    }
    fn command(&mut self, k: usize, now: i64, want: u16, kind: Kind) -> bool {
        if self.loops.iter().any(|l| l.busy()) {
            return false;
        }
        let spec = self.loops[k].spec;
        let pump_base = if spec.name == "fan" { self.inputs.pump_baseline(now) } else { None };
        if kind == Kind::Normal && spec.name == "fan" && pump_base.is_none() {
            return false;
        }
        let dir = self.dir.clone();
        let (Some(guard), Some(rec)) = (self.guard.as_mut(), self.recovery.as_mut()) else { return false };
        let mut pending = |t: &[u8]| {
            rec.pending = Some(hex(t));
            rec.save(&dir);
        };
        self.last_verify = now;
        match guard.write(now, &spec, want, kind, &mut pending) {
            Ok(Outcome::Sent { prev, sent }) => {
                let (a, b) = (be16(&prev, spec.setpoint()), be16(&sent, spec.setpoint()));
                log::info!("{} control: set-point {:.2} -> {:.2} ({kind:?})", spec.name, a as f64 / 100.0, b as f64 / 100.0);
                self.loops[k].current_centi = Some(b);
                self.loops[k].phase = Phase::Sent(Pending { at: now, sent, prev, pump_base });
                true
            }
            Ok(Outcome::Skipped(_)) => false,
            Err(Fault::Foreign(m)) => {
                self.pause(m);
                false
            }
            Err(e) => {
                self.fail(now, e.message().to_string(), None);
                false
            }
        }
    }
    fn check_pump(&self, p: &Pending) -> Result<(), String> {
        let Some(base) = p.pump_base else { return Ok(()) };
        match self.inputs.pump_since(p.at).iter().find(|v| (**v - base).abs() > Guard::PUMP_TOLERANCE_RPM) {
            Some(v) => Err(format!("pump rpm {v:.0} deviates from baseline {base:.0} after the fan write")),
            None => Ok(()),
        }
    }
    fn verified(&mut self, p: &Pending) {
        if let Some(rec) = self.recovery.as_mut() {
            rec.expected = hex(&p.sent);
            rec.pending = None;
            rec.save(&self.dir);
        }
    }
    fn after_write(&mut self, k: usize, p: Pending, now: i64, watching: bool) {
        let spec = self.loops[k].spec;
        let restore = Some((spec, be16(&p.prev, spec.setpoint())));
        if let Err(m) = self.check_pump(&p) {
            return self.fail(now, m, restore);
        }
        if !watching {
            if now - p.at < Guard::READBACK_MS {
                self.loops[k].phase = Phase::Sent(p);
                return;
            }
            return match self.guard.as_mut().map(|g| g.readback(&p.sent)) {
                Some(Ok(())) => {
                    self.verified(&p);
                    self.loops[k].phase = Phase::Watch(p);
                }
                Some(Err(e)) => self.fail(now, e.message().to_string(), restore),
                None => self.loops[k].phase = Phase::Fault("no device handle".into()),
            };
        }
        if now - p.at < Guard::WATCH_MS {
            self.loops[k].phase = Phase::Watch(p);
        } else if p.pump_base.is_some() && self.inputs.pump_since(p.at).len() < 2 {
            self.fail(now, "no pump telemetry after the write".into(), restore);
        } else {
            self.last_verify = now;
            self.loops[k].phase = Phase::Idle;
        }
    }
    fn pump_hard_stop(&mut self, k: usize, now: i64, rows: &[Row]) -> bool {
        let stale = (self.cfg.stale_secs * 1000.0) as i64;
        let fresh_pump = rows.iter().find(|r| r.channel == self.cfg.pump_rpm_channel).map(|r| r.value);
        let l = &mut self.loops[k];
        match fresh_pump {
            Some(v) if v < Guard::PUMP_MIN_RPM => {
                l.low_since.get_or_insert(now);
            }
            Some(_) => l.low_since = None,
            None => {}
        }
        let why = if l.low_since.is_some_and(|t| now - t >= Guard::PUMP_MIN_MS) {
            Some("pump rpm below 1800 for 3 s")
        } else if self.inputs.pump.back().is_none_or(|p| now - p.0 > stale) {
            Some("pump rpm telemetry stale")
        } else if self.inputs.water.is_none_or(|w| now - w.0 > stale) {
            Some("water telemetry stale while the pump is controlled")
        } else {
            None
        };
        let Some(why) = why else { return false };
        let spec = self.loops[k].spec;
        self.fail(now, format!("pump hard stop: {why}"), Some((spec, spec.min)));
        true
    }
    fn idle(&mut self, k: usize, now: i64) {
        let floor = self.loops[k].floor();
        let stale = (self.cfg.stale_secs * 1000.0) as i64;
        let Some(meas) = self.inputs.water.filter(|(t, _)| now - t <= stale).map(|(_, x)| x) else {
            let waited = self.inputs.water.map_or(i64::MAX, |w| now - w.0);
            if waited > (self.cfg.lost_secs * 1000.0) as i64 && !self.loops[k].lost {
                self.loops[k].lost = true;
                log::error!("{} control: water temperature lost, set-point to floor", self.loops[k].spec.name);
                self.command(k, now, floor, Kind::Safety);
            }
            return;
        };
        let limit = self.cfg.water_limit;
        let drop = match (self.loops[k].cpu_guard, self.inputs.cpu) {
            (true, Some((t, c))) if now - t <= stale => (self.cfg.cpu_guard_gain * (c - self.cfg.cpu_guard_start)).clamp(0.0, self.cfg.cpu_guard_max_drop),
            _ => 0.0,
        };
        let l = &mut self.loops[k];
        let dt = ((now - l.last_pid) as f64 / 1000.0).clamp(0.0, 5.0);
        l.last_pid = now;
        let held = l.current_centi.map_or(l.pid.lo, |c| c as f64 / 100.0);
        if l.lost {
            l.lost = false;
            l.pid.init(l.target, meas, held);
        }
        if meas >= limit {
            if !l.over {
                log::error!("{} control: water {meas:.2} at the limit {limit:.1}, set-point to floor", l.spec.name);
            }
            l.over = true;
        } else if l.over && meas < limit - 1.0 {
            l.over = false;
            l.pid.init(l.target, meas, held);
        }
        if l.over {
            l.pid.force(l.pid.lo);
            if l.current_centi != Some(floor) {
                self.command(k, now, floor, Kind::Safety);
            }
            return;
        }
        let want = (l.pid.step(l.target, meas, drop, dt) * 100.0).round() as u16;
        let moved = l.current_centi.is_none_or(|c| c.abs_diff(want) >= Guard::DEADBAND);
        if moved && self.guard.as_ref().is_some_and(|g| g.due(now)) {
            self.command(k, now, want, Kind::Normal);
        }
    }
    fn verify(&mut self, now: i64) {
        if self.loops.iter().any(|l| l.busy()) || !self.loops.iter().any(|l| l.running()) || now - self.last_verify < (self.cfg.verify_secs * 1000.0) as i64 {
            return;
        }
        self.last_verify = now;
        match self.guard.as_mut().map(|g| g.verify()) {
            Some(Ok(_)) => self.misses = 0,
            Some(Err(Fault::Foreign(m))) => self.pause(m),
            Some(Err(e)) => {
                self.misses += 1;
                if self.misses >= 3 {
                    self.fail(now, format!("device lost: {}", e.message()), None);
                }
            }
            None => {}
        }
    }
    pub fn step(&mut self, rows: &mut Vec<Row>, now: i64) {
        self.inputs.absorb(rows, now, &self.cfg);
        let pump = self.index("pump").unwrap();
        let stopped = self.loops[pump].running() && self.pump_hard_stop(pump, now, rows);
        if !stopped {
            for k in 0..self.loops.len() {
                match std::mem::replace(&mut self.loops[k].phase, Phase::Taken) {
                    Phase::Idle => {
                        self.loops[k].phase = Phase::Idle;
                        self.idle(k, now);
                    }
                    Phase::Sent(p) => self.after_write(k, p, now, false),
                    Phase::Watch(p) => self.after_write(k, p, now, true),
                    other => self.loops[k].phase = other,
                }
            }
            self.verify(now);
        }
        rows.extend(self.loops.iter().filter(|l| l.allowed).flat_map(|l| l.rows()));
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
