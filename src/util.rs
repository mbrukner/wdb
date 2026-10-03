use crate::cli::hex;
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

pub fn crc32(data: &[u8]) -> u32 {
    // Kernel COPYFILE ABI uses the reflected ZIP polynomial with zero seed
    // and no final XOR; this is deliberately not the usual ZIP CRC result.
    let mut crc = 0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xEDB88320 } else { 0 };
        }
    }
    crc
}

pub fn copy_payload(path: &Path, data: &[u8]) -> Result<Vec<u8>> {
    const LIMIT: usize = 7 * 65536 - 9 * 1024;
    ensure!(
        data.len() < LIMIT,
        "SD copy file must be smaller than {LIMIT} bytes"
    );
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .context("filename must be valid UTF-8")?;
    ensure!(
        !name.as_bytes().contains(&0),
        "filename contains a NUL byte"
    );
    let mut payload = name.as_bytes().to_vec();
    payload.push(0);
    payload.extend_from_slice(&crc32(data).to_le_bytes());
    payload.extend_from_slice(&(data.len() as u32).to_le_bytes()[..3]);
    payload.extend_from_slice(data);
    ensure!(
        payload.len() < 7 * 65536,
        "SD copy metadata and payload exceed RAM buffer"
    );
    Ok(payload)
}

pub fn lookup(path: &Path, label: &str) -> Result<u32> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read labels {}", path.display()))?;
    for line in text.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        if let Some((name, value)) = line.split_once('=')
            && name.trim() == label
        {
            let value = value
                .trim()
                .strip_prefix('$')
                .context("label address must start with '$'")?
                .split_whitespace()
                .next()
                .context("missing label address")?;
            return hex(value).map_err(anyhow::Error::msg);
        }
    }
    anyhow::bail!("label {label:?} not found in {}", path.display())
}

pub fn dump(writer: &mut impl Write, data: &[u8], start: u32) -> io::Result<()> {
    for (index, row) in data.chunks(16).enumerate() {
        write!(writer, "{:06X}: ", start + (index * 16) as u32)?;
        for byte in row {
            write!(writer, "{byte:02X} ")?;
        }
        for _ in row.len()..16 {
            write!(writer, "   ")?;
        }
        write!(writer, " | ")?;
        for byte in row {
            write!(
                writer,
                "{}",
                if (32..=126).contains(byte) {
                    char::from(*byte)
                } else {
                    '.'
                }
            )?;
        }
        writeln!(writer)?;
    }
    Ok(())
}

pub fn confirm(message: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    eprint!("{message}. Type 'yes' to continue: ");
    io::stderr().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim() == "yes")
}
