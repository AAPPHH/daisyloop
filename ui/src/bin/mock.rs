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
const WATER: &str = "d5next/water/temp";
const CPU_TEMP: &str = "/intelcpu/0/temperature/0";
const GPU_TEMP: &str = "nvml/0/temp";
const FAN_MAX_RPM: f64 = 1470.0;
const PUMP_RPM: (f64, f64) = (2000.0, 4800.0);

#[derive(Clone, Copy)]
enum Role {
    Plain,
    Duty(usize),
    Rpm(usize),
}

struct Sensor {
    channel: Channel,
    base: f64,
    seed: f64,
    rel: f64,
    role: Role,
}

struct Flags {
    writes: bool,
    identified: bool,
    fault: bool,
}

struct Sim {
    sensors: Vec<Sensor>,
    index: HashMap<String, usize>,
    controls: Vec<Control>,
    flags: Flags,
    weight: f64,
    step: Option<(usize, i64)>,
    extremes: HashMap<String, (f64, f64)>,
    seq: u64,
    live: Vec<f64>,
    water_offset: f64,
    last_tick: i64,
    predicted: Predicted,
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

fn d5_channel(suffix: &str) -> Option<(&'static str, &'static str)> {
    Some(match suffix {
        "temperature/0" => (WATER, "Wassertemperatur"),
        "fan/0" => ("d5next/pump/rpm", "Pumpe Drehzahl"),
        "fan/1" => ("d5next/fan/rpm", "Lüfter Drehzahl"),
        "control/0" => ("d5next/fan/duty", "Lüfter Duty"),
        "control/1" => ("d5next/pump/duty", "Pumpe Duty"),
        "power/0" => ("d5next/fan/power", "Lüfter Leistung"),
        "power/1" => ("d5next/pump/power", "Pumpe Leistung"),
        "voltage/0" => ("d5next/fan/voltage", "Lüfter Spannung"),
        "voltage/1" => ("d5next/pump/voltage", "Pumpe Spannung"),
        "voltage/2" => ("d5next/5v/voltage", "+5V"),
        "voltage/3" => ("d5next/12v/voltage", "+12V"),
        _ => return None,
    })
}

fn group(id: &str, source: &str) -> String {
    let a = id.trim_start_matches('/').split('/').next().unwrap_or("");
    match (source, a) {
        ("d5", _) => "Aquacomputer D5 NEXT".into(),
        ("nvml", _) => "NVIDIA GPU (NVML)".into(),
        (_, "intelcpu") => "Intel CPU".into(),
        (_, "gpu-nvidia") => "NVIDIA GPU".into(),
        (_, "gpu-intel-integrated") => "Intel iGPU".into(),
        (_, "lpc") => "Mainboard".into(),
        (_, "memory") => "DIMM".into(),
        (_, "ram") | (_, "vram") => "Arbeitsspeicher".into(),
        (_, "nvme") | (_, "ssd") => "Datenträger".into(),
        (_, "nic") => "Netzwerk".into(),
        _ => "LibreHardwareMonitor".into(),
    }
}

fn walk(node: &Value, out: &mut Vec<Sensor>) {
    if let Some(id) = node["SensorId"].as_str() {
        let (value, unit) = split_value(node["Value"].as_str().unwrap_or(""));
        let (min, _) = split_value(node["Min"].as_str().unwrap_or(""));
        let (max, _) = split_value(node["Max"].as_str().unwrap_or(""));
        let kind = kind_of(node["Type"].as_str().unwrap_or(""));
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
        let seed = (id.bytes().fold(7u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64)) % 1000) as f64 / 100.0;
        let text = node["Text"].as_str().unwrap_or(id);
        let unit = match unit.as_str() {
            "RPM" => "RPM".to_string(),
            _ => unit,
        };
        let (cid, label, source) = match id.strip_prefix(D5).and_then(|s| d5_channel(s.trim_start_matches('/'))) {
            Some((c, l)) => (c.to_string(), l.to_string(), "d5"),
            None => (id.to_string(), clean_name(text), "lhm"),
        };
        if id.starts_with(D5) && source != "d5" {
            return;
        }
        let mut add = |cid: String, label: String, source: &str| {
            out.push(Sensor { channel: Channel { hardware: group(&cid, source), id: cid, label, kind, unit: unit.clone(), value, min, max, age_ms: 0, stale: false }, base: value.unwrap_or(0.0), seed, rel, role: Role::Plain });
        };
        if id == "/gpu-nvidia/0/temperature/0" {
            add(GPU_TEMP.into(), "GPU Temperatur".into(), "nvml");
        }
        if id == "/gpu-nvidia/0/power/0" {
            add("nvml/0/power".into(), "GPU Leistung".into(), "nvml");
        }
        add(cid, label, source);
    }
    for child in node["Children"].as_array().into_iter().flatten() {
        walk(child, out);
    }
}

impl Sim {
    fn new() -> Self {
        let root: Value = serde_json::from_str(SOURCE).unwrap();
        let mut sensors = Vec::new();
        walk(&root, &mut sensors);
        let index: HashMap<String, usize> = sensors.iter().enumerate().map(|(i, s)| (s.channel.id.clone(), i)).collect();
        let flag = |k: &str| std::env::var(k).is_ok_and(|v| v == "1");
        let flags = Flags { writes: flag("MOCK_WRITES"), identified: flag("MOCK_IDENTIFIED"), fault: flag("MOCK_FAULT") };
        let mut sim = Self { sensors, index, controls: vec![], flags, weight: 0.5, step: None, extremes: HashMap::new(), seq: 0, live: vec![], water_offset: 0.0, last_tick: now_ms() - 1000, predicted: Predicted::default() };
        sim.build_controls();
        sim.tick();
        sim
    }

    fn build_controls(&mut self) {
        for (i, (name, label, kind, floor, min_rpm, max_rpm)) in [("fan", "Radiatorlüfter (D5 NEXT)", ControlKind::Fan, 0.0, 0.0, FAN_MAX_RPM), ("pump", "Pumpe (D5 NEXT)", ControlKind::Pump, 15.0, 2420.0, PUMP_RPM.1)].into_iter().enumerate() {
            let (rpm, duty) = (format!("d5next/{name}/rpm"), format!("d5next/{name}/duty"));
            let (ri, di) = (self.index[&rpm], self.index[&duty]);
            self.sensors[ri].role = Role::Rpm(i);
            self.sensors[di].role = Role::Duty(i);
            self.controls.push(Control {
                id: format!("d5.{name}"),
                label: label.into(),
                kind,
                rpm_channel: rpm,
                duty_channel: duty,
                mode: Mode::Shadow,
                allowed: self.flags.writes,
                identified: self.flags.identified,
                drive: Drive::Firmware,
                target_rpm: None,
                actual_rpm: None,
                duty: None,
                commanded_duty: None,
                floor_duty: floor,
                start_duty: if i == 0 { 20.0 } else { 0.0 },
                min_rpm,
                max_rpm,
                fallback: None,
                note: None,
            });
        }
        if self.flags.fault {
            self.controls[0].fallback = Some("Wassertemperatur veraltet (> 10 s)".into());
        }
    }

    fn firmware_duty(&self, i: usize, water: f64) -> f64 {
        if i == 0 { (25.0 + 9.0 * (water - 27.0)).clamp(30.0, 100.0) } else { (30.0 + 6.0 * (water - 28.0)).clamp(15.0, 100.0) }
    }

    fn rpm_of(i: usize, duty: f64) -> f64 {
        if i == 0 { FAN_MAX_RPM * duty / 100.0 } else { PUMP_RPM.0 + (PUMP_RPM.1 - PUMP_RPM.0) * duty / 100.0 }
    }

    fn duty_of(i: usize, rpm: f64) -> f64 {
        if i == 0 { rpm / FAN_MAX_RPM * 100.0 } else { (rpm - PUMP_RPM.0) / (PUMP_RPM.1 - PUMP_RPM.0) * 100.0 }
    }

    fn analytic(&self, i: usize, t_ms: i64) -> f64 {
        let s = &self.sensors[i];
        let t = t_ms as f64 / 1000.0;
        let water = |t_ms: i64| {
            let t = t_ms as f64 / 1000.0;
            29.5 + 5.0 * (0.5 + 0.5 * (std::f64::consts::TAU * t / 86400.0).sin()) + 4.0 * wave(t, 2.0) + (hash01(1.0, t_ms / 1000) - 0.5) * 0.15
        };
        match s.role {
            Role::Duty(ci) => self.firmware_duty(ci, water(t_ms)),
            Role::Rpm(ci) => Self::rpm_of(ci, self.firmware_duty(ci, water(t_ms))) * (1.0 + 0.01 * (hash01(s.seed, t_ms / 1000) - 0.5)),
            Role::Plain => {
                let load = wave(t, 1.0);
                let gpu = wave(t, 4.0);
                let noise = hash01(s.seed, t_ms / 1000) - 0.5;
                let v = match s.channel.id.as_str() {
                    CPU_TEMP => 42.0 + 55.0 * load + noise * 2.0,
                    "/intelcpu/0/temperature/26" => 40.0 + 54.0 * load + noise * 2.0,
                    "/intelcpu/0/power/0" => 28.0 + 235.0 * load + noise * 6.0,
                    GPU_TEMP | "/gpu-nvidia/0/temperature/0" => 34.0 + 42.0 * gpu + noise,
                    "nvml/0/power" | "/gpu-nvidia/0/power/0" => 22.0 + 270.0 * gpu + noise * 5.0,
                    WATER => water(t_ms),
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

    fn plan(&mut self, water: f64, t: i64) {
        let w = self.weight;
        let need = ((water - (29.0 - 3.0 * w)) / 10.0).clamp(0.0, 1.0);
        for i in 0..2 {
            let c = &mut self.controls[i];
            if c.mode == Mode::Off {
                c.target_rpm = None;
                continue;
            }
            let want = c.min_rpm + (c.max_rpm - c.min_rpm) * need.powf(if i == 0 { 1.0 } else { 1.6 });
            let from = c.target_rpm.unwrap_or(want);
            c.target_rpm = Some(from + (want - from).clamp(-4.0, 20.0));
            c.note = (c.mode == Mode::Shadow).then(|| "nur berechnet".to_string());
            if c.mode == Mode::Active && c.drive != Drive::Test {
                c.drive = Drive::Duty;
                c.commanded_duty = Some(Self::duty_of(i, c.target_rpm.unwrap()).clamp(c.floor_duty, 100.0));
            }
        }
        let gain = 1.0 - (self.controls[0].target_rpm.unwrap_or(0.0) / FAN_MAX_RPM) * 0.8;
        self.predicted = Predicted { horizon_s: 120, water: Some(water + 0.4 + 1.2 * gain), water_limit: 43.5, feasible: water + 1.6 * gain < 41.0, age_ms: (t % 30_000) };
    }

    fn tick(&mut self) {
        let t = now_ms();
        let dt = ((t - self.last_tick) as f64 / 1000.0).clamp(0.05, 5.0);
        self.last_tick = t;
        let mut live: Vec<f64> = (0..self.sensors.len()).map(|i| self.analytic(i, t)).collect();
        live[self.index[WATER]] += self.water_offset;
        let water = live[self.index[WATER]];
        for i in 0..2 {
            let duty = match self.controls[i].commanded_duty.filter(|_| self.controls[i].drive != Drive::Firmware) {
                Some(d) => d,
                None => self.firmware_duty(i, water),
            } * (1.0 + 0.002 * (hash01(i as f64, t / 1000) - 0.5));
            let (di, ri) = (self.index[&self.controls[i].duty_channel], self.index[&self.controls[i].rpm_channel]);
            live[di] = duty;
            live[ri] = Self::rpm_of(i, duty) * (1.0 + 0.01 * (hash01(i as f64 + 0.5, t / 1000) - 0.5));
            let (rpm, d) = (live[ri], live[di]);
            self.controls[i].actual_rpm = Some(rpm);
            self.controls[i].duty = Some(d);
        }
        self.water_offset += (-6.0 * (live[self.index[&self.controls[0].duty_channel]] - 50.0) / 50.0 - self.water_offset) * (dt / 90.0).min(1.0);
        self.live = live;
        self.plan(water, t);
        if let Some((i, since)) = self.step {
            let phase = ((t - since) / 10_000) as usize;
            if phase >= 5 {
                self.step = None;
                self.controls[i].drive = Drive::Firmware;
                self.controls[i].commanded_duty = None;
            } else {
                self.controls[i].drive = Drive::Test;
                self.controls[i].commanded_duty = Some([100.0f64, 80.0, 60.0, 40.0, 100.0][phase].max(self.controls[i].floor_duty));
            }
        }
        self.seq += 1;
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
                Channel { value: Some(self.live[i]), min: Some(e.0), max: Some(e.1), ..s.channel.clone() }
            })
            .collect();
        let water = self.live[self.index[WATER]];
        let mut alarms = vec![];
        if water >= 48.0 {
            alarms.push(Alarm { kind: AlarmKind::WaterHigh, channel: WATER.into(), message: format!("Wasser {water:.1} °C über 48 °C"), value: Some(water), limit: Some(48.0), since_ms: self.last_tick });
        }
        for c in self.controls.iter().filter(|c| c.fallback.is_some()) {
            alarms.push(Alarm { kind: AlarmKind::Fallback, channel: c.rpm_channel.clone(), message: format!("Rückfall auf Firmware-PID: {}", c.fallback.clone().unwrap()), value: None, limit: None, since_ms: self.last_tick - 20_000 });
        }
        Snapshot {
            ts_ms: self.last_tick,
            seq: self.seq,
            weight: self.weight,
            model: ModelInfo { loaded: true, warm: true, warmup_left_s: 0, fan_identified: self.flags.identified, pump_identified: self.flags.identified, error: None },
            predicted: Some(self.predicted.clone()),
            step_test: self.step.map(|(i, since)| {
                let phase = (((self.last_tick - since) / 10_000) as u32).min(4);
                StepStatus { id: self.controls[i].id.clone(), phase: phase + 1, phases: 5, duty: self.controls[i].commanded_duty.unwrap_or(0.0), overridden: phase == 3, left_s: (10 - ((self.last_tick - since) / 1000) % 10) as u32 }
            }),
            channels,
            controls: self.controls.clone(),
            limits: vec![Limit { channel: WATER.into(), low: None, high: Some(48.0), plan: Some(46.0), top: Some(50.0) }, Limit { channel: "d5next/pump/rpm".into(), low: Some(1800.0), high: None, plan: None, top: None }],
            alarms,
        }
    }

    fn apply(&mut self, cmd: Command) -> Result<(), String> {
        match cmd {
            Command::SetMode { id, mode, confirmed } => {
                let i = self.controls.iter().position(|c| c.id == id).ok_or("unbekannter Kanal")?;
                if mode == Mode::Active {
                    if !self.flags.writes {
                        return Err("Schreiben ist deaktiviert (writes_enabled in der Admin-Konfiguration)".into());
                    }
                    if self.controls[i].kind == ControlKind::Pump && !confirmed {
                        return Err("Pumpe: zweite Bestätigung fehlt".into());
                    }
                    if !self.flags.identified {
                        return Err("Modell: Antwort von Lüfter und Pumpe ist nicht identifiziert, Aktivieren verweigert".into());
                    }
                }
                let c = &mut self.controls[i];
                c.mode = mode;
                c.fallback = None;
                if mode != Mode::Active && c.drive == Drive::Duty {
                    c.drive = Drive::Firmware;
                    c.commanded_duty = None;
                }
            }
            Command::SetWeight { weight } => {
                if !(0.0..=1.0).contains(&weight) {
                    return Err("Gewicht muss zwischen 0 und 1 liegen".into());
                }
                self.weight = weight;
            }
            Command::StartStepTest { id, confirmed } => {
                let i = self.controls.iter().position(|c| c.id == id).ok_or("unbekannter Kanal")?;
                if !self.flags.writes {
                    return Err("Schreiben ist deaktiviert (writes_enabled in der Admin-Konfiguration)".into());
                }
                if !confirmed || self.step.is_some() || self.controls[i].mode == Mode::Active {
                    return Err("Stufentest nicht möglich: Bestätigung fehlt, Kanal aktiv oder Test läuft schon".into());
                }
                self.step = Some((i, now_ms()));
            }
            Command::StopStepTest => {
                if let Some((i, _)) = self.step.take() {
                    self.controls[i].drive = Drive::Firmware;
                    self.controls[i].commanded_duty = None;
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
            .take(64)
            .filter_map(|id| {
                let i = *self.index.get(id)?;
                let (mut ts_ms, mut values) = (Vec::new(), Vec::new());
                let mut t = from - from.rem_euclid(1000);
                while t <= to {
                    ts_ms.push(t);
                    values.push(self.analytic(i, t));
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
    if let Some(path) = std::env::var_os("MOCK_DUMP") {
        let s = sim.lock().unwrap().snapshot();
        std::fs::write(path, serde_json::to_vec(&ResponseBody::Snapshot(s)).unwrap()).unwrap();
        return;
    }
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
