#![cfg(target_os = "linux")]

use clap::Parser;
use std::{
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use wdb::{
    cli::Cli,
    config::{Config, Target},
    discovery::{self, Device},
};

struct Usb {
    _dir: TempDir,
    root: PathBuf,
    dev: PathBuf,
}
impl Usb {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sys");
        let dev = dir.path().join("dev");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(dev.join("serial/by-id")).unwrap();
        Self {
            _dir: dir,
            root,
            dev,
        }
    }
    fn board(&self, location: &str, serial: Option<&str>, product: &str) {
        let path = self.root.join(location);
        fs::create_dir_all(&path).unwrap();
        for (name, value) in [
            ("idVendor", "0403"),
            ("idProduct", "6011"),
            ("manufacturer", "Xilinx"),
            ("product", product),
        ] {
            fs::write(path.join(name), value).unwrap();
        }
        if let Some(serial) = serial {
            fs::write(path.join("serial"), serial).unwrap();
        }
    }
    fn interface(&self, location: &str, number: u8, tty: Option<u8>) {
        let path = self.root.join(format!("{location}:1.{number}"));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("bInterfaceNumber"), format!("{number:02x}\n")).unwrap();
        if let Some(tty) = tty {
            let name = format!("ttyUSB{tty}");
            fs::create_dir_all(path.join(&name).join("tty").join(&name)).unwrap();
            fs::write(self.dev.join(name), []).unwrap();
            symlink("/sys/bus/usb/drivers/ftdi_sio", path.join("driver")).unwrap();
        }
    }
    fn alias(&self, tty: u8, alias: &str) -> PathBuf {
        let path = self.dev.join("serial/by-id").join(alias);
        symlink(format!("../../ttyUSB{tty}"), &path).unwrap();
        path
    }
    fn scan(&self) -> Vec<Device> {
        discovery::scan_linux(&self.root, &self.dev).unwrap()
    }
}

#[test]
fn k2_selects_interface_c_independent_of_tty_number() {
    let usb = Usb::new();
    usb.board("1-5", Some("K2-SERIAL"), "New Retro Computer");
    for (number, tty) in [(0, 8), (1, 3), (2, 17), (3, 1)] {
        usb.interface("1-5", number, Some(tty));
    }
    let stable = usb.alias(17, "usb-Xilinx_New_Retro_Computer_K2-SERIAL-if02-port0");
    let devices = usb.scan();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].interfaces.len(), 4);
    assert_eq!(
        discovery::select(&devices, Target::K2, None).unwrap(),
        stable
    );
    assert_eq!(
        discovery::select(&devices, Target::Jr2, None).unwrap(),
        usb.dev.join("ttyUSB3")
    );
}

#[test]
fn jtag_can_be_unbound_without_hiding_the_board() {
    let usb = Usb::new();
    usb.board("1-5", Some("BOARD"), "New Retro Computer");
    usb.interface("1-5", 0, None);
    usb.interface("1-5", 2, Some(7));
    let devices = usb.scan();
    assert!(devices[0].interfaces[0].port.is_none());
    assert!(devices[0].interfaces[0].driver.is_none());
    assert_eq!(
        discovery::select(&devices, Target::K2, None).unwrap(),
        usb.dev.join("ttyUSB7")
    );
    let mut out = Vec::new();
    discovery::display(&mut out, &devices, Some(Target::K2), None).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("FPGA JTAG        no tty [unbound/unavailable]"));
}

#[test]
fn missing_debug_interface_never_falls_back_to_another_uart() {
    let usb = Usb::new();
    usb.board("1-5", Some("BOARD"), "New Retro Computer");
    for number in [0, 1, 3] {
        usb.interface("1-5", number, Some(number));
    }
    let error = discovery::select(&usb.scan(), Target::K2, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("C / 02"));
    assert!(error.contains("no tty"));
}

#[test]
fn multiple_boards_require_a_selector() {
    let usb = Usb::new();
    for (location, serial, tty) in [("1-5", "FIRST", 2), ("2-3", "SECOND", 6)] {
        usb.board(location, Some(serial), "New Retro Computer");
        usb.interface(location, 2, Some(tty));
    }
    let devices = usb.scan();
    assert!(
        discovery::select(&devices, Target::K2, None)
            .unwrap_err()
            .to_string()
            .contains("multiple")
    );
    assert_eq!(
        discovery::select(&devices, Target::K2, Some("SECOND")).unwrap(),
        usb.dev.join("ttyUSB6")
    );
    assert_eq!(
        discovery::select(&devices, Target::K2, Some("usb:1-5")).unwrap(),
        usb.dev.join("ttyUSB2")
    );
    assert!(discovery::select(&devices, Target::K2, Some("MISSING")).is_err());
}

#[test]
fn duplicate_and_absent_serials_stay_separate_by_usb_parent() {
    for serial in [None, Some("DUPLICATE")] {
        let usb = Usb::new();
        for (location, tty) in [("1-5", 2), ("1-5.1", 6)] {
            usb.board(location, serial, "New Retro Computer");
            usb.interface(location, 2, Some(tty));
        }
        let devices = usb.scan();
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].interfaces.len(), 1);
        assert_eq!(devices[1].interfaces.len(), 1);
        if let Some(serial) = serial {
            assert!(discovery::select(&devices, Target::K2, Some(serial)).is_err());
        }
        assert_eq!(
            discovery::select(&devices, Target::K2, Some("usb:1-5.1")).unwrap(),
            usb.dev.join("ttyUSB6")
        );
    }
}

#[test]
fn generic_ftdi_adapters_require_explicit_selection() {
    let usb = Usb::new();
    usb.board("1-2", Some("ADAPTER"), "FT4232H");
    usb.interface("1-2", 2, Some(2));
    let devices = usb.scan();
    assert!(!devices[0].recognized());
    assert!(discovery::select(&devices, Target::K2, None).is_err());
    assert_eq!(
        discovery::select(&devices, Target::K2, Some("ADAPTER")).unwrap(),
        usb.dev.join("ttyUSB2")
    );
    // A different PID must never become an automatic four-channel candidate.
    fs::write(usb.root.join("1-2/idProduct"), "6001").unwrap();
    assert!(usb.scan().is_empty());
}

#[test]
fn stable_paths_survive_tty_renumbering() {
    let usb = Usb::new();
    fs::write(usb.dev.join("ttyUSB2"), []).unwrap();
    let alias = usb.alias(2, "usb-BOARD-if02-port0");
    let before = discovery::stable_port(&usb.dev.join("ttyUSB2"), &usb.dev.join("serial/by-id"));
    fs::rename(usb.dev.join("ttyUSB2"), usb.dev.join("ttyUSB9")).unwrap();
    fs::remove_file(&alias).unwrap();
    symlink("../../ttyUSB9", &alias).unwrap();
    let after = discovery::stable_port(&usb.dev.join("ttyUSB9"), &usb.dev.join("serial/by-id"));
    assert_eq!(before, after);
    assert_eq!(after, alias);
}

#[test]
fn serial_symlinks_are_not_required_for_discovery() {
    let usb = Usb::new();
    usb.board("3-7", Some("BOARD"), "New Retro Computer");
    usb.interface("3-7", 1, Some(11));
    fs::remove_dir_all(usb.dev.join("serial")).unwrap();
    assert_eq!(
        discovery::select(&usb.scan(), Target::Jr2, None).unwrap(),
        usb.dev.join("ttyUSB11")
    );
}

#[test]
fn missing_interface_attributes_are_not_guessed() {
    let usb = Usb::new();
    usb.board("3-7", Some("BOARD"), "New Retro Computer");
    usb.interface("3-7", 2, Some(2));
    fs::write(usb.root.join("3-7:1.2/bInterfaceNumber"), "oops").unwrap();
    assert!(discovery::select(&usb.scan(), Target::K2, None).is_err());
}

#[test]
fn listing_without_target_shows_both_role_maps() {
    let usb = Usb::new();
    usb.board("1-5", Some("BOARD"), "New Retro Computer");
    let mut out = Vec::new();
    discovery::display(&mut out, &usb.scan(), None, None).unwrap();
    let text = String::from_utf8(out).unwrap();
    for text_part in [
        "K2 role",
        "JR2 role",
        "RP2040 UART",
        "Wi-Fi UART",
        "FPGA JTAG",
    ] {
        assert!(text.contains(text_part));
    }
}

#[test]
fn explicit_ports_bypass_usb_discovery() {
    for port in ["127.0.0.1:2560", "/dev/custom-uart"] {
        let mut cfg = Config {
            port: Some(port.into()),
            ..Config::default()
        };
        discovery::resolve(&mut cfg).unwrap();
        assert_eq!(cfg.port().unwrap(), port);
        cfg.device = Some("BOARD".into());
        assert!(discovery::resolve(&mut cfg).is_err());
    }
}

fn config(dir: &Path, text: &str, flags: &[&str]) -> Config {
    let path = dir.join("wdb.toml");
    fs::write(&path, text).unwrap();
    let mut args = vec!["wdb", "--config", path.to_str().unwrap()];
    args.extend_from_slice(flags);
    args.push("devices");
    Config::load(&Cli::try_parse_from(args).unwrap()).unwrap()
}

#[test]
fn command_line_selector_overrides_configured_port_and_vice_versa() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), "port = '/dev/old'", &["--device", "BOARD"]);
    assert_eq!(cfg.device.as_deref(), Some("BOARD"));
    assert!(cfg.port.is_none());
    let cfg = config(dir.path(), "device = 'OLD'", &["--port", "/dev/manual"]);
    assert!(cfg.device.is_none());
    assert_eq!(cfg.port().unwrap(), "/dev/manual");
    let cfg = config(
        dir.path(),
        "port = '/dev/old'",
        &["--port", "auto", "--device", "BOARD"],
    );
    assert_eq!(cfg.port().unwrap(), "auto");
    assert_eq!(cfg.device.as_deref(), Some("BOARD"));
    let cfg = config(dir.path(), "device = 'SAVED'", &["--port", "auto"]);
    assert_eq!(cfg.device.as_deref(), Some("SAVED"));
    assert_eq!(cfg.port().unwrap(), "auto");
}
