use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use flate2::read::GzDecoder;

mod cat_markdown;
mod cat_docx;
mod cat_image;

const STATE_DIR: &str = "/tmp/ccat-state";

/// ccat - An enhanced cat tool with automatic file type detection
#[derive(Parser)]
#[command(name = "ccat", version, about = "Enhanced cat: auto-detect and display markdown, docx, images, and gz files")]
struct Cli {
    /// File(s) to display (or use - to read stdin). When a directory is given,
    /// shows a summary similar to `file`.
    files: Vec<String>,

    /// Force plain text output
    #[arg(short = 'A', long = "ascii")]
    ascii: bool,

    /// Display raw bytes (no processing)
    #[arg(short = 'B', long = "binary")]
    binary: bool,

    /// Show detected file type (like `file` command)
    #[arg(short = 'T', long = "type")]
    show_type: bool,
}

enum FileKind {
    Markdown,
    Docx,
    Gzip,
    Image,
    PlainText,
}

fn detect_kind(data: &[u8], path: &Path) -> FileKind {
    // Infer by magic bytes
    match infer::get(data) {
        Some(kind) => match kind.mime_type() {
            "application/gzip" => return FileKind::Gzip,
            "application/zip" => {
                // .docx files are zips with specific internal structure
                if path.extension().and_then(|e| e.to_str()) == Some("docx") {
                    return FileKind::Docx;
                }
            }
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            | "image/bmp" | "image/tiff" => return FileKind::Image,
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
            _ => {}
        }
    }

    // Check if it looks like markdown (starts with common markdown syntax)
    if looks_like_markdown(data) {
        return FileKind::Markdown;
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
        "inode/directory" => "directory".into(),
        "text/plain" => "ASCII text".into(),
        "application/json" => "JSON data".into(),
        "text/html" => "HTML document".into(),
        "application/xml" | "text/xml" => "XML document".into(),
        "application/x-elf" => "ELF executable".into(),
        "application/x-sharedlib" => "ELF shared library".into(),
        "application/x-executable" => "ELF executable".into(),
        "inode/symlink" => "symbolic link".into(),
        _ => mime.into(),
    }
}

fn is_binary(data: &[u8]) -> bool {
    let sample = data.iter().take(8192);
    let nul_count = sample.filter(|&&b| b == 0).count();
    // If more than 1% null bytes in first 8KB, it's binary
    let sample_len = data.len().min(8192);
    sample_len > 0 && nul_count > sample_len / 100
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

fn cat_hex(data: &[u8]) {
    let mut stdout = io::stdout();
    let columns = 16;
    let lines = data.len().div_ceil(columns);
    let page_size = 24; // lines per page
    let total_pages = lines.div_ceil(page_size);
    let mut current_page: usize = 0;

    loop {
        let start = current_page * page_size * columns;
        let end = ((current_page + 1) * page_size * columns).min(data.len());
        let chunk = &data[start..end];

        for (row_idx, row) in chunk.chunks(columns).enumerate() {
            let offset = start + row_idx * columns;
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
                for i in 0..remaining {
                    if row.len() + i == 8 { let _ = write!(stdout, " "); }
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

        // Footer line
        let end_offset = end.min(data.len());
        let _ = writeln!(stdout, "\x1b[2m{:08x}\x1b[0m", end_offset);

        // Page indicator + prompt
        if total_pages > 1 {
            let _ = write!(
                stdout,
                "\x1b[2m-- Page {}/{} -- q:quit n:next p:prev  \x1b[0m",
                current_page + 1,
                total_pages
            );
            let _ = stdout.flush();

            // Read single keypress
            let mut buf = [0u8; 1];
            // Set terminal to raw mode
            let _ = std::process::Command::new("sh")
                .args(["-c", "stty raw -echo < /dev/tty 2>/dev/null"])
                .status();
            let _ = io::stdin().read_exact(&mut buf);
            let _ = std::process::Command::new("sh")
                .args(["-c", "stty sane < /dev/tty 2>/dev/null"])
                .status();

            match buf[0] {
                b'q' | 0x03 | 0x1b => break, // q / Ctrl-C / Escape
                b'n' | b' ' => {
                    if current_page + 1 < total_pages {
                        current_page += 1;
                    }
                }
                b'p' | b'b' => {
                    if current_page > 0 {
                        current_page -= 1;
                    }
                }
                _ => {}
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

fn cat_file(path: &str, force_ascii: bool, force_binary: bool, show_type: bool) -> io::Result<()> {
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
        FileKind::PlainText => {
            if is_binary(&data) {
                if !show_type {
                    let canonical = path_obj.canonicalize().ok()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string());

                    if check_binary_repeat(&canonical) {
                        eprintln!("ccat: {path}: binary (hex dump):");
                        cat_hex(&data);
                    } else {
                        let desc = describe_kind(&data, path_obj);
                        eprintln!("ccat: {path}: {desc} (repeat to hex dump)");
                    }
                }
            } else {
                cat_plain(&data);
            }
        }
    }

    Ok(())
}

fn main() {
    let cli = Cli::parse();

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
        if let Err(e) = cat_file(file, cli.ascii, cli.binary, cli.show_type) {
            if e.kind() != io::ErrorKind::Other {
                // We already printed the error in cat_file
            }
        }
    }
}
