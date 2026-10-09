use crate::config::Target;
use anyhow::{Context, Result, bail, ensure};
use std::io::{Read, Write};

pub const READ: u8 = 0x00;
pub const WRITE: u8 = 0x01;
pub const PROGRAM_FLASH: u8 = 0x10;
pub const ERASE_FLASH: u8 = 0x11;
pub const ERASE_SECTOR: u8 = 0x12;
pub const PROGRAM_SECTOR: u8 = 0x13;
pub const STOP: u8 = 0x20;
pub const START: u8 = 0x21;
pub const ENTER: u8 = 0x80;
pub const EXIT: u8 = 0x81;
pub const BOOT_RAM: u8 = 0x90;
pub const BOOT_FLASH: u8 = 0x91;
pub const REVISION: u8 = 0xFE;
pub const PS2_REGISTER: u32 = 0xF01642;
pub const OPTICAL_REGISTER: u32 = 0xF01DE0;
pub const MAX_TRANSFER: usize = 1024;
pub const ADDRESS_SPACE: u32 = 0x1000000;
pub const FLASH_BASE: u32 = 0xF80000;

pub fn check_range(address: u32, length: usize) -> Result<()> {
    ensure!(
        address < ADDRESS_SPACE && length <= (ADDRESS_SPACE - address) as usize,
        "range {address:06X} + {length} bytes exceeds the 24-bit address space"
    );
    Ok(())
}

pub fn xor(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |acc, byte| acc ^ byte)
}

pub fn packet(command: u8, address: u32, data: &[u8], read_length: usize) -> Result<Vec<u8>> {
    check_range(address, data.len().max(read_length))?;
    ensure!(
        data.len().max(read_length) <= MAX_TRANSFER,
        "transaction exceeds {MAX_TRANSFER} bytes"
    );
    ensure!(
        data.is_empty() || read_length == 0,
        "cannot read and write in one transaction"
    );
    let length = data.len().max(read_length) as u16;
    let mut bytes = vec![
        0x55,
        command,
        (address >> 16) as u8,
        (address >> 8) as u8,
        address as u8,
        (length >> 8) as u8,
        length as u8,
    ];
    bytes.extend_from_slice(data);
    bytes.push(xor(&bytes));
    Ok(bytes)
}

pub struct Response {
    pub status: [u8; 2],
    pub data: Vec<u8>,
}

pub fn read_response(reader: &mut impl Read, length: usize) -> Result<Response> {
    // Bound resynchronization even if a peer continuously emits unrelated data.
    let mut sync = [0];
    let mut found = false;
    for _ in 0..4096 {
        reader.read_exact(&mut sync).context("read response sync")?;
        if sync[0] == 0xAA {
            found = true;
            break;
        }
    }
    ensure!(found, "response sync not found");
    let mut body = vec![0; length + 3];
    reader
        .read_exact(&mut body)
        .context("read response status, payload and checksum")?;
    ensure!(xor(&body) ^ 0xAA == 0, "invalid response checksum");
    let status = [body[0], body[1]];
    ensure!(
        status[0] & 0x07 == 0,
        "device reported protocol error: {:02X} {:02X}",
        status[0],
        status[1]
    );
    Ok(Response {
        status,
        data: body[2..2 + length].to_vec(),
    })
}

pub struct DebugPort<T> {
    pub transport: T,
    chunk_size: usize,
}

impl<T: Read + Write> DebugPort<T> {
    pub fn new(transport: T, chunk_size: usize) -> Result<Self> {
        ensure!(
            (1..=MAX_TRANSFER).contains(&chunk_size),
            "invalid transaction size"
        );
        Ok(Self {
            transport,
            chunk_size,
        })
    }
    pub fn transfer(
        &mut self,
        command: u8,
        address: u32,
        data: &[u8],
        read_length: usize,
    ) -> Result<Response> {
        let bytes = packet(command, address, data, read_length)?;
        self.transport
            .write_all(&bytes)
            .with_context(|| format!("send command {command:02X}"))?;
        self.transport.flush().context("flush request")?;
        read_response(&mut self.transport, read_length)
            .with_context(|| format!("command {command:02X} at {address:06X}"))
    }
    pub fn command(&mut self, command: u8) -> Result<()> {
        self.transfer(command, 0, &[], 0).map(|_| ())
    }
    pub fn revision(&mut self) -> Result<u8> {
        Ok(self.transfer(REVISION, 0, &[], 0)?.status[1])
    }
    pub fn read(&mut self, address: u32, length: usize) -> Result<Vec<u8>> {
        self.read_progress(address, length, |_| {})
    }
    pub fn read_progress(
        &mut self,
        address: u32,
        length: usize,
        mut progress: impl FnMut(usize),
    ) -> Result<Vec<u8>> {
        check_range(address, length)?;
        let mut data = Vec::with_capacity(length);
        while data.len() < length {
            let current = address + data.len() as u32;
            let remaining = length - data.len();
            if current >= FLASH_BASE {
                data.extend(self.read_flash_chunk(current, remaining)?);
            } else {
                let size = remaining
                    .min(MAX_TRANSFER)
                    .min((FLASH_BASE - current) as usize);
                data.extend(self.transfer(READ, current, &[], size)?.data);
            }
            progress(data.len());
        }
        Ok(data)
    }

    fn read_flash_chunk(&mut self, address: u32, remaining: usize) -> Result<Vec<u8>> {
        let available = (ADDRESS_SPACE - address) as usize;
        if remaining == 1 || available == 1 {
            return Ok(self.transfer(READ, address, &[], 1)?.data);
        }
        // Some loaded cores return [byte0, byte0, byte1, ...] for flash bursts,
        // while single-byte reads are correct. Request one extra byte and use
        // independent single-byte reads at differing positions to distinguish
        // this timing quirk from a core with ordinary, unshifted readback.
        // This decision never consults the expected programming image.
        let size = remaining.min(MAX_TRANSFER - 1).min(available - 1);
        let bytes = self.transfer(READ, address, &[], size + 1)?.data;
        let mut transitions = bytes.windows(2).enumerate().filter(|(_, w)| w[0] != w[1]);
        let Some((first, _)) = transitions.next() else {
            return Ok(bytes[..size].to_vec()); // Both interpretations are identical.
        };
        let actual = self.transfer(READ, address + first as u32, &[], 1)?.data[0];
        let shift = if actual == bytes[first] {
            0
        } else if bytes[0] == bytes[1] && actual == bytes[first + 1] {
            1
        } else {
            bail!(
                "inconsistent flash read at {:06X}: single-byte {:02X}, burst {:02X}/{:02X}",
                address + first as u32,
                actual,
                bytes[first],
                bytes[first + 1]
            );
        };
        if let Some((last, _)) = transitions.next_back() {
            let actual = self.transfer(READ, address + last as u32, &[], 1)?.data[0];
            ensure!(
                actual == bytes[last + shift],
                "inconsistent flash burst alignment at {:06X}",
                address + last as u32
            );
        }
        Ok(bytes[shift..shift + size].to_vec())
    }
    pub fn write(&mut self, address: u32, data: &[u8]) -> Result<()> {
        self.write_progress(address, data, |_| {})
    }
    pub fn write_progress(
        &mut self,
        address: u32,
        data: &[u8],
        mut progress: impl FnMut(usize),
    ) -> Result<()> {
        check_range(address, data.len())?;
        for (index, chunk) in data.chunks(self.chunk_size).enumerate() {
            self.transfer(WRITE, address + (index * self.chunk_size) as u32, chunk, 0)?;
            progress(index * self.chunk_size + chunk.len());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub revision: u8,
    pub ps2: bool,
    pub optical: bool,
}

impl Capabilities {
    pub fn new(target: Target, revision: u8) -> Self {
        // Unified Wildbits RTL revision 5 introduced both injection apertures.
        // Optical matrix hardware is populated on K2 only.
        Self {
            revision,
            ps2: revision >= 5,
            optical: target == Target::K2 && revision >= 5,
        }
    }
    pub fn require_keyboard(self, target: Target) -> Result<()> {
        if (target == Target::K2 && !self.optical) || (target == Target::Jr2 && !self.ps2) {
            bail!(
                "{} keyboard injection requires debug revision 5 or newer (device reports {})",
                target.name(),
                self.revision
            );
        }
        Ok(())
    }
}
