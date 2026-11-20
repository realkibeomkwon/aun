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

/// The directed broadcast address of a network: the host bits all ones.
pub fn directed(addr: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(addr) | !u32::from(mask))
}

/// The directed broadcast address of every IPv4 interface that is up, not loopback, and not a
/// point-to-point link (/31, /32). A Mac on Ethernet and Wi-Fi at once reaches both networks;
/// 255.255.255.255 alone leaves on one interface.
pub fn broadcasts() -> Vec<Ipv4Addr> {
    let mut v: Vec<_> = interfaces().into_iter().filter(|(_, m)| u32::from(*m).leading_ones() <= 30).map(|(a, m)| directed(a, m)).collect();
    v.sort();
    v.dedup();
    v
}

/// (address, netmask) of IPv4 interfaces that are up and not loopback.
#[cfg(unix)]
pub fn interfaces() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a list we only read, then free once.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut p = head;
        while let Some(i) = p.as_ref() {
            p = i.ifa_next;
            let flags = i.ifa_flags as i32;
            if i.ifa_addr.is_null() || i.ifa_netmask.is_null() || flags & libc::IFF_UP == 0 || flags & libc::IFF_LOOPBACK != 0 {
                continue;
            }
            if (*i.ifa_addr).sa_family as i32 != libc::AF_INET {
                continue;
            }
            let v4 = |sa: *const libc::sockaddr| Ipv4Addr::from(u32::from_be((*(sa as *const libc::sockaddr_in)).sin_addr.s_addr));
            out.push((v4(i.ifa_addr), v4(i.ifa_netmask)));
        }
        libc::freeifaddrs(head);
    }
    out
}

#[cfg(windows)]
pub fn interfaces() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH};
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
    let mut out = Vec::new();
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 16u32 << 10;
    for _ in 0..3 {
        // u64s keep the list 8-byte aligned, as its structs need.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let first = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
        // SAFETY: `buf` holds `size` bytes; the list it gets is read before `buf` goes.
        let r = unsafe { GetAdaptersAddresses(AF_INET as u32, flags, std::ptr::null(), first, &mut size) };
        if r == windows_sys::Win32::Foundation::ERROR_BUFFER_OVERFLOW {
            continue;
        }
        if r != 0 {
            return out;
        }
        // SAFETY: a list GetAdaptersAddresses just wrote into `buf`.
        unsafe {
            let mut a = first as *const IP_ADAPTER_ADDRESSES_LH;
            while let Some(ad) = a.as_ref() {
                a = ad.Next;
                if ad.OperStatus != IfOperStatusUp || ad.IfType == IF_TYPE_SOFTWARE_LOOPBACK {
                    continue;
                }
                let mut u = ad.FirstUnicastAddress as *const windows_sys::Win32::NetworkManagement::IpHelper::IP_ADAPTER_UNICAST_ADDRESS_LH;
                while let Some(ua) = u.as_ref() {
                    u = ua.Next;
                    let sa = ua.Address.lpSockaddr;
                    if sa.is_null() || (*sa).sa_family != AF_INET {
                        continue;
                    }
                    let ip = Ipv4Addr::from(u32::from_be((*(sa as *const SOCKADDR_IN)).sin_addr.S_un.S_addr));
                    let bits = ua.OnLinkPrefixLength.min(32) as u32;
                    let mask = Ipv4Addr::from(u32::MAX.checked_shl(32 - bits).unwrap_or(0));
                    out.push((ip, mask));
                }
            }
        }
        return out;
    }
    out
}

#[cfg(not(any(unix, windows)))]
pub fn interfaces() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    Vec::new()
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
        Err(e) => return crate::log(format_args!("discovery off, UDP {PORT}: {e}")),
    };
    let _ = sock.set_broadcast(true);
    let _ = sock.set_read_timeout(Some(Duration::from_secs(1)));
    let hello = encode(&me, tcp_port, &name);
    let all = SocketAddr::from((Ipv4Addr::BROADCAST, PORT));
    let mut last: std::collections::HashMap<Id, Instant> = Default::default();
    let mut next = Instant::now();
    let mut buf = [0u8; 512];
    loop {
        if Instant::now() >= next {
            let _ = sock.send_to(&hello, all);
            // Read every round: Wi-Fi joins and leaves, a cable is plugged in.
            for b in broadcasts() {
                let _ = sock.send_to(&hello, SocketAddr::from((b, PORT)));
            }
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

    #[test]
    fn directed_broadcasts_set_the_host_bits() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        assert_eq!(directed(ip("192.168.0.23"), ip("255.255.255.0")), ip("192.168.0.255"));
        assert_eq!(directed(ip("10.1.2.3"), ip("255.255.240.0")), ip("10.1.15.255"));
        assert_eq!(directed(ip("172.16.5.4"), ip("255.255.0.0")), ip("172.16.255.255"));
    }

    /// This machine (and a CI runner) has at least one network besides loopback, and every
    /// broadcast address found lies in its interface's network.
    #[test]
    fn this_machine_has_a_broadcast_address() {
        let ifs = interfaces();
        println!("{ifs:?} -> {:?}", broadcasts());
        assert!(ifs.iter().all(|(a, _)| !a.is_loopback()), "{ifs:?}");
        assert!(!broadcasts().is_empty(), "{ifs:?}");
        for (a, m) in ifs {
            let b = directed(a, m);
            assert_eq!(u32::from(b) & u32::from(m), u32::from(a) & u32::from(m));
        }
    }
}
