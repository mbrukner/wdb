use crate::{
    bridge,
    cli::{BootSource, Cli, Command, LabelArgs, hex},
    config::Config,
    keyboard::{self, Input},
    loader::{self, Format, Image},
    protocol::{self, Capabilities, DebugPort},
    state::{State, Stopped},
    transport::Transport,
    util,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

type Port = DebugPort<Transport>;

fn connect(cfg: &Config) -> Result<Port> {
    DebugPort::new(Transport::open(cfg)?, cfg.chunk_size)
}
fn info(quiet: bool, message: &str) {
    if !quiet {
        eprintln!("{message}");
    }
}
fn read_file(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("read {}", path.display()))
}

/// Normal memory operations enter debug mode and reset on successful completion.
/// An explicit stop persists across commands. Failed operations remain stopped,
/// preventing an incomplete executable or flash image from being launched.
fn with_debug<R>(cfg: &Config, operation: impl FnOnce(&mut Port) -> Result<R>) -> Result<R> {
    let state = State::new(cfg)?;
    let prior = state.read()?;
    let stopped = prior.is_some();
    state.prepare()?;
    let mut dp = connect(cfg)?;
    if prior.is_none() || prior == Some(Stopped::Uncertain) {
        state.set(Stopped::Uncertain)?;
        dp.command(protocol::ENTER)
            .context("enter debug mode; CPU state is uncertain on failure")?;
        state.set(Stopped::Reset)?;
    }
    let value = operation(&mut dp).context(
        "operation failed; CPU state retained, use 'wdb start' or 'wdb reset' to recover",
    )?;
    if !stopped {
        state.set(Stopped::Uncertain)?;
        dp.command(protocol::EXIT).context("release reset")?;
        state.clear()?;
    }
    Ok(value)
}

fn upload(cfg: &Config, image: &Image) -> Result<()> {
    with_debug(cfg, |dp| {
        // Invalidate a previous launch marker before staging a new executable.
        if image.verify_launch {
            dp.write(0x0080, &[0; 8])?;
        }
        for block in &image.blocks {
            dp.write(block.address, &block.data)?;
        }
        if image.verify_launch {
            ensure!(
                dp.read(0x0080, 8)? == b"CROSSDEV",
                "launch marker readback failed"
            );
        }
        Ok(())
    })
}

fn load(cfg: &Config, path: &Path, format: Format, quiet: bool) -> Result<()> {
    // Parse the entire file, including checksums and address bounds, before
    // opening the debug port or changing the CPU state.
    let image = loader::parse(&read_file(path)?, format, cfg.cpu)?;
    upload(cfg, &image)?;
    info(quiet, "Upload complete.");
    Ok(())
}

fn label(cfg: &Config, args: &LabelArgs, dereference: bool) -> Result<()> {
    let path = args
        .label_file
        .as_ref()
        .or(cfg.label_file.as_ref())
        .context("specify --label-file or label_file in wdb.toml")?;
    let address = util::lookup(path, &args.label)?;
    protocol::check_range(address, if dereference { 3 } else { args.count as usize })?;
    let (address, data) = with_debug(cfg, |dp| {
        let address = if dereference {
            let pointer = dp.read(address, 3)?;
            u32::from(pointer[0]) | (u32::from(pointer[1]) << 8) | (u32::from(pointer[2]) << 16)
        } else {
            address
        };
        Ok((address, dp.read(address, args.count as usize)?))
    })?;
    util::dump(&mut io::stdout().lock(), &data, address)?;
    Ok(())
}

fn inject(cfg: &Config, input: Input, quiet: bool) -> Result<()> {
    let mut dp = connect(cfg)?;
    Capabilities::new(cfg.target()?, dp.revision()?).require_keyboard(cfg.target()?)?;
    match input {
        Input::Optical(frames) => {
            for frame in frames {
                dp.write(protocol::OPTICAL_REGISTER, &frame)?;
                // Firmware consumes one complete optical image per video scan.
                thread::sleep(Duration::from_millis(25));
            }
        }
        Input::Ps2(strokes) => {
            for stroke in strokes {
                for byte in stroke {
                    dp.write(protocol::PS2_REGISTER, &[byte])?;
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
    }
    info(quiet, "Keyboard input sent.");
    Ok(())
}

fn sector_number(value: u32) -> Result<u8> {
    ensure!(value < 64, "sector must be between 00 and 3F (hex)");
    Ok(value as u8)
}

fn sector_image(path: &Path) -> Result<Vec<u8>> {
    let data = read_file(path)?;
    ensure!(
        data.len() == 8192,
        "{} must be exactly 8192 bytes for sector programming",
        path.display()
    );
    Ok(data)
}

fn verify_flash(dp: &mut Port, offset: u32, data: &[u8]) -> Result<()> {
    let actual = dp.read(protocol::FLASH_BASE + offset, data.len())?;
    if let Some(first) = actual.iter().zip(data).position(|(a, b)| a != b) {
        let mismatches = actual.iter().zip(data).filter(|(a, b)| a != b).count();
        anyhow::bail!(
            "flash readback mismatch at offset {:06X} (physical {:06X}): expected {:02X}, read {:02X}; {mismatches}/{} bytes differ; CPU remains stopped",
            offset + first as u32,
            protocol::FLASH_BASE + offset + first as u32,
            data[first],
            actual[first],
            data.len()
        );
    }
    Ok(())
}

fn flash_source(cfg: &Config) -> Result<u32> {
    let address = State::new(cfg)?.flash_source()?;
    ensure!(
        address <= 0x200000 - 8192,
        "flash source must lie within the 2 MiB SRAM"
    );
    Ok(address)
}

fn program_sector(dp: &mut Port, sector: u8, data: &[u8], erase: bool, source: u32) -> Result<()> {
    dp.write(source, data)?;
    ensure!(
        dp.read(source, data.len())? == data,
        "RAM staging readback mismatch at {source:06X}; flash has not been programmed for sector {sector:02X}"
    );
    if erase {
        for half in 0..2 {
            dp.transfer(
                protocol::ERASE_SECTOR,
                (u32::from(sector) * 2 + half) << 16,
                &[],
                0,
            )?;
        }
    }
    dp.transfer(
        protocol::PROGRAM_SECTOR,
        (u32::from(sector) * 2) << 16,
        &[],
        0,
    )?;
    // Unified RTL acknowledges flash commands only after its timed operation
    // completes; no additional host-side delay is necessary.
    verify_flash(dp, u32::from(sector) * 8192, data)
}

struct Sector {
    number: u8,
    path: PathBuf,
    data: Vec<u8>,
}

fn bulk_plan(path: &Path) -> Result<Vec<Sector>> {
    let mut csv = csv::ReaderBuilder::new()
        .has_headers(false)
        .trim(csv::Trim::All)
        .comment(Some(b'#'))
        .from_path(path)?;
    let mut seen = HashSet::new();
    let mut sectors = Vec::new();
    for record in csv.records() {
        let record = record?;
        ensure!(
            record.len() == 2,
            "CSV rows must contain sector_hex,filename"
        );
        let number = sector_number(hex(&record[0]).map_err(anyhow::Error::msg)?)?;
        ensure!(seen.insert(number), "duplicate sector {number:02X} in CSV");
        let file = path.parent().unwrap_or(Path::new(".")).join(&record[1]);
        let data = sector_image(&file)?;
        sectors.push(Sector {
            number,
            path: file,
            data,
        });
    }
    ensure!(!sectors.is_empty(), "CSV contains no sectors");
    Ok(sectors)
}

pub fn run(cli: Cli) -> Result<()> {
    if matches!(cli.command, Command::ListPorts) {
        for port in serialport::available_ports().context("enumerate serial ports")? {
            println!("{}", port.port_name);
        }
        return Ok(());
    }
    let mut cfg = Config::load(&cli)?;
    if matches!(cli.command, Command::Devices) {
        return crate::discovery::display(
            &mut io::stdout().lock(),
            &crate::discovery::scan()?,
            cfg.target,
            cfg.device.as_deref(),
        );
    }
    crate::discovery::resolve(&mut cfg)?;
    cfg.port()?;
    if let Command::TcpBridge { listen } = &cli.command {
        return bridge::serve(&cfg, listen, cli.quiet);
    }
    cfg.target()?;
    // Include state reads, flash-source selection, transport I/O, and cleanup.
    // Reject concurrent commands before either can act on the other's marker.
    let _endpoint_lock = State::new(&cfg)?.lock()?;
    let quiet = cli.quiet;
    match cli.command {
        Command::Dump(args) => {
            let address = args
                .address
                .map(Ok)
                .unwrap_or_else(|| hex(&cfg.address))
                .map_err(anyhow::Error::msg)?;
            protocol::check_range(address, args.count as usize)?;
            let data = with_debug(&cfg, |dp| dp.read(address, args.count as usize))?;
            util::dump(&mut io::stdout().lock(), &data, address)?;
        }
        Command::Upload { file } => load(&cfg, &file, Format::IntelHex, quiet)?,
        Command::UploadSrec { file } => load(&cfg, &file, Format::Srec, quiet)?,
        Command::UploadWdc { file } => load(&cfg, &file, Format::Wdc, quiet)?,
        Command::RunPgx { file } => load(&cfg, &file, Format::Pgx, quiet)?,
        Command::RunPgz { file } => load(&cfg, &file, Format::Pgz, quiet)?,
        Command::Binary { file, address } => {
            let mut image = Image::default();
            let data = read_file(&file)?;
            ensure!(!data.is_empty(), "binary file is empty");
            image.push(address, &data)?;
            upload(&cfg, &image)?;
            info(quiet, "Upload complete.");
        }
        Command::Reset { boot } => {
            let state = State::new(&cfg)?;
            state.prepare()?;
            let mut dp = connect(&cfg)?;
            state.set(Stopped::Uncertain)?;
            dp.command(protocol::ENTER).context("assert reset")?;
            dp.command(match boot {
                BootSource::Ram => protocol::BOOT_RAM,
                BootSource::Flash => protocol::BOOT_FLASH,
            })
            .context("select reset boot mapping; CPU remains in reset")?;
            // EXIT leaves the independent RDY halt asserted on current cores.
            // START clears RDY, restores the CPU bus, and reaches RESTART1,
            // which also releases the reset asserted by ENTER.
            dp.command(protocol::START)
                .context("release reset and debug halt")?;
            state.clear()?;
            info(
                quiet,
                match boot {
                    BootSource::Flash => "CPU reset; flash boot selected.",
                    BootSource::Ram => "CPU reset; RAM boot selected.",
                },
            );
        }
        Command::Stop => {
            let state = State::new(&cfg)?;
            let prior = state.read()?;
            ensure!(
                prior != Some(Stopped::Uncertain),
                "CPU state is uncertain; use 'wdb reset' to recover"
            );
            if prior.is_some() {
                info(quiet, "CPU is already stopped.");
                return Ok(());
            }
            state.prepare()?;
            let mut dp = connect(&cfg)?;
            state.set(Stopped::Uncertain)?;
            dp.command(protocol::STOP).context("halt CPU")?;
            state.set(Stopped::Halted)?;
            info(quiet, "CPU stopped. Use 'wdb start' to resume.");
        }
        Command::Start => {
            let state = State::new(&cfg)?;
            let Some(_) = state.read()? else {
                info(
                    quiet,
                    "No stopped CPU state recorded for this target and port.",
                );
                return Ok(());
            };
            let mut dp = connect(&cfg)?;
            state.set(Stopped::Uncertain)?;
            // START releases reset and any independent RDY halt, including one
            // left by an earlier command whose acknowledgement was lost.
            dp.command(protocol::START)?;
            state.clear()?;
            info(quiet, "CPU started.");
        }
        Command::Boot { source } => {
            with_debug(&cfg, |dp| {
                dp.command(match source {
                    BootSource::Ram => protocol::BOOT_RAM,
                    BootSource::Flash => protocol::BOOT_FLASH,
                })
            })?;
            info(quiet, "Boot source selected.");
        }
        Command::Revision => println!("{:X}", connect(&cfg)?.revision()?),
        Command::Capabilities => {
            let target = cfg.target()?;
            let caps = Capabilities::new(target, connect(&cfg)?.revision()?);
            println!(
                "Target: {}\nDebug revision: {}\nPS/2 injection: {}\nOptical matrix injection: {}",
                target.name(),
                caps.revision,
                caps.ps2,
                caps.optical
            );
        }
        Command::Key { keys } => inject(&cfg, keyboard::keys(cfg.target()?, &keys)?, quiet)?,
        Command::Type { text } => inject(&cfg, keyboard::text(cfg.target()?, &text)?, quiet)?,
        Command::Scancodes { bytes } => {
            let bytes: Vec<u8> = bytes
                .into_iter()
                .map(u8::try_from)
                .collect::<std::result::Result<_, _>>()
                .context("scan bytes must be between 00 and FF")?;
            let mut dp = connect(&cfg)?;
            let caps = Capabilities::new(cfg.target()?, dp.revision()?);
            ensure!(
                caps.ps2,
                "PS/2 injection requires debug revision 5 or newer (device reports {})",
                caps.revision
            );
            for byte in bytes {
                dp.write(protocol::PS2_REGISTER, &[byte])?;
            }
            info(quiet, "Scan codes sent.");
        }
        Command::Copy { file } => {
            let payload = util::copy_payload(&file, &read_file(&file)?)?;
            with_debug(&cfg, |dp| {
                dp.write(0x0080, &[0; 8])?;
                dp.write(0x10000, &payload)?;
                dp.write(0x0080, b"COPYFILE")?;
                ensure!(
                    dp.read(0x0080, 8)? == b"COPYFILE",
                    "copy marker readback failed"
                );
                Ok(())
            })?;
            info(
                quiet,
                "SD copy request staged. The kernel performs the copy when the CPU runs.",
            );
        }
        Command::Lookup(args) => label(&cfg, &args, false)?,
        Command::Deref(args) => label(&cfg, &args, true)?,
        Command::Erase { yes } => {
            if !util::confirm("Erase the entire firmware flash", yes)? {
                info(quiet, "Cancelled.");
                return Ok(());
            }
            with_debug(&cfg, |dp| dp.command(protocol::ERASE_FLASH))?;
            info(quiet, "Firmware flash erased.");
        }
        Command::Flash {
            file,
            address,
            flash_sector,
            yes,
        } => {
            if let Some(sector) = flash_sector {
                let sector = sector_number(sector)?;
                let data = sector_image(&file)?;
                if !util::confirm(
                    &format!("Program flash sector {sector:02X} from {}", file.display()),
                    yes,
                )? {
                    info(quiet, "Cancelled.");
                    return Ok(());
                }
                let source = flash_source(&cfg)?;
                with_debug(&cfg, |dp| program_sector(dp, sector, &data, true, source))?;
            } else {
                let address =
                    address.context("full flash programming requires --address for RAM staging")?;
                let data = read_file(&file)?;
                ensure!(
                    data.len() == cfg.flash_size,
                    "full flash image must be exactly {} bytes",
                    cfg.flash_size
                );
                protocol::check_range(address, data.len())?;
                ensure!(
                    address <= 0x200000 - cfg.flash_size as u32,
                    "full flash staging must fit within the 2 MiB SRAM (000000–1FFFFF)"
                );
                if !util::confirm(
                    &format!("Replace the entire firmware flash with {}", file.display()),
                    yes,
                )? {
                    info(quiet, "Cancelled.");
                    return Ok(());
                }
                with_debug(&cfg, |dp| {
                    dp.write(address, &data)?;
                    ensure!(
                        dp.read(address, data.len())? == data,
                        "RAM staging readback mismatch at {address:06X}; flash has not been erased"
                    );
                    dp.command(protocol::ERASE_FLASH)?;
                    State::new(&cfg)?.set_flash_source(address)?;
                    dp.transfer(protocol::PROGRAM_FLASH, address, &[], 0)?;
                    // Current unified RTL stops 0x10 after 8 KiB. Explicitly
                    // program every remaining sector. This is also correct
                    // on revisions where 0x10 programs the complete image.
                    for (sector, chunk) in data.chunks(8192).enumerate().skip(1) {
                        program_sector(dp, sector as u8, chunk, false, address)?;
                    }
                    verify_flash(dp, 0, &data)?;
                    Ok(())
                })?;
            }
            info(quiet, "Flash programming complete.");
        }
        Command::FlashBulk { file, erase, yes } => {
            let plan = bulk_plan(&file)?;
            let source = flash_source(&cfg)?;
            for sector in &plan {
                info(
                    quiet,
                    &format!("Sector {:02X}: {}", sector.number, sector.path.display()),
                );
            }
            let message = if erase {
                "Erase the entire firmware flash, then program listed sectors"
            } else {
                "Erase and program listed flash sectors"
            };
            if !util::confirm(message, yes)? {
                info(quiet, "Cancelled.");
                return Ok(());
            }
            with_debug(&cfg, |dp| {
                if erase {
                    dp.command(protocol::ERASE_FLASH)?;
                }
                for sector in &plan {
                    program_sector(dp, sector.number, &sector.data, !erase, source).with_context(
                        || {
                            format!(
                                "sector {:02X} from {}",
                                sector.number,
                                sector.path.display()
                            )
                        },
                    )?;
                }
                Ok(())
            })?;
            info(quiet, "Bulk flash programming complete.");
        }
        Command::ListPorts | Command::Devices | Command::TcpBridge { .. } => unreachable!(),
    }
    Ok(())
}
