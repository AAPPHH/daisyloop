use crate::d5::{Actuator, Crc16Usb, D5Actuator, D5Control, Guard, Setting, Spec};
use crate::model::{Axis, Model, Operating, Plan, Planner, Temps};
use crate::protocol::{Alarm, AlarmKind, Control as PControl, ControlKind, Drive, Limit, Mode, ModelInfo, Predicted, StepStatus};
use crate::{Config, Row};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Debug, PartialEq)]
pub struct Calibration {
    points: Vec<(f64, f64)>,
}

impl Calibration {
    pub fn new(points: &[[f64; 2]]) -> Result<Calibration, String> {
        let p: Vec<(f64, f64)> = points.iter().map(|x| (x[0], x[1])).collect();
        let sorted = p.windows(2).all(|w| w[1].0 > w[0].0 && w[1].1 >= w[0].1);
        if p.len() < 2 || !sorted || p.iter().any(|x| !x.0.is_finite() || !x.1.is_finite() || x.1 < 0.0) {
            return Err("curve needs at least two points with rising duty and non-falling rpm".into());
        }
        Ok(Calibration { points: p })
    }
    fn interp(from: &[(f64, f64)], x: f64) -> f64 {
        let (first, last) = (from[0], from[from.len() - 1]);
        if x <= first.0 {
            return first.1;
        }
        if x >= last.0 {
            return last.1;
        }
        let i = from.windows(2).position(|w| x <= w[1].0).unwrap();
        let (a, b) = (from[i], from[i + 1]);
        a.1 + (b.1 - a.1) * (x - a.0) / (b.0 - a.0)
    }
    pub fn rpm(&self, duty: f64) -> f64 {
        Self::interp(&self.points, duty)
    }
    pub fn duty(&self, rpm: f64) -> f64 {
        let flipped: Vec<(f64, f64)> = self.points.iter().map(|p| (p.1, p.0)).collect();
        let mut dedup: Vec<(f64, f64)> = Vec::new();
        for p in flipped {
            if dedup.last().is_none_or(|l| p.0 > l.0) {
                dedup.push(p);
            }
        }
        if dedup.len() < 2 {
            return self.points[0].0;
        }
        Self::interp(&dedup, rpm)
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ChannelOverride {
    pub floor_duty: Option<f64>,
    pub max_duty: Option<f64>,
    pub step_up_rpm: Option<f64>,
    pub step_down_rpm: Option<f64>,
    pub deadband_duty: Option<f64>,
    pub autostart_active: Option<bool>,
    pub curve: Option<Vec<[f64; 2]>>,
    pub rpm_tolerance: Option<f64>,
    pub alarm_rpm: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct ChannelConfig {
    pub floor_duty: f64,
    pub max_duty: f64,
    pub step_up_rpm: f64,
    pub step_down_rpm: f64,
    pub deadband_duty: f64,
    pub autostart_active: bool,
    pub curve: Vec<[f64; 2]>,
    pub rpm_tolerance: f64,
    pub alarm_rpm: f64,
}

impl ChannelConfig {
    pub fn defaults(kind: ControlKind) -> ChannelConfig {
        match kind {
            ControlKind::Fan => ChannelConfig { floor_duty: 30.0, max_duty: 100.0, step_up_rpm: 700.0, step_down_rpm: 150.0, deadband_duty: 2.0, autostart_active: false, curve: vec![[0.0, 0.0], [100.0, 1470.0]], rpm_tolerance: 0.25, alarm_rpm: 1.0 },
            ControlKind::Pump => ChannelConfig { floor_duty: 40.0, max_duty: 100.0, step_up_rpm: 1000.0, step_down_rpm: 200.0, deadband_duty: 2.0, autostart_active: false, curve: vec![[0.0, 2000.0], [19.8, 2570.0], [100.0, 4800.0]], rpm_tolerance: 0.25, alarm_rpm: 1800.0 },
        }
    }
    pub fn resolve(kind: ControlKind, o: &ChannelOverride) -> Result<ChannelConfig, String> {
        let d = ChannelConfig::defaults(kind);
        let c = ChannelConfig {
            floor_duty: o.floor_duty.unwrap_or(d.floor_duty),
            max_duty: o.max_duty.unwrap_or(d.max_duty),
            step_up_rpm: o.step_up_rpm.unwrap_or(d.step_up_rpm),
            step_down_rpm: o.step_down_rpm.unwrap_or(d.step_down_rpm),
            deadband_duty: o.deadband_duty.unwrap_or(d.deadband_duty),
            autostart_active: o.autostart_active.unwrap_or(d.autostart_active),
            curve: o.curve.clone().unwrap_or(d.curve),
            rpm_tolerance: o.rpm_tolerance.unwrap_or(d.rpm_tolerance),
            alarm_rpm: o.alarm_rpm.unwrap_or(d.alarm_rpm),
        };
        let all = [c.floor_duty, c.max_duty, c.step_up_rpm, c.step_down_rpm, c.deadband_duty, c.rpm_tolerance, c.alarm_rpm];
        if all.iter().any(|v| !v.is_finite()) || c.floor_duty < 0.0 || c.max_duty > 100.0 || c.floor_duty > c.max_duty {
            return Err("duty floor and ceiling must satisfy 0 <= floor <= max <= 100".into());
        }
        if c.step_up_rpm <= 0.0 || c.step_down_rpm <= 0.0 || c.deadband_duty < 0.0 || !(0.0..=1.0).contains(&c.rpm_tolerance) || c.alarm_rpm < 0.0 {
            return Err("step, deadband, tolerance or alarm value out of range".into());
        }
        Calibration::new(&c.curve)?;
        Ok(c)
    }
    pub fn calibration(&self) -> Calibration {
        Calibration::new(&self.curve).unwrap()
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct StepTestConfig {
    pub fan: Vec<[f64; 2]>,
    pub pump: Vec<[f64; 2]>,
}

impl Default for StepTestConfig {
    fn default() -> Self {
        StepTestConfig {
            fan: vec![[100.0, 600.0], [80.0, 600.0], [60.0, 600.0], [40.0, 600.0], [100.0, 600.0]],
            pump: vec![[100.0, 600.0], [70.0, 600.0], [50.0, 600.0], [100.0, 600.0]],
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ControlConfig {
    pub fan: ChannelOverride,
    pub pump: ChannelOverride,
    pub step_test: StepTestConfig,
    pub decision_secs: f64,
    pub horizon_secs: u32,
    pub rollout_secs: u32,
    pub warmup_secs: i64,
    pub fan_candidates: usize,
    pub pump_candidates: usize,
    pub weight: f64,
    pub weight_span_k: f64,
    pub pump_noise: f64,
    pub water_plan_limit: f64,
    pub cpu_plan_limit: f64,
    pub gpu_plan_limit: f64,
    pub water_limit: f64,
    pub cpu_limit: f64,
    pub cpu_hold_secs: f64,
    pub stale_secs: f64,
    pub pump_low_secs: f64,
    pub fan_stop_secs: f64,
    pub follow_settle_secs: f64,
    pub follow_secs: f64,
    pub water_channel: String,
    pub cpu_channel: String,
    pub gpu_channel: String,
    pub model_spec: String,
    pub model_file: String,
    pub history_secs: u64,
}

impl Default for ControlConfig {
    fn default() -> Self {
        ControlConfig {
            fan: ChannelOverride::default(),
            pump: ChannelOverride::default(),
            step_test: StepTestConfig::default(),
            decision_secs: 30.0,
            horizon_secs: 120,
            rollout_secs: 120,
            warmup_secs: 1800,
            fan_candidates: 7,
            pump_candidates: 5,
            weight: 0.5,
            weight_span_k: 5.0,
            pump_noise: 0.5,
            water_plan_limit: 38.0,
            cpu_plan_limit: 95.0,
            gpu_plan_limit: 80.0,
            water_limit: 40.0,
            cpu_limit: 104.0,
            cpu_hold_secs: 60.0,
            stale_secs: 10.0,
            pump_low_secs: 3.0,
            fan_stop_secs: 10.0,
            follow_settle_secs: 15.0,
            follow_secs: 60.0,
            water_channel: "d5next/water/temp".into(),
            cpu_channel: "/intelcpu/0/temperature/0".into(),
            gpu_channel: "nvml/0/temp".into(),
            model_spec: "feature_spec_deploy.json".into(),
            model_file: "model_deploy.json".into(),
            history_secs: 6 * 3600,
        }
    }
}

impl ControlConfig {
    pub fn validate(&self) -> Result<(), String> {
        let nums = [self.decision_secs, self.weight, self.weight_span_k, self.pump_noise, self.water_plan_limit, self.cpu_plan_limit, self.gpu_plan_limit, self.water_limit, self.cpu_limit, self.cpu_hold_secs, self.stale_secs, self.pump_low_secs, self.fan_stop_secs, self.follow_settle_secs, self.follow_secs];
        if nums.iter().any(|v| !v.is_finite() || *v < 0.0) || self.decision_secs < 5.0 || self.stale_secs < 2.0 {
            return Err("timing or limit value out of range".into());
        }
        if !(0.0..=1.0).contains(&self.weight) || self.horizon_secs == 0 || self.rollout_secs == 0 || self.fan_candidates < 2 || self.pump_candidates < 2 || self.fan_candidates > 16 || self.pump_candidates > 16 {
            return Err("weight, horizon, rollout or candidate count out of range".into());
        }
        for (kind, o, steps) in [(ControlKind::Fan, &self.fan, &self.step_test.fan), (ControlKind::Pump, &self.pump, &self.step_test.pump)] {
            let c = ChannelConfig::resolve(kind, o)?;
            if steps.iter().any(|s| s[0] < c.floor_duty || s[0] > c.max_duty || s[1] < self.decision_secs) {
                return Err("step test duty outside the floor and ceiling or step shorter than the decision interval".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct Status {
    pub controls: Vec<PControl>,
    pub alarms: Vec<Alarm>,
    pub limits: Vec<Limit>,
    pub model: ModelInfo,
    pub predicted: Option<Predicted>,
    pub step: Option<StepStatus>,
    pub weight: f64,
}

struct Chan {
    spec: Spec,
    cfg: ChannelConfig,
    cal: Calibration,
    mode: Mode,
    drive: Drive,
    target: Option<f64>,
    duty_cmd: Option<f64>,
    wrote_ms: i64,
    fallback: Option<String>,
    note: Option<String>,
    release_due: bool,
    follow_since: Option<i64>,
    autostart: bool,
}

impl Chan {
    fn new(kind: ControlKind, o: &ChannelOverride) -> Chan {
        let cfg = ChannelConfig::resolve(kind, o).unwrap_or_else(|e| {
            log::error!("control: invalid {kind:?} channel config ({e}), defaults used");
            ChannelConfig::defaults(kind)
        });
        Chan { spec: Spec::of(kind), cal: cfg.calibration(), autostart: cfg.autostart_active, cfg, mode: Mode::Shadow, drive: Drive::Firmware, target: None, duty_cmd: None, wrote_ms: 0, fallback: None, note: None, release_due: false, follow_since: None }
    }
    fn rpm_channel(&self) -> String {
        format!("d5next/{}/rpm", self.spec.name)
    }
    fn duty_channel(&self) -> String {
        format!("d5next/{}/duty", self.spec.name)
    }
    fn rpm_range(&self) -> (f64, f64) {
        (self.cal.rpm(self.cfg.floor_duty), self.cal.rpm(self.cfg.max_duty))
    }
    fn driving(&self) -> bool {
        self.drive != Drive::Firmware
    }
}

struct StepTest {
    fresh: bool,
    chan: usize,
    steps: Vec<[f64; 2]>,
    idx: usize,
    since: i64,
}

pub struct Control {
    cfg: ControlConfig,
    writes: bool,
    chans: Vec<Chan>,
    act: Box<dyn Actuator>,
    model: Option<Box<dyn Model>>,
    model_error: Option<String>,
    last: HashMap<String, (i64, f64)>,
    start: i64,
    started: bool,
    weight: f64,
    plan: Option<(Plan, i64)>,
    last_decision: Option<i64>,
    last_write: i64,
    last_release: i64,
    test: Option<StepTest>,
    since: BTreeMap<(String, String), i64>,
    cpu_hot: Option<i64>,
    pump_low: Option<i64>,
    fan_stop: Option<i64>,
    alarms: Vec<Alarm>,
    wall_off: i64,
}

impl Control {
    pub fn new(cfg: &Config, act: Box<dyn Actuator>, model: Result<Box<dyn Model>, String>) -> Control {
        let cc = match cfg.control.validate() {
            Ok(()) => cfg.control.clone(),
            Err(e) => {
                log::error!("control: invalid configuration ({e}), defaults used");
                ControlConfig::default()
            }
        };
        let (model, model_error) = match model {
            Ok(m) => (Some(m), None),
            Err(e) => (None, Some(e)),
        };
        let chans = vec![Chan::new(ControlKind::Fan, &cc.fan), Chan::new(ControlKind::Pump, &cc.pump)];
        Control { weight: cc.weight, cfg: cc, writes: cfg.writes_enabled, chans, act, model, model_error, last: HashMap::new(), start: 0, started: false, plan: None, last_decision: None, last_write: i64::MIN / 2, last_release: i64::MIN / 2, test: None, since: BTreeMap::new(), cpu_hot: None, pump_low: None, fan_stop: None, alarms: vec![], wall_off: 0 }
    }

    fn ix(&self, id: &str) -> Result<usize, String> {
        self.chans.iter().position(|c| format!("d5.{}", c.spec.name) == id).ok_or_else(|| "unknown control".to_string())
    }
    fn stale_ms(&self) -> i64 {
        (self.cfg.stale_secs * 1000.0) as i64
    }
    fn fresh(&self, ch: &str, now: i64) -> Option<f64> {
        self.last.get(ch).filter(|(t, v)| now - t <= self.stale_ms() && v.is_finite()).map(|x| x.1)
    }
    fn age(&self, ch: &str, now: i64) -> i64 {
        now - self.last.get(ch).map_or(self.start, |x| x.0)
    }
    fn temps(&self, now: i64) -> Temps {
        Temps { water: self.fresh(&self.cfg.water_channel, now), cpu: self.fresh(&self.cfg.cpu_channel, now), gpu: self.fresh(&self.cfg.gpu_channel, now) }
    }
    fn watched(&self) -> Vec<String> {
        let mut w = vec![self.cfg.water_channel.clone(), self.cfg.cpu_channel.clone(), self.cfg.gpu_channel.clone()];
        for c in &self.chans {
            w.extend([c.rpm_channel(), c.duty_channel()]);
        }
        w
    }
    fn identified(&self, i: usize) -> bool {
        let (fan, pump) = self.model.as_ref().map_or((false, false), |m| m.identified());
        if self.chans[i].spec.kind == ControlKind::Fan { fan } else { pump }
    }
    fn warm_left(&self) -> i64 {
        self.model.as_ref().map_or(self.cfg.warmup_secs, |m| (self.cfg.warmup_secs - m.warm_secs()).max(0))
    }
    fn operating(&self, c: &Chan, now: i64) -> Option<Operating> {
        let ch = |s: &str| format!("d5next/{}/{s}", c.spec.name);
        Some(Operating { duty: self.fresh(&ch("duty"), now)?, rpm: self.fresh(&ch("rpm"), now)?, power: self.fresh(&ch("power"), now), current: self.fresh(&ch("current"), now) })
    }

    fn global_fault(&self, now: i64) -> Option<String> {
        if self.model.is_none() {
            return Some(format!("Modell nicht geladen: {}", self.model_error.clone().unwrap_or_default()));
        }
        self.sensor_fault(now)
    }

    fn sensor_fault(&self, now: i64) -> Option<String> {
        let t = self.temps(now);
        for (v, name) in [(t.water, "Wassertemperatur"), (t.cpu, "CPU-Temperatur"), (t.gpu, "GPU-Temperatur")] {
            if v.is_none() {
                return Some(format!("{name} veraltet (> {:.0} s)", self.cfg.stale_secs));
            }
        }
        if t.water.is_some_and(|w| w >= self.cfg.water_limit) {
            return Some(format!("Wasser {:.1} °C am Limit {:.0} °C", t.water.unwrap(), self.cfg.water_limit));
        }
        if self.cpu_hot.is_some_and(|s| now - s >= (self.cfg.cpu_hold_secs * 1000.0) as i64) {
            return Some(format!("CPU seit {:.0} s über {:.0} °C", self.cfg.cpu_hold_secs, self.cfg.cpu_limit));
        }
        None
    }

    fn startup(&mut self, now: i64) {
        self.start = now;
        if !self.writes {
            return;
        }
        match self.act.recover() {
            Ok(k) if !k.is_empty() => log::warn!("control: {k:?} was in duty mode at start, returned to the firmware PID"),
            Ok(_) => {}
            Err(e) => log::error!("control: start check of the D5 mode bytes failed: {e}"),
        }
    }

    fn absorb(&mut self, rows: &[Row], now: i64, epoch: i64) {
        let map: HashMap<String, f64> = rows.iter().map(|r| (r.channel.clone(), r.value)).collect();
        for w in self.watched() {
            if let Some(v) = map.get(&w).filter(|v| v.is_finite()) {
                self.last.insert(w, (now, *v));
            }
        }
        for c in &self.chans {
            for s in ["power", "current"] {
                let ch = format!("d5next/{}/{s}", c.spec.name);
                if let Some(v) = map.get(&ch).filter(|v| v.is_finite()) {
                    self.last.insert(ch, (now, *v));
                }
            }
        }
        if let Some(m) = self.model.as_mut() {
            m.observe(&map, epoch);
        }
    }

    fn raise(&mut self, out: &mut Vec<Alarm>, kind: AlarmKind, channel: &str, message: String, value: Option<f64>, limit: Option<f64>, now: i64) {
        let key = (format!("{kind:?}"), channel.to_string());
        let since = *self.since.entry(key).or_insert(now);
        out.push(Alarm { kind, channel: channel.into(), message, value, limit, since_ms: since + self.wall_off });
    }

    fn refresh_alarms(&mut self, now: i64) {
        let mut out = Vec::new();
        let t = self.temps(now);
        let cfg = self.cfg.clone();
        let hold = |s: Option<i64>, secs: f64| s.is_some_and(|s| now - s >= (secs * 1000.0) as i64);
        if let Some(w) = t.water.filter(|w| *w >= cfg.water_limit) {
            self.raise(&mut out, AlarmKind::WaterHigh, &cfg.water_channel, format!("Wasser {w:.1} °C über {:.0} °C", cfg.water_limit), Some(w), Some(cfg.water_limit), now);
        }
        self.cpu_hot = if t.cpu.is_some_and(|c| c >= cfg.cpu_limit) { self.cpu_hot.or(Some(now)) } else { None };
        if hold(self.cpu_hot, cfg.cpu_hold_secs) {
            self.raise(&mut out, AlarmKind::CpuHigh, &cfg.cpu_channel, format!("CPU seit {:.0} s über {:.0} °C", cfg.cpu_hold_secs, cfg.cpu_limit), t.cpu, Some(cfg.cpu_limit), now);
        }
        let pump = &self.chans[1];
        let (pch, plimit) = (pump.rpm_channel(), pump.cfg.alarm_rpm);
        let prpm = self.fresh(&pch, now);
        self.pump_low = if prpm.is_some_and(|r| r < plimit) { self.pump_low.or(Some(now)) } else { None };
        if hold(self.pump_low, cfg.pump_low_secs) {
            self.raise(&mut out, AlarmKind::PumpLow, &pch, format!("Pumpe unter {plimit:.0} U/min"), prpm, Some(plimit), now);
        }
        let (fch, fduty) = (self.chans[0].rpm_channel(), self.chans[0].duty_channel());
        let stopped = self.fresh(&fch, now).is_some_and(|r| r < 1.0) && self.fresh(&fduty, now).is_some_and(|d| d >= 5.0);
        self.fan_stop = if stopped { self.fan_stop.or(Some(now)) } else { None };
        if hold(self.fan_stop, cfg.fan_stop_secs) {
            self.raise(&mut out, AlarmKind::FanStopped, &fch, "Lüfter steht trotz Duty".into(), self.fresh(&fch, now), None, now);
        }
        for ch in self.watched() {
            let age = self.age(&ch, now);
            if age > self.stale_ms() {
                self.raise(&mut out, AlarmKind::Stale, &ch, format!("{ch} seit {} s ohne Wert", age / 1000), None, None, now);
            }
        }
        let falls: Vec<(String, String)> = self.chans.iter().filter_map(|c| c.fallback.clone().map(|f| (c.rpm_channel(), f))).collect();
        for (ch, f) in falls {
            self.raise(&mut out, AlarmKind::Fallback, &ch, format!("Rückfall auf Firmware-PID: {f}"), None, None, now);
        }
        let active: Vec<(String, String)> = out.iter().map(|a| (format!("{:?}", a.kind), a.channel.clone())).collect();
        self.since.retain(|k, _| active.contains(k));
        self.alarms = out;
    }

    fn fall_back(&mut self, i: usize, why: &str) {
        if self.chans[i].mode == Mode::Active || self.chans[i].driving() {
            self.trip(i, why);
        }
    }

    fn trip(&mut self, i: usize, why: &str) {
        let c = &mut self.chans[i];
        log::error!("control: {} falls back to the firmware PID: {why}", c.spec.name);
        c.fallback = Some(why.to_string());
        c.mode = Mode::Shadow;
        c.release_due = true;
        c.target = None;
        c.follow_since = None;
        if self.test.as_ref().is_some_and(|t| t.chan == i) {
            self.test = None;
        }
    }

    fn check_faults(&mut self, now: i64) {
        for i in 0..self.chans.len() {
            if self.chans[i].mode != Mode::Active && !self.chans[i].driving() {
                continue;
            }
            let global = if self.chans[i].mode == Mode::Active { self.global_fault(now) } else { self.sensor_fault(now) };
            if let Some(g) = &global {
                self.fall_back(i, g);
                continue;
            }
            let c = &self.chans[i];
            let Some(cmd) = c.duty_cmd.filter(|_| now - c.wrote_ms >= (self.cfg.follow_settle_secs * 1000.0) as i64) else { continue };
            let (lo, hi) = c.rpm_range();
            let rpm = self.fresh(&c.rpm_channel(), now);
            let off = rpm.is_none_or(|r| (r - c.cal.rpm(cmd)).abs() > c.cfg.rpm_tolerance * (hi - lo).max(1.0));
            let since = if off { c.follow_since.or(Some(now)) } else { None };
            self.chans[i].follow_since = since;
            if since.is_some_and(|s| now - s >= (self.cfg.follow_secs * 1000.0) as i64) {
                self.fall_back(i, &format!("Drehzahl folgt dem Befehl nicht (Soll {:.0}, Ist {})", self.chans[i].cal.rpm(cmd), rpm.map_or("fehlt".into(), |r| format!("{r:.0}"))));
            }
        }
    }

    fn write(&mut self, set: &[(ControlKind, Setting)], now: i64, drive: Drive) -> bool {
        match self.act.apply(set) {
            Ok(()) => {
                self.last_write = now;
                for (k, s) in set {
                    let c = self.chans.iter_mut().find(|c| c.spec.kind == *k).unwrap();
                    if let Setting::Duty(d) = s {
                        c.duty_cmd = Some(d.clamp(c.cfg.floor_duty, c.cfg.max_duty));
                        c.drive = drive;
                        c.wrote_ms = now;
                    }
                }
                true
            }
            Err(e) => {
                for (k, _) in set {
                    let i = self.chans.iter().position(|c| c.spec.kind == *k).unwrap();
                    self.trip(i, &format!("Schreibfehler: {e}"));
                }
                false
            }
        }
    }

    fn try_release(&mut self, now: i64, force: bool) {
        let due: Vec<usize> = (0..self.chans.len()).filter(|i| self.chans[*i].release_due).collect();
        if due.is_empty() || (!force && now - self.last_release < 2000) {
            return;
        }
        self.last_release = now;
        let set: Vec<(ControlKind, Setting)> = due.iter().map(|i| (self.chans[*i].spec.kind, Setting::Firmware)).collect();
        let mut r = self.act.apply(&set);
        if r.is_err() && force {
            r = self.act.apply(&set);
        }
        for i in due {
            let c = &mut self.chans[i];
            match &r {
                Ok(()) => {
                    c.release_due = false;
                    c.drive = Drive::Firmware;
                    c.duty_cmd = None;
                    c.note = None;
                    log::info!("control: {} is back on the firmware PID", c.spec.name);
                }
                Err(e) => {
                    c.note = Some(format!("Rückfall fehlgeschlagen, neuer Versuch: {e}"));
                    log::error!("control: release of {} failed: {e}", c.spec.name);
                }
            }
        }
    }

    fn step_duty(&self, t: &StepTest) -> f64 {
        let c = &self.chans[t.chan];
        t.steps[t.idx][0].clamp(c.cfg.floor_duty, c.cfg.max_duty)
    }

    fn progress_test(&mut self, now: i64) {
        let Some(t) = self.test.as_mut() else { return };
        if t.fresh {
            t.fresh = false;
        } else if now - t.since >= (t.steps[t.idx][1] * 1000.0) as i64 {
            t.idx += 1;
        } else {
            return;
        }
        t.since = now;
        let i = t.chan;
        if t.idx >= t.steps.len() {
            self.test = None;
            self.chans[i].release_due = true;
            return;
        }
        let duty = self.step_duty(self.test.as_ref().unwrap());
        let kind = self.chans[i].spec.kind;
        self.write(&[(kind, Setting::Duty(duty))], now, Drive::Test);
    }

    fn decide(&mut self, now: i64) {
        let interval = (self.cfg.decision_secs * 1000.0) as i64;
        let due = self.last_decision.is_none_or(|t| now - t >= interval) && now - self.last_write >= interval;
        if !due || self.chans.iter().all(|c| c.mode == Mode::Off) {
            return;
        }
        self.last_decision = Some(now);
        self.plan = None;
        let Some(model) = self.model.as_ref() else {
            for c in &mut self.chans {
                c.note = Some("Modell nicht geladen".into());
            }
            return;
        };
        let ops = (self.operating(&self.chans[0], now), self.operating(&self.chans[1], now));
        let (Some(fb), Some(pb)) = ops else {
            for c in &mut self.chans {
                c.note = Some("Messwerte von Lüfter oder Pumpe veraltet".into());
            }
            return;
        };
        let (fan_c, pump_c) = (&self.chans[0], &self.chans[1]);
        let fan = Axis { name: "fan", cfg: &fan_c.cfg, cal: &fan_c.cal, free: fan_c.mode != Mode::Off, base: fb };
        let pump = Axis { name: "pump", cfg: &pump_c.cfg, cal: &pump_c.cal, free: pump_c.mode != Mode::Off, base: pb };
        let result = Planner { cfg: &self.cfg }.plan(model.as_ref(), &fan, &pump, &self.temps(now), self.weight);
        let warm = self.warm_left() == 0;
        let plan = match result {
            Ok(p) => p,
            Err(e) => {
                self.model_error = Some(e.clone());
                for i in 0..self.chans.len() {
                    if self.chans[i].mode == Mode::Active {
                        self.fall_back(i, &format!("Modellfehler: {e}"));
                    }
                }
                return;
            }
        };
        self.model_error = None;
        self.plan = Some((plan, now));
        let mut set = Vec::new();
        for i in 0..self.chans.len() {
            let c = &mut self.chans[i];
            if c.mode == Mode::Off {
                c.target = None;
                continue;
            }
            let base_op = if i == 0 { fb } else { pb };
            let (lo, hi) = c.rpm_range();
            let want = if i == 0 { plan.fan_rpm } else { plan.pump_rpm };
            let from = if c.mode == Mode::Active { c.target.unwrap_or(base_op.rpm) } else { base_op.rpm };
            let new = if want > from { want.min(from + c.cfg.step_up_rpm) } else { want.max(from - c.cfg.step_down_rpm) }.clamp(lo, hi);
            c.target = Some(new);
            if c.mode != Mode::Active {
                c.note = Some(if warm { "nur berechnet".into() } else { "nur berechnet, Modell noch nicht warm".into() });
                continue;
            }
            if !warm {
                c.note = Some("Modell wärmt auf".into());
                continue;
            }
            c.note = None;
            let duty = c.cal.duty(new).clamp(c.cfg.floor_duty, c.cfg.max_duty);
            if c.duty_cmd.is_none_or(|d| (d - duty).abs() >= c.cfg.deadband_duty) || c.drive == Drive::Firmware {
                set.push((c.spec.kind, Setting::Duty(duty)));
            }
        }
        if !set.is_empty() {
            self.write(&set, now, Drive::Duty);
        }
    }

    pub fn set_mode(&mut self, id: &str, mode: Mode, confirmed: bool, now: i64) -> Result<(), String> {
        let i = self.ix(id)?;
        if self.chans[i].mode == mode && self.chans[i].fallback.is_none() {
            return Ok(());
        }
        if mode == Mode::Active {
            if !self.writes {
                return Err("Schreiben ist deaktiviert (writes_enabled in der Admin-Konfiguration)".into());
            }
            if self.chans[i].spec.kind == ControlKind::Pump && !confirmed {
                return Err("Pumpe: zweite Bestätigung fehlt".into());
            }
            if !self.identified(i) {
                return Err(match &self.model_error {
                    Some(e) if self.model.is_none() => format!("Modell nicht geladen: {e}"),
                    _ => "Modell: Antwort von Lüfter und Pumpe ist nicht identifiziert, Aktivieren verweigert".into(),
                });
            }
            if self.test.as_ref().is_some_and(|t| t.chan == i) {
                return Err("Stufentest läuft auf diesem Kanal".into());
            }
            if self.warm_left() > 0 {
                return Err(format!("Modell wärmt auf, noch {} min", (self.warm_left() + 59) / 60));
            }
            if let Some(f) = self.global_fault(now) {
                return Err(f);
            }
        }
        let c = &mut self.chans[i];
        c.mode = mode;
        c.fallback = None;
        c.note = if mode == Mode::Active { Some("wartet auf die nächste Entscheidung".into()) } else { None };
        c.target = None;
        c.follow_since = None;
        if mode != Mode::Active && c.driving() && self.test.as_ref().is_none_or(|t| t.chan != i) {
            c.release_due = true;
            self.try_release(now, true);
            if self.chans[i].release_due {
                return Err("Rückfall auf Firmware-PID fehlgeschlagen, wird wiederholt".into());
            }
        }
        log::info!("control: {id} mode {mode:?}");
        Ok(())
    }

    pub fn set_weight(&mut self, weight: f64) -> Result<(), String> {
        if !weight.is_finite() || !(0.0..=1.0).contains(&weight) {
            return Err("weight must be within 0..1".into());
        }
        self.weight = weight;
        Ok(())
    }

    pub fn start_test(&mut self, id: &str, confirmed: bool, now: i64) -> Result<(), String> {
        let i = self.ix(id)?;
        if !self.writes {
            return Err("Schreiben ist deaktiviert (writes_enabled in der Admin-Konfiguration)".into());
        }
        if !confirmed {
            return Err("Stufentest: Bestätigung fehlt".into());
        }
        if self.test.is_some() || self.chans[i].mode == Mode::Active || self.chans[i].driving() {
            return Err("Stufentest nicht möglich: Kanal aktiv oder Test läuft schon".into());
        }
        if now - self.last_write < (self.cfg.decision_secs * 1000.0) as i64 {
            return Err("Schreibpause: letzter Schreibvorgang liegt weniger als ein Entscheidungsintervall zurück".into());
        }
        if let Some(f) = self.sensor_fault(now) {
            return Err(f);
        }
        let steps = if self.chans[i].spec.kind == ControlKind::Fan { self.cfg.step_test.fan.clone() } else { self.cfg.step_test.pump.clone() };
        if steps.is_empty() {
            return Err("Kein Stufenplan konfiguriert".into());
        }
        self.chans[i].fallback = None;
        self.test = Some(StepTest { fresh: true, chan: i, steps, idx: 0, since: now });
        log::info!("control: step test started on {id}");
        Ok(())
    }

    pub fn stop_test(&mut self, now: i64) -> Result<(), String> {
        let Some(t) = self.test.take() else { return Ok(()) };
        self.chans[t.chan].release_due = true;
        self.try_release(now, true);
        if self.chans[t.chan].release_due { Err("Rückfall auf Firmware-PID fehlgeschlagen, wird wiederholt".into()) } else { Ok(()) }
    }

    pub fn shutdown(&mut self, now: i64) {
        self.test = None;
        for c in &mut self.chans {
            c.mode = Mode::Off;
            c.release_due |= c.driving();
        }
        self.try_release(now, true);
    }

    fn autostart(&mut self, now: i64) {
        for i in 0..self.chans.len() {
            if !self.chans[i].autostart || !self.writes {
                continue;
            }
            match self.set_mode(&format!("d5.{}", self.chans[i].spec.name), Mode::Active, true, now) {
                Ok(()) => self.chans[i].autostart = false,
                Err(e) if now - self.start > 300_000 => {
                    log::error!("control: autostart of {} gave up: {e}", self.chans[i].spec.name);
                    self.chans[i].autostart = false;
                }
                Err(_) => {}
            }
        }
    }

    pub fn step(&mut self, rows: &mut Vec<Row>, now: i64, epoch: i64) {
        if !self.started {
            self.started = true;
            self.startup(now);
        }
        self.wall_off = epoch * 1000 - now;
        self.absorb(rows, now, epoch);
        self.refresh_alarms(now);
        self.check_faults(now);
        self.progress_test(now);
        self.decide(now);
        self.try_release(now, false);
        self.autostart(now);
        rows.extend(self.telemetry());
    }

    fn telemetry(&self) -> Vec<Row> {
        let row = |ch: String, text: String, unit: &str, v: f64, role: &'static str| Row::new("daisyloop", format!("daisyloop/{ch}"), text, "other", unit, v).role(role);
        let mut rows = Vec::new();
        for c in &self.chans {
            let n = c.spec.name;
            rows.push(row(format!("{n}/mode"), format!("{n} mode (0 off, 1 shadow, 2 active)"), "", c.mode as u8 as f64, "input"));
            rows.push(row(format!("{n}/drive"), format!("{n} drive (0 firmware, 1 duty, 2 test)"), "", c.drive as u8 as f64, "input"));
            rows.push(row(format!("{n}/fallback"), format!("{n} fallback active"), "", c.fallback.is_some() as u8 as f64, "aux"));
            if let Some(t) = c.target {
                rows.push(row(format!("{n}/target_rpm"), format!("{n} target rpm"), "rpm", t, "aux"));
            }
            if let Some(d) = c.duty_cmd {
                rows.push(row(format!("{n}/duty_cmd"), format!("{n} commanded duty"), "%", d, "input"));
            }
        }
        if let Some((p, _)) = &self.plan {
            for (ch, v) in [("water", p.water), ("cpu", p.cpu), ("gpu", p.gpu)] {
                if let Some(v) = v {
                    rows.push(row(format!("plan/{ch}"), format!("predicted {ch} temperature"), "degC", v, "aux"));
                }
            }
            rows.push(row("plan/feasible".into(), "plan within limits".into(), "", p.feasible as u8 as f64, "aux"));
        }
        if let Some(t) = &self.test {
            rows.push(row("step/phase".into(), "step test phase".into(), "", (t.idx + 1) as f64, "input"));
            rows.push(row("step/duty".into(), "step test duty".into(), "%", self.step_duty(t), "input"));
        }
        rows
    }

    pub fn status(&self, now: i64) -> Status {
        let controls = self
            .chans
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let (lo, hi) = c.rpm_range();
                let note = c.note.clone().or_else(|| (!self.writes).then(|| "Schreiben deaktiviert".to_string()));
                PControl {
                    id: format!("d5.{}", c.spec.name),
                    label: if c.spec.kind == ControlKind::Pump { "Pumpe (D5 NEXT)".into() } else { "Radiatorlüfter (D5 NEXT)".into() },
                    kind: c.spec.kind,
                    rpm_channel: c.rpm_channel(),
                    duty_channel: c.duty_channel(),
                    mode: c.mode,
                    allowed: self.writes,
                    identified: self.identified(i),
                    drive: c.drive,
                    target_rpm: c.target,
                    actual_rpm: self.fresh(&c.rpm_channel(), now),
                    duty: self.fresh(&c.duty_channel(), now),
                    commanded_duty: c.duty_cmd,
                    floor_duty: c.cfg.floor_duty,
                    min_rpm: lo,
                    max_rpm: hi,
                    fallback: c.fallback.clone(),
                    note,
                }
            })
            .collect();
        let left = self.warm_left();
        let model = ModelInfo { loaded: self.model.is_some(), warm: self.model.is_some() && left == 0, warmup_left_s: left as u32, fan_identified: self.identified(0), pump_identified: self.identified(1), error: self.model_error.clone() };
        let predicted = self.plan.as_ref().map(|(p, at)| Predicted { horizon_s: self.cfg.horizon_secs, water: p.water, cpu: p.cpu, gpu: p.gpu, water_limit: self.cfg.water_plan_limit, cpu_limit: self.cfg.cpu_plan_limit, gpu_limit: self.cfg.gpu_plan_limit, feasible: p.feasible, age_ms: now - at });
        let step = self.test.as_ref().map(|t| StepStatus { id: format!("d5.{}", self.chans[t.chan].spec.name), phase: t.idx as u32 + 1, phases: t.steps.len() as u32, duty: self.step_duty(t), left_s: ((t.steps[t.idx][1] * 1000.0 - (now - t.since) as f64) / 1000.0).max(0.0) as u32 });
        let limits = vec![
            Limit { channel: self.cfg.water_channel.clone(), low: None, high: Some(self.cfg.water_limit) },
            Limit { channel: self.cfg.cpu_channel.clone(), low: None, high: Some(self.cfg.cpu_limit) },
            Limit { channel: self.chans[1].rpm_channel(), low: Some(self.chans[1].cfg.alarm_rpm), high: None },
        ];
        Status { controls, alarms: self.alarms.clone(), limits, model, predicted, step, weight: self.weight }
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
