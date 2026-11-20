//! Frames and the encrypted byte stream they travel on.
//!
//! A link is TCP → Noise XX (`Noise_XX_25519_ChaChaPoly_BLAKE2s`) → a byte stream of
//! length-prefixed frames. Noise messages carry at most 64 KiB, so frames are written into
//! [`SealWriter`], which packs them into full Noise messages, and read back through [`OpenReader`].

use std::io::{self, Read, Write};
use std::sync::Arc;

use snow::StatelessTransportState;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Hello { pair: bool, name: String },
    Spake(Vec<u8>),
    Confirm(Vec<u8>),
    Ping,
    Clip(String),
    FileStart { id: u32, size: u64, name: String },
    FileData { id: u32, data: Vec<u8> },
    FileEnd { id: u32 },
    FileAbort { id: u32 },
    /// A PNG.
    ClipImage(Vec<u8>),
    /// The receiver doesn't want file `id` (the sender's id) any more: stop sending it.
    FileCancel { id: u32 },
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
        Frame::Hello { pair, name } => {
            b.push(0);
            b.push(*pair as u8);
            put_str(&mut b, name);
        }
        Frame::Spake(m) => (b.push(1), b.extend_from_slice(m)).1,
        Frame::Confirm(m) => (b.push(2), b.extend_from_slice(m)).1,
        Frame::Ping => b.push(3),
        Frame::Clip(t) => (b.push(4), b.extend_from_slice(t.as_bytes())).1,
        Frame::FileStart { id, size, name } => {
            b.push(5);
            b.extend_from_slice(&id.to_be_bytes());
            b.extend_from_slice(&size.to_be_bytes());
            put_str(&mut b, name);
        }
        Frame::FileEnd { id } => (b.push(7), b.extend_from_slice(&id.to_be_bytes())).1,
        Frame::FileAbort { id } => (b.push(8), b.extend_from_slice(&id.to_be_bytes())).1,
        Frame::FileCancel { id } => (b.push(10), b.extend_from_slice(&id.to_be_bytes())).1,
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
        0 => Frame::Hello { pair: b.get(1) == Some(&1), name: str_at(&b, 2)? },
        1 => Frame::Spake(b.split_off(1)),
        2 => Frame::Confirm(b.split_off(1)),
        3 => Frame::Ping,
        4 => Frame::Clip(String::from_utf8(b.split_off(1)).map_err(|_| bad("utf8"))?),
        5 => {
            let size = b.get(5..13).map(|s| u64::from_be_bytes(s.try_into().unwrap())).ok_or_else(|| bad("short"))?;
            Frame::FileStart { id: u32_at(&b, 1)?, size, name: str_at(&b, 13)? }
        }
        6 => {
            let id = u32_at(&b, 1)?;
            b.drain(..5);
            Frame::FileData { id, data: b }
        }
        7 => Frame::FileEnd { id: u32_at(&b, 1)? },
        8 => Frame::FileAbort { id: u32_at(&b, 1)? },
        10 => Frame::FileCancel { id: u32_at(&b, 1)? },
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
        round(Frame::Hello { pair: true, name: "맥북".into() });
        round(Frame::Spake(vec![1, 2, 3]));
        round(Frame::Confirm(vec![9; 32]));
        round(Frame::Ping);
        round(Frame::Clip("안녕 clipboard".into()));
        round(Frame::FileStart { id: 7, size: 1 << 40, name: "a b.txt".into() });
        round(Frame::FileData { id: 7, data: vec![5; 1000] });
        round(Frame::FileEnd { id: 7 });
        round(Frame::FileAbort { id: 7 });
        round(Frame::ClipImage(vec![0x89, b'P', b'N', b'G']));
        round(Frame::FileCancel { id: 7 });
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

    #[test]
    fn hostile_frames_are_rejected_not_panicked() {
        for b in [&[0, 0, 0, 0][..], &[0xff, 0xff, 0xff, 0xff], &[0, 0, 0, 1, 99], &[0, 0, 0, 3, 5, 0, 0], &[0, 0, 0, 2, 4, 0xff]] {
            assert!(read_frame(&mut &b[..]).is_err(), "{b:?}");
        }
    }
}
