//! Two nodes on loopback: pairing, a wrong code, pinned reconnects and a file across.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aun_core::{Config, Node, Note};

fn dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("aun-it-{}-{tag}", std::process::id()))
}

fn node(tag: &str, name: &str) -> (Node, mpsc::Receiver<Note>) {
    let _ = std::fs::remove_dir_all(dir(tag));
    restart(tag, name)
}

/// Starts a node on whatever `tag`'s folder already holds.
fn restart(tag: &str, name: &str) -> (Node, mpsc::Receiver<Note>) {
    let (tx, rx) = mpsc::channel();
    let d = dir(tag);
    let cfg = Config { dir: d.join("cfg"), downloads: d.join("dl"), name: name.into(), port: 0, discovery: false, clipboard: false };
    (Node::start(cfg, move |n| { let _ = tx.send(n); }).unwrap(), rx)
}

fn addr(n: &Node) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], n.port()))
}

fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn connected(a: &Node, b: &Node) -> bool {
    a.peers().iter().any(|p| p.id == b.id() && p.paired && p.connected)
}

#[test]
fn pair_send_and_reconnect() {
    let (host, _hn) = node("host", "Host");
    let (guest, gn) = node("guest", "Guest");

    // A wrong code pairs nothing and says so.
    let code = host.show_code();
    let wrong = format!("{:06}", (code.parse::<u32>().unwrap() + 1) % 1_000_000);
    guest.pair(addr(&host), wrong);
    let failed = gn.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(&failed, Note::Info(m) if m.contains("페어링 실패")), "{failed:?}");
    assert!(host.peers().is_empty() && guest.peers().is_empty());

    // The right code pairs both sides and leaves the link up.
    guest.pair(addr(&host), host.code().expect("one miss keeps the code").0);
    until("pairing", || connected(&host, &guest) && connected(&guest, &host));
    assert_eq!(host.code(), None, "a code works once");
    assert_eq!(host.peers()[0].name, "Guest");

    // A file larger than one chunk and one Noise message arrives byte for byte.
    let src = dir("src");
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).unwrap();
    let data: Vec<u8> = (0..3_000_017u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let file = src.join("../evil name.bin");
    std::fs::write(&file, &data).unwrap();
    guest.send_file(host.id(), file);
    let got = dir("host").join("dl").join("evil name.bin");
    until("file", || std::fs::read(&got).is_ok_and(|b| b.len() == data.len()));
    assert_eq!(std::fs::read(&got).unwrap(), data);
    assert!(!dir_has_part(&dir("host").join("dl")));

    // A stranger with a valid Noise key but no pairing is refused.
    let (stranger, _sn) = node("stranger", "Stranger");
    stranger.connect(addr(&host));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!host.peers().iter().any(|p| p.id == stranger.id()));

    // The guest restarts from its saved key and reconnects without a code.
    let (guest2, _g2) = restart("guest", "Guest");
    assert_eq!(guest2.id(), guest.id());
    guest2.connect(addr(&host));
    until("reconnect", || connected(&guest2, &host));

    // Forgetting drops the link and the pairing on that side.
    host.forget(guest.id());
    until("forget", || !host.peers().iter().any(|p| p.paired));
}

fn dir_has_part(d: &std::path::Path) -> bool {
    std::fs::read_dir(d).unwrap().flatten().any(|e| e.path().extension().is_some_and(|x| x == "part"))
}

/// `cargo test --release -p aun-core --test link -- --ignored --nocapture` prints loopback MB/s.
#[test]
#[ignore]
fn throughput() {
    let (host, _hn) = node("tp-host", "Host");
    let (guest, _gn) = node("tp-guest", "Guest");
    guest.pair(addr(&host), host.show_code());
    until("pairing", || connected(&guest, &host));
    let src = dir("tp-src");
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).unwrap();
    let size = 512usize << 20;
    let file = src.join("big.bin");
    std::fs::write(&file, vec![0x5au8; size]).unwrap();
    let got = dir("tp-host").join("dl").join("big.bin");
    let t = Instant::now();
    guest.send_file(host.id(), file);
    let end = Instant::now() + Duration::from_secs(120);
    while !std::fs::metadata(&got).is_ok_and(|m| m.len() == size as u64) {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(5));
    }
    let s = t.elapsed().as_secs_f64();
    println!("512 MiB in {s:.2} s = {:.0} MB/s", size as f64 / s / 1e6);
    let _ = std::fs::remove_dir_all(dir("tp-host"));
    let _ = std::fs::remove_dir_all(src);
}
