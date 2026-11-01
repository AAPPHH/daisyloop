use crate::{Config, Row, Sample};
use hidapi::{HidApi, HidDevice};
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
use nvml_wrapper::Nvml;
use serde_json::Value;
use std::time::Duration;

pub trait Source {
    fn name(&self) -> &'static str;
    fn poll(&mut self) -> Result<Sample, String>;
}

pub struct D5Next {
    cfg: Config,
    dev: Option<HidDevice>,
}

impl D5Next {
    const LEN: usize = 0x9E;
    const DISCONNECTED: u16 = 0x7FFF;
    pub fn new(cfg: &Config) -> D5Next {
        D5Next { cfg: cfg.clone(), dev: None }
    }
    fn open(&self) -> Result<HidDevice, String> {
        let api = HidApi::new().map_err(|e| e.to_string())?;
        let info = api
            .device_list()
            .find(|d| d.vendor_id() == self.cfg.d5_vid && d.product_id() == self.cfg.d5_pid && d.usage_page() == self.cfg.d5_usage_page)
            .ok_or("D5 Next HID interface not found")?;
        let dev = info.open_device(&api).map_err(|e| e.to_string())?;
        dev.set_blocking_mode(false).map_err(|e| e.to_string())?;
        Ok(dev)
    }
    fn newest(dev: &HidDevice) -> Result<Option<Vec<u8>>, String> {
        let mut buf = [0u8; 512];
        let mut last = None;
        let mut wait = 0;
        loop {
            let n = dev.read_timeout(&mut buf, wait).map_err(|e| e.to_string())?;
            if n == 0 {
                if last.is_some() || wait > 0 {
                    break;
                }
                wait = 1200;
                continue;
            }
            if n >= Self::LEN && buf[0] == 1 {
                last = Some(buf[..n].to_vec());
            }
            wait = 0;
        }
        Ok(last)
    }
    fn u16at(b: &[u8], o: usize) -> f64 {
        u16::from_be_bytes([b[o], b[o + 1]]) as f64
    }
    fn decode(b: &[u8]) -> Vec<Row> {
        let mut rows = Vec::new();
        let u = Self::u16at;
        let mut temps = vec![("d5next/water/temp".to_string(), "D5 Next coolant temperature".to_string(), 0x57usize)];
        for n in 0..8 {
            temps.push((format!("d5next/virt{}/temp", n + 1), format!("D5 Next virtual temperature {}", n + 1), 0x3F + 2 * n));
        }
        for (ch, label, o) in temps {
            if u(b, o) as u16 != Self::DISCONNECTED {
                rows.push(Row::new("d5next", ch, label, "temperature", "degC", u(b, o) / 100.0));
            }
        }
        rows.push(Row::new("d5next", "d5next/12v/voltage", "D5 Next +12V", "voltage", "V", u(b, 0x37) / 100.0));
        rows.push(Row::new("d5next", "d5next/5v/voltage", "D5 Next +5V", "voltage", "V", u(b, 0x39) / 100.0));
        for (name, base) in [("pump", 0x6C), ("fan", 0x5F)] {
            let ch = |s: &str| format!("d5next/{name}/{s}");
            let lb = |s: &str| format!("D5 Next {name} {s}");
            rows.push(Row::new("d5next", ch("duty"), lb("duty"), "duty", "%", u(b, base) / 100.0));
            rows.push(Row::new("d5next", ch("voltage"), lb("voltage"), "voltage", "V", u(b, base + 2) / 100.0));
            rows.push(Row::new("d5next", ch("current"), lb("current"), "current", "A", u(b, base + 4) / 1000.0));
            rows.push(Row::new("d5next", ch("power"), lb("power"), "power", "W", u(b, base + 6) / 100.0).role("aux"));
            rows.push(Row::new("d5next", ch("rpm"), lb("speed"), "rpm", "rpm", u(b, base + 8)));
        }
        rows
    }
}

impl Source for D5Next {
    fn name(&self) -> &'static str {
        "d5next"
    }
    fn poll(&mut self) -> Result<Sample, String> {
        if self.dev.is_none() {
            self.dev = Some(self.open()?);
        }
        match Self::newest(self.dev.as_ref().unwrap()) {
            Ok(Some(b)) => Ok(Sample { rows: Self::decode(&b), raw: Some(b) }),
            Ok(None) => Err("no D5 Next report received".into()),
            Err(e) => {
                self.dev = None;
                Err(e)
            }
        }
    }
}

pub struct Nvidia {
    nvml: Option<Nvml>,
}

impl Nvidia {
    pub fn new() -> Nvidia {
        Nvidia { nvml: None }
    }
    fn read(nvml: &Nvml) -> Result<Vec<Row>, String> {
        let e = |e: nvml_wrapper::error::NvmlError| e.to_string();
        let d = nvml.device_by_index(0).map_err(e)?;
        let mut rows = Vec::new();
        let mut add = |ch: &str, label: &str, kind: &'static str, unit: &str, v: f64| rows.push(Row::new("nvml", format!("nvml/0/{ch}"), format!("GPU 0 {label}"), kind, unit, v));
        add("temp", "temperature", "temperature", "degC", d.temperature(TemperatureSensor::Gpu).map_err(e)? as f64);
        add("power", "power", "power", "W", d.power_usage().map_err(e)? as f64 / 1000.0);
        let u = d.utilization_rates().map_err(e)?;
        add("util_gpu", "utilization", "load", "%", u.gpu as f64);
        add("util_mem", "memory utilization", "load", "%", u.memory as f64);
        for i in 0..d.num_fans().unwrap_or(0) {
            add(&format!("fan{i}"), &format!("fan {i} speed"), "duty", "%", d.fan_speed(i).map_err(e)? as f64);
        }
        add("clock_graphics", "graphics clock", "clock", "MHz", d.clock_info(Clock::Graphics).map_err(e)? as f64);
        add("clock_mem", "memory clock", "clock", "MHz", d.clock_info(Clock::Memory).map_err(e)? as f64);
        add("power_limit", "power limit", "power", "W", d.power_management_limit().map_err(e)? as f64 / 1000.0);
        if let Some(last) = rows.last_mut() {
            last.role = "aux";
        }
        Ok(rows)
    }
}

impl Source for Nvidia {
    fn name(&self) -> &'static str {
        "nvml"
    }
    fn poll(&mut self) -> Result<Sample, String> {
        if self.nvml.is_none() {
            self.nvml = Some(Nvml::init().map_err(|e| e.to_string())?);
        }
        match Self::read(self.nvml.as_ref().unwrap()) {
            Ok(rows) => Ok(Sample { rows, raw: None }),
            Err(e) => {
                self.nvml = None;
                Err(e)
            }
        }
    }
}

pub struct Lhm {
    url: String,
    agent: ureq::Agent,
}

impl Lhm {
    pub fn new(cfg: &Config) -> Lhm {
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_millis(cfg.lhm_timeout_ms))).build().into();
        Lhm { url: cfg.lhm_url.clone(), agent }
    }
    fn kind(t: &str, unit: &str) -> &'static str {
        match t {
            "Temperature" => "temperature",
            "Fan" => "rpm",
            "Control" => "duty",
            "Power" => "power",
            "Voltage" => "voltage",
            "Current" => "current",
            "Load" => "load",
            "Clock" => "clock",
            "Flow" => "flow",
            _ if unit.chars().count() == 2 && unit.ends_with('C') => "temperature",
            _ => "other",
        }
    }
    fn clean(s: &str) -> String {
        s.chars().filter(|c| !c.is_control()).map(|c| if c == '\u{FFFD}' { '\u{b0}' } else { c }).collect::<String>().trim().to_string()
    }
    fn number(s: &str) -> Option<(f64, String)> {
        let s = s.trim();
        let end = s.find(|c: char| !(c.is_ascii_digit() || c == ',' || c == '.' || c == '-')).unwrap_or(s.len());
        let v = s[..end].replace(',', ".").parse::<f64>().ok()?;
        Some((v, Self::clean(&s[end..])))
    }
    fn walk(node: &Value, hw: &str, depth: usize, rows: &mut Vec<Row>) {
        let text = node["Text"].as_str().unwrap_or("");
        if let Some(id) = node["SensorId"].as_str() {
            let parsed = Self::number(node["Value"].as_str().unwrap_or(""));
            let unit = parsed.as_ref().map(|p| p.1.clone()).unwrap_or_default();
            let raw = node["RawValue"].as_str().and_then(Self::number).map(|p| p.0);
            if let Some(v) = raw.or(parsed.map(|p| p.0)) {
                let kind = Self::kind(node["Type"].as_str().unwrap_or(""), &unit);
                rows.push(Row::new("lhm", id, Self::clean(&format!("{hw} {text}")), kind, unit, v));
            }
        }
        if let Some(children) = node["Children"].as_array() {
            for c in children {
                let h = if depth == 1 { c["Text"].as_str().unwrap_or("") } else { hw };
                Self::walk(c, h, depth + 1, rows);
            }
        }
    }
}

impl Source for Lhm {
    fn name(&self) -> &'static str {
        "lhm"
    }
    fn poll(&mut self) -> Result<Sample, String> {
        let body = self.agent.get(&self.url).call().map_err(|e| e.to_string())?.body_mut().read_to_string().map_err(|e| e.to_string())?;
        let json: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        Self::walk(&json, "", 0, &mut rows);
        Ok(Sample { rows, raw: None })
    }
}
