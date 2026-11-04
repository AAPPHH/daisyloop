use daisyloop_ui::ipc::Framed;
use daisyloop_ui::protocol::*;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::os::windows::io::FromRawHandle;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};

const SOURCE: &str = include_str!("../../mock/lhm_data.json");
const D5: &str = "/usbhid/0C70/F00E/00000-00000";
const CPU_TEMP: &str = "/intelcpu/0/temperature/26";
const WATER: &str = "/usbhid/0C70/F00E/00000-00000/temperature/0";

#[derive(Clone, Copy)]
enum Role {
    Plain,
    Duty(usize),
    Rpm(usize, f64),
}

struct Sensor {
    channel: Channel,
    base: f64,
    seed: f64,
    rel: f64,
    role: Role,
}

struct Sim {
    sensors: Vec<Sensor>,
    index: HashMap<String, usize>,
    controls: Vec<Control>,
    initial: Vec<Control>,
    limits: Vec<Limit>,
    active: HashMap<(String, Side), i64>,
    extremes: HashMap<String, (f64, f64)>,
    seq: u64,
    live: Vec<f64>,
    prev_error: Vec<f64>,
    water_offset: f64,
    last_tick: i64,
    alarms: Vec<Alarm>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

fn parse_number(s: &str) -> Option<f64> {
    s.trim().replace(',', ".").parse().ok()
}

fn split_value(s: &str) -> (Option<f64>, String) {
    match s.rsplit_once(' ') {
        Some((n, u)) if parse_number(n).is_some() => (parse_number(n), u.to_string()),
        _ => (parse_number(s), String::new()),
    }
}

fn kind_of(t: &str) -> Kind {
    match t {
        "Temperature" => Kind::Temperature,
        "Fan" => Kind::Fan,
        "Control" => Kind::Duty,
        "Power" => Kind::Power,
        "Voltage" => Kind::Voltage,
        "Current" => Kind::Current,
        "Load" => Kind::Load,
        "Clock" => Kind::Clock,
        "Flow" => Kind::Flow,
        "Data" | "SmallData" | "Throughput" => Kind::Data,
        _ => Kind::Other,
    }
}

fn clean_name(s: &str) -> String {
    let cut: String = s.chars().take_while(|c| !c.is_control()).collect();
    cut.trim().to_string()
}

fn hash01(seed: f64, k: i64) -> f64 {
    let mut x = (seed * 1000.0) as u64 ^ (k as u64).wrapping_mul(0x9E3779B97F4A7C15);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51AFD7ED558CCD);
    x ^= x >> 33;
    (x % 1_000_000) as f64 / 1_000_000.0
}

fn wave(t_s: f64, seed: f64) -> f64 {
    let tau = std::f64::consts::TAU;
    (0.5 + 0.30 * (tau * t_s / 5400.0 + seed).sin() + 0.22 * (tau * t_s / 640.0 + seed * 2.3).sin() + 0.14 * (tau * t_s / 71.0 + seed * 3.1).sin()).clamp(0.0, 1.0)
}

fn d5_label(s: &str) -> String {
    match s {
        "Water Temperature" => "Wassertemperatur",
        "Pump" => "Pumpe Drehzahl",
        "Fan" => "Lüfter Drehzahl",
        "Viritual Flow" => "Durchfluss (virtuell)",
        "Fan Control" => "Lüfter Duty",
        "Pump Control" => "Pumpe Duty",
        "Fan Power" => "Lüfter Leistung",
        "Pump Power" => "Pumpe Leistung",
        "Fan Voltage" => "Lüfter Spannung",
        "Pump Voltage" => "Pumpe Spannung",
        other => other,
    }
    .to_string()
}

fn walk(node: &Value, hardware: &str, out: &mut Vec<Sensor>) {
    let hardware = match (node.get("HardwareId"), node["Text"].as_str()) {
        (Some(_), Some(t)) => clean_name(t),
        _ => hardware.to_string(),
    };
    if let Some(id) = node["SensorId"].as_str() {
        let (value, unit) = split_value(node["Value"].as_str().unwrap_or(""));
        let (min, _) = split_value(node["Min"].as_str().unwrap_or(""));
        let (max, _) = split_value(node["Max"].as_str().unwrap_or(""));
        let kind = kind_of(node["Type"].as_str().unwrap_or(""));
        let is_d5 = id.starts_with(D5);
        let rel = match kind {
            Kind::Temperature => 0.03,
            Kind::Power => 0.12,
            Kind::Load => 0.35,
            Kind::Clock => 0.06,
            Kind::Voltage => 0.004,
            Kind::Fan => 0.02,
            Kind::Duty => 0.0,
            _ => 0.01,
        };
        let seed = id.bytes().fold(7u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64)) % 1000;
        let text = node["Text"].as_str().unwrap_or(id);
        out.push(Sensor {
            channel: Channel {
                id: id.to_string(),
                label: if is_d5 { d5_label(text) } else { text.to_string() },
                hardware: if is_d5 { "Aquacomputer D5 NEXT".into() } else { hardware.clone() },
                kind,
                unit,
                value,
                min,
                max,
            },
            base: value.unwrap_or(0.0),
            seed: seed as f64 / 100.0,
            rel,
            role: Role::Plain,
        });
    }
    for child in node["Children"].as_array().into_iter().flatten() {
        walk(child, &hardware, out);
    }
}

impl Sim {
    fn new() -> Self {
        let root: Value = serde_json::from_str(SOURCE).unwrap();
        let mut sensors = Vec::new();
        walk(&root, "", &mut sensors);
        let index: HashMap<String, usize> = sensors.iter().enumerate().map(|(i, s)| (s.channel.id.clone(), i)).collect();
        let mut sim = Self { sensors, index, controls: vec![], initial: vec![], limits: vec![], active: HashMap::new(), extremes: HashMap::new(), seq: 0, live: vec![], prev_error: vec![], water_offset: 0.0, last_tick: now_ms() - 1000, alarms: vec![] };
        sim.build_controls();
        sim.build_limits();
        sim.tick();
        sim
    }

    fn add_control(&mut self, id: &str, label: &str, kind: ControlKind, rpm: &str, duty: &str, floor: f64, source: &str, setpoint: f64, gains: (f64, f64, f64)) {
        let ci = self.controls.len();
        let base_duty = self.sensors[self.index[duty]].base;
        let base_rpm = self.sensors[self.index[rpm]].base;
        let max_rpm = if base_duty > 0.0 { base_rpm / base_duty * 100.0 } else { 0.0 };
        let (ri, di) = (self.index[rpm], self.index[duty]);
        self.sensors[ri].role = Role::Rpm(ci, max_rpm);
        self.sensors[di].role = Role::Duty(ci);
        let start = 40.0f64.max(floor);
        self.controls.push(Control {
            id: id.into(),
            label: label.into(),
            kind,
            rpm_channel: rpm.into(),
            duty_channel: duty.into(),
            mode: Mode::Pid,
            floor,
            pid: PidParams { source: source.into(), setpoint, kp: gains.0, ki: gains.1, kd: gains.2, min_duty: floor, max_duty: 100.0 },
            state: PidState { output: start, i: start, ..Default::default() },
        });
        self.prev_error.push(0.0);
    }

    fn build_controls(&mut self) {
        let d5 = |s: &str| format!("{D5}/{s}");
        self.add_control("d5.pump", "Pumpe", ControlKind::Pump, &d5("fan/0"), &d5("control/1"), 30.0, WATER, 35.0, (8.0, 0.15, 0.0));
        self.add_control("d5.fan", "Radiatorlüfter (D5 NEXT)", ControlKind::Fan, &d5("fan/1"), &d5("control/0"), 15.0, WATER, 35.0, (12.0, 0.25, 20.0));
        for (i, label) in [(0, "CPU Fan"), (1, "Pump Fan"), (2, "Chipset Fan"), (3, "EZ-Connect Fan"), (10, "System Fan #1"), (11, "System Fan #2"), (12, "System Fan #3"), (13, "System Fan #4"), (14, "System Fan #5"), (15, "System Fan #6")] {
            let b = |s: &str| format!("/lpc/nct6687dr/0/{s}/{i}");
            self.add_control(&format!("board.{i}"), label, ControlKind::Fan, &b("fan"), &b("control"), 20.0, CPU_TEMP, 70.0, (3.0, 0.05, 0.0));
        }
        self.initial = self.controls.clone();
    }

    fn build_limits(&mut self) {
        let mut add = |c: &str, low: Option<f64>, high: Option<f64>| self.limits.push(Limit { channel: c.into(), low, high });
        add(WATER, None, Some(40.0));
        add(CPU_TEMP, None, Some(90.0));
        add("/gpu-nvidia/0/temperature/0", None, Some(83.0));
        add(&format!("{D5}/fan/0"), Some(1500.0), None);
        add("/lpc/nct6687dr/0/temperature/2", None, Some(95.0));
        add("/nvme/1/temperature/0", None, Some(80.0));
    }

    fn analytic(&self, i: usize, t_ms: i64, cfg: &[Control]) -> f64 {
        let s = &self.sensors[i];
        let t = t_ms as f64 / 1000.0;
        match s.role {
            Role::Duty(ci) => self.historic_duty(&cfg[ci], t_ms, cfg),
            Role::Rpm(ci, max) => {
                let d = self.historic_duty(&cfg[ci], t_ms, cfg);
                max * d / 100.0 * (1.0 + 0.01 * (hash01(s.seed, t_ms / 1000) - 0.5))
            }
            Role::Plain => {
                let id = s.channel.id.as_str();
                let load = wave(t, 1.0);
                let gpu = wave(t, 4.0);
                let noise = hash01(s.seed, t_ms / 1000) - 0.5;
                let v = match id {
                    CPU_TEMP => 42.0 + 55.0 * load + noise * 2.0,
                    "/intelcpu/0/power/0" => 28.0 + 235.0 * load + noise * 6.0,
                    "/intelcpu/0/power/1" => 22.0 + 215.0 * load + noise * 6.0,
                    "/gpu-nvidia/0/temperature/0" => 34.0 + 42.0 * gpu + noise,
                    "/gpu-nvidia/0/power/0" => 22.0 + 270.0 * gpu + noise * 5.0,
                    WATER => 29.5 + 5.0 * (0.5 + 0.5 * (std::f64::consts::TAU * t / 86400.0).sin()) + 4.0 * wave(t, 2.0) + noise * 0.15,
                    _ => {
                        let w = (std::f64::consts::TAU * t / (97.0 + s.seed * 9.0) + s.seed).sin();
                        let thermal = if s.channel.kind == Kind::Temperature { 0.7 + 0.3 * load + 0.05 * gpu } else { 1.0 };
                        s.base * thermal * (1.0 + s.rel * (0.6 * w + noise * 0.8))
                    }
                };
                match s.channel.kind {
                    Kind::Load => v.clamp(0.0, 100.0),
                    _ => v.max(0.0),
                }
            }
        }
    }

    fn historic_duty(&self, c: &Control, t_ms: i64, cfg: &[Control]) -> f64 {
        let temp = self.analytic(self.index[&c.pid.source], t_ms, cfg);
        (c.state.i + c.pid.kp * (temp - c.pid.setpoint) - 2.0 * c.pid.kp).clamp(c.pid.min_duty, c.pid.max_duty)
    }

    fn tick(&mut self) {
        let t = now_ms();
        let dt = ((t - self.last_tick) as f64 / 1000.0).clamp(0.05, 5.0);
        self.last_tick = t;
        let cfg = self.initial.clone();
        let mut live: Vec<f64> = (0..self.sensors.len()).map(|i| self.analytic(i, t, &cfg)).collect();
        let wi = self.index[WATER];
        live[wi] += self.water_offset;
        for ci in 0..self.controls.len() {
            let c = &mut self.controls[ci];
            if c.mode == Mode::Pid {
                let temp = live[self.index[&c.pid.source]];
                let err = temp - c.pid.setpoint;
                let p = c.pid.kp * err;
                let d = c.pid.kd * (err - self.prev_error[ci]) / dt;
                let i = (c.state.i + c.pid.ki * err * dt).clamp(c.pid.min_duty, c.pid.max_duty);
                let raw = p + i + d;
                let out = raw.clamp(c.pid.min_duty.max(c.floor), c.pid.max_duty);
                let saturated = out != raw;
                c.state = PidState { output: out, error: err, p, i: if saturated && (raw - out) * err > 0.0 { c.state.i } else { i }, d, saturated };
                self.prev_error[ci] = err;
            }
            let duty = c.state.output * (1.0 + 0.002 * (hash01(ci as f64, t / 1000) - 0.5));
            live[self.index[&c.duty_channel]] = duty;
            if let Role::Rpm(_, max) = self.sensors[self.index[&c.rpm_channel]].role {
                live[self.index[&c.rpm_channel]] = max * duty / 100.0 * (1.0 + 0.01 * (hash01(ci as f64 + 0.5, t / 1000) - 0.5));
            }
        }
        let fan = live[self.index[&self.controls[1].duty_channel]];
        let target = -6.0 * (fan - 50.0) / 50.0;
        self.water_offset += (target - self.water_offset) * (dt / 90.0).min(1.0);
        self.live = live;
        self.seq += 1;
        self.alarms.clear();
        for l in &self.limits {
            let v = self.live[self.index[&l.channel]];
            for (side, limit, hit) in [(Side::Low, l.low, l.low.map(|x| v < x)), (Side::High, l.high, l.high.map(|x| v > x))] {
                let key = (l.channel.clone(), side);
                if hit == Some(true) {
                    let since = *self.active.entry(key).or_insert(t);
                    self.alarms.push(Alarm { channel: l.channel.clone(), side, limit: limit.unwrap(), value: v, since_ms: since });
                } else {
                    self.active.remove(&key);
                }
            }
        }
        for (i, s) in self.sensors.iter().enumerate() {
            let v = self.live[i];
            let e = self.extremes.entry(s.channel.id.clone()).or_insert((s.channel.min.unwrap_or(v).min(v), s.channel.max.unwrap_or(v).max(v)));
            e.0 = e.0.min(v);
            e.1 = e.1.max(v);
        }
    }

    fn snapshot(&self) -> Snapshot {
        let channels = self
            .sensors
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let e = self.extremes[&s.channel.id];
                let mut c = s.channel.clone();
                c.value = Some(self.live[i]);
                c.min = Some(e.0);
                c.max = Some(e.1);
                c
            })
            .collect();
        Snapshot { ts_ms: self.last_tick, seq: self.seq, channels, controls: self.controls.clone(), limits: self.limits.clone(), alarms: self.alarms.clone() }
    }

    fn apply(&mut self, cmd: Command) -> Result<(), String> {
        match cmd {
            Command::SetMode { id, mode } => {
                let c = self.controls.iter_mut().find(|c| c.id == id).ok_or("unbekannter Kanal")?;
                c.mode = mode;
            }
            Command::SetPid { id, pid } => {
                if !self.index.contains_key(&pid.source) {
                    return Err("unbekannte Quelle".into());
                }
                let c = self.controls.iter_mut().find(|c| c.id == id).ok_or("unbekannter Kanal")?;
                let min = pid.min_duty.max(c.floor);
                c.pid = PidParams { min_duty: min, max_duty: pid.max_duty.clamp(min, 100.0), ..pid };
                c.state.i = c.state.i.clamp(c.pid.min_duty, c.pid.max_duty);
            }
            Command::SetLimit { channel, low, high } => {
                if !self.index.contains_key(&channel) {
                    return Err("unbekannter Kanal".into());
                }
                self.limits.retain(|l| l.channel != channel);
                if low.is_some() || high.is_some() {
                    self.limits.push(Limit { channel, low, high });
                }
            }
        }
        Ok(())
    }

    fn history(&self, q: &HistoryQuery) -> HistoryResponse {
        let to = q.to_ms.min(now_ms());
        let from = q.from_ms.max(to - 8 * 86_400_000);
        let n = q.max_points.clamp(2, 4000) as i64;
        let step = ((to - from) / n).max(1000);
        let series = q
            .channels
            .iter()
            .filter_map(|id| {
                let i = *self.index.get(id)?;
                let (mut ts_ms, mut values) = (Vec::new(), Vec::new());
                let mut t = from - from.rem_euclid(1000);
                while t <= to {
                    ts_ms.push(t);
                    values.push(self.analytic(i, t, &self.initial));
                    t += step;
                }
                Some(Series { channel: id.clone(), ts_ms, values })
            })
            .collect();
        HistoryResponse { series }
    }
}

fn serve(file: File, sim: Arc<Mutex<Sim>>) {
    let Ok(mut io) = Framed::new(file) else { return };
    while let Ok(req) = io.recv::<Request>() {
        let body = if req.v != VERSION {
            ResponseBody::Error { message: format!("Protokoll v{} nicht unterstützt", req.v) }
        } else {
            let mut s = sim.lock().unwrap();
            match req.body {
                RequestBody::Snapshot => ResponseBody::Snapshot(s.snapshot()),
                RequestBody::History(q) => ResponseBody::History(s.history(&q)),
                RequestBody::Command(c) => match s.apply(c) {
                    Ok(()) => ResponseBody::Ack,
                    Err(message) => ResponseBody::Error { message },
                },
            }
        };
        if io.send(&Response::new(req.id, body)).is_err() {
            break;
        }
    }
}

fn main() {
    let sim = Arc::new(Mutex::new(Sim::new()));
    let ticker = sim.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        ticker.lock().unwrap().tick();
    });
    let name: Vec<u16> = PIPE_NAME.encode_utf16().chain(Some(0)).collect();
    println!("mock listening on {PIPE_NAME}");
    loop {
        let handle = unsafe { CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, PIPE_UNLIMITED_INSTANCES, 1 << 20, 1 << 20, 0, std::ptr::null()) };
        if handle == INVALID_HANDLE_VALUE {
            eprintln!("CreateNamedPipe failed: {}", unsafe { GetLastError() });
            return;
        }
        let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if connected {
            let file = unsafe { File::from_raw_handle(handle as _) };
            let sim = sim.clone();
            std::thread::spawn(move || serve(file, sim));
        }
    }
}
