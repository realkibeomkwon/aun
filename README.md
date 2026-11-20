# aun

A Mac and a Windows PC working as one desk: pair them once with a 6-digit code, then clipboard
text stays in sync and files dropped on a device land in its Downloads folder. Rust, with a
[Rune](https://github.com/anomalyco/rune) UI. Inspired by [TwinDesk](https://github.com/inukaasith/twindesk);
no code or protocol is shared with it.

## Run

```sh
cargo run --release -p aun
```

Build on each machine (`cargo build --release -p aun`, Windows needs the MSVC build tools).
Allow Aun through the firewall on first launch: TCP **47810** (links) and UDP **47811** (discovery).

Closing the window keeps sync running and gives the GPU back. The menu bar item (macOS) or tray
icon (Windows) opens the window, switches clipboard sharing and quits; launching Aun again or
clicking the Dock icon also brings the window back. "로그인 시 실행" starts Aun at login without
its window (a LaunchAgent, or the `HKCU\...\Run` value). On macOS the Dock icon can be hidden.
Settings live in `settings.txt` in the state folder.

## Layout

- `crates/aun-core`: the engine, no UI. Discovery, pairing, encrypted links, clipboard, files.
  `cargo test -p aun-core` pairs two nodes over loopback and sends a 3 MB file.
- `app`: the Rune window. `cargo run -p aun -- --snap aun.png --scene demo` renders it headless.

## Protocol

- **Discovery**: UDP broadcast on 47811 every 3 s: `AUN1 | id[32] | tcp port u16 | name_len u8 | name`.
  The id is the device's X25519 static public key. A new device is answered at once. Sent to
  255.255.255.255 and to the directed broadcast address of every IPv4 interface that is up
  (`getifaddrs`, `GetAdaptersAddresses`), so a Mac on Ethernet and Wi-Fi reaches both.
- **By address**: where discovery doesn't reach, pair by typing the other device's IP. The guest
  keeps that address in `peers.txt` and redials it every 5 s while the link is down.
- **Sleep**: when the wall clock runs more than 10 s ahead of the monotonic one (the machine slept),
  every link is closed at once so the next announce or redial replaces it.
- **Link**: TCP 47810, `Noise_XX_25519_ChaChaPoly_BLAKE2s` (ChaCha20 on NEON on Apple Silicon via
  `--cfg chacha20_force_neon` in `.cargo/config.toml`, which a `RUSTFLAGS` environment variable
  would override; AVX2 on x86), then a byte stream of frames
  `[u32 len][u8 kind][payload]` packed into full Noise messages. One link per paired device.
  Frames: hello, spake, confirm, ping, clip, clip image, file start/data/end/abort. Frames are at
  most 4 MiB, clipboard images 16 MiB (checked from the kind byte before allocating). Pings every 10 s; 35 s of
  silence closes the link.
- **Pairing**: the host shows a one-time code (valid until used, hidden, or 3 wrong tries). SPAKE2 over the code yields a
  shared key; each side sends HMAC-SHA256(key, role ‖ Noise handshake hash), so a man in the middle
  fails and can't guess the code offline. The peer's static key is then pinned in `peers.txt`;
  unpaired keys are refused.
- **Files**: 256 KiB chunks, received as `name.part` and renamed when complete. Files for one
  peer go one at a time from a queue (a dropped pile shows every file at once as waiting). Either
  side can cancel: the sender sends `file abort`, the receiver deletes its `.part` and sends
  `file cancel`. The download folder can be changed (kept in `settings.txt`). Names from the
  network are reduced to one safe component (no paths, no Windows reserved names) and made unique.
- **Clipboard**: text up to 1 MiB, or an image as PNG (fdeflate "fast", encoded off the
  clipboard thread; decoded images are capped at 128 MiB of pixels). The OS change counter is
  polled every 300 ms (`GetClipboardSequenceNumber`, `NSPasteboard.changeCount`); the clipboard is
  read only when it moved. Copies marked private are never read or sent: the nspasteboard.org
  Concealed/Transient types on macOS, and on Windows `ExcludeClipboardContentFromMonitorProcessing`
  or `CanIncludeInClipboardHistory` / `CanUploadToCloudClipboard` = 0.

State lives in `%APPDATA%\Aun` or `~/Library/Application Support/Aun`.
