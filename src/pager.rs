use std::io::{Read, Write};

/// Get terminal height (lines) and width (columns).
pub fn terminal_size() -> (usize, usize) {
    // Try stty size first
    if let Ok(out) = std::process::Command::new("sh")
        .args(["-c", "stty size < /dev/tty 2>/dev/null | head -1"])
        .output()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        let parts: Vec<&str> = s.trim().split(' ').collect();
        if parts.len() == 2 {
            if let (Ok(h), Ok(w)) = (parts[0].parse(), parts[1].parse()) {
                return (h, w);
            }
        }
    }
    // Fallback
    (24, 80)
}

/// Read a single keypress from stdin (terminal must be in raw mode).
/// Returns the byte or escape sequence bytes.
pub fn read_key() -> Vec<u8> {
    // Set raw mode
    let _ = std::process::Command::new("sh")
        .args(["-c", "stty raw -echo < /dev/tty 2>/dev/null"])
        .status();

    let mut buf = vec![0u8; 8];
    let mut stdin = std::io::stdin();
    let n = stdin.read(&mut buf).unwrap_or(0);

    // Restore terminal
    let _ = std::process::Command::new("sh")
        .args(["-c", "stty sane < /dev/tty 2>/dev/null"])
        .status();

    buf[..n].to_vec()
}

/// Map raw bytes to page navigation action.
pub enum PageAction {
    Next,
    Prev,
    Quit,
    None,
}

pub fn parse_key(raw: &[u8]) -> PageAction {
    match raw {
        // Single char
        [b'q'] | [0x1b] | [0x03] => PageAction::Quit,  // q, Esc, Ctrl-C
        [b'n'] | [b' '] => PageAction::Next,            // n, Space
        [b'p'] | [b'b'] => PageAction::Prev,            // p, b
        // Up/Down arrows (CSI A / CSI B)
        [0x1b, b'[', b'A'] => PageAction::Prev,         // Up
        [0x1b, b'[', b'B'] => PageAction::Next,         // Down
        _ => PageAction::None,
    }
}

/// Print a page footer and wait for keypress.
pub fn page_footer(
    stdout: &mut impl Write,
    page: usize,
    total_pages: usize,
    start: usize,
    end: usize,
    total: usize,
) -> PageAction {
    let _ = write!(
        stdout,
        "\x1b[2m-- Page {}/{} ({}-{} / {}) -- [\u{2191}/\u{2193}/n/p] or [q]uit  \x1b[0m",
        page + 1,
        total_pages,
        start,
        end,
        total,
    );
    let _ = stdout.flush();
    let key = read_key();
    parse_key(&key)
}
