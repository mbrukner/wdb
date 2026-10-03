use crate::{
    config::Config,
    protocol::{self, READ, WRITE},
    transport::{Transport, is_tcp},
};
use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::Duration,
};

pub fn relay(client: &mut (impl Read + Write), serial: &mut (impl Read + Write)) -> Result<()> {
    loop {
        let mut header = [0; 7];
        match client.read(&mut header[..1]) {
            Ok(0) => return Ok(()),
            Ok(_) => (),
            Err(error) => return Err(error.into()),
        }
        client
            .read_exact(&mut header[1..])
            .context("read bridge request header")?;
        ensure!(header[0] == 0x55, "invalid request sync");
        let command = header[1];
        let length = u16::from_be_bytes([header[5], header[6]]) as usize;
        ensure!(
            length <= protocol::MAX_TRANSFER,
            "bridge transaction exceeds 1024 bytes"
        );
        ensure!(
            length == 0 || command == READ || command == WRITE,
            "unsupported request payload"
        );
        let mut packet = header.to_vec();
        let payload_length = if command == WRITE { length } else { 0 };
        packet.resize(8 + payload_length, 0);
        client
            .read_exact(&mut packet[7..])
            .context("read bridge request body")?;
        // The bridge is a transparent relay, including the sender's request
        // checksum, so existing clients can use it unchanged.
        serial.write_all(&packet)?;
        serial.flush()?;
        let response = protocol::read_response(serial, if command == READ { length } else { 0 })?;
        let mut reply = vec![0xAA, response.status[0], response.status[1]];
        reply.extend_from_slice(&response.data);
        reply.push(protocol::xor(&reply));
        client.write_all(&reply)?;
        client.flush()?;
    }
}

pub fn serve(cfg: &Config, listen: &str, quiet: bool) -> Result<()> {
    ensure!(!is_tcp(cfg.port()?), "tcp-bridge requires a serial --port");
    let listener = TcpListener::bind(listen).with_context(|| format!("listen on {listen}"))?;
    if !quiet {
        eprintln!("Listening on {}", listener.local_addr()?);
    }
    // Serial ownership spans each complete client session, so commands from
    // two clients cannot interleave inside a debug/upload/reset sequence.
    for client in listener.incoming() {
        let mut client: TcpStream = client?;
        let timeout = Some(Duration::from_secs(cfg.timeout));
        client.set_read_timeout(timeout)?;
        client.set_write_timeout(timeout)?;
        client.set_nodelay(true)?;
        let result = Transport::open(cfg).and_then(|mut serial| relay(&mut client, &mut serial));
        if let Err(error) = result {
            eprintln!("wdb bridge: {error:#}");
        }
    }
    Ok(())
}
