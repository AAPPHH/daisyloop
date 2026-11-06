use d5_probe::{AbortConfig, Hid, Reopening};
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
    let program = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files")).join("daisyloop").join("config.toml");
    let stop = AbortConfig::stop_path(&program, std::env::var_os("DAISYLOOP_DATA_DIR").map(PathBuf::from), std::env::var_os("LOCALAPPDATA").map(PathBuf::from));
    let abort_cfg = AbortConfig::d5_next(stop);
    let mut open = || Device::open(&cfg, abort_cfg.len, abort_cfg.id).map(|d| Box::new(d) as Box<dyn Hid>);
    let (result, log) = Reopening::run(&abort_cfg, &mut open, &mut |ms| std::thread::sleep(Duration::from_millis(ms)));
    log.iter().for_each(|l| println!("{l}"));
    match result {
        Ok(()) => println!("OK fan and pump are on the firmware PID"),
        Err(e) => {
            println!("FAILED {e}");
            std::process::exit(1);
        }
    }
}
