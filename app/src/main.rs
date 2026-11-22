//! Aun: one desk across a Mac and a Windows PC. Pairs with a 6-digit code, then keeps clipboard
//! text in sync and takes files dropped on a device.
//!
//! `cargo run -p aun` opens the window. `-- --snap aun.png --scene demo` renders it headless.
//! Closing the window keeps sync running; launching again (or the Dock icon) brings it back.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod autostart;
mod settings;
mod tray;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long a status message stays up.
const MESSAGE_FOR: Duration = Duration::from_secs(5);
const FLASH_FOR: Duration = Duration::from_millis(1500);
/// Pixels the remote pointer moves per point moved on the pad.
const PAD_GAIN: f32 = 2.0;
/// macOS announces one input source switch more than once, 3-7 ms apart (seen 2026-11-22):
/// announcements this close are one switch.
const SAME_SWITCH: Duration = Duration::from_millis(100);
/// How long after an input source shortcut on the pad the switch it makes may be announced.
const COMBO_SWITCH: Duration = Duration::from_secs(3);
/// The pad's ⌘/Ctrl switch (`Settings::swap`), off and on.
const SWAP: [&str; 2] = ["⌘·Ctrl 맞바꾸기 꺼짐", "⌘·Ctrl 맞바꾸기 켜짐"];

use aun_core::wire::Frame;
use aun_core::{Config, Id, Node, Note, PeerView};
use settings::Settings;
use rune::tok::{self, Theme, radius, space, type_};
use rune::{App, AppHandle, AppSpec, Cx, El, FileDialogOptions, ScrollSpec, TextEdit, Tx, col, el, row, text, text_input};

struct Transfer {
    key: u64,
    peer_id: Id,
    peer: String,
    file: String,
    path: PathBuf,
    incoming: bool,
    done: u64,
    total: u64,
    finished: bool,
    failed: bool,
    cancelled: bool,
}

/// The trackpad driving a peer's pointer (docs/kvm-design.md, step 1).
struct Pad {
    id: Id,
    name: String,
    route: u32,
    mn: u32,
    /// Points moved since the pad opened, times the gain: sent as a running total.
    total: (f32, f32),
    /// The last pointer point over the pad while no button is down; None after it entered.
    last: Option<(f32, f32)>,
    /// The drag's travel already sent, while the button is down.
    held: Option<(f32, f32)>,
    /// Scrolling not yet sent: a slow trackpad scroll comes in fractions of a pixel.
    wheel: (f32, f32),
    /// Keys sent down and not yet up (HID usages).
    keys: Vec<u16>,
    /// Keys here sent as 한/영 or 한자 (`kvm::lang_keys`), read when the pad opens.
    lang: Vec<(rune::PhysicalKeyCode, u16)>,
    /// The pad has the keyboard: clicked or typed on, not lost it since.
    focused: bool,
    /// When the last input source switch was sent.
    switched: Option<Instant>,
    /// Keys that, with these modifiers (macOS flags), switch this Mac's input source
    /// (`kvm::input_source_combos`): kept here, the switch goes as 한/영.
    combos: Vec<(rune::PhysicalKeyCode, u64)>,
    /// When a shortcut in `combos` went as 한/영: the switch macOS announces after it is that one.
    combo_at: Option<Instant>,
}

struct Ui {
    node: Option<Node>,
    pad: Option<Pad>,
    /// The last pad's route: each opening starts the next.
    route: u32,
    #[cfg(test)]
    sent: std::cell::RefCell<Vec<Frame>>,
    me: String,
    peers: Vec<PeerView>,
    code: Option<String>,
    /// The nearby device a code is being typed for, and the typing.
    /// The device (its name and address) a code is being typed for.
    pairing: Option<(String, SocketAddr)>,
    /// An address typed for a device discovery can't find.
    ip_input: TextEdit,
    code_input: TextEdit,
    transfers: Vec<Transfer>,
    /// A status line and when it goes away.
    message: Option<(String, Instant)>,
    /// A transfer row marked for a moment (the one a repeated send would have duplicated).
    flash: Option<(u64, Instant)>,
    /// Files copied in Finder or Explorer, offered to send, and how many folders were left out.
    copied: Vec<PathBuf>,
    copied_dirs: usize,
    over: Option<Id>,
    /// Where `settings` is saved; None in headless runs.
    dir: Option<PathBuf>,
    settings: Settings,
    autostart: bool,
    handle: Option<AppHandle<Ui>>,
    tray: Option<tray::Tray>,
}

impl Ui {
    fn new(node: Option<Node>, dir: Option<PathBuf>) -> Ui {
        let me = node.as_ref().map_or_else(|| "시작하지 못했어요".into(), |n| n.name().to_owned());
        let settings = dir.as_deref().map(Settings::load).unwrap_or_default();
        let autostart = dir.is_some() && autostart::enabled();
        let mut ui = Ui {
            node, pad: None, route: 0, #[cfg(test)] sent: Default::default(), me, peers: vec![], code: None, pairing: None, ip_input: TextEdit::new(""), code_input: TextEdit::new(""), transfers: vec![], message: None, flash: None, copied: vec![], copied_dirs: 0, over: None,
            dir, settings, autostart, handle: None, tray: None,
        };
        ui.refresh();
        ui
    }

    /// The one way clipboard sharing changes: the header button, the tray menu.
    fn set_clip(&mut self, on: bool, tx: &mut Tx) {
        if let Some(n) = &self.node {
            n.set_clipboard(on);
        }
        self.settings.clipboard = on;
        if let Some(t) = &self.tray {
            t.set_clip(on);
        }
        self.save(tx);
    }

    fn set_secrets(&mut self, on: bool, tx: &mut Tx) {
        if let Some(n) = &self.node {
            n.set_secrets(on);
        }
        self.settings.secrets = on;
        self.save(tx);
    }

    fn set_kvm(&mut self, on: bool, tx: &mut Tx) {
        if let Some(n) = &self.node {
            n.set_kvm(on);
        }
        self.settings.kvm = on;
        self.save(tx);
        // macOS drops posted input without Accessibility; asking opens the system's request.
        if on && self.node.is_some() && !aun_core::kvm::allowed(true) {
            self.say("시스템 설정 > 개인정보 보호 및 보안에서 Aun을 허용하세요 (macOS 27은 \"System & App Protection Override\", 그 전은 \"손쉬운 사용\". 목록에 없으면 + 로 추가하고, 새 버전을 설치한 뒤에는 지웠다가 다시 추가)", tx);
        }
    }

    fn open_pad(&mut self, id: Id, name: String) {
        self.close_pad();
        self.route = self.route.wrapping_add(1);
        let lang = aun_core::kvm::lang_keys();
        aun_core::log(format_args!("pad: keys sent as 한/영 (0x90) or 한자 (0x91): {lang:?}"));
        self.pad = Some(Pad { id, name, route: self.route, mn: 0, total: (0.0, 0.0), last: None, held: None, wheel: (0.0, 0.0), keys: Vec::new(), lang, focused: false, switched: None, combos: aun_core::kvm::input_source_combos(), combo_at: None });
    }

    /// Lets go of what the pad holds at the peer, then closes it.
    fn close_pad(&mut self) {
        if let Some(p) = self.pad.take() {
            self.input(p.id, Frame::KvmLeave);
        }
        menu_off(false);
    }

    fn input(&self, id: Id, f: Frame) {
        #[cfg(test)]
        self.sent.borrow_mut().push(f.clone());
        if let Some(n) = &self.node {
            let must = matches!(f, Frame::KvmButton { .. } | Frame::KvmLeave | Frame::KvmKey { .. });
            let what = must.then(|| format!("{f:?}"));
            if !n.send_input(id, f)
                && let Some(what) = what
            {
                // A button left down at the peer would show up here.
                aun_core::log(format_args!("input not sent: {what}"));
            }
        }
    }

    /// A key on this keyboard while the pad has focus. Every key is taken, sent or not: Tab
    /// would move focus off the pad, Space and Return press a button here.
    fn pad_key(&mut self, e: &rune::PhysicalInput) -> bool {
        let swap = self.settings.swap;
        let Some(p) = &mut self.pad else { return false };
        let id = p.id;
        p.focused = !matches!(e, rune::PhysicalInput::FocusLost);
        let sent: Vec<Frame> = match *e {
            rune::PhysicalInput::Key { key, pressed, mods, .. } => {
                let held = [(mods.shift, 0x2_0000), (mods.ctrl, 0x4_0000), (mods.alt, 0x8_0000), (mods.logo, 0x10_0000)].iter().filter(|m| m.0).fold(0, |f, m| f | m.1);
                if pressed && p.combos.contains(&(key, held)) {
                    // As 한/영 now, while its modifier is down at the peer: macOS switches only
                    // when the modifier comes up, after it went up there (a Win alone: Start).
                    // The key's up finds nothing held and goes nowhere.
                    p.combo_at = Some(Instant::now());
                    for down in [true, false] {
                        self.input(id, Frame::KvmKey { hid: 0x90, down });
                    }
                    return true;
                }
                let hid = p.lang.iter().find(|l| l.0 == key).map(|l| l.1).or_else(|| aun_core::kvm::hid(key));
                let Some(hid) = hid else { return true };
                let hid = swapped(hid, swap);
                let at = p.keys.iter().position(|&k| k == hid);
                match (pressed, at) {
                    (true, None) => p.keys.push(hid),
                    (true, Some(_)) => {} // repeating: the peer's OS doesn't repeat it for us
                    (false, None) => return true,
                    (false, Some(i)) => drop(p.keys.remove(i)),
                }
                vec![Frame::KvmKey { hid, down: pressed }]
            }
            // Focus went (another control, another app): what is held here would stay down there.
            rune::PhysicalInput::FocusLost => p.keys.drain(..).rev().map(|hid| Frame::KvmKey { hid, down: false }).collect(),
            _ => return false,
        };
        for f in sent {
            self.input(id, f);
        }
        // ⌘ held here: AppKit would also run the menu item (⌘Q quits Aun) for the key going there.
        menu_off(self.command_held());
        true
    }

    /// macOS switched the input source. With the pad focused, that was 한/영 meant for the peer:
    /// the key itself (Caps Lock, F18, ⌃Space) goes to the system only and never reaches the pad.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))] // only macOS announces it
    fn input_source_changed(&mut self) {
        let Some(p) = self.pad.as_mut().filter(|p| p.focused) else { return };
        if p.switched.is_some_and(|t| t.elapsed() < SAME_SWITCH) {
            return;
        }
        p.switched = Some(Instant::now());
        if p.combo_at.take().is_some_and(|t| t.elapsed() < COMBO_SWITCH) {
            return; // sent when the shortcut was pressed
        }
        let id = p.id;
        self.input(id, Frame::KvmKey { hid: 0x90, down: true });
        self.input(id, Frame::KvmKey { hid: 0x90, down: false });
    }

    /// Whether ⌘ on this keyboard (whatever it is sent as) is down on the pad.
    fn command_held(&self) -> bool {
        self.pad.as_ref().is_some_and(|p| p.keys.iter().any(|&k| matches!(swapped(k, self.settings.swap), 0xE3 | 0xE7)))
    }

    fn pad_move(&mut self, dx: f32, dy: f32) {
        let Some(p) = &mut self.pad else { return };
        p.total = (p.total.0 + dx * PAD_GAIN, p.total.1 + dy * PAD_GAIN);
        p.mn += 1;
        let f = Frame::KvmMove { route: p.route, mn: p.mn, cx: p.total.0.round() as i32, cy: p.total.1.round() as i32 };
        let id = p.id;
        self.input(id, f);
    }

    fn set_dock(&mut self, on: bool, tx: &mut Tx) {
        self.settings.dock = on;
        if let Some(h) = &self.handle {
            h.set_dock_icon(on);
        }
        self.save(tx);
    }

    fn set_autostart(&mut self, on: bool, tx: &mut Tx) {
        match autostart::set(on) {
            Ok(()) => self.autostart = on,
            Err(e) => self.say(format!("자동 실행을 바꾸지 못했어요: {e}"), tx),
        }
    }

    fn save(&mut self, tx: &mut Tx) {
        let Some(dir) = &self.dir else { return };
        if let Err(e) = self.settings.save(dir) {
            self.say(format!("설정을 저장하지 못했어요: {e}"), tx);
        }
    }

    fn refresh(&mut self) {
        if let Some(n) = &self.node {
            self.peers = n.peers();
            self.code = n.code();
        }
        // Its link went: the peer let go of what it held when it dropped.
        if self.pad.as_ref().is_some_and(|p| !self.peers.iter().any(|v| v.id == p.id && v.kvm)) {
            self.pad = None;
            menu_off(false);
        }
    }

    fn say(&mut self, m: impl Into<String>, tx: &mut Tx) {
        self.message = Some((m.into(), Instant::now() + MESSAGE_FOR));
        tx.wake_after(MESSAGE_FOR); // runs `poll` then, which drops it
    }

    /// Drops a status message past its time; true when the view must re-run.
    fn expire(&mut self) -> bool {
        let now = Instant::now();
        let old = self.message.as_ref().is_some_and(|(_, until)| *until <= now);
        if old {
            self.message = None;
        }
        let done = self.flash.is_some_and(|(_, until)| until <= now);
        if done {
            self.flash = None;
        }
        old || done
    }

    fn apply(&mut self, note: Note, tx: &mut Tx) {
        match note {
            Note::Changed => self.refresh(),
            Note::Info(m) => {
                self.say(m, tx);
                self.refresh();
            }
            // Nothing held here, but the peer may be gone mid-drag: stop, and say why.
            Note::InputLost(id) if self.pad.as_ref().is_some_and(|p| p.id == id) => {
                self.close_pad();
                self.say("상대가 응답하지 않아 원격 조작을 멈췄어요", tx);
            }
            Note::InputLost(_) => {}
            Note::Copied(paths) => {
                let n = paths.len();
                self.copied = paths.into_iter().filter(|p| p.is_file()).collect();
                self.copied_dirs = n - self.copied.len();
                self.show_copied();
            }
            Note::AlreadySending { key, file } => {
                self.say(format!("이미 보내는 중이에요: {file}"), tx);
                self.flash = Some((key, Instant::now() + FLASH_FOR));
                tx.scroll_into_view_in("transfers", format!("t-{key}"));
                tx.wake_after(FLASH_FOR);
            }
            Note::Transfer { key, peer_id, peer, file, path, done, total, incoming, finished, failed, cancelled } => {
                match self.transfers.iter_mut().find(|t| t.key == key) {
                    Some(t) => (t.done, t.finished, t.failed, t.cancelled) = (done, finished, failed, cancelled),
                    None => self.transfers.insert(0, Transfer { key, peer_id, peer, file, path, incoming, done, total, finished, failed, cancelled }),
                }
                // Keep every unfinished one (a dropped pile waits in line) and the latest 100 done:
                // a big pile stays checkable, at about 100 bytes a row.
                let mut kept_done = 0;
                self.transfers.retain(|t| !t.finished || { kept_done += 1; kept_done <= 100 });
            }
        }
    }

    fn show_copied(&self) {
        if let Some(t) = &self.tray {
            t.set_copied(!self.copied.is_empty());
        }
    }

    /// Sends the copied files to `id`, or (from the tray, `None`) to the one connected device;
    /// with none or several, the window opens to choose.
    fn send_copied(&mut self, id: Option<Id>) {
        let mut on = self.peers.iter().filter(|p| p.connected).map(|p| p.id);
        let id = id.or_else(|| on.next().filter(|_| on.next().is_none()));
        match id {
            Some(id) => {
                let files = std::mem::take(&mut self.copied);
                self.send(id, &files);
                self.show_copied();
            }
            None => {
                if let Some(h) = &self.handle {
                    h.open();
                }
            }
        }
    }

    fn send(&mut self, id: Id, files: &[PathBuf]) {
        if let Some(n) = &self.node {
            n.send_files(id, files.iter().filter(|f| f.is_file()).cloned().collect());
        }
    }

    fn set_downloads(&mut self, dir: PathBuf, tx: &mut Tx) {
        if let Some(n) = &self.node {
            n.set_downloads(dir.clone());
        }
        self.settings.downloads = Some(dir);
        self.save(tx);
    }

    fn downloads(&self) -> PathBuf {
        self.settings.downloads.clone().unwrap_or_else(downloads_dir)
    }
}

fn button(key: String, label: &str, t: Theme, primary: bool, f: impl Fn(&mut Ui, &mut Tx) + 'static) -> El {
    let fill = if primary { tok::accent(0, true) } else { t.card };
    let color = if primary { rune::oklch(0.99, 0.0, 0.0) } else { t.text };
    row().key(key).role("button").label(label).focusable(true).h(28.0).px(space::S3).center().radius(radius::MD)
        .fill(fill).border(1.0, t.card_border).hover(t.hover).on_press_tx(f)
        .child(text(label.to_owned()).font(type_::LABEL).color(color))
}

fn status(p: &PeerView) -> &'static str {
    match (p.paired, p.connected, p.nearby) {
        (true, true, _) => "연결됨 · 파일을 끌어다 놓으세요",
        (true, false, true) => "연결 중…",
        (true, false, false) => "오프라인",
        _ => "근처 기기 · 페어링되지 않음",
    }
}

fn peer_row(p: &PeerView, s: &Ui, t: Theme) -> El {
    let id = p.id;
    let k = aun_core::hex(&id[..6]);
    let dot = if p.connected { tok::accent(0, true) } else { t.control_off };
    // The text gives way (wraps) before the buttons do: a narrow window cut the last one off.
    let info = col().grow(1.0).shrink().min_w(0.0).gap(2.0)
        .child(text(p.name.clone()).font(type_::BODY).color(t.text).wrap_anywhere())
        .child(text(status(p)).font(type_::LABEL).color(t.text_secondary).wrap());
    let mut r = row().key(format!("peer-{k}")).w_full().min_h(52.0).px(space::S3).py(space::S2).gap(space::S3).center_y()
        .radius(radius::MD).fill(if s.over == Some(id) { t.hover } else { t.card }).border(1.0, t.card_border)
        .child(el().size(8.0, 8.0).radius(4.0).fill(dot)).child(info);
    if p.connected {
        r = r.child(button(format!("send-{k}"), "파일 보내기", t, true, move |_, tx| {
            tx.open_file(FileDialogOptions { title: "보낼 파일".into(), ..Default::default() }, move |s: &mut Ui, res, _| {
                if let Ok(Some(path)) = res {
                    s.send(id, &[path]);
                }
            });
        }))
        .on_drop_files(move |s: &mut Ui, files, _| s.send(id, files))
        .on_drop_hover(move |s: &mut Ui, over, _| s.over = over.then_some(id));
    }
    if p.kvm {
        let open = s.pad.as_ref().is_some_and(|x| x.id == id);
        let name = p.name.clone();
        r = r.child(button(format!("pad-{k}"), "원격 조작", t, open, move |s, _| {
            if open { s.close_pad() } else { s.open_pad(id, name.clone()) }
        }));
    }
    if p.paired {
        r = r.child(button(format!("forget-{k}"), "해제", t, false, move |s, _| {
            if let Some(n) = &s.node {
                n.forget(id);
            }
        }));
    } else if let Some(addr) = p.addr
        && s.pairing.as_ref().is_none_or(|(_, a)| *a != addr)
    {
        let name = p.name.clone();
        r = r.child(button(format!("pair-{k}"), "페어링", t, true, move |s, _| {
            s.pairing = Some((name.clone(), addr));
            s.code_input = TextEdit::new("");
        }));
    }
    r
}

fn pair_box(s: &Ui, t: Theme) -> Option<El> {
    let (name, _) = s.pairing.as_ref()?;
    let submit = |s: &mut Ui, tx: &mut Tx| {
        let Some((_, addr)) = s.pairing.take() else { return };
        let code: String = s.code_input.text.chars().filter(char::is_ascii_digit).collect();
        match (&s.node, code.len()) {
            (Some(n), 6) => {
                n.pair(addr, code);
                s.say("페어링 중…", tx);
            }
            _ => s.say("6자리 숫자를 입력하세요", tx),
        }
    };
    // The input itself fills the box: a click anywhere in it focuses the field.
    let field = el().key("code-field").w(120.0).h(32.0).radius(radius::MD).fill(t.bg).border(1.0, t.card_border).center_y()
        .child(text_input(&s.code_input, type_::BODY).key("code-input").w_full().px(space::S2).py(6.0).label("페어링 코드").color(t.text).placeholder("000 000", t.text_secondary)
            .on_edit(|s: &mut Ui| &mut s.code_input).on_submit(submit)
            // Codes are six digits, shown "123 456" like the host shows them: drop anything else
            // as it is typed or pasted. shortcut: the caret goes to the end after a reformat.
            .on_input_change(|s: &mut Ui, _| {
                let mut digits: String = s.code_input.text.chars().filter(char::is_ascii_digit).take(6).collect();
                if digits.len() > 3 {
                    digits.insert(3, ' ');
                }
                if digits != s.code_input.text {
                    // In place (caret to the end): a new TextEdit would drop the field's focus.
                    s.code_input.replace_text(digits);
                }
            }));
    Some(col().key("pair-box").w_full().gap(space::S2).padding(space::S3).radius(radius::MD).fill(t.card).border(1.0, t.card_border)
        .child(text(format!("{name}에 표시된 6자리 코드를 입력하세요")).font(type_::LABEL).color(t.text))
        .child(row().gap(space::S2).center_y().child(field)
            .child(button("pair-go".into(), "연결", t, true, submit))
            .child(button("pair-cancel".into(), "취소", t, false, |s, _| s.pairing = None))))
}

/// `192.168.0.5` or `192.168.0.5:47810`; Aun's port when none is given.
fn parse_addr(s: &str) -> Option<SocketAddr> {
    let s = s.trim();
    s.parse().ok().or_else(|| Some(SocketAddr::new(s.parse::<std::net::IpAddr>().ok()?, aun_core::node::PORT)))
}

/// A host name such as a Tailscale device name (`kibeom-windows`), with an optional port.
fn parse_name(s: &str) -> Option<(String, u16)> {
    let s = s.trim();
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (s, aun_core::node::PORT),
    };
    let label_ok = |l: &str| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    // An all-digit last label is a mistyped IP ("192.168.0"), which the resolver would still
    // turn into some address.
    let numeric = host.rsplit('.').next().is_some_and(|l| l.chars().all(|c| c.is_ascii_digit()));
    (host.split('.').all(label_ok) && !numeric).then(|| (host.to_owned(), port))
}

/// The first IPv4 address of `host`: Aun listens on IPv4 only.
fn resolve(host: &str, port: u16) -> std::io::Result<SocketAddr> {
    use std::net::ToSocketAddrs;
    (host, port).to_socket_addrs()?.find(SocketAddr::is_ipv4).ok_or_else(|| std::io::Error::other("IPv4 주소가 없어요"))
}

/// For networks where discovery doesn't reach (another subnet, a VPN, broadcasts blocked).
fn by_address(s: &Ui, t: Theme) -> El {
    let go = |s: &mut Ui, tx: &mut Tx| {
        if let Some(addr) = parse_addr(&s.ip_input.text) {
            s.pairing = Some((addr.ip().to_string(), addr));
            s.code_input = TextEdit::new("");
            s.ip_input = TextEdit::new("");
        } else if let (Some((host, port)), Some(h)) = (parse_name(&s.ip_input.text), s.handle.clone()) {
            s.ip_input = TextEdit::new("");
            s.say(format!("{host} 찾는 중…"), tx);
            // A lookup can take seconds (an unknown name waits out DNS): off the UI thread.
            std::thread::spawn(move || {
                let found = resolve(&host, port);
                h.update(move |s: &mut Ui, tx| match found {
                    Ok(addr) => {
                        s.pairing = Some((host, addr));
                        s.code_input = TextEdit::new("");
                        s.message = None;
                    }
                    Err(e) => s.say(format!("{host}을(를) 찾지 못했어요: {e}"), tx),
                });
            });
        } else {
            s.say("IP 주소나 기기 이름을 확인하세요 (예: 192.168.0.5)", tx);
        }
    };
    let field = el().key("ip-field").grow(1.0).h(28.0).radius(radius::MD).fill(t.bg).border(1.0, t.card_border).center_y()
        .child(text_input(&s.ip_input, type_::LABEL).key("ip-input").w_full().px(space::S2).py(4.0).label("상대 기기 IP 주소 또는 이름").color(t.text)
            .placeholder("목록에 없나요? IP 주소나 이름으로 페어링 (예: 192.168.0.5)", t.text_secondary)
            .on_edit(|s: &mut Ui| &mut s.ip_input).on_submit(go));
    row().key("by-address").w_full().gap(space::S2).center_y().child(field).child(button("ip-go".into(), "페어링", t, false, go))
}

fn view(s: &Ui, cx: &mut Cx<Ui>) -> El {
    let t = tok::theme(cx.system_dark());
    let header = row().w_full().center_y().gap(space::S3)
        .child(col().grow(1.0).gap(2.0)
            .child(text("Aun").font(type_::TITLE).color(t.text))
            .child(text(format!("이 기기: {}", s.me)).font(type_::LABEL).color(t.text_secondary)))
        .child(button("clip".into(), if s.settings.clipboard { "클립보드 공유 켜짐" } else { "클립보드 공유 꺼짐" }, t, s.settings.clipboard, |s, tx| {
            s.set_clip(!s.settings.clipboard, tx)
        }));
    let mut prefs = row().key("prefs").w_full().gap(space::S2).flex_wrap()
        .child(button("autostart".into(), if s.autostart { "로그인 시 실행 켜짐" } else { "로그인 시 실행 꺼짐" }, t, s.autostart, |s, tx| {
            s.set_autostart(!s.autostart, tx)
        }))
        .child(button("secrets".into(), if s.settings.secrets { "비밀번호 공유 켜짐" } else { "비밀번호 공유 꺼짐" }, t, s.settings.secrets, |s, tx| {
            s.set_secrets(!s.settings.secrets, tx)
        }))
        .child(button("kvm".into(), if s.settings.kvm { "원격 조작 허용 켜짐" } else { "원격 조작 허용 꺼짐" }, t, s.settings.kvm, |s, tx| {
            s.set_kvm(!s.settings.kvm, tx)
        }));
    if cfg!(target_os = "macos") {
        prefs = prefs.child(button("dock".into(), if s.settings.dock { "Dock 아이콘 표시" } else { "Dock 아이콘 숨김" }, t, s.settings.dock, |s, tx| {
            s.set_dock(!s.settings.dock, tx)
        }));
    }

    let folder = row().key("downloads").w_full().gap(space::S2).center_y()
        .child(text(format!("받은 파일: {}", short_path(&s.downloads()))).font(type_::LABEL).color(t.text_secondary).grow(1.0))
        .child(button("downloads-pick".into(), "폴더 변경", t, false, |s, tx| {
            let directory = Some(s.downloads());
            tx.open_folder(FileDialogOptions { title: "받은 파일을 둘 폴더".into(), directory, ..Default::default() }, |s: &mut Ui, res, tx| {
                if let Ok(Some(dir)) = res {
                    s.set_downloads(dir, tx);
                }
            });
        }));

    let code = match &s.code {
        Some(c) => row().key("code").w_full().gap(space::S3).center_y().padding(space::S3).radius(radius::MD).fill(t.card).border(1.0, t.card_border)
            .child(col().grow(1.0).gap(2.0)
                .child(text("다른 기기에서 이 코드를 입력하세요").font(type_::LABEL).color(t.text_secondary))
                .child(text(format!("{} {}", &c[..3], &c[3..])).key("code-digits").font(type_::TITLE).color(t.text)))
            .child(button("code-hide".into(), "닫기", t, false, |s, _| {
                if let Some(n) = &s.node {
                    n.hide_code();
                }
            })),
        None => row().w_full().child(button("code-show".into(), "페어링 코드 표시", t, false, |s, _| {
            if let Some(n) = &s.node {
                n.show_code();
            }
            s.refresh();
        })),
    };

    let mut list = col().key("peers").w_full().gap(space::S2);
    if s.peers.is_empty() {
        list = list.child(text("같은 네트워크에서 Aun을 실행한 기기를 찾는 중…").font(type_::LABEL).color(t.text_secondary));
    }
    for p in &s.peers {
        list = list.child(peer_row(p, s, t));
    }

    // The controls stay put while the transfer history below scrolls on its own. In a short
    // window the controls give way first (and scroll), keeping a few history rows in view.
    let mut controls = col().key("controls").w_full().shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).gap(space::S3)
        .child(header).child(prefs).child(folder).child(code).child(list).child(by_address(s, t));
    if let Some(b) = pair_box(s, t) {
        controls = controls.child(b);
    }
    let mut root = col().key("root").w_full().h_full().padding(space::S6).gap(space::S3).fill(t.bg).child(controls);
    // Outside the scrolling controls, so it stays in view while it is in use.
    if let Some(p) = pad(s, t) {
        root = root.child(p);
    }
    // Notices sit just above the history, where the files they are about are.
    if let Some((m, _)) = &s.message {
        let close = row().key("message-close").role("button").label("닫기").focusable(true).size(24.0, 24.0).center().radius(radius::MD)
            .hover(t.hover).on_press_tx(|s: &mut Ui, _| s.message = None)
            .child(rune::icon(&CLOSE, 12.0).color(t.text_secondary));
        root = root.child(row().key("message-banner").w_full().px(space::S3).py(space::S2).gap(space::S2).center_y()
            .radius(radius::MD).fill(t.card).border(1.0, tok::accent(0, true))
            .child(rune::icon(&INFO, 16.0).color(tok::accent(0, true)))
            .child(text(m.clone()).key("message").font(type_::LABEL).color(t.text).grow(1.0).shrink().wrap_anywhere())
            .child(close));
    }
    if !s.copied.is_empty() {
        let close = row().key("copied-close").role("button").label("닫기").focusable(true).size(24.0, 24.0).center().radius(radius::MD)
            .hover(t.hover).on_press_tx(|s: &mut Ui, _| {
                s.copied.clear();
                s.show_copied();
            })
            .child(rune::icon(&CLOSE, 12.0).color(t.text_secondary));
        let mut b = row().key("copied").w_full().px(space::S3).py(space::S2).gap(space::S2).center_y()
            .radius(radius::MD).fill(t.card).border(1.0, tok::accent(0, true))
            .child(text(copied_text(s)).key("copied-text").font(type_::LABEL).color(t.text).grow(1.0).shrink().wrap_anywhere());
        for p in s.peers.iter().filter(|p| p.connected) {
            let id = p.id;
            b = b.child(button(format!("copied-send-{}", aun_core::hex(&id[..6])), &format!("{}로 보내기", p.name), t, true, move |s, _| s.send_copied(Some(id))));
        }
        root = root.child(b.child(close));
    }
    if !s.transfers.is_empty() {
        root = root.child(transfers(s, t));
    }
    root
}

/// Moving over it moves the peer's pointer; a click, a drag, a right click and a scroll here
/// happen there.
fn pad(s: &Ui, t: Theme) -> Option<El> {
    let p = s.pad.as_ref()?;
    let area = el().key("pad-area").w_full().h(180.0).radius(radius::MD).fill(t.bg).border(1.0, tok::accent(0, true)).center()
        .label(format!("{} 트랙패드", p.name))
        .child(text("여기서 움직이면 상대 커서가 움직여요 · 클릭하면 키보드도 상대에게 가요").font(type_::LABEL).color(t.text_secondary))
        .focusable(true)
        .on_physical_input(|s: &mut Ui, e, _| s.pad_key(e))
        // Out and back in elsewhere is no move: the next point over the pad starts afresh.
        .on_hover_change(|s: &mut Ui, inside, _| {
            if let Some(p) = &mut s.pad
                && !inside
            {
                p.last = None;
            }
        })
        .on_pointer_move_local(|s: &mut Ui, e, _| {
            let Some(p) = &mut s.pad else { return };
            if p.held.is_some() {
                return; // the drag sends its own travel
            }
            let last = p.last.replace((e.x, e.y));
            if let Some((x, y)) = last {
                s.pad_move(e.x - x, e.y - y);
            }
        })
        .on_drag(|s: &mut Ui, d, _| {
            let Some(p) = &mut s.pad else { return };
            let id = p.id;
            match d.phase {
                rune::Phase::Down => {
                    (p.held, p.focused) = (Some((0.0, 0.0)), true);
                    s.input(id, Frame::KvmButton { button: 0, down: true, clicks: d.clicks.min(255) as u8 });
                }
                rune::Phase::Move => {
                    let (x, y) = p.held.replace((d.dx, d.dy)).unwrap_or((d.dx, d.dy));
                    s.pad_move(d.dx - x, d.dy - y);
                }
                // A cancelled gesture lets go too: the button is down at the peer.
                rune::Phase::Up => {
                    (p.held, p.last) = (None, None);
                    s.input(id, Frame::KvmButton { button: 0, down: false, clicks: d.clicks.min(255) as u8 });
                }
            }
        })
        .on_secondary_press(|s: &mut Ui, _, _| {
            if let Some(id) = s.pad.as_ref().map(|p| p.id) {
                s.input(id, Frame::KvmButton { button: 1, down: true, clicks: 1 });
                s.input(id, Frame::KvmButton { button: 1, down: false, clicks: 1 });
            }
        })
        .on_wheel(|s: &mut Ui, w, _| {
            let Some(p) = &mut s.pad else { return true };
            let (x, y) = (p.wheel.0 + w.dx, p.wheel.1 + w.dy);
            let px = |v: f32| v.trunc().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            let (dx, dy) = (px(x), px(y));
            p.wheel = (x - dx as f32, y - dy as f32);
            let id = p.id;
            if dx != 0 || dy != 0 {
                s.input(id, Frame::KvmWheel { dx, dy });
            }
            true
        });
    Some(col().key("pad").w_full().gap(space::S2).padding(space::S3).radius(radius::MD).fill(t.card).border(1.0, t.card_border)
        .child(row().w_full().center_y().gap(space::S2).flex_wrap()
            .child(text(format!("{} 원격 조작 중", p.name)).font(type_::LABEL).color(t.text).grow(1.0))
            .child(button("pad-swap".into(), SWAP[s.settings.swap as usize], t, s.settings.swap, |s, tx| {
                s.settings.swap = !s.settings.swap;
                s.save(tx);
            }))
            .child(button("pad-close".into(), "그만", t, false, |s, _| s.close_pad())))
        .child(area))
}

/// Control and ⌘ (the Windows key) swapped, left and right, when `swap` (`Settings::swap`).
fn swapped(hid: u16, swap: bool) -> u16 {
    match hid {
        0xE0 | 0xE4 if swap => hid + 3,
        0xE3 | 0xE7 if swap => hid - 3,
        _ => hid,
    }
}

/// macOS: takes the menu bar away (`true`) and puts it back, so no menu key equivalent runs.
/// Elsewhere, and off the main thread (tests), nothing.
fn menu_off(off: bool) {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplication, NSMenu};
        thread_local!(static TAKEN: std::cell::RefCell<Option<objc2::rc::Retained<NSMenu>>> = const { std::cell::RefCell::new(None) });
        let Some(mtm) = objc2::MainThreadMarker::new() else { return };
        let app = NSApplication::sharedApplication(mtm);
        TAKEN.with_borrow_mut(|taken| match (off, taken.is_some()) {
            (true, false) => {
                *taken = app.mainMenu();
                app.setMainMenu(None);
            }
            (false, true) => app.setMainMenu(taken.take().as_deref()),
            _ => {}
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = off;
}

fn copied_text(s: &Ui) -> String {
    let dirs = if s.copied_dirs > 0 { format!(" (폴더 {}개는 보낼 수 없어요)", s.copied_dirs) } else { String::new() };
    format!("복사한 파일 {}개{dirs}", s.copied.len())
}

/// Direction arrows for the transfer history: an arrow inside a ring, both stroked in the
/// element's colour.
static ARROW_DOWN: rune::Icon = rune::Icon {
    name: "aun-arrow-down",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="11" stroke="currentColor" stroke-width="1.75" fill="none"/><path d="M12 6V17.5M7.25 12.75L12 17.5L16.75 12.75" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
static ARROW_UP: rune::Icon = rune::Icon {
    name: "aun-arrow-up",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="11" stroke="currentColor" stroke-width="1.75" fill="none"/><path d="M12 18V6.5M7.25 11.25L12 6.5L16.75 11.25" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static FAILED: rune::Icon = rune::Icon {
    name: "aun-failed",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="11" stroke="currentColor" stroke-width="1.75" fill="none"/><path d="M8 8L16 16M16 8L8 16" stroke="currentColor" stroke-width="2" stroke-linecap="round" fill="none"/>"#,
};
static STOPPED: rune::Icon = rune::Icon {
    name: "aun-stopped",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="11" stroke="currentColor" stroke-width="1.75" fill="none"/><path d="M7.5 12H16.5" stroke="currentColor" stroke-width="2" stroke-linecap="round" fill="none"/>"#,
};

static INFO: rune::Icon = rune::Icon {
    name: "aun-info",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="10.5" stroke="currentColor" stroke-width="2" fill="none"/><path d="M12 11V17" stroke="currentColor" stroke-width="2.25" stroke-linecap="round"/><circle cx="12" cy="7.5" r="1.4" fill="currentColor"/>"#,
};
static CLOSE: rune::Icon = rune::Icon {
    name: "aun-close",
    view: 24.0,
    body: r#"<path d="M6 6L18 18M18 6L6 18" stroke="currentColor" stroke-width="2.25" stroke-linecap="round"/>"#,
};

/// The ring alone: the faint track a transfer's progress arc runs on.
static RING: rune::Icon = rune::Icon {
    name: "aun-ring",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="11" stroke="currentColor" stroke-width="1.75" fill="none"/>"#,
};

/// The ring's arc for `done` (0 to 1), clockwise from 12 o'clock, in 2% steps: icons are
/// static, so each step is built once and kept (51 at most, a few KB).
fn progress_arc(done: f64) -> &'static rune::Icon {
    use std::f64::consts::{FRAC_PI_2, TAU};
    static CACHE: std::sync::Mutex<[Option<&'static rune::Icon>; 51]> = std::sync::Mutex::new([None; 51]);
    let step = (done.clamp(0.0, 1.0) * 50.0).round() as usize;
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache[step].get_or_insert_with(|| {
        let a = step as f64 / 50.0 * TAU - FRAC_PI_2;
        let (x, y, large) = (12.0 + 11.0 * a.cos(), 12.0 + 11.0 * a.sin(), u8::from(step > 25));
        let body: &'static str = match step {
            0 => "",
            50 => RING.body,
            _ => format!(r#"<path d="M12 1A11 11 0 {large} 1 {x:.3} {y:.3}" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" fill="none"/>"#).leak(),
        };
        Box::leak(Box::new(rune::Icon { name: "aun-progress", view: 24.0, body }))
    })
}

/// Rune's accents have no red; this one reads on light and dark cards alike.
fn red() -> rune::Color {
    rune::oklch(0.63, 0.21, 27.0)
}

/// The history: one card per file, newest first, in a box that takes the rest of the window.
fn transfers(s: &Ui, t: Theme) -> El {
    let mut tl = col().w_full().gap(space::S2);
    for x in &s.transfers {
        let pct = (x.done * 100).checked_div(x.total).unwrap_or(100);
        let state = match () {
            _ if x.cancelled => "취소됨".to_owned(),
            _ if x.failed => "실패".into(),
            _ if x.finished => "완료".into(),
            _ => format!("{pct}%"),
        };
        // The arrow and its colour say the direction (received green ↓, sent blue ↑); the line
        // below names the peer. Screen readers get the direction from the badge's label.
        // A failure (not a cancel, which the user chose) shows a red ✕ instead of the direction.
        let failed = x.failed && !x.cancelled;
        let (arrow, way, tint) = match () {
            _ if failed => (&FAILED, "실패", red()),
            // Called off by either side: grey, nothing went wrong.
            _ if x.finished && x.cancelled => (&STOPPED, "취소됨", t.text_secondary),
            _ if x.incoming => (&ARROW_DOWN, "받음", tok::accent(4, true)),
            _ => (&ARROW_UP, "보냄", tok::accent(0, true)),
        };
        // Two thirds of the two text lines' height, centred beside them.
        let d = 32.0 * 2.0 / 3.0;
        // Under way: no arrow, the ring itself fills clockwise as the bytes go.
        let badge = if x.finished {
            rune::icon(arrow, d).color(tint).label(way)
        } else {
            el().size(d, d).label(format!("{way} {pct}%"))
                .child(rune::icon(&RING, d).absolute(0.0, 0.0).color(tint).opacity(0.25))
                .child(rune::icon(progress_arc(pct as f64 / 100.0), d).absolute(0.0, 0.0).color(tint))
        };
        let info = col().grow(1.0).shrink().gap(2.0)
            .child(text(x.file.clone()).font(type_::BODY).color(t.text).wrap_anywhere())
            .child(text(format!("{} · {} · {state}", x.peer, size(x.total))).font(type_::LABEL).color(if failed { red() } else { t.text_secondary }));
        // The row a repeated send pointed at is outlined in the accent for a moment.
        let marked = s.flash.is_some_and(|(k, _)| k == x.key);
        let mut r = row().key(format!("t-{}", x.key)).w_full().px(space::S3).py(space::S2).gap(space::S3).center_y()
            .radius(radius::MD).fill(if marked { t.hover } else { t.card })
            .border(if marked { 2.0 } else { 1.0 }, if marked { tok::accent(0, true) } else { t.card_border }).child(badge).child(info);
        let key = x.key;
        if !x.finished {
            r = r.child(button(format!("t-cancel-{key}"), "취소", t, false, move |s, _| {
                if let Some(n) = &s.node {
                    n.cancel(key);
                }
            }));
        } else if !x.incoming && x.failed && s.peers.iter().any(|p| p.id == x.peer_id && p.connected) {
            // A cancelled or failed send can go again, from the start, as a new transfer whose
            // row takes this one's place: one row per file, showing its latest attempt.
            let (id, path) = (x.peer_id, x.path.clone());
            r = r.child(button(format!("t-retry-{key}"), "다시 보내기", t, false, move |s, _| {
                s.transfers.retain(|t| t.key != key);
                s.send(id, std::slice::from_ref(&path));
            }));
        } else if x.incoming && !x.failed {
            let path = x.path.clone();
            r = r.child(button(format!("t-show-{key}"), REVEAL, t, false, move |s, tx| {
                if let Err(e) = reveal(&path) {
                    s.say(format!("폴더를 열지 못했어요: {e}"), tx);
                }
            }));
        }
        tl = tl.child(r);
    }
    col().w_full().grow(1.0).min_h(150.0).gap(space::S2)
        .child(text(format!("전송 기록 {}", s.transfers.len())).font(type_::LABEL).color(t.text_secondary))
        .child(col().key("transfers").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(tl))
}

const REVEAL: &str = if cfg!(windows) { "탐색기에서 보기" } else { "Finder에서 보기" };

/// Shows `path` selected in Finder or Explorer.
fn reveal(path: &std::path::Path) -> std::io::Result<()> {
    // The Shell call reuses an Explorer window already showing the folder (`explorer /select`
    // opens a new one each time) and brings it to the front.
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        use windows_sys::Win32::UI::Shell::{ILFree, SHOpenFolderAndSelectItems, SHParseDisplayName};
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let fail = |hr: i32| std::io::Error::other(format!("HRESULT {:#010x}", hr as u32));
        // SAFETY: `wide` is NUL-terminated and outlives the calls; the PIDL the Shell allocates is
        // freed once, after its last use.
        unsafe {
            // The Shell needs COM on this thread; already initialised, in either model, is fine.
            CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
            let mut pidl = std::ptr::null_mut();
            let hr = SHParseDisplayName(wide.as_ptr(), std::ptr::null_mut(), &mut pidl, 0, std::ptr::null_mut());
            if hr < 0 {
                return Err(fail(hr));
            }
            // No child items: `pidl` names the file itself, selected in its folder.
            let hr = SHOpenFolderAndSelectItems(pidl, 0, std::ptr::null(), 0);
            ILFree(pidl);
            if hr < 0 { Err(fail(hr)) } else { Ok(()) }
        }
    }
    #[cfg(not(windows))]
    std::process::Command::new("open").arg("-R").arg(path).spawn().map(drop)
}

/// `~/Downloads` rather than the whole home path.
fn short_path(p: &std::path::Path) -> String {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    match home.as_deref().and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => p.display().to_string(),
    }
}

fn size(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b => format!("{} KB", b.div_ceil(1024)),
    }
}

fn demo(s: &mut Ui) {
    let peer = |n: u8, name: &str, paired, nearby, connected| PeerView { id: [n; 32], name: name.into(), addr: Some(([192, 168, 0, n], 47810).into()), paired, nearby, connected, kvm: connected };
    s.me = "MacBook Pro".into();
    s.peers = vec![peer(1, "Windows 노트북", true, true, true), peer(2, "작업실 PC", true, false, false), peer(3, "Mac mini", false, true, false)];
    s.code = Some("482913".into());
    s.pairing = Some(("Mac mini".into(), ([192, 168, 0, 3], 47810).into()));
    s.code_input = TextEdit::new("123 4");
    s.transfers = vec![
        Transfer { key: 4, peer_id: [1; 32], peer: "Windows 노트북".into(), file: "발표.key".into(), path: "발표.key".into(), incoming: false, done: 2 << 20, total: 30 << 20, finished: true, failed: true, cancelled: true },
        Transfer { key: 1, peer_id: [1; 32], peer: "Windows 노트북".into(), file: "보고서.pdf".into(), path: "보고서.pdf".into(), incoming: false, done: 3 << 20, total: 8 << 20, finished: false, failed: false, cancelled: false },
        Transfer { key: 2, peer_id: [1; 32], peer: "Windows 노트북".into(), file: "사진.zip".into(), path: "사진.zip".into(), incoming: true, done: 120 << 20, total: 120 << 20, finished: true, failed: false, cancelled: false },
        Transfer { key: 3, peer_id: [1; 32], peer: "Windows 노트북".into(), file: "영상.mov".into(), path: "영상.mov".into(), incoming: true, done: 40 << 20, total: 900 << 20, finished: true, failed: true, cancelled: false },
    ];
}

fn config_dir() -> PathBuf {
    let var = |k| std::env::var_os(k).map(PathBuf::from);
    if cfg!(windows) {
        var("APPDATA").unwrap_or_default().join("Aun")
    } else {
        var("HOME").unwrap_or_default().join("Library/Application Support/Aun")
    }
}

fn downloads_dir() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default();
    home.join("Downloads")
}

fn device_name() -> String {
    #[cfg(windows)]
    return std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into());
    #[cfg(not(windows))]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; gethostname NUL-terminates within it on success.
        let ok = unsafe { libc_gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
        let end = if ok { buf.iter().position(|&b| b == 0).unwrap_or(0) } else { 0 };
        let name = String::from_utf8_lossy(&buf[..end]).into_owned();
        let name = name.trim_end_matches(".local").to_owned();
        if name.is_empty() { "Mac".into() } else { name }
    }
}

#[cfg(not(windows))]
unsafe extern "C" {
    #[link_name = "gethostname"]
    fn libc_gethostname(name: *mut std::ffi::c_char, len: usize) -> std::ffi::c_int;
}

/// macOS, Dock icon hidden: Rune's default reopen does nothing for an accessory app, so a launch
/// from Finder or Spotlight opens the window here.
fn reopen(s: &mut Ui, visible: bool, _: &mut Tx) -> bool {
    if s.settings.dock || visible {
        return false;
    }
    if let Some(h) = &s.handle {
        h.open();
    }
    true
}

fn main() {
    if !rune::single_instance("dev.aun.desk") {
        return;
    }
    let args: Vec<String> = std::env::args().collect();
    let headless = args.iter().any(|a| a.starts_with("--snap") || a.starts_with("--tree") || a.starts_with("--script"));
    let dir = config_dir();
    let settings = if headless { Settings::default() } else { Settings::load(&dir) };
    let mut app = App::new(AppSpec::<Ui> {
        scenes: &[("demo", demo)],
        min_size: Some((420.0, 360.0)),
        poll: Some(|s, _, _| s.expire()),
        ..AppSpec::new("Aun", (480.0, 560.0), || Ui::new(None, None), view)
    })
    .keep_running()
    // Closed, the app is a few threads and sockets: give the GPU device back.
    .release_gpu_when_closed()
    .on_reopen(reopen);
    if !settings.dock {
        app = app.accessory();
    }
    if args.iter().any(|a| a == autostart::BACKGROUND) {
        app = app.windowless();
    }
    let node = if headless {
        None
    } else {
        // The login item names this executable: follow it if the app moved.
        if autostart::enabled() {
            let _ = autostart::set(true);
        }
        let h = app.handle();
        let cfg = Config { dir: dir.clone(), downloads: settings.downloads.clone().unwrap_or_else(downloads_dir), name: device_name(), port: aun_core::node::PORT, discovery: true, clipboard: settings.clipboard, secrets: settings.secrets, kvm: settings.kvm };
        let node = match Node::start(cfg, move |note| h.update(move |s: &mut Ui, tx| s.apply(note, tx))) {
            Ok(n) => Some(n),
            Err(e) => {
                aun_core::log(format_args!("start: {e}"));
                None
            }
        };
        // Runs on the main thread once the loop starts, window or not.
        let h = app.handle();
        let power = node.clone();
        app.handle().update(move |s, _| {
            if let Some(n) = power {
                aun_core::power::watch(n);
            }
            s.tray = tray::Tray::new(h.clone(), s.settings.clipboard);
            #[cfg(target_os = "macos")]
            {
                let h = h.clone();
                aun_core::kvm::on_input_source_change(move || h.update(|s: &mut Ui, _| s.input_source_changed()));
            }
            aun_core::log(format_args!("tray {}", if s.tray.is_some() { "up" } else { "unavailable" }));
            s.handle = Some(h);
        });
        node
    };
    let dir = (!headless).then_some(dir);
    app.init(move || Ui::new(node, dir)).run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Key, KeyEvent, Runtime};

    #[test]
    fn a_clicked_code_field_takes_typed_digits() {
        let mut ui = Ui::new(None, None);
        ui.peers = vec![PeerView { id: [3; 32], name: "PC".into(), addr: Some(([10, 0, 0, 3], 47810).into()), paired: false, nearby: true, connected: false, kvm: false }];
        ui.pairing = Some(("PC".into(), ([10, 0, 0, 3], 47810).into()));
        let mut rt = Runtime::new(ui, view, (480.0, 560.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        // Anywhere in the box, not only on the placeholder's glyphs.
        let (cx, cy) = rt.find("code-field").expect("code field");
        for (i, x) in [cx - 54.0, cx, cx + 54.0].into_iter().enumerate() {
            rt.pointer_down(x, cy);
            rt.pointer_up(x, cy);
            rt.frame(1.0 / 60.0);
            assert!(rt.has_focus(), "a click at {x} (box centre {cx}) focuses the field");
            rt.key(KeyEvent { key: Key::Text(i.to_string()), mods: Default::default() });
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(rt.state.code_input.text, "012");
        rt.key(KeyEvent { key: Key::Text("a가-".into()), mods: Default::default() });
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.code_input.text, "012", "letters are dropped");
        rt.key(KeyEvent { key: Key::Text("34567".into()), mods: Default::default() });
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.code_input.text, "012 345", "and the code stops at six digits, grouped 3 + 3");
        rt.key(KeyEvent { key: Key::Backspace, mods: Default::default() });
        rt.key(KeyEvent { key: Key::Backspace, mods: Default::default() });
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.code_input.text, "012 3");
        rt.key(KeyEvent { key: Key::Backspace, mods: Default::default() });
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.code_input.text, "012", "no trailing space once the second group is gone");
    }

    /// Hovering moves the pointer there, a drag holds the button while it moves (each point once,
    /// though the pad hears both the hover and the drag), and closing lets go.
    #[test]
    fn the_pad_moves_clicks_and_drags_the_peer_s_pointer() {
        let mut ui = Ui::new(None, None);
        ui.peers = vec![PeerView { id: [3; 32], name: "PC".into(), addr: None, paired: true, nearby: true, connected: true, kvm: true }];
        let mut rt = Runtime::new(ui, view, (480.0, 700.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        let (x, y) = rt.find("pad-030303030303").expect("the remote control button");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        let (cx, cy) = rt.find("pad-area").expect("the pad");
        // Rune hands the first moves over a just-shown pad to no handler (seen, cause not looked
        // into): settle on it first.
        for i in 0..3 {
            rt.pointer_move(cx - 10.0, cy);
            rt.frame(0.05 + i as f64 * 0.02);
        }
        rt.state.sent.borrow_mut().clear();
        for (i, px) in [cx, cx + 5.0, cx + 15.0].into_iter().enumerate() {
            rt.pointer_move(px, cy);
            rt.frame(0.12 + i as f64 * 0.02);
        }
        rt.pointer_down(cx + 15.0, cy);
        rt.pointer_move(cx + 15.0, cy + 10.0);
        rt.pointer_move(cx + 15.0, cy + 30.0);
        rt.pointer_up(cx + 15.0, cy + 30.0);
        rt.frame(0.2);
        rt.pointer_move(cx + 16.0, cy + 30.0);
        rt.pointer_move(cx + 18.0, cy + 30.0);
        rt.frame(0.3);
        // A slow scroll: fractions add up to a pixel before anything goes.
        rt.wheel_xy(cx, cy + 30.0, 0.0, 0.6);
        rt.frame(0.32);
        rt.wheel_xy(cx, cy + 30.0, 0.0, 0.6);
        rt.frame(0.34);
        let (x, y) = rt.find("pad-close").expect("the stop button");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.4);
        let sent: Vec<_> = rt.state.sent.borrow().iter().map(|f| match f {
            Frame::KvmMove { cx, cy, .. } => format!("to {cx},{cy}"),
            Frame::KvmButton { down, .. } => (if *down { "down" } else { "up" }).into(),
            f => format!("{f:?}"),
        }).collect::<Vec<_>>();
        // Points times the gain, as a running total.
        assert_eq!(sent, ["to 20,0", "to 30,0", "to 50,0", "down", "to 50,20", "to 50,60", "up", "to 54,60", "KvmWheel { dx: 0, dy: 1 }", "KvmLeave"]);
        assert!(rt.state.pad.is_none());
    }

    /// A click on the pad gives it the keyboard: keys go to the peer by where they are, and keys
    /// still held when focus goes are let go there. (That Tab then stays on the pad can't be
    /// seen headless: the harness sends no logical key behind a physical one. Checked on device.)
    #[test]
    fn the_pad_sends_keys_and_lets_go_when_it_loses_focus() {
        use rune::{PhysicalInput, PhysicalKeyCode as K};
        let mut ui = Ui::new(None, None);
        ui.peers = vec![PeerView { id: [3; 32], name: "PC".into(), addr: None, paired: true, nearby: true, connected: true, kvm: true }];
        ui.open_pad([3; 32], "PC".into());
        let mut rt = Runtime::new(ui, view, (480.0, 700.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        let (x, y) = rt.find("pad-area").expect("the pad");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.1);
        assert!(rt.has_focus(), "a click focuses the pad");
        rt.state.sent.borrow_mut().clear();
        let key = |key, pressed, repeat| PhysicalInput::Key { key, mods: Default::default(), pressed, repeat };
        for e in [key(K::ShiftLeft, true, false), key(K::KeyA, true, false), key(K::KeyA, true, true), key(K::KeyA, false, false), key(K::Tab, true, false)] {
            rt.physical_input(e);
        }
        rt.frame(0.2);
        rt.physical_input(PhysicalInput::FocusLost);
        rt.frame(0.3);
        let sent: Vec<_> = rt.state.sent.borrow().iter().map(|f| format!("{f:?}")).collect();
        let k = |hid: u16, down: bool| format!("{:?}", Frame::KvmKey { hid, down });
        assert_eq!(sent, [k(0xE1, true), k(4, true), k(4, true), k(4, false), k(0x2B, true), k(0x2B, false), k(0xE1, false)]);
    }

    /// ⌘C on a Mac pad is Ctrl+C at the peer (and Ctrl is ⌘ there), unless switched off. (The
    /// switch is a button, so the pad has let go of every key before it flips.)
    #[test]
    fn the_pad_sends_command_as_control_unless_switched_off() {
        use rune::{PhysicalInput, PhysicalKeyCode as K};
        let mut ui = Ui::new(None, None);
        ui.peers = vec![PeerView { id: [3; 32], name: "PC".into(), addr: None, paired: true, nearby: true, connected: true, kvm: true }];
        ui.open_pad([3; 32], "PC".into());
        let mut rt = Runtime::new(ui, view, (480.0, 700.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        let (x, y) = rt.find("pad-area").expect("the pad");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.1);
        rt.state.sent.borrow_mut().clear();
        let key = |key, pressed| PhysicalInput::Key { key, mods: Default::default(), pressed, repeat: false };
        for e in [key(K::SuperLeft, true), key(K::KeyC, true), key(K::KeyC, false), key(K::SuperLeft, false), key(K::ControlRight, true), key(K::ControlRight, false)] {
            rt.physical_input(e);
        }
        rt.frame(0.2);
        rt.state.settings.swap = false;
        rt.physical_input(key(K::SuperRight, true));
        rt.frame(0.3);
        let sent: Vec<_> = rt.state.sent.borrow().iter().map(|f| format!("{f:?}")).collect();
        let k = |hid: u16, down: bool| format!("{:?}", Frame::KvmKey { hid, down });
        assert_eq!(sent, [k(0xE0, true), k(6, true), k(6, false), k(0xE0, false), k(0xE7, true), k(0xE7, false), k(0xE7, true)]);
    }

    /// The menu bar goes while ⌘ on this keyboard is down on the pad (sent as Ctrl or not), and
    /// only then: Control is not ⌘, and letting go or losing focus brings it back.
    #[test]
    fn command_held_on_the_pad_is_this_keyboard_s_command() {
        use rune::{PhysicalInput, PhysicalKeyCode as K};
        let key = |key, pressed| PhysicalInput::Key { key, mods: Default::default(), pressed, repeat: false };
        for swap in [true, false] {
            let mut ui = Ui::new(None, None);
            ui.settings.swap = swap;
            ui.open_pad([3; 32], "PC".into());
            ui.pad_key(&key(K::ControlLeft, true));
            assert!(!ui.command_held(), "Control (swap {swap})");
            ui.pad_key(&key(K::SuperRight, true));
            assert!(ui.command_held(), "right ⌘ (swap {swap})");
            ui.pad_key(&key(K::SuperRight, false));
            assert!(!ui.command_held(), "⌘ up (swap {swap})");
            ui.pad_key(&key(K::SuperLeft, true));
            ui.pad_key(&PhysicalInput::FocusLost);
            assert!(!ui.command_held(), "focus lost (swap {swap})");
        }
    }

    /// Keys that are 한/영 or 한자 on this keyboard go to the peer as those: a Mac's input source
    /// key (a Caps Lock remapped to F18), a Korean PC layout's right Ctrl (한자). The ⌘/Ctrl swap
    /// doesn't turn 한자 into ⌘.
    #[test]
    fn the_keyboard_s_hangul_and_hanja_keys_go_as_those() {
        use rune::{PhysicalInput, PhysicalKeyCode as K};
        let key = |key, pressed| PhysicalInput::Key { key, mods: Default::default(), pressed, repeat: false };
        let mut ui = Ui::new(None, None);
        ui.open_pad([3; 32], "PC".into());
        ui.pad.as_mut().unwrap().lang = vec![(K::F18, 0x90), (K::ControlRight, 0x91)];
        for e in [key(K::F18, true), key(K::F18, false), key(K::ControlRight, true), key(K::ControlRight, false), key(K::F19, true)] {
            ui.pad_key(&e);
        }
        let k = |hid, down| Frame::KvmKey { hid, down };
        assert_eq!(*ui.sent.borrow(), [k(0x90, true), k(0x90, false), k(0x91, true), k(0x91, false), k(0x6E, true)]);
    }

    /// A Mac never shows an app the key that switches its input source (Caps Lock remapped to
    /// F18 went to the system only, 2026-11-22), so the switch itself is sent as 한/영: once per
    /// switch while the pad has the keyboard, never after it lost it.
    #[test]
    fn an_input_source_switch_on_the_focused_pad_goes_as_hangul() {
        use rune::PhysicalInput;
        let mut ui = Ui::new(None, None);
        ui.open_pad([3; 32], "PC".into());
        ui.input_source_changed(); // not focused yet
        ui.pad_key(&PhysicalInput::Key { key: rune::PhysicalKeyCode::KeyA, mods: Default::default(), pressed: true, repeat: false });
        ui.sent.borrow_mut().clear();
        ui.input_source_changed();
        ui.pad_key(&PhysicalInput::FocusLost);
        ui.sent.borrow_mut().clear();
        ui.input_source_changed();
        assert!(ui.sent.borrow().is_empty(), "after focus went");
        std::thread::sleep(SAME_SWITCH);
        ui.pad_key(&PhysicalInput::Modifiers(Default::default())); // the keyboard is back
        ui.input_source_changed();
        ui.input_source_changed(); // macOS announced the same switch twice, 3-7 ms apart
        let k = |hid, down| Frame::KvmKey { hid, down };
        assert_eq!(*ui.sent.borrow(), [k(0x90, true), k(0x90, false)]);
        std::thread::sleep(SAME_SWITCH);
        ui.input_source_changed();
        assert_eq!(ui.sent.borrow().len(), 4, "a switch after that is another one");
    }

    /// ⌃Space set to switch the Mac's input source reaches the pad as Control and Space too: the
    /// Space stays here and goes as 한/영 at once, while Control (Win at the PC) is still down,
    /// whichever key comes up first. (macOS switches when Control comes up: let go first, a 한/영
    /// sent then came after Win and the PC opened Start.) The switch announced after it is the
    /// same one.
    #[test]
    fn the_input_source_shortcut_goes_as_hangul_while_its_modifier_is_down() {
        use rune::{PhysicalInput, PhysicalKeyCode as K};
        let key = |key, pressed, ctrl| PhysicalInput::Key { key, mods: rune::Mods { ctrl, ..Default::default() }, pressed, repeat: false };
        let mut ui = Ui::new(None, None);
        ui.open_pad([3; 32], "PC".into());
        ui.pad.as_mut().unwrap().combos = vec![(K::Space, 0x4_0000)];
        ui.pad_key(&key(K::ControlLeft, true, true));
        ui.pad_key(&key(K::Space, true, true));
        ui.pad_key(&key(K::ControlLeft, false, false));
        ui.input_source_changed(); // macOS switches now
        ui.pad_key(&key(K::Space, false, false));
        ui.pad_key(&key(K::Space, true, false)); // a plain Space
        let k = |hid, down| Frame::KvmKey { hid, down };
        assert_eq!(*ui.sent.borrow(), [k(0xE3, true), k(0x90, true), k(0x90, false), k(0xE3, false), k(0x2C, true)]);
    }

    /// Shift held on the pad, then a click on another control: the pad loses the keyboard, and
    /// the Shift held at the peer must go up then, not whenever the pad closes.
    #[test]
    fn a_click_elsewhere_lets_go_of_keys_held_on_the_pad() {
        use rune::{PhysicalInput, PhysicalKeyCode as K};
        let mut ui = Ui::new(None, None);
        ui.peers = vec![PeerView { id: [3; 32], name: "PC".into(), addr: None, paired: true, nearby: true, connected: true, kvm: true }];
        ui.open_pad([3; 32], "PC".into());
        let mut rt = Runtime::new(ui, view, (480.0, 700.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        let (x, y) = rt.find("pad-area").expect("the pad");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.1);
        rt.physical_input(PhysicalInput::Key { key: K::ShiftLeft, mods: Default::default(), pressed: true, repeat: false });
        rt.frame(0.2);
        rt.state.sent.borrow_mut().clear();
        // The clipboard switch: safe headless (no node, no settings folder).
        let (x, y) = rt.find("clip").expect("the clipboard switch");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.3);
        assert_eq!(*rt.state.sent.borrow(), [Frame::KvmKey { hid: 0xE1, down: false }]);
    }

    #[test]
    fn the_clipboard_switch_is_remembered_across_restarts() {
        let dir = std::env::temp_dir().join(format!("aun-ui-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut rt = Runtime::new(Ui::new(None, Some(dir.clone())), view, (480.0, 560.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        assert!(rt.state.settings.clipboard, "on by default");
        let (x, y) = rt.find("clip").expect("clipboard button");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        assert!(!rt.state.settings.clipboard);
        assert!(!Ui::new(None, Some(dir.clone())).settings.clipboard, "the next start reads it back");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn transfers_past_the_window_bottom_scroll_into_view() {
        let mut ui = Ui::new(None, None);
        demo(&mut ui);
        let mut rt = Runtime::new(ui, view, (480.0, 560.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        // The window's content ends at its bottom padding; the list is clipped there.
        let bottom = 560.0 - space::S6;
        let (_, y) = rt.find("t-3").expect("the last transfer row");
        assert!(y > bottom, "below the visible list to begin with: {y}");
        let (lx, ly) = rt.find("transfers").expect("the transfer list");
        for i in 0..30 {
            rt.wheel_xy(lx, ly, 0.0, -40.0);
            rt.frame(i as f64 / 60.0 + 0.1);
        }
        let (_, y) = rt.find("t-3").unwrap();
        assert!(y < bottom, "scrolled into view: {y}");
    }

    fn finished(key: u64, file: String) -> Transfer {
        Transfer { key, peer_id: [1; 32], peer: "Windows 노트북".into(), path: file.clone().into(), file, incoming: true, done: 1, total: 1, finished: true, failed: false, cancelled: false }
    }

    /// A dropped pile of files: every row stays reachable, the oldest at the bottom.
    #[test]
    fn every_transfer_of_a_pile_can_be_scrolled_to() {
        let mut ui = Ui::new(None, None);
        ui.transfers = (1..=30).rev().map(|k| finished(k, format!("파일 {k}.jpeg"))).collect();
        let mut rt = Runtime::new(ui, view, (480.0, 560.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        let header = rt.find("clip").expect("the clipboard switch");
        let (lx, ly) = rt.find("transfers").expect("the transfer list");
        for i in 0..200 {
            rt.wheel_xy(lx, ly, 0.0, -40.0);
            rt.frame(i as f64 / 60.0 + 0.1);
        }
        let (_, y) = rt.find("t-1").expect("the oldest of 30 rows");
        assert!(y < 560.0, "scrolled into view: {y}");
        assert_eq!(rt.find("clip"), Some(header), "only the list scrolls; the controls above stay put");
    }

    /// A long file name wraps inside the window instead of pushing the row's button off it.
    #[test]
    fn a_long_file_name_keeps_its_row_inside_the_window() {
        let mut ui = Ui::new(None, None);
        let long = "아주_긴_파일_이름_".repeat(12) + "끝.mov";
        ui.transfers = vec![finished(1, long)];
        let mut rt = Runtime::new(ui, view, (480.0, 560.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        let (x, _) = rt.find("t-show-1").expect("the reveal button");
        assert!(x < 480.0, "the button is inside the window: {x}");
    }

    /// A connected device has three buttons, and its open pad two; at the narrowest window none of
    /// them is cut off.
    #[test]
    fn a_connected_device_s_buttons_stay_inside_the_window() {
        for w in [420.0, 480.0] {
            let mut ui = Ui::new(None, None);
            ui.peers = vec![PeerView { id: [3; 32], name: "Kibeoms-Mac-mini".into(), addr: None, paired: true, nearby: true, connected: true, kvm: true }];
            ui.open_pad([3; 32], "Kibeoms-Mac-mini".into());
            let mut rt = Runtime::new(ui, view, (w, 900.0), Env { system_dark: false, scale: 2.0 });
            rt.frame(0.0);
            for key in ["send-030303030303", "pad-030303030303", "forget-030303030303", "pad-swap", "pad-close"] {
                let (r, _) = rt.presented(key).unwrap_or_else(|| panic!("{key}"));
                assert!(r[0] + r[2] <= w - 24.0, "{key} ends at {} in a {w} pt window", r[0] + r[2]);
            }
        }
    }

    #[test]
    fn a_typed_address_opens_the_code_box_for_it() {
        let mut rt = Runtime::new(Ui::new(None, None), view, (480.0, 560.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        assert!(rt.find("pair-box").is_none());
        let (x, y) = rt.find("ip-field").expect("address field");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        rt.key(KeyEvent { key: Key::Text("100.98.146.73".into()), mods: Default::default() });
        rt.frame(1.0 / 60.0);
        let (x, y) = rt.find("ip-go").unwrap();
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.pairing, Some(("100.98.146.73".into(), ([100, 98, 146, 73], aun_core::node::PORT).into())));
        assert!(rt.find("pair-box").is_some());
    }

    #[test]
    fn typed_addresses_take_aun_s_port_unless_given() {
        assert_eq!(parse_addr(" 192.168.0.5 "), Some(([192, 168, 0, 5], aun_core::node::PORT).into()));
        assert_eq!(parse_addr("10.0.0.2:5000"), Some(([10, 0, 0, 2], 5000).into()));
        assert_eq!(parse_addr("my-pc"), None);
        assert_eq!(parse_addr("192.168.0"), None);
    }

    /// A send the user cancelled (or that failed) can go again while the device is connected;
    /// a received file can't be asked for from this side.
    /// A status message about a long file name wraps inside the window instead of being cut
    /// at its edge.
    #[test]
    fn a_long_status_message_stays_inside_the_window() {
        let mut rt = Runtime::new(Ui::new(None, None), view, (480.0, 560.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        let name = "「vol.1」 열도를 넘어 세계로 | ONE OK ROCK 라이브 모음 1탄 (full ver.) [가사해석].mp4";
        rt.state.message = Some((format!("이미 보내는 중이에요: {name}"), Instant::now() + MESSAGE_FOR));
        rt.invalidate();
        rt.frame(1.0 / 60.0);
        let long = rt.presented("message").expect("the message").0;
        rt.state.message = Some(("짧은 안내".into(), Instant::now() + MESSAGE_FOR));
        rt.invalidate();
        // Let the height change settle (it moves on a layout spring).
        for i in 2..60 {
            rt.frame(i as f64 / 60.0);
        }
        let short = rt.presented("message").unwrap().0;
        // presented: [x, y, width, height]. Cut to one line, the long one is no taller.
        assert!(long[3] > short[3] * 1.5, "the long message wraps onto more lines: {long:?} vs {short:?}");
    }

    /// The node saying the driven peer went silent closes that peer's pad (letting go there) and
    /// says why; the same note about another peer leaves the pad open.
    #[test]
    fn a_silent_peer_closes_its_pad() {
        let mut ui = Ui::new(None, None);
        ui.peers = vec![PeerView { id: [3; 32], name: "PC".into(), addr: None, paired: true, nearby: true, connected: true, kvm: true }];
        ui.open_pad([3; 32], "PC".into());
        let mut rt = Runtime::new(ui, view, (480.0, 760.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        rt.update(|s, tx| s.apply(Note::InputLost([4; 32]), tx));
        assert!(rt.state.pad.is_some(), "another peer's note");
        rt.update(|s, tx| s.apply(Note::InputLost([3; 32]), tx));
        assert!(rt.state.pad.is_none());
        assert_eq!(rt.state.sent.borrow().last(), Some(&Frame::KvmLeave));
        assert!(rt.state.message.as_ref().is_some_and(|(m, _)| m.contains("응답하지 않아")));
    }

    /// Sending a file that is already going: a banner says so, the row it would repeat is
    /// marked for a moment, and the banner closes on its ✕.
    #[test]
    fn a_repeated_send_points_at_the_transfer_already_going() {
        let mut ui = Ui::new(None, None);
        demo(&mut ui);
        let mut rt = Runtime::new(ui, view, (480.0, 760.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        rt.update(|s, tx| s.apply(Note::AlreadySending { key: 1, file: "보고서.pdf".into() }, tx));
        rt.frame(1.0 / 60.0);
        assert!(rt.state.message.as_ref().is_some_and(|(m, _)| m.contains("이미 보내는 중이에요: 보고서.pdf")));
        assert_eq!(rt.state.flash.map(|f| f.0), Some(1), "the transfer already going is marked");
        let (x, y) = rt.find("message-close").expect("the banner's close button");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(2.0 / 60.0);
        assert!(rt.state.message.is_none() && rt.find("message").is_none(), "closed");
        rt.state.flash.as_mut().unwrap().1 = Instant::now() - Duration::from_millis(1);
        assert!(rt.state.expire(), "the mark goes on its own");
        assert!(rt.state.flash.is_none());
    }

    /// Files copied in Finder or Explorer are offered for each connected device; folders are left
    /// out and said so, sending or closing takes the offer down, and a copy of folders only offers nothing.
    #[test]
    fn copied_files_are_offered_to_connected_devices() {
        let d = std::env::temp_dir().join(format!("aun-ui-copied-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("폴더")).unwrap();
        std::fs::write(d.join("a.txt"), b"a").unwrap();
        std::fs::write(d.join("b.txt"), b"b").unwrap();
        let mut ui = Ui::new(None, None);
        demo(&mut ui);
        let mut rt = Runtime::new(ui, view, (480.0, 760.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        assert!(rt.find("copied").is_none());
        let copy = |rt: &mut Runtime<Ui>, paths: Vec<PathBuf>| {
            rt.update(|s, tx| s.apply(Note::Copied(paths), tx));
            rt.frame(1.0 / 60.0);
        };
        copy(&mut rt, vec![d.join("a.txt"), d.join("폴더"), d.join("b.txt")]);
        assert_eq!(rt.state.copied, vec![d.join("a.txt"), d.join("b.txt")]);
        assert!(rt.find("copied-text").is_some(), "the offer");
        let words = copied_text(&rt.state);
        assert!(words.contains("파일 2개") && words.contains("폴더 1개"), "{words}");
        // Only the connected device (Windows 노트북) gets a button.
        let k = aun_core::hex(&[1u8; 32][..6]);
        assert!(rt.find(&format!("copied-send-{}", aun_core::hex(&[2u8; 32][..6]))).is_none(), "an offline device");
        let (x, y) = rt.find(&format!("copied-send-{k}")).expect("send to the connected device");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(2.0 / 60.0);
        assert!(rt.state.copied.is_empty() && rt.find("copied").is_none(), "sent: the offer goes");

        copy(&mut rt, vec![d.join("a.txt")]);
        let (x, y) = rt.find("copied-close").expect("close");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(3.0 / 60.0);
        assert!(rt.find("copied").is_none(), "closed");

        copy(&mut rt, vec![d.join("폴더")]);
        assert!(rt.find("copied").is_none(), "folders alone: nothing to offer");
        std::fs::remove_dir_all(d).unwrap();
    }

    /// The row a repeated send points at may sit below the visible list: it is scrolled to.
    #[test]
    fn the_marked_transfer_is_scrolled_into_view() {
        let mut ui = Ui::new(None, None);
        demo(&mut ui);
        let mut rt = Runtime::new(ui, view, (480.0, 560.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        let bottom = 560.0 - space::S6;
        assert!(rt.find("t-3").unwrap().1 > bottom, "below the visible list to begin with");
        rt.update(|s, tx| s.apply(Note::AlreadySending { key: 3, file: "사진.heic".into() }, tx));
        for i in 1..60 {
            rt.frame(i as f64 / 60.0);
        }
        let (_, y) = rt.find("t-3").unwrap();
        assert!(y < bottom, "scrolled into view: {y}");
    }

    #[test]
    fn a_stopped_send_offers_to_send_it_again() {
        let mut ui = Ui::new(None, None);
        demo(&mut ui);
        let mut rt = Runtime::new(ui, view, (480.0, 760.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        assert!(rt.find("t-retry-4").is_some(), "the cancelled send to a connected device");
        assert!(rt.find("t-retry-3").is_none(), "a failed receive");
        assert!(rt.find("t-retry-2").is_none(), "a finished receive");
        // Sending it again replaces the stopped row: one row per file, its latest attempt.
        let (x, y) = rt.find("t-retry-4").unwrap();
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        assert!(rt.find("t-4").is_none(), "the cancelled row goes once it is sent again");
        rt.state.peers.iter_mut().for_each(|p| p.connected = false);
        rt.invalidate();
        rt.frame(1.0 / 60.0);
        assert!(rt.find("t-retry-4").is_none(), "nowhere to send it while offline");
    }

    #[test]
    fn progress_rings_fill_with_the_transfer() {
        assert_eq!(progress_arc(0.0).body, "", "nothing sent: no arc");
        assert!(progress_arc(1.0).body.starts_with("<circle"), "all sent: the whole ring");
        let half = progress_arc(0.5).body;
        assert!(half.contains("A11 11 0 0 1 12.000 23.000"), "half ends at 6 o'clock: {half}");
        assert!(progress_arc(0.75).body.contains("A11 11 0 1 1"), "past half takes the large arc");
        assert!(std::ptr::eq(progress_arc(0.371), progress_arc(0.379)), "2% steps share one icon");
        assert!(std::ptr::eq(progress_arc(-1.0), progress_arc(0.0)) && std::ptr::eq(progress_arc(7.0), progress_arc(1.0)), "clamped");
    }

    #[test]
    fn device_names_are_accepted_alongside_addresses() {
        assert_eq!(parse_name(" kibeom-windows "), Some(("kibeom-windows".into(), aun_core::node::PORT)));
        assert_eq!(parse_name("kibeom-mac.tail7efa79.ts.net:5000"), Some(("kibeom-mac.tail7efa79.ts.net".into(), 5000)));
        assert_eq!(parse_name("my pc"), None, "no spaces in a host name");
        assert_eq!(parse_name(""), None);
        assert_eq!(parse_name("-pc"), None);
        assert_eq!(parse_name("192.168.0"), None, "a mistyped IP is not a name");
        assert_eq!(resolve("localhost", 47810).unwrap(), SocketAddr::from(([127, 0, 0, 1], 47810)), "names resolve to IPv4");
        assert!(resolve("no-such-host.invalid", 47810).is_err());
    }

    #[test]
    fn a_status_message_goes_away_on_its_own() {
        let mut rt = Runtime::new(Ui::new(None, None), view, (480.0, 560.0), Env { system_dark: false, scale: 2.0 });
        // A cold first frame took 15 s on a Windows CI runner: start the message's clock after it.
        rt.frame(0.0);
        rt.state.message = Some(("연결된 기기가 아니에요".into(), Instant::now() + MESSAGE_FOR));
        rt.invalidate();
        rt.frame(1.0 / 60.0);
        assert!(rt.find("message").is_some(), "a fresh message shows");
        assert!(!rt.state.expire(), "and stays until its time");
        rt.state.message.as_mut().unwrap().1 = Instant::now() - Duration::from_millis(1);
        assert!(rt.state.expire(), "past its time, poll drops it and asks for a new view");
        rt.invalidate(); // what poll's `true` does in the app
        rt.frame(1.0 / 60.0);
        assert!(rt.find("message").is_none(), "an old one is gone");
    }
}
