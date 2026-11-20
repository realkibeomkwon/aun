//! A device on the desk: listens, dials paired peers, pairs with a 6-digit code, and moves
//! clipboard text and files over one encrypted link per peer.
//!
//! Pairing: Noise XX gives both sides each other's static key and a handshake hash `h`. SPAKE2
//! with the code then yields a key only the two code holders share, and each side proves it with
//! HMAC(key, role ‖ h). A man in the middle has a different `h` on each leg and can't learn the
//! code offline, so it fails the check. After that the peer's static key is pinned.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

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

pub struct Config {
    pub dir: PathBuf,
    pub downloads: PathBuf,
    pub name: String,
    /// 0 picks a free port. Discovery off is for tests and two nodes on one machine.
    pub port: u16,
    pub discovery: bool,
    pub clipboard: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    /// Peers, links or the pairing code changed: read `peers()` / `code()` again.
    Changed,
    Info(String),
    Transfer { key: u64, peer: String, file: String, done: u64, total: u64, incoming: bool, finished: bool, failed: bool },
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
}

#[derive(Default)]
struct Peer {
    name: String,
    addr: Option<SocketAddr>,
    nearby: bool,
    paired: bool,
    link: Option<Link>,
}

struct Code {
    digits: String,
    misses: u32,
}

#[derive(Default)]
struct St {
    peers: HashMap<Id, Peer>,
    code: Option<Code>,
    dialing: HashSet<Id>,
    next: u64,
}

struct Shared {
    key: Vec<u8>,
    id: Id,
    name: String,
    port: u16,
    dir: PathBuf,
    downloads: PathBuf,
    clip_on: AtomicBool,
    clip_tx: Mutex<Option<Sender<String>>>,
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
        let me = store::identity(&cfg.dir)?;
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, cfg.port)).or_else(|_| TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)))?;
        let port = listener.local_addr()?.port();
        let mut st = St::default();
        for (id, name) in store::load_peers(&cfg.dir) {
            st.peers.insert(id, Peer { name, paired: true, ..Default::default() });
        }
        let node = Node(Arc::new(Shared {
            key: me.private,
            id: me.id,
            name: store::clean_name(&cfg.name),
            port,
            dir: cfg.dir,
            downloads: cfg.downloads,
            clip_on: AtomicBool::new(cfg.clipboard),
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

        let (tx, rx) = mpsc::channel();
        *node.0.clip_tx.lock().unwrap() = Some(tx);
        let n = node.clone();
        spawn("aun-clip", move || clip::run(rx, move |text| n.broadcast_clip(text)));
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

    pub fn clipboard(&self) -> bool {
        self.0.clip_on.load(Ordering::Relaxed)
    }
    pub fn set_clipboard(&self, on: bool) {
        self.0.clip_on.store(on, Ordering::Relaxed);
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
        let paired = st.peers.iter().filter(|(_, p)| p.paired).map(|(id, p)| (id, &p.name));
        if let Err(e) = store::save_peers(&self.0.dir, paired) {
            (self.0.notify)(Note::Info(format!("페어링 정보를 저장하지 못했어요: {e}")));
        }
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
            let n = self.clone();
            spawn("aun-dial", move || {
                if let Ok(sock) = TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
                    n.clone().run_link(sock, true, None);
                }
                n.st().dialing.remove(&id);
            });
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

    fn broadcast_clip(&self, text: String) {
        if !self.clipboard() {
            return;
        }
        let txs: Vec<_> = self.st().peers.values().filter_map(|p| p.link.as_ref().map(|l| l.tx.clone())).collect();
        for tx in txs {
            let _ = tx.try_send(Frame::Clip(text.clone()));
        }
    }

    pub fn send_file(&self, id: Id, path: PathBuf) {
        let n = self.clone();
        spawn("aun-send", move || {
            let (tx, peer) = {
                let mut st = n.st();
                st.next += 1;
                let key = st.next;
                match st.peers.get(&id) {
                    Some(Peer { link: Some(l), name, .. }) => ((l.tx.clone(), key), name.clone()),
                    _ => return (n.0.notify)(Note::Info("연결된 기기가 아니에요".into())),
                }
            };
            if let Err(e) = n.stream_file(tx.0, tx.1, &peer, &path) {
                (n.0.notify)(Note::Info(format!("{} 전송 실패: {e}", path.display())));
            }
        });
    }

    fn stream_file(&self, tx: SyncSender<Frame>, key: u64, peer: &str, path: &Path) -> io::Result<()> {
        let mut f = File::open(path)?;
        let total = f.metadata()?.len();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file").to_owned();
        let id = key as u32;
        let gone = || io::Error::new(io::ErrorKind::BrokenPipe, "연결이 끊겼어요");
        let note = |done, finished, failed| {
            (self.0.notify)(Note::Transfer { key, peer: peer.into(), file: name.clone(), done, total, incoming: false, finished, failed })
        };
        tx.send(Frame::FileStart { id, size: total, name: name.clone() }).map_err(|_| gone())?;
        let (mut done, mut told) = (0u64, Instant::now());
        loop {
            let mut buf = vec![0u8; wire::FILE_CHUNK];
            let n = match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = tx.send(Frame::FileAbort { id });
                    note(done, true, true);
                    return Err(e);
                }
            };
            buf.truncate(n);
            if tx.send(Frame::FileData { id, data: buf }).is_err() {
                note(done, true, true);
                return Err(gone());
            }
            done += n as u64;
            if told.elapsed() > Duration::from_millis(100) {
                told = Instant::now();
                note(done, false, false);
            }
        }
        tx.send(Frame::FileEnd { id }).map_err(|_| gone())?;
        note(done, true, done != total);
        Ok(())
    }

    /// One link, from handshake to close. `pair` is the code when this side dials to pair.
    fn run_link(self, sock: TcpStream, initiator: bool, pair: Option<String>) {
        let peer_addr = sock.peer_addr().ok();
        if let Err(e) = self.link(sock, initiator, pair.clone()) {
            if pair.is_some() {
                (self.0.notify)(Note::Info(format!("페어링 실패: {e}")));
            } else if e.kind() != io::ErrorKind::UnexpectedEof && e.kind() != io::ErrorKind::ConnectionReset {
                eprintln!("aun: link {peer_addr:?}: {e}");
            }
        }
    }

    fn link(&self, sock: TcpStream, initiator: bool, pair: Option<String>) -> io::Result<()> {
        sock.set_nodelay(true)?;
        sock.set_read_timeout(Some(Duration::from_secs(10)))?;
        let (t, h, remote) = handshake(&sock, &self.0.key, initiator)?;
        let t = Arc::new(t);
        let mut w = SealWriter::new(t.clone(), sock.try_clone()?);
        let mut r = OpenReader::new(t, BufReader::with_capacity(128 << 10, sock.try_clone()?));

        wire::write_frame(&mut w, &Frame::Hello { pair: pair.is_some(), name: self.0.name.clone() })?;
        w.flush()?;
        let Frame::Hello { pair: they_pair, name } = wire::read_frame(&mut r)? else { return Err(denied("hello")) };
        let name = store::clean_name(&name);

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
            self.paired(remote, &name);
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
            self.paired(remote, &name);
        } else if !self.st().peers.get(&remote).is_some_and(|p| p.paired) {
            return Err(denied("not paired"));
        }

        // Authenticated. Register, then run the writer on its own thread and read here.
        sock.set_read_timeout(Some(DEAD))?;
        let (tx, rx) = mpsc::sync_channel::<Frame>(4);
        let conn = {
            let mut st = self.st();
            st.next += 1;
            let conn = st.next;
            let initiator_id = if initiator { self.0.id } else { remote };
            let p = st.peers.entry(remote).or_default();
            p.name = name.clone();
            if let Some(old) = &p.link {
                // Both sides dialled: both keep the link whose dialler has the smaller id. The same
                // dialler again means it lost the old link (restart, sleep): the new one wins.
                if old.initiator < initiator_id {
                    return Err(denied("duplicate"));
                }
                let _ = old.sock.shutdown(std::net::Shutdown::Both);
            }
            p.link = Some(Link { conn, initiator: initiator_id, tx: tx.clone(), sock: sock.try_clone()? });
            conn
        };
        (self.0.notify)(Note::Changed);
        let wsock = sock.try_clone()?;
        spawn("aun-write", move || writer(w, rx, wsock));

        let mut rx_files = Incoming::new(self, name.clone());
        let res = self.read_loop(&mut r, &mut rx_files);
        rx_files.abort_all();
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
        res
    }

    fn read_loop(&self, r: &mut impl Read, files: &mut Incoming) -> io::Result<()> {
        loop {
            match wire::read_frame(r)? {
                Frame::Ping => {}
                Frame::Clip(text) => {
                    if self.clipboard()
                        && text.len() <= clip::MAX_TEXT
                        && let Some(tx) = self.0.clip_tx.lock().unwrap().as_ref()
                    {
                        let _ = tx.send(text);
                    }
                }
                Frame::FileStart { id, size, name } => files.start(id, size, &name)?,
                Frame::FileData { id, data } => files.data(id, &data)?,
                Frame::FileEnd { id } => files.end(id),
                Frame::FileAbort { id } => files.abort(id),
                _ => return Err(denied("unexpected frame")),
            }
        }
    }

    fn paired(&self, id: Id, name: &str) {
        let mut st = self.st();
        let p = st.peers.entry(id).or_default();
        p.paired = true;
        p.name = name.to_owned();
        self.save(&st);
        drop(st);
        (self.0.notify)(Note::Info(format!("{name}와(과) 페어링했어요")));
    }
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
    if res.is_err() {
        let _ = sock.shutdown(std::net::Shutdown::Both);
    }
}

struct InFile {
    path: PathBuf,
    out: BufWriter<File>,
    size: u64,
    done: u64,
    key: u64,
    told: Instant,
}

struct Incoming<'a> {
    node: &'a Node,
    peer: String,
    files: HashMap<u32, InFile>,
}

impl<'a> Incoming<'a> {
    fn new(node: &'a Node, peer: String) -> Self {
        Incoming { node, peer, files: HashMap::new() }
    }

    fn note(&self, f: &InFile, finished: bool, failed: bool) {
        let file = f.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let (key, done, total) = (f.key, f.done, f.size);
        (self.node.0.notify)(Note::Transfer { key, peer: self.peer.clone(), file, done, total, incoming: true, finished, failed });
    }

    fn start(&mut self, id: u32, size: u64, name: &str) -> io::Result<()> {
        if self.files.len() >= 64 {
            return Err(denied("too many files"));
        }
        self.abort(id);
        std::fs::create_dir_all(&self.node.0.downloads)?;
        let path = store::incoming_path(&self.node.0.downloads, name);
        let out = BufWriter::with_capacity(512 << 10, File::create(store::part_of(&path))?);
        let key = {
            let mut st = self.node.st();
            st.next += 1;
            st.next
        };
        let f = InFile { path, out, size, done: 0, key, told: Instant::now() };
        self.note(&f, false, false);
        self.files.insert(id, f);
        Ok(())
    }

    fn data(&mut self, id: u32, data: &[u8]) -> io::Result<()> {
        let Some(f) = self.files.get_mut(&id) else { return Ok(()) };
        if f.done + data.len() as u64 > f.size {
            self.abort(id);
            return Err(denied("file larger than announced"));
        }
        if let Err(e) = f.out.write_all(data) {
            self.abort(id);
            (self.node.0.notify)(Note::Info(format!("파일을 저장하지 못했어요: {e}")));
            return Ok(());
        }
        f.done += data.len() as u64;
        if f.told.elapsed() > Duration::from_millis(100) {
            f.told = Instant::now();
            let f = &self.files[&id];
            self.note(f, false, false);
        }
        Ok(())
    }

    fn end(&mut self, id: u32) {
        let Some(mut f) = self.files.remove(&id) else { return };
        let ok = f.done == f.size && f.out.flush().is_ok() && f.out.get_ref().sync_all().is_ok();
        let part = store::part_of(&f.path);
        let ok = ok && std::fs::rename(&part, &f.path).is_ok();
        if !ok {
            let _ = std::fs::remove_file(part);
        }
        self.note(&f, true, !ok);
    }

    fn abort(&mut self, id: u32) {
        if let Some(mut f) = self.files.remove(&id) {
            let _ = f.out.flush();
            let _ = std::fs::remove_file(store::part_of(&f.path));
            self.note(&f, true, true);
        }
    }

    fn abort_all(&mut self) {
        let ids: Vec<_> = self.files.keys().copied().collect();
        for id in ids {
            self.abort(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transports() -> (StatelessTransportState, StatelessTransportState) {
        let b = || snow::Builder::new(wire::NOISE.parse().unwrap());
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
