use crate::{Config, Row, Sample};
use hidapi::{HidApi, HidDevice};
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
use nvml_wrapper::Nvml;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub trait Source {
    fn poll(&mut self) -> Result<Sample, String>;
}

pub struct D5Next {
    cfg: Config,
    dev: Option<HidDevice>,
    misses: u32,
}

impl D5Next {
    const LEN: usize = 0x9E;
    const DISCONNECTED: u16 = 0x7FFF;
    pub fn new(cfg: &Config) -> D5Next {
        D5Next { cfg: cfg.clone(), dev: None, misses: 0 }
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
    fn poll(&mut self) -> Result<Sample, String> {
        if self.dev.is_none() {
            self.dev = Some(self.open()?);
        }
        match Self::newest(self.dev.as_ref().unwrap()) {
            Ok(Some(b)) => {
                self.misses = 0;
                Ok(Sample { rows: Self::decode(&b), raw: Some(b) })
            }
            Ok(None) => {
                self.misses += 1;
                if self.misses >= 5 {
                    self.dev = None;
                    self.misses = 0;
                }
                Err("no D5 Next report received".into())
            }
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
            last.role = "aux".into();
        }
        Ok(rows)
    }
}

impl Source for Nvidia {
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

struct Latest {
    at: Instant,
    rows: Vec<Row>,
}

pub struct Lhm {
    exe: PathBuf,
    child: Option<Child>,
    latest: Arc<Mutex<Option<Latest>>>,
    spawned: Option<Instant>,
    stale: Duration,
}

impl Lhm {
    pub fn new(cfg: &Config) -> Lhm {
        Lhm { exe: cfg.sidecar_exe(), child: None, latest: Arc::new(Mutex::new(None)), spawned: None, stale: Duration::from_secs(cfg.sidecar_stale_secs) }
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
        s.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string()
    }
    pub fn parse(line: &str) -> Option<Vec<Row>> {
        let json: Value = serde_json::from_str(line).ok()?;
        let rows = json["sensors"]
            .as_array()?
            .iter()
            .filter_map(|s| {
                let unit = s["unit"].as_str().unwrap_or("");
                let kind = Self::kind(s["type"].as_str()?, unit);
                Some(Row::new("lhm", s["id"].as_str()?, Self::clean(&format!("{} {}", s["hw"].as_str()?, s["name"].as_str()?)), kind, unit, s["value"].as_f64()?))
            })
            .collect();
        Some(rows)
    }
    fn spawn(&mut self) -> Result<(), String> {
        let mut child = Command::new(&self.exe).stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).creation_flags(0x0800_0000).spawn().map_err(|e| format!("{}: {e}", self.exe.display()))?;
        KillOnExit::assign(&child);
        let out = child.stdout.take().ok_or("no sidecar stdout")?;
        let latest = self.latest.clone();
        *latest.lock().unwrap() = None;
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if let Some(rows) = Self::parse(&line) {
                    *latest.lock().unwrap() = Some(Latest { at: Instant::now(), rows });
                }
            }
        });
        self.child = Some(child);
        self.spawned = Some(Instant::now());
        Ok(())
    }
    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
    fn healthy(&mut self) -> bool {
        let Some(child) = self.child.as_mut() else { return false };
        if !matches!(child.try_wait(), Ok(None)) {
            return false;
        }
        match self.latest.lock().unwrap().as_ref() {
            Some(l) => l.at.elapsed() < self.stale,
            None => self.spawned.is_some_and(|s| s.elapsed() < self.stale * 3),
        }
    }
}

impl Source for Lhm {
    fn poll(&mut self) -> Result<Sample, String> {
        if !self.healthy() {
            self.stop();
            if self.spawned.is_some_and(|s| s.elapsed() < Duration::from_secs(5)) {
                return Err("sidecar restarting".into());
            }
            self.spawn()?;
            return Err("sidecar started".into());
        }
        match self.latest.lock().unwrap().as_ref() {
            Some(l) if l.at.elapsed() < Duration::from_secs(3) => Ok(Sample { rows: l.rows.clone(), raw: None }),
            _ => Err("no fresh sidecar data".into()),
        }
    }
}

struct KillOnExit;

impl KillOnExit {
    fn job() -> isize {
        static JOB: OnceLock<isize> = OnceLock::new();
        *JOB.get_or_init(|| unsafe {
            use windows_sys::Win32::System::JobObjects::*;
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32);
            job as isize
        })
    }
    fn assign(child: &Child) {
        unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(Self::job() as _, child.as_raw_handle() as _);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_line() {
        let line = r#"{"seq":3,"sensors":[{"id":"/intelcpu/0/load/0","hw":"Intel Core","name":"CPU Total","type":"Load","unit":"%","value":43.5},{"id":"/x/temperature/0","hw":"H","name":"T","type":"Temperature","unit":"°C","value":40},{"id":"bad","hw":"H"}]}"#;
        let rows = Lhm::parse(line).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].channel.as_str(), rows[0].label.as_str(), rows[0].kind.as_str(), rows[0].role.as_str(), rows[0].value), ("/intelcpu/0/load/0", "Intel Core CPU Total", "load", "disturbance", 43.5));
        assert_eq!((rows[1].kind.as_str(), rows[1].role.as_str()), ("temperature", "state"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_line() {
        let rows = Lhm::parse(r#"{"seq":1,"sensors":[{"id":"/intelcpu/0/temperature/0","hw":"CPU","name":"Core","type":"Temperature","unit":"°C","value":41.5},{"id":"/x/fan/0","hw":"B","name":"F","type":"Fan","unit":"RPM","value":700}]}"#).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].kind.as_str(), rows[0].role.as_str(), rows[0].label.as_str()), ("temperature", "state", "CPU Core"));
        assert_eq!(rows[1].kind, "rpm");
    }
}
