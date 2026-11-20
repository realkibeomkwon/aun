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
    guest.pair(addr(&host), host.code().expect("one miss keeps the code"));
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

/// Discovery keeps announcing while a pairing completes. Sends made right after must neither
/// fail nor find the device unconnected, and the pairing must not be reported as failed.
#[test]
fn sends_right_after_pairing_survive_discovery() {
    for round in 0..10 {
        let (host, _hn) = node(&format!("race-host-{round}"), "Host");
        let (guest, gn) = node(&format!("race-guest-{round}"), "Guest");
        let (hid, gid, haddr, gaddr) = (host.id(), guest.id(), addr(&host), addr(&guest));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let announce = |n: Node, id, a, stop: std::sync::Arc<std::sync::atomic::AtomicBool>| {
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    n.saw(id, "Peer".into(), a);
                    std::thread::sleep(Duration::from_micros(200));
                }
            })
        };
        let a = announce(host.clone(), gid, gaddr, stop.clone());
        let b = announce(guest.clone(), hid, haddr, stop.clone());
        guest.pair(haddr, host.show_code());
        until("pairing", || connected(&guest, &host) && connected(&host, &guest));
        let src = dir(&format!("race-src-{round}"));
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("f.bin");
        std::fs::write(&file, vec![7u8; 2 << 20]).unwrap();
        for _ in 0..5 {
            guest.send_file(hid, file.clone());
            std::thread::sleep(Duration::from_millis(15));
        }
        let (mut done, mut bad) = (0, Vec::new());
        let end = Instant::now() + Duration::from_secs(10);
        while done < 5 && Instant::now() < end {
            match gn.recv_timeout(Duration::from_millis(200)) {
                Ok(Note::Transfer { finished: true, failed: false, .. }) => done += 1,
                Ok(n @ Note::Transfer { failed: true, .. }) | Ok(n @ Note::Info(_)) => bad.push(n),
                _ => {}
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = (a.join(), b.join());
        bad.retain(|n| !matches!(n, Note::Info(m) if m.contains("페어링했어요")));
        assert!(bad.is_empty() && done == 5, "round {round}: {done}/5 done, {bad:?}");
    }
}

/// Dropping a pile of files on a device sends them all; none fails and the link stays up.
#[test]
fn many_files_dropped_at_once_all_arrive() {
    let (host, _hn) = node("many-host", "Host");
    let (guest, gn) = node("many-guest", "Guest");
    guest.pair(addr(&host), host.show_code());
    until("pairing", || connected(&guest, &host) && connected(&host, &guest));
    let src = dir("many-src");
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).unwrap();
    let files: Vec<_> = (0..80)
        .map(|i| {
            let f = src.join(format!("f{i}.bin"));
            std::fs::write(&f, vec![i as u8; 1 << 20]).unwrap();
            f
        })
        .collect();
    guest.send_files(host.id(), files);
    let (mut done, mut bad) = (0, Vec::new());
    let end = Instant::now() + Duration::from_secs(30);
    while done < 80 && Instant::now() < end {
        match gn.recv_timeout(Duration::from_millis(200)) {
            Ok(Note::Transfer { finished: true, failed: false, .. }) => done += 1,
            Ok(n @ Note::Transfer { failed: true, .. }) | Ok(n @ Note::Info(_)) => bad.push(n),
            _ => {}
        }
    }
    bad.retain(|n| !matches!(n, Note::Info(m) if m.contains("페어링했어요")));
    assert!(bad.is_empty() && done == 80, "{done}/80 done, {bad:?}");
    assert!(connected(&guest, &host), "the link is still up");
    let dl = dir("many-host").join("dl");
    // "Sent" is when the last frame is queued; the receiver renames a moment later.
    until("80 files in the folder", || std::fs::read_dir(&dl).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "bin")).count() == 80);
    assert!(!dir_has_part(&dl));
}

/// Either side can call a transfer off: no `.part` stays behind, the link stays up and the next
/// file still arrives.
#[test]
fn a_cancelled_transfer_leaves_no_part_file() {
    let (host, hn) = node("cancel-host", "Host");
    let (guest, gn) = node("cancel-guest", "Guest");
    guest.pair(addr(&host), host.show_code());
    until("pairing", || connected(&guest, &host) && connected(&host, &guest));
    let src = dir("cancel-src");
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).unwrap();
    // Sparse: big enough to still be on the wire when the cancel lands, free to make.
    let big = src.join("big.bin");
    std::fs::File::create(&big).unwrap().set_len(4 << 30).unwrap();
    let small = src.join("small.txt");
    std::fs::write(&small, b"after").unwrap();
    let dl = dir("cancel-host").join("dl");

    // Wait for a note matching `want` on `rx`; returns its key.
    let wait = |rx: &mpsc::Receiver<Note>, what: &str, want: &dyn Fn(&Note) -> bool| -> u64 {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            if let Ok(n) = rx.recv_timeout(Duration::from_millis(100))
                && want(&n)
                && let Note::Transfer { key, .. } = n
            {
                return key;
            }
        }
    };
    let moving = |incoming| move |n: &Note| matches!(n, Note::Transfer { incoming: i, done, finished: false, .. } if *i == incoming && *done > 0);
    let stopped = |incoming| move |n: &Note| matches!(n, Note::Transfer { incoming: i, finished: true, failed: true, .. } if *i == incoming);

    for by_receiver in [true, false] {
        guest.send_file(host.id(), big.clone());
        if by_receiver {
            host.cancel(wait(&hn, "bytes arriving", &moving(true)));
            wait(&hn, "the receiver to stop", &|n| matches!(n, Note::Transfer { incoming: true, cancelled: true, .. }));
            wait(&gn, "the sender to stop", &|n| matches!(n, Note::Transfer { incoming: false, cancelled: true, .. }));
        } else {
            guest.cancel(wait(&gn, "bytes leaving", &moving(false)));
            wait(&gn, "the sender to stop", &|n| matches!(n, Note::Transfer { incoming: false, cancelled: true, .. }));
            wait(&hn, "the receiver to drop it", &stopped(true));
        }
        until("the .part to go", || !dir_has_part(&dl));
        assert!(!dl.join("big.bin").exists(), "by receiver: {by_receiver}");
        assert!(connected(&guest, &host) && connected(&host, &guest), "the link survives");
        guest.send_file(host.id(), small.clone());
        until("the next file", || std::fs::read(dl.join("small.txt")).is_ok_and(|b| b == b"after"));
        std::fs::remove_file(dl.join("small.txt")).unwrap();
    }
}
