//! Aun: one desk across a Mac and a Windows PC. Pairs with a 6-digit code, then keeps clipboard
//! text in sync and takes files dropped on a device.
//!
//! `cargo run -p aun` opens the window. `-- --snap aun.png --scene demo` renders it headless.
//! Closing the window keeps sync running; launching again (or the Dock icon) brings it back.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long a status message stays up.
const MESSAGE_FOR: Duration = Duration::from_secs(5);

use aun_core::{Config, Id, Node, Note, PeerView};
use rune::tok::{self, Theme, radius, space, type_};
use rune::{App, AppSpec, Cx, El, FileDialogOptions, TextEdit, Tx, col, el, row, text, text_input};

struct Transfer {
    key: u64,
    label: String,
    done: u64,
    total: u64,
    finished: bool,
    failed: bool,
}

struct Ui {
    node: Option<Node>,
    me: String,
    peers: Vec<PeerView>,
    code: Option<String>,
    /// The nearby device a code is being typed for, and the typing.
    pairing: Option<Id>,
    code_input: TextEdit,
    transfers: Vec<Transfer>,
    /// A status line and when it goes away.
    message: Option<(String, Instant)>,
    clip: bool,
    over: Option<Id>,
}

impl Ui {
    fn new(node: Option<Node>) -> Ui {
        let me = node.as_ref().map_or_else(|| "시작하지 못했어요".into(), |n| n.name().to_owned());
        let mut ui = Ui { node, me, peers: vec![], code: None, pairing: None, code_input: TextEdit::new(""), transfers: vec![], message: None, clip: true, over: None };
        ui.refresh();
        ui
    }

    fn refresh(&mut self) {
        if let Some(n) = &self.node {
            self.peers = n.peers();
            self.code = n.code();
            self.clip = n.clipboard();
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
            Note::Transfer { key, peer, file, done, total, incoming, finished, failed } => {
                let label = if incoming { format!("{peer} → {file}") } else { format!("{file} → {peer}") };
                match self.transfers.iter_mut().find(|t| t.key == key) {
                    Some(t) => (t.done, t.finished, t.failed) = (done, finished, failed),
                    None => self.transfers.insert(0, Transfer { key, label, done, total, finished, failed }),
                }
                self.transfers.truncate(6);
            }
        }
    }

    fn send(&mut self, id: Id, files: &[PathBuf]) {
        if let Some(n) = &self.node {
            for f in files.iter().filter(|f| f.is_file()) {
                n.send_file(id, f.clone());
            }
        }
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
    } else if p.addr.is_some() && s.pairing != Some(id) {
        r = r.child(button(format!("pair-{k}"), "페어링", t, true, move |s, _| {
            s.pairing = Some(id);
            s.code_input = TextEdit::new("");
        }));
    }
    r
}

fn pair_box(s: &Ui, t: Theme) -> Option<El> {
    let id = s.pairing?;
    let name = s.peers.iter().find(|p| p.id == id).map_or("기기", |p| p.name.as_str());
    let submit = |s: &mut Ui, tx: &mut Tx| {
        let Some(id) = s.pairing.take() else { return };
        let code: String = s.code_input.text.chars().filter(char::is_ascii_digit).collect();
        let addr = s.peers.iter().find(|p| p.id == id).and_then(|p| p.addr);
        match (&s.node, addr, code.len()) {
            (Some(n), Some(addr), 6) => {
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

fn view(s: &Ui, cx: &mut Cx<Ui>) -> El {
    let t = tok::theme(cx.system_dark());
    let header = row().w_full().center_y().gap(space::S3)
        .child(col().grow(1.0).gap(2.0)
            .child(text("Aun").font(type_::TITLE).color(t.text))
            .child(text(format!("이 기기: {}", s.me)).font(type_::LABEL).color(t.text_secondary)))
        .child(button("clip".into(), if s.clip { "클립보드 공유 켜짐" } else { "클립보드 공유 꺼짐" }, t, s.clip, |s, _| {
            if let Some(n) = &s.node {
                n.set_clipboard(!n.clipboard());
            }
            s.refresh();
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

    let mut root = col().key("root").w_full().h_full().padding(space::S6).gap(space::S3).fill(t.bg)
        .child(header).child(code).child(list);
    if let Some(b) = pair_box(s, t) {
        root = root.child(b);
    }
    if !s.transfers.is_empty() {
        let mut tl = col().key("transfers").w_full().gap(space::S1);
        for x in &s.transfers {
            let pct = (x.done * 100).checked_div(x.total).unwrap_or(100);
            let state = if x.failed { "실패".to_owned() } else if x.finished { "완료".into() } else { format!("{pct}%") };
            tl = tl.child(row().key(format!("t-{}", x.key)).w_full().gap(space::S2)
                .child(text(x.label.clone()).font(type_::LABEL).color(t.text).grow(1.0))
                .child(text(format!("{state} · {}", size(x.total))).font(type_::LABEL).color(t.text_secondary)));
        }
        root = root.child(tl);
    }
    if let Some((m, _)) = &s.message {
        root = root.child(text(m.clone()).key("message").font(type_::LABEL).color(t.text_secondary));
    }
    root
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
    s.pairing = Some([3; 32]);
    s.code_input = TextEdit::new("123 4");
    s.transfers = vec![
        Transfer { key: 1, label: "보고서.pdf → Windows 노트북".into(), done: 3 << 20, total: 8 << 20, finished: false, failed: false },
        Transfer { key: 2, label: "Windows 노트북 → 사진.zip".into(), done: 120 << 20, total: 120 << 20, finished: true, failed: false },
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

fn main() {
    if !rune::single_instance("dev.aun.desk") {
        return;
    }
    let headless = std::env::args().any(|a| a.starts_with("--snap") || a.starts_with("--tree") || a.starts_with("--script"));
    let app = App::new(AppSpec::<Ui> {
        scenes: &[("demo", demo)],
        min_size: Some((420.0, 360.0)),
        poll: Some(|s, _, _| s.expire()),
        ..AppSpec::new("Aun", (480.0, 560.0), || Ui::new(None), view)
    })
    .keep_running()
    // Closed, the app is a few threads and sockets: give the GPU device back.
    .release_gpu_when_closed();
    let node = if headless {
        None
    } else {
        let h = app.handle();
        let cfg = Config { dir: config_dir(), downloads: downloads_dir(), name: device_name(), port: aun_core::node::PORT, discovery: true, clipboard: true };
        match Node::start(cfg, move |note| h.update(move |s: &mut Ui, tx| s.apply(note, tx))) {
            Ok(n) => Some(n),
            Err(e) => {
                aun_core::log(format_args!("start: {e}"));
                None
            }
        }
    };
    app.init(move || Ui::new(node)).run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Key, KeyEvent, Runtime};

    #[test]
    fn a_clicked_code_field_takes_typed_digits() {
        let mut ui = Ui::new(None);
        ui.peers = vec![PeerView { id: [3; 32], name: "PC".into(), addr: Some(([10, 0, 0, 3], 47810).into()), paired: false, nearby: true, connected: false }];
        ui.pairing = Some([3; 32]);
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
    fn a_status_message_goes_away_on_its_own() {
        let mut ui = Ui::new(None);
        ui.message = Some(("연결된 기기가 아니에요".into(), Instant::now() + MESSAGE_FOR));
        let mut rt = Runtime::new(ui, view, (480.0, 560.0), Env { system_dark: false, scale: 2.0 });
        rt.frame(0.0);
        assert!(rt.find("message").is_some(), "a fresh message shows");
        assert!(!rt.state.expire(), "and stays until its time");
        rt.state.message.as_mut().unwrap().1 = Instant::now() - Duration::from_millis(1);
        assert!(rt.state.expire(), "past its time, poll drops it and asks for a new view");
        rt.invalidate(); // what poll's `true` does in the app
        rt.frame(1.0 / 60.0);
        assert!(rt.find("message").is_none(), "an old one is gone");
    }
}
