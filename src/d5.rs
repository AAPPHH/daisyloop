use crate::control::{ChannelConfig, ChannelOverride, ControlConfig};
use crate::protocol::ControlKind;
use crate::Config;
use hidapi::{HidApi, HidDevice};
use std::time::Duration;

pub struct Crc16Usb;

impl Crc16Usb {
    pub fn compute(data: &[u8]) -> u16 {
        let mut crc = 0xFFFFu16;
        for &b in data {
            crc ^= b as u16;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
            }
        }
        crc ^ 0xFFFF
    }
    pub fn stored(r: &[u8]) -> u16 {
        u16::from_be_bytes([r[r.len() - 2], r[r.len() - 1]])
    }
    pub fn of_report(r: &[u8]) -> u16 {
        Self::compute(&r[1..r.len() - 2])
    }
    pub fn seal(r: &mut [u8]) {
        let c = Self::of_report(r).to_be_bytes();
        let n = r.len();
        r[n - 2..].copy_from_slice(&c);
    }
}
pub trait D5Control {
    fn get(&mut self) -> Result<Vec<u8>, String>;
    fn set(&mut self, report: &[u8]) -> Result<(), String>;
    fn settle(&mut self, _ms: u64) {}
}

pub struct HidControl {
    dev: HidDevice,
}

impl HidControl {
    pub fn open(cfg: &Config) -> Result<HidControl, String> {
        let api = HidApi::new().map_err(|e| e.to_string())?;
        let info = api
            .device_list()
            .find(|d| d.vendor_id() == cfg.d5_vid && d.product_id() == cfg.d5_pid && d.usage_page() == cfg.d5_usage_page)
            .ok_or("D5 Next HID interface not found")?;
        Ok(HidControl { dev: info.open_device(&api).map_err(|e| e.to_string())? })
    }
}

impl D5Control for HidControl {
    fn get(&mut self) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; Guard::LEN];
        buf[0] = Guard::ID;
        let n = self.dev.get_feature_report(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(n);
        Ok(buf)
    }
    fn set(&mut self, report: &[u8]) -> Result<(), String> {
        if report.len() != Guard::LEN || report[0] != Guard::ID {
            return Err("refused: only the 809 byte report 0x03 may be sent".into());
        }
        std::thread::sleep(Duration::from_millis(200));
        self.dev.send_feature_report(report).map_err(|e| e.to_string())
    }
    fn settle(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

pub fn be16(r: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([r[at], r[at + 1]])
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub kind: ControlKind,
    pub name: &'static str,
    pub base: usize,
}

impl Spec {
    pub const FAN: Spec = Spec { kind: ControlKind::Fan, name: "fan", base: 0x41 };
    pub const PUMP: Spec = Spec { kind: ControlKind::Pump, name: "pump", base: 0x96 };
    pub const ALL: [Spec; 2] = [Spec::FAN, Spec::PUMP];
    pub fn of(kind: ControlKind) -> Spec {
        Spec::ALL[Spec::ALL.iter().position(|s| s.kind == kind).unwrap()]
    }
    pub fn mode(&self) -> usize {
        self.base
    }
    pub fn preset(&self) -> usize {
        self.base + 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Setting {
    Firmware,
    Duty(f64),
}

pub trait Actuator {
    fn apply(&mut self, set: &[(ControlKind, Setting)]) -> Result<(), String>;
    fn recover(&mut self) -> Result<Vec<ControlKind>, String>;
}

pub struct Guard {
    dev: Box<dyn D5Control>,
}

impl Guard {
    pub const LEN: usize = 0x329;
    pub const ID: u8 = 3;
    pub const CRC: usize = 0x327;
    pub const READBACK_MS: u64 = 500;

    pub fn new(dev: Box<dyn D5Control>) -> Guard {
        Guard { dev }
    }
    pub fn validate(r: &[u8]) -> Result<(), String> {
        if r.len() != Self::LEN || r[0] != Self::ID {
            return Err(format!("bad report length {} id {}", r.len(), r.first().copied().unwrap_or(0)));
        }
        if Crc16Usb::stored(r) != Crc16Usb::of_report(r) {
            return Err("crc mismatch on device report".into());
        }
        Ok(())
    }
    pub fn check_diff(fresh: &[u8], target: &[u8], edits: &[(Spec, Setting)]) -> Result<(), String> {
        Self::validate(target)?;
        if fresh.len() != target.len() {
            return Err("length differs".into());
        }
        let mut allowed = vec![Self::CRC, Self::CRC + 1];
        for (spec, setting) in edits {
            allowed.push(spec.mode());
            if matches!(setting, Setting::Duty(_)) {
                allowed.extend([spec.preset(), spec.preset() + 1]);
                if be16(target, spec.preset()) > 10000 {
                    return Err(format!("{} preset above 100 %", spec.name));
                }
            }
            if target[spec.mode()] > 1 {
                return Err(format!("{} mode byte {} is neither duty nor set-point mode", spec.name, target[spec.mode()]));
            }
        }
        match (0..target.len()).find(|o| target[*o] != fresh[*o] && !allowed.contains(o)) {
            Some(o) => Err(format!("byte {o:#05X} outside the requested channel fields would change")),
            None => Ok(()),
        }
    }
    fn fresh(&mut self) -> Result<Vec<u8>, String> {
        let r = self.dev.get()?;
        Self::validate(&r)?;
        Ok(r)
    }
    pub fn modes(&mut self) -> Result<Vec<(Spec, u8)>, String> {
        let r = self.fresh()?;
        Ok(Spec::ALL.iter().map(|s| (*s, r[s.mode()])).collect())
    }
    pub fn write(&mut self, edits: &[(Spec, Setting)]) -> Result<bool, String> {
        let fresh = self.fresh()?;
        let mut target = fresh.clone();
        for (spec, setting) in edits {
            if fresh[spec.mode()] > 1 {
                return Err(format!("{} is in mode {}, refusing to touch it", spec.name, fresh[spec.mode()]));
            }
            match setting {
                Setting::Firmware => target[spec.mode()] = 1,
                Setting::Duty(duty) => {
                    target[spec.mode()] = 0;
                    target[spec.preset()..spec.preset() + 2].copy_from_slice(&((duty * 100.0).round() as u16).to_be_bytes());
                }
            }
        }
        Crc16Usb::seal(&mut target);
        if target == fresh {
            return Ok(false);
        }
        Self::check_diff(&fresh, &target, edits)?;
        self.dev.set(&target)?;
        self.dev.settle(Self::READBACK_MS);
        if self.fresh()? != target {
            return Err("readback differs from what was written".into());
        }
        Ok(true)
    }
}

pub type Factory = Box<dyn Fn() -> Result<Box<dyn D5Control>, String>>;

pub struct D5Actuator {
    factory: Factory,
    guard: Option<Guard>,
    floors: [(f64, f64); 2],
}

impl D5Actuator {
    pub fn new(factory: Factory, cfg: &ControlConfig) -> D5Actuator {
        let bound = |k, o: &ChannelOverride| ChannelConfig::resolve(k, o).unwrap_or_else(|_| ChannelConfig::defaults(k));
        let (f, p) = (bound(ControlKind::Fan, &cfg.fan), bound(ControlKind::Pump, &cfg.pump));
        D5Actuator { factory, guard: None, floors: [(f.floor_duty, f.max_duty), (p.floor_duty, p.max_duty)] }
    }
    fn guard(&mut self) -> Result<&mut Guard, String> {
        if self.guard.is_none() {
            self.guard = Some(Guard::new((self.factory)()?));
        }
        Ok(self.guard.as_mut().unwrap())
    }
    fn edits(&self, set: &[(ControlKind, Setting)]) -> Vec<(Spec, Setting)> {
        set.iter()
            .map(|(k, s)| {
                let spec = Spec::of(*k);
                let (lo, hi) = self.floors[Spec::ALL.iter().position(|x| x.kind == *k).unwrap()];
                (spec, if let Setting::Duty(d) = s { Setting::Duty(d.clamp(lo, hi)) } else { *s })
            })
            .collect()
    }
}

impl Actuator for D5Actuator {
    fn apply(&mut self, set: &[(ControlKind, Setting)]) -> Result<(), String> {
        let edits = self.edits(set);
        let r = self.guard().and_then(|g| g.write(&edits));
        if r.is_err() {
            self.guard = None;
        }
        r.map(|_| ())
    }
    fn recover(&mut self) -> Result<Vec<ControlKind>, String> {
        let modes = self.guard().and_then(|g| g.modes());
        let stuck: Vec<ControlKind> = match modes {
            Ok(m) => m.iter().filter(|(_, b)| *b == 0).map(|(s, _)| s.kind).collect(),
            Err(e) => {
                self.guard = None;
                return Err(e);
            }
        };
        if !stuck.is_empty() {
            let set: Vec<(ControlKind, Setting)> = stuck.iter().map(|k| (*k, Setting::Firmware)).collect();
            self.apply(&set)?;
        }
        Ok(stuck)
    }
}
