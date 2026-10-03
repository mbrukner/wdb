use std::io::{self, Cursor, Read, Write};
use wdb::protocol::{self, DebugPort};

struct Memory {
    base: u32,
    bytes: Vec<u8>,
    delayed: bool,
    inconsistent: bool,
    response: Cursor<Vec<u8>>,
    requests: Vec<(u32, usize)>,
}
impl Read for Memory {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.response.read(bytes)
    }
}
impl Write for Memory {
    fn write(&mut self, request: &[u8]) -> io::Result<usize> {
        assert_eq!(request[1], protocol::READ); // These reads must never write hardware.
        let address = u32::from_be_bytes([0, request[2], request[3], request[4]]);
        let length = u16::from_be_bytes([request[5], request[6]]) as usize;
        self.requests.push((address, length));
        assert!(address >= self.base);
        assert!(address as usize + length <= self.base as usize + self.bytes.len());
        assert!(address as usize + length <= protocol::ADDRESS_SPACE as usize);
        let mut response = vec![0xAA, 0, 0];
        for i in 0..length {
            let offset = if self.delayed && address >= protocol::FLASH_BASE {
                i.saturating_sub(1)
            } else {
                i
            };
            let byte = self.bytes[(address - self.base) as usize + offset];
            response.push(if self.inconsistent && length == 1 {
                byte ^ 0x80
            } else {
                byte
            });
        }
        response.push(protocol::xor(&response));
        self.response = Cursor::new(response);
        Ok(request.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn memory(base: u32, bytes: Vec<u8>, delayed: bool) -> DebugPort<Memory> {
    DebugPort::new(
        Memory {
            base,
            bytes,
            delayed,
            inconsistent: false,
            response: Cursor::new(vec![]),
            requests: vec![],
        },
        1024,
    )
    .unwrap()
}

#[test]
fn delayed_flash_bursts_are_disambiguated_with_independent_reads() {
    let bytes: Vec<_> = (0..8193).map(|i| ((i * 37 + i / 13) % 256) as u8).collect();
    for delayed in [false, true] {
        let mut dp = memory(0xFF6000, bytes.clone(), delayed);
        assert_eq!(dp.read(0xFF6000, 8192).unwrap(), bytes[..8192]);
        assert!(dp.transport.requests.iter().any(|(_, size)| *size == 1));
        assert!(dp.transport.requests.iter().all(|(_, size)| *size <= 1024));
    }
}

#[test]
fn genuine_repeated_bytes_are_not_removed_from_flash() {
    let bytes = b"JJPZP\0\0\0real data".to_vec();
    for delayed in [false, true] {
        let mut dp = memory(0xFF6000, bytes.clone(), delayed);
        assert_eq!(
            dp.read(0xFF6000, bytes.len() - 1).unwrap(),
            bytes[..bytes.len() - 1]
        );
    }
}

#[test]
fn uniform_flash_requires_no_alignment_probes() {
    for delayed in [false, true] {
        let mut dp = memory(0xFF6000, vec![0xFF; 1024], delayed);
        assert_eq!(dp.read(0xFF6000, 1023).unwrap(), vec![0xFF; 1023]);
        assert_eq!(dp.transport.requests.len(), 1);
    }
}

#[test]
fn flash_end_never_wraps_to_address_zero() {
    let bytes: Vec<_> = (0..2048).map(|i| (i % 256) as u8).collect();
    for delayed in [false, true] {
        let mut dp = memory(0xFFF800, bytes.clone(), delayed);
        assert_eq!(dp.read(0xFFF800, bytes.len()).unwrap(), bytes);
        assert_eq!(dp.transport.requests.last(), Some(&(0xFFFFFF, 1)));
    }
}

#[test]
fn reads_crossing_into_flash_keep_the_regions_separate() {
    let bytes: Vec<_> = (0..2048).map(|i| (i % 256) as u8).collect();
    let mut dp = memory(protocol::FLASH_BASE - 2, bytes.clone(), true);
    assert_eq!(
        dp.read(protocol::FLASH_BASE - 2, 2047).unwrap(),
        bytes[..2047]
    );
    assert_eq!(dp.transport.requests[0], (protocol::FLASH_BASE - 2, 2));
}

#[test]
fn inconsistent_flash_data_is_rejected_instead_of_realigned() {
    let mut dp = memory(0xFF6000, vec![1, 2, 3, 4, 5], true);
    dp.transport.inconsistent = true;
    assert!(
        dp.read(0xFF6000, 4)
            .unwrap_err()
            .to_string()
            .contains("inconsistent flash read")
    );
}
