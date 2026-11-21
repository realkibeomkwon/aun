//! Aun's engine: discovery, pairing, encrypted links, clipboard and file transfer. No UI.

pub mod clip;
pub mod discovery;
pub mod node;
pub mod power;
pub mod store;
pub mod wire;

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static LOG: OnceLock<PathBuf> = OnceLock::new();

/// Appends a line to `aun.log` in the config folder (set by `Node::start`) and to stderr.
/// The release Windows app has no console, so the file is the only record.
pub fn log(msg: impl std::fmt::Display) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let line = format!("{secs:.3} {msg}\n");
    eprint!("aun: {line}");
    let Some(path) = LOG.get() else { return };
    // Keep it small: past 1 MiB the log moves to aun.log.old.
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

pub(crate) fn log_to(path: PathBuf) {
    let _ = LOG.set(path);
}

pub use node::{Config, Node, Note, PeerView};
pub use store::{Id, hex};
