use crate::config::Config;
use anyhow::{Context, Result, ensure};
use std::{
    io::{self, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    time::Duration,
};

pub enum Transport {
    Serial(Box<dyn serialport::SerialPort>),
    Tcp(TcpStream),
}

pub fn is_tcp(port: &str) -> bool {
    port.starts_with("tcp://") || port.contains(':')
}

impl Transport {
    pub fn open(cfg: &Config) -> Result<Self> {
        let port = cfg.port()?;
        if is_tcp(port) {
            let timeout = Duration::from_secs(cfg.timeout);
            let address = port.strip_prefix("tcp://").unwrap_or(port);
            let addresses: Vec<_> = address
                .to_socket_addrs()
                .context("resolve TCP endpoint")?
                .collect();
            ensure!(
                !addresses.is_empty(),
                "TCP endpoint resolved to no addresses"
            );
            let mut last_error = None;
            for address in addresses {
                match TcpStream::connect_timeout(&address, timeout) {
                    Ok(stream) => {
                        stream.set_read_timeout(Some(timeout))?;
                        stream.set_write_timeout(Some(timeout))?;
                        stream.set_nodelay(true)?;
                        return Ok(Self::Tcp(stream));
                    }
                    Err(error) => last_error = Some(error),
                }
            }
            Err(last_error.expect("nonempty addresses").into())
        } else {
            Ok(Self::Serial(
                serialport::new(port, cfg.baud)
                    .timeout(Duration::from_secs(cfg.timeout))
                    .open()
                    .with_context(|| format!("open serial device {port}"))?,
            ))
        }
    }
}
impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Serial(port) => port.read(buf),
            Self::Tcp(stream) => stream.read(buf),
        }
    }
}
impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Serial(port) => port.write(buf),
            Self::Tcp(stream) => stream.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Serial(port) => port.flush(),
            Self::Tcp(stream) => stream.flush(),
        }
    }
}
