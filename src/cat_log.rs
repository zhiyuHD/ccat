use std::io::Write;

/// Highlight common log formats (syslog, journal, app logs) with colors.
///
/// Detects: log levels, timestamps, IPs, stack traces, HTTP status codes.
pub fn cat_log(data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    for line in s.lines() {
        let colored = highlight_log_line(line);
        let _ = writeln!(handle, "{}", colored);
    }
}

fn highlight_log_line(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;

    // Try to detect and colorize timestamp at start
    // ISO 8601: 2024-01-15T10:30:00 or similar
    if rest.len() > 19 {
        let candidate = &rest[..19];
        if candidate.chars().nth(4) == Some('-') && candidate.chars().nth(7) == Some('-')
            && candidate.chars().nth(10) == Some('T')
        {
            out.push_str(&format!("\x1b[2m{}\x1b[0m", candidate));
            rest = &rest[19..];
        } else if candidate.chars().nth(4) == Some('-') && candidate.chars().nth(7) == Some('-')
            && (candidate.chars().nth(10) == Some(' ') || rest.len() > 20 && &rest[..10] == candidate)
        {
            // Date only or date with space
            let date = &rest[..10];
            out.push_str(&format!("\x1b[2m{}\x1b[0m", date));
            rest = &rest[10..];
            if rest.starts_with(' ') {
                let time = rest.trim_start();
                if time.len() >= 8 && time.as_bytes()[2] == b':' && time.as_bytes()[5] == b':' {
                    let time_part = &time[..8];
                    out.push_str(&format!(" \x1b[2m{}\x1b[0m", time_part));
                    rest = &time[8..];
                }
            }
        }
    }

    // Colorize log levels
    let lower = rest.to_lowercase();
    if lower.contains("error") || lower.contains("fatal") || lower.contains("critical") || lower.contains("panic") {
        // Red for errors
        return format!("{}{}", out, highlight_with_color(rest, "\x1b[1;31m"));
    }
    if lower.contains("warn") || lower.contains("warning") {
        return format!("{}{}", out, highlight_with_color(rest, "\x1b[33m"));
    }
    if lower.contains("info") {
        return format!("{}{}", out, highlight_with_color(rest, "\x1b[36m"));
    }
    if lower.contains("debug") || lower.contains("trace") || lower.contains("verbose") {
        return format!("{}{}", out, highlight_with_color(rest, "\x1b[2m"));
    }

    // HTTP status codes
    if rest.contains(" 200 ") || rest.contains(" 201 ") || rest.contains(" 204 ") {
        return format!("{}{}", out, highlight_with_color(rest, "\x1b[32m"));
    }
    if rest.contains(" 301 ") || rest.contains(" 302 ") || rest.contains(" 304 ") {
        return format!("{}{}", out, highlight_with_color(rest, "\x1b[36m"));
    }
    if rest.contains(" 400 ") || rest.contains(" 401 ") || rest.contains(" 403 ") || rest.contains(" 404 ") || rest.contains(" 405 ") {
        return format!("{}{}", out, highlight_with_color(rest, "\x1b[33m"));
    }
    if rest.contains(" 500 ") || rest.contains(" 502 ") || rest.contains(" 503 ") {
        return format!("{}{}", out, highlight_with_color(rest, "\x1b[1;31m"));
    }

    // IP addresses
    if rest.contains(|c: char| c.is_ascii_digit()) {
        let colored = colorize_ips(rest);
        return format!("{}{}", out, colored);
    }

    format!("{}{}", out, rest)
}

fn highlight_with_color(line: &str, color: &str) -> String {
    format!("{}{}\x1b[0m", color, line)
}

fn colorize_ips(line: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    let bytes = line.as_bytes();
    while i < bytes.len() {
        // Look for IPv4 pattern: digit.digit.digit.digit
        if i + 7 < bytes.len() {
            // Simple heuristic: x.x.x.x
            let mut is_ip = true;
            let mut parts = 0;
            for (j, &b) in bytes[i..].iter().enumerate() {
                if b == b'.' {
                    parts += 1;
                } else if !b.is_ascii_digit() {
                    if parts == 3 && b != b'.' {
                        break;
                    }
                    if parts < 3 {
                        is_ip = false;
                    }
                    break;
                }
            }
            if is_ip && parts >= 3 {
                // Find the end of IP
                let mut end = i;
                let mut dot_count = 0;
                while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
                    if bytes[end] == b'.' { dot_count += 1; }
                    end += 1;
                }
                if dot_count == 3 {
                    let ip = &line[i..end];
                    out.push_str(&format!("\x1b[94m{}\x1b[0m", ip));
                    i = end;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}
