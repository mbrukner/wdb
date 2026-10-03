use crate::config::{Cpu, Target};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

pub fn hex(value: &str) -> Result<u32, String> {
    let digits = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .or_else(|| value.strip_prefix('$'))
        .unwrap_or(value);
    u32::from_str_radix(digits, 16).map_err(|_| format!("invalid hexadecimal value: {value}"))
}

#[derive(Debug, Parser)]
#[command(name = "wdb", version, about = "Wildbits Debug Bridge for JR2 and K2")]
pub struct Cli {
    /// Serial device, TCP address, or auto (default: discover the debug interface)
    #[arg(long, global = true)]
    pub port: Option<String>,
    /// Select a USB bridge by serial number or usb:LOCATION (see devices)
    #[arg(long, global = true)]
    pub device: Option<String>,
    /// Wildbits board model
    #[arg(short = 't', long, global = true, value_enum, ignore_case = true)]
    pub target: Option<Target>,
    /// CPU used by executable loaders
    #[arg(long, global = true, value_enum, ignore_case = true)]
    pub cpu: Option<Cpu>,
    /// Configuration file (otherwise search local and user wdb.toml)
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    /// Serial baud rate (decimal)
    #[arg(long, global = true)]
    pub baud: Option<u32>,
    /// Transport timeout in seconds (decimal)
    #[arg(long, global = true)]
    pub timeout: Option<u64>,
    /// Directory for persistent CPU state
    #[arg(long, global = true)]
    pub state_dir: Option<PathBuf>,
    /// Suppress progress messages; command results still print
    #[arg(long, short, global = true)]
    pub quiet: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct ReadArgs {
    /// Starting physical address in hex (defaults to configuration address)
    #[arg(long, value_parser = hex)]
    pub address: Option<u32>,
    /// Number of bytes in hex
    #[arg(long, default_value = "10", value_parser = hex)]
    pub count: u32,
}

#[derive(Debug, Args)]
pub struct LabelArgs {
    pub label: String,
    #[arg(long)]
    pub label_file: Option<PathBuf>,
    /// Number of bytes in hex
    #[arg(long, default_value = "10", value_parser = hex)]
    pub count: u32,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum BootSource {
    Ram,
    Flash,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Read physical memory as a hex dump
    Dump(ReadArgs),
    /// Upload an Intel HEX file
    Upload { file: PathBuf },
    /// Upload a Motorola S-record file
    UploadSrec { file: PathBuf },
    /// Upload a WDCTools binary file
    UploadWdc { file: PathBuf },
    /// Upload a raw binary to physical RAM
    Binary {
        file: PathBuf,
        #[arg(long, value_parser = hex)]
        address: u32,
    },
    /// Upload a PGX executable and install its launch vectors
    RunPgx { file: PathBuf },
    /// Upload a PGZ executable and install its launch vectors
    RunPgz { file: PathBuf },
    /// Halt the CPU without resetting it
    Stop,
    /// Resume a CPU stopped by wdb
    Start,
    /// Reset the CPU, boot from flash, and clear its stopped state
    Reset {
        /// Boot mapping to restore before releasing reset
        #[arg(long, value_enum, ignore_case = true, default_value = "flash")]
        boot: BootSource,
    },
    /// Select the boot lookup tables
    Boot {
        #[arg(value_enum, ignore_case = true)]
        source: BootSource,
    },
    /// Query the debug interface revision without resetting the CPU
    Revision,
    /// Show features supported by the selected target and debug revision
    Capabilities,
    /// Inject named keys or characters (K2 optical matrix; JR2 PS/2)
    Key {
        #[arg(required = true, num_args = 1..)]
        keys: Vec<String>,
    },
    /// Type text (K2 optical matrix; JR2 PS/2)
    Type { text: String },
    /// Inject raw hexadecimal PS/2 Set-2 bytes
    Scancodes {
        #[arg(required = true, num_args = 1.., value_parser = hex)]
        bytes: Vec<u32>,
    },
    /// Request that the running kernel copy a file to SD
    Copy { file: PathBuf },
    /// Display memory at a 64TASS label
    Lookup(LabelArgs),
    /// Follow a 24-bit little-endian pointer at a 64TASS label
    Deref(LabelArgs),
    /// Erase the entire firmware flash
    Erase {
        #[arg(long)]
        yes: bool,
    },
    /// Program firmware flash from a binary image
    Flash {
        file: PathBuf,
        /// RAM staging address in hex (full images only)
        #[arg(long, value_parser = hex, conflicts_with = "flash_sector")]
        address: Option<u32>,
        /// 8 KiB sector number in hex
        #[arg(long, value_parser = hex)]
        flash_sector: Option<u32>,
        #[arg(long)]
        yes: bool,
    },
    /// Program 8 KiB sectors listed as sector_hex,filename in a CSV file
    FlashBulk {
        file: PathBuf,
        /// Erase the entire flash before programming the listed sectors
        #[arg(long)]
        erase: bool,
        #[arg(long)]
        yes: bool,
    },
    /// List available serial ports
    ListPorts,
    /// List USB bridges, their channels, roles, stable paths, and driver bindings
    Devices,
    /// Serve a serial debug port over TCP (one client session at a time)
    TcpBridge { listen: String },
}
