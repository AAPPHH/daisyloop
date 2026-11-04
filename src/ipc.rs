use crate::control::{Control as Controller, ControlConfig, Status};
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

pub const SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;0x0012019b;;;IU)";
const MAX_CLIENTS: usize = 8;
const MAX_LINE: u64 = 1 << 20;
const COMMAND_WAIT: Duration = Duration::from_secs(5);
const COMMAND_GAP: Duration = Duration::from_millis(100);
const MAX_HISTORY_CHANNELS: usize = 64;

pub type Job = (Command, Sender<Result<(), String>>);

pub struct Hub {
    cfg: ControlConfig,
    latest: BTreeMap<String, (i64, Row)>,
    extremes: HashMap<String, (f64, f64)>,
    history: HashMap<String, VecDeque<(i64, f64)>>,
    status: Status,
    ts_ms: i64,
    seq: u64,
}

impl Hub {
    pub fn new(cfg: &ControlConfig, status: Status) -> Hub {
        Hub { cfg: cfg.clone(), latest: BTreeMap::new(), extremes: HashMap::new(), history: HashMap::new(), status, ts_ms: 0, seq: 0 }
    }
    pub fn update(&mut self, tick: &Tick, status: Status) {
        self.ts_ms = tick.ts / 1000;
        self.seq += 1;
        self.status = status;
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
            self.latest.insert(r.channel.clone(), (self.ts_ms, r.clone()));
        }
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
    fn unit(u: &str) -> String {
        match u {
            "degC" => "°C",
            "rpm" => "RPM",
            other => other,
        }
        .to_string()
    }
    fn hardware(source: &str, channel: &str) -> String {
        let mut parts = channel.trim_start_matches('/').split('/');
        let (a, b) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let group = match (source, a) {
            ("d5next", _) => "Aquacomputer D5 NEXT".to_string(),
            ("nvml", _) => "NVIDIA GPU (NVML)".into(),
            ("daisyloop", _) => "daisyloop".into(),
            (_, "intelcpu") => "Intel CPU".into(),
            (_, "gpu-nvidia") => "NVIDIA GPU".into(),
            (_, "gpu-intel-integrated") => "Intel iGPU".into(),
            (_, "lpc") => "Mainboard".into(),
            (_, "memory") => "DIMM".into(),
            (_, "ram") | (_, "vram") => "Arbeitsspeicher".into(),
            (_, "nvme") => format!("NVMe {b}"),
            (_, "ssd") => format!("SSD {b}"),
            (_, "nic") => "Netzwerk".into(),
            (_, "usbhid") => "D5 NEXT (LibreHardwareMonitor)".into(),
            ("lhm", _) => "LibreHardwareMonitor".into(),
            (other, _) => other.to_string(),
        };
        group
    }
    pub fn snapshot(&self) -> Snapshot {
        let wall = crate::now_micros() / 1000;
        let stale_ms = (self.cfg.stale_secs * 1000.0) as i64;
        let channels = self
            .latest
            .values()
            .map(|(seen, r)| {
                let e = self.extremes.get(&r.channel).copied();
                let age = (wall - seen).max(0);
                Channel { id: r.channel.clone(), label: r.label.clone(), hardware: Self::hardware(&r.source, &r.channel), kind: Self::kind(&r.kind), unit: Self::unit(&r.unit), value: Some(r.value), min: e.map(|x| x.0), max: e.map(|x| x.1), age_ms: age, stale: age > stale_ms }
            })
            .collect();
        Snapshot {
            ts_ms: self.ts_ms,
            seq: self.seq,
            weight: self.status.weight,
            model: self.status.model.clone(),
            predicted: self.status.predicted.clone(),
            step_test: self.status.step.clone(),
            channels,
            controls: self.status.controls.clone(),
            limits: self.status.limits.clone(),
            alarms: self.status.alarms.clone(),
        }
    }
    pub fn history(&self, q: &HistoryQuery) -> HistoryResponse {
        let n = q.max_points.clamp(2, 4000);
        let mut ids: Vec<&String> = Vec::new();
        for id in &q.channels {
            if !ids.contains(&id) && ids.len() < MAX_HISTORY_CHANNELS {
                ids.push(id);
            }
        }
        let series = ids
            .into_iter()
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
        let mut last_command: Option<std::time::Instant> = None;
        while let Ok(req) = io.recv() {
            let limited = matches!(req.body, RequestBody::Command(_)) && last_command.is_some_and(|t| t.elapsed() < COMMAND_GAP);
            if matches!(req.body, RequestBody::Command(_)) {
                last_command = Some(std::time::Instant::now());
            }
            let resp = if limited { Response::new(req.id, ResponseBody::Error { message: "zu viele Befehle in kurzer Zeit".into() }) } else { self.handle(req) };
            if io.send(&resp).is_err() {
                break;
            }
        }
    }
}

pub fn apply(control: &mut Controller, cmd: Command, now: i64) -> Result<(), String> {
    match cmd {
        Command::SetMode { id, mode, confirmed } => control.set_mode(&id, mode, confirmed, now),
        Command::SetWeight { weight } => control.set_weight(weight),
        Command::StartStepTest { id, confirmed } => control.start_test(&id, confirmed, now),
        Command::StopStepTest => control.stop_test(now),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{D5Actuator, Factory};
    use crate::Config;

    fn control(writes: bool) -> Controller {
        let cfg = Config { writes_enabled: writes, ..Config::default() };
        let factory: Factory = Box::new(|| Err("no device in tests".into()));
        Controller::new(&cfg, Box::new(D5Actuator::new(factory, &cfg.control)), Err("no model in tests".into()))
    }

    fn tick(ts_us: i64, rows: Vec<Row>) -> Tick {
        Tick { ts: ts_us, rows, raw: None }
    }

    #[test]
    fn commands_surface_their_refusals() {
        let mut c = control(false);
        let set = |id: &str, mode, confirmed| Command::SetMode { id: id.into(), mode, confirmed };
        assert!(apply(&mut c, set("d5.fan", Mode::Active, true), 0).unwrap_err().contains("writes_enabled"));
        assert!(apply(&mut c, set("board.1", Mode::Off, false), 0).is_err());
        assert!(apply(&mut c, set("d5.pump", Mode::Off, false), 0).is_ok());
        assert!(apply(&mut c, Command::SetWeight { weight: 2.0 }, 0).is_err());
        assert!(apply(&mut c, Command::SetWeight { weight: 0.25 }, 0).is_ok());
        assert!(apply(&mut c, Command::StartStepTest { id: "d5.fan".into(), confirmed: true }, 0).unwrap_err().contains("writes_enabled"));
        assert!(apply(&mut c, Command::StopStepTest, 0).is_ok());
        let mut w = control(true);
        assert!(apply(&mut w, set("d5.pump", Mode::Active, false), 0).unwrap_err().contains("Bestätigung"));
    }

    #[test]
    fn snapshot_marks_stale_channels_normalises_units_and_groups_hardware() {
        let cfg = ControlConfig::default();
        let c = control(false);
        let mut hub = Hub::new(&cfg, c.status(0));
        let first = hub.snapshot();
        assert_eq!((first.seq, first.channels.len(), first.controls.len()), (0, 0, 2));
        let now = crate::now_micros();
        let old = vec![Row::new("d5next", "d5next/water/temp", "Water", "temperature", "degC", 31.0)];
        hub.update(&tick(now - 30_000_000, old), c.status(0));
        let fresh = vec![
            Row::new("d5next", "d5next/pump/rpm", "Pump", "rpm", "rpm", 2570.0),
            Row::new("lhm", "/intelcpu/0/temperature/0", "CPU", "temperature", "degC", 50.0),
            Row::new("lhm", "/lpc/nct6687dr/0/fan/1", "Fan", "rpm", "rpm", 700.0),
            Row::new("lhm", "/nvme/2/temperature/0", "NVMe", "temperature", "degC", 40.0),
        ];
        hub.update(&tick(now, fresh), c.status(0));
        let s = hub.snapshot();
        let ch = |id: &str| s.channels.iter().find(|c| c.id == id).unwrap();
        assert!(ch("d5next/water/temp").stale && ch("d5next/water/temp").age_ms >= 29_000 && ch("d5next/water/temp").value == Some(31.0));
        assert!(!ch("d5next/pump/rpm").stale);
        assert_eq!((ch("d5next/water/temp").unit.as_str(), ch("d5next/pump/rpm").unit.as_str()), ("°C", "RPM"));
        assert_eq!(ch("/intelcpu/0/temperature/0").hardware, "Intel CPU");
        assert_eq!(ch("/lpc/nct6687dr/0/fan/1").hardware, "Mainboard");
        assert_eq!(ch("/nvme/2/temperature/0").hardware, "NVMe 2");
        assert_eq!(ch("d5next/pump/rpm").hardware, "Aquacomputer D5 NEXT");
    }

    #[test]
    fn history_dedupes_and_caps_channels_and_downsamples() {
        let cfg = ControlConfig::default();
        let c = control(false);
        let mut hub = Hub::new(&cfg, c.status(0));
        for i in 0..1000i64 {
            let rows = (0..80).map(|k| Row::new("lhm", format!("/x/{k}"), "x", "load", "%", i as f64)).collect();
            hub.update(&tick(i * 1_000_000, rows), c.status(0));
        }
        let mut ids: Vec<String> = (0..80).map(|k| format!("/x/{k}")).collect();
        ids.extend(std::iter::repeat("/x/0".to_string()).take(4000));
        let h = hub.history(&HistoryQuery { channels: ids, from_ms: 0, to_ms: i64::MAX, max_points: 100 });
        assert_eq!(h.series.len(), MAX_HISTORY_CHANNELS);
        assert!(h.series.iter().all(|s| s.values.len() <= 100 && s.values.len() >= 50));
        let (tx, _rx) = channel();
        let ipc = Ipc { hub: Arc::new(Mutex::new(hub)), jobs: tx };
        let r = ipc.handle(Request::new(7, RequestBody::Snapshot));
        assert_eq!(r.id, 7);
        assert!(matches!(r.body, ResponseBody::Snapshot(_)));
        let bad = ipc.handle(Request { v: 1, id: 8, body: RequestBody::Snapshot });
        assert!(matches!(bad.body, ResponseBody::Error { .. }));
    }

    #[test]
    fn pipe_acl_does_not_grant_instance_creation_to_interactive_users() {
        assert!(SDDL.contains("(A;;0x0012019b;;;IU)") && !SDDL.contains("GRGW"));
    }
}
