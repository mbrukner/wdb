use crate::config::Config;
use anyhow::{Context, Result, bail};
use std::{env, fs, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    Halted,
    Reset,
    Uncertain,
}

pub struct State {
    path: PathBuf,
    lock_path: PathBuf,
}

impl State {
    pub fn new(cfg: &Config) -> Result<Self> {
        let root = cfg
            .state_dir
            .clone()
            .or_else(|| env::var_os("XDG_STATE_HOME").map(|p| PathBuf::from(p).join("wdb")))
            .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state/wdb")))
            .context("set --state-dir when HOME and XDG_STATE_HOME are unavailable")?;
        let port = cfg.port()?;
        let endpoint = if crate::transport::is_tcp(port) {
            port.strip_prefix("tcp://").unwrap_or(port).to_owned()
        } else {
            crate::discovery::stable_port(
                std::path::Path::new(port),
                std::path::Path::new("/dev/serial/by-id"),
            )
            .to_string_lossy()
            .into_owned()
        };
        // Reversible endpoint encoding keeps separate boards from sharing a marker.
        let encoded: String = endpoint.bytes().map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            path: root.join(format!("{}-{encoded}.stopped", cfg.target()?.name())),
            lock_path: root.join(format!("{encoded}.lock")),
        })
    }
    /// Hold this file for the complete command, before reading any endpoint state.
    /// Never unlink it: other processes must always lock the same inode.
    pub fn lock(&self) -> Result<fs::File> {
        self.prepare()?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)
            .context("open endpoint lock")?;
        file.try_lock().context(
            "lock endpoint; another wdb command may be using it, retry after it finishes",
        )?;
        Ok(file)
    }
    pub fn read(&self) -> Result<Option<Stopped>> {
        match fs::read_to_string(&self.path) {
            Ok(value) => match value.trim() {
                "halted" => Ok(Some(Stopped::Halted)),
                "reset" => Ok(Some(Stopped::Reset)),
                "uncertain" => Ok(Some(Stopped::Uncertain)),
                _ => bail!("invalid CPU state in {}", self.path.display()),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("read CPU state"),
        }
    }
    pub fn prepare(&self) -> Result<()> {
        fs::create_dir_all(self.path.parent().expect("state parent"))
            .context("create CPU state directory")
    }
    pub fn set(&self, state: Stopped) -> Result<()> {
        self.prepare()?;
        fs::write(
            &self.path,
            match state {
                Stopped::Halted => "halted\n",
                Stopped::Reset => "reset\n",
                Stopped::Uncertain => "uncertain\n",
            },
        )
        .context("save CPU state")
    }
    pub fn clear(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("clear CPU state"),
        }
    }

    // The FPGA sector command reuses the RAM source last selected by command
    // 0x10. Retain that address alongside CPU state for later sector commands.
    pub fn flash_source(&self) -> Result<u32> {
        match fs::read_to_string(self.path.with_extension("flash-source")) {
            Ok(value) => crate::cli::hex(value.trim()).map_err(anyhow::Error::msg),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error).context("read flash staging address"),
        }
    }

    pub fn set_flash_source(&self, address: u32) -> Result<()> {
        self.prepare()?;
        fs::write(
            self.path.with_extension("flash-source"),
            format!("{address:X}\n"),
        )
        .context("save flash staging address")
    }
}
