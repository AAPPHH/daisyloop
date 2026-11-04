use crate::control::{Control as Controller, ControlConfig, LoopStatus};
use crate::lake::Tick;
use crate::protocol::*;
use crate::Row;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::{GetLastError, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};

pub const SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;GRGW;;;IU)";
const MAX_CLIENTS: usize = 8;
const MAX_LINE: u64 = 1 << 20;
const COMMAND_WAIT: Duration = Duration::from_secs(5);

pub type Job = (Command, Sender<Result<(), String>>);

pub struct Hub {
    cfg: ControlConfig,
    latest: BTreeMap<String, Row>,
    extremes: HashMap<String, (f64, f64)>,
    history: HashMap<String, VecDeque<(i64, f64)>>,
    statuses: Vec<LoopStatus>,
    ts_ms: i64,
    seq: u64,
    alarm_since: Option<i64>,
}

impl Hub {
    pub fn new(cfg: &ControlConfig) -> Hub {
        Hub { cfg: cfg.clone(), latest: BTreeMap::new(), extremes: HashMap::new(), history: HashMap::new(), statuses: vec![], ts_ms: 0, seq: 0, alarm_since: None }
    }
    pub fn update(&mut self, tick: &Tick, statuses: Vec<LoopStatus>) {
        self.ts_ms = tick.ts / 1000;
        self.seq += 1;
        self.statuses = statuses;
        let keep = self.cfg.history_secs as i64 * 1000;
        for r in &tick.rows {
            if !r.value.is_finite() {
                continue;
            }
            let e = self.extremes.entry(r.channel.clone()).or_insert((r.value, r.value));
            e.0 = e.0.min(r.value);
            e.1 = e.1.max(r.value);
            let h = self.history.entry(r.channel.clone()).or_default();
            h.push_back((self.ts_ms, r.value));
            while h.front().is_some_and(|p| self.ts_ms - p.0 > keep) {
                h.pop_front();
            }
            self.latest.insert(r.channel.clone(), r.clone());
        }
        let hot = self.latest.get(&self.cfg.water_channel).is_some_and(|r| r.value > self.cfg.water_limit);
        self.alarm_since = if hot { self.alarm_since.or(Some(self.ts_ms)) } else { None };
    }
    fn kind(k: &str) -> Kind {
        match k {
            "temperature" => Kind::Temperature,
            "rpm" => Kind::Fan,
            "duty" => Kind::Duty,
            "power" => Kind::Power,
            "voltage" => Kind::Voltage,
            "current" => Kind::Current,
            "load" => Kind::Load,
            "clock" => Kind::Clock,
            "flow" => Kind::Flow,
            _ => Kind::Other,
        }
    }
    fn hardware(source: &str) -> &str {
        match source {
            "d5next" => "Aquacomputer D5 NEXT",
            "nvml" => "NVIDIA GPU",
            "lhm" => "LibreHardwareMonitor",
            "daisyloop" => "daisyloop",
            other => other,
        }
    }
    fn control(&self, s: &LoopStatus) -> Control {
        let pump = s.name == "pump";
        Control {
            id: format!("d5.{}", s.name),
            label: if pump { "Pumpe (D5 NEXT)".into() } else { "Radiatorlüfter (D5 NEXT)".into() },
            kind: if pump { ControlKind::Pump } else { ControlKind::Fan },
            rpm_channel: format!("d5next/{}/rpm", s.name),
            duty_channel: format!("d5next/{}/duty", s.name),
            mode: if s.state == 1 { Mode::Pid } else { Mode::Manual },
            floor: s.lo,
            pid: PidParams { source: self.cfg.water_channel.clone(), setpoint: s.target, kp: s.kp, ki: s.ki, kd: s.kd, min_duty: s.lo, max_duty: s.hi },
            state: PidState { output: s.setpoint.unwrap_or(s.output), error: s.terms.error, p: s.terms.p, i: s.terms.i, d: s.terms.d, saturated: s.terms.saturated },
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        let channels = self
            .latest
            .values()
            .map(|r| {
                let e = self.extremes.get(&r.channel).copied();
                Channel { id: r.channel.clone(), label: r.label.clone(), hardware: Self::hardware(&r.source).into(), kind: Self::kind(&r.kind), unit: r.unit.clone(), value: Some(r.value), min: e.map(|x| x.0), max: e.map(|x| x.1) }
            })
            .collect();
        let water = &self.cfg.water_channel;
        let alarms = match (self.alarm_since, self.latest.get(water)) {
            (Some(since), Some(r)) => vec![Alarm { channel: water.clone(), side: Side::High, limit: self.cfg.water_limit, value: r.value, since_ms: since }],
            _ => vec![],
        };
        Snapshot {
            ts_ms: self.ts_ms,
            seq: self.seq,
            channels,
            controls: self.statuses.iter().map(|s| self.control(s)).collect(),
            limits: vec![Limit { channel: water.clone(), low: None, high: Some(self.cfg.water_limit) }],
            alarms,
        }
    }
    pub fn history(&self, q: &HistoryQuery) -> HistoryResponse {
        let n = q.max_points.clamp(2, 4000);
        let series = q
            .channels
            .iter()
            .filter_map(|id| {
                let pts: Vec<&(i64, f64)> = self.history.get(id)?.iter().filter(|p| p.0 >= q.from_ms && p.0 <= q.to_ms).collect();
                let stride = pts.len().div_ceil(n).max(1);
                let picked: Vec<&&(i64, f64)> = pts.iter().step_by(stride).collect();
                Some(Series { channel: id.clone(), ts_ms: picked.iter().map(|p| p.0).collect(), values: picked.iter().map(|p| p.1).collect() })
            })
            .collect();
        HistoryResponse { series }
    }
}

struct Framed {
    reader: BufReader<File>,
    writer: File,
}

impl Framed {
    fn new(file: File) -> std::io::Result<Framed> {
        Ok(Framed { writer: file.try_clone()?, reader: BufReader::new(file) })
    }
    fn recv(&mut self) -> std::io::Result<Request> {
        let mut line = String::new();
        if self.reader.by_ref().take(MAX_LINE).read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        serde_json::from_str(&line).map_err(std::io::Error::other)
    }
    fn send(&mut self, msg: &Response) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
        line.push(b'\n');
        self.writer.write_all(&line)?;
        self.writer.flush()
    }
}

pub struct Ipc {
    hub: Arc<Mutex<Hub>>,
    jobs: Sender<Job>,
}

impl Ipc {
    pub fn spawn(hub: Arc<Mutex<Hub>>, jobs: Sender<Job>) {
        let ipc = Arc::new(Ipc { hub, jobs });
        std::thread::Builder::new().name("ipc".into()).spawn(move || ipc.accept_loop()).expect("ipc thread");
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    fn security() -> Option<SECURITY_ATTRIBUTES> {
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(Self::wide(SDDL).as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) };
        (ok != 0).then_some(SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 })
    }
    fn create(name: &[u16], sa: &SECURITY_ATTRIBUTES, first: bool) -> Option<File> {
        let open = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        let h = unsafe { CreateNamedPipeW(name.as_ptr(), open, mode, PIPE_UNLIMITED_INSTANCES, 1 << 20, 1 << 20, 0, sa) };
        if h == INVALID_HANDLE_VALUE {
            log::error!("ipc: CreateNamedPipe failed: {}", unsafe { GetLastError() });
            return None;
        }
        Some(unsafe { File::from_raw_handle(h as _) })
    }
    fn accept_loop(self: Arc<Self>) {
        let Some(sa) = Self::security() else { return log::error!("ipc: security descriptor could not be built, pipe not served") };
        let name = Self::wide(PIPE_NAME);
        let active = Arc::new(AtomicUsize::new(0));
        let mut first = true;
        loop {
            let Some(pipe) = Self::create(&name, &sa, first) else {
                if first {
                    return log::error!("ipc: {PIPE_NAME} already exists, pipe not served");
                }
                std::thread::sleep(Duration::from_secs(1));
                continue;
            };
            first = false;
            use std::os::windows::io::AsRawHandle;
            let connected = unsafe { ConnectNamedPipe(pipe.as_raw_handle() as _, std::ptr::null_mut()) } != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            if !connected || active.load(Ordering::Relaxed) >= MAX_CLIENTS {
                continue;
            }
            active.fetch_add(1, Ordering::Relaxed);
            let (me, active) = (self.clone(), active.clone());
            std::thread::spawn(move || {
                me.serve(pipe);
                active.fetch_sub(1, Ordering::Relaxed);
            });
        }
    }
    pub fn handle(&self, req: Request) -> Response {
        let body = if req.v != VERSION {
            ResponseBody::Error { message: format!("protocol v{} not supported", req.v) }
        } else {
            match req.body {
                RequestBody::Snapshot => ResponseBody::Snapshot(self.hub.lock().unwrap().snapshot()),
                RequestBody::History(q) => ResponseBody::History(self.hub.lock().unwrap().history(&q)),
                RequestBody::Command(c) => {
                    let (tx, rx) = channel();
                    match self.jobs.send((c, tx)).ok().and_then(|_| rx.recv_timeout(COMMAND_WAIT).ok()) {
                        Some(Ok(())) => ResponseBody::Ack,
                        Some(Err(message)) => ResponseBody::Error { message },
                        None => ResponseBody::Error { message: "daemon did not answer the command".into() },
                    }
                }
            }
        };
        Response::new(req.id, body)
    }
    fn serve(&self, file: File) {
        let Ok(mut io) = Framed::new(file) else { return };
        while let Ok(req) = io.recv() {
            if io.send(&self.handle(req)).is_err() {
                break;
            }
        }
    }
}

pub fn apply(control: &mut Controller, cmd: Command, now: i64) -> Result<(), String> {
    let name = |id: &str| id.strip_prefix("d5.").filter(|n| control.index(n).is_some()).map(str::to_string).ok_or_else(|| "unknown control".to_string());
    match cmd {
        Command::SetMode { id, mode: Mode::Pid } => control.enable(&name(&id)?, now),
        Command::SetMode { id, mode: Mode::Manual } => {
            control.disable(&name(&id)?, now);
            Ok(())
        }
        Command::SetMode { .. } => Err("curve mode is not supported, the firmware stays in set-point mode".into()),
        Command::SetPid { id, pid } => control.set_pid(&name(&id)?, pid.setpoint, pid.kp, pid.ki, pid.kd),
        Command::SetLimit { .. } => Err("limits are not configurable at runtime".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    fn control(fan: bool, pump: bool, name: &str) -> Controller {
        let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("test-tmp").join(format!("ipc-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let cfg = Config { fan_control_enabled: fan, pump_control_enabled: pump, data_dir: d, ..Config::default() };
        Controller::new(&cfg, Box::new(|| Err("no device in tests".into())))
    }

    fn pid(setpoint: f64) -> PidParams {
        PidParams { source: "d5next/water/temp".into(), setpoint, kp: 0.3, ki: 0.002, kd: 0.0, min_duty: 28.0, max_duty: 36.0 }
    }

    #[test]
    fn commands_are_refused_without_the_flags() {
        let mut c = control(false, false, "none");
        for id in ["d5.fan", "d5.pump"] {
            assert!(apply(&mut c, Command::SetMode { id: id.into(), mode: Mode::Pid }, 0).unwrap_err().contains("disabled"));
            assert!(apply(&mut c, Command::SetPid { id: id.into(), pid: pid(31.0) }, 0).unwrap_err().contains("disabled"));
        }
        assert!(apply(&mut c, Command::SetMode { id: "d5.fan".into(), mode: Mode::Curve }, 0).is_err());
        assert!(apply(&mut c, Command::SetMode { id: "board.1".into(), mode: Mode::Pid }, 0).is_err());
        assert!(apply(&mut c, Command::SetLimit { channel: "x".into(), low: None, high: Some(1.0) }, 0).is_err());
    }

    #[test]
    fn pump_commands_need_the_pump_flag() {
        let mut c = control(true, false, "fan-only");
        assert!(apply(&mut c, Command::SetMode { id: "d5.pump".into(), mode: Mode::Pid }, 0).unwrap_err().contains("pump_control_enabled"));
        assert!(apply(&mut c, Command::SetPid { id: "d5.pump".into(), pid: pid(31.0) }, 0).unwrap_err().contains("pump_control_enabled"));
        assert!(apply(&mut c, Command::SetPid { id: "d5.fan".into(), pid: pid(31.0) }, 0).is_ok());
        assert!(apply(&mut c, Command::SetPid { id: "d5.fan".into(), pid: pid(50.0) }, 0).is_err());
        assert!(apply(&mut c, Command::SetMode { id: "d5.fan".into(), mode: Mode::Pid }, 0).unwrap_err().contains("water"));
    }

    #[test]
    fn hub_serves_snapshot_and_downsampled_history() {
        let cfg = ControlConfig::default();
        let mut hub = Hub::new(&cfg);
        let c = control(false, false, "hub");
        for i in 0..1000i64 {
            let rows = vec![Row::new("d5next", "d5next/water/temp", "Water", "temperature", "degC", 30.0 + i as f64 / 100.0), Row::new("d5next", "d5next/pump/rpm", "Pump", "rpm", "rpm", 2570.0)];
            hub.update(&Tick { ts: i * 1_000_000, rows, raw: None }, c.statuses());
        }
        let s = hub.snapshot();
        assert_eq!(s.seq, 1000);
        assert_eq!(s.channels.len(), 2);
        assert_eq!(s.controls.len(), 2);
        assert!(s.controls.iter().all(|c| c.mode == Mode::Manual));
        assert_eq!(s.alarms.len(), 0);
        let water = s.channels.iter().find(|c| c.id == "d5next/water/temp").unwrap();
        assert_eq!((water.min, water.max), (Some(30.0), Some(39.99)));
        let h = hub.history(&HistoryQuery { channels: vec!["d5next/water/temp".into(), "nope".into()], from_ms: 0, to_ms: i64::MAX, max_points: 100 });
        assert_eq!(h.series.len(), 1);
        assert!(h.series[0].values.len() <= 100 && h.series[0].values.len() >= 50);
        let (tx, _rx) = channel();
        let ipc = Ipc { hub: Arc::new(Mutex::new(hub)), jobs: tx };
        let r = ipc.handle(Request::new(7, RequestBody::Snapshot));
        assert_eq!(r.id, 7);
        assert!(matches!(r.body, ResponseBody::Snapshot(_)));
        let bad = ipc.handle(Request { v: 99, id: 8, body: RequestBody::Snapshot });
        assert!(matches!(bad.body, ResponseBody::Error { .. }));
    }
}
