//! A device on the desk: listens, dials paired peers, pairs with a 6-digit code, and moves
//! clipboard text and files over one encrypted link per peer.
//!
//! Pairing: Noise XX gives both sides each other's static key and a handshake hash `h`. SPAKE2
//! with the code then yields a key only the two code holders share, and each side proves it with
//! HMAC(key, role ‖ h). A man in the middle has a different `h` on each leg and can't learn the
//! code offline, so it fails the check. After that the peer's static key is pinned.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use snow::StatelessTransportState;
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};

use crate::store::{self, Id};
use crate::wire::{self, Frame, OpenReader, SealWriter};
use crate::{clip, discovery};

pub const PORT: u16 = 47810;
const PING: Duration = Duration::from_secs(10);
const DEAD: Duration = Duration::from_secs(35);
const CODE_TRIES: u32 = 3;
const STACK: usize = 256 << 10;
/// How often paired devices at a typed address are redialled, and a wake from sleep looked for.
const KEEP: Duration = Duration::from_secs(5);
/// Marked asleep this long (awake time) with input seen: the wake notice was missed.
const MISSED_WAKE: Duration = Duration::from_secs(60);

pub struct Config {
    pub dir: PathBuf,
    pub downloads: PathBuf,
    pub name: String,
    /// 0 picks a free port. Discovery off is for tests and two nodes on one machine.
    pub port: u16,
    pub discovery: bool,
    pub clipboard: bool,
    /// Send copies that password managers mark private, too.
    pub secrets: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    /// Peers, links or the pairing code changed: read `peers()` / `code()` again.
    Changed,
    Info(String),
    /// A file going out or coming in. `path` is the source, or where an incoming file lands.
    /// `cancelled` (with `failed`) when either side called it off. `peer_id` is who it is with,
    /// to send it again.
    /// A file asked to go again while it is still going there: not queued. `key` is the
    /// transfer already under way (or waiting) for it.
    AlreadySending { key: u64, file: String },
    /// Files (or folders) were copied here: the app may offer to send them.
    Copied(Vec<PathBuf>),
    Transfer { key: u64, peer_id: Id, peer: String, file: String, path: PathBuf, done: u64, total: u64, incoming: bool, finished: bool, failed: bool, cancelled: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct PeerView {
    pub id: Id,
    pub name: String,
    pub addr: Option<SocketAddr>,
    pub paired: bool,
    pub nearby: bool,
    pub connected: bool,
}

struct Link {
    conn: u64,
    initiator: Id,
    tx: SyncSender<Frame>,
    sock: TcpStream,
    /// The peer's build can say where a stopped file goes on from.
    resume: bool,
}

#[derive(Default)]
struct Peer {
    name: String,
    addr: Option<SocketAddr>,
    nearby: bool,
    paired: bool,
    link: Option<Link>,
    /// The address the user typed to pair, kept for networks where discovery doesn't reach.
    typed: Option<SocketAddr>,
}

struct Code {
    digits: String,
    misses: u32,
}

/// Files waiting for one peer, sent one at a time: a pile dropped at once would otherwise open
/// more files at the receiver than it allows. Present while its sender thread runs.
#[derive(Default)]
struct Outbox {
    queue: VecDeque<(u64, PathBuf)>,
    /// The file on the wire: (frame id, key).
    current: Option<(u32, u64)>,
    /// Its path, so the same file isn't queued again while it goes.
    sending: Option<PathBuf>,
    /// Where the reader hands over the peer's `FileFrom` for the file on the wire.
    from: Option<SyncSender<u64>>,
}

/// What a receiver kept of a file whose sender stopped or whose link dropped.
struct Partial {
    size: u64,
    mtime: u64,
    /// Where the finished file goes; its `.part` holds what arrived.
    path: PathBuf,
}

#[derive(Default)]
struct St {
    peers: HashMap<Id, Peer>,
    code: Option<Code>,
    dialing: HashSet<Id>,
    next: u64,
    outboxes: HashMap<Id, Outbox>,
    /// Transfers (by key) to stop at the next chunk.
    cancelled: HashSet<u64>,
    /// Files that stopped on their way to a peer: sent again, they ask where to go on from.
    unfinished: HashSet<(Id, PathBuf)>,
    /// Kept `.part`s by sender and file name.
    /// shortcut: in memory, so a restart forgets them (the `.part` stays, unused); persist if
    /// stopped sends across restarts turn out to matter.
    partials: HashMap<(Id, String), Partial>,
}

struct Shared {
    key: Vec<u8>,
    id: Id,
    name: String,
    port: u16,
    dir: PathBuf,
    downloads: Mutex<PathBuf>,
    clip_on: AtomicBool,
    secrets_on: AtomicBool,
    /// Between the OS saying it will sleep and saying it woke: no links, in or out.
    asleep: AtomicBool,
    /// While asleep: the input idle time at the last look (`power::idle`), and since when.
    idle: Mutex<(Option<Duration>, Instant)>,
    clip_tx: Mutex<Option<Sender<Option<clip::Content>>>>,
    st: Mutex<St>,
    notify: Box<dyn Fn(Note) + Send + Sync>,
}

#[derive(Clone)]
pub struct Node(Arc<Shared>);

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = thread::Builder::new().name(name.into()).stack_size(STACK).spawn(f);
}

impl Node {
    pub fn start(cfg: Config, notify: impl Fn(Note) + Send + Sync + 'static) -> io::Result<Node> {
        std::fs::create_dir_all(&cfg.dir)?;
        crate::log_to(cfg.dir.join("aun.log"));
        let me = store::identity(&cfg.dir)?;
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, cfg.port)).or_else(|_| TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)))?;
        let port = listener.local_addr()?.port();
        let mut st = St::default();
        for (id, (name, typed)) in store::load_peers(&cfg.dir) {
            st.peers.insert(id, Peer { name, paired: true, typed, addr: typed, ..Default::default() });
        }
        let node = Node(Arc::new(Shared {
            key: me.private,
            id: me.id,
            name: store::clean_name(&cfg.name),
            port,
            dir: cfg.dir,
            downloads: Mutex::new(cfg.downloads),
            clip_on: AtomicBool::new(cfg.clipboard),
            secrets_on: AtomicBool::new(cfg.secrets),
            asleep: AtomicBool::new(false),
            idle: Mutex::new((None, Instant::now())),
            clip_tx: Mutex::new(None),
            st: Mutex::new(st),
            notify: Box::new(notify),
        }));

        let n = node.clone();
        spawn("aun-accept", move || {
            for sock in listener.incoming().flatten() {
                let n = n.clone();
                spawn("aun-link", move || n.run_link(sock, false, None));
            }
        });

        if cfg.discovery {
            let n = node.clone();
            let lost = node.clone();
            spawn("aun-discovery", move || {
                discovery::run(n.0.id, n.0.port, n.0.name.clone(), move |id, name, addr, is_new| n.seen(id, name, addr, is_new), move |id| lost.lost(id))
            });
        }

        let n = node.clone();
        spawn("aun-keep", move || n.keep());

        let (tx, rx) = mpsc::channel();
        *node.0.clip_tx.lock().unwrap() = Some(tx);
        let (n, s, a) = (node.clone(), node.clone(), node.clone());
        let active = move || a.clipboard() && a.st().peers.values().any(|p| p.link.is_some());
        spawn("aun-clip", move || clip::run(rx, active, move || s.secrets(), move |c| n.broadcast_clip(c)));
        Ok(node)
    }

    pub fn id(&self) -> Id {
        self.0.id
    }
    pub fn name(&self) -> &str {
        &self.0.name
    }
    pub fn port(&self) -> u16 {
        self.0.port
    }

    fn st(&self) -> MutexGuard<'_, St> {
        self.0.st.lock().unwrap()
    }

    pub fn peers(&self) -> Vec<PeerView> {
        let st = self.st();
        let mut v: Vec<_> = st
            .peers
            .iter()
            .filter(|(_, p)| p.paired || p.nearby)
            .map(|(id, p)| PeerView { id: *id, name: p.name.clone(), addr: p.addr, paired: p.paired, nearby: p.nearby, connected: p.link.is_some() })
            .collect();
        v.sort_by(|a, b| (!a.paired, &a.name).cmp(&(!b.paired, &b.name)));
        v
    }

    pub fn downloads(&self) -> PathBuf {
        self.0.downloads.lock().unwrap().clone()
    }
    /// Where files from now on land; one already arriving finishes where it started.
    pub fn set_downloads(&self, dir: PathBuf) {
        *self.0.downloads.lock().unwrap() = dir;
    }

    pub fn clipboard(&self) -> bool {
        self.0.clip_on.load(Ordering::Relaxed)
    }
    pub fn set_clipboard(&self, on: bool) {
        self.0.clip_on.store(on, Ordering::Relaxed);
        if on {
            self.wake_clip();
        }
    }
    /// Sharing may have started: the clipboard thread, asleep while nothing would be sent, looks again.
    fn wake_clip(&self) {
        if let Some(tx) = self.0.clip_tx.lock().unwrap().as_ref() {
            let _ = tx.send(None);
        }
    }
    /// The OS is about to sleep (true) or woke (false).
    pub fn set_sleeping(&self, on: bool) {
        if self.0.asleep.swap(on, Ordering::Relaxed) == on {
            return;
        }
        crate::log(format_args!("{}", if on { "going to sleep: closing links" } else { "woke: links allowed again" }));
        if on {
            *self.0.idle.lock().unwrap() = (crate::power::idle(), Instant::now());
            self.close_links();
        }
    }

    /// The safety net for a wake notice that never came: input since the last look means
    /// someone is using the machine, which a DarkWake never has. Not in the first minute (awake
    /// time; `Instant` stops while the Mac sleeps): a hand on the mouse as the machine goes down
    /// isn't a wake. `idle`: `power::idle()`.
    fn input_while_asleep(&self, idle: Option<Duration>) {
        let (was, since) = {
            let mut g = self.0.idle.lock().unwrap();
            (std::mem::replace(&mut g.0, idle), g.1)
        };
        if let (Some(was), Some(now)) = (was, idle)
            && now < was
            && since.elapsed() >= MISSED_WAKE
            && self.asleep()
        {
            crate::log(format_args!("input {:.0} s ago while marked asleep: the wake notice was missed", now.as_secs_f64()));
            self.set_sleeping(false);
        }
    }
    fn asleep(&self) -> bool {
        self.0.asleep.load(Ordering::Relaxed)
    }
    pub fn secrets(&self) -> bool {
        self.0.secrets_on.load(Ordering::Relaxed)
    }
    pub fn set_secrets(&self, on: bool) {
        self.0.secrets_on.store(on, Ordering::Relaxed);
    }

    /// Shows a fresh one-time code for a guest to type. It stays valid until it pairs a device,
    /// takes three wrong guesses, or is hidden.
    pub fn show_code(&self) -> String {
        let mut r = [0u8; 4];
        getrandom::fill(&mut r).expect("OS randomness");
        let digits = format!("{:06}", u32::from_le_bytes(r) % 1_000_000);
        self.st().code = Some(Code { digits: digits.clone(), misses: 0 });
        (self.0.notify)(Note::Changed);
        digits
    }

    pub fn code(&self) -> Option<String> {
        self.st().code.as_ref().map(|c| c.digits.clone())
    }

    pub fn hide_code(&self) {
        self.st().code = None;
        (self.0.notify)(Note::Changed);
    }

    /// Pairs with the device at `addr` showing `code`. The answer arrives as notes.
    pub fn pair(&self, addr: SocketAddr, code: String) {
        let n = self.clone();
        spawn("aun-pair", move || match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
            Ok(sock) => n.run_link(sock, true, Some(code)),
            Err(e) => (n.0.notify)(Note::Info(format!("연결 실패: {e}"))),
        });
    }

    pub fn forget(&self, id: Id) {
        let mut st = self.st();
        if let Some(p) = st.peers.get_mut(&id) {
            p.paired = false;
            if let Some(l) = p.link.take() {
                let _ = l.sock.shutdown(std::net::Shutdown::Both);
            }
        }
        self.save(&st);
        drop(st);
        (self.0.notify)(Note::Changed);
    }

    fn save(&self, st: &St) {
        let paired = st.peers.iter().filter(|(_, p)| p.paired).map(|(id, p)| (id, p.name.as_str(), p.typed));
        if let Err(e) = store::save_peers(&self.0.dir, paired) {
            (self.0.notify)(Note::Info(format!("페어링 정보를 저장하지 못했어요: {e}")));
        }
    }

    /// What discovery does with each announce from another device; public for tests.
    pub fn saw(&self, id: Id, name: String, addr: SocketAddr) {
        self.seen(id, name, addr, false);
    }

    fn seen(&self, id: Id, name: String, addr: SocketAddr, is_new: bool) {
        let mut st = self.st();
        let p = st.peers.entry(id).or_default();
        let changed = is_new || p.addr != Some(addr) || !p.nearby || p.name != name;
        p.nearby = true;
        p.addr = Some(addr);
        if !p.paired {
            p.name = name;
        }
        let dial = p.paired && p.link.is_none() && st.dialing.insert(id);
        drop(st);
        if changed {
            (self.0.notify)(Note::Changed);
        }
        if dial {
            self.dial(id, addr);
        }
    }

    /// Dials a paired peer; the caller has put it in `dialing`.
    fn dial(&self, id: Id, addr: SocketAddr) {
        let n = self.clone();
        spawn("aun-dial", move || {
            if !n.asleep() {
                match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
                    Ok(sock) => n.clone().run_link(sock, true, None),
                    Err(e) => crate::log(format_args!("dial {addr}: {e}")),
                }
            }
            n.st().dialing.remove(&id);
        });
    }

    /// Every few seconds: redial paired devices at a typed address, and after the machine slept,
    /// close every link at once. A link from before the sleep is usually dead but would hold its
    /// place (and block a new one) until 35 s of silence; closed, the next announce or redial
    /// brings it back in seconds.
    fn keep(&self) {
        let mut then = (SystemTime::now(), Instant::now());
        loop {
            thread::sleep(KEEP);
            let wall = then.0.elapsed().unwrap_or_default();
            if slept(wall, then.1.elapsed()) {
                crate::log(format_args!("woke after {:.0} s asleep: closing links", wall.saturating_sub(then.1.elapsed()).as_secs_f64()));
                self.close_links();
            }
            then = (SystemTime::now(), Instant::now());
            if self.asleep() {
                self.input_while_asleep(crate::power::idle());
            }
            let due: Vec<_> = {
                let mut st = self.st();
                let due: Vec<_> = st.peers.iter().filter(|(_, p)| p.paired && p.link.is_none()).filter_map(|(id, p)| Some((*id, p.typed?))).collect();
                due.into_iter().filter(|(id, _)| st.dialing.insert(*id)).collect()
            };
            for (id, addr) in due {
                self.dial(id, addr);
            }
        }
    }

    fn close_links(&self) {
        for p in self.st().peers.values() {
            if let Some(l) = &p.link {
                let _ = l.sock.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    fn lost(&self, id: Id) {
        if let Some(p) = self.st().peers.get_mut(&id) {
            p.nearby = false;
        }
        (self.0.notify)(Note::Changed);
    }

    /// Dials a paired peer directly (tests, or an address the user typed).
    pub fn connect(&self, addr: SocketAddr) {
        let n = self.clone();
        spawn("aun-dial", move || match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
            Ok(sock) => n.run_link(sock, true, None),
            Err(e) => (n.0.notify)(Note::Info(format!("연결 실패: {e}"))),
        });
    }

    fn broadcast_clip(&self, content: clip::Content) {
        if !self.clipboard() {
            return;
        }
        let frame = match content {
            clip::Content::Files(paths) => return (self.0.notify)(Note::Copied(paths)),
            clip::Content::Text(t) => Frame::Clip(t),
            clip::Content::Png(png) if png.len() < wire::MAX_IMAGE => Frame::ClipImage(png),
            clip::Content::Png(png) => return crate::log(format_args!("clipboard image not sent: {} KB PNG is past the limit", png.len() >> 10)),
        };
        let txs: Vec<_> = self.st().peers.values().filter_map(|p| p.link.as_ref().map(|l| l.tx.clone())).collect();
        for tx in txs {
            let _ = tx.try_send(frame.clone());
        }
    }

    fn to_clipboard(&self, c: clip::Content) {
        if self.clipboard()
            && let Some(tx) = self.0.clip_tx.lock().unwrap().as_ref()
        {
            let _ = tx.send(Some(c));
        }
    }

    pub fn send_file(&self, id: Id, path: PathBuf) {
        self.send_files(id, vec![path]);
    }

    /// Queues files for a connected peer; they go one after another, each shown at once as waiting.
    pub fn send_files(&self, id: Id, paths: Vec<PathBuf>) {
        let mut st = self.st();
        let Some(peer) = st.peers.get(&id).filter(|p| p.link.is_some()).map(|p| p.name.clone()) else {
            drop(st);
            return (self.0.notify)(Note::Info("연결된 기기가 아니에요".into()));
        };
        let mut queued: Vec<(u64, PathBuf)> = Vec::new();
        let mut again = Vec::new();
        for path in paths {
            let o = st.outboxes.get(&id);
            let on_wire = o.and_then(|o| o.current.filter(|_| o.sending.as_ref() == Some(&path))).map(|c| c.1);
            let waiting = o.into_iter().flat_map(|o| &o.queue).chain(&queued).find(|(_, p)| *p == path).map(|(k, _)| *k);
            if let Some(key) = on_wire.or(waiting) {
                again.push((key, path));
                continue;
            }
            st.next += 1;
            queued.push((st.next, path));
        }
        let start = !st.outboxes.contains_key(&id);
        st.outboxes.entry(id).or_default().queue.extend(queued.iter().cloned());
        drop(st);
        for (key, path) in again {
            (self.0.notify)(Note::AlreadySending { key, file: file_name(&path) });
        }
        for (key, path) in queued {
            let total = std::fs::metadata(&path).map_or(0, |m| m.len());
            (self.0.notify)(Note::Transfer { key, peer_id: id, peer: peer.clone(), file: file_name(&path), path, done: 0, total, incoming: false, finished: false, failed: false, cancelled: false });
        }
        if start {
            let n = self.clone();
            spawn("aun-send", move || n.drain_outbox(id, &peer));
        }
    }

    fn drain_outbox(&self, id: Id, peer: &str) {
        loop {
            let (key, path, tx) = {
                let mut st = self.st();
                let Some((key, path)) = st.outboxes.get_mut(&id).and_then(|o| o.queue.pop_front()) else {
                    st.outboxes.remove(&id);
                    return;
                };
                let link = st.peers.get(&id).and_then(|p| p.link.as_ref()).map(|l| (l.tx.clone(), l.resume));
                let ask = link.as_ref().is_some_and(|l| l.1) && st.unfinished.contains(&(id, path.clone()));
                if let Some(o) = st.outboxes.get_mut(&id) {
                    o.current = Some((key as u32, key));
                    o.sending = Some(path.clone());
                }
                (key, path, link.map(|l| (l.0, ask)))
            };
            let res = match tx {
                Some((tx, ask)) => self.stream_file(tx, ask, key, id, peer, &path),
                None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "연결이 끊겼어요")),
            };
            let cancelled = {
                let mut st = self.st();
                if let Some(o) = st.outboxes.get_mut(&id) {
                    o.current = None;
                    o.sending = None;
                    o.from = None;
                }
                if res.is_ok() {
                    st.unfinished.remove(&(id, path.clone()));
                }
                st.cancelled.remove(&key)
            };
            if let Err(e) = res
                && !cancelled
            {
                (self.0.notify)(Note::Info(format!("{} 전송 실패: {e}", path.display())));
            }
        }
    }

    /// Stops a transfer, either way; the other side is told. Stopped here, an incoming one loses
    /// its `.part`; stopped at the sender, the receiver keeps it for the send to go on later.
    pub fn cancel(&self, key: u64) {
        self.st().cancelled.insert(key);
    }

    /// Sends one file; every outcome ends in a finished note. `ask`: it stopped before, so ask the
    /// receiver how much it kept and send only the rest.
    fn stream_file(&self, tx: SyncSender<Frame>, ask: bool, key: u64, peer_id: Id, peer: &str, path: &Path) -> io::Result<()> {
        let cancelled = || self.st().cancelled.contains(&key);
        let meta = std::fs::metadata(path).ok();
        let total = meta.as_ref().map_or(0, |m| m.len());
        let mtime = meta.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as u64);
        let ask = ask && mtime.is_some();
        let name = file_name(path);
        let id = key as u32;
        let gone = || io::Error::new(io::ErrorKind::BrokenPipe, "연결이 끊겼어요");
        let note = |done, finished, failed, cancelled| {
            (self.0.notify)(Note::Transfer { key, peer_id, peer: peer.into(), file: name.clone(), path: path.into(), done, total, incoming: false, finished, failed, cancelled })
        };
        let stopped = || io::Error::other("cancelled");
        if cancelled() {
            note(0, true, true, true);
            return Err(stopped());
        }
        let (from_tx, from) = mpsc::sync_channel(1);
        if ask && let Some(o) = self.st().outboxes.get_mut(&peer_id) {
            o.from = Some(from_tx);
        }
        let opened = File::open(path).and_then(|f| tx.send(Frame::FileStart { id, size: total, name: name.clone(), mtime, resume: ask }).map(|_| f).map_err(|_| gone()));
        let mut f = opened.inspect_err(|_| note(0, true, true, false))?;
        // From here the receiver may hold part of it: a later try asks.
        self.st().unfinished.insert((peer_id, path.into()));
        let offset = if ask {
            let asked = Instant::now();
            loop {
                match from.recv_timeout(Duration::from_millis(200)) {
                    // More than the file holds is nonsense: from the start, then.
                    Ok(o) => break if o <= total { o } else { 0 },
                    Err(_) if cancelled() => {
                        let _ = tx.send(Frame::FileAbort { id, cancelled: true });
                        note(0, true, true, true);
                        return Err(stopped());
                    }
                    Err(_) if asked.elapsed() > DEAD || self.st().peers.get(&peer_id).is_none_or(|p| p.link.is_none()) => {
                        note(0, true, true, false);
                        return Err(gone());
                    }
                    Err(_) => {}
                }
            }
        } else {
            0
        };
        if offset > 0 {
            crate::log(format_args!("sending {name} from {offset} of {total} bytes"));
            f.seek(SeekFrom::Start(offset)).inspect_err(|_| note(0, true, true, false))?;
        }
        let (mut done, mut told) = (offset, Instant::now());
        loop {
            if cancelled() {
                let _ = tx.send(Frame::FileAbort { id, cancelled: true });
                note(done, true, true, true);
                return Err(stopped());
            }
            let mut buf = vec![0u8; wire::FILE_CHUNK];
            let n = match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = tx.send(Frame::FileAbort { id, cancelled: false });
                    note(done, true, true, false);
                    return Err(e);
                }
            };
            buf.truncate(n);
            if tx.send(Frame::FileData { id, data: buf }).is_err() {
                note(done, true, true, false);
                return Err(gone());
            }
            done += n as u64;
            if told.elapsed() > Duration::from_millis(100) {
                told = Instant::now();
                note(done, false, false, false);
            }
        }
        tx.send(Frame::FileEnd { id }).map_err(|_| gone())?;
        note(done, true, done != total, false);
        Ok(())
    }

    /// One link, from handshake to close. `pair` is the code when this side dials to pair.
    fn run_link(self, sock: TcpStream, initiator: bool, pair: Option<String>) {
        // Dropped unanswered: a DarkWake would otherwise bring the link back while "asleep".
        if self.asleep() {
            return;
        }
        let peer_addr = sock.peer_addr().ok();
        if let Err(e) = self.link(sock, initiator, pair.clone()) {
            crate::log(format_args!("link {peer_addr:?} (we dialled: {initiator}) refused before it was up: {e}"));
            if pair.is_some() {
                (self.0.notify)(Note::Info(format!("페어링 실패: {e}")));
            }
        }
    }

    fn link(&self, sock: TcpStream, initiator: bool, pair: Option<String>) -> io::Result<()> {
        sock.set_nodelay(true)?;
        sock.set_read_timeout(Some(Duration::from_secs(10)))?;
        let (t, h, remote) = handshake(&sock, &self.0.key, initiator)?;
        let t = Arc::new(t);
        let mut w = SealWriter::new(t.clone(), sock.try_clone()?);
        // The timeout goes on the handle that reads: on Windows a duplicated socket handle kept
        // the handshake's 10 s, which raced the peer's 10 s pings and closed idle links.
        let rsock = sock.try_clone()?;
        rsock.set_read_timeout(Some(DEAD))?;
        let mut r = OpenReader::new(t, BufReader::with_capacity(128 << 10, rsock));

        wire::write_frame(&mut w, &Frame::Hello { pair: pair.is_some(), name: self.0.name.clone(), resume: true })?;
        w.flush()?;
        let Frame::Hello { pair: they_pair, name, resume } = wire::read_frame(&mut r)? else { return Err(denied("hello")) };
        let name = store::clean_name(&name);

        // Marked paired only when the link registers below: in between, discovery would see a
        // paired device with no link and dial a second one.
        let mut just_paired = false;
        if let Some(code) = pair {
            let (s, msg) = Spake2::<Ed25519Group>::start_a(&Password::new(code.as_bytes()), &SpakeId::new(b"aun-guest"), &SpakeId::new(b"aun-host"));
            wire::write_frame(&mut w, &Frame::Spake(msg))?;
            w.flush()?;
            let Frame::Spake(theirs) = wire::read_frame(&mut r)? else { return Err(denied("spake")) };
            let k = s.finish(&theirs).map_err(|_| denied("spake"))?;
            wire::write_frame(&mut w, &Frame::Confirm(confirm(&k, b"G", &h)))?;
            w.flush()?;
            let Frame::Confirm(c) = wire::read_frame(&mut r)? else { return Err(denied("코드가 맞지 않아요")) };
            if !verify(&k, b"H", &h, &c) {
                return Err(denied("코드가 맞지 않아요"));
            }
            just_paired = true;
        } else if they_pair {
            let code = self.code().ok_or_else(|| denied("no code"))?;
            let (s, msg) = Spake2::<Ed25519Group>::start_b(&Password::new(code.as_bytes()), &SpakeId::new(b"aun-guest"), &SpakeId::new(b"aun-host"));
            let Frame::Spake(theirs) = wire::read_frame(&mut r)? else { return Err(denied("spake")) };
            wire::write_frame(&mut w, &Frame::Spake(msg))?;
            w.flush()?;
            let k = s.finish(&theirs).map_err(|_| denied("spake"))?;
            let ok = matches!(wire::read_frame(&mut r)?, Frame::Confirm(c) if verify(&k, b"G", &h, &c));
            if !ok {
                let mut st = self.st();
                if let Some(c) = st.code.as_mut() {
                    c.misses += 1;
                    if c.misses >= CODE_TRIES {
                        st.code = None;
                    }
                }
                drop(st);
                (self.0.notify)(Note::Info(format!("{name}: 잘못된 코드로 페어링을 시도했어요")));
                return Err(denied("bad code"));
            }
            wire::write_frame(&mut w, &Frame::Confirm(confirm(&k, b"H", &h)))?;
            w.flush()?;
            self.st().code = None;
            just_paired = true;
        } else if !self.st().peers.get(&remote).is_some_and(|p| p.paired) {
            return Err(denied("not paired"));
        }

        // Authenticated. Register, then run the writer on its own thread and read here.
        let (tx, rx) = mpsc::sync_channel::<Frame>(4);
        let (conn, dup) = {
            let mut st = self.st();
            st.next += 1;
            let conn = st.next;
            let initiator_id = if initiator { self.0.id } else { remote };
            let link = Link { conn, initiator: initiator_id, tx: tx.clone(), sock: sock.try_clone()?, resume };
            let p = st.peers.entry(remote).or_default();
            p.name = name.clone();
            p.paired |= just_paired;
            // Paired at an address discovery didn't report (typed, often a VPN while also on the
            // same Wi-Fi): keep it to redial once the LAN is gone.
            let dialled = sock.peer_addr().ok();
            if just_paired && initiator && !(p.nearby && p.addr == dialled) {
                p.typed = dialled;
                p.addr = dialled;
            }
            // Both sides dialled: both keep the link whose dialler has the smaller id. The same
            // dialler again means it lost the old link (restart, sleep): the new one wins.
            let dup = p.link.as_ref().is_some_and(|old| old.initiator < initiator_id);
            if !dup
                && let Some(old) = p.link.replace(link)
            {
                crate::log(format_args!("link {} to {name} replaces link {}", conn, old.conn));
                let _ = old.sock.shutdown(std::net::Shutdown::Both);
            }
            if just_paired {
                self.save(&st);
            }
            (conn, dup)
        };
        if just_paired {
            (self.0.notify)(Note::Info(format!("{name}와(과) 페어링했어요")));
        }
        (self.0.notify)(Note::Changed);
        let peer_addr = sock.peer_addr().ok();
        if dup {
            crate::log(format_args!("link {conn} to {name} at {peer_addr:?} dropped: a link dialled by the smaller id is up"));
            return Ok(());
        }
        crate::log(format_args!("link {conn} to {name} at {peer_addr:?} up (we dialled: {initiator})"));
        self.wake_clip();
        let up = Instant::now();
        let wsock = sock.try_clone()?;
        spawn("aun-write", move || writer(w, rx, wsock));

        let mut rx_files = Incoming::new(self, name.clone(), remote, tx);
        let res = self.read_loop(&mut r, &mut rx_files);
        rx_files.keep_all();
        let _ = sock.shutdown(std::net::Shutdown::Both);
        {
            let mut st = self.st();
            if let Some(p) = st.peers.get_mut(&remote)
                && p.link.as_ref().is_some_and(|l| l.conn == conn)
            {
                p.link = None;
            }
        }
        (self.0.notify)(Note::Changed);
        // Authenticated links end by closing; that is not a failed pairing.
        let why = res.err().map_or_else(|| "closed".into(), |e| format!("{:?}: {e}", e.kind()));
        crate::log(format_args!("link {conn} to {name} down after {:.1} s: {why}", up.elapsed().as_secs_f64()));
        Ok(())
    }

    fn read_loop(&self, r: &mut impl Read, files: &mut Incoming) -> io::Result<()> {
        loop {
            files.tell_cancels();
            match wire::read_frame(r)? {
                Frame::Ping => {}
                Frame::Clip(text) if text.len() <= clip::MAX_TEXT => self.to_clipboard(clip::Content::Text(text)),
                Frame::Clip(_) => {}
                Frame::ClipImage(png) => self.to_clipboard(clip::Content::Png(png)),
                Frame::FileStart { id, size, name, mtime, resume } => files.start(id, size, name, mtime, resume)?,
                Frame::FileData { id, data } => files.data(id, &data)?,
                Frame::FileEnd { id } => files.end(id),
                // A cancel at the sender keeps what arrived; a failure there (its read) doesn't.
                Frame::FileAbort { id, cancelled } => files.abort(id, cancelled, cancelled),
                Frame::FileFrom { id, offset } => {
                    let st = self.st();
                    if let Some(o) = st.outboxes.get(&files.remote)
                        && o.current.is_some_and(|c| c.0 == id)
                        && let Some(from) = &o.from
                    {
                        let _ = from.try_send(offset);
                    }
                }
                Frame::FileCancel { id } => {
                    let mut st = self.st();
                    let key = st.outboxes.get(&files.remote).and_then(|o| o.current).filter(|c| c.0 == id).map(|c| c.1);
                    st.cancelled.extend(key);
                }
                _ => return Err(denied("unexpected frame")),
            }
        }
    }
}

/// Whether the wall clock ran well past the monotonic one, which stops while the machine sleeps
/// (macOS `CLOCK_UPTIME_RAW`). A clock set forward by more than the margin also counts.
pub fn slept(wall: Duration, mono: Duration) -> bool {
    wall > mono + Duration::from_secs(10)
}

fn file_name(path: &Path) -> String {
    path.file_name().and_then(|s| s.to_str()).unwrap_or("file").to_owned()
}

fn denied(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why.to_owned())
}

fn confirm(k: &[u8], role: &[u8], h: &[u8]) -> Vec<u8> {
    let mut m = <Hmac<Sha256>>::new_from_slice(k).unwrap();
    m.update(role);
    m.update(h);
    m.finalize().into_bytes().to_vec()
}

fn verify(k: &[u8], role: &[u8], h: &[u8], tag: &[u8]) -> bool {
    let mut m = <Hmac<Sha256>>::new_from_slice(k).unwrap();
    m.update(role);
    m.update(h);
    m.verify_slice(tag).is_ok()
}

fn handshake(sock: &TcpStream, key: &[u8], initiator: bool) -> io::Result<(StatelessTransportState, Vec<u8>, Id)> {
    let b = snow::Builder::new(wire::NOISE.parse().unwrap()).local_private_key(key);
    let mut hs = if initiator { b.build_initiator() } else { b.build_responder() }.map_err(io::Error::other)?;
    let (mut s, mut buf, mut msg) = (sock, vec![0u8; 1024], vec![0u8; 1024]);
    let e = |e: snow::Error| io::Error::new(io::ErrorKind::InvalidData, e.to_string());
    // XX: -> e; <- e, ee, s, es; -> s, se
    for step in 0..3 {
        if (step % 2 == 0) == initiator {
            let n = hs.write_message(&[], &mut msg).map_err(e)?;
            wire::send_raw(&mut s, &msg[..n])?;
        } else {
            let n = wire::recv_raw(&mut s, &mut buf)?;
            hs.read_message(&buf[..n], &mut msg).map_err(e)?;
        }
    }
    let h = hs.get_handshake_hash().to_vec();
    let remote: Id = hs.get_remote_static().and_then(|r| r.try_into().ok()).ok_or_else(|| denied("no static key"))?;
    Ok((hs.into_stateless_transport_mode().map_err(e)?, h, remote))
}

/// Writes queued frames; flushes when the queue runs dry so small frames share one Noise message.
fn writer(mut w: SealWriter<TcpStream>, rx: Receiver<Frame>, sock: TcpStream) {
    let res = (|| -> io::Result<()> {
        loop {
            let f = match rx.recv_timeout(PING) {
                Ok(f) => f,
                Err(RecvTimeoutError::Timeout) => Frame::Ping,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            };
            wire::write_frame(&mut w, &f)?;
            while let Ok(f) = rx.try_recv() {
                wire::write_frame(&mut w, &f)?;
            }
            w.flush()?;
        }
    })();
    if let Err(e) = res {
        crate::log(format_args!("writer to {:?} stopped: {:?}: {e}", sock.peer_addr().ok(), e.kind()));
        let _ = sock.shutdown(std::net::Shutdown::Both);
    }
}

struct InFile {
    path: PathBuf,
    /// As the sender named it, and its time: together they tell a later try of this file.
    name: String,
    mtime: Option<u64>,
    out: BufWriter<File>,
    size: u64,
    done: u64,
    key: u64,
    told: Instant,
}

struct Incoming<'a> {
    node: &'a Node,
    peer: String,
    remote: Id,
    /// This link's queue, for telling the sender a file was cancelled.
    tx: SyncSender<Frame>,
    files: HashMap<u32, InFile>,
    /// Cancels not yet sent: the queue was full (it holds our own outgoing chunks).
    untold: Vec<u32>,
}

impl<'a> Incoming<'a> {
    fn new(node: &'a Node, peer: String, remote: Id, tx: SyncSender<Frame>) -> Self {
        Incoming { node, peer, remote, tx, files: HashMap::new(), untold: Vec::new() }
    }

    fn note(&self, f: &InFile, finished: bool, failed: bool, cancelled: bool) {
        let file = f.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let (key, done, total, path) = (f.key, f.done, f.size, f.path.clone());
        (self.node.0.notify)(Note::Transfer { key, peer_id: self.remote, peer: self.peer.clone(), file, path, done, total, incoming: true, finished, failed, cancelled });
    }

    /// Never blocks: this thread reads, and a blocked reader can stall both writers.
    fn tell_cancels(&mut self) {
        while let Some(&id) = self.untold.last() {
            if self.tx.try_send(Frame::FileCancel { id }).is_err() {
                return;
            }
            self.untold.pop();
        }
    }

    fn start(&mut self, id: u32, size: u64, name: String, mtime: Option<u64>, resume: bool) -> io::Result<()> {
        if self.files.len() >= 64 {
            return Err(denied("too many files"));
        }
        self.abort(id, false, false);
        let downloads = self.node.downloads();
        std::fs::create_dir_all(&downloads)?;
        // A part kept from an earlier try under this name: go on with it if it is the same file
        // (size and time) in the same folder and the sender asks, and nothing has taken its name
        // since (finishing renames over it); otherwise it is stale.
        let kept = self.node.st().partials.remove(&(self.remote, name.clone()));
        let mut resumed = None;
        if let Some(k) = kept {
            let part = store::part_of(&k.path);
            let len = std::fs::metadata(&part).map_or(u64::MAX, |m| m.len());
            if resume && mtime == Some(k.mtime) && size == k.size && len <= size && k.path.parent() == Some(&downloads) && !k.path.exists() {
                let mut file = std::fs::OpenOptions::new().write(true).open(&part)?;
                file.seek(SeekFrom::Start(len))?;
                resumed = Some((k.path, file, len));
            } else {
                let _ = std::fs::remove_file(part);
            }
        }
        let (path, file, done) = match resumed {
            Some(r) => r,
            None => {
                let path = store::incoming_path(&downloads, &name);
                let file = File::create(store::part_of(&path))?;
                (path, file, 0)
            }
        };
        if resume {
            // Off this thread: a reader blocked on a full queue can stall both writers.
            let tx = self.tx.clone();
            spawn("aun-from", move || drop(tx.send(Frame::FileFrom { id, offset: done })));
        }
        let out = BufWriter::with_capacity(512 << 10, file);
        let key = {
            let mut st = self.node.st();
            st.next += 1;
            st.next
        };
        let f = InFile { path, name, mtime, out, size, done, key, told: Instant::now() };
        self.note(&f, false, false, false);
        self.files.insert(id, f);
        Ok(())
    }

    fn data(&mut self, id: u32, data: &[u8]) -> io::Result<()> {
        let Some(f) = self.files.get_mut(&id) else { return Ok(()) };
        if self.node.st().cancelled.remove(&f.key) {
            self.abort(id, true, false);
            self.untold.push(id);
            return Ok(());
        }
        if f.done + data.len() as u64 > f.size {
            self.abort(id, false, false);
            return Err(denied("file larger than announced"));
        }
        if let Err(e) = f.out.write_all(data) {
            self.abort(id, false, false);
            (self.node.0.notify)(Note::Info(format!("파일을 저장하지 못했어요: {e}")));
            return Ok(());
        }
        f.done += data.len() as u64;
        if f.told.elapsed() > Duration::from_millis(100) {
            f.told = Instant::now();
            let f = &self.files[&id];
            self.note(f, false, false, false);
        }
        Ok(())
    }

    fn end(&mut self, id: u32) {
        let Some(mut f) = self.files.remove(&id) else { return };
        self.node.st().cancelled.remove(&f.key); // too late to cancel
        let ok = f.done == f.size && f.out.flush().is_ok() && f.out.get_ref().sync_all().is_ok();
        let part = store::part_of(&f.path);
        let ok = ok && std::fs::rename(&part, &f.path).is_ok();
        if !ok {
            let _ = std::fs::remove_file(part);
        }
        self.note(&f, true, !ok, false);
    }

    /// `keep`: leave the `.part` for the sender to go on with, if it said which file it is.
    fn abort(&mut self, id: u32, cancelled: bool, keep: bool) {
        let Some(f) = self.files.remove(&id) else { return };
        self.note(&f, true, true, cancelled);
        let part = store::part_of(&f.path);
        let kept = f.mtime.filter(|_| keep).map(|mtime| Partial { size: f.size, mtime, path: f.path.clone() });
        let name = f.name.clone();
        drop(f); // flushed and closed before it is kept or deleted
        match kept {
            Some(k) => {
                let old = self.node.st().partials.insert((self.remote, name), k);
                if let Some(old) = old {
                    let _ = std::fs::remove_file(store::part_of(&old.path));
                }
            }
            None => drop(std::fs::remove_file(part)),
        }
    }

    /// The link is down: every file under way keeps what arrived.
    fn keep_all(&mut self) {
        let ids: Vec<_> = self.files.keys().copied().collect();
        for id in ids {
            self.abort(id, false, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transports() -> (StatelessTransportState, StatelessTransportState) {
        transports_for(wire::NOISE)
    }

    fn transports_for(noise: &str) -> (StatelessTransportState, StatelessTransportState) {
        let b = || snow::Builder::new(noise.parse().unwrap());
        let (ka, kb) = (b().generate_keypair().unwrap(), b().generate_keypair().unwrap());
        let mut i = b().local_private_key(&ka.private).build_initiator().unwrap();
        let mut r = b().local_private_key(&kb.private).build_responder().unwrap();
        let (mut m, mut out) = ([0u8; 1024], [0u8; 1024]);
        let n = i.write_message(&[], &mut m).unwrap();
        r.read_message(&m[..n], &mut out).unwrap();
        let n = r.write_message(&[], &mut m).unwrap();
        i.read_message(&m[..n], &mut out).unwrap();
        let n = i.write_message(&[], &mut m).unwrap();
        r.read_message(&m[..n], &mut out).unwrap();
        (i.into_stateless_transport_mode().unwrap(), r.into_stateless_transport_mode().unwrap())
    }

    /// `cargo test --release -p aun-core --lib -- --ignored --nocapture seal_open_speed`: the link's
    /// framing and encryption alone, in memory, one direction at a time (no socket, no disk).
    #[test]
    #[ignore]
    fn seal_open_speed() {
        for noise in ["Noise_XX_25519_ChaChaPoly_BLAKE2s", "Noise_XX_25519_AESGCM_BLAKE2s"] {
            seal_open(noise);
        }
    }

    fn seal_open(noise: &str) {
        let (ti, tr) = transports_for(noise);
        let chunk = vec![0x5au8; wire::FILE_CHUNK];
        let n = 256; // 64 MiB
        let mut sealed = Vec::with_capacity(n * (wire::FILE_CHUNK + 1024));
        let mut w = SealWriter::new(Arc::new(ti), &mut sealed);
        let t = Instant::now();
        for _ in 0..n {
            wire::write_frame(&mut w, &Frame::FileData { id: 1, data: chunk.clone() }).unwrap();
        }
        w.flush().unwrap();
        let seal = t.elapsed().as_secs_f64();
        drop(w);
        let mut r = OpenReader::new(Arc::new(tr), &sealed[..]);
        let t = Instant::now();
        for _ in 0..n {
            assert!(matches!(wire::read_frame(&mut r).unwrap(), Frame::FileData { .. }));
        }
        let open = t.elapsed().as_secs_f64();
        let mb = (n * wire::FILE_CHUNK) as f64 / 1e6;
        println!("{noise}: seal {:.0} MB/s, open {:.0} MB/s", mb / seal, mb / open);
    }

    /// A missed wake notice leaves the node refusing links; input at the machine ends that, while
    /// a DarkWake (idle time only growing, or not readable) doesn't.
    #[test]
    fn input_after_a_missed_wake_notice_lets_links_back() {
        let d = std::env::temp_dir().join(format!("aun-unit-idle-{}", std::process::id()));
        let cfg = Config { dir: d.join("cfg"), downloads: d.join("dl"), name: "n".into(), port: 0, discovery: false, clipboard: false, secrets: false };
        let n = Node::start(cfg, |_| {}).unwrap();
        let s = Duration::from_secs;
        n.set_sleeping(true);
        // `None` first: going to sleep took this machine's real idle time as the baseline.
        for idle in [None, Some(s(40)), Some(s(2))] {
            n.input_while_asleep(idle);
            assert!(n.asleep(), "input in the first minute doesn't count: {idle:?}");
        }
        n.0.idle.lock().unwrap().1 -= MISSED_WAKE;
        for idle in [Some(s(45)), Some(s(45)), None, Some(s(3)), Some(s(50))] {
            n.input_while_asleep(idle);
            assert!(n.asleep(), "no input seen yet: {idle:?}");
        }
        n.input_while_asleep(Some(s(2)));
        assert!(!n.asleep(), "input since the last look: awake");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_sleep_shows_as_wall_time_running_ahead() {
        let s = Duration::from_secs;
        assert!(!slept(s(5), s(5)), "awake");
        assert!(!slept(s(12), s(5)), "a scheduling hiccup or small clock step");
        assert!(slept(s(65), s(5)), "a minute asleep");
    }

    #[test]
    fn the_writer_flushes_the_last_frame_of_a_burst() {
        let (ti, tr) = transports();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let out = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (inp, _) = l.accept().unwrap();
        let (tx, rx) = mpsc::sync_channel(4);
        tx.send(Frame::Clip("a".into())).unwrap();
        tx.send(Frame::Clip("b".into())).unwrap();
        let w = SealWriter::new(Arc::new(ti), out.try_clone().unwrap());
        thread::spawn(move || writer(w, rx, out));
        inp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut r = OpenReader::new(Arc::new(tr), inp);
        assert_eq!(wire::read_frame(&mut r).unwrap(), Frame::Clip("a".into()));
        assert_eq!(wire::read_frame(&mut r).expect("second frame within 2 s, not at the next ping"), Frame::Clip("b".into()));
        drop(tx);
    }
}
