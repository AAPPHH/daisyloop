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
