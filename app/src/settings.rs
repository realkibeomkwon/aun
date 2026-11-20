//! What the user switched, kept as `key=value` lines in `settings.txt` beside the pairing data.
//! Unknown keys are ignored, so a later version can add some without breaking this one.

use std::io;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub clipboard: bool,
    /// macOS: show the Dock icon. Off, Aun lives in the menu bar only.
    pub dock: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { clipboard: true, dock: true }
    }
}

impl Settings {
    pub fn load(dir: &Path) -> Settings {
        let mut s = Settings::default();
        for line in std::fs::read_to_string(dir.join("settings.txt")).unwrap_or_default().lines() {
            match line.split_once('=') {
                Some(("clipboard", v)) => s.clipboard = v == "1",
                Some(("dock", v)) => s.dock = v == "1",
                _ => {}
            }
        }
        s
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = format!("clipboard={}\ndock={}\n", self.clipboard as u8, self.dock as u8);
        aun_core::store::write_atomic(&dir.join("settings.txt"), text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("aun-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Settings::load(&dir), Settings::default(), "no file: defaults");
        let off = Settings { clipboard: false, dock: false };
        off.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), off);
        std::fs::write(dir.join("settings.txt"), "junk\nclipboard=0\nfuture=7\n").unwrap();
        assert_eq!(Settings::load(&dir), Settings { clipboard: false, dock: true });
        std::fs::remove_dir_all(dir).unwrap();
    }
}
