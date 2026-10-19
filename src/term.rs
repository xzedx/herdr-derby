//! Raw-mode terminal input and sizing via libc, no TUI framework.
//!
//! Unlike easyjump's single-letter HUD, the broadcast popup is a text
//! editor, so input decoding has to handle UTF-8 (CJK prompts), bracketed
//! paste (multi-line prompts pasted in one go) and a newline key that is
//! not Enter (Alt+Enter, Ctrl+J, Shift+Enter where the terminal reports it).

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Make SIGHUP/SIGTERM/SIGINT interrupt the blocking read so cleanup runs.
pub fn install_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as *const () as usize;
        sa.sa_flags = 0; // no SA_RESTART: read() returns EINTR
        libc::sigemptyset(&mut sa.sa_mask);
        for sig in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

pub struct RawMode {
    orig: libc::termios,
}

impl RawMode {
    pub fn enable() -> Option<Self> {
        unsafe {
            let mut orig: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut orig) != 0 {
                return None;
            }
            let mut raw = orig;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(0, libc::TCSANOW, &raw) != 0 {
                return None;
            }
            Some(RawMode { orig })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // TCSANOW, not TCSADRAIN: draining blocks forever when nobody reads
        // the pty master (test harnesses, a dying parent).
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.orig);
        }
    }
}

pub fn size() -> (usize, usize) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            return (ws.ws_col as usize, ws.ws_row as usize);
        }
    }
    let env = |k: &str, d: usize| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    (env("COLUMNS", 76), env("LINES", 16))
}

#[derive(Debug, PartialEq, Clone)]
pub enum Key {
    Char(char),
    Paste(String),
    /// Plain Enter: send.
    Enter,
    /// Alt+Enter, Ctrl+J, Shift+Enter: insert a line break.
    Newline,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Backspace,
    Delete,
    Esc,
    CtrlC,
    CtrlU,
    /// Line start in the editor, select all in a list.
    CtrlA,
    /// Select none in a list.
    CtrlL,
    Eof,
    Unknown,
}

/// Where key bytes come from. `next(ms)` waits up to `ms` for a byte and
/// returns None on timeout; `ms < 0` waits forever. `closed()` tells a
/// timeout from end of input.
pub trait ByteSource {
    fn next(&mut self, ms: i32) -> Option<u8>;
    fn closed(&self) -> bool;
}

/// The popup's stdin, with optional injected bytes in front (testing).
pub struct Stdin {
    pub injected: VecDeque<u8>,
    eof: bool,
}

impl Stdin {
    pub fn new(injected: &str) -> Self {
        Stdin {
            injected: injected.bytes().collect(),
            eof: false,
        }
    }
}

impl ByteSource for Stdin {
    fn next(&mut self, ms: i32) -> Option<u8> {
        if let Some(b) = self.injected.pop_front() {
            return Some(b);
        }
        let mut pfd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            let r = unsafe { libc::poll(&mut pfd, 1, ms) };
            if r == 0 {
                return None;
            }
            if r < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    && !INTERRUPTED.load(Ordering::SeqCst)
                {
                    continue;
                }
                self.eof = true;
                return None;
            }
            let mut b = [0u8; 1];
            let n = unsafe { libc::read(0, b.as_mut_ptr() as *mut libc::c_void, 1) };
            if n == 1 {
                return Some(b[0]);
            }
            if n < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                && !INTERRUPTED.load(Ordering::SeqCst)
            {
                continue;
            }
            self.eof = true;
            return None;
        }
    }

    fn closed(&self) -> bool {
        self.eof || INTERRUPTED.load(Ordering::SeqCst)
    }
}

/// Bytes of an escape sequence arrive together; anything slower than this
/// after ESC is a lone Esc key press.
const ESC_GAP_MS: i32 = 30;

/// Wait up to `ms` for one key; None on timeout.
pub fn read_key<S: ByteSource>(src: &mut S, ms: i32) -> Option<Key> {
    let Some(b) = src.next(ms) else {
        return src.closed().then_some(Key::Eof);
    };
    Some(decode(src, b))
}

fn decode<S: ByteSource>(src: &mut S, b: u8) -> Key {
    match b {
        0x1b => decode_escape(src),
        0x0d => Key::Enter,
        0x0a => Key::Newline,
        0x09 => Key::Tab,
        0x7f | 0x08 => Key::Backspace,
        0x03 => Key::CtrlC,
        0x15 => Key::CtrlU,
        0x01 => Key::CtrlA,
        0x0c => Key::CtrlL,
        0x05 => Key::End,
        b if b < 0x20 => Key::Unknown,
        b if b < 0x80 => Key::Char(b as char),
        lead => decode_utf8(src, lead),
    }
}

fn decode_escape<S: ByteSource>(src: &mut S) -> Key {
    let Some(b) = src.next(ESC_GAP_MS) else {
        return Key::Esc;
    };
    match b {
        b'[' => decode_csi(src),
        b'O' => match src.next(ESC_GAP_MS) {
            Some(b'A') => Key::Up,
            Some(b'B') => Key::Down,
            Some(b'C') => Key::Right,
            Some(b'D') => Key::Left,
            Some(b'H') => Key::Home,
            Some(b'F') => Key::End,
            _ => Key::Unknown,
        },
        0x0d | 0x0a => Key::Newline,
        _ => Key::Unknown,
    }
}

fn decode_csi<S: ByteSource>(src: &mut S) -> Key {
    let mut params = String::new();
    let fin = loop {
        match src.next(ESC_GAP_MS) {
            Some(c @ 0x40..=0x7e) => break c,
            Some(c) => params.push(c as char),
            None => return Key::Unknown,
        }
    };
    match (params.as_str(), fin) {
        ("", b'A') => Key::Up,
        ("", b'B') => Key::Down,
        ("", b'C') => Key::Right,
        ("", b'D') => Key::Left,
        ("", b'H') | ("1", b'~') | ("7", b'~') => Key::Home,
        ("", b'F') | ("4", b'~') | ("8", b'~') => Key::End,
        ("", b'Z') => Key::BackTab,
        ("3", b'~') => Key::Delete,
        ("200", b'~') => Key::Paste(read_paste(src)),
        // Shift/Alt+Enter as kitty keyboard protocol or xterm modifyOtherKeys
        // report them.
        (p, b'u') if p.starts_with("13;") => Key::Newline,
        (p, b'~') if p.starts_with("27;") && p.ends_with(";13") => Key::Newline,
        _ => Key::Unknown,
    }
}

/// Everything up to ESC [ 201 ~, with CR/CRLF turned into LF.
fn read_paste<S: ByteSource>(src: &mut S) -> String {
    const END: &[u8] = b"\x1b[201~";
    let mut bytes: Vec<u8> = Vec::new();
    // A paste can be large and arrive in chunks; allow generous gaps.
    while let Some(b) = src.next(1000) {
        bytes.push(b);
        if bytes.ends_with(END) {
            bytes.truncate(bytes.len() - END.len());
            break;
        }
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn decode_utf8<S: ByteSource>(src: &mut S, lead: u8) -> Key {
    let len = match lead {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return Key::Unknown,
    };
    let mut buf = vec![lead];
    for _ in 1..len {
        match src.next(ESC_GAP_MS) {
            Some(b) if b & 0xc0 == 0x80 => buf.push(b),
            _ => return Key::Unknown,
        }
    }
    std::str::from_utf8(&buf)
        .ok()
        .and_then(|s| s.chars().next())
        .map(Key::Char)
        .unwrap_or(Key::Unknown)
}

/// Draw a full frame and leave the cursor at `cursor` (row, col), shown,
/// or hidden when None.
pub fn draw(lines: &[String], cursor: Option<(usize, usize)>) {
    let mut out = String::from("\x1b[?25l\x1b[H");
    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        out.push_str("\x1b[K");
        if i + 1 < lines.len() {
            out.push_str("\r\n");
        }
    }
    out.push_str("\x1b[J");
    if let Some((row, col)) = cursor {
        out.push_str(&format!("\x1b[{};{}H\x1b[?25h", row + 1, col + 1));
    }
    let mut so = std::io::stdout().lock();
    let _ = so.write_all(out.as_bytes());
    let _ = so.flush();
}

/// Alternate screen plus bracketed paste.
pub fn enter_screen() {
    let mut so = std::io::stdout().lock();
    let _ = so.write_all(b"\x1b[?1049h\x1b[?2004h");
    let _ = so.flush();
}

pub fn leave_screen() {
    let mut so = std::io::stdout().lock();
    let _ = so.write_all(b"\x1b[?2004l\x1b[?25h\x1b[?1049l");
    let _ = so.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bytes(VecDeque<u8>);

    impl ByteSource for Bytes {
        fn next(&mut self, _ms: i32) -> Option<u8> {
            self.0.pop_front()
        }
        fn closed(&self) -> bool {
            self.0.is_empty()
        }
    }

    fn keys(input: &[u8]) -> Vec<Key> {
        let mut src = Bytes(input.iter().copied().collect());
        let mut out = Vec::new();
        while let Some(k) = read_key(&mut src, 0) {
            if k == Key::Eof {
                break;
            }
            out.push(k);
        }
        out
    }

    #[test]
    fn ascii_and_cjk() {
        assert_eq!(
            keys("a赛马".as_bytes()),
            vec![Key::Char('a'), Key::Char('赛'), Key::Char('马')]
        );
    }

    #[test]
    fn enter_versus_newline() {
        assert_eq!(keys(b"\r"), vec![Key::Enter]);
        assert_eq!(keys(b"\n"), vec![Key::Newline]);
        assert_eq!(keys(b"\x1b\r"), vec![Key::Newline]);
        assert_eq!(keys(b"\x1b[13;2u"), vec![Key::Newline]);
        assert_eq!(keys(b"\x1b[27;2;13~"), vec![Key::Newline]);
    }

    #[test]
    fn arrows_and_lone_esc() {
        assert_eq!(keys(b"\x1b[A\x1bOB"), vec![Key::Up, Key::Down]);
        assert_eq!(keys(b"\x1b"), vec![Key::Esc]);
        assert_eq!(keys(b"\x1b[Z"), vec![Key::BackTab]);
        assert_eq!(keys(b"\x01\x0c"), vec![Key::CtrlA, Key::CtrlL]);
    }

    #[test]
    fn bracketed_paste_keeps_lines() {
        let input = "\x1b[200~第一行\r\nline two\rthree\x1b[201~x".as_bytes();
        assert_eq!(
            keys(input),
            vec![
                Key::Paste("第一行\nline two\nthree".into()),
                Key::Char('x')
            ]
        );
    }
}
