use d5_probe::{Abort, AbortConfig, Hid};
use hidapi::{HidApi, HidDevice};
use std::path::PathBuf;
use std::time::Duration;

struct Config {
    vid: u16,
    pid: u16,
    usage_page: u16,
}

struct Device {
    dev: HidDevice,
    len: usize,
    id: u8,
}

impl Device {
    fn open(cfg: &Config, len: usize, id: u8) -> Result<Device, String> {
        let api = HidApi::new().map_err(|e| e.to_string())?;
        let info = api
            .device_list()
            .find(|d| d.vendor_id() == cfg.vid && d.product_id() == cfg.pid && d.usage_page() == cfg.usage_page)
            .ok_or("D5 Next interface not found")?;
        Ok(Device { dev: info.open_device(&api).map_err(|e| e.to_string())?, len, id })
    }
}

impl Hid for Device {
    fn get(&mut self) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; self.len];
        buf[0] = self.id;
        let n = self.dev.get_feature_report(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(n);
        Ok(buf)
    }
    fn set(&mut self, report: &[u8]) -> Result<(), String> {
        if report.len() != self.len || report[0] != self.id {
            return Err("refused: only the 809 byte report 0x03 may be sent".into());
        }
        self.dev.send_feature_report(report).map_err(|e| e.to_string())
    }
    fn pause(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

fn main() {
    let cfg = Config { vid: 0x0C70, pid: 0xF00E, usage_page: 0xFF00 };
    let stop = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("daisyloop").join("stop"));
    let abort_cfg = AbortConfig::d5_next(stop);
    let mut result = Err("device not opened".to_string());
    for i in 0..abort_cfg.attempts {
        match Device::open(&cfg, abort_cfg.len, abort_cfg.id) {
            Ok(dev) => {
                let mut abort = Abort::new(Box::new(dev), abort_cfg.clone());
                result = abort.run();
                abort.log.iter().for_each(|l| println!("{l}"));
                break;
            }
            Err(e) => {
                println!("open attempt {} failed: {e}", i + 1);
                result = Err(e);
                std::thread::sleep(Duration::from_millis(abort_cfg.retry_ms));
            }
        }
    }
    match result {
        Ok(()) => println!("OK fan and pump are on the firmware PID"),
        Err(e) => {
            println!("FAILED {e}");
            std::process::exit(1);
        }
    }
}
