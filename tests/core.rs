use clap::Parser;
use std::{
    io::{self, Cursor, Read, Write},
    path::Path,
};
use wdb::{
    cli::{Cli, hex},
    config::{Config, Cpu, Target},
    keyboard::{self, Input},
    loader::{self, Format},
    protocol::{self, Capabilities, DebugPort},
    state::{State, Stopped},
    util,
};

#[derive(Default)]
struct Fake {
    reads: Cursor<Vec<u8>>,
    writes: Vec<u8>,
}
impl Read for Fake {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.reads.read(b)
    }
}
impl Write for Fake {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.writes.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn response(data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0xAA, 0, 0];
    bytes.extend_from_slice(data);
    bytes.push(protocol::xor(&bytes));
    bytes
}

#[test]
fn request_checksum_includes_low_length_byte() {
    assert_eq!(
        protocol::packet(1, 0x123456, &[0xAB], 0).unwrap(),
        [0x55, 1, 0x12, 0x34, 0x56, 0, 1, 0xAB, 0x8E]
    );
}
#[test]
fn responses_validate_checksum_and_errors_but_allow_cpu_status() {
    let mut good = Cursor::new(response(&[1, 2]));
    assert_eq!(protocol::read_response(&mut good, 2).unwrap().data, [1, 2]);
    let mut bad = response(&[]);
    bad[3] ^= 1;
    assert!(protocol::read_response(&mut Cursor::new(bad), 0).is_err());
    assert!(protocol::read_response(&mut Cursor::new([0xAA, 4, 0, 0xAE]), 0).is_err());
    assert!(protocol::read_response(&mut Cursor::new([0xAA, 8, 0, 0xA2]), 0).is_ok());
}
#[test]
fn short_reply_is_an_error() {
    assert!(protocol::read_response(&mut Cursor::new([0xAA, 0, 0]), 0).is_err());
}
#[test]
fn sync_search_is_bounded() {
    assert!(protocol::read_response(&mut Cursor::new(vec![0; 4097]), 0).is_err());
}
#[test]
fn reads_are_chunked_and_reassembled() {
    let mut bytes = response(&vec![1; 1024]);
    bytes.extend(response(&[2, 3]));
    let mut dp = DebugPort::new(
        Fake {
            reads: Cursor::new(bytes),
            ..Fake::default()
        },
        1024,
    )
    .unwrap();
    let data = dp.read(0x120000, 1026).unwrap();
    assert_eq!(&data[1024..], [2, 3]);
    assert_eq!(&dp.transport.writes[2..7], [0x12, 0, 0, 4, 0]);
    assert_eq!(&dp.transport.writes[10..15], [0x12, 4, 0, 0, 2]);
}
#[test]
fn writes_are_chunked() {
    let mut bytes = response(&[]);
    bytes.extend(response(&[]));
    let mut dp = DebugPort::new(
        Fake {
            reads: Cursor::new(bytes),
            ..Fake::default()
        },
        1024,
    )
    .unwrap();
    dp.write(0x2000, &vec![0xCD; 1025]).unwrap();
    assert_eq!(dp.transport.writes.len(), 1025 + 16);
    assert_eq!(&dp.transport.writes[1034..1039], [0, 0x24, 0, 0, 1]);
}
#[test]
fn invalid_ranges_never_write() {
    let mut dp = DebugPort::new(Fake::default(), 1024).unwrap();
    assert!(dp.read(0xFFFFFF, 2).is_err());
    assert!(dp.write(0x1000000, &[1]).is_err());
    assert!(dp.transport.writes.is_empty());
}
#[test]
fn hex_values_are_strict() {
    for text in ["100", "0x100", "0X100", "$100"] {
        assert_eq!(hex(text).unwrap(), 256);
    }
    for text in ["", "100oops", "-1", "0x", "100000000"] {
        assert!(hex(text).is_err());
    }
}
#[test]
fn cli_only_accepts_supported_targets_and_cpus() {
    assert!(Cli::try_parse_from(["wdb", "--target", "JR2", "--cpu", "65816", "reset"]).is_ok());
    for target in ["unknown", "a2560"] {
        assert!(Cli::try_parse_from(["wdb", "--target", target, "reset"]).is_err());
    }
    assert!(Cli::try_parse_from(["wdb", "--cpu", "68000", "reset"]).is_err());
    assert!(Cli::try_parse_from(["wdb", "reset", "extra"]).is_err());
}
#[test]
fn capabilities_depend_on_revision_and_target() {
    assert!(
        Capabilities::new(Target::K2, 4)
            .require_keyboard(Target::K2)
            .is_err()
    );
    let jr2 = Capabilities::new(Target::Jr2, 5);
    assert!(jr2.ps2);
    assert!(!jr2.optical);
    assert!(Capabilities::new(Target::K2, 5).optical);
}
#[test]
fn pgx_65c02_publishes_marker_after_vectors() {
    let image = loader::parse(b"PGX\x03\x59\x03\x00\x00\xEA", Format::Pgx, Cpu::C65c02).unwrap();
    assert_eq!(
        image.blocks.iter().map(|b| b.address).collect::<Vec<_>>(),
        [0x359, 0xFFFC, 0x88, 0xFA, 0x80]
    );
    assert_eq!(image.blocks.last().unwrap().data, b"CROSSDEV");
    assert!(image.verify_launch);
}
#[test]
fn pgx_cpu_and_version_are_validated() {
    for data in [
        b"PGX\x01\x00\x20\x00\x00\xEA",
        b"PGX\x13\x00\x20\x00\x00\xEA",
    ] {
        assert!(loader::parse(data, Format::Pgx, Cpu::C65c02).is_err());
    }
}
#[test]
fn banked_65816_entry_uses_long_jump_stub() {
    let image = loader::parse(b"PGX\x01\x56\x34\x12\x00\xEA", Format::Pgx, Cpu::C65816).unwrap();
    assert_eq!(image.blocks[1].address, 0xFF80);
    assert_eq!(image.blocks[1].data, [0x18, 0xFB, 0x5C, 0x56, 0x34, 0x12]);
    assert_eq!(image.blocks[2].data, [0x80, 0xFF]);
    assert!(!image.verify_launch);
}
#[test]
fn bank_zero_65816_uses_direct_vector() {
    let image = loader::parse(b"PGX\x01\x00\x20\x00\x00\xEA", Format::Pgx, Cpu::C65816).unwrap();
    assert_eq!(image.blocks.len(), 2);
    assert_eq!(image.blocks[1].data, [0, 0x20]);
}
#[test]
fn pgz_handles_address_zero_data_and_defers_launch_marker() {
    let data = [
        b'Z', 0, 0, 0, 1, 0, 0, 0xEA, 0, 0x20, 0, 0, 0, 0, 0, 0x30, 0, 1, 0, 0, 0x60,
    ];
    let image = loader::parse(&data, Format::Pgz, Cpu::C65c02).unwrap();
    assert_eq!(image.blocks[0].address, 0);
    assert_eq!(image.blocks[0].data, [0xEA]);
    assert_eq!(image.blocks[1].address, 0x3000);
    assert_eq!(image.blocks.last().unwrap().data, b"CROSSDEV");
}
#[test]
fn pgz_supports_32_bit_fields_and_detects_truncation() {
    let data = [
        b'z', 0, 0x20, 0, 0, 1, 0, 0, 0, 0xEA, 0, 0x20, 0, 0, 0, 0, 0, 0,
    ];
    assert_eq!(
        loader::parse(&data, Format::Pgz, Cpu::C65c02)
            .unwrap()
            .entry,
        Some(0x2000)
    );
    assert!(loader::parse(&data[..data.len() - 1], Format::Pgz, Cpu::C65c02).is_err());
}
#[test]
fn out_of_range_executables_are_rejected() {
    assert!(loader::parse(b"PGX\x03\x00\x00\x01\x00\xEA", Format::Pgx, Cpu::C65c02).is_err());
    assert!(loader::parse(b"PGX\x01\x00\x00\x00\x01\xEA", Format::Pgx, Cpu::C65816).is_err());
}
#[test]
fn wdc_block_and_terminator() {
    let data = [b'Z', 0, 0x20, 0, 2, 0, 0, 0xEA, 0x60, 0, 0, 0, 0, 0, 0];
    let image = loader::parse(&data, Format::Wdc, Cpu::C65c02).unwrap();
    assert_eq!(image.blocks[0].address, 0x2000);
    assert_eq!(image.blocks[0].data, [0xEA, 0x60]);
    assert_eq!(image.entry, None);
}
#[test]
fn intel_hex_checksums_and_extended_address() {
    let data = b":020000040001F9\n:02100000EA60A4\n:00000001FF\n";
    let image = loader::parse(data, Format::IntelHex, Cpu::C65c02).unwrap();
    assert_eq!(image.blocks[0].address, 0x11000);
    assert_eq!(image.blocks[0].data, [0xEA, 0x60]);
    let mut bad = data.to_vec();
    bad[30] = b'B';
    assert!(loader::parse(&bad, Format::IntelHex, Cpu::C65c02).is_err());
}
#[test]
fn srec_checksums_and_counts() {
    let data = b"S1052000EA6090\nS9032000DC\n";
    let image = loader::parse(data, Format::Srec, Cpu::C65c02).unwrap();
    assert_eq!(image.blocks[0].address, 0x2000);
    assert_eq!(image.blocks[0].data, [0xEA, 0x60]);
    assert!(loader::parse(b"S1052000EA6091", Format::Srec, Cpu::C65c02).is_err());
    assert!(loader::parse(b"S1062000EA6090", Format::Srec, Cpu::C65c02).is_err());
}
#[test]
fn keyboard_encodings_match_matrix_and_set2() {
    let Input::Optical(frames) = keyboard::text(Target::K2, "A").unwrap() else {
        panic!()
    };
    assert_eq!(frames[0][2..4], [0x10, 0x84]);
    assert_eq!(frames[1], keyboard::empty_snapshot());
    let Input::Optical(frames) = keyboard::keys(Target::K2, &["run-stop".into()]).unwrap() else {
        panic!()
    };
    assert_eq!(frames[0][14..16], [0x70, 0x80]);
    let Input::Ps2(strokes) = keyboard::text(Target::Jr2, "tA").unwrap() else {
        panic!()
    };
    assert_eq!(strokes[0], [0x2C, 0xF0, 0x2C]);
    assert_eq!(strokes[1], [0x12, 0x1C, 0xF0, 0x1C, 0xF0, 0x12]);
    assert!(keyboard::text(Target::K2, "é").is_err());
}
#[test]
fn copy_payload_uses_kernel_crc_and_layout() {
    assert_eq!(util::crc32(b"COPYFILE"), 0xB792616B);
    assert_eq!(util::crc32(&[1, 2, 3, 4]), 0x977824D1);
    let payload = util::copy_payload(Path::new("dir/a.bin"), &[1, 2, 3, 4]).unwrap();
    assert_eq!(&payload[..6], b"a.bin\0");
    assert_eq!(&payload[6..10], [0xD1, 0x24, 0x78, 0x97]);
    assert_eq!(&payload[10..], [4, 0, 0, 1, 2, 3, 4]);
}
#[test]
fn stop_state_is_separate_for_endpoints_and_targets() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config {
        port: Some("127.0.0.1:2560".into()),
        target: Some(Target::K2),
        state_dir: Some(dir.path().into()),
        ..Config::default()
    };
    let state = State::new(&cfg).unwrap();
    state.set(Stopped::Halted).unwrap();
    cfg.port = Some("tcp://127.0.0.1:2560".into());
    assert_eq!(
        State::new(&cfg).unwrap().read().unwrap(),
        Some(Stopped::Halted)
    );
    cfg.port = Some("127.0.0.1:2561".into());
    assert_eq!(State::new(&cfg).unwrap().read().unwrap(), None);
    cfg.port = Some("127.0.0.1:2560".into());
    cfg.target = Some(Target::Jr2);
    assert_eq!(State::new(&cfg).unwrap().read().unwrap(), None);
    state.clear().unwrap();
    assert_eq!(state.read().unwrap(), None);
}

#[test]
fn endpoint_lock_is_shared_across_targets_and_released_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config {
        port: Some("127.0.0.1:2560".into()),
        target: Some(Target::K2),
        state_dir: Some(dir.path().into()),
        ..Config::default()
    };
    let lock = State::new(&cfg).unwrap().lock().unwrap();
    cfg.target = Some(Target::Jr2);
    cfg.port = Some("tcp://127.0.0.1:2560".into());
    assert!(State::new(&cfg).unwrap().lock().is_err());
    cfg.port = Some("127.0.0.1:2561".into());
    let _other = State::new(&cfg).unwrap().lock().unwrap();
    cfg.port = Some("127.0.0.1:2560".into());
    drop(lock);
    assert!(State::new(&cfg).unwrap().lock().is_ok());
}
#[test]
fn label_parser_and_dump_preserve_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.lbl");
    std::fs::write(&path, "; comment\n  example = $1234 ; comment\n").unwrap();
    assert_eq!(util::lookup(&path, "example").unwrap(), 0x1234);
    let mut out = Vec::new();
    util::dump(&mut out, &[b'A', 0], 0x1234).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.starts_with("001234: 41 00 "));
    assert!(text.ends_with(" | A.\n"));
}
