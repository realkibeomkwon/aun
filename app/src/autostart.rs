//! Start Aun at login, without its window: a LaunchAgent on macOS, the `HKCU\...\Run` value on
//! Windows. Whether it is on is read back from the system, so a login item the user removed in
//! System Settings or Task Manager shows as off here too.

use std::io;
use std::path::Path;

/// Passed at login: start with no window (the menu bar item or tray icon brings it).
pub const BACKGROUND: &str = "--background";

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::path::PathBuf;

    pub fn plist(home: &Path) -> PathBuf {
        home.join("Library/LaunchAgents/dev.aun.desk.plist")
    }

    pub fn enabled(home: &Path) -> bool {
        plist(home).exists()
    }

    pub fn set(home: &Path, exe: &Path, on: bool) -> io::Result<()> {
        let path = plist(home);
        if !on {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                r => r,
            };
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        let exe = exe.to_string_lossy().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        // launchd reads it at the next login; loading it now would start a second Aun.
        let text = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>dev.aun.desk</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>{BACKGROUND}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
</dict>
</plist>
"#
        );
        std::fs::write(path, text)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};

    pub const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Aun";

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    /// The command line stored under `key`, if any.
    pub fn value(key: &str) -> Option<String> {
        let (key, name) = (w(key), w(NAME));
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: NUL-terminated strings; `buf` holds `len` bytes.
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        (r == ERROR_SUCCESS).then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
    }

    pub fn set(key: &str, exe: &Path, on: bool) -> io::Result<()> {
        let (k, name) = (w(key), w(NAME));
        // SAFETY: NUL-terminated strings; `data` is `data.len() * 2` bytes.
        let r = unsafe {
            if on {
                let data = w(&format!("\"{}\" {BACKGROUND}", exe.display()));
                RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), name.as_ptr(), REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32)
            } else {
                match RegDeleteKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), name.as_ptr()) {
                    ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
                    r => r,
                }
            }
        };
        if r == ERROR_SUCCESS { Ok(()) } else { Err(io::Error::from_raw_os_error(r as i32)) }
    }
}

#[cfg(target_os = "macos")]
fn home() -> std::path::PathBuf {
    std::env::var_os("HOME").map(Into::into).unwrap_or_default()
}

pub fn enabled() -> bool {
    #[cfg(target_os = "macos")]
    return imp::enabled(&home());
    #[cfg(windows)]
    return imp::value(imp::RUN).is_some();
    #[allow(unreachable_code)]
    false
}

pub fn set(on: bool) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    #[cfg(target_os = "macos")]
    return imp::set(&home(), &exe, on);
    #[cfg(windows)]
    return imp::set(imp::RUN, &exe, on);
    #[allow(unreachable_code)]
    {
        let _ = (exe, on);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn the_launch_agent_is_valid_and_goes_away() {
        let home = std::env::temp_dir().join(format!("aun-autostart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let exe = Path::new("/Applications/A & <B>/Aun.app/Contents/MacOS/aun");
        assert!(!imp::enabled(&home));
        imp::set(&home, exe, true).unwrap();
        assert!(imp::enabled(&home));
        let lint = std::process::Command::new("plutil").arg("-lint").arg(imp::plist(&home)).output().unwrap();
        assert!(lint.status.success(), "{}", String::from_utf8_lossy(&lint.stdout));
        let args = std::process::Command::new("plutil").args(["-extract", "ProgramArguments.0", "raw"]).arg(imp::plist(&home)).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&args.stdout).trim(), exe.to_str().unwrap());
        imp::set(&home, exe, false).unwrap();
        assert!(!imp::enabled(&home));
        imp::set(&home, exe, false).expect("turning off twice is fine");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn the_run_value_is_quoted_and_goes_away() {
        let key = format!(r"Software\AunTest-{}", std::process::id());
        let exe = Path::new(r"C:\Program Files\Aun\aun.exe");
        assert_eq!(imp::value(&key), None);
        imp::set(&key, exe, true).unwrap();
        assert_eq!(imp::value(&key).as_deref(), Some(r#""C:\Program Files\Aun\aun.exe" --background"#));
        imp::set(&key, exe, false).unwrap();
        assert_eq!(imp::value(&key), None);
        imp::set(&key, exe, false).expect("turning off twice is fine");
        let k: Vec<u16> = key.encode_utf16().chain([0]).collect();
        // SAFETY: NUL-terminated key under HKCU that this test made.
        unsafe { windows_sys::Win32::System::Registry::RegDeleteTreeW(windows_sys::Win32::System::Registry::HKEY_CURRENT_USER, k.as_ptr()) };
    }
}
