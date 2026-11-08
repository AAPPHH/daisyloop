#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod control;
mod correct;
mod d5;
mod hot;
mod ipc;
mod lake;
mod model;
mod protocol;
mod sources;
use control::Control;
use d5::{D5Actuator, D5Control, HidControl};
use ipc::{Hub, Ipc, Job};
use hot::HotStore;
use lake::{Committer, Gate, Idle, Lake, Legacy, Tick};
use model::{Model, XgbModel};
use serde::{Deserialize, Serialize};
use sources::{D5Next, Lhm, Nvidia, Source};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use simplelog::{ConfigBuilder, LevelFilter, WriteLogger};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub interval_secs: f64,
    pub commit_secs: u64,
    pub wal_sync_secs: u64,
    pub wal_max_bytes: u64,
    pub parked_max_bytes: u64,
    pub gate: lake::GateConfig,
    pub stall_secs: u64,
    pub poll_timeout_ms: u64,
    pub keep_versions: usize,
    pub data_dir: PathBuf,
    pub sidecar: Option<PathBuf>,
    pub sidecar_stale_secs: u64,
    pub d5_vid: u16,
    pub d5_pid: u16,
    pub d5_usage_page: u16,
    pub writes_enabled: bool,
    pub control: control::ControlConfig,
    pub ipc_enabled: bool,
    #[serde(skip)]
    pub install_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            interval_secs: 1.0,
            commit_secs: 3600,
            wal_sync_secs: 1,
            wal_max_bytes: 2 << 30,
            parked_max_bytes: 10 << 30,
            gate: lake::GateConfig::default(),
            stall_secs: 60,
            poll_timeout_ms: 1300,
            keep_versions: 10,
            data_dir: Config::default_data_dir(),
            sidecar: None,
            sidecar_stale_secs: 15,
            d5_vid: 0x0C70,
            d5_pid: 0xF00E,
            d5_usage_page: 0xFF00,
            writes_enabled: false,
            control: control::ControlConfig::default(),
            ipc_enabled: true,
            install_dir: None,
        }
    }
}

impl Config {
    pub fn default_data_dir() -> PathBuf {
        Config::dev_data_dir(std::env::var_os("DAISYLOOP_DATA_DIR").map(PathBuf::from), std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
    }
    fn dev_data_dir(env_dir: Option<PathBuf>, local: Option<PathBuf>) -> PathBuf {
        env_dir.unwrap_or_else(|| local.unwrap_or_else(std::env::temp_dir).join("daisyloop"))
    }
    const ADMIN_KEYS: [&'static str; 9] = ["interval_secs", "data_dir", "sidecar", "writes_enabled", "control", "ipc_enabled", "d5_vid", "d5_pid", "d5_usage_page"];
    pub fn program_dir() -> PathBuf {
        std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files")).join("daisyloop")
    }
    fn normal(p: &Path) -> String {
        p.to_string_lossy().to_lowercase().replace('/', "\\").trim_end_matches('\\').to_string()
    }
    pub fn is_install_dir(exe_dir: &Path, program_dir: &Path) -> bool {
        Config::normal(exe_dir) == Config::normal(program_dir)
    }
    pub fn installed_dir() -> Option<PathBuf> {
        let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        Config::is_install_dir(&dir, &Config::program_dir()).then_some(dir)
    }
    fn table(path: &Path) -> Result<toml::Table, String> {
        match std::fs::read_to_string(path) {
            Ok(t) => t.parse::<toml::Table>().map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
    pub fn from_tables(mut user: toml::Table, admin: Option<toml::Table>) -> Config {
        if let Some(admin) = admin {
            for k in Config::ADMIN_KEYS {
                user.remove(k);
            }
            user.extend(admin);
        }
        toml::Value::Table(user).try_into().unwrap_or_default()
    }
    pub fn resolve(install: Option<&Path>, env_dir: Option<PathBuf>, local: Option<PathBuf>, read: &dyn Fn(&Path) -> Result<toml::Table, String>) -> Result<Config, String> {
        let Some(install) = install else {
            let dir = Config::dev_data_dir(env_dir, local);
            let mut cfg = Config::from_tables(read(&dir.join("config.toml")).unwrap_or_default(), None);
            cfg.data_dir = dir;
            return Ok(cfg);
        };
        let admin = read(&install.join("config.toml"))?;
        let dir = admin.get("data_dir").and_then(|v| v.as_str()).map(PathBuf::from).ok_or_else(|| format!("{}: no data_dir", install.join("config.toml").display()))?;
        let mut cfg = Config::from_tables(read(&dir.join("config.toml")).unwrap_or_default(), Some(admin));
        cfg.data_dir = dir;
        cfg.install_dir = Some(install.to_path_buf());
        Ok(cfg)
    }
    pub fn load() -> Result<Config, String> {
        let env_dir = std::env::var_os("DAISYLOOP_DATA_DIR").map(PathBuf::from);
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        let installed = Config::installed_dir();
        let cfg = Config::resolve(installed.as_deref(), env_dir, local, &Config::table);
        if let (Err(e), Some(dir)) = (&cfg, &installed) {
            let state = dir.join("state");
            let _ = std::fs::create_dir_all(&state);
            let _ = std::fs::write(state.join("config-error.log"), format!("{e}\r\n"));
        }
        cfg
    }
    pub fn state_dir(&self) -> PathBuf {
        self.install_dir.as_ref().map_or_else(|| self.data_dir.clone(), |d| d.join("state"))
    }
    pub fn sidecar_exe(&self) -> PathBuf {
        if let Some(p) = self.sidecar.as_ref().filter(|_| self.install_dir.is_none()) {
            return p.clone();
        }
        let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default();
        let deployed = dir.join("sidecar").join("daisyloop-lhm.exe");
        if self.install_dir.is_some() || deployed.exists() {
            return deployed;
        }
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sidecar").join("publish").join("daisyloop-lhm.exe")
    }
    pub fn mutex_name(&self) -> String {
        let dir = std::fs::canonicalize(&self.data_dir).unwrap_or_else(|_| self.data_dir.clone());
        let h = dir.to_string_lossy().to_lowercase().replace('/', "\\").trim_end_matches('\\').bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
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
    #[serde(deserialize_with = "nan_from_null")]
    pub value: f64,
}

fn nan_from_null<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::NAN))
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
    seen: bool,
}

type Factory = fn(&Config) -> Box<dyn Source>;
type Reply = (usize, u64, Result<Sample, String>);

struct Collector {
    workers: Vec<Worker>,
    results: Receiver<Reply>,
    seq: u64,
    hot: HotStore,
    idle: Arc<Idle>,
    gate_first: bool,
    committer: Committer,
    alive: Arc<AtomicU64>,
    cfg: Config,
    control: Control,
    hub: Arc<Mutex<Hub>>,
    jobs: Receiver<Job>,
    start: Instant,
    wal_full: bool,
}

impl Collector {
    fn new(cfg: &Config) -> Result<Collector, Box<dyn std::error::Error>> {
        Legacy::seal_leftover(&cfg.data_dir)?;
        let hot = HotStore::open(&cfg.data_dir, Duration::from_secs(cfg.wal_sync_secs))?;
        HotStore::trim_parked(&cfg.data_dir, cfg.parked_max_bytes);
        let dirs = XgbModel::search_dirs(&cfg.data_dir, cfg.install_dir.as_deref());
        let model = XgbModel::load(&dirs, &cfg.control.model_spec, &cfg.control.model_file);
        if let Err(e) = &model {
            log::error!("model not loaded: {e}");
        }
        let history = model.as_ref().map_or_else(|_| Vec::new(), |m| HotStore::recent(&cfg.data_dir, &|c| m.wants(c), cfg.control.seed_secs));
        let model = model.map(|m| Box::new(m) as Box<dyn Model>);
        let lake = Lake::open(cfg)?;
        let idle = Arc::new(Idle::new(&cfg.gate));
        let gate = Gate::new(cfg, idle.clone());
        let gate_first = gate.first_pending();
        let committer = Committer::spawn(lake, cfg.data_dir.clone(), Duration::from_secs(60), gate);
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
                Worker { name, tx, busy_since: None, failing: false, seen: false }
            })
            .collect();
        let hid_cfg = cfg.clone();
        let actuator = D5Actuator::new(Box::new(move || HidControl::open(&hid_cfg).map(|h| Box::new(h) as Box<dyn D5Control>)), &cfg.control);
        let mut control = Control::new(cfg, Box::new(actuator), model);
        control.seed(&history, now_micros() / 1_000_000);
        let hub = Arc::new(Mutex::new(Hub::new(&cfg.control, control.status(0))));
        let (jobs_tx, jobs) = channel();
        if cfg.ipc_enabled {
            Ipc::spawn(hub.clone(), jobs_tx);
        }
        Ok(Collector { workers, results, seq: 0, hot, idle, gate_first, committer, alive: Arc::new(AtomicU64::new(0)), cfg: cfg.clone(), control, hub, jobs, start: Instant::now(), wal_full: false })
    }

    fn accept(&mut self, i: usize, r: Result<Sample, String>, tick: &mut Tick) {
        let w = &mut self.workers[i];
        match r {
            Ok(sample) => {
                if !w.seen {
                    log::info!("{} first data", w.name);
                    w.seen = true;
                }
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
                    let now = self.start.elapsed().as_millis() as i64;
                    self.control.shutdown(now);
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
        self.control_step(&mut tick);
        if self.seq % 60 == 1 {
            HotStore::trim_parked(&self.cfg.data_dir, self.cfg.parked_max_bytes);
            let used = Legacy::backlog_bytes(&self.cfg.data_dir) + self.hot.live_bytes();
            let full = used >= self.cfg.wal_max_bytes;
            if full {
                log::error!("hot store backlog {used} bytes reached wal_max_bytes {}, new ticks are DROPPED until the commit backlog drains", self.cfg.wal_max_bytes);
            } else if self.wal_full {
                log::warn!("hot store backlog drained to {used} bytes, recording again");
            }
            self.wal_full = full;
        }
        self.idle.observe(&tick.rows);
        if !self.wal_full && (!tick.rows.is_empty() || tick.raw.is_some()) {
            if let Err(e) = self.hot.append(&tick) {
                log::error!("hot append failed: {e}");
            }
        }
    }

    fn mono(&self) -> i64 {
        self.start.elapsed().as_millis() as i64
    }

    fn control_step(&mut self, tick: &mut Tick) {
        let now = self.mono();
        for (cmd, reply) in self.jobs.try_iter().take(16).collect::<Vec<_>>() {
            let _ = reply.send(ipc::apply(&mut self.control, cmd, now));
        }
        self.control.step(&mut tick.rows, now, tick.ts / 1_000_000);
        self.hub.lock().unwrap().update(tick, self.control.status(now));
    }

    fn rotate(&mut self) {
        match self.hot.rotate() {
            Ok(true) => self.committer.notify(),
            Ok(false) => {}
            Err(e) => log::error!("hot rotate failed: {e}"),
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
        let mut next_commit = Instant::now() + if self.gate_first { commit_every.min(Duration::from_secs(60)) } else { commit_every };
        self.committer.notify();
        loop {
            self.tick();
            self.alive.fetch_add(1, Ordering::Relaxed);
            let _ = std::fs::write(&heartbeat, now_micros().to_string());
            if !self.committer.alive() {
                log::error!("commit thread died, exiting");
                let now = self.mono();
                self.control.shutdown(now);
                std::process::exit(4);
            }
            if stop_file.exists() {
                let _ = std::fs::remove_file(&stop_file);
                let now = self.mono();
                self.control.shutdown(now);
                if let Err(e) = self.hot.rotate() {
                    log::error!("hot rotate failed: {e}");
                }
                self.committer.finish();
                log::info!("stopped");
                return;
            }
            if Instant::now() >= next_commit {
                self.rotate();
                next_commit = Instant::now() + commit_every;
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

fn heartbeat_age(path: &Path) -> String {
    match std::fs::metadata(path).and_then(|m| m.modified()).map(|t| t.elapsed()) {
        Ok(Ok(age)) => format!("{}s", age.as_secs()),
        Ok(Err(_)) => "in the future".into(),
        Err(_) => "none".into(),
    }
}

fn main() {
    let Ok(cfg) = Config::load() else { return };
    let _ = std::fs::create_dir_all(&cfg.data_dir);
    let _ = std::fs::create_dir_all(cfg.state_dir());
    if !single_instance(&cfg.mutex_name()) {
        return;
    }
    init_log(&cfg);
    log::info!("previous heartbeat age {}", heartbeat_age(&cfg.data_dir.join("heartbeat")));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(files: Vec<(&'static str, &'static str)>) -> impl Fn(&Path) -> Result<toml::Table, String> {
        move |p: &Path| files.iter().find(|(k, _)| Config::normal(Path::new(k)) == Config::normal(p)).map_or(Ok(toml::Table::new()), |(_, v)| v.parse::<toml::Table>().map_err(|e| e.to_string()))
    }

    const INSTALL: &str = r"C:\Program Files\daisyloop";
    const DATA: &str = r"C:\Local\daisyloop";
    const ADMIN_FILE: &str = r"C:\Program Files\daisyloop\config.toml";
    const USER_FILE: &str = r"C:\Local\daisyloop\config.toml";

    #[test]
    fn installed_ignores_security_keys_from_the_user_config() {
        let admin = "data_dir = 'C:\\Local\\daisyloop'\nwrites_enabled = true\n[control.fan]\nfloor_duty = 35.0\n";
        let user = "keep_versions = 3\nwrites_enabled = false\ndata_dir = 'C:\\evil'\nsidecar = 'C:/evil.exe'\n[control.fan]\nautostart_active = true\n";
        let cfg = Config::resolve(Some(Path::new(INSTALL)), Some(PathBuf::from(r"C:\env")), Some(PathBuf::from(r"C:\Users\SYSTEM")), &reader(vec![(ADMIN_FILE, admin), (USER_FILE, user)])).unwrap();
        assert_eq!(cfg.keep_versions, 3);
        assert!(cfg.writes_enabled);
        assert!(cfg.sidecar.is_none() && cfg.control.fan.autostart_active.is_none());
        assert_eq!(cfg.control.fan.floor_duty, Some(35.0));
        assert_eq!(cfg.data_dir, PathBuf::from(DATA));
        assert_eq!(cfg.state_dir(), Path::new(INSTALL).join("state"));
        assert!(cfg.install_dir.is_some());
    }

    #[test]
    fn installed_never_falls_back_when_the_admin_config_is_broken_or_lacks_data_dir() {
        let resolve = |files| Config::resolve(Some(Path::new(INSTALL)), Some(PathBuf::from(r"C:\env")), Some(PathBuf::from(r"C:\Local")), &reader(files));
        assert!(resolve(vec![(ADMIN_FILE, "ipc_enabled = false")]).is_err());
        assert!(resolve(vec![(ADMIN_FILE, "data_dir = 'C:\\x'\nbroken ===")]).is_err());
        assert!(resolve(vec![]).is_err());
        let ok = resolve(vec![(ADMIN_FILE, "data_dir = 'C:\\x'"), (USER_FILE, "broken ===")]).unwrap();
        assert_eq!(ok.data_dir, PathBuf::from(r"C:\x"));
    }

    #[test]
    fn the_timebase_is_an_admin_key() {
        assert!(Config::ADMIN_KEYS.contains(&"interval_secs"));
    }

    #[test]
    fn wal_sync_defaults_to_one_second_and_is_configurable() {
        assert_eq!(Config::default().wal_sync_secs, 1);
        assert!(Config::from_tables("wal_sync_secs = 5".parse().unwrap(), None).wal_sync_secs == 5);
    }

    #[test]
    fn installed_sidecar_is_always_the_deployed_one() {
        let cfg = Config { install_dir: Some(PathBuf::from(INSTALL)), sidecar: Some(PathBuf::from(r"C:\evil.exe")), ..Config::default() };
        let p = cfg.sidecar_exe();
        assert!(p.ends_with(r"sidecar\daisyloop-lhm.exe") && !p.to_string_lossy().contains("evil") && !p.to_string_lossy().contains("publish"));
    }

    #[test]
    fn table_separates_missing_from_unparsable() {
        let dir = std::env::temp_dir().join(format!("dl-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(Config::table(&dir.join("none.toml")).unwrap().is_empty());
        std::fs::write(dir.join("bad.toml"), "x = = 1").unwrap();
        assert!(Config::table(&dir.join("bad.toml")).is_err());
        std::fs::write(dir.join("ok.toml"), "x = 1").unwrap();
        assert_eq!(Config::table(&dir.join("ok.toml")).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dev_run_uses_env_or_localappdata_and_keeps_everything_from_the_user_config() {
        let files = vec![(r"C:\env\config.toml", "writes_enabled = true\nsidecar = 'C:/x.exe'"), (USER_FILE, "keep_versions = 7")];
        let env = Config::resolve(None, Some(PathBuf::from(r"C:\env")), Some(PathBuf::from(r"C:\Local")), &reader(files.clone())).unwrap();
        assert_eq!(env.data_dir, PathBuf::from(r"C:\env"));
        assert!(env.writes_enabled && env.sidecar.is_some() && env.install_dir.is_none());
        assert_eq!(env.state_dir(), PathBuf::from(r"C:\env"));
        let local = Config::resolve(None, None, Some(PathBuf::from(r"C:\Local")), &reader(files)).unwrap();
        assert_eq!(local.data_dir, PathBuf::from(DATA));
        assert_eq!(local.keep_versions, 7);
    }

    #[test]
    fn installed_means_exactly_the_program_files_dir() {
        let pf = Path::new(INSTALL);
        assert!(Config::is_install_dir(Path::new(r"c:\program files\DAISYLOOP\"), pf));
        assert!(Config::is_install_dir(Path::new("C:/Program Files/daisyloop"), pf));
        for other in [r"C:\Program Files\daisyloop\bin", r"C:\Program Files", r"C:\Local\daisyloop\bin", r"C:\Program Files\daisyloop2", r"D:\Program Files\daisyloop"] {
            assert!(!Config::is_install_dir(Path::new(other), pf), "{other}");
        }
    }

    #[test]
    fn mutex_name_is_derived_from_the_data_dir() {
        let a = Config { data_dir: PathBuf::from(r"C:\Temp\X"), ..Config::default() };
        let b = Config { data_dir: PathBuf::from(r"c:/temp/x/"), ..Config::default() };
        let c = Config { data_dir: PathBuf::from(r"C:\Temp\Y"), ..Config::default() };
        assert_eq!(a.mutex_name(), b.mutex_name());
        assert_ne!(a.mutex_name(), c.mutex_name());
    }
}
