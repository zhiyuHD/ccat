use std::io::{Read, Write};

/// Terminal size cache.
static mut TERM_H: usize = 24;
static mut TERM_W: usize = 80;

/// Get terminal height (lines) and width (columns).
pub fn terminal_size() -> (usize, usize) {
    if let Ok(out) = std::process::Command::new("sh")
        .args(["-c", "stty size < /dev/tty 2>/dev/null | head -1"])
        .output()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        let parts: Vec<&str> = s.trim().split(' ').collect();
        if parts.len() == 2 {
            if let (Ok(h), Ok(w)) = (parts[0].parse(), parts[1].parse()) {
                unsafe { TERM_H = h; TERM_W = w; }
                return (h, w);
            }
        }
    }
    unsafe { (TERM_H, TERM_W) }
}

/// Pager action.
pub enum PageAction {
    Next(usize),     // +n lines
    Prev(usize),     // -n lines
    Goto(usize),     // go to page
    Search,  // search forward
    Quit,
    None,
}

/// State for the interactive pager.
pub struct PagerState {
    pub page: usize,
    pub total_pages: usize,
    pub page_size: usize,
    pub total_items: usize,
    pub search_query: Option<String>,
    pub search_results: Vec<usize>,
    pub search_idx: usize,
}

impl PagerState {
    pub fn new(total_items: usize) -> Self {
        let (h, _) = terminal_size();
        let page_size = h.saturating_sub(2).max(5);
        let total_pages = total_items.div_ceil(page_size);
        Self {
            page: 0,
            total_pages,
            page_size,
            total_items,
            search_query: None,
            search_results: Vec::new(),
            search_idx: 0,
        }
    }

    pub fn range(&self) -> (usize, usize) {
        let start = self.page * self.page_size;
        let end = (start + self.page_size).min(self.total_items);
        (start, end)
    }

    pub fn navigate(&mut self, action: PageAction) -> bool {
        match action {
            PageAction::Next(n) => {
                if self.page + 1 < self.total_pages {
                    self.page = (self.page + n).min(self.total_pages - 1);
                }
                true
            }
            PageAction::Prev(n) => {
                if self.page > 0 {
                    self.page = self.page.saturating_sub(n);
                }
                true
            }
            PageAction::Goto(p) => {
                self.page = p.min(self.total_pages.saturating_sub(1));
                true
            }
            PageAction::Quit => false,
            PageAction::Search => true, // handled by caller
            PageAction::None => true,
        }
    }
}

/// Set terminal raw mode.
fn raw_mode(on: bool) {
    let cmd = if on { "stty raw -echo < /dev/tty 2>/dev/null" } else { "stty sane < /dev/tty 2>/dev/null" };
    let _ = std::process::Command::new("sh").args(["-c", cmd]).status();
}

/// Read a key sequence. Returns up to 8 bytes.
pub fn read_key() -> Vec<u8> {
    raw_mode(true);
    let mut buf = vec![0u8; 8];
    let n = std::io::stdin().read(&mut buf).unwrap_or(0);
    raw_mode(false);
    buf[..n].to_vec()
}

/// Parse keypress into pager action with movement amount.
pub fn parse_key(raw: &[u8]) -> PageAction {
    match raw {
        [b'q'] | [0x1b] | [0x03] => PageAction::Quit,
        [b' '] | [0x42] => PageAction::Next(1),      // space or Down
        [0x1b, b'[', b'B'] => PageAction::Next(1),    // Down arrow
        [0x1b, b'[', b'6', b'~'] => PageAction::Next(1),  // PgDn
        [b'b'] | [0x1b, b'[', b'A'] => PageAction::Prev(1), // Up arrow
        [0x1b, b'[', b'5', b'~'] => PageAction::Prev(1),   // PgUp
        [b'n'] => PageAction::Next(1),
        [b'p'] => PageAction::Prev(1),
        [b'g'] => PageAction::Goto(0),
        [b'G'] => PageAction::Goto(usize::MAX),
        [b'/'] => PageAction::Search,
        _ => PageAction::None,
    }
}

/// Read a search query from user after '/'.
pub fn read_search_query() -> String {
    let mut query = String::new();
    raw_mode(true);
    let mut stdin = std::io::stdin();
    loop {
        // Show mini prompt
        let _ = write!(
            std::io::stderr(),
            "\r\x1b[K\x1b[2m/{} \x1b[0m",
            query
        );
        let _ = std::io::stderr().flush();

        let mut buf = [0u8; 1];
        if stdin.read_exact(&mut buf).is_err() {
            break;
        }
        match buf[0] {
            0x03 | 0x1b => break,  // Ctrl-C / Esc — cancel
            0x0a | 0x0d => break,  // Enter — confirm
            0x7f | 0x08 => { query.pop(); }  // Backspace
            c if c >= 0x20 => query.push(c as char),
            _ => {}
        }
    }
    raw_mode(false);
    // Clear the prompt line
    let _ = write!(std::io::stderr(), "\r\x1b[K");
    let _ = std::io::stderr().flush();
    query
}

/// Render status bar for the pager.
pub fn status_bar(
    state: &PagerState,
    _items: &[String],
) {
    let (_, w) = terminal_size();
    let (start, end) = state.range();

    // Search hit indicator
    let search_info = if let Some(ref q) = state.search_query {
        let hits = state.search_results.len();
        if hits > 0 {
            let hit_idx = state.search_results.iter().position(|&p| p >= start).unwrap_or(0) + 1;
            format!(" /{q} ({hit_idx}/{hits})")
        } else {
            format!(" /{q} (0)")
        }
    } else {
        String::new()
    };

    let status = format!(
        "\x1b[7m Page {}/{} ({}-{}/{}) {} \x1b[0m",
        state.page + 1,
        state.total_pages,
        start + 1,
        end,
        state.total_items,
        search_info,
    );

    // Truncate if too long
    let status = if status.len() > w.saturating_sub(1) {
        let prefix = "\x1b[7m \x1b[0m";
        format!("{}{}", prefix, &status[status.len().saturating_sub(w.saturating_sub(prefix.len()))..])
    } else {
        status
    };

    let _ = write!(std::io::stderr(), "\r{}\x1b[K", status);
    let _ = std::io::stderr().flush();
}

/// Run full interactive pager for a list of lines.
/// Lines should NOT include trailing newlines.
pub fn run_pager(items: &[String]) {
    if items.is_empty() {
        return;
    }

    if items.len() <= 20 {
        // Short enough to print directly
        for line in items {
            println!("{line}");
        }
        return;
    }

    let mut state = PagerState::new(items.len());

    loop {
        let (start, end) = state.range();
        for line in &items[start..end] {
            println!("{line}");
        }

        status_bar(&state, items);

        let key = read_key();
        let action = parse_key(&key);

        let mut handled = false;

        if let PageAction::Search = &action {
            let query = read_search_query();
            if query.is_empty() {
                continue;
            }
            // Search from current position
            let results: Vec<usize> = items.iter()
                .enumerate()
                .filter(|(_, line)| line.to_lowercase().contains(&query.to_lowercase()))
                .map(|(i, _)| i)
                .collect();
            state.search_query = Some(query);
            state.search_results = results;

            if !state.search_results.is_empty() {
                // Jump to first result after current page
                let first = state.search_results[0];
                state.page = first / state.page_size;
                state.search_idx = 0;
            }
            handled = true;
        }

        if !handled {
            if !state.navigate(action) {
                break;
            }
        }
    }

    // Clear status bar
    let _ = write!(std::io::stderr(), "\r\x1b[K");
    let _ = std::io::stderr().flush();
}

/// Simple page_footer for non-line-based content (hex/asm pages).
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
        "\x1b[2m-- Page {}/{} ({}-{} / {}) -- [PgUp/PgDn/↑/↓/space/q] or [/]search \x1b[0m",
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
