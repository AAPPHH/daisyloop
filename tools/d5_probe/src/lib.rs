use std::path::PathBuf;

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

pub trait Hid {
    fn get(&mut self) -> Result<Vec<u8>, String>;
    fn set(&mut self, report: &[u8]) -> Result<(), String>;
    fn pause(&mut self, ms: u64);
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub name: &'static str,
    pub base: usize,
    pub setpoint: Option<f64>,
}

impl Channel {
    pub fn mode(&self) -> usize {
        self.base
    }
    pub fn preset(&self) -> usize {
        self.base + 1
    }
    pub fn setpoint_at(&self) -> usize {
        self.base + 5
    }
    fn word(r: &[u8], at: usize) -> u16 {
        u16::from_be_bytes([r[at], r[at + 1]])
    }
    pub fn preset_of(&self, r: &[u8]) -> u16 {
        Self::word(r, self.preset())
    }
    pub fn setpoint_of(&self, r: &[u8]) -> u16 {
        Self::word(r, self.setpoint_at())
    }
    fn wanted_setpoint(&self, r: &[u8]) -> u16 {
        self.setpoint.map_or(self.setpoint_of(r), |s| (s * 100.0).round() as u16)
    }
}

#[derive(Clone, Debug)]
pub struct AbortConfig {
    pub len: usize,
    pub id: u8,
    pub channels: Vec<Channel>,
    pub attempts: usize,
    pub retry_ms: u64,
    pub settle_ms: u64,
    pub watch_reads: usize,
    pub watch_ms: u64,
    pub stop_file: Option<PathBuf>,
    pub stop_wait_ms: u64,
}

impl AbortConfig {
    pub fn d5_next(stop_file: Option<PathBuf>) -> AbortConfig {
        AbortConfig {
            len: 0x329,
            id: 3,
            channels: vec![Channel { name: "fan", base: 0x41, setpoint: None }, Channel { name: "pump", base: 0x96, setpoint: None }],
            attempts: 8,
            retry_ms: 1000,
            settle_ms: 500,
            watch_reads: 10,
            watch_ms: 1000,
            stop_file,
            stop_wait_ms: 6000,
        }
    }
}

pub struct Abort {
    dev: Box<dyn Hid>,
    cfg: AbortConfig,
    pub log: Vec<String>,
}

impl Abort {
    pub fn new(dev: Box<dyn Hid>, cfg: AbortConfig) -> Abort {
        Abort { dev, cfg, log: Vec::new() }
    }
    fn fresh(&mut self) -> Result<Vec<u8>, String> {
        let r = self.dev.get()?;
        if r.len() != self.cfg.len || r[0] != self.cfg.id {
            return Err(format!("bad report length {} id {}", r.len(), r.first().copied().unwrap_or(0)));
        }
        if Crc16Usb::stored(&r) != Crc16Usb::of_report(&r) {
            return Err("crc mismatch on device report".into());
        }
        Ok(r)
    }
    fn target(&self, fresh: &[u8]) -> Vec<u8> {
        let mut t = fresh.to_vec();
        for c in &self.cfg.channels {
            t[c.mode()] = 1;
            let s = c.wanted_setpoint(fresh).to_be_bytes();
            t[c.setpoint_at()..c.setpoint_at() + 2].copy_from_slice(&s);
        }
        Crc16Usb::seal(&mut t);
        t
    }
    fn on_firmware(&self, r: &[u8], before: &[u8]) -> bool {
        self.cfg.channels.iter().all(|c| r[c.mode()] == 1 && c.preset_of(r) == c.preset_of(before) && c.setpoint_of(r) == c.wanted_setpoint(before))
    }
    fn describe(&self, r: &[u8]) -> String {
        self.cfg.channels.iter().map(|c| format!("{} mode {} preset {:.2} % set-point {:.2} C", c.name, r[c.mode()], c.preset_of(r) as f64 / 100.0, c.setpoint_of(r) as f64 / 100.0)).collect::<Vec<_>>().join(", ")
    }
    fn note(&mut self, line: String) {
        self.log.push(line);
    }
    pub fn release(&mut self) -> Result<bool, String> {
        let fresh = self.fresh()?;
        let target = self.target(&fresh);
        if target == fresh {
            self.note(format!("already on the firmware PID: {}", self.describe(&fresh)));
            return Ok(false);
        }
        let allowed: Vec<usize> = self.cfg.channels.iter().flat_map(|c| [c.mode(), c.setpoint_at(), c.setpoint_at() + 1]).chain([self.cfg.len - 2, self.cfg.len - 1]).collect();
        if let Some(o) = (0..fresh.len()).find(|o| fresh[*o] != target[*o] && !allowed.contains(o)) {
            return Err(format!("byte {o:#05X} outside the requested fields would change, nothing sent"));
        }
        self.note(format!("before: {}", self.describe(&fresh)));
        self.dev.set(&target)?;
        self.dev.pause(self.cfg.settle_ms);
        let back = self.fresh()?;
        if !self.on_firmware(&back, &fresh) {
            return Err(format!("readback does not show the firmware PID: {}", self.describe(&back)));
        }
        self.note(format!("after: {}", self.describe(&back)));
        Ok(true)
    }
    fn request_stop(&mut self) {
        let Some(path) = self.cfg.stop_file.clone() else { return };
        match std::fs::write(&path, b"") {
            Ok(()) => {
                self.note(format!("stop file written at {}, waiting {} ms for the daemon to release", path.display(), self.cfg.stop_wait_ms));
                self.dev.pause(self.cfg.stop_wait_ms);
            }
            Err(e) => self.note(format!("stop file not written: {e}")),
        }
    }
    pub fn run(&mut self) -> Result<(), String> {
        self.request_stop();
        let mut last = String::new();
        let mut done = false;
        for i in 0..self.cfg.attempts {
            if i > 0 {
                self.dev.pause(self.cfg.retry_ms);
            }
            match self.release() {
                Ok(_) => {
                    done = true;
                    break;
                }
                Err(e) => {
                    self.note(format!("attempt {} failed: {e}", i + 1));
                    last = e;
                }
            }
        }
        if !done {
            return Err(format!("could not return fan and pump to the firmware PID: {last}"));
        }
        for _ in 0..self.cfg.watch_reads {
            self.dev.pause(self.cfg.watch_ms);
            let r = self.fresh()?;
            if self.cfg.channels.iter().any(|c| r[c.mode()] != 1) {
                self.note(format!("a duty mode came back: {}", self.describe(&r)));
                self.release()?;
            }
        }
        let r = self.fresh()?;
        if self.cfg.channels.iter().any(|c| r[c.mode()] != 1) {
            return Err(format!("final readback is not on the firmware PID: {}", self.describe(&r)));
        }
        self.note(format!("final: {}", self.describe(&r)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const REAL: &[u8] = include_bytes!("../../../snapshot/raw/d5_control_backup.bin");

    struct World {
        report: Vec<u8>,
        sets: usize,
        fail_get: usize,
        fail_set: usize,
        ignore_set: bool,
        regress: usize,
        pauses: Vec<u64>,
    }

    #[derive(Clone)]
    struct Fake(Rc<RefCell<World>>);

    impl Fake {
        fn from(report: Vec<u8>) -> Fake {
            Fake(Rc::new(RefCell::new(World { report, sets: 0, fail_get: 0, fail_set: 0, ignore_set: false, regress: 0, pauses: vec![] })))
        }
        fn duty(real: &[u8]) -> Fake {
            let mut r = real.to_vec();
            for c in AbortConfig::d5_next(None).channels {
                r[c.mode()] = 0;
                r[c.preset()..c.preset() + 2].copy_from_slice(&0u16.to_be_bytes());
            }
            Crc16Usb::seal(&mut r);
            Fake::from(r)
        }
        fn report(&self) -> Vec<u8> {
            self.0.borrow().report.clone()
        }
        fn expected(real: &[u8]) -> Vec<u8> {
            let mut r = Fake::duty(real).report();
            for c in AbortConfig::d5_next(None).channels {
                r[c.mode()] = 1;
            }
            Crc16Usb::seal(&mut r);
            r
        }
        fn sets(&self) -> usize {
            self.0.borrow().sets
        }
    }

    impl Hid for Fake {
        fn get(&mut self) -> Result<Vec<u8>, String> {
            let mut w = self.0.borrow_mut();
            if w.fail_get > 0 {
                w.fail_get -= 1;
                return Err("get failed".into());
            }
            Ok(w.report.clone())
        }
        fn set(&mut self, report: &[u8]) -> Result<(), String> {
            let mut w = self.0.borrow_mut();
            if w.fail_set > 0 {
                w.fail_set -= 1;
                return Err("set failed".into());
            }
            w.sets += 1;
            if !w.ignore_set {
                w.report = report.to_vec();
            }
            Ok(())
        }
        fn pause(&mut self, ms: u64) {
            let mut w = self.0.borrow_mut();
            w.pauses.push(ms);
            if w.regress > 0 && ms == 1000 && w.report[0x41] == 1 {
                w.regress -= 1;
                w.report[0x41] = 0;
                Crc16Usb::seal(&mut w.report);
            }
        }
    }

    fn abort(f: &Fake) -> Abort {
        Abort::new(Box::new(f.clone()), AbortConfig::d5_next(None))
    }

    #[test]
    fn fixture_is_a_valid_firmware_report_with_the_expected_layout() {
        assert_eq!(REAL.len(), 0x329);
        assert_eq!(Crc16Usb::stored(REAL), Crc16Usb::of_report(REAL));
        let c = AbortConfig::d5_next(None).channels;
        assert_eq!((REAL[c[0].mode()], REAL[c[1].mode()]), (1, 1));
        assert_eq!((c[0].setpoint_of(REAL), c[1].setpoint_of(REAL)), (3000, 4500));
    }

    #[test]
    fn fan_at_zero_percent_and_pump_in_duty_come_back_on_the_firmware_pid_with_preset_and_setpoint_untouched() {
        let f = Fake::duty(REAL);
        let mut a = abort(&f);
        a.run().unwrap();
        assert!(f.report() == Fake::expected(REAL));
        assert_eq!(f.sets(), 1);
    }

    #[test]
    fn nothing_is_sent_when_both_are_already_on_the_firmware_pid() {
        let f = Fake::from(REAL.to_vec());
        abort(&f).run().unwrap();
        assert_eq!(f.sets(), 0);
    }

    #[test]
    fn transient_get_and_set_failures_are_retried() {
        let f = Fake::duty(REAL);
        {
            let mut w = f.0.borrow_mut();
            w.fail_get = 2;
            w.fail_set = 2;
        }
        let mut a = abort(&f);
        a.run().unwrap();
        assert!(f.report() == Fake::expected(REAL));
        assert!(a.log.iter().filter(|l| l.contains("failed")).count() >= 3);
    }

    #[test]
    fn a_hung_daemon_that_rewrites_the_duty_is_overruled() {
        let f = Fake::duty(REAL);
        f.0.borrow_mut().regress = 2;
        let mut a = abort(&f);
        a.run().unwrap();
        assert!(f.report() == Fake::expected(REAL));
        assert!(f.sets() >= 2);
    }

    #[test]
    fn a_set_the_device_ignores_is_reported_as_failure() {
        let f = Fake::duty(REAL);
        f.0.borrow_mut().ignore_set = true;
        let mut a = abort(&f);
        assert!(a.run().unwrap_err().contains("firmware PID"));
        assert_eq!(f.sets(), 8);
    }

    #[test]
    fn a_report_with_a_bad_crc_or_length_is_never_written() {
        let mut bad = Fake::duty(REAL).report();
        bad[0x100] ^= 1;
        let f = Fake::from(bad);
        assert!(abort(&f).run().is_err());
        assert_eq!(f.sets(), 0);
        let f = Fake::from(REAL[..100].to_vec());
        assert!(abort(&f).run().is_err());
        assert_eq!(f.sets(), 0);
    }

    #[test]
    fn only_mode_bytes_and_the_crc_differ_from_what_the_device_held() {
        let f = Fake::duty(REAL);
        let before = f.report();
        abort(&f).run().unwrap();
        let diff: Vec<usize> = (0..before.len()).filter(|o| before[*o] != f.report()[*o]).collect();
        assert_eq!(diff.len(), 4);
        assert!(diff.iter().all(|o| [0x41, 0x96, 0x327, 0x328].contains(o)));
    }

    #[test]
    fn a_configured_setpoint_is_restored_and_read_back() {
        let mut moved = Fake::duty(REAL).report();
        moved[0x46..0x48].copy_from_slice(&3200u16.to_be_bytes());
        Crc16Usb::seal(&mut moved);
        let f = Fake::from(moved);
        let mut cfg = AbortConfig::d5_next(None);
        cfg.channels[0].setpoint = Some(30.0);
        Abort::new(Box::new(f.clone()), cfg).run().unwrap();
        assert!(f.report() == Fake::expected(REAL));
    }

    #[test]
    fn the_stop_file_is_written_before_the_device_is_touched() {
        let dir = std::env::temp_dir().join(format!("d5_abort_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stop = dir.join("stop");
        let f = Fake::duty(REAL);
        let mut a = Abort::new(Box::new(f.clone()), AbortConfig::d5_next(Some(stop.clone())));
        a.run().unwrap();
        assert!(stop.exists());
        assert_eq!(f.0.borrow().pauses[0], 6000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
