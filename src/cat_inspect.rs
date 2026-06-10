use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crate::*; // for describe_kind, detect_kind, is_binary, FileKind

/// Compute Shannon entropy of a byte slice (bits per byte, 0–8).
pub(crate) fn shannon_entropy(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let len = data.len() as f64;
    let inv_len = 1.0 / len;
    let mut entropy = 0.0_f64;
    for &c in &counts {
        if c == 0 {
            continue;
        }
        let p = c as f64 * inv_len;
        entropy -= p * p.log2();
    }
    entropy
}

/// Compute SHA256 hex digest using the system's sha256sum.
fn sha256_hex(data: &[u8]) -> String {
    use std::process::{Command, Stdio};
    let mut child = match Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return "N/A (sha256sum not available)".into(),
    };
    let _ = io::Write::write_all(&mut child.stdin.take().unwrap(), data);
    let output = child.wait_with_output().ok();
    match output {
        Some(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout);
            s.split_whitespace().next().unwrap_or("???").to_string()
        }
        _ => "ERR".into(),
    }
}

/// Human-readable size string.
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {} ({} B)", size, UNITS[unit_idx], bytes)
    }
}

/// Detect text encoding from BOM or content.
pub(crate) fn detect_encoding(data: &[u8]) -> &'static str {
    if data.len() < 2 {
        return "ASCII";
    }
    match (data[0], data[1]) {
        (0xfe, 0xff) => "UTF-16 BE",
        (0xff, 0xfe) => "UTF-16 LE",
        (0xef, 0xbb) if data.len() > 2 && data[2] == 0xbf => "UTF-8 with BOM",
        _ => {
            // Check if valid UTF-8
            let sample = if data.len() > 4096 { &data[..4096] } else { data };
            if std::str::from_utf8(sample).is_ok() {
                // Check for multi-byte sequences
                let non_ascii = sample.iter().filter(|&&b| b >= 0x80).count();
                if non_ascii > 0 {
                    "UTF-8"
                } else {
                    "ASCII"
                }
            } else {
                // Check if mostly Latin-1 (bytes 0x80-0xFF not valid UTF-8 alone)
                "Binary"
            }
        }
    }
}

/// Text statistics for text-like files.
pub(crate) struct TextStats {
    pub(crate) lines: usize,
    pub(crate) blank_lines: usize,
    pub(crate) words: usize,
    pub(crate) chars: usize,
    pub(crate) max_line_len: usize,
}

pub(crate) fn compute_text_stats(data: &[u8]) -> TextStats {
    let s = String::from_utf8_lossy(data);
    let mut lines = 0usize;
    let mut blank_lines = 0usize;
    let mut words = 0usize;
    let chars = s.chars().count();
    let mut max_line_len = 0usize;

    for line in s.lines() {
        lines += 1;
        if line.trim().is_empty() {
            blank_lines += 1;
        } else {
            words += line.split_whitespace().count();
        }
        let line_len = line.chars().count();
        if line_len > max_line_len {
            max_line_len = line_len;
        }
    }

    TextStats { lines, blank_lines, words, chars, max_line_len }
}

/// Inspect a JSON/YAML/TOML value and return (key_count, max_depth)
fn inspect_value(value: &serde_json::Value, depth: usize) -> (usize, usize) {
    match value {
        serde_json::Value::Object(map) => {
            let mut key_count = map.len();
            let mut max_depth = depth + 1;
            for v in map.values() {
                let (kc, md) = inspect_value(v, depth + 1);
                key_count += kc;
                if md > max_depth {
                    max_depth = md;
                }
            }
            (key_count, max_depth)
        }
        serde_json::Value::Array(arr) => {
            let mut max_depth = depth + 1;
            for v in arr {
                let (_, md) = inspect_value(v, depth + 1);
                if md > max_depth {
                    max_depth = md;
                }
            }
            (0, max_depth)
        }
        _ => (0, depth),
    }
}

pub(crate) fn format_structured_info(data: &[u8], _path: &Path) -> Option<(usize, usize, &'static str)> {
    // Try JSON
    let s = String::from_utf8_lossy(data);
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&s) {
        let (keys, depth) = inspect_value(&val, 0);
        let doc_type = if val.is_object() { "JSON object" } else if val.is_array() { "JSON array" } else { "JSON value" };
        return Some((keys, depth, doc_type));
    }

    // Try YAML
    if let Ok(val) = serde_yaml::from_str::<serde_json::Value>(&s) {
        let (keys, depth) = inspect_value(&val, 0);
        return Some((keys, depth, "YAML document"));
    }

    // Try TOML
    if let Ok(val) = s.parse::<toml::Value>() {
        let json_val = toml_to_json(&val);
        let (keys, depth) = inspect_value(&json_val, 0);
        return Some((keys, depth, "TOML document"));
    }

    None
}

fn toml_to_json(val: &toml::Value) -> serde_json::Value {
    match val {
        toml::Value::Table(map) => {
            let mut m = serde_json::Map::new();
            for (k, v) in map {
                m.insert(k.clone(), toml_to_json(v));
            }
            serde_json::Value::Object(m)
        }
        toml::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(toml_to_json).collect())
        }
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::Value::Number((*i).into()),
        toml::Value::Float(f) => {
            serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
    }
}

/// Get syntect language name for a file.
fn detect_language(path: &Path) -> Option<String> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    let synth = syntect::parsing::SyntaxSet::load_defaults_newlines();
    let syn = synth.find_syntax_by_extension(ext)?;
    Some(syn.name.clone())
}

/// Label for common file types.
fn type_label(_kind: &FileKind, data: &[u8], path: &Path) -> String {
    let desc = describe_kind(data, path);
    // describe_kind returns "mime: description" format
    if let Some((_, readable)) = desc.split_once(": ") {
        readable.to_string()
    } else {
        desc
    }
}

// ── Public API ──

/// Print a detailed inspection of a file to stdout.
pub fn inspect_file(data: &[u8], path: &Path) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // ── File metadata from filesystem ──
    let (size, modified, mode_str) = match fs::metadata(path) {
        Ok(meta) => {
            let size = meta.len();
            let modified = meta.modified().ok()
                .map(|t| {
                    // Format as local time (approximate)
                    use std::time::SystemTime;
                    let since_epoch = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
                    let secs = since_epoch.as_secs();
                    let days = secs / 86400;
                    let time_of_day = secs % 86400;
                    let hours = time_of_day / 3600;
                    let mins = (time_of_day % 3600) / 60;
                    let sec = time_of_day % 60;
                    let remaining_days = days as i64;
                    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
                        1970 + (remaining_days / 365) as u32,
                        1 + ((remaining_days % 365) / 30) as u32,
                        1 + (remaining_days % 30) as u32,
                        hours, mins, sec)
                })
                .unwrap_or_else(|| "unknown".into());

            let mode = meta.permissions();
            let mode_str = if cfg!(unix) {
                use std::os::unix::fs::PermissionsExt;
                let m = mode.mode();
                let file_type = if meta.is_dir() { 'd' } else if meta.is_symlink() { 'l' } else { '-' };
                let ur = if m & 0o400 != 0 { 'r' } else { '-' };
                let uw = if m & 0o200 != 0 { 'w' } else { '-' };
                let ux = if m & 0o100 != 0 { 'x' } else { '-' };
                let gr = if m & 0o040 != 0 { 'r' } else { '-' };
                let gw = if m & 0o020 != 0 { 'w' } else { '-' };
                let gx = if m & 0o010 != 0 { 'x' } else { '-' };
                let or = if m & 0o004 != 0 { 'r' } else { '-' };
                let ow = if m & 0o002 != 0 { 'w' } else { '-' };
                let ox = if m & 0o001 != 0 { 'x' } else { '-' };
                format!("{}{}{}{}{}{}{}{}{}{} ({:o})",
                    file_type, ur, uw, ux, gr, gw, gx, or, ow, ox, m & 0o777)
            } else {
                "--- (unknown)".into()
            };

            (size, modified, mode_str)
        }
        Err(_) => (data.len() as u64, "N/A (stdin)".into(), "N/A".into()),
    };

    let bin = is_binary(data);
    let encoding = detect_encoding(data);
    let entropy = shannon_entropy(data);
    let sha = sha256_hex(data);
    let kind = detect_kind(data, path);
    let label = type_label(&kind, data, path);

    // Magic bytes (first 16 bytes as hex)
    let magic_len = data.len().min(16);
    let magic_hex: String = data[..magic_len].iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|chunk| chunk.join(""))
        .collect::<Vec<_>>()
        .join(" ");
    let magic_ascii: String = data[..magic_len].iter()
        .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' })
        .collect();

    // ── Render ──
    let _ = writeln!(out, "{}", style::dim("┌─ ccat inspect ──────────────────────────────────────────────────┐"));
    let _ = writeln!(out, " │ {} {} {}", style::bold("File:"), style::cyan(&path.display().to_string()), style::dim("│"));

    let label_line = format!(" │ {} {} {} {}{}", style::bold("Type:"), style::green(&label), style::dim("│"), "", "");
    let _ = writeln!(out, "{}", label_line);

    let mut lines: Vec<(String, String)> = Vec::new();
    lines.push(("Size".into(), human_size(size)));
    lines.push(("Modified".into(), modified));
    lines.push(("Mode".into(), mode_str));
    lines.push(("MIME".into(), {
        if let Some(k) = infer::get(data) {
            k.mime_type().to_string()
        } else {
            "application/octet-stream".into()
        }
    }));
    lines.push(("Encoding".into(), encoding.into()));
    lines.push(("Binary".into(), if bin { style::red("yes").to_string() } else { style::green("no").to_string() }));
    lines.push(("Entropy".into(), format!("{:.2} bits/byte", entropy)));

    // Truncate SHA to fit
    let sha_display = if sha.len() > 64 {
        format!("{}...{}", &sha[..16], &sha[sha.len()-16..])
    } else {
        sha.clone()
    };
    lines.push(("SHA256".into(), sha_display));

    // Magic bytes
    let magic_display = if magic_len > 0 {
        format!("{}  {}", style::dim(&magic_hex), style::dim(&magic_ascii))
    } else {
        "(empty file)".into()
    };
    lines.push(("Magic".into(), magic_display));

    // Text statistics if applicable
    if !bin && kind != FileKind::Image && kind != FileKind::Media
        && kind != FileKind::Archive && kind != FileKind::Pdf
        && kind != FileKind::Docx && kind != FileKind::Gzip
    {
        let stats = compute_text_stats(data);
        lines.push(("Lines".into(), format!("{} (blank: {}, code: {})",
            stats.lines, stats.blank_lines, stats.lines.saturating_sub(stats.blank_lines))));
        lines.push(("Words".into(), format!("{}", stats.words)));
        lines.push(("Chars".into(), format!("{}", stats.chars)));
        lines.push(("Max line".into(), format!("{} chars", stats.max_line_len)));
    }

    // Structured data info if JSON/YAML/TOML (also try for PlainText — common with stdin)
    if kind == FileKind::Json || kind == FileKind::Yaml || kind == FileKind::Toml || kind == FileKind::PlainText {
        if let Some((keys, depth, doc_type)) = format_structured_info(data, path) {
            lines.push(("Structure".into(), format!("{} ({} keys, depth {})", doc_type, keys, depth)));
        }
    }

    // Language detection for source code
    if kind == FileKind::SourceCode {
        if let Some(lang) = detect_language(path) {
            lines.push(("Language".into(), lang));
        }
    }

    // Render key-value lines
    let max_key_len = lines.iter().map(|(k, _)| k.len()).max().unwrap_or(10);
    for (key, value) in &lines {
        let padding = " ".repeat(max_key_len.saturating_sub(key.len()));
        let _ = writeln!(out, " │ {} {}{}  {} {}", style::bold(key), padding, style::dim("│"), value, "");
    }

    let _ = writeln!(out, " {}", style::dim("└─────────────────────────────────────────────────────────────────┘"));
}

/// Print a compact inspection of stdin data.
pub fn inspect_stdin(data: &[u8]) {
    let bin = is_binary(data);
    let encoding = detect_encoding(data);
    let entropy = shannon_entropy(data);
    let sha = sha256_hex(data);
    let kind = detect_kind(data, Path::new(""));
    let label = type_label(&kind, data, Path::new(""));

    let stdout = io::stdout();
    let mut out = stdout.lock();

    let _ = writeln!(out, "{}", style::dim("┌─ ccat inspect (stdin) ──────────────────────────────────────────┐"));
    let _ = writeln!(out, " │ {} {} {}", style::bold("Type:"), style::green(&label), style::dim("│"));
    let _ = writeln!(out, " │ {} {} ({} B)        {}", style::bold("Size:"), data.len(), data.len(), style::dim("│"));
    let _ = writeln!(out, " │ {} {}             {}", style::bold("Encoding:"), encoding, style::dim("│"));
    let _ = writeln!(out, " │ {} {}                   {}", style::bold("Binary:"),
        if bin { style::red("yes") } else { style::green("no") }, style::dim("│"));
    let _ = writeln!(out, " │ {} {:.2} bits/byte          {}", style::bold("Entropy:"), entropy, style::dim("│"));

    let sha_display = if sha.len() > 64 {
        format!("{}...{}", &sha[..16], &sha[sha.len()-16..])
    } else {
        sha.clone()
    };
    let _ = writeln!(out, " │ {} {}  {}", style::bold("SHA256:"), sha_display, style::dim("│"));

    if !bin && kind != FileKind::Image && kind != FileKind::Media
        && kind != FileKind::Archive && kind != FileKind::Pdf
        && kind != FileKind::Docx
    {
        let stats = compute_text_stats(data);
        let _ = writeln!(out, " │ {} {} (blank: {})             {}", style::bold("Lines:"),
            stats.lines, stats.blank_lines, style::dim("│"));
        let _ = writeln!(out, " │ {} {}                   {}", style::bold("Words:"), stats.words, style::dim("│"));
    }

    // Try structured data detection for stdin too
    if kind == FileKind::PlainText || kind == FileKind::Json || kind == FileKind::Yaml || kind == FileKind::Toml {
        if let Some((keys, depth, doc_type)) = format_structured_info(data, Path::new("")) {
            let _ = writeln!(out, " │ {} {} ({} keys, depth {})     {}",
                style::bold("Structure:"), doc_type, keys, depth, style::dim("│"));
        }
    }

    let _ = writeln!(out, " {}", style::dim("└───────────────────────────────────────────────────────────────┘"));
}

// ── Terminal styling helpers (copied from color_scheme.rs conventions) ──

mod style {
    pub fn bold(s: &str) -> String {
        format!("\x1b[1m{}\x1b[0m", s)
    }
    pub fn dim(s: &str) -> String {
        format!("\x1b[2m{}\x1b[0m", s)
    }
    pub fn green(s: &str) -> String {
        format!("\x1b[32m{}\x1b[0m", s)
    }
    pub fn red(s: &str) -> String {
        format!("\x1b[31m{}\x1b[0m", s)
    }
    pub fn cyan(s: &str) -> String {
        format!("\x1b[36m{}\x1b[0m", s)
    }
}
