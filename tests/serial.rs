#![cfg(unix)]

use serialport::{SerialPort, TTYPort};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    thread,
    time::Duration,
};
use wdb::{bridge, config::Config, protocol::DebugPort, transport::Transport};

#[test]
fn serial_transport_uses_configured_port_and_timeout() {
    let (mut master, mut slave) = TTYPort::pair().unwrap();
    master.set_timeout(Duration::from_secs(3)).unwrap();
    slave.set_exclusive(false).unwrap();
    let path = slave.name().unwrap();
    drop(slave);
    let cfg = Config {
        port: Some(path),
        timeout: 1,
        ..Config::default()
    };
    let mut dp = DebugPort::new(Transport::open(&cfg).unwrap(), 1024).unwrap();
    let worker = thread::spawn(move || {
        let mut packet = [0; 8];
        master.read_exact(&mut packet).unwrap();
        assert_eq!(packet, [0x55, 0xFE, 0, 0, 0, 0, 0, 0xAB]);
        master.write_all(&[0xAA, 0, 5, 0xAF]).unwrap();
        // Keep the PTY master alive until the response has been consumed.
        thread::sleep(Duration::from_millis(100));
    });
    assert_eq!(dp.revision().unwrap(), 5);
    worker.join().unwrap();
}

#[test]
fn bridge_relays_a_complete_cli_reset_over_serial() {
    let (mut master, mut slave) = TTYPort::pair().unwrap();
    master.set_timeout(Duration::from_secs(3)).unwrap();
    slave.set_timeout(Duration::from_secs(3)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let relay = thread::spawn(move || {
        let (mut client, _) = listener.accept().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        bridge::relay(&mut client, &mut slave).unwrap();
    });
    let hardware = thread::spawn(move || {
        for command in [0x80, 0x91, 0x21] {
            let mut packet = [0; 8];
            master.read_exact(&mut packet).unwrap();
            assert_eq!(packet, [0x55, command, 0, 0, 0, 0, 0, 0x55 ^ command]);
            master.write_all(&[0xAA, 0, 0, 0xAA]).unwrap();
        }
        thread::sleep(Duration::from_millis(100));
    });
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wdb"))
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path())
        .env_remove("WDB_CONFIG_DIR")
        .args([
            "--port",
            &address.to_string(),
            "--target",
            "k2",
            "--state-dir",
        ])
        .arg(dir.path().join("state"))
        .arg("reset")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    relay.join().unwrap();
    hardware.join().unwrap();
}

#[test]
fn bridge_rejects_oversized_requests_before_serial_write() {
    let (mut master, mut slave) = TTYPort::pair().unwrap();
    master.set_timeout(Duration::from_millis(100)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    client.write_all(&[0x55, 1, 0, 0, 0, 0x10, 0]).unwrap();
    assert!(bridge::relay(&mut server, &mut slave).is_err());
    let mut buffer = [0];
    assert!(master.read(&mut buffer).is_err());
}
