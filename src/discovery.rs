//! Passive USB discovery. Never open a UART, claim an interface, or detach a driver.
use crate::config::{Config, Target};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub const VID: u16 = 0x0403;
pub const PID: u16 = 0x6011;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub number: u8,
    pub driver: Option<String>,
    pub tty: Option<PathBuf>,
    pub port: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Device {
    /// Physical USB topology identity, e.g. 1-5; independent of tty numbering.
    pub location: String,
    pub serial: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub interfaces: Vec<Interface>,
}

impl Device {
    pub fn recognized(&self) -> bool {
        // This EEPROM label is shared by the boards. It does NOT identify
        // their model. VID/PID alone only identify a generic FT4232H bridge.
        self.product.as_deref() == Some("New Retro Computer")
    }
    pub fn selector(&self) -> String {
        self.serial
            .clone()
            .unwrap_or_else(|| format!("usb:{}", self.location))
    }
    fn matches(&self, selector: &str) -> bool {
        self.serial.as_deref() == Some(selector) || selector == format!("usb:{}", self.location)
    }
}

pub fn debug_interface(target: Target) -> u8 {
    match target {
        Target::K2 => 2,
        Target::Jr2 => 1,
    }
}

pub fn role(target: Target, interface: u8) -> &'static str {
    match (target, interface) {
        (_, 0) => "FPGA JTAG",
        (Target::K2, 1) => "RP2040 UART",
        (Target::K2, 2) | (Target::Jr2, 1) => "Binary debug",
        (Target::K2, 3) | (Target::Jr2, 2) => "CPU UART",
        (Target::Jr2, 3) => "Wi-Fi UART",
        _ => "Unknown",
    }
}

fn attribute(path: &Path, name: &str) -> Option<String> {
    fs::read_to_string(path.join(name))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}
fn hex_attribute(path: &Path, name: &str) -> Option<u16> {
    u16::from_str_radix(&attribute(path, name)?, 16).ok()
}

/// Return a stable by-id path if it currently resolves to this tty.
pub fn stable_port(port: &Path, by_id: &Path) -> PathBuf {
    let Ok(real) = fs::canonicalize(port) else {
        return port.into();
    };
    let mut aliases = Vec::new();
    if let Ok(entries) = fs::read_dir(by_id) {
        for entry in entries.flatten() {
            if fs::canonicalize(entry.path()).ok().as_ref() == Some(&real) {
                aliases.push(entry.path());
            }
        }
    }
    aliases.sort();
    aliases.into_iter().next().unwrap_or(real)
}

fn tty_under(path: &Path, dev_root: &Path, depth: u8) -> Option<PathBuf> {
    let mut entries: Vec<_> = fs::read_dir(path).ok()?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("ttyUSB") && dev_root.join(&name).exists() {
            return Some(dev_root.join(name));
        }
        // Stay in real child directories; sysfs driver/device symlinks lead
        // back out of this interface and must never be followed recursively.
        if depth > 0
            && entry.file_type().ok().is_some_and(|t| t.is_dir())
            && let Some(tty) = tty_under(&entry.path(), dev_root, depth - 1)
        {
            return Some(tty);
        }
    }
    None
}

/// Injectable roots permit testing composite devices and driver handoffs
/// without touching the host USB subsystem.
pub fn scan_linux(usb_root: &Path, dev_root: &Path) -> Result<Vec<Device>> {
    let mut paths: Vec<_> = fs::read_dir(usb_root)
        .context("enumerate USB sysfs devices")?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    paths.sort();
    let mut devices = Vec::new();
    for path in &paths {
        if hex_attribute(path, "idVendor") != Some(VID)
            || hex_attribute(path, "idProduct") != Some(PID)
        {
            continue;
        }
        let location = path
            .file_name()
            .context("USB device has no location")?
            .to_string_lossy()
            .into_owned();
        let mut interfaces = Vec::new();
        for interface in &paths {
            if !interface
                .file_name()
                .is_some_and(|s| s.to_string_lossy().starts_with(&format!("{location}:")))
            {
                continue;
            }
            let Some(number) =
                hex_attribute(interface, "bInterfaceNumber").and_then(|n| u8::try_from(n).ok())
            else {
                continue;
            };
            if number > 3 {
                continue;
            }
            let tty = tty_under(interface, dev_root, 3);
            let port = tty
                .as_ref()
                .map(|p| stable_port(p, &dev_root.join("serial/by-id")));
            let driver = fs::read_link(interface.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));
            interfaces.push(Interface {
                number,
                driver,
                tty,
                port,
            });
        }
        // The device can still be listed when all interfaces are unbound.
        interfaces.sort_by_key(|i| i.number);
        devices.push(Device {
            location,
            serial: attribute(path, "serial"),
            manufacturer: attribute(path, "manufacturer"),
            product: attribute(path, "product"),
            interfaces,
        });
    }
    Ok(devices)
}

pub fn scan() -> Result<Vec<Device>> {
    #[cfg(target_os = "linux")]
    {
        scan_linux(Path::new("/sys/bus/usb/devices"), Path::new("/dev"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        bail!("USB board discovery currently requires Linux; use list-ports and an explicit --port")
    }
}

pub fn select(devices: &[Device], target: Target, selector: Option<&str>) -> Result<PathBuf> {
    let candidates: Vec<_> = devices
        .iter()
        .filter(|d| match selector {
            Some(selector) => d.matches(selector),
            None => d.recognized(),
        })
        .collect();
    let device = match candidates.as_slice() {
        [] => bail!(
            "no matching Wildbits USB bridge; run 'wdb devices', choose --device SERIAL (or usb:LOCATION), or specify --port"
        ),
        [device] => device,
        _ => bail!(
            "multiple matching USB bridges: {}; select one with --device SERIAL or usb:LOCATION",
            candidates
                .iter()
                .map(|d| format!("{} (usb:{})", d.selector(), d.location))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let number = debug_interface(target);
    device.interfaces.iter().find(|i| i.number == number).and_then(|i| i.port.clone())
        .with_context(|| format!("{} debug interface {} / {number:02} on {} has no tty; check its USB driver binding or use --port", target.name(), char::from(b'A'+number), device.selector()))
}

pub fn resolve(cfg: &mut Config) -> Result<()> {
    let explicit = cfg
        .port
        .as_deref()
        .is_some_and(|p| !p.is_empty() && p != "auto");
    ensure!(
        !explicit || cfg.device.is_none(),
        "--device/device cannot be combined with an explicit port; use --port auto to override a configured port"
    );
    if explicit {
        return Ok(());
    }
    cfg.port = Some(
        select(&scan()?, cfg.target()?, cfg.device.as_deref())?
            .to_string_lossy()
            .into_owned(),
    );
    Ok(())
}

pub fn display(
    writer: &mut impl Write,
    devices: &[Device],
    target: Option<Target>,
    selector: Option<&str>,
) -> Result<()> {
    let mut count = 0;
    for device in devices
        .iter()
        .filter(|d| selector.is_none_or(|s| d.matches(s)))
    {
        count += 1;
        writeln!(
            writer,
            "{} — {} / {} (0403:6011, usb:{}){}",
            device.serial.as_deref().unwrap_or("no serial"),
            device.manufacturer.as_deref().unwrap_or("unknown vendor"),
            device.product.as_deref().unwrap_or("unknown product"),
            device.location,
            if device.recognized() {
                ""
            } else {
                " [generic FT4232H; explicit --device required]"
            }
        )?;
        if target.is_none() {
            writeln!(
                writer,
                "  IF    K2 role          JR2 role         Port / driver"
            )?;
        } else if let Some(target) = target {
            writeln!(writer, "  Roles for selected target {}:", target.name())?;
        }
        for number in 0..4 {
            let interface = device.interfaces.iter().find(|i| i.number == number);
            let port = interface
                .and_then(|i| i.port.as_ref())
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "no tty".into());
            let driver = interface
                .and_then(|i| i.driver.as_deref())
                .unwrap_or("unbound/unavailable");
            if let Some(target) = target {
                writeln!(
                    writer,
                    "  {} / {number:02}  {:16} {port} [{driver}]{}",
                    char::from(b'A' + number),
                    role(target, number),
                    if number == debug_interface(target) {
                        " <-- debug"
                    } else {
                        ""
                    }
                )?;
            } else {
                writeln!(
                    writer,
                    "  {} / {number:02}  {:16} {:16} {port} [{driver}]",
                    char::from(b'A' + number),
                    role(Target::K2, number),
                    role(Target::Jr2, number)
                )?;
            }
            if let Some(i) = interface
                && i.tty != i.port
                && let Some(tty) = &i.tty
            {
                writeln!(writer, "        currently {}", tty.display())?;
            }
        }
    }
    if count == 0 {
        writeln!(writer, "No matching FT4232H USB bridges found.")?;
    }
    Ok(())
}
