#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use serde::Deserialize;
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

fn main() {
    let cfg = Config::load();
    let _ = std::fs::create_dir_all(&cfg.data_dir);
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(cfg.data_dir.join("collector.log")) {
        let lc = ConfigBuilder::new().set_time_format_rfc3339().set_time_offset_to_local().unwrap_or(&mut ConfigBuilder::new()).build();
        let _ = WriteLogger::init(LevelFilter::Info, lc, f);
    }
    std::panic::set_hook(Box::new(|i| log::error!("panic: {i}")));
    let _ = (Instant::now(), Duration::ZERO);
}
