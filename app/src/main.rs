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

use aun_core::{Config, Id, Node, Note, PeerView};
use settings::Settings;
use rune::tok::{self, Theme, radius, space, type_};
use rune::{App, AppHandle, AppSpec, Cx, El, FileDialogOptions, ScrollSpec, TextEdit, Tx, col, el, row, text, text_input};

struct Transfer {
    key: u64,
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

struct Ui {
    node: Option<Node>,
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
            node, me, peers: vec![], code: None, pairing: None, ip_input: TextEdit::new(""), code_input: TextEdit::new(""), transfers: vec![], message: None, over: None,
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
    }

    fn say(&mut self, m: impl Into<String>, tx: &mut Tx) {
        self.message = Some((m.into(), Instant::now() + MESSAGE_FOR));
        tx.wake_after(MESSAGE_FOR); // runs `poll` then, which drops it
    }

    /// Drops a status message past its time; true when the view must re-run.
    fn expire(&mut self) -> bool {
        let old = self.message.as_ref().is_some_and(|(_, until)| *until <= Instant::now());
        if old {
            self.message = None;
        }
        old
    }

    fn apply(&mut self, note: Note, tx: &mut Tx) {
        match note {
            Note::Changed => self.refresh(),
            Note::Info(m) => {
                self.say(m, tx);
                self.refresh();
            }
            Note::Transfer { key, peer, file, path, done, total, incoming, finished, failed, cancelled } => {
                match self.transfers.iter_mut().find(|t| t.key == key) {
                    Some(t) => (t.done, t.finished, t.failed, t.cancelled) = (done, finished, failed, cancelled),
                    None => self.transfers.insert(0, Transfer { key, peer, file, path, incoming, done, total, finished, failed, cancelled }),
                }
                // Keep every unfinished one (a dropped pile waits in line) and the latest 100 done:
                // a big pile stays checkable, at about 100 bytes a row.
                let mut kept_done = 0;
                self.transfers.retain(|t| !t.finished || { kept_done += 1; kept_done <= 100 });
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
    let info = col().grow(1.0).gap(2.0)
        .child(text(p.name.clone()).font(type_::BODY).color(t.text))
        .child(text(status(p)).font(type_::LABEL).color(t.text_secondary));
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
    let mut prefs = row().key("prefs").w_full().gap(space::S2)
        .child(button("autostart".into(), if s.autostart { "로그인 시 실행 켜짐" } else { "로그인 시 실행 꺼짐" }, t, s.autostart, |s, tx| {
            s.set_autostart(!s.autostart, tx)
        }))
        .child(button("secrets".into(), if s.settings.secrets { "비밀번호 공유 켜짐" } else { "비밀번호 공유 꺼짐" }, t, s.settings.secrets, |s, tx| {
            s.set_secrets(!s.settings.secrets, tx)
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
    if let Some((m, _)) = &s.message {
        controls = controls.child(text(m.clone()).key("message").font(type_::LABEL).color(t.text_secondary));
    }
    let mut root = col().key("root").w_full().h_full().padding(space::S6).gap(space::S3).fill(t.bg).child(controls);
    if !s.transfers.is_empty() {
        root = root.child(transfers(s, t));
    }
    root
}

/// Direction arrows for the transfer history: a long shaft and an open head, drawn in the
/// element's colour.
static ARROW_DOWN: rune::Icon = rune::Icon {
    name: "aun-arrow-down",
    view: 24.0,
    body: r#"<path d="M12 1.5V22.5M5 15.5L12 22.5L19 15.5" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
static ARROW_UP: rune::Icon = rune::Icon {
    name: "aun-arrow-up",
    view: 24.0,
    body: r#"<path d="M12 22.5V1.5M5 8.5L12 1.5L19 8.5" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

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
        let (arrow, way, tint) = if x.incoming { (&ARROW_DOWN, "받음", tok::accent(4, true)) } else { (&ARROW_UP, "보냄", tok::accent(0, true)) };
        // From the top of the file name to the bottom of the details line.
        let badge = rune::icon(arrow, 32.0).color(tint).label(way);
        let info = col().grow(1.0).shrink().gap(2.0)
            .child(text(x.file.clone()).font(type_::BODY).color(t.text).wrap_anywhere())
            .child(text(format!("{} · {} · {state}", x.peer, size(x.total))).font(type_::LABEL).color(if x.failed { tok::accent(2, true) } else { t.text_secondary }));
        let mut r = row().key(format!("t-{}", x.key)).w_full().px(space::S3).py(space::S2).gap(space::S3).center_y()
            .radius(radius::MD).fill(t.card).border(1.0, t.card_border).child(badge).child(info);
        let key = x.key;
        if !x.finished {
            r = r.child(button(format!("t-cancel-{key}"), "취소", t, false, move |s, _| {
                if let Some(n) = &s.node {
                    n.cancel(key);
                }
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
    let peer = |n: u8, name: &str, paired, nearby, connected| PeerView { id: [n; 32], name: name.into(), addr: Some(([192, 168, 0, n], 47810).into()), paired, nearby, connected };
    s.me = "MacBook Pro".into();
    s.peers = vec![peer(1, "Windows 노트북", true, true, true), peer(2, "작업실 PC", true, false, false), peer(3, "Mac mini", false, true, false)];
    s.code = Some("482913".into());
    s.pairing = Some(("Mac mini".into(), ([192, 168, 0, 3], 47810).into()));
    s.code_input = TextEdit::new("123 4");
    s.transfers = vec![
        Transfer { key: 1, peer: "Windows 노트북".into(), file: "보고서.pdf".into(), path: "보고서.pdf".into(), incoming: false, done: 3 << 20, total: 8 << 20, finished: false, failed: false, cancelled: false },
        Transfer { key: 2, peer: "Windows 노트북".into(), file: "사진.zip".into(), path: "사진.zip".into(), incoming: true, done: 120 << 20, total: 120 << 20, finished: true, failed: false, cancelled: false },
        Transfer { key: 3, peer: "Windows 노트북".into(), file: "영상.mov".into(), path: "영상.mov".into(), incoming: true, done: 40 << 20, total: 900 << 20, finished: true, failed: true, cancelled: true },
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
        let cfg = Config { dir: dir.clone(), downloads: settings.downloads.clone().unwrap_or_else(downloads_dir), name: device_name(), port: aun_core::node::PORT, discovery: true, clipboard: settings.clipboard, secrets: settings.secrets };
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
        ui.peers = vec![PeerView { id: [3; 32], name: "PC".into(), addr: Some(([10, 0, 0, 3], 47810).into()), paired: false, nearby: true, connected: false }];
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
        Transfer { key, peer: "Windows 노트북".into(), path: file.clone().into(), file, incoming: true, done: 1, total: 1, finished: true, failed: false, cancelled: false }
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
