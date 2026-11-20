//! What survives a restart: this device's key and the devices it is paired with.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub type Id = [u8; 32];

pub struct Identity {
    pub private: Vec<u8>,
    pub id: Id,
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Id> {
    let mut id = [0u8; 32];
    if s.len() != 64 {
        return None;
    }
    for (i, out) in id.iter_mut().enumerate() {
        *out = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(id)
}

pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(tmp, path)
}

pub fn identity(dir: &Path) -> io::Result<Identity> {
    let path = dir.join("identity.key");
    if let Ok(b) = fs::read(&path)
        && b.len() == 64
    {
        return Ok(Identity { private: b[..32].to_vec(), id: b[32..].try_into().unwrap() });
    }
    let kp = snow::Builder::new(crate::wire::NOISE.parse().unwrap()).generate_keypair().map_err(io::Error::other)?;
    write_atomic(&path, &[kp.private.as_slice(), kp.public.as_slice()].concat())?;
    Ok(Identity { private: kp.private, id: kp.public.try_into().unwrap() })
}

/// A paired device: its name, and the address the user typed for it, if discovery can't find it.
pub type Saved = (String, Option<SocketAddr>);

/// `peers.txt`: `id \t name [\t address]` per line.
pub fn load_peers(dir: &Path) -> HashMap<Id, Saved> {
    let text = fs::read_to_string(dir.join("peers.txt")).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let id = unhex(f.next()?)?;
            Some((id, (f.next()?.to_owned(), f.next().and_then(|a| a.parse().ok()))))
        })
        .collect()
}

pub fn save_peers<'a>(dir: &Path, peers: impl Iterator<Item = (&'a Id, &'a str, Option<SocketAddr>)>) -> io::Result<()> {
    let text: String = peers
        .map(|(id, name, addr)| match addr {
            Some(a) => format!("{}\t{}\t{a}\n", hex(id), clean_name(name)),
            None => format!("{}\t{}\n", hex(id), clean_name(name)),
        })
        .collect();
    write_atomic(&dir.join("peers.txt"), text.as_bytes())
}

/// A device name from the network: one line, at most 64 characters.
pub fn clean_name(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).take(64).collect();
    if s.trim().is_empty() { "Unknown".into() } else { s }
}

/// A file name from the network, reduced to one safe component, made unique inside `dir`.
pub fn incoming_path(dir: &Path, name: &str) -> PathBuf {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut clean: String = base.chars().map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c }).take(200).collect();
    clean = clean.trim_matches([' ', '.']).to_owned();
    // Windows reserves these names in every directory.
    let stem = clean.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
    if clean.is_empty() || reserved {
        clean = format!("file_{clean}");
    }
    let (stem, ext) = match clean.rfind('.') {
        Some(i) if i > 0 => (&clean[..i], &clean[i..]),
        _ => (clean.as_str(), ""),
    };
    let mut path = dir.join(&clean);
    let mut n = 1;
    while path.exists() || part_of(&path).exists() {
        path = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    path
}

pub fn part_of(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_file_names_stay_inside_the_folder() {
        let dir = std::env::temp_dir().join(format!("aun-names-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for (input, want) in [
            ("../../etc/passwd", "passwd"),
            ("..\\..\\Windows\\win.ini", "win.ini"),
            ("..", "file_"),
            ("", "file_"),
            ("CON.txt", "file_CON.txt"),
            ("com1", "file_com1"),
            ("a:b?.txt", "a_b_.txt"),
            ("보고서.pdf", "보고서.pdf"),
        ] {
            let p = incoming_path(&dir, input);
            assert_eq!(p.parent().unwrap(), dir, "{input}");
            assert_eq!(p.file_name().unwrap().to_str().unwrap(), want, "{input}");
        }
        fs::write(dir.join("a.txt"), b"x").unwrap();
        assert_eq!(incoming_path(&dir, "a.txt").file_name().unwrap(), "a (1).txt");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn identity_and_peers_persist() {
        let dir = std::env::temp_dir().join(format!("aun-store-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let a = identity(&dir).unwrap();
        assert_eq!(identity(&dir).unwrap().id, a.id);
        let typed: SocketAddr = "100.98.146.73:47810".parse().unwrap();
        save_peers(&dir, [(&a.id, "맥\t북\n", None), (&[9; 32], "PC", Some(typed))].into_iter()).unwrap();
        let back = load_peers(&dir);
        assert_eq!(back[&a.id], ("맥북".to_owned(), None));
        assert_eq!(back[&[9; 32]], ("PC".to_owned(), Some(typed)));
        // A file from before addresses were kept still loads.
        fs::write(dir.join("peers.txt"), format!("{}\tOld\n", hex(&a.id))).unwrap();
        assert_eq!(load_peers(&dir)[&a.id], ("Old".to_owned(), None));
        fs::remove_dir_all(dir).unwrap();
    }
}
