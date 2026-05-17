use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use clap::CommandFactory;
use clap_complete::{Generator, Shell};
use flate2::read::GzDecoder;

mod cat_markdown;
mod cat_docx;
mod cat_image;
mod cat_disasm;
mod cat_diff;
mod cat_pdf;
mod cat_archive;
mod cat_media;
mod cat_json;
mod cat_yaml;
mod cat_log;
mod cat_csv;
mod cat_toml;
mod pager;

const STATE_DIR: &str = "/tmp/ccat-state";

/// ccat - An enhanced cat tool with automatic file type detection
#[derive(Parser)]
#[command(name = "ccat", version, about = "Enhanced cat: auto-detect and display markdown, docx, images, gz, diff, and disassemble ELF")]
struct Cli {
    /// File(s) to display (or use - to read stdin). When a directory is given,
    /// shows a summary similar to `file`.
    files: Vec<String>,

    /// Generate shell completions
    #[arg(long = "completions", value_name = "SHELL", value_parser = clap::value_parser!(Shell), hide = true)]
    completions: Option<Shell>,

    /// Diff mode: compare two files (like `diff`)
    #[arg(short = 'D', long = "diff", num_args = 2, value_names = ["file1", "file2"])]
    diff: Option<Vec<String>>,

    /// Force plain text output
    #[arg(short = 'A', long = "ascii")]
    ascii: bool,

    /// Display raw bytes (no processing)
    #[arg(short = 'B', long = "binary")]
    binary: bool,

    /// Show detected file type (like `file` command)
    #[arg(short = 'T', long = "type")]
    show_type: bool,

    /// Number lines (-n: all, -b: non-blank)
    #[arg(short = 'n', long = "number", conflicts_with = "number_nonblank")]
    number: bool,

    /// Number non-blank lines
    #[arg(short = 'b', long = "number-nonblank", conflicts_with = "number")]
    number_nonblank: bool,

    /// Squeeze consecutive blank lines into one
    #[arg(short = 's', long = "squeeze-blank")]
    squeeze_blank: bool,

    /// Apply sed-like substitution (e.g. s/foo/bar/)
    #[arg(short = 'e', long = "edit", value_name = "expression")]
    edit: Option<String>,
}

enum FileKind {
    Markdown,
    Docx,
    Gzip,
    Image,
    Pdf,
    Archive,
    Media,
    Json,
    Yaml,
    Toml,
    Csv,
    Log,
    PlainText,
}

fn detect_kind(data: &[u8], path: &Path) -> FileKind {
    // Infer by magic bytes
    match infer::get(data) {
        Some(kind) => match kind.mime_type() {
            "application/gzip" => {
                // Check if it's a tar.gz by extension
                return FileKind::Gzip;
            }
            "application/zip" => {
                if path.extension().and_then(|e| e.to_str()) == Some("docx") {
                    return FileKind::Docx;
                }
                return FileKind::Archive;
            }
            "application/pdf" => return FileKind::Pdf,
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            | "image/bmp" | "image/tiff" => return FileKind::Image,
            "audio/mpeg" | "audio/flac" | "audio/ogg" | "audio/wav"
            | "audio/aac" | "audio/mp4" | "video/mp4" | "video/x-matroska"
            | "video/webm" | "audio/x-m4a" => return FileKind::Media,
            _ => {}
        },
        None => {}
    }

    // Check extension-based hints
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        match ext.to_lowercase().as_str() {
            "md" | "markdown" => return FileKind::Markdown,
            "docx" => return FileKind::Docx,
            "gz" | "gzip" => return FileKind::Gzip,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" => {
                return FileKind::Image;
            }
            "pdf" => return FileKind::Pdf,
            "zip" | "tar" | "tgz" | "tbz2" | "xz" | "bz2" => return FileKind::Archive,
            "mp3" | "flac" | "ogg" | "wav" | "aac" | "m4a" | "mp4" | "mkv" | "webm" | "opus" => {
                return FileKind::Media;
            }
            "json" => return FileKind::Json,
            "yaml" | "yml" => return FileKind::Yaml,
            "toml" => return FileKind::Toml,
            "csv" | "tsv" => return FileKind::Csv,
            "log" => return FileKind::Log,
            _ => {}
        }
    }

    // Check if it looks like markdown
    if looks_like_markdown(data) {
        return FileKind::Markdown;
    }

    // Detect JSON by looking at first non-whitespace char
    let first_nonws = data.iter().find(|&&b| b != b' ' && b != b'\t' && b != b'\n' && b != b'\r');
    if first_nonws == Some(&b'{') || first_nonws == Some(&b'[') {
        // Verify it parses
        let s = String::from_utf8_lossy(data);
        if serde_json::from_str::<serde_json::Value>(&s).is_ok() {
            return FileKind::Json;
        }
    }

    // Detect YAML: starts with key: value, or ---
    if let Some(&b) = first_nonws {
        if b == b'-' && data.len() > 3 && data[..3].as_ref() == b"---" {
            return FileKind::Yaml;
        }
        if data.iter().take(5).any(|&b| b == b':') {
            let s = String::from_utf8_lossy(data);
            let lines: Vec<&str> = s.lines().take(10).collect();
            if lines.iter().any(|l| l.trim().starts_with(|c: char| c.is_ascii_alphabetic()) && l.contains(':')) {
                // Could be YAML or TOML — check for = instead of :
                if lines.iter().any(|l| l.trim().contains('=') && !l.trim().starts_with('#')) {
                    return FileKind::Toml;
                }
                return FileKind::Yaml;
            }
        }
    }

    // Detect CSV: contains comma-separated values
    if first_nonws.map_or(false, |&b| b.is_ascii_alphanumeric()) {
        let s = String::from_utf8_lossy(data);
        let lines: Vec<&str> = s.lines().take(10).collect();
        if lines.len() >= 2 {
            let comma_count = lines[0].matches(',').count();
            if comma_count >= 1 {
                let consistent = lines.iter().skip(1)
                    .filter(|l| !l.trim().is_empty())
                    .all(|l| l.matches(',').count() == comma_count || l.matches(',').count() == 0);
                if consistent {
                    return FileKind::Csv;
                }
            }
            let tab_count = lines[0].matches('\t').count();
            if tab_count >= 1 {
                let consistent = lines.iter().skip(1)
                    .filter(|l| !l.trim().is_empty())
                    .all(|l| l.matches('\t').count() == tab_count || l.matches('\t').count() == 0);
                if consistent {
                    return FileKind::Csv;
                }
            }
        }
    }

    // Detect TOML: contains [section] or key = value patterns

    // Detect log files: contains timestamps or log levels
    if first_nonws.map_or(false, |&b| b.is_ascii_alphanumeric()) {
        let sample = String::from_utf8_lossy(data);
        let lower = sample.to_lowercase();
        let log_keywords = ["error", "warn", "info", "debug", "trace", "fatal", "panic"];
        let has_level = log_keywords.iter().any(|k| lower.contains(k));
        let has_timestamp = sample.contains(|c: char| c.is_ascii_digit()) && (sample.contains('-') || sample.contains(':'));
        if has_level || has_timestamp {
            return FileKind::Log;
        }
    }

    FileKind::PlainText
}

fn looks_like_markdown(data: &[u8]) -> bool {
    let s = String::from_utf8_lossy(data);
    let first_lines: Vec<&str> = s.lines().take(10).collect();
    if first_lines.is_empty() {
        return false;
    }

    // Must start with a heading to be markdown
    let first = first_lines[0].trim();
    if !(first.starts_with("# ")
        || first.starts_with("## ")
        || first.starts_with("### ")
        || first.starts_with("#### ")
        || first.starts_with("##### ")
        || first.starts_with("###### "))
    {
        return false;
    }

    // And must contain another markdown construct among first lines
    first_lines.iter().skip(1).any(|line| {
        let t = line.trim();
        t.starts_with("```")
            || t.starts_with("| ") || t.ends_with('|')
            || t.starts_with("> ")
            || t == "---"
            || t.starts_with("- ") || t.starts_with("* ")
            || t.starts_with("1. ") || t.starts_with("1)")
    })
}

fn describe_kind(data: &[u8], path: &Path) -> String {
    // Try magic bytes first
    if let Some(kind) = infer::get(data) {
        let mime = kind.mime_type();
        let readable = readable_file_kind(mime, path);
        return format!("{mime}: {}", readable);
    }

    // Fallback: extension + look_like
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        match ext.to_lowercase().as_str() {
            "md" | "markdown" => return "text/markdown: Markdown document".into(),
            "docx" => return "application/vnd.openxmlformats-officedocument.wordprocessingml.document: Word document".into(),
            "gz" | "gzip" => return "application/gzip: gzip compressed data".into(),
            "txt" => return "text/plain: ASCII text".into(),
            "rs" => return "text/rust: Rust source code".into(),
            "py" => return "text/x-python: Python script".into(),
            "toml" => return "text/x-toml: TOML configuration".into(),
            "json" => return "application/json: JSON data".into(),
            "yaml" | "yml" => return "text/yaml: YAML document".into(),
            "html" | "htm" => return "text/html: HTML document".into(),
            "css" => return "text/css: CSS stylesheet".into(),
            "js" | "mjs" => return "text/javascript: JavaScript source".into(),
            "sh" => return "text/x-shellscript: Shell script".into(),
            "png" => return "image/png: PNG image".into(),
            "jpg" | "jpeg" => return "image/jpeg: JPEG image".into(),
            "gif" => return "image/gif: GIF image".into(),
            "webp" => return "image/webp: WebP image".into(),
            "bmp" => return "image/bmp: BMP image".into(),
            "tiff" | "tif" => return "image/tiff: TIFF image".into(),
            _ => {}
        }
    }

    if looks_like_markdown(data) {
        return "text/plain: Markdown document (heuristic)".into();
    }

    // Check if it's printable text
    let printable_ratio = data.iter().take(4096).filter(|&&b| b.is_ascii_graphic() || b.is_ascii_whitespace()).count();
    let sample_len = data.len().min(4096);
    if sample_len > 0 && printable_ratio > sample_len / 2 {
        if data.contains(&b'\0') {
            return "text/plain: Unicode text".into();
        }
        return "text/plain: ASCII text".into();
    }

    "application/octet-stream: data".into()
}

fn readable_file_kind(mime: &str, path: &Path) -> String {
    match mime {
        "image/png" => "PNG image".into(),
        "image/jpeg" => "JPEG image".into(),
        "image/gif" => "GIF image".into(),
        "image/webp" => "WebP image".into(),
        "image/bmp" => "BMP image".into(),
        "image/tiff" => "TIFF image".into(),
        "application/gzip" => "gzip compressed data".into(),
        "application/zip" => {
            if path.extension().and_then(|e| e.to_str()) == Some("docx") {
                "Word document".into()
            } else {
                "ZIP archive".into()
            }
        }
        "application/pdf" => "PDF document".into(),
        "application/json" => "JSON data".into(),
        "text/yaml" | "text/x-yaml" => "YAML document".into(),
        "text/x-log" => "log file".into(),
        "inode/directory" => "directory".into(),
        "text/plain" => "ASCII text".into(),
        "text/html" => "HTML document".into(),
        "application/xml" | "text/xml" => "XML document".into(),
        "application/x-elf" => "ELF executable".into(),
        "application/x-sharedlib" => "ELF shared library".into(),
        "application/x-executable" => "ELF executable".into(),
        "inode/symlink" => "symbolic link".into(),
        "audio/mpeg" => "MP3 audio".into(),
        "audio/flac" => "FLAC audio".into(),
        "audio/ogg" => "OGG audio".into(),
        "audio/wav" => "WAV audio".into(),
        "audio/aac" => "AAC audio".into(),
        "audio/mp4" | "audio/x-m4a" => "M4A audio".into(),
        "video/mp4" => "MP4 video".into(),
        "video/x-matroska" => "MKV video".into(),
        "video/webm" => "WebM video".into(),
        _ => mime.into(),
    }
}

fn is_elf(data: &[u8]) -> bool {
    data.len() > 4 && data[0] == 0x7f && data[1] == b'E' && data[2] == b'L' && data[3] == b'F'
}

fn is_binary(data: &[u8]) -> bool {
    let sample_len = data.len().min(8192);
    if sample_len == 0 { return false; }
    let nul_count = data.iter().take(8192).filter(|&&b| b == 0).count();
    let non_printable = data.iter().take(8192).filter(|&&b| b != 0 && !b.is_ascii_graphic() && !b.is_ascii_whitespace()).count();
    // If more than 1% null bytes OR more than 30% non-printable non-null bytes
    nul_count > sample_len / 100 || non_printable > sample_len * 3 / 10
}

/// Returns true if this is the 3rd consecutive call on the same binary path.
fn check_binary_repeat(path: &str) -> bool {
    let dir = Path::new(STATE_DIR);
    let _ = fs::create_dir_all(dir);
    let state_file = dir.join("bin");

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Read previous state
    let (prev_path, prev_time, count) = match fs::read_to_string(&state_file) {
        Ok(s) => {
            let parts: Vec<&str> = s.split('\n').collect();
            if parts.len() >= 3 {
                let p = parts[0].to_string();
                let t = parts[1].parse::<u64>().unwrap_or(0);
                let c = parts[2].parse::<u32>().unwrap_or(0);
                (p, t, c)
            } else {
                (String::new(), 0, 0)
            }
        }
        Err(_) => (String::new(), 0, 0),
    };

    // Reset if more than 2 seconds apart (not consecutive)
    let same_path = prev_path == path;
    let elapsed = now.saturating_sub(prev_time);
    let close_enough = elapsed <= 2;

    let (new_count, trigger) = if same_path && close_enough {
        let c = count + 1;
        (c, c >= 2)
    } else {
        (0, false)
    };

    // Debug: eprintln!("dbg: same={same_path} elapsed={elapsed}s count_in={count} new_count={new_count} trigger={trigger}");


    // Write new state
    let content = format!("{path}\n{now}\n{new_count}\n");
    let _ = fs::write(&state_file, content);

    trigger
}

pub fn cat_hex(data: &[u8]) {
    let mut stdout = io::stdout();
    let columns = 16;
    let lines = data.len().div_ceil(columns);
    let (term_height, _) = pager::terminal_size();
    let page_lines = term_height.saturating_sub(2).max(5);
    let total_pages = lines.div_ceil(page_lines);
    let mut current_page: usize = 0;

    loop {
        let start_line = current_page * page_lines;
        let end_line = (start_line + page_lines).min(lines);

        for line_idx in start_line..end_line {
            let offset = line_idx * columns;
            let row = &data[offset..data.len().min(offset + columns)];
            let _ = write!(stdout, "\x1b[2m{:08x}  \x1b[0m", offset);

            for (i, byte) in row.iter().enumerate() {
                if i == 8 { let _ = write!(stdout, " "); }
                if *byte == 0 {
                    let _ = write!(stdout, "\x1b[2m{:02x}\x1b[0m ", byte);
                } else if byte.is_ascii_graphic() || byte.is_ascii_whitespace() {
                    let _ = write!(stdout, "\x1b[33m{:02x}\x1b[0m ", byte);
                } else {
                    let _ = write!(stdout, "{:02x} ", byte);
                }
            }

            let remaining = columns - row.len();
            if remaining > 0 {
                if row.len() < 8 { let _ = write!(stdout, " "); }
                for _ in 0..remaining {
                    let _ = write!(stdout, "   ");
                }
            }

            let _ = write!(stdout, " \x1b[2m|\x1b[0m");
            for &byte in row {
                if byte.is_ascii_graphic() || byte == b' ' {
                    let _ = write!(stdout, "{}", byte as char);
                } else {
                    let _ = write!(stdout, "\x1b[2m.\x1b[0m");
                }
            }
            let _ = writeln!(stdout, "\x1b[2m|\x1b[0m");
        }

        let end_offset = end_line * columns;
        let _ = writeln!(stdout, "\x1b[2m{:08x}\x1b[0m", end_offset);

        if total_pages > 1 {
            let action = pager::page_footer(
                &mut stdout, current_page, total_pages,
                start_line * columns, end_line * columns, data.len(),
            );
            match action {
                pager::PageAction::Quit => break,
                pager::PageAction::Next(_) => {
                    if current_page + 1 < total_pages {
                        current_page += 1;
                    }
                }
                pager::PageAction::Prev(_) => {
                    if current_page > 0 {
                        current_page -= 1;
                    }
                }
                pager::PageAction::None | pager::PageAction::Search(_) | pager::PageAction::Goto(_) => {}
            }
        } else {
            break;
        }
    }
}

fn cat_plain(data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    print!("{s}");
}

fn cat_plain_with_opts(data: &[u8], number: bool, number_nonblank: bool, squeeze: bool, edit: Option<&str>) {
    let s = String::from_utf8_lossy(data);
    let mut lines: Vec<&str> = s.lines().collect();

    // Squeeze consecutive blank lines
    if squeeze {
        let mut squeezed = Vec::new();
        let mut prev_blank = false;
        for line in &lines {
            let blank = line.trim().is_empty();
            if blank && prev_blank {
                continue;
            }
            squeezed.push(*line);
            prev_blank = blank;
        }
        lines = squeezed;
    }

    // Sed-like substitution
    let re = edit.and_then(|expr| {
        let parts: Vec<&str> = expr.split('/').collect();
        if parts.len() >= 3 && parts[0] == "s" {
            let pattern = parts[1];
            let replacement = parts[2];
            regex_lite::Regex::new(pattern).ok().map(|r| (r, replacement.to_string()))
        } else {
            None
        }
    });

    // Output
    let mut line_num = 0u64;
    for line in &lines {
        let blank = line.trim().is_empty();

        // Determine output text after substitution
        let output = if let Some((ref re, ref replacement)) = re {
            re.replace(line, replacement.as_str()).to_string()
        } else {
            line.to_string()
        };

        // Line number
        if number {
            line_num += 1;
            println!("{:6}\t{output}", line_num);
        } else if number_nonblank && !blank {
            line_num += 1;
            println!("{:6}\t{output}", line_num);
        } else if number_nonblank && blank {
            println!("       \t{output}");
        } else {
            println!("{output}");
        }
    }
}

fn cat_gz(data: &[u8]) {
    let mut decoder = GzDecoder::new(data);
    let mut buf = Vec::new();
    match decoder.read_to_end(&mut buf) {
        Ok(_) => {
            let inner = String::from_utf8_lossy(&buf);
            print!("{inner}");
        }
        Err(e) => {
            eprintln!("ccat: gzip decompression error: {e}");
        }
    }
}

fn cat_file(path: &str, force_ascii: bool, force_binary: bool, show_type: bool, has_opts: bool, number: bool, number_nonblank: bool, squeeze: bool, edit: Option<&str>) -> io::Result<()> {
    let path_obj = Path::new(path);

    // If it's a directory, show file-like summary for each entry
    if path_obj.is_dir() {
        let entries = fs::read_dir(path).map_err(|e| {
            eprintln!("ccat: {path}: {e}");
            e
        })?;
        for entry in entries {
            let entry = entry?;
            let entry_path = entry.path();
            let entry_name = entry_path.display();
            if entry.file_type()?.is_dir() {
                println!("{entry_name}: directory");
            } else if let Ok(data) = fs::read(&entry_path) {
                let desc = describe_kind(&data, &entry_path);
                println!("{entry_name}: {desc}");
            }
        }
        return Ok(());
    }

    // Read from stdin if path is "-"
    let data = if path == "-" {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)?;
        buf
    } else {
        fs::read(path).map_err(|e| {
            eprintln!("ccat: {path}: {e}");
            e
        })?
    };

    if data.is_empty() {
        return Ok(());
    }

    if show_type || force_binary {
        if show_type {
            let desc = describe_kind(&data, path_obj);
            eprintln!("ccat: {path}: {desc}");
        }
        if force_binary {
            cat_plain(&data);
            return Ok(());
        }
    }

    let raw = detect_kind(&data, path_obj);

    if force_ascii {
        match raw {
            FileKind::Gzip => cat_gz(&data),
            _ => cat_plain(&data),
        }
        return Ok(());
    }

    // If the content is binary and we don't have a specific handler,
    // just show type info or skip
    match raw {
        FileKind::Markdown => cat_markdown::cat_markdown(&data),
        FileKind::Docx => cat_docx::cat_docx(&data),
        FileKind::Gzip => cat_gz(&data),
        FileKind::Image => cat_image::cat_image(&data),
        FileKind::Pdf => cat_pdf::cat_pdf(&data),
        FileKind::Archive => cat_archive::cat_archive(&data, path),
        FileKind::Media => cat_media::cat_media(&data),
        FileKind::Json => cat_json::cat_json(&data),
        FileKind::Yaml => cat_yaml::cat_yaml(&data),
        FileKind::Toml => cat_toml::cat_toml(&data),
        FileKind::Csv => cat_csv::cat_csv(&data),
        FileKind::Log => cat_log::cat_log(&data),
        FileKind::PlainText => {
            if is_binary(&data) {
                if !show_type {
                    let canonical = path_obj.canonicalize().ok()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string());

                    if check_binary_repeat(&canonical) {
                        // Check if it's an ELF binary -> disassemble
                        if is_elf(&data) {
                            eprintln!("ccat: {path}: ELF binary (disassembly):");
                            cat_disasm::disassemble_elf(&data);
                        } else {
                            eprintln!("ccat: {path}: binary (hex dump):");
                            cat_hex(&data);
                        }
                    } else {
                        let desc = describe_kind(&data, path_obj);
                        eprintln!("ccat: {path}: {desc} (repeat to hex dump)");
                    }
                }
            } else {
                if has_opts {
                    cat_plain_with_opts(&data, number, number_nonblank, squeeze, edit);
                } else {
                    cat_plain(&data);
                }
            }
        }
    }

    Ok(())
}

fn main() {
    let mut cli = Cli::parse();

    // Generate shell completions
    if let Some(shell) = cli.completions.take() {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
        return;
    }

    // Respect NO_COLOR
    if std::env::var("NO_COLOR").is_ok() && !std::env::var("NO_COLOR").unwrap_or_default().is_empty() {
        // SAFETY: Setting TERM before any color output is safe
        unsafe { std::env::set_var("TERM", "dumb"); }
    }

    let _force_ascii = cli.ascii;
    let _force_binary = cli.binary;
    let _show_type = cli.show_type;
    let number = cli.number;
    let number_nonblank = cli.number_nonblank;
    let squeeze = cli.squeeze_blank;
    let edit = cli.edit.as_deref();

    // Processing flags apply to plain text output
    let has_opts = number || number_nonblank || squeeze || edit.is_some();

    // Diff mode
    if let Some(paths) = cli.diff {
        if paths.len() != 2 {
            eprintln!("ccat: diff requires exactly 2 files");
            return;
        }
        let data = match fs::read(&paths[0]) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("ccat: {}: {e}", paths[0]);
                return;
            }
        };
        cat_diff::cat_diff(&data, &paths[0], &paths[1]);
        return;
    }

    if cli.files.is_empty() {
        // Read from stdin
        let mut buf = Vec::new();
        if io::stdin().read_to_end(&mut buf).is_ok() && !buf.is_empty() {
            let force_ascii = cli.ascii;
            let force_binary = cli.binary;
            let show_type = cli.show_type;

            if force_binary {
                cat_plain(&buf);
                return;
            }

            let kind = detect_kind(&buf, Path::new(""));
            if show_type {
                let desc = describe_kind(&buf, Path::new(""));
                eprintln!("ccat: stdin: {desc}");
            }

            if force_ascii {
                match kind {
                    FileKind::Gzip => cat_gz(&buf),
                    _ => cat_plain(&buf),
                }
                return;
            }

            match kind {
                FileKind::Markdown => cat_markdown::cat_markdown(&buf),
                FileKind::Docx => cat_docx::cat_docx(&buf),
                FileKind::Gzip => cat_gz(&buf),
                FileKind::Image => cat_image::cat_image(&buf),
                FileKind::Pdf => cat_pdf::cat_pdf(&buf),
                FileKind::Archive => cat_archive::cat_archive(&buf, "stdin"),
                FileKind::Media => cat_media::cat_media(&buf),
                FileKind::Json => cat_json::cat_json(&buf),
                FileKind::Yaml => cat_yaml::cat_yaml(&buf),
                FileKind::Toml => cat_toml::cat_toml(&buf),
                FileKind::Csv => cat_csv::cat_csv(&buf),
                FileKind::Log => cat_log::cat_log(&buf),
                FileKind::PlainText => {
                    if is_binary(&buf) {
                        if !show_type {
                            eprintln!("ccat: stdin: binary data (use -B for raw output)");
                        }
                    } else {
                        cat_plain(&buf);
                    }
                }
            }
        }
        return;
    }

    for (i, file) in cli.files.iter().enumerate() {
        if i > 0 {
            println!();
        }
        if let Err(e) = cat_file(file, cli.ascii, cli.binary, cli.show_type, has_opts, number, number_nonblank, squeeze, edit) {
            if e.kind() != io::ErrorKind::Other {
                // We already printed the error in cat_file
            }
        }
    }
}
