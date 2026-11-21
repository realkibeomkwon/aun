//! Frames and the encrypted byte stream they travel on.
//!
//! A link is TCP → Noise XX (`Noise_XX_25519_ChaChaPoly_BLAKE2s`) → a byte stream of
//! length-prefixed frames. Noise messages carry at most 64 KiB, so frames are written into
//! [`SealWriter`], which packs them into full Noise messages, and read back through [`OpenReader`].

use std::io::{self, Read, Write};
use std::sync::Arc;

use snow::StatelessTransportState;

/// ChaChaPoly, on NEON on Apple Silicon (`.cargo/config.toml`) and AVX2 on x86. AES-GCM was
/// measured too: 3.5x faster on an M4 Pro, but 10% slower on a Windows x86 runner, and the
/// laptop in the pair is the Windows one (docs/overnight-report.md, task G).
pub const NOISE: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const NOISE_MAX: usize = 65535;
const TAG: usize = 16;
const PLAIN_MAX: usize = NOISE_MAX - TAG;
/// The largest frame a peer may send; bigger ones end the link (trust boundary).
pub const MAX_FRAME: usize = 4 << 20;
/// Clipboard images only: a Retina screenshot as PNG is often past 4 MiB. Checked from the kind
/// byte before the frame is allocated, so other frames keep the small bound.
pub const MAX_IMAGE: usize = 16 << 20;
const CLIP_IMAGE: u8 = 9;
pub const FILE_CHUNK: usize = 256 << 10;
/// The `Kvm*` frames this build speaks, sent in `Hello`. Peers use them only when both say the
/// same version (0, as an older build reads: none); a change to their layout bumps it.
pub const KVM: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// `resume`: this build answers `FileStart { resume: true }` with `FileFrom`. Older builds
    /// send no such byte and never get asked. `kvm`: the [`KVM`] version, 0 from older builds.
    Hello { pair: bool, name: String, resume: bool, kvm: u8 },
    Spake(Vec<u8>),
    Confirm(Vec<u8>),
    Ping,
    Clip(String),
    /// `mtime` (ns since the epoch, absent from older builds) tells a resumable copy of the same
    /// file apart from a changed one. `resume` asks where to go on from: send nothing until `FileFrom`.
    FileStart { id: u32, size: u64, name: String, mtime: Option<u64>, resume: bool },
    FileData { id: u32, data: Vec<u8> },
    FileEnd { id: u32 },
    /// The sender stopped: `cancelled` when the user called it off, not when reading failed.
    FileAbort { id: u32, cancelled: bool },
    /// A PNG.
    ClipImage(Vec<u8>),
    /// The receiver doesn't want file `id` (the sender's id) any more: stop sending it.
    FileCancel { id: u32 },
    /// The answer to `FileStart { resume: true }`: send file `id` (the sender's id) from `offset`.
    FileFrom { id: u32, offset: u64 },
    /// The pointer moved, in pixels, `(cx, cy)` in total since `route` began: one lost or late
    /// move doesn't lose distance. `mn` counts moves in the route; an older one is dropped.
    KvmMove { route: u32, mn: u32, cx: i32, cy: i32 },
    /// Button 0 left, 1 right, 2 middle. `clicks`: which click of a run (2 for a double click).
    KvmButton { button: u8, down: bool, clicks: u8 },
    /// Scroll by pixels, positive moving the content right and down.
    KvmWheel { dx: i16, dy: i16 },
    /// The driver let go: release whatever it holds down.
    KvmLeave,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    let s = &s.as_bytes()[..s.len().min(u16::MAX as usize)];
    b.extend_from_slice(&(s.len() as u16).to_be_bytes());
    b.extend_from_slice(s);
}

pub fn write_frame(w: &mut impl Write, f: &Frame) -> io::Result<()> {
    if let Frame::FileData { id, data } = f {
        // Header then payload straight from the caller's buffer: no copy of file bytes.
        let len = (5 + data.len()) as u32;
        let mut head = [0u8; 9];
        head[..4].copy_from_slice(&len.to_be_bytes());
        head[4] = 6;
        head[5..].copy_from_slice(&id.to_be_bytes());
        w.write_all(&head)?;
        return w.write_all(data);
    }
    if let Frame::ClipImage(png) = f {
        w.write_all(&((1 + png.len()) as u32).to_be_bytes())?;
        w.write_all(&[CLIP_IMAGE])?;
        return w.write_all(png);
    }
    let mut b = Vec::with_capacity(64);
    match f {
        Frame::Hello { pair, name, resume, kvm } => {
            b.push(0);
            b.push(*pair as u8);
            put_str(&mut b, name);
            b.push(*resume as u8);
            b.push(*kvm);
        }
        Frame::Spake(m) => (b.push(1), b.extend_from_slice(m)).1,
        Frame::Confirm(m) => (b.push(2), b.extend_from_slice(m)).1,
        Frame::Ping => b.push(3),
        Frame::Clip(t) => (b.push(4), b.extend_from_slice(t.as_bytes())).1,
        Frame::FileStart { id, size, name, mtime, resume } => {
            b.push(5);
            b.extend_from_slice(&id.to_be_bytes());
            b.extend_from_slice(&size.to_be_bytes());
            put_str(&mut b, name);
            // Past the name, where older builds stop reading.
            if let Some(m) = mtime {
                b.extend_from_slice(&m.to_be_bytes());
                b.push(*resume as u8);
            }
        }
        Frame::FileEnd { id } => (b.push(7), b.extend_from_slice(&id.to_be_bytes())).1,
        Frame::FileAbort { id, cancelled } => (b.push(8), b.extend_from_slice(&id.to_be_bytes()), b.push(*cancelled as u8)).2,
        Frame::FileCancel { id } => (b.push(10), b.extend_from_slice(&id.to_be_bytes())).1,
        Frame::FileFrom { id, offset } => (b.push(11), b.extend_from_slice(&id.to_be_bytes()), b.extend_from_slice(&offset.to_be_bytes())).2,
        Frame::KvmMove { route, mn, cx, cy } => {
            b.push(12);
            for v in [route.to_be_bytes(), mn.to_be_bytes(), cx.to_be_bytes(), cy.to_be_bytes()] {
                b.extend_from_slice(&v);
            }
        }
        Frame::KvmButton { button, down, clicks } => b.extend_from_slice(&[13, *button, *down as u8, *clicks]),
        Frame::KvmWheel { dx, dy } => (b.push(14), b.extend_from_slice(&dx.to_be_bytes()), b.extend_from_slice(&dy.to_be_bytes())).2,
        Frame::KvmLeave => b.push(15),
        Frame::FileData { .. } | Frame::ClipImage(_) => unreachable!(),
    }
    w.write_all(&(b.len() as u32).to_be_bytes())?;
    w.write_all(&b)
}

pub fn read_frame(r: &mut impl Read) -> io::Result<Frame> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 {
        return Err(bad("frame size"));
    }
    let mut kind = [0u8];
    r.read_exact(&mut kind)?;
    if len > if kind[0] == CLIP_IMAGE { MAX_IMAGE } else { MAX_FRAME } {
        return Err(bad("frame size"));
    }
    let mut b = vec![0u8; len];
    b[0] = kind[0];
    r.read_exact(&mut b[1..])?;
    let u32_at = |b: &[u8], i: usize| b.get(i..i + 4).map(|s| u32::from_be_bytes(s.try_into().unwrap())).ok_or_else(|| bad("short"));
    let str_at = |b: &[u8], i: usize| -> io::Result<String> {
        let n = b.get(i..i + 2).map(|s| u16::from_be_bytes([s[0], s[1]]) as usize).ok_or_else(|| bad("short"))?;
        let s = b.get(i + 2..i + 2 + n).ok_or_else(|| bad("short"))?;
        String::from_utf8(s.to_vec()).map_err(|_| bad("utf8"))
    };
    Ok(match b[0] {
        0 => {
            let name = str_at(&b, 2)?;
            Frame::Hello { pair: b.get(1) == Some(&1), resume: b.get(4 + name.len()) == Some(&1), kvm: b.get(5 + name.len()).copied().unwrap_or(0), name }
        }
        1 => Frame::Spake(b.split_off(1)),
        2 => Frame::Confirm(b.split_off(1)),
        3 => Frame::Ping,
        4 => Frame::Clip(String::from_utf8(b.split_off(1)).map_err(|_| bad("utf8"))?),
        5 => {
            let size = b.get(5..13).map(|s| u64::from_be_bytes(s.try_into().unwrap())).ok_or_else(|| bad("short"))?;
            let name = str_at(&b, 13)?;
            let rest = &b[15 + name.len()..];
            let mtime = rest.get(..8).map(|s| u64::from_be_bytes(s.try_into().unwrap()));
            Frame::FileStart { id: u32_at(&b, 1)?, size, mtime, resume: mtime.is_some() && rest.get(8) == Some(&1), name }
        }
        6 => {
            let id = u32_at(&b, 1)?;
            b.drain(..5);
            Frame::FileData { id, data: b }
        }
        7 => Frame::FileEnd { id: u32_at(&b, 1)? },
        // A build before the cancel byte sends none: read as a failure, as it was shown then.
        8 => Frame::FileAbort { id: u32_at(&b, 1)?, cancelled: b.get(5) == Some(&1) },
        10 => Frame::FileCancel { id: u32_at(&b, 1)? },
        11 => Frame::FileFrom { id: u32_at(&b, 1)?, offset: b.get(5..13).map(|s| u64::from_be_bytes(s.try_into().unwrap())).ok_or_else(|| bad("short"))? },
        12 => {
            let i32_at = |i| u32_at(&b, i).map(|v| v as i32);
            Frame::KvmMove { route: u32_at(&b, 1)?, mn: u32_at(&b, 5)?, cx: i32_at(9)?, cy: i32_at(13)? }
        }
        13 => match b.get(1..4) {
            Some(&[button, down, clicks]) => Frame::KvmButton { button, down: down == 1, clicks },
            _ => return Err(bad("short")),
        },
        14 => {
            let i16_at = |i: usize| b.get(i..i + 2).map(|s| i16::from_be_bytes([s[0], s[1]])).ok_or_else(|| bad("short"));
            Frame::KvmWheel { dx: i16_at(1)?, dy: i16_at(3)? }
        }
        15 => Frame::KvmLeave,
        CLIP_IMAGE => {
            b.remove(0);
            Frame::ClipImage(b)
        }
        _ => return Err(bad("frame kind")),
    })
}

/// Packs written bytes into Noise messages: `[u16 len][ciphertext]`, one write per message.
pub struct SealWriter<W> {
    t: Arc<StatelessTransportState>,
    w: W,
    nonce: u64,
    buf: Vec<u8>,
    out: Vec<u8>,
}

impl<W: Write> SealWriter<W> {
    pub fn new(t: Arc<StatelessTransportState>, w: W) -> Self {
        SealWriter { t, w, nonce: 0, buf: Vec::with_capacity(PLAIN_MAX), out: vec![0; NOISE_MAX + 2] }
    }

    fn seal(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let n = self.t.write_message(self.nonce, &self.buf, &mut self.out[2..]).map_err(|e| bad(&e.to_string()))?;
        self.nonce += 1;
        self.out[..2].copy_from_slice(&(n as u16).to_be_bytes());
        self.buf.clear();
        self.w.write_all(&self.out[..n + 2])
    }
}

impl<W: Write> Write for SealWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = data.len().min(PLAIN_MAX - self.buf.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() == PLAIN_MAX {
            self.seal()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.seal()?;
        self.w.flush()
    }
}

pub struct OpenReader<R> {
    t: Arc<StatelessTransportState>,
    r: R,
    nonce: u64,
    cipher: Vec<u8>,
    plain: Vec<u8>,
    pos: usize,
    end: usize,
}

impl<R: Read> OpenReader<R> {
    pub fn new(t: Arc<StatelessTransportState>, r: R) -> Self {
        OpenReader { t, r, nonce: 0, cipher: vec![0; NOISE_MAX], plain: vec![0; NOISE_MAX], pos: 0, end: 0 }
    }
}

impl<R: Read> Read for OpenReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.end {
            let mut len = [0u8; 2];
            self.r.read_exact(&mut len)?;
            let len = u16::from_be_bytes(len) as usize;
            self.r.read_exact(&mut self.cipher[..len])?;
            self.end = self.t.read_message(self.nonce, &self.cipher[..len], &mut self.plain).map_err(|_| bad("decrypt"))?;
            self.nonce += 1;
            self.pos = 0;
        }
        let n = out.len().min(self.end - self.pos);
        out[..n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Handshake messages go on the raw socket with the same `[u16 len]` prefix.
pub fn send_raw(w: &mut impl Write, m: &[u8]) -> io::Result<()> {
    w.write_all(&(m.len() as u16).to_be_bytes())?;
    w.write_all(m)
}

pub fn recv_raw(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len)?;
    let len = u16::from_be_bytes(len) as usize;
    r.read_exact(&mut buf[..len])?;
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(f: Frame) {
        let mut b = Vec::new();
        write_frame(&mut b, &f).unwrap();
        assert_eq!(read_frame(&mut &b[..]).unwrap(), f);
    }

    #[test]
    fn every_frame_round_trips() {
        round(Frame::Hello { pair: true, name: "맥북".into(), resume: true, kvm: KVM });
        round(Frame::Hello { pair: false, name: "맥북".into(), resume: false, kvm: 0 });
        round(Frame::Spake(vec![1, 2, 3]));
        round(Frame::Confirm(vec![9; 32]));
        round(Frame::Ping);
        round(Frame::Clip("안녕 clipboard".into()));
        round(Frame::FileStart { id: 7, size: 1 << 40, name: "a b.txt".into(), mtime: Some(1 << 60), resume: true });
        round(Frame::FileStart { id: 7, size: 1 << 40, name: "a b.txt".into(), mtime: Some(0), resume: false });
        round(Frame::FileStart { id: 7, size: 1 << 40, name: "a b.txt".into(), mtime: None, resume: false });
        round(Frame::FileFrom { id: 7, offset: 1 << 33 });
        round(Frame::FileData { id: 7, data: vec![5; 1000] });
        round(Frame::FileEnd { id: 7 });
        round(Frame::FileAbort { id: 7, cancelled: false });
        round(Frame::FileAbort { id: 7, cancelled: true });
        round(Frame::ClipImage(vec![0x89, b'P', b'N', b'G']));
        round(Frame::FileCancel { id: 7 });
        round(Frame::KvmMove { route: 3, mn: u32::MAX, cx: -5000, cy: i32::MAX });
        round(Frame::KvmButton { button: 1, down: true, clicks: 2 });
        round(Frame::KvmButton { button: 0, down: false, clicks: 1 });
        round(Frame::KvmWheel { dx: -3, dy: i16::MAX });
        round(Frame::KvmLeave);
    }

    /// A build from before the cancel byte still talks to this one: its abort reads as a failure.
    #[test]
    fn an_abort_from_an_older_build_reads_as_a_failure() {
        let old = [&5u32.to_be_bytes()[..], &[8], &7u32.to_be_bytes()].concat();
        assert_eq!(read_frame(&mut &old[..]).unwrap(), Frame::FileAbort { id: 7, cancelled: false });
    }

    /// An older build ends both frames at the name: it can't be asked to resume.
    #[test]
    fn hello_and_file_start_from_an_older_build_offer_no_resume() {
        let hello = [&5u32.to_be_bytes()[..], &[0, 1], &1u16.to_be_bytes(), b"m"].concat();
        assert_eq!(read_frame(&mut &hello[..]).unwrap(), Frame::Hello { pair: true, name: "m".into(), resume: false, kvm: 0 });
        // The build before KVM: it offers resume, and no input frames.
        let hello = [&6u32.to_be_bytes()[..], &[0, 1], &1u16.to_be_bytes(), b"m", &[1]].concat();
        assert_eq!(read_frame(&mut &hello[..]).unwrap(), Frame::Hello { pair: true, name: "m".into(), resume: true, kvm: 0 });
        let start = [&16u32.to_be_bytes()[..], &[5], &7u32.to_be_bytes(), &9u64.to_be_bytes(), &1u16.to_be_bytes(), b"f"].concat();
        assert_eq!(read_frame(&mut &start[..]).unwrap(), Frame::FileStart { id: 7, size: 9, name: "f".into(), mtime: None, resume: false });
    }

    #[test]
    fn only_images_may_pass_the_frame_bound_and_not_by_much() {
        let head = |len: usize, kind: u8| [&(len as u32).to_be_bytes()[..], &[kind]].concat();
        let refused = |b: Vec<u8>| matches!(read_frame(&mut &b[..]), Err(e) if e.kind() == io::ErrorKind::InvalidData);
        // Refused from the header alone, before the body is read or allocated.
        assert!(refused(head(MAX_FRAME + 1, 4)), "a clip text past 4 MiB");
        assert!(refused(head(MAX_IMAGE + 1, CLIP_IMAGE)), "an image past 16 MiB");
        let big = vec![7u8; MAX_FRAME + 10];
        let mut b = Vec::new();
        write_frame(&mut b, &Frame::ClipImage(big.clone())).unwrap();
        assert_eq!(read_frame(&mut &b[..]).unwrap(), Frame::ClipImage(big), "an image past 4 MiB is fine");
    }

    /// The Mac (NEON) and the PC (AVX2) must produce the same bytes: RFC 8439 §2.8.2 (from the
    /// chacha20poly1305 crate's tests), then snow's cipher against it at the nonce layout Noise
    /// uses for ChaChaPoly (4 zero bytes, then the counter little-endian).
    #[test]
    fn this_machine_s_chacha_poly_matches_the_standard() {
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        use snow::params::CipherChoice;
        use snow::resolvers::{CryptoResolver, DefaultResolver};
        let h = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
        let key: Vec<u8> = (0x80..=0x9f).collect();
        let aad = h("50515253c0c1c2c3c4c5c6c7");
        let msg = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let c = chacha20poly1305::ChaCha20Poly1305::new_from_slice(&key).unwrap();
        let out = c.encrypt(h("070000004041424344454647")[..].into(), Payload { msg, aad: &aad }).unwrap();
        let want = "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b6116";
        assert_eq!(out, [h(want), h("1ae10b594f09e26a7e902ecbd0600691")].concat());

        assert!(NOISE.contains("ChaChaPoly"));
        let mut snow_c = DefaultResolver.resolve_cipher(&CipherChoice::ChaChaPoly).unwrap();
        snow_c.set(&key);
        let mut sealed = vec![0u8; msg.len() + TAG];
        let n = snow_c.encrypt(7, &aad, msg, &mut sealed);
        let iv = [&[0u8; 4][..], &7u64.to_le_bytes()].concat();
        assert_eq!(&sealed[..n], c.encrypt(iv[..].into(), Payload { msg, aad: &aad }).unwrap());
    }

    #[test]
    fn hostile_frames_are_rejected_not_panicked() {
        for b in [&[0, 0, 0, 0][..], &[0xff, 0xff, 0xff, 0xff], &[0, 0, 0, 1, 99], &[0, 0, 0, 3, 5, 0, 0], &[0, 0, 0, 2, 4, 0xff], &[0, 0, 0, 5, 11, 0, 0, 0, 7], &[0, 0, 0, 3, 13, 0, 1], &[0, 0, 0, 9, 12, 0, 0, 0, 1, 0, 0, 0, 1]] {
            assert!(read_frame(&mut &b[..]).is_err(), "{b:?}");
        }
    }
}
