use d5_probe::Crc16Usb;
use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Config {
    vid: u16,
    pid: u16,
    usage_page: u16,
    report_id: u8,
    len: usize,
    fan: std::ops::Range<usize>,
    pump: std::ops::Range<usize>,
    fan_setpoint_at: usize,
    pump_setpoint_at: usize,
    pump_setpoint_min: u16,
    setpoint_range: (u16, u16),
    baseline_secs: u64,
    pump_rpm_tolerance: f64,
    pump_rpm_floor: f64,
    water_max: f64,
    out_dir: &'static str,
}

const CFG: Config = Config {
    vid: 0x0C70,
    pid: 0xF00E,
    usage_page: 0xFF00,
    report_id: 0x03,
    len: 0x329,
    fan: 0x41..0x96,
    pump: 0x96..0xEB,
    fan_setpoint_at: 0x46,
    pump_setpoint_at: 0x9B,
    pump_setpoint_min: 2800,
    setpoint_range: (2800, 3800),
    baseline_secs: 30,
    pump_rpm_tolerance: 150.0,
    pump_rpm_floor: 1800.0,
    water_max: 40.0,
    out_dir: "snapshot/raw",
};

#[derive(Clone, Copy)]
struct Live {
    water: f64,
    pump_rpm: f64,
    pump_duty: f64,
    fan_rpm: f64,
    fan_duty: f64,
}

impl Live {
    fn be16(r: &[u8], o: usize) -> f64 {
        u16::from_be_bytes([r[o], r[o + 1]]) as f64
    }
    fn parse(r: &[u8]) -> Live {
        Live {
            water: Self::be16(r, 0x57) / 100.0,
            pump_rpm: Self::be16(r, 0x74),
            pump_duty: Self::be16(r, 0x6C) / 100.0,
            fan_rpm: Self::be16(r, 0x67),
            fan_duty: Self::be16(r, 0x5F) / 100.0,
        }
    }
    fn print(&self, tag: &str) {
        println!("{tag} water {:.2} pump {:.0} rpm {:.1} % fan {:.0} rpm {:.1} %", self.water, self.pump_rpm, self.pump_duty, self.fan_rpm, self.fan_duty);
    }
}

struct D5 {
    dev: HidDevice,
}

impl D5 {
    fn open() -> Result<D5, String> {
        let api = HidApi::new().map_err(|e| e.to_string())?;
        let info = api.device_list().find(|d| d.vendor_id() == CFG.vid && d.product_id() == CFG.pid && d.usage_page() == CFG.usage_page).ok_or("D5 not found")?;
        Ok(D5 { dev: info.open_device(&api).map_err(|e| e.to_string())? })
    }
    fn get(&self) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; CFG.len];
        buf[0] = CFG.report_id;
        let n = self.dev.get_feature_report(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(n);
        if n != CFG.len || buf[0] != CFG.report_id {
            return Err(format!("bad report len {n} id {}", buf[0]));
        }
        if Crc16Usb::stored(&buf) != Crc16Usb::of_report(&buf) {
            return Err("crc mismatch on device report".into());
        }
        Ok(buf)
    }
    fn set(&self, r: &[u8]) -> Result<(), String> {
        std::thread::sleep(Duration::from_millis(200));
        self.dev.send_feature_report(r).map_err(|e| e.to_string())
    }
    fn live(&self) -> Result<Live, String> {
        let mut buf = [0u8; 512];
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            let n = self.dev.read_timeout(&mut buf, 1100).map_err(|e| e.to_string())?;
            if n >= 0x9E && buf[0] == 1 {
                return Ok(Live::parse(&buf[..n]));
            }
        }
        Err("no input report".into())
    }
}

struct Stage {
    d5: D5,
    send: bool,
    setpoint: Option<u16>,
    pump_setpoint: Option<u16>,
    hold_secs: u64,
    restore: bool,
}

impl Stage {
    fn env(k: &str) -> Option<String> {
        std::env::var(k).ok()
    }
    fn from_env() -> Result<Stage, String> {
        Ok(Stage {
            d5: D5::open()?,
            send: Self::env("D5_SEND").as_deref() == Some("YES"),
            setpoint: Self::env("D5_FAN_SETPOINT").map(|s| s.parse().map_err(|_| "bad setpoint")).transpose()?,
            pump_setpoint: Self::env("D5_PUMP_SETPOINT").map(|s| s.parse().map_err(|_| "bad pump setpoint")).transpose()?,
            hold_secs: Self::env("D5_HOLD").and_then(|s| s.parse().ok()).unwrap_or(60),
            restore: Self::env("D5_RESTORE").as_deref() != Some("NO"),
        })
    }
    fn target(&self, base: &[u8]) -> Result<Vec<u8>, String> {
        let mut t = base.to_vec();
        if let Some(sp) = self.setpoint {
            if sp < CFG.setpoint_range.0 || sp > CFG.setpoint_range.1 {
                return Err(format!("setpoint {sp} outside {:?}", CFG.setpoint_range));
            }
            t[CFG.fan_setpoint_at..CFG.fan_setpoint_at + 2].copy_from_slice(&sp.to_be_bytes());
            Crc16Usb::seal(&mut t);
        }
        if let Some(sp) = self.pump_setpoint {
            if self.setpoint.is_some() {
                return Err("refused: one channel per run".into());
            }
            let now = Live::be16(base, CFG.pump_setpoint_at) as u16;
            if sp > now || sp < CFG.pump_setpoint_min {
                return Err(format!("refused: pump set-point {sp} must stay within {}..={now}", CFG.pump_setpoint_min));
            }
            t[CFG.pump_setpoint_at..CFG.pump_setpoint_at + 2].copy_from_slice(&sp.to_be_bytes());
            Crc16Usb::seal(&mut t);
        }
        let allowed = if self.pump_setpoint.is_some() { CFG.pump_setpoint_at..CFG.pump_setpoint_at + 2 } else { CFG.fan.clone() };
        let n = t.len();
        let diffs: Vec<usize> = (0..n).filter(|&o| t[o] != base[o]).collect();
        for &o in &diffs {
            println!("diff {o:#05X}: {:02X} -> {:02X}", base[o], t[o]);
            if !allowed.contains(&o) && o < n - 2 {
                return Err(format!("refused: byte {o:#05X} outside {allowed:?}"));
            }
        }
        println!("diff bytes: {}", diffs.len());
        Ok(t)
    }
    fn baseline(&self) -> Result<Live, String> {
        let mut rows = Vec::new();
        for _ in 0..CFG.baseline_secs {
            rows.push(self.d5.live()?);
        }
        let last = rows[rows.len() - 1];
        let rpm: Vec<f64> = rows.iter().map(|l| l.pump_rpm).collect();
        let (lo, hi) = rpm.iter().fold((f64::MAX, f64::MIN), |a, &v| (a.0.min(v), a.1.max(v)));
        last.print("baseline");
        println!("baseline pump rpm spread {:.0}..{:.0}", lo, hi);
        if hi - lo > CFG.pump_rpm_tolerance / 2.0 {
            return Err("pump rpm not stable enough for a clean test".into());
        }
        if last.water >= CFG.water_max {
            return Err(format!("water {:.2} at or above {}", last.water, CFG.water_max));
        }
        Ok(last)
    }
    fn watch(&self, secs: u64, base: Live, expect: &[u8]) -> Result<Live, String> {
        let start = Instant::now();
        let mut last = base;
        let mut checks = vec![1u64, 2, 10];
        while start.elapsed().as_secs() < secs {
            let l = self.d5.live()?;
            last = l;
            let t = start.elapsed().as_secs();
            l.print(&format!("t+{t:>3}s"));
            if self.pump_setpoint.is_none() && (l.pump_rpm - base.pump_rpm).abs() > CFG.pump_rpm_tolerance {
                return Err(format!("pump rpm {:.0} deviates from {:.0}", l.pump_rpm, base.pump_rpm));
            }
            if l.pump_rpm < CFG.pump_rpm_floor {
                return Err(format!("pump rpm {:.0} below floor", l.pump_rpm));
            }
            if l.water >= CFG.water_max {
                return Err(format!("water {:.2} at or above {}", l.water, CFG.water_max));
            }
            if checks.first().is_some_and(|&c| t >= c) {
                checks.remove(0);
                if self.d5.get()? != expect {
                    return Err("readback differs from what was written".into());
                }
                println!("t+{t:>3}s readback identical");
            }
        }
        Ok(last)
    }
    fn restore_to(&self, base: &[u8]) -> Result<(), String> {
        self.d5.set(base)?;
        std::thread::sleep(Duration::from_millis(500));
        if self.d5.get()? != base {
            return Err("RESTORE READBACK DIFFERS".into());
        }
        println!("restored and verified");
        Ok(())
    }
    fn run(&self) -> Result<(), String> {
        let base = self.d5.get()?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
        let path = format!("{}/d5_pre_{stamp}.bin", CFG.out_dir);
        std::fs::write(&path, &base).map_err(|e| e.to_string())?;
        println!("saved {path}");
        println!("fan set-point now {} pump set-point now {}", Live::be16(&base, CFG.fan_setpoint_at) / 100.0, Live::be16(&base, CFG.pump.start + 5) / 100.0);
        let target = self.target(&base)?;
        let before = self.baseline()?;
        if !self.send {
            println!("dry run, nothing sent");
            return Ok(());
        }
        if self.d5.get()? != base {
            return Err("report changed during baseline, nothing sent".into());
        }
        self.d5.set(&target)?;
        println!("sent");
        match self.watch(self.hold_secs, before, &target) {
            Ok(after) => {
                after.print("end");
                if self.restore && target != base {
                    self.restore_to(&base)?;
                    self.watch(30, before, &base)?.print("after restore");
                }
                Ok(())
            }
            Err(e) => {
                println!("ABORT: {e}");
                if target != base {
                    self.restore_to(&base)?;
                }
                Err(e)
            }
        }
    }
}

fn main() {
    let r = Stage::from_env().and_then(|s| s.run());
    if let Err(e) = r {
        println!("FAILED: {e}");
        std::process::exit(1);
    }
}
