use crate::{config::Cpu, protocol::check_range};
use anyhow::{Context, Result, bail, ensure};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub address: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct Image {
    pub blocks: Vec<Block>,
    pub entry: Option<u32>,
    pub verify_launch: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Format {
    IntelHex,
    Srec,
    Wdc,
    Pgx,
    Pgz,
}

impl Image {
    pub fn push(&mut self, address: u32, data: &[u8]) -> Result<()> {
        check_range(address, data.len())?;
        self.blocks.push(Block {
            address,
            data: data.to_vec(),
        });
        Ok(())
    }
    pub fn vectors(&mut self, cpu: Cpu, entry: u32) -> Result<()> {
        check_range(entry, 1)?;
        self.entry = Some(entry);
        match cpu {
            Cpu::C65c02 => {
                ensure!(entry <= 0xFFFF, "65C02 entry point must fit in 16 bits");
                let vector = (entry as u16).to_le_bytes();
                self.push(0xFFFC, &vector)?;
                self.push(0x0088, &vector)?;
                self.push(0x00FA, &[0, 0])?;
                // Publish only after all program data, entry and arguments are staged.
                self.push(0x0080, b"CROSSDEV")?;
                self.verify_launch = true;
            }
            Cpu::C65816 => {
                let vector = if entry > 0xFFFF {
                    self.push(
                        0xFF80,
                        &[
                            0x18,
                            0xFB,
                            0x5C,
                            entry as u8,
                            (entry >> 8) as u8,
                            (entry >> 16) as u8,
                        ],
                    )?;
                    0xFF80u16
                } else {
                    entry as u16
                };
                self.push(0xFFFC, &vector.to_le_bytes())?;
            }
        }
        Ok(())
    }
}

pub fn parse(data: &[u8], format: Format, cpu: Cpu) -> Result<Image> {
    ensure!(!data.is_empty(), "input file is empty");
    match format {
        Format::IntelHex => intel_hex(std::str::from_utf8(data).context("Intel HEX must be text")?),
        Format::Srec => srec(std::str::from_utf8(data).context("S-record file must be text")?),
        Format::Wdc => blocks(data, cpu, true),
        Format::Pgz => blocks(data, cpu, false),
        Format::Pgx => {
            ensure!(
                data.len() >= 8 && &data[..3] == b"PGX",
                "invalid PGX header"
            );
            ensure!(data[3] >> 4 == 0, "unsupported PGX version");
            let expected = match cpu {
                Cpu::C65c02 => 3,
                Cpu::C65816 => 1,
            };
            ensure!(data[3] & 0x0F == expected, "PGX CPU does not match --cpu");
            let entry = le(&data[4..8]);
            let mut image = Image::default();
            image.push(entry, &data[8..])?;
            image.vectors(cpu, entry)?;
            Ok(image)
        }
    }
}

fn le(data: &[u8]) -> u32 {
    data.iter()
        .enumerate()
        .fold(0, |value, (i, b)| value | (u32::from(*b) << (8 * i)))
}

fn blocks(data: &[u8], cpu: Cpu, wdc: bool) -> Result<Image> {
    let width = match data[0] {
        b'Z' => 3,
        b'z' if !wdc => 4,
        _ => bail!("invalid block file signature"),
    };
    let mut image = Image::default();
    let mut offset = 1;
    let mut entry = None;
    while offset < data.len() {
        ensure!(
            data.len() - offset >= width * 2,
            "truncated block header at offset {offset}"
        );
        let address = le(&data[offset..offset + width]);
        let size = le(&data[offset + width..offset + width * 2]) as usize;
        offset += width * 2;
        if (wdc && address == 0) || (address == 0 && size == 0) {
            ensure!(offset == data.len(), "unexpected data after terminator");
            break;
        }
        if size == 0 && !wdc {
            ensure!(
                entry.replace(address).is_none(),
                "multiple PGZ entry points"
            );
        } else {
            ensure!(
                size <= data.len() - offset,
                "truncated block payload at offset {offset}"
            );
            image.push(address, &data[offset..offset + size])?;
            offset += size;
        }
    }
    if let Some(entry) = entry {
        image.vectors(cpu, entry)?;
    }
    ensure!(!image.blocks.is_empty(), "file contains no data blocks");
    Ok(image)
}

fn hex_bytes(text: &str) -> Result<Vec<u8>> {
    ensure!(
        text.is_ascii() && text.len().is_multiple_of(2),
        "invalid hex record"
    );
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).context("invalid hex byte"))
        .collect()
}

fn intel_hex(text: &str) -> Result<Image> {
    let mut image = Image::default();
    let mut base = 0u32;
    let mut ended = false;
    for (line_number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        ensure!(!ended, "data after Intel HEX EOF");
        let record = hex_bytes(
            line.strip_prefix(':')
                .context("Intel HEX record must start with ':'")?,
        )
        .with_context(|| format!("line {}", line_number + 1))?;
        ensure!(
            record.len() >= 5 && record.len() == record[0] as usize + 5,
            "invalid Intel HEX record length at line {}",
            line_number + 1
        );
        ensure!(
            record.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)) == 0,
            "Intel HEX checksum mismatch at line {}",
            line_number + 1
        );
        let address = u32::from(u16::from_be_bytes([record[1], record[2]]));
        let payload = &record[4..record.len() - 1];
        match record[3] {
            0 => image.push(
                base.checked_add(address)
                    .context("Intel HEX address overflow")?,
                payload,
            )?,
            1 => {
                ensure!(payload.is_empty() && address == 0, "invalid EOF record");
                ended = true;
            }
            2 | 4 => {
                ensure!(
                    payload.len() == 2 && address == 0,
                    "invalid extended address record"
                );
                base = u32::from(u16::from_be_bytes([payload[0], payload[1]]))
                    << if record[3] == 2 { 4 } else { 16 };
            }
            3 | 5 => ensure!(
                payload.len() == 4 && address == 0,
                "invalid start address record"
            ),
            kind => bail!("unsupported Intel HEX record {kind:02X}"),
        }
    }
    ensure!(ended, "missing Intel HEX EOF record");
    ensure!(!image.blocks.is_empty(), "file contains no data records");
    Ok(image)
}

fn srec(text: &str) -> Result<Image> {
    let mut image = Image::default();
    for (line_number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let text = line
            .strip_prefix('S')
            .context("S-record must start with 'S'")?;
        ensure!(
            text.is_ascii() && text.len() >= 3,
            "invalid S-record header"
        );
        let kind = text.as_bytes()[0];
        let record = hex_bytes(&text[1..]).with_context(|| format!("line {}", line_number + 1))?;
        ensure!(
            record.len() == record[0] as usize + 1,
            "S-record byte count mismatch"
        );
        ensure!(
            record.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)) == 0xFF,
            "S-record checksum mismatch"
        );
        let width = match kind {
            b'0' | b'1' | b'5' | b'9' => 2,
            b'2' | b'6' | b'8' => 3,
            b'3' | b'7' => 4,
            _ => bail!("unsupported S-record type"),
        };
        ensure!(record.len() >= width + 2, "S-record is too short");
        let address = record[1..1 + width]
            .iter()
            .fold(0u32, |v, b| (v << 8) | u32::from(*b));
        if matches!(kind, b'1'..=b'3') {
            image.push(address, &record[1 + width..record.len() - 1])?;
        }
    }
    ensure!(!image.blocks.is_empty(), "file contains no data records");
    Ok(image)
}
