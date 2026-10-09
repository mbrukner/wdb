use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};
use wdb::{
    config::{Config, Target},
    protocol,
    state::{State, Stopped},
};

struct Device {
    port: String,
    worker: thread::JoinHandle<Vec<Vec<u8>>>,
}

fn device(connections: usize, revision: u8, fail: Option<u8>) -> Device {
    flash_device(connections, revision, fail, false, false)
}

fn flash_device(
    connections: usize,
    revision: u8,
    fail: Option<u8>,
    delayed: bool,
    corrupt: bool,
) -> Device {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().to_string();
    listener.set_nonblocking(true).unwrap();
    let worker = thread::spawn(move || {
        let mut requests = Vec::new();
        let mut memory = HashMap::new();
        let mut flash_source = 0;
        for connection in 0..connections {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream.set_nodelay(true).unwrap();
            loop {
                let mut header = [0; 7];
                match stream.read(&mut header[..1]) {
                    Ok(0) => break,
                    Ok(_) => (),
                    Err(error) => panic!("read: {error}"),
                }
                stream.read_exact(&mut header[1..]).unwrap();
                assert_eq!(header[0], 0x55);
                let command = header[1];
                let address = (u32::from(header[2]) << 16)
                    | (u32::from(header[3]) << 8)
                    | u32::from(header[4]);
                let length = u16::from_be_bytes([header[5], header[6]]) as usize;
                let mut packet = header.to_vec();
                packet.resize(8 + if command == 1 { length } else { 0 }, 0);
                stream.read_exact(&mut packet[7..]).unwrap();
                assert_eq!(protocol::xor(&packet), 0, "request checksum");
                requests.push(packet.clone());
                if connection == 0 && fail == Some(command) {
                    stream.write_all(&[0xAA, 0, 0]).unwrap();
                    break;
                }
                if command == 1 {
                    for (i, byte) in packet[7..7 + length].iter().enumerate() {
                        memory.insert(address + i as u32, *byte);
                    }
                }
                if command == 0x11 {
                    memory.retain(|address, _| *address < 0xF80000);
                }
                if command == 0x12 {
                    let start = 0xF80000 + ((address >> 16) * 4096);
                    memory.retain(|address, _| *address < start || *address >= start + 4096);
                }
                if command == 0x10 || command == 0x13 {
                    // Match the unified RTL: 0x10 selects the source but only
                    // programs 8 KiB; 0x13 retains that source across sessions.
                    let destination = if command == 0x10 {
                        flash_source = address;
                        0xF80000
                    } else {
                        0xF80000 + ((address >> 16) * 4096)
                    };
                    for i in 0..8192 {
                        let byte = *memory.get(&(flash_source + i)).unwrap_or(&0xFF);
                        memory.insert(destination + i, byte);
                    }
                    if corrupt {
                        *memory.get_mut(&(destination + 1)).unwrap() ^= 0x80;
                    }
                }
                let mut response = vec![0xAA, 0, if command == 0xFE { revision } else { 0 }];
                if command == 0 {
                    for i in 0..length {
                        let offset = if delayed && address >= protocol::FLASH_BASE {
                            i.saturating_sub(1)
                        } else {
                            i
                        };
                        response.push(*memory.get(&(address + offset as u32)).unwrap_or(
                            &(if address >= 0xF80000 {
                                0xFF
                            } else {
                                (address + i as u32) as u8
                            }),
                        ));
                    }
                }
                response.push(protocol::xor(&response));
                // Deliberately fragment responses to exercise read_exact.
                stream.write_all(&response[..1]).unwrap();
                stream.write_all(&response[1..]).unwrap();
            }
        }
        requests
    });
    Device { port, worker }
}

fn invoke(dir: &Path, port: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wdb"))
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env_remove("WDB_CONFIG_DIR")
        .args(["--port", port, "--target", "k2", "--state-dir"])
        .arg(dir.join("state"))
        .args(args)
        .output()
        .unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn commands(requests: &[Vec<u8>]) -> Vec<u8> {
    requests.iter().map(|r| r[1]).collect()
}
fn state(dir: &Path, port: &str) -> State {
    State::new(&Config {
        port: Some(port.into()),
        target: Some(Target::K2),
        state_dir: Some(dir.join("state")),
        ..Config::default()
    })
    .unwrap()
}

#[test]
fn reset_clears_stopped_state_only_after_success() {
    // A hardware halt can outlive the local marker, so clear RDY regardless
    // of the recorded state. Flash boot must also override an earlier RAM boot.
    for prior in [
        None,
        Some(Stopped::Halted),
        Some(Stopped::Reset),
        Some(Stopped::Uncertain),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let d = device(2, 5, None);
        success(&invoke(dir.path(), &d.port, &["boot", "ram"]));
        let state = state(dir.path(), &d.port);
        if let Some(prior) = prior {
            state.set(prior).unwrap();
        }
        success(&invoke(dir.path(), &d.port, &["reset", "--quiet"]));
        assert_eq!(state.read().unwrap(), None);
        assert_eq!(
            commands(&d.worker.join().unwrap()),
            [0x80, 0x90, 0x81, 0x80, 0x91, 0x21]
        );
    }
}
#[test]
fn reset_failure_preserves_recovery_state() {
    let sequence = [0x80, 0x91, 0x21];
    for (index, command) in sequence.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let d = device(1, 5, Some(*command));
        let out = invoke(dir.path(), &d.port, &["reset"]);
        assert!(!out.status.success());
        assert_eq!(
            state(dir.path(), &d.port).read().unwrap(),
            Some(Stopped::Uncertain)
        );
        // In particular, never release reset if boot selection failed.
        assert_eq!(commands(&d.worker.join().unwrap()), sequence[..=index]);
    }
}
#[test]
fn reset_can_explicitly_boot_staged_ram() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, None);
    state(dir.path(), &d.port).set(Stopped::Reset).unwrap();
    success(&invoke(dir.path(), &d.port, &["reset", "--boot", "ram"]));
    assert_eq!(state(dir.path(), &d.port).read().unwrap(), None);
    assert_eq!(commands(&d.worker.join().unwrap()), [0x80, 0x90, 0x21]);
}
#[test]
fn stop_dump_start_does_not_reset() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(3, 5, None);
    for args in [
        vec!["stop"],
        vec!["dump", "--address", "2000", "--count", "10"],
        vec!["start"],
    ] {
        success(&invoke(dir.path(), &d.port, &args));
    }
    assert_eq!(commands(&d.worker.join().unwrap()), [0x20, 0, 0x21]);
}
#[test]
fn dump_chunks_and_prints_data_in_quiet_mode() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, None);
    let output = invoke(
        dir.path(),
        &d.port,
        &["--quiet", "dump", "--address", "2000", "--count", "401"],
    );
    success(&output);
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("002000: 00 01"));
    assert!(text.contains("002400: 00"));
    assert_eq!(commands(&d.worker.join().unwrap()), [0x80, 0, 0, 0x81]);
}
#[test]
fn binary_upload_is_chunked() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, None);
    std::fs::write(dir.path().join("image.bin"), vec![0xAB; 2050]).unwrap();
    success(&invoke(
        dir.path(),
        &d.port,
        &["binary", "image.bin", "--address", "10000"],
    ));
    let requests = d.worker.join().unwrap();
    assert_eq!(commands(&requests), [0x80, 1, 1, 1, 0x81]);
    assert_eq!(&requests[3][2..7], [1, 8, 0, 0, 2]);
}
#[test]
fn failed_upload_is_not_launched() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, Some(1));
    std::fs::write(dir.path().join("image.bin"), [0xEA]).unwrap();
    let out = invoke(
        dir.path(),
        &d.port,
        &["binary", "image.bin", "--address", "2000"],
    );
    assert!(!out.status.success());
    assert_eq!(commands(&d.worker.join().unwrap()), [0x80, 1]);
    assert_eq!(
        state(dir.path(), &d.port).read().unwrap(),
        Some(Stopped::Reset)
    );
}

#[test]
fn failed_enter_is_reasserted_before_retrying_upload() {
    for initial in [
        vec!["binary", "image.bin", "--address", "2000"],
        vec!["reset"],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let d = device(3, 5, Some(protocol::ENTER));
        std::fs::write(dir.path().join("image.bin"), [0xEA]).unwrap();
        assert!(!invoke(dir.path(), &d.port, &initial).status.success());
        assert_eq!(
            state(dir.path(), &d.port).read().unwrap(),
            Some(Stopped::Uncertain)
        );
        success(&invoke(
            dir.path(),
            &d.port,
            &["binary", "image.bin", "--address", "2000"],
        ));
        assert_eq!(
            state(dir.path(), &d.port).read().unwrap(),
            Some(Stopped::Reset)
        );
        success(&invoke(dir.path(), &d.port, &["start"]));
        assert_eq!(state(dir.path(), &d.port).read().unwrap(), None);
        assert_eq!(commands(&d.worker.join().unwrap()), [0x80, 0x80, 1, 0x21]);
    }
}

#[test]
fn uncertain_stop_does_not_claim_cpu_is_already_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(2, 5, Some(protocol::STOP));
    assert!(!invoke(dir.path(), &d.port, &["stop"]).status.success());
    assert_eq!(
        state(dir.path(), &d.port).read().unwrap(),
        Some(Stopped::Uncertain)
    );
    assert!(!invoke(dir.path(), &d.port, &["stop"]).status.success());
    success(&invoke(dir.path(), &d.port, &["start"]));
    assert_eq!(state(dir.path(), &d.port).read().unwrap(), None);
    assert_eq!(commands(&d.worker.join().unwrap()), [0x20, 0x21]);
}

#[test]
fn failed_release_requires_debug_entry_on_next_upload() {
    for release in [protocol::EXIT, protocol::START] {
        let dir = tempfile::tempdir().unwrap();
        let d = device(2, 5, Some(release));
        std::fs::write(dir.path().join("image.bin"), [0xEA]).unwrap();
        let upload = ["binary", "image.bin", "--address", "2000"];
        let initial = if release == protocol::START {
            state(dir.path(), &d.port).set(Stopped::Halted).unwrap();
            &["start"][..]
        } else {
            &upload[..]
        };
        assert!(!invoke(dir.path(), &d.port, initial).status.success());
        assert_eq!(
            state(dir.path(), &d.port).read().unwrap(),
            Some(Stopped::Uncertain)
        );
        success(&invoke(dir.path(), &d.port, &upload));
        assert_eq!(
            state(dir.path(), &d.port).read().unwrap(),
            Some(Stopped::Reset)
        );
        let requests = d.worker.join().unwrap();
        assert_eq!(&commands(&requests)[requests.len() - 2..], [0x80, 1]);
    }
}

#[test]
fn overlapping_upload_is_rejected_before_connecting_and_can_retry() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("image.bin"), [0xEA]).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().to_string();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let mut sessions = Vec::new();
        for session in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut commands = Vec::new();
            loop {
                let mut header = [0; 7];
                if stream.read(&mut header[..1]).unwrap() == 0 {
                    break;
                }
                stream.read_exact(&mut header[1..]).unwrap();
                let length = u16::from_be_bytes([header[5], header[6]]) as usize;
                let mut body = vec![
                    0;
                    1 + if header[1] == protocol::WRITE {
                        length
                    } else {
                        0
                    }
                ];
                stream.read_exact(&mut body).unwrap();
                commands.push(header[1]);
                if session == 0 && header[1] == protocol::ENTER {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                    // The rejected process must not have queued a TCP connection.
                    assert_eq!(
                        listener.accept().unwrap_err().kind(),
                        std::io::ErrorKind::WouldBlock
                    );
                }
                stream.write_all(&[0xAA, 0, 0, 0xAA]).unwrap();
            }
            sessions.push(commands);
        }
        sessions
    });
    let first_dir = dir.path().to_owned();
    let first_port = port.clone();
    let first = thread::spawn(move || {
        invoke(
            &first_dir,
            &first_port,
            &["binary", "image.bin", "--address", "2000"],
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let rejected = invoke(
        dir.path(),
        &port,
        &["binary", "image.bin", "--address", "2000"],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("lock endpoint"));
    release_tx.send(()).unwrap();
    success(&first.join().unwrap());
    success(&invoke(
        dir.path(),
        &port,
        &["binary", "image.bin", "--address", "2000"],
    ));
    assert_eq!(state(dir.path(), &port).read().unwrap(), None);
    assert_eq!(
        worker.join().unwrap(),
        vec![vec![0x80, 1, 0x81], vec![0x80, 1, 0x81]]
    );
}
#[test]
fn pgz_launch_has_readback_barrier_before_reset_release() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, None);
    std::fs::write(
        dir.path().join("app.pgz"),
        [b'Z', 0, 0x20, 0, 1, 0, 0, 0xEA, 0, 0x20, 0, 0, 0, 0],
    )
    .unwrap();
    success(&invoke(dir.path(), &d.port, &["run-pgz", "app.pgz"]));
    let requests = d.worker.join().unwrap();
    assert_eq!(commands(&requests), [0x80, 1, 1, 1, 1, 1, 1, 0, 0x81]);
    assert_eq!(&requests[6][7..15], b"CROSSDEV");
    assert_eq!(&requests[7][2..7], [0, 0, 0x80, 0, 8]);
}
#[test]
fn revision_query_does_not_reset_cpu() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, None);
    let out = invoke(dir.path(), &d.port, &["revision"]);
    success(&out);
    assert_eq!(out.stdout, b"5\n");
    assert_eq!(commands(&d.worker.join().unwrap()), [0xFE]);
}
#[test]
fn unsupported_keyboard_revision_never_writes() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 4, None);
    let out = invoke(dir.path(), &d.port, &["key", "a"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("revision 5"));
    assert_eq!(commands(&d.worker.join().unwrap()), [0xFE]);
}
#[test]
fn k2_key_injects_complete_press_and_release_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, None);
    success(&invoke(dir.path(), &d.port, &["key", "A"]));
    let requests = d.worker.join().unwrap();
    assert_eq!(commands(&requests), [0xFE, 1, 1]);
    assert_eq!(&requests[1][2..7], [0xF0, 0x1D, 0xE0, 0, 16]);
    assert_eq!(&requests[1][9..11], [0x10, 0x84]);
}
#[test]
fn jr2_key_uses_ps2_aperture() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, None);
    // A dedicated invocation avoids supplying --target twice.
    let out = Command::new(env!("CARGO_BIN_EXE_wdb"))
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path())
        .env_remove("WDB_CONFIG_DIR")
        .args(["--port", &d.port, "--target", "jr2", "key", "a"])
        .output()
        .unwrap();
    success(&out);
    let requests = d.worker.join().unwrap();
    assert_eq!(commands(&requests), [0xFE, 1, 1, 1]);
    assert_eq!(&requests[1][2..7], [0xF0, 0x16, 0x42, 0, 1]);
    assert_eq!(requests[1][7], 0x1C);
}
#[test]
fn copy_request_publishes_metadata_before_signature() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, None);
    std::fs::write(dir.path().join("copy.bin"), [1, 2, 3, 4]).unwrap();
    success(&invoke(dir.path(), &d.port, &["copy", "copy.bin"]));
    let requests = d.worker.join().unwrap();
    assert_eq!(commands(&requests), [0x80, 1, 1, 1, 0, 0x81]);
    assert_eq!(&requests[2][7..16], b"copy.bin\0");
    assert_eq!(&requests[3][7..15], b"COPYFILE");
}
#[test]
fn invalid_files_and_flash_plans_fail_before_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().to_string();
    std::fs::write(dir.path().join("bad.hex"), b":00000001FE").unwrap();
    std::fs::write(dir.path().join("short.bin"), [1, 2]).unwrap();
    std::fs::write(dir.path().join("bulk.csv"), b"01,missing.bin\n").unwrap();
    for args in [
        vec!["upload", "bad.hex"],
        vec!["flash", "short.bin", "--address", "10000", "--yes"],
        vec!["flash", "short.bin", "--flash-sector", "FF", "--yes"],
        vec!["flash-bulk", "bulk.csv", "--erase", "--yes"],
    ] {
        assert!(!invoke(dir.path(), &port, &args).status.success());
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
#[test]
fn declined_erase_never_connects() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    success(&invoke(
        dir.path(),
        &listener.local_addr().unwrap().to_string(),
        &["erase"],
    ));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
#[test]
fn explicit_config_and_flags_work_without_legacy_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, None);
    std::fs::write(
        dir.path().join("settings.toml"),
        "target = 'jr2'\nport = 'bad:1'\nchunk_size = 512\n",
    )
    .unwrap();
    let out = invoke(
        dir.path(),
        &d.port,
        &["--config", "settings.toml", "revision"],
    );
    success(&out);
    assert_eq!(commands(&d.worker.join().unwrap()), [0xFE]);
}

#[test]
fn full_flash_stages_complete_image_before_erasing() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(2, 5, None);
    let data: Vec<u8> = (0..512 * 1024).map(|i| (i / 8192) as u8).collect();
    std::fs::write(dir.path().join("flash.bin"), data).unwrap();
    let output = invoke(
        dir.path(),
        &d.port,
        &["flash", "flash.bin", "--address", "10000", "--yes"],
    );
    success(&output);
    let progress = String::from_utf8_lossy(&output.stderr);
    for phase in [
        "Staging RAM",
        "Checking RAM",
        "Erasing flash",
        "Programming flash",
        "Verifying flash",
    ] {
        assert!(progress.contains(phase));
    }
    assert_eq!(progress.matches("100%").count(), 5);
    assert!(!progress.contains(['\r', '\x1b']));
    assert!(output.stdout.is_empty());
    assert_eq!(state(dir.path(), &d.port).flash_source().unwrap(), 0x10000);
    std::fs::write(dir.path().join("sector.bin"), vec![0xCD; 8192]).unwrap();
    let output = invoke(
        dir.path(),
        &d.port,
        &[
            "flash",
            "sector.bin",
            "--flash-sector",
            "02",
            "--yes",
            "--quiet",
        ],
    );
    success(&output);
    assert!(output.stderr.is_empty());
    assert!(output.stdout.is_empty());
    let requests = d.worker.join().unwrap();
    assert!(requests[1..513].iter().all(|r| r[1] == 1));
    assert!(requests[513..1025].iter().all(|r| r[1] == 0));
    assert_eq!(commands(&requests[1025..1027]), [0x11, 0x10]);
    assert_eq!(&requests[1026][2..5], [1, 0, 0]);
    let end = requests.iter().position(|r| r[1] == 0x81).unwrap();
    let sectors: Vec<_> = requests[..end]
        .iter()
        .filter(|r| r[1] == 0x13)
        .map(|r| r[2])
        .collect();
    assert_eq!(sectors, (1..64).map(|i| i * 2).collect::<Vec<u8>>());
    assert!(requests[end - 512..end].iter().all(|r| r[1] == 0));
    assert_eq!(&requests[end + 2][2..5], [1, 0, 0]);
}

#[test]
fn flash_failure_identifies_the_pending_operation() {
    for (command, stage) in [
        (protocol::ERASE_SECTOR, "erase 1/2"),
        (protocol::PROGRAM_SECTOR, "program"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let d = device(1, 5, Some(command));
        std::fs::write(dir.path().join("sector.bin"), vec![0xCD; 8192]).unwrap();
        let output = invoke(
            dir.path(),
            &d.port,
            &["flash", "sector.bin", "--flash-sector", "01", "--yes"],
        );
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(&format!("(1/1) Sector 01: {stage}...")));
        assert!(!error.contains("100%"));
        assert_eq!(d.worker.join().unwrap().last().unwrap()[1], command);
    }
}

#[test]
fn sector_programming_erases_both_halves_and_uses_sector_address() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, None);
    std::fs::write(dir.path().join("sector.bin"), vec![0xCD; 8192]).unwrap();
    success(&invoke(
        dir.path(),
        &d.port,
        &["flash", "sector.bin", "--flash-sector", "3F", "--yes"],
    ));
    let requests = d.worker.join().unwrap();
    assert!(requests[9..17].iter().all(|r| r[1] == 0));
    assert_eq!(commands(&requests[17..20]), [0x12, 0x12, 0x13]);
    assert!(requests[20..requests.len() - 1].iter().all(|r| r[1] == 0));
    assert_eq!(requests.last().unwrap()[1], 0x81);
    assert_eq!(&requests[17][2..5], [0x7E, 0, 0]);
    assert_eq!(&requests[18][2..5], [0x7F, 0, 0]);
    assert_eq!(&requests[19][2..5], [0x7E, 0, 0]);
}

#[test]
fn bulk_flash_resolves_files_relative_to_csv() {
    let dir = tempfile::tempdir().unwrap();
    let d = device(1, 5, None);
    std::fs::create_dir(dir.path().join("images")).unwrap();
    std::fs::write(dir.path().join("images/sector.bin"), vec![0xCD; 8192]).unwrap();
    std::fs::write(dir.path().join("images/plan.csv"), "02,sector.bin\n").unwrap();
    success(&invoke(
        dir.path(),
        &d.port,
        &["flash-bulk", "images/plan.csv", "--erase", "--yes"],
    ));
    let requests = d.worker.join().unwrap();
    assert_eq!(&commands(&requests)[..2], [0x80, 0x11]);
    let program = requests.iter().position(|r| r[1] == 0x13).unwrap();
    assert!(
        requests[program + 1..requests.len() - 1]
            .iter()
            .all(|r| r[1] == 0)
    );
    assert_eq!(requests.last().unwrap()[1], 0x81);
    assert_eq!(&requests[program][2..5], [4, 0, 0]);
}

#[test]
fn bulk_flash_verifies_delayed_readback_but_rejects_real_corruption() {
    for corrupt in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let d = flash_device(1, 5, None, true, corrupt);
        let data: Vec<u8> = (0..8192).map(|i| (i % 256) as u8).collect();
        std::fs::write(dir.path().join("3b.bin"), data).unwrap();
        std::fs::write(dir.path().join("update.csv"), "3b,3b.bin\n").unwrap();
        let output = invoke(dir.path(), &d.port, &["flash-bulk", "update.csv", "--yes"]);
        assert_eq!(
            output.status.success(),
            !corrupt,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let requests = d.worker.join().unwrap();
        if corrupt {
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("sector 3B from 3b.bin"));
            assert!(error.contains("(1/1) Sector 3B: failed"));
            assert!(!error.contains("100%"));
            assert!(error.contains("offset 076001 (physical FF6001): expected 01, read 81"));
            assert!(!commands(&requests).contains(&0x81));
            assert_eq!(
                state(dir.path(), &d.port).read().unwrap(),
                Some(Stopped::Reset)
            );
        } else {
            assert_eq!(requests.last().unwrap()[1], 0x81);
        }
    }
}
