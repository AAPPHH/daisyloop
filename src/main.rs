#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod lake;
mod sources;
use lake::{Committer, Lake, Tick, Wal};
use serde::{Deserialize, Serialize};
use sources::{D5Next, Lhm, Nvidia, Source};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use simplelog::{ConfigBuilder, LevelFilter, WriteLogger};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub interval_secs: f64,
    pub commit_secs: u64,
    pub wal_sync_secs: u64,
    pub stall_secs: u64,
    pub poll_timeout_ms: u64,
    pub keep_versions: usize,
    pub data_dir: PathBuf,
    pub sidecar: Option<PathBuf>,
    pub sidecar_stale_secs: u64,
    pub d5_vid: u16,
    pub d5_pid: u16,
    pub d5_usage_page: u16,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            interval_secs: 1.0,
            commit_secs: 900,
            wal_sync_secs: 10,
            stall_secs: 60,
            poll_timeout_ms: 1300,
            keep_versions: 10,
            data_dir: Config::default_data_dir(),
            sidecar: None,
            sidecar_stale_secs: 15,
            d5_vid: 0x0C70,
            d5_pid: 0xF00E,
            d5_usage_page: 0xFF00,
        }
    }
}

impl Config {
    pub fn default_data_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os("DAISYLOOP_DATA_DIR") {
            return PathBuf::from(dir);
        }
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        if let Some(dir) = exe_dir.filter(|d| d.file_name().is_some_and(|n| n.eq_ignore_ascii_case("bin"))) {
            if let Some(parent) = dir.parent() {
                return parent.to_path_buf();
            }
        }
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("daisyloop")
    }
    pub fn load() -> Config {
        let dir = Config::default_data_dir();
        let text = std::fs::read_to_string(dir.join("config.toml")).unwrap_or_default();
        let mut cfg: Config = toml::from_str(&text).unwrap_or_default();
        cfg.data_dir = dir;
        cfg
    }
    pub fn sidecar_exe(&self) -> PathBuf {
        if let Some(p) = &self.sidecar {
            return p.clone();
        }
        let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default();
        let deployed = dir.join("sidecar").join("daisyloop-lhm.exe");
        if deployed.exists() {
            return deployed;
        }
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sidecar").join("publish").join("daisyloop-lhm.exe")
    }
    pub fn mutex_name(&self) -> String {
        if std::env::var_os("DAISYLOOP_DATA_DIR").is_none() {
            return r"Global\daisyloop-collector".into();
        }
        let h = self.data_dir.to_string_lossy().to_lowercase().bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
        format!(r"Global\daisyloop-{h:016x}")
    }
    pub fn catalog_db(&self) -> PathBuf {
        self.data_dir.join("catalog.db")
    }
    pub fn warehouse(&self) -> PathBuf {
        self.data_dir.join("warehouse")
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Row {
    pub source: String,
    pub channel: String,
    pub label: String,
    pub kind: String,
    pub unit: String,
    pub role: String,
    pub value: f64,
}

impl Row {
    pub fn new(source: &'static str, channel: impl Into<String>, label: impl Into<String>, kind: &'static str, unit: impl Into<String>, value: f64) -> Row {
        let role = match kind {
            "duty" => "input",
            "power" | "load" | "clock" => "disturbance",
            "temperature" | "rpm" | "flow" => "state",
            _ => "aux",
        };
        Row { source: source.into(), channel: channel.into(), label: label.into(), kind: kind.into(), unit: unit.into(), role: role.into(), value }
    }
    pub fn role(mut self, role: &'static str) -> Row {
        self.role = role.into();
        self
    }
}

pub struct Sample {
    pub rows: Vec<Row>,
    pub raw: Option<Vec<u8>>,
}

pub fn now_micros() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0)
}

struct Worker {
    name: &'static str,
    tx: Sender<u64>,
    busy_since: Option<Instant>,
    failing: bool,
}

type Factory = fn(&Config) -> Box<dyn Source>;
type Reply = (usize, u64, Result<Sample, String>);

struct Collector {
    workers: Vec<Worker>,
    results: Receiver<Reply>,
    seq: u64,
    wal: Wal,
    committer: Committer,
    alive: Arc<AtomicU64>,
    cfg: Config,
}

impl Collector {
    fn new(cfg: &Config) -> Result<Collector, Box<dyn std::error::Error>> {
        let wal = Wal::open(&cfg.data_dir, Duration::from_secs(cfg.wal_sync_secs))?;
        let lake = Lake::open(cfg)?;
        let committer = Committer::spawn(lake, cfg.data_dir.clone(), Duration::from_secs(60));
        let (rtx, results) = channel();
        let factories: [(&'static str, Factory); 3] = [
            ("d5next", |c| Box::new(D5Next::new(c))),
            ("nvml", |_| Box::new(Nvidia::new())),
            ("lhm", |c| Box::new(Lhm::new(c))),
        ];
        let workers = factories
            .into_iter()
            .enumerate()
            .map(|(i, (name, make))| {
                let (tx, rx) = channel::<u64>();
                let (rtx, c) = (rtx.clone(), cfg.clone());
                std::thread::Builder::new()
                    .name(name.into())
                    .spawn(move || {
                        let mut src = make(&c);
                        for seq in rx {
                            if rtx.send((i, seq, src.poll())).is_err() {
                                break;
                            }
                        }
                    })
                    .expect("source thread");
                Worker { name, tx, busy_since: None, failing: false }
            })
            .collect();
        Ok(Collector { workers, results, seq: 0, wal, committer, alive: Arc::new(AtomicU64::new(0)), cfg: cfg.clone() })
    }

    fn accept(&mut self, i: usize, r: Result<Sample, String>, tick: &mut Tick) {
        let w = &mut self.workers[i];
        match r {
            Ok(sample) => {
                if w.failing {
                    log::info!("{} recovered", w.name);
                    w.failing = false;
                }
                tick.rows.extend(sample.rows);
                if sample.raw.is_some() {
                    tick.raw = sample.raw;
                }
            }
            Err(e) => {
                if !w.failing {
                    log::warn!("{} unavailable: {e}", w.name);
                    w.failing = true;
                }
            }
        }
    }

    fn tick(&mut self) {
        while let Ok((i, _, _)) = self.results.try_recv() {
            self.workers[i].busy_since = None;
        }
        self.seq += 1;
        let mut tick = Tick { ts: now_micros(), ..Tick::default() };
        let mut waiting = 0;
        for w in &mut self.workers {
            match w.busy_since {
                None => {
                    if w.tx.send(self.seq).is_ok() {
                        w.busy_since = Some(Instant::now());
                        waiting += 1;
                    }
                }
                Some(t) if t.elapsed() > Duration::from_secs(self.cfg.stall_secs) => {
                    log::error!("{} poll hung, exiting", w.name);
                    std::process::exit(2);
                }
                Some(_) => {}
            }
        }
        let deadline = Instant::now() + Duration::from_millis(self.cfg.poll_timeout_ms);
        while waiting > 0 {
            match self.results.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok((i, seq, r)) => {
                    self.workers[i].busy_since = None;
                    if seq == self.seq {
                        waiting -= 1;
                        self.accept(i, r, &mut tick);
                    }
                }
                Err(_) => break,
            }
        }
        if !tick.rows.is_empty() || tick.raw.is_some() {
            if let Err(e) = self.wal.append(&tick) {
                log::error!("wal append failed: {e}");
            }
        }
    }

    fn rotate(&mut self) {
        match self.wal.rotate() {
            Ok(true) => self.committer.notify(),
            Ok(false) => {}
            Err(e) => log::error!("wal rotate failed: {e}"),
        }
    }

    fn spawn_watchdog(&self) {
        let alive = self.alive.clone();
        let stall = self.cfg.stall_secs;
        std::thread::spawn(move || {
            let (mut last, mut quiet) = (0, 0);
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let now = alive.load(Ordering::Relaxed);
                quiet = if now == last { quiet + 5 } else { 0 };
                last = now;
                if quiet > stall {
                    log::error!("tick loop stalled for {quiet}s, exiting");
                    std::process::exit(3);
                }
            }
        });
    }

    fn run(mut self) {
        self.spawn_watchdog();
        let interval = Duration::from_secs_f64(self.cfg.interval_secs);
        let commit_every = Duration::from_secs(self.cfg.commit_secs);
        let stop_file = self.cfg.data_dir.join("stop");
        let heartbeat = self.cfg.data_dir.join("heartbeat");
        let mut next = Instant::now();
        let mut last_commit = Instant::now();
        self.committer.notify();
        loop {
            self.tick();
            self.alive.fetch_add(1, Ordering::Relaxed);
            let _ = std::fs::write(&heartbeat, now_micros().to_string());
            if !self.committer.alive() {
                log::error!("commit thread died, exiting");
                std::process::exit(4);
            }
            if stop_file.exists() {
                let _ = std::fs::remove_file(&stop_file);
                self.rotate();
                self.committer.finish();
                log::info!("stopped");
                return;
            }
            if last_commit.elapsed() >= commit_every {
                self.rotate();
                last_commit = Instant::now();
            }
            next += interval;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > interval * 5 {
                next = now;
            }
        }
    }
}

fn single_instance(name: &str) -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        let err = GetLastError();
        if h.is_null() {
            return err != ERROR_ACCESS_DENIED;
        }
        err != ERROR_ALREADY_EXISTS
    }
}

fn init_log(cfg: &Config) {
    let path = cfg.data_dir.join("collector.log");
    if std::fs::metadata(&path).map(|m| m.len() > 8 << 20).unwrap_or(false) {
        let _ = std::fs::rename(&path, cfg.data_dir.join("collector.log.old"));
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let mut b = ConfigBuilder::new();
        b.set_time_format_rfc3339();
        let _ = b.set_time_offset_to_local();
        let _ = WriteLogger::init(LevelFilter::Info, b.build(), f);
    }
}

fn main() {
    let cfg = Config::load();
    let _ = std::fs::create_dir_all(&cfg.data_dir);
    if !single_instance(&cfg.mutex_name()) {
        return;
    }
    init_log(&cfg);
    std::panic::set_hook(Box::new(|i| log::error!("panic: {i}")));
    let c = match Collector::new(&cfg) {
        Ok(c) => c,
        Err(e) => {
            log::error!("startup failed: {e}");
            return;
        }
    };
    log::info!("collector started, data {}, interval {}s, commit {}s", cfg.data_dir.display(), cfg.interval_secs, cfg.commit_secs);
    c.run();
}
