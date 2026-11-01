#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod lake;
mod sources;
use lake::Lake;
use serde::Deserialize;
use sources::{D5Next, Lhm, Nvidia, Source};
use simplelog::{ConfigBuilder, LevelFilter, WriteLogger};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub interval_secs: f64,
    pub commit_secs: u64,
    pub data_dir: PathBuf,
    pub lhm_url: String,
    pub lhm_timeout_ms: u64,
    pub d5_vid: u16,
    pub d5_pid: u16,
    pub d5_usage_page: u16,
}

impl Default for Config {
    fn default() -> Self {
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        Config {
            interval_secs: 1.0,
            commit_secs: 60,
            data_dir: base.join("daisyloop"),
            lhm_url: "http://127.0.0.1:8085/data.json".into(),
            lhm_timeout_ms: 300,
            d5_vid: 0x0C70,
            d5_pid: 0xF00E,
            d5_usage_page: 0xFF00,
        }
    }
}

impl Config {
    pub fn load() -> Config {
        let dflt = Config::default();
        let text = std::fs::read_to_string(dflt.data_dir.join("config.toml")).unwrap_or_default();
        toml::from_str(&text).unwrap_or(dflt)
    }
    pub fn catalog_db(&self) -> PathBuf {
        self.data_dir.join("catalog.db")
    }
    pub fn warehouse(&self) -> PathBuf {
        self.data_dir.join("warehouse")
    }
}

pub struct Row {
    pub source: &'static str,
    pub channel: String,
    pub label: String,
    pub kind: &'static str,
    pub unit: String,
    pub role: &'static str,
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
        Row { source, channel: channel.into(), label: label.into(), kind, unit: unit.into(), role, value }
    }
    pub fn role(mut self, role: &'static str) -> Row {
        self.role = role;
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

struct Slot {
    src: Box<dyn Source>,
    failing: bool,
}

struct Collector {
    slots: Vec<Slot>,
    lake: Lake,
    interval: Duration,
    commit_every: Duration,
    stop_file: PathBuf,
}

impl Collector {
    fn new(cfg: &Config) -> Result<Collector, Box<dyn std::error::Error>> {
        let srcs: Vec<Box<dyn Source>> = vec![Box::new(D5Next::new(cfg)), Box::new(Nvidia::new()), Box::new(Lhm::new(cfg))];
        Ok(Collector {
            slots: srcs.into_iter().map(|src| Slot { src, failing: false }).collect(),
            lake: Lake::open(cfg)?,
            interval: Duration::from_secs_f64(cfg.interval_secs),
            commit_every: Duration::from_secs(cfg.commit_secs),
            stop_file: cfg.data_dir.join("stop"),
        })
    }

    fn tick(&mut self) {
        let ts = now_micros();
        for s in &mut self.slots {
            match s.src.poll() {
                Ok(sample) => {
                    if s.failing {
                        log::info!("{} recovered", s.src.name());
                        s.failing = false;
                    }
                    self.lake.push(ts, sample.rows, sample.raw);
                }
                Err(e) => {
                    if !s.failing {
                        log::warn!("{} unavailable: {e}", s.src.name());
                        s.failing = true;
                    }
                }
            }
        }
    }

    fn commit(&mut self) {
        match self.lake.commit() {
            Ok((a, b)) if a + b > 0 => log::info!("committed {a} telemetry rows, {b} raw reports"),
            Ok(_) => {}
            Err(e) => log::error!("commit failed ({} rows buffered): {e}", self.lake.pending()),
        }
    }

    fn run(&mut self) {
        let mut next = Instant::now();
        let mut last_commit = Instant::now();
        loop {
            self.tick();
            if self.stop_file.exists() {
                let _ = std::fs::remove_file(&self.stop_file);
                self.commit();
                log::info!("stopped");
                return;
            }
            if last_commit.elapsed() >= self.commit_every {
                self.commit();
                last_commit = Instant::now();
            }
            next += self.interval;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > self.interval * 5 {
                next = now;
            }
        }
    }
}

fn single_instance() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = r"Local\daisyloop-collector".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        !h.is_null() && GetLastError() != ERROR_ALREADY_EXISTS
    }
}

fn main() {
    let cfg = Config::load();
    let _ = std::fs::create_dir_all(&cfg.data_dir);
    if !single_instance() {
        return;
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(cfg.data_dir.join("collector.log")) {
        let mut b = ConfigBuilder::new();
        b.set_time_format_rfc3339();
        let _ = b.set_time_offset_to_local();
        let _ = WriteLogger::init(LevelFilter::Info, b.build(), f);
    }
    std::panic::set_hook(Box::new(|i| log::error!("panic: {i}")));
    let mut c = match Collector::new(&cfg) {
        Ok(c) => c,
        Err(e) => {
            log::error!("startup failed: {e}");
            return;
        }
    };
    log::info!("collector started, interval {}s, commit {}s", cfg.interval_secs, cfg.commit_secs);
    c.run();
}
