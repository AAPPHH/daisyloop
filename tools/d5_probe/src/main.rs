use d5_probe::Crc16Usb;
use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};

struct Config {
    vid: u16,
    pid: u16,
    usage_page: u16,
    report_id: u8,
    len: usize,
    samples: usize,
    gap: Duration,
    out_dir: &'static str,
}

struct Probe {
    cfg: Config,
    dev: HidDevice,
}

impl Probe {
    fn open(cfg: Config) -> Probe {
        let api = HidApi::new().expect("hidapi");
        let info = api
            .device_list()
            .find(|d| d.vendor_id() == cfg.vid && d.product_id() == cfg.pid && d.usage_page() == cfg.usage_page)
            .expect("D5 Next interface not found");
        let dev = info.open_device(&api).expect("open");
        Probe { cfg, dev }
    }
    fn get_report(&self) -> Vec<u8> {
        let mut buf = vec![0u8; self.cfg.len];
        buf[0] = self.cfg.report_id;
        let n = self.dev.get_feature_report(&mut buf).expect("get_feature_report");
        buf.truncate(n);
        buf
    }
    fn telemetry(&self) -> Option<Vec<u8>> {
        let mut buf = [0u8; 512];
        let end = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < end {
            let n = self.dev.read_timeout(&mut buf, 1100).unwrap_or(0);
            if n >= 0x9E && buf[0] == 1 {
                return Some(buf[..n].to_vec());
            }
        }
        None
    }
    fn hexdump(data: &[u8]) -> String {
        data.chunks(16)
            .enumerate()
            .map(|(i, c)| format!("{:04X}  {}", i * 16, c.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")))
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn crc_report(r: &[u8]) -> String {
        let end = r.len() - 2;
        let stored = u16::from_be_bytes([r[end], r[end + 1]]);
        let without_id = Crc16Usb::compute(&r[1..end]);
        let with_id = Crc16Usb::compute(&r[0..end]);
        format!(
            "stored={:04X} crc[1..{:#X})={:04X} {} crc[0..{:#X})={:04X} {}",
            stored, end, without_id, without_id == stored, end, with_id, with_id == stored
        )
    }
    fn run(&self) {
        println!("crc vector 123456789 -> {:04X} (expect B4C8)", Crc16Usb::compute(b"123456789"));
        let mut reports = Vec::new();
        let mut tele = Vec::new();
        for i in 0..self.cfg.samples {
            if i > 0 {
                std::thread::sleep(self.cfg.gap);
            }
            let r = self.get_report();
            println!("sample {} len={:#X} id={:#04X} {}", i, r.len(), r[0], Self::crc_report(&r));
            reports.push(r);
            tele.push(self.telemetry());
        }
        std::fs::write(format!("{}/d5_control_backup.bin", self.cfg.out_dir), &reports[0]).unwrap();
        let mut txt = format!("report 0x03 sample 0 len={:#X}\n{}\n", reports[0].len(), Self::hexdump(&reports[0]));
        for (i, r) in reports.iter().enumerate().skip(1) {
            let diffs: Vec<String> = (0..r.len().min(reports[0].len()))
                .filter(|&o| r[o] != reports[0][o])
                .map(|o| format!("{:#05X}:{:02X}->{:02X}", o, reports[0][o], r[o]))
                .collect();
            println!("sample {} vs 0 diffs: {}", i, if diffs.is_empty() { "none".into() } else { diffs.join(" ") });
            txt += &format!("\nsample {} full dump\n{}\n", i, Self::hexdump(r));
        }
        for (i, t) in tele.iter().enumerate() {
            if let Some(t) = t {
                txt += &format!("\ntelemetry {} (input report 0x01)\n{}\n", i, Self::hexdump(t));
            }
        }
        std::fs::write(format!("{}/d5_control_backup.txt", self.cfg.out_dir), txt).unwrap();
    }
}

fn main() {
    Probe::open(Config {
        vid: 0x0C70,
        pid: 0xF00E,
        usage_page: 0xFF00,
        report_id: 0x03,
        len: 0x329,
        samples: 3,
        gap: Duration::from_secs(4),
        out_dir: "snapshot/raw",
    })
    .run();
}
