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
    let cfg = Config { dir: d.join("cfg"), downloads: d.join("dl"), name: name.into(), port: 0, discovery: false, clipboard: false, secrets: false, kvm: false };
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
        // Five different files: the same one again while it is still going is refused by design.
        for i in 0..5 {
            let file = src.join(format!("f{i}.bin"));
            std::fs::write(&file, vec![7u8; 2 << 20]).unwrap();
            guest.send_file(hid, file);
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
    // Generous: a Windows CI runner's disk took 0.5 s a file (each is synced before its rename).
    let end = Instant::now() + Duration::from_secs(120);
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

/// Either side can call a transfer off: the link stays up and the next file still arrives. A
/// receiver that cancels keeps no `.part`; one cancelled by the sender keeps it, to go on later.
#[test]
fn a_cancelled_transfer_keeps_the_link() {
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
    // Only this round's big.bin: a slow machine leaves earlier progress notes in the queue (the
    // small file's, once, which the cancel then hit instead).
    let big_after = |incoming: bool, after: u64| {
        move |n: &Note| matches!(n, Note::Transfer { incoming: i, key, file, .. } if *i == incoming && *key > after && file == "big.bin")
    };
    let (mut after_in, mut after_out) = (0, 0);
    for by_receiver in [true, false] {
        guest.send_file(host.id(), big.clone());
        let moving_in = |n: &Note| big_after(true, after_in)(n) && matches!(n, Note::Transfer { done, finished: false, .. } if *done > 0);
        let moving_out = |n: &Note| big_after(false, after_out)(n) && matches!(n, Note::Transfer { done, finished: false, .. } if *done > 0);
        let (k_in, k_out);
        if by_receiver {
            k_in = wait(&hn, "bytes arriving", &moving_in);
            host.cancel(k_in);
            wait(&hn, "the receiver to stop", &|n| matches!(n, Note::Transfer { key, cancelled: true, .. } if *key == k_in));
            k_out = wait(&gn, "the sender to stop", &|n| big_after(false, after_out)(n) && matches!(n, Note::Transfer { cancelled: true, .. }));
        } else {
            k_out = wait(&gn, "bytes leaving", &moving_out);
            guest.cancel(k_out);
            wait(&gn, "the sender to stop", &|n| matches!(n, Note::Transfer { key, cancelled: true, .. } if *key == k_out));
            // The receiver hears it was a cancel, not a failure: both sides show "취소됨".
            k_in = wait(&hn, "the receiver to drop it as cancelled", &|n| big_after(true, after_in)(n) && matches!(n, Note::Transfer { finished: true, failed: true, cancelled: true, .. }));
        }
        (after_in, after_out) = (k_in, k_out);
        if by_receiver {
            until("the .part to go", || !dir_has_part(&dl));
        } else {
            std::thread::sleep(Duration::from_millis(300));
            assert!(dir_has_part(&dl), "kept for the sender to go on with");
        }
        assert!(!dl.join("big.bin").exists(), "by receiver: {by_receiver}");
        assert!(connected(&guest, &host) && connected(&host, &guest), "the link survives");
        guest.send_file(host.id(), small.clone());
        until("the next file", || std::fs::read(dl.join("small.txt")).is_ok_and(|b| b == b"after"));
        std::fs::remove_file(dl.join("small.txt")).unwrap();
    }
}

/// Paired by a typed address on a network where discovery finds nothing: the guest keeps the
/// address and, after a restart, reconnects on its own.
#[test]
fn a_device_paired_by_address_reconnects_without_discovery() {
    let (host, _hn) = node("typed-host", "Host");
    let (guest, _gn) = node("typed-guest", "Guest");
    guest.pair(addr(&host), host.show_code());
    until("pairing", || connected(&guest, &host) && connected(&host, &guest));
    let saved = std::fs::read_to_string(dir("typed-guest").join("cfg").join("peers.txt")).unwrap();
    assert!(saved.contains(&addr(&host).to_string()), "the typed address is saved: {saved}");
    let (guest2, _g2) = restart("typed-guest", "Guest");
    until("a reconnect nobody asked for", || connected(&guest2, &host));
}

/// The laptop leaves the LAN: an address typed while discovery also saw the device elsewhere
/// (Tailscale while on the same Wi-Fi) must still be kept, or nothing redials it.
#[test]
fn a_typed_address_is_kept_even_when_discovery_saw_the_device() {
    let (host, _hn) = node("typed-lan-host", "Host");
    let (guest, _gn) = node("typed-lan-guest", "Guest");
    // Discovery reports the host at its LAN address; the user types another one.
    guest.saw(host.id(), "Host".into(), SocketAddr::from(([192, 168, 0, 4], host.port())));
    guest.pair(addr(&host), host.show_code());
    until("pairing", || connected(&guest, &host) && connected(&host, &guest));
    let saved = std::fs::read_to_string(dir("typed-lan-guest").join("cfg").join("peers.txt")).unwrap();
    assert!(saved.contains(&addr(&host).to_string()), "the typed address is saved: {saved}");
}

/// Going to sleep closes links at once, so the peer stops showing "connected" without waiting
/// out 35 s of silence, and no link comes back (a DarkWake redial) until the machine wakes.
#[test]
fn a_sleeping_node_drops_its_links_until_it_wakes() {
    let (host, _hn) = node("sleep-host", "Host");
    let (guest, _gn) = node("sleep-guest", "Guest");
    guest.pair(addr(&host), host.show_code());
    until("pairing", || connected(&guest, &host) && connected(&host, &guest));
    for (sleeper, other) in [(&host, &guest), (&guest, &host)] {
        sleeper.set_sleeping(true);
        let t = Instant::now();
        until("the peer to see the link go", || !connected(other, sleeper));
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        // The guest redials the typed address every 5 s: refused (or not made) while asleep.
        std::thread::sleep(Duration::from_secs(6));
        assert!(!connected(other, sleeper) && !connected(sleeper, other), "a link came back during sleep");
        sleeper.set_sleeping(false);
        until("a reconnect after waking", || connected(&guest, &host) && connected(&host, &guest));
    }
}

/// `-- --ignored --exact idle_nodes` / `idle_link`, watched with `top -stats csw`: what one idle
/// link adds to two nodes.
#[test]
#[ignore]
fn idle_nodes() {
    let (_a, _an) = node("idle-a", "A");
    let (_b, _bn) = node("idle-b", "B");
    std::thread::sleep(Duration::from_secs(45));
}

#[test]
#[ignore]
fn idle_link() {
    let (host, _hn) = node("idle-host", "Host");
    let (guest, _gn) = node("idle-guest", "Guest");
    guest.pair(addr(&host), host.show_code());
    until("pairing", || connected(&guest, &host));
    std::thread::sleep(Duration::from_secs(45));
}

/// The same file to the same device while it is still waiting or on the wire is a mistake
/// (two copies, twice the time): the second is not queued, and the user is told.
#[test]
fn a_file_already_on_its_way_is_not_queued_again() {
    let (host, _hn) = node("dup-host", "Host");
    let (guest, gn) = node("dup-guest", "Guest");
    guest.pair(addr(&host), host.show_code());
    until("pairing", || connected(&guest, &host) && connected(&host, &guest));
    let src = dir("dup-src");
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).unwrap();
    let big = src.join("big.bin");
    std::fs::File::create(&big).unwrap().set_len(4 << 30).unwrap();
    // Once on the wire, once waiting (in the same drop), once more after it started.
    guest.send_files(host.id(), vec![big.clone(), big.clone()]);
    std::thread::sleep(Duration::from_millis(300));
    guest.send_file(host.id(), big.clone());
    let (mut keys, mut told) = (std::collections::HashSet::new(), Vec::new());
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        match gn.recv_timeout(Duration::from_millis(50)) {
            Ok(Note::Transfer { key, incoming: false, .. }) => drop(keys.insert(key)),
            Ok(Note::AlreadySending { key, file }) => told.push((key, file)),
            _ => {}
        }
    }
    assert_eq!(keys.len(), 1, "one transfer for one file: {keys:?}");
    // Each refused copy is reported with the transfer it would have repeated, for the UI to point at.
    let key = *keys.iter().next().unwrap();
    assert_eq!(told, vec![(key, "big.bin".to_owned()); 2]);
    guest.cancel(*keys.iter().next().unwrap());
}

/// The next transfer note on `rx` that `want` accepts. A timeout lists the notes passed over.
fn next_transfer(rx: &mpsc::Receiver<Note>, what: &str, want: impl Fn(&Note) -> bool) -> Note {
    let end = Instant::now() + Duration::from_secs(30);
    let mut skipped = Vec::new();
    loop {
        assert!(Instant::now() < end, "timed out waiting for {what}; passed over: {skipped:#?}");
        if let Ok(n) = rx.recv_timeout(Duration::from_millis(100)) {
            if matches!(n, Note::Transfer { .. }) && want(&n) {
                return n;
            }
            // Progress is most of it and says little; keep the rest.
            if !matches!(n, Note::Transfer { finished: false, done, .. } if done > 0) {
                skipped.push(n);
            }
        }
    }
}

/// A send the sender stopped goes again from where the receiver got to, not from the start; a
/// source changed in between starts over.
#[test]
fn a_stopped_send_resumes_where_it_stopped() {
    use std::io::{Seek, SeekFrom, Write};
    let (host, hn) = node("resume-host", "Host");
    let (guest, gn) = node("resume-guest", "Guest");
    guest.pair(addr(&host), host.show_code());
    until("pairing", || connected(&guest, &host) && connected(&host, &guest));
    let src = dir("resume-src");
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).unwrap();
    let big = src.join("big.bin");
    // A debug build on the Windows runner moves about 2.6 MB/s on loopback (64 MiB went past the
    // 30 s wait there): small enough to finish, big enough to stop midway.
    let size: u64 = 16 << 20;
    // Sparse, with marks at both ends and the middle: bytes before and after the resume point.
    let mark = |at: u64, byte: u8| {
        let mut f = std::fs::OpenOptions::new().write(true).open(&big).unwrap();
        f.seek(SeekFrom::Start(at)).unwrap();
        f.write_all(&[byte; 4096]).unwrap();
    };
    std::fs::File::create(&big).unwrap().set_len(size).unwrap();
    for at in [0, size / 2, size - 4096] {
        mark(at, 0xA5);
    }
    let dl = dir("resume-host").join("dl");
    let got = dl.join("big.bin");
    let check_at = |at: &std::path::Path, first: u8| {
        let b = std::fs::read(at).unwrap();
        assert_eq!(b.len() as u64, size);
        assert!(b[..4096].iter().all(|&x| x == first), "start");
        assert!(b[size as usize / 2..][..4096].iter().all(|&x| x == 0xA5), "middle");
        assert!(b[size as usize - 4096..].iter().all(|&x| x == 0xA5), "end");
        assert!(b[4096..8192].iter().all(|&x| x == 0), "a hole stays zero");
    };
    let check = |first: u8| check_at(&got, first);
    let (mut after_in, mut after_out) = (0u64, 0u64);
    let ours = |incoming: bool, after: u64| move |n: &Note| matches!(n, Note::Transfer { incoming: i, key, file, .. } if *i == incoming && *key > after && file.starts_with("big"));
    // Sends big.bin and, once the receiver has written some of it, stops it: a cancel at the
    // sender, or the link dropping (the sender going to sleep). Returns what the receiver kept.
    // (The sender's count is what it queued: bytes still in flight may never land.)
    let stop_midway = |after_in: &mut u64, after_out: &mut u64, drop_link: bool| -> u64 {
        guest.send_file(host.id(), big.clone());
        next_transfer(&hn, "bytes arriving", |n| ours(true, *after_in)(n) && matches!(n, Note::Transfer { done, finished: false, .. } if *done > 0));
        let Note::Transfer { key: k_out, .. } = next_transfer(&gn, "the send under way", |n| ours(false, *after_out)(n)) else { unreachable!() };
        if drop_link {
            guest.set_sleeping(true);
        } else {
            guest.cancel(k_out);
        }
        let Note::Transfer { key: k_in, done, .. } = next_transfer(&hn, "the receiver to stop", |n| ours(true, *after_in)(n) && matches!(n, Note::Transfer { finished: true, failed: true, .. })) else { unreachable!() };
        next_transfer(&gn, "the sender to stop", |n| matches!(n, Note::Transfer { key, finished: true, .. } if *key == k_out));
        assert!(done < size, "stopped before the end");
        (*after_in, *after_out) = (k_in, k_out);
        if drop_link {
            // Woken before the old link's reader has seen the close, "connected" would still
            // see that link, and the next send could find none (Windows, about 1 run in 10).
            until("the old link to go", || !connected(&guest, &host) && !connected(&host, &guest));
            guest.set_sleeping(false);
            until("a reconnect after waking", || connected(&guest, &host) && connected(&host, &guest));
        }
        // The receiver notes the stop before it would delete the part: give it the time to.
        std::thread::sleep(Duration::from_millis(300));
        assert!(dir_has_part(&dl), "a send stopped by the sender keeps what arrived (link dropped: {drop_link})");
        done
    };
    // Sends it again; returns where the receiver started.
    let again = |after_in: &mut u64, after_out: &mut u64| -> u64 {
        guest.send_file(host.id(), big.clone());
        // The sender first: if the try never reaches the receiver, its notes say why.
        let Note::Transfer { key: k_out, .. } = next_transfer(&gn, "the sender to finish", |n| ours(false, *after_out)(n) && matches!(n, Note::Transfer { finished: true, failed: false, .. })) else { unreachable!() };
        let Note::Transfer { key, done: from, .. } = next_transfer(&hn, "the next try", ours(true, *after_in)) else { unreachable!() };
        next_transfer(&hn, "the next try to finish", |n| matches!(n, Note::Transfer { key: k, finished: true, failed: false, .. } if *k == key));
        (*after_in, *after_out) = (key, k_out);
        from
    };

    for drop_link in [false, true] {
        let kept = stop_midway(&mut after_in, &mut after_out, drop_link);
        assert!(kept > 0);
        assert_eq!(again(&mut after_in, &mut after_out), kept, "picks up where it stopped (link dropped: {drop_link})");
        check(0xA5);
        assert!(!dir_has_part(&dl));
        std::fs::remove_file(&got).unwrap();
    }

    // A file of that name turned up while the part waited: it stays, the send lands beside it.
    stop_midway(&mut after_in, &mut after_out, false);
    std::fs::write(&got, b"mine").unwrap();
    again(&mut after_in, &mut after_out);
    assert!(std::fs::read(&got).unwrap() == b"mine", "not overwritten");
    check_at(&dl.join("big (1).bin"), 0xA5);
    assert!(!dir_has_part(&dl));
    std::fs::remove_file(&got).unwrap();
    std::fs::remove_file(dl.join("big (1).bin")).unwrap();

    // Same size, new content and time: what arrived before is not this file any more.
    stop_midway(&mut after_in, &mut after_out, false);
    mark(0, 0x5A);
    let f = std::fs::OpenOptions::new().write(true).open(&big).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5)).unwrap();
    drop(f);
    assert_eq!(again(&mut after_in, &mut after_out), 0, "a changed source starts over");
    check(0x5A);
    assert!(!dir_has_part(&dl), "the stale part is gone");
    std::fs::remove_dir_all(&src).unwrap();
    std::fs::remove_dir_all(dir("resume-host")).unwrap();
}

#[derive(Clone, Default)]
struct Rec(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl aun_core::kvm::Sink for Rec {
    fn move_by(&mut self, dx: i32, dy: i32, held: u8) {
        self.0.lock().unwrap().push(format!("move {dx} {dy} held {held}"));
    }
    fn button(&mut self, b: u8, down: bool, _: u8) {
        self.0.lock().unwrap().push(format!("button {b} {}", if down { "down" } else { "up" }));
    }
    fn wheel(&mut self, dx: i32, dy: i32) {
        self.0.lock().unwrap().push(format!("wheel {dx} {dy}"));
    }
    fn key(&mut self, hid: u16, down: bool) {
        self.0.lock().unwrap().push(format!("key {hid:#x} {}", if down { "down" } else { "up" }));
    }
}

/// Input from a peer lands only while allowed, and a button or key it held is let go when the
/// link drops: a mouse button or Shift stuck down at the other desk is the accident to prevent.
#[test]
fn injected_input_is_allowed_only_when_switched_on_and_let_go_when_the_link_drops() {
    use aun_core::wire::Frame;
    let (host, hn) = node("kvm-host", "Host");
    let (guest, _gn) = node("kvm-guest", "Guest");
    let rec = Rec::default();
    let r = rec.clone();
    host.set_sink(move || Box::new(r.clone()));
    guest.pair(addr(&host), host.show_code());
    until("pairing", || connected(&guest, &host) && connected(&host, &guest));
    assert!(guest.peers()[0].kvm && host.peers()[0].kvm, "both builds speak the input frames");
    let log = || rec.0.lock().unwrap().clone();

    // Off (the default): dropped. A file sent after it arrives after it was read.
    assert!(guest.send_input(host.id(), Frame::KvmButton { button: 0, down: true, clicks: 1 }));
    let f = dir("kvm-guest").join("marker.txt");
    std::fs::write(&f, b"x").unwrap();
    guest.send_file(host.id(), f);
    next_transfer(&hn, "the marker file", |n| matches!(n, Note::Transfer { finished: true, .. }));
    host.set_kvm(true);
    assert!(guest.send_input(host.id(), Frame::KvmWheel { dx: 0, dy: 3 }));
    until("the wheel", || !log().is_empty());
    assert_eq!(log(), ["wheel 0 3"]);

    assert!(guest.send_input(host.id(), Frame::KvmButton { button: 0, down: true, clicks: 1 }));
    assert!(guest.send_input(host.id(), Frame::KvmMove { route: 1, mn: 1, cx: 7, cy: -2 }));
    assert!(guest.send_input(host.id(), Frame::KvmKey { hid: 0xE1, down: true }));
    until("the shift drag", || log().len() == 4);
    guest.set_sleeping(true);
    until("the release", || log().len() == 6);
    assert_eq!(log(), ["wheel 0 3", "button 0 down", "move 7 -2 held 1", "key 0xe1 down", "key 0xe1 up", "button 0 up"]);
    // The guest drops its own link entry once its reader ends, which can come after the host's
    // release (seen on Windows CI).
    until("the guest to drop the link", || !connected(&guest, &host));
    assert!(!guest.send_input(host.id(), Frame::KvmLeave), "no link, nothing sent");
}

/// Forwards one connection to `to` until `frozen`, then stops passing bytes without closing
/// anything: a link stalled on bad Wi-Fi or a peer hung, not one that ended.
fn relay(to: SocketAddr, frozen: std::sync::Arc<std::sync::atomic::AtomicBool>) -> SocketAddr {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let at = l.local_addr().unwrap();
    std::thread::spawn(move || {
        let (a, _) = l.accept().unwrap();
        let b = std::net::TcpStream::connect(to).unwrap();
        for (mut from, mut into) in [(a.try_clone().unwrap(), b.try_clone().unwrap()), (b, a)] {
            let frozen = frozen.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 64 << 10];
                loop {
                    let Ok(n @ 1..) = from.read(&mut buf) else { return };
                    // Frozen: hold both ends open and pass nothing more, the stall is the point.
                    while frozen.load(std::sync::atomic::Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    if into.write_all(&buf[..n]).is_err() {
                        return;
                    }
                }
            });
        }
    });
    at
}

/// While input goes across, a link that goes silent without closing makes the receiving side
/// let go of what it holds within a second and tells the sending side within half of one. A quiet
/// link that is still up does neither.
#[test]
fn a_stalled_link_lets_go_of_held_input_and_tells_the_sender() {
    use aun_core::wire::Frame;
    let (host, _hn) = node("stall-host", "Host");
    let (guest, gn) = node("stall-guest", "Guest");
    let rec = Rec::default();
    let r = rec.clone();
    host.set_sink(move || Box::new(r.clone()));
    host.set_kvm(true);
    let frozen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    guest.pair(relay(addr(&host), frozen.clone()), host.show_code());
    until("pairing", || connected(&guest, &host) && connected(&host, &guest));
    let log = || rec.0.lock().unwrap().clone();

    assert!(guest.send_input(host.id(), Frame::KvmButton { button: 0, down: true, clicks: 1 }));
    assert!(guest.send_input(host.id(), Frame::KvmKey { hid: 0xE1, down: true }));
    until("the shift drag", || log().len() == 2);
    // Held with nothing else sent: each side's beat keeps the other from giving up.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(log().len(), 2, "nothing let go while the link is up");
    assert!(!gn.try_iter().any(|n| matches!(n, Note::InputLost(_))), "the sender isn't told while the link is up");

    frozen.store(true, std::sync::atomic::Ordering::Relaxed);
    let at = Instant::now();
    let lost = std::iter::from_fn(|| gn.recv_timeout(Duration::from_secs(5)).ok()).find(|n| matches!(n, Note::InputLost(_)));
    assert_eq!(lost, Some(Note::InputLost(host.id())), "told within 5 s");
    let told = at.elapsed();
    until("the release", || log().len() == 4);
    let released = at.elapsed();
    assert_eq!(log()[2..], ["key 0xe1 up", "button 0 up"]);
    assert!(told < Duration::from_millis(900), "told after {told:?}");
    assert!(released < Duration::from_millis(1600), "let go after {released:?}");
    assert!(connected(&host, &guest), "let go for the silence, not a closed link");
}
