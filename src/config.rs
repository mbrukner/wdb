use crate::cli::Cli;
use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use serde::Deserialize;
use std::{env, fs, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Jr2,
    K2,
}

impl Target {
    pub fn name(self) -> &'static str {
        match self {
            Self::Jr2 => "JR2",
            Self::K2 => "K2",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ValueEnum, Default)]
pub enum Cpu {
    #[default]
    #[serde(rename = "65c02", alias = "65C02")]
    #[value(name = "65c02")]
    C65c02,
    #[serde(rename = "65816")]
    #[value(name = "65816")]
    C65816,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub port: Option<String>,
    pub device: Option<String>,
    pub target: Option<Target>,
    pub cpu: Cpu,
    pub baud: u32,
    pub timeout: u64,
    pub chunk_size: usize,
    pub flash_size: usize,
    pub address: String,
    pub label_file: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: None,
            device: None,
            target: None,
            cpu: Cpu::default(),
            baud: 6_000_000,
            timeout: 60,
            chunk_size: 1024,
            flash_size: 512 * 1024,
            address: "0".into(),
            label_file: None,
            state_dir: None,
        }
    }
}

impl Config {
    pub fn load(cli: &Cli) -> Result<Self> {
        let path = if let Some(path) = &cli.config {
            Some(path.clone())
        } else {
            let mut paths = vec![PathBuf::from("wdb.toml")];
            if let Some(dir) = env::var_os("WDB_CONFIG_DIR") {
                paths.push(PathBuf::from(dir).join("wdb.toml"));
            }
            if let Some(dir) = env::var_os("XDG_CONFIG_HOME") {
                paths.push(PathBuf::from(dir).join("wdb/wdb.toml"));
            } else if let Some(home) = env::var_os("HOME") {
                paths.push(PathBuf::from(home).join(".config/wdb/wdb.toml"));
            }
            paths.into_iter().find(|p| p.exists())
        };
        let mut cfg: Self = match path {
            Some(path) => toml::from_str(
                &fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
            )
            .with_context(|| format!("parse {}", path.display()))?,
            None => Self::default(),
        };
        if let Some(port) = &cli.port {
            cfg.port = Some(port.clone());
            if cli.device.is_none() && port != "auto" {
                cfg.device = None;
            }
        }
        if let Some(device) = &cli.device {
            cfg.device = Some(device.clone());
            if cli.port.is_none() {
                cfg.port = None;
            }
        }
        if let Some(target) = cli.target {
            cfg.target = Some(target);
        }
        if let Some(cpu) = cli.cpu {
            cfg.cpu = cpu;
        }
        if let Some(baud) = cli.baud {
            cfg.baud = baud;
        }
        if let Some(timeout) = cli.timeout {
            cfg.timeout = timeout;
        }
        if let Some(dir) = &cli.state_dir {
            cfg.state_dir = Some(dir.clone());
        }
        ensure!(cfg.baud > 0, "baud must be positive");
        ensure!(
            (1..=3600).contains(&cfg.timeout),
            "timeout must be between 1 and 3600 seconds"
        );
        ensure!(
            (1..=1024).contains(&cfg.chunk_size),
            "chunk_size must be between 1 and 1024 bytes"
        );
        ensure!(
            cfg.flash_size == 512 * 1024,
            "the debug programmer supports 524288-byte firmware images"
        );
        Ok(cfg)
    }
    pub fn port(&self) -> Result<&str> {
        self.port
            .as_deref()
            .filter(|p| !p.is_empty())
            .context("specify --port or port in wdb.toml")
    }
    pub fn target(&self) -> Result<Target> {
        self.target
            .context("specify --target jr2 or --target k2 (or target in wdb.toml)")
    }
}
