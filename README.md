# wdb — Wildbits Debug Bridge

`wdb` is a standalone Rust command-line tool for Wildbits **JR2** and **K2**.
It controls the CPU, transfers programs, reads memory, programs firmware flash,
injects keyboard input, and serves a serial debug connection over TCP.
Executable loaders support **65C02** and **65816** CPUs.

## Build and install

Requires Rust 1.99 or newer. Dependencies are pinned in `Cargo.lock`.

```sh
cargo build --release --locked
install -Dm755 target/release/wdb ~/.local/bin/wdb
wdb --help
```

The binary has no Go or Python runtime dependency. Linux is the development
and test platform; the serial library also supports other operating systems.

## Quick start

```sh
wdb devices
wdb --target k2 revision
wdb --target k2 capabilities
wdb --target k2 reset
wdb --target jr2 --port 127.0.0.1:2560 dump --address 2000 --count 100
```

Specify the target using flags or configuration. On Linux, omitting `--port`
automatically selects the target's binary debug interface on the single matching
USB bridge. `--port` still accepts a serial path or TCP address; `--port auto`
overrides a configured port. Target flags accept `jr2`/`k2` in either case.
Target selection describes the connected board; neither the shared USB descriptor
nor the debug revision identifies its model automatically.

Configuration is optional. The first available file is used in this order:

1. `--config PATH` (when supplied, this exact file is required)
2. `./wdb.toml`
3. `$WDB_CONFIG_DIR/wdb.toml`
4. `$XDG_CONFIG_HOME/wdb/wdb.toml`, or `~/.config/wdb/wdb.toml`

Flags override configuration. See [wdb.toml.example](wdb.toml.example).
No configuration is needed for help, version output, or serial-port listing.

```toml
target = "k2"
# Optional when more than one board is connected:
# device = "138SA20405"
cpu = "65c02"
```

With that saved in `wdb.toml`, commands become `wdb reset`, `wdb key enter`, etc.
`--quiet` suppresses progress on stderr while retaining command results on stdout.
Transport reads and writes use `--timeout` seconds (default 60).

## USB discovery

JR2 and K2 use an FT4232H (`0403:6011`) with four USB interfaces. Their roles,
from the K2 B6C and JR2 A2A schematics, are:

| Channel / interface | K2 | JR2 |
| --- | --- | --- |
| A / 00 | FPGA JTAG | FPGA JTAG |
| B / 01 | RP2040 supervisor UART | Binary debug |
| C / 02 | Binary debug | CPU UART |
| D / 03 | CPU UART | WROOM Wi-Fi UART |

`wdb devices` groups interfaces by their physical USB parent, displaying the USB
serial, location, stable path, current tty, and kernel driver. Without a target it
shows both role maps; `wdb --target k2 devices` shows the selected target's roles.
The interface number comes from USB metadata, never the `ttyUSB` suffix.

```sh
wdb devices
wdb --target k2 --device 138SA20405 revision
wdb --target jr2 --device usb:1-5 revision
```

Automatic selection recognizes the shared `New Retro Computer` USB product name.
Other FT4232H devices are listed but require an explicit `--device SERIAL` or
`--device usb:LOCATION`. Multiple matching devices produce an error instead of
choosing the first. Missing or duplicate serials can be disambiguated by USB
location. This is model selection by the user, not model detection from the USB
serial. `--device` overrides a configured port; an explicit serial/TCP `--port`
overrides a configured device selector. Supplying both explicitly is rejected
unless the port is `auto`.

Discovery only reads Linux sysfs and `/dev/serial/by-id`; it does not open UARTs,
probe unrelated channels, detach drivers, or claim USB interfaces. JTAG software
may detach the tty driver from A, leaving only three tty devices. The USB parent
and interfaces are still listed, and debug discovery does not require A to have a
tty. An absent tty is reported as such, not as proof that a specific process owns
it. If the selected debug channel itself has no tty, the command fails rather
than falling back to JTAG or another UART.

Stable `/dev/serial/by-id` paths are preferred when available. Stopped state uses
the same identity even when a raw tty path is supplied, so tty renumbering does
not transfer state to another board. Without a by-id link, discovery falls back
to the current tty path. Non-Linux systems still use `list-ports` and explicit
serial/TCP ports.

## Commands

Addresses, byte counts, sectors, and scan bytes are **hexadecimal**, accepting
plain digits, `0x`, or `$` prefixes. Quote `$` values in a shell. Baud rates and
timeouts are decimal; TOML numeric fields use normal TOML syntax.

| Command | Behavior |
| --- | --- |
| `dump --address ADDR --count COUNT` | Hex and ASCII memory dump; count defaults to `10` (16 bytes) |
| `binary FILE --address ADDR` | Upload a raw binary |
| `upload FILE` | Upload Intel HEX, verifying record checksums |
| `upload-srec FILE` | Upload Motorola S-records, verifying checksums and lengths |
| `upload-wdc FILE` | Upload WDCTools block format |
| `run-pgx FILE` | Upload a PGX executable, validate its CPU, and install launch vectors |
| `run-pgz FILE` | Upload PGZ blocks with 24- or 32-bit address/size fields and optional entry point |
| `stop` | Halt the CPU without asserting reset |
| `start` | Resume a CPU stopped by `wdb` |
| `reset [--boot flash\|ram]` | Reset the CPU, restore flash boot by default, and clear any debug halt and saved stopped state |
| `boot ram` / `boot flash` | Select boot lookup tables |
| `revision` | Print the debug revision in hex, without resetting the CPU |
| `capabilities` | Show revision and available keyboard injection features |
| `key KEY [KEY...]` | Inject individual named keys or characters |
| `type TEXT` | Type a quoted string |
| `scancodes BYTE [BYTE...]` | Inject raw PS/2 Set-2 bytes |
| `copy FILE` | Stage a kernel SD-copy request |
| `lookup LABEL --label-file FILE --count COUNT` | Display memory at a 64TASS label |
| `deref LABEL --label-file FILE --count COUNT` | Follow a 24-bit little-endian pointer at a label |
| `erase` | Erase the entire firmware flash |
| `flash FILE --address ADDR` | Stage a 512 KiB firmware image in RAM, erase flash, program, and verify |
| `flash FILE --flash-sector SECTOR` | Program one 8 KiB sector, numbered `00`–`3F` |
| `flash-bulk FILE [--erase]` | Program sectors from a CSV manifest |
| `list-ports` | List serial devices |
| `devices` | Group USB bridges and show channel roles and stable paths (Linux) |
| `tcp-bridge HOST:PORT` | Relay a serial debug device over TCP |

There are no 680x0 targets or executable commands.

## CPU state and program launches

Ordinary memory, upload, boot, copy, and flash commands enter debug mode and
release reset on success. To perform multiple operations without resetting
between them, use `stop` first and `start` afterward. A `run-*` command while
stopped stages the program; use `reset --boot ram` when ready to launch from its
RAM vectors.

`reset` explicitly restores flash boot, even after `boot ram`, and clears the
FPGA's independent RDY halt before releasing the CPU. `reset --boot ram` selects
RAM instead. This is a CPU/MMU reset; it does not power-cycle the board or reset
all peripherals as the physical reset button does.

Stopped state is stored separately for each target and endpoint under
`$XDG_STATE_HOME/wdb`, falling back to `~/.local/state/wdb`. `--state-dir` overrides
that directory. It distinguishes a CPU halted through RDY from one held in reset.
Use the same target across commands. Serial paths prefer their stable by-id
identity; TCP host aliases are not resolved for state identity.

If a transfer fails, `wdb` preserves the stopped state instead of launching an
incomplete image. Retry the transfer and use `reset` to boot the firmware, or
`reset --boot ram` to launch a RAM executable. Use `start` to release the recorded
state with the existing boot selection. After an external reset or power cycle, `wdb reset`
also clears stale state. State files record the last operation; they are not live
hardware status queries.

Commands sharing an endpoint and state directory take an exclusive endpoint lock
before reading state or connecting. An overlapping command fails with a lock error;
retry it after the active command finishes. Locks are released when the process
exits. Different state directories or machines do not share this protection.

If a CPU control command fails, the saved state is marked uncertain. A subsequent
transfer reasserts reset before accessing memory and leaves the CPU stopped on
success. Use `start` to release reset and any independent halt, or `reset` to
restore the chosen boot mapping and resume. An uncertain `stop` requires recovery
instead of reporting that the CPU is already stopped.

65C02 executables publish the `CROSSDEV` kernel marker after program data, entry
point, and arguments. A readback verifies the marker before reset release.
65816 banked entry points use a long-jump stub at `$FF80`; bank-zero entries use
the reset vector directly. The selected boot mapping must expose these vectors.
PGZ is a block container, not a compression format; an image without an entry
record only loads data. Data blocks at physical address zero are supported.

## Keyboard support

| Target | `key` / `type` backend | Required debug revision |
| --- | --- | --- |
| JR2 | PS/2 Set-2 FIFO | 5 or newer |
| K2 | Optical matrix snapshots | 5 or newer |

`scancodes` uses the PS/2 FIFO on either target. Feature checks run before any
injection writes. Revision queries and injection do not enter debug mode or reset
the CPU. The capability rules follow the unified Wildbits RTL revision 5; a
numeric revision is not a board-identity check.

K2 supports the mapped letters, digits, punctuation, and named keys `enter`,
`space`, `tab`, `backspace`, `home`, `up`, `left`, and `run-stop` (also `runstop`,
`pause`, `break`). Unsupported matrix keys return an error. JR2 additionally
supports `escape`/`esc`, `down`, and `right`. Input is US-layout based.
Press/release optical snapshots are paced 25 ms apart so firmware sees both.

## Flash and SD copy

Flash commands require typing `yes`; `--yes` is available for scripts.
Full images must be exactly 524288 bytes and sector images exactly 8192 bytes.
The supported firmware window is 512 KiB on both targets; K2's physical flash
chip is larger. A chip erase affects the entire physical device.
`--address` selects full-image staging within the 2 MiB SRAM; `10000` is one
valid example. Sector commands reuse the FPGA's last selected RAM source,
which `wdb` records per target and endpoint (zero initially).
After an FPGA reload, remove that endpoint's `.flash-source` state file, since
the programmer returns to its initial source. Other flash clients can also
change this register; keep flash programming under one client's control.
These commands program firmware flash through the debug port, not FPGA images.

The current unified RTL's full-program opcode writes only 8 KiB. `wdb` follows
it with explicit programming of the other 63 sectors. RAM staging is read back
before programming. Sector operations and complete images are read back through
the firmware window before success is reported. A mismatch reports the first
failing offset, physical address, expected and actual bytes, and mismatch count,
and leaves the CPU stopped. The FPGA acknowledges each command after its
internal erase/program timing completes.

Some FPGA cores repeat the first byte of a flash burst, shifting subsequent
bytes by one position. Flash reads request an extra byte and check independent
single-byte reads to distinguish this behavior from correctly aligned bursts.
This also applies to memory dumps, works with either behavior, and never uses
the expected image to choose the alignment. Inconsistent reads fail verification.

Bulk CSV rows contain `sector_hex,filename`, without a header. Paths are relative
to the CSV file. Every file, size, sector number, and duplicate is checked before
connecting or erasing. `--erase` erases the **entire** flash before programming
the selected sectors; otherwise only listed sectors are erased.

```csv
01,sector01.bin
02,sector02.bin
0A,sector0a.bin
```

`copy` stages a basename, kernel-compatible CRC32, 24-bit length, and file bytes
at `$010000`, then publishes `COPYFILE`. Files must be smaller than 449536 bytes.
The kernel must implement this request ABI. A successful command confirms staging
and marker readback, not completion of the SD write. A stopped CPU must be started
before the kernel can process it.

## TCP bridge

```sh
wdb --target k2 tcp-bridge 127.0.0.1:2560
wdb --target k2 --port 127.0.0.1:2560 reset
```

The bridge serves one complete client connection at a time to prevent interleaved
debug sessions. Client and serial operations have timeouts. It relays debug
transactions up to 1024 bytes; this is not an arbitrary serial byte-stream proxy.

## Development and validation

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

The tests cover packet framing/checksums, malformed files, both CPU launch paths,
keyboard encodings, state isolation, flash preflight and sequencing, aligned and
delayed flash bursts, rejection of actual flash corruption, a TCP device emulator,
and Unix pseudo-terminal serial/bridge integration. Corrected flash reads were
checked against a complete 8 KiB sector on a K2 with debug revision 5. Broader
physical JR2/K2 validation remains necessary before calling this release
hardware-verified.

Protocol constants and feature gates are centralized in `src/protocol.rs`.
The CLI and configuration expose only JR2/K2 targets; future FPGA commands can
add explicit capability requirements there.

MIT licensed. The included license preserves attribution for the original
implementation from which protocol and loader behavior were ported.
