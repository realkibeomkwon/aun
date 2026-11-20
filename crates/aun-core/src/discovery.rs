//! LAN discovery: every device broadcasts `AUN1 | id[32] | port u16 | name_len u8 | name` on UDP
//! every few seconds and answers a newcomer at once. Announces carry no secrets; a link is only
//! trusted after the Noise handshake shows the paired key.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::store::Id;

pub const PORT: u16 = 47811;
const EVERY: Duration = Duration::from_secs(3);
const MAGIC: &[u8; 4] = b"AUN1";

pub fn encode(id: &Id, port: u16, name: &str) -> Vec<u8> {
    let name = &name.as_bytes()[..floor_char(name, 64)];
    let mut b = Vec::with_capacity(39 + name.len());
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(id);
    b.extend_from_slice(&port.to_be_bytes());
    b.push(name.len() as u8);
    b.extend_from_slice(name);
    b
}

fn floor_char(s: &str, max: usize) -> usize {
    (0..=max.min(s.len())).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0)
}

pub fn decode(b: &[u8]) -> Option<(Id, u16, String)> {
    if b.get(..4)? != MAGIC {
        return None;
    }
    let id: Id = b.get(4..36)?.try_into().ok()?;
    let port = u16::from_be_bytes(b.get(36..38)?.try_into().ok()?);
    let n = *b.get(38)? as usize;
    let name = std::str::from_utf8(b.get(39..39 + n)?).ok()?;
    Some((id, port, crate::store::clean_name(name)))
}

/// Runs forever on the calling thread. `seen(id, name, addr, is_new)` runs for every announce.
pub fn run(me: Id, tcp_port: u16, name: String, mut seen: impl FnMut(Id, String, SocketAddr, bool), mut lost: impl FnMut(Id)) {
    let sock = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PORT)) {
        Ok(s) => s,
        Err(e) => return eprintln!("aun: discovery off, UDP {PORT}: {e}"),
    };
    let _ = sock.set_broadcast(true);
    let _ = sock.set_read_timeout(Some(Duration::from_secs(1)));
    let hello = encode(&me, tcp_port, &name);
    // shortcut: limited broadcast leaves on the default interface only; add per-interface
    // directed broadcasts if machines on a second NIC/VLAN must find each other.
    let all = SocketAddr::from((Ipv4Addr::BROADCAST, PORT));
    let mut last: std::collections::HashMap<Id, Instant> = Default::default();
    let mut next = Instant::now();
    let mut buf = [0u8; 512];
    loop {
        if Instant::now() >= next {
            let _ = sock.send_to(&hello, all);
            next = Instant::now() + EVERY;
            last.retain(|id, t| {
                let alive = t.elapsed() < EVERY * 3 + Duration::from_secs(1);
                if !alive {
                    lost(*id);
                }
                alive
            });
        }
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        let Some((id, port, name)) = decode(&buf[..n]) else { continue };
        if id == me {
            continue;
        }
        let is_new = last.insert(id, Instant::now()).is_none();
        if is_new {
            let _ = sock.send_to(&hello, SocketAddr::from((from.ip(), PORT)));
        }
        seen(id, name, SocketAddr::new(from.ip(), port), is_new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_round_trips_and_cuts_names_on_char_boundaries() {
        let name = "가".repeat(40); // 120 bytes
        let (id, port, got) = decode(&encode(&[7; 32], 47810, &name)).unwrap();
        assert_eq!((id, port), ([7; 32], 47810));
        assert_eq!(got, "가".repeat(21));
        assert!(decode(b"AUN1short").is_none());
        assert!(decode(b"XXXX").is_none());
    }
}
