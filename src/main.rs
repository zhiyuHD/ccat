use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use clap::CommandFactory;
use clap_complete::Shell;
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
mod cat_source;
mod cat_html;
mod cat_follow;
mod serve;
mod config;
mod cat_tree;
mod color_scheme;
mod cat_elf;
mod pager;
mod cat_schema;
mod cat_search;
mod cat_inspect;

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

    /// Follow mode: watch file for changes (like tail -f)
    #[arg(short = 'f', long = "follow")]
    follow: bool,

    /// In follow mode: start from the last N lines
    #[arg(long = "lines", value_name = "N", default_value_t = 10, requires = "follow")]
    lines: usize,

    /// Generate HTML output for browser viewing
    #[arg(long = "html")]
    html: bool,

    /// Start HTTP server to serve files as HTML (e.g. --serve 8080)
    #[arg(long = "serve", value_name = "PORT")]
    serve: Option<u16>,

    /// Display directory as a tree with file types, sizes, and line counts
    #[arg(short = 'r', long = "tree")]
    tree: bool,

    /// In --tree mode: also show hidden files (dotfiles)
    #[arg(long = "all", requires = "tree")]
    show_all: bool,

    /// In --tree mode: maximum recursion depth (default: unlimited)
    #[arg(long = "depth", value_name = "N", requires = "tree")]
    tree_depth: Option<usize>,

    /// Color scheme: auto (default), dark, light
    #[arg(long = "color-scheme", value_name = "SCHEME", default_value = "auto")]
    color_scheme: String,

    /// Side-by-side diff view (requires --diff)
    #[arg(long = "side-by-side", requires = "diff")]
    side_by_side: bool,

    /// Syntax highlighting theme for source code (requires source highlighting).
    /// Use --list-themes to see available options. Overrides auto-detection.
    #[arg(long = "theme", value_name = "NAME")]
    theme: Option<String>,

    /// List available syntax highlighting themes and exit.
    #[arg(long = "list-themes")]
    list_themes: bool,

    /// ELF binary introspection: show headers, sections, segments, and symbols
    #[arg(long = "elf")]
    elf: bool,

    /// Show inferred schema for structured data (JSON, TOML, YAML, CSV)
    #[arg(long = "schema")]
    schema: bool,

    /// Show detailed file inspection (type, size, entropy, hash, stats, etc.)
    #[arg(short = 'i', long = "inspect")]
    inspect: bool,

    /// Search for regex pattern in files (grep mode)
    #[arg(short = 'g', long = "search", value_name = "PATTERN", conflicts_with_all = &["diff", "tree", "elf", "schema", "html", "follow", "serve"])]
    search: Option<String>,

    /// Number of context lines for --search (default: 2)
    #[arg(short = 'C', long = "context", value_name = "N", default_value_t = 2, requires = "search")]
    context: usize,

    /// Only show match counts per file (with --search)
    #[arg(short = 'c', long = "count", requires = "search")]
    count: bool,

    /// Only show filenames with matches (with --search)
    #[arg(short = 'l', long = "files-with-matches", requires = "search")]
    files_with_matches: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
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
    SourceCode,
    UnifiedDiff,
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

    // Detect unified diff from stdin: git diff (diff --git) or unified diff (--- / +++)
    if first_nonws == Some(&b'd') || first_nonws == Some(&b'-') {
        if cat_diff::is_unified_diff(data) {
            return FileKind::UnifiedDiff;
        }
    }

    // Detect YAML: starts with key: value, or ---
    if let Some(&b) = first_nonws {
        if b == b'-' && data.len() > 3 && &data[..3] == b"---" {
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
        let has_timestamp = sample.contains(|c: char| c.is_ascii_digit())
            && (sample.contains('-') || sample.contains(':'));
        if has_level || has_timestamp {
            return FileKind::Log;
        }
    }

    // Detect source code by extension
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext_lower = ext.to_lowercase();
        let source_extensions = [
            "rs", "py", "js", "ts", "jsx", "tsx", "go", "rb", "java", "kt", "scala",
            "swift", "c", "h", "cpp", "hpp", "cc", "cxx", "hh", "hxx", "c++", "h++",
            "cs", "fs", "fsx", "clj", "cljs", "lisp", "cl", "el", "scm", "rkt",
            "hs", "lhs", "ex", "exs", "erl", "hrl", "elm", "nim", "cr", "d",
            "php", "pl", "pm", "t", "pod", "ps1", "psm1", "bat", "sh", "bash", "zsh",
            "awk", "sed", "sql", "r", "m", "mm", "pas", "inc",
            "sass", "scss", "less", "styl", "css",
            "dockerfile", "cmake", "makefile", "gnumakefile",
        ];
        if source_extensions.contains(&ext_lower.as_str()) {
            return FileKind::SourceCode;
        }
    }
    // Check by base name (Dockerfile, Makefile, etc.) — file may have no extension
    if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
        let fname_lower = fname.to_lowercase();
        let exact_names = [
            "dockerfile", "makefile", "cmakelists.txt", "justfile",
            "gemfile", "rakefile", "snakefile",
        ];
        if exact_names.contains(&fname_lower.as_str()) {
            return FileKind::SourceCode;
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
    // Exclude bytes >= 0x80 (UTF-8 multi-byte continuation/leading bytes) so
    // that UTF-8 encoded text like "café" or "日本語" is not falsely flagged
    // as binary.
    let non_printable = data.iter().take(8192).filter(|&&b| b != 0 && !b.is_ascii_graphic() && !b.is_ascii_whitespace() && b < 0x80).count();
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
                pager::PageAction::None | pager::PageAction::Search | pager::PageAction::Goto(_) => {}
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
        FileKind::UnifiedDiff => cat_diff::cat_diff_stdin(&data),
        FileKind::SourceCode => cat_source::cat_source(&data, path),
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

    // Handle --list-themes: show available syntect themes and exit
    if cli.list_themes {
        let ts = syntect::highlighting::ThemeSet::load_defaults();
        let mut names: Vec<&String> = ts.themes.keys().collect();
        names.sort();
        for name in &names {
            println!("{name}");
        }
        return;
    }

    // Load config and merge with CLI args (CLI overrides config)
    let merged = config::MergedConfig::new(
        &cli.color_scheme,
        cli.theme.take(),
        cli.number,
        cli.number_nonblank,
        cli.squeeze_blank,
    );
    cli.color_scheme = merged.color_scheme;
    cli.number = merged.number;
    cli.number_nonblank = merged.number_nonblank;
    cli.squeeze_blank = merged.squeeze_blank;

    // Store the chosen theme in the state dir so cat_source can read it
    if let Some(ref theme_name) = merged.theme {
        let dir = Path::new(STATE_DIR);
        let _ = fs::create_dir_all(dir);
        let _ = fs::write(dir.join("theme"), theme_name);
    } else {
        // Clear theme override
        let _ = fs::remove_file(Path::new(STATE_DIR).join("theme"));
    }

    // Respect NO_COLOR
    if std::env::var("NO_COLOR").is_ok()
        && !std::env::var("NO_COLOR").unwrap_or_default().is_empty()
    {
        // SAFETY: Setting TERM before any color output is safe
        unsafe {
            std::env::set_var("TERM", "dumb");
        }
    }
    // Initialize color scheme based on CLI arg or auto-detection
    {
        let cs = cli.color_scheme.to_lowercase();
        let theme = match cs.as_str() {
            "dark" => Some(color_scheme::Theme::Dark),
            "light" => Some(color_scheme::Theme::Light),
            _ => None, // auto
        };
        color_scheme::force_theme(theme);
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

    // Serve mode: start HTTP server
    if let Some(port) = cli.serve {
        if cli.files.is_empty() {
            eprintln!("ccat: --serve requires at least one file path");
            return;
        }
        if let Err(e) = serve::serve_files(&cli.files, port) {
            eprintln!("ccat: --serve: {e}");
        }
        return;
    }

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
        if cli.side_by_side {
            cat_diff::cat_diff_sxs(&data, &paths[0], &paths[1]);
        } else {
            cat_diff::cat_diff(&data, &paths[0], &paths[1]);
        }
        return;
    }

    // Search mode
    if let Some(pattern) = cli.search {
        if cli.files.is_empty() {
            eprintln!("ccat: --search requires at least one file path");
            return;
        }
        let opts = cat_search::SearchOpts {
            pattern,
            context_lines: cli.context,
            count_only: cli.count,
            files_with_matches: cli.files_with_matches,
        };
        if let Err(e) = cat_search::search_main(&opts, &cli.files) {
            eprintln!("ccat: --search: {e}");
        }
        return;
    }

    if cli.files.is_empty() {
        // Read from stdin
        let mut buf = Vec::new();
        if io::stdin().read_to_end(&mut buf).is_ok() && !buf.is_empty() {
            // Inspect stdin
            if cli.inspect {
                cat_inspect::inspect_stdin(&buf);
                return;
            }

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
                FileKind::UnifiedDiff => cat_diff::cat_diff_stdin(&buf),
                FileKind::SourceCode => cat_source::cat_source(&buf, "stdin"),
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

    // Follow mode
    if cli.follow {
        if cli.files.is_empty() {
            eprintln!("ccat: --follow requires a file path");
            return;
        }
        for file in &cli.files {
            // Do a quick read of the first bytes to detect file type
            let data = match fs::read(file) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("ccat: {file}: {e}");
                    continue;
                }
            };
            if data.is_empty() {
                eprintln!("ccat: {file}: empty file, nothing to follow");
                continue;
            }
            let kind = detect_kind(&data, Path::new(file));
            let desc = describe_kind(&data, Path::new(file));
            eprintln!("\x1b[2mccat: {}: following {} (--lines {})\x1b[0m", file, desc, cli.lines);
            if let Err(e) = cat_follow::cat_follow(file, kind, cli.lines) {
                eprintln!("ccat: {file}: {e}");
            }
        }
        return;
    }

    // Tree mode
    if cli.tree {
        for dir in &cli.files {
            match cat_tree::print_tree(dir, cli.tree_depth, cli.show_all) {
                Ok(()) => {}
                Err(e) => eprintln!("ccat: --tree {dir}: {e}"),
            }
        }
        return;
    }

    for (i, file) in cli.files.iter().enumerate() {
        if i > 0 {
            println!();
        }

        // ELF introspection mode
        if cli.elf {
            let data = match fs::read(file) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("ccat: {file}: {e}");
                    continue;
                }
            };
            if data.is_empty() {
                eprintln!("ccat: {file}: empty file");
                continue;
            }
            cat_elf::cat_elf(&data);
            continue;
        }

        // Schema mode
        if cli.schema {
            let data = match fs::read(file) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("ccat: {file}: {e}");
                    continue;
                }
            };
            if data.is_empty() {
                eprintln!("ccat: {file}: empty file");
                continue;
            }
            cat_schema::print_schema(&data, Path::new(file));
            continue;
        }

        // Inspect mode
        if cli.inspect {
            let data = if file == "-" {
                let mut buf = Vec::new();
                if io::stdin().read_to_end(&mut buf).is_err() || buf.is_empty() {
                    eprintln!("ccat: stdin: empty");
                    continue;
                }
                buf
            } else {
                match fs::read(file) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("ccat: {file}: {e}");
                        continue;
                    }
                }
            };
            if data.is_empty() {
                if file == "-" { continue; }
                eprintln!("ccat: {file}: empty file");
                continue;
            }
            cat_inspect::inspect_file(&data, Path::new(file));
            continue;
        }

        if cli.html {
            // HTML mode: output HTML to stdout
            let data = match fs::read(file) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("ccat: {file}: {e}");
                    continue;
                }
            };
            if data.is_empty() {
                continue;
            }
            let path_obj = Path::new(file);
            let kind = detect_kind(&data, path_obj);
            let html = cat_html::cat_file_html(&data, kind, path_obj);
            print!("{html}");
        } else if let Err(e) = cat_file(file, cli.ascii, cli.binary, cli.show_type, has_opts, number, number_nonblank, squeeze, edit) {
            if e.kind() != io::ErrorKind::Other {
                // We already printed the error in cat_file
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ── detect_kind tests ──

    #[test]
    fn test_detect_markdown() {
        let data = b"# Hello\n\nThis is a paragraph.\n\n- list item\n";
        let kind = detect_kind(data, Path::new("test.md"));
        assert_eq!(kind, FileKind::Markdown, "markdown by heading + list");
    }

    #[test]
    fn test_detect_markdown_no_heading_no_match() {
        let data = b"Just a plain paragraph.\n\n> not a quote?\n";
        let kind = detect_kind(data, Path::new("test.txt"));
        assert_eq!(kind, FileKind::PlainText, "no heading = not markdown");
    }

    #[test]
    fn test_detect_json_by_extension() {
        let data = b"{\"key\": \"value\"}";
        let kind = detect_kind(data, Path::new("data.json"));
        assert_eq!(kind, FileKind::Json, "json by .json extension");
    }

    #[test]
    fn test_detect_json_by_content() {
        let data = b"{\"name\": \"ccat\", \"version\": 1}";
        let kind = detect_kind(data, Path::new("unknown"));
        assert_eq!(kind, FileKind::Json, "json by leading brace");
    }

    #[test]
    fn test_detect_json_array() {
        let data = b"[1, 2, 3]";
        let kind = detect_kind(data, Path::new("array.dat"));
        assert_eq!(kind, FileKind::Json, "json array by leading bracket");
    }

    #[test]
    fn test_detect_yaml_by_extension() {
        let data = b"key: value\nfoo: bar\n";
        let kind = detect_kind(data, Path::new("config.yaml"));
        assert_eq!(kind, FileKind::Yaml, "yaml by .yaml extension");
    }

    #[test]
    fn test_detect_yaml_doc_separator() {
        let data = b"---\nkey: value\n";
        let kind = detect_kind(data, Path::new("unknown"));
        assert_eq!(kind, FileKind::Yaml, "yaml by doc separator");
    }

    #[test]
    fn test_detect_toml_by_extension() {
        let data = b"[package]\nname = \"ccat\"\n";
        let kind = detect_kind(data, Path::new("Cargo.toml"));
        assert_eq!(kind, FileKind::Toml, "toml by extension");
    }

    #[test]
    fn test_detect_toml_heuristic() {
        let data = b"a: name = \"ccat\"\nversion = \"1.0\"\n";
        let kind = detect_kind(data, Path::new("unknown"));
        assert_eq!(kind, FileKind::Toml, "toml by key = value heuristic");
    }

    #[test]
    fn test_detect_csv_by_extension() {
        let data = b"a,b,c\n1,2,3\n";
        let kind = detect_kind(data, Path::new("data.csv"));
        assert_eq!(kind, FileKind::Csv, "csv by extension");
    }

    #[test]
    fn test_detect_csv_heuristic() {
        let data = b"name,age,city\nalice,30,nyc\nbob,25,sf\n";
        let kind = detect_kind(data, Path::new("unknown"));
        assert_eq!(kind, FileKind::Csv, "csv by comma heuristic");
    }

    #[test]
    fn test_detect_tsv() {
        let data = b"name\tage\tcity\nalice\t30\tnyc\n";
        let kind = detect_kind(data, Path::new("unknown"));
        assert_eq!(kind, FileKind::Csv, "tsv by tab heuristic");
    }

    #[test]
    fn test_detect_log_file() {
        let data = b"2024-01-01 12:00:00 [INFO] Server started\n";
        let kind = detect_kind(data, Path::new("server.log"));
        assert_eq!(kind, FileKind::Log, "log by extension");
    }

    #[test]
    fn test_detect_log_heuristic() {
        let data = b"2024-06-10 10:30:00 ERROR something broke\n";
        let kind = detect_kind(data, Path::new("unknown"));
        assert_eq!(kind, FileKind::Log, "log by timestamp + error level");
    }

    #[test]
    fn test_detect_unified_diff_git() {
        let data = b"diff --git a/src/main.rs b/src/main.rs\nindex abc..def\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,5 +1,6 @@\n fn main() {\n+    println!(\"hello\");\n }\n";
        let kind = detect_kind(data, Path::new("unknown"));
        assert_eq!(kind, FileKind::UnifiedDiff, "git diff by diff --git header");
    }

    #[test]
    fn test_detect_unified_diff_std() {
        let data = b"--- a/old.txt\n+++ b/new.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let kind = detect_kind(data, Path::new("unknown"));
        assert_eq!(kind, FileKind::UnifiedDiff, "unified diff by ---/+++ headers");
    }

    #[test]
    fn test_detect_yaml_not_confused_with_diff() {
        // Make sure YAML frontmatter (--- without path) is NOT detected as diff
        let data = b"---\nkey: value\nfoo: bar\n";
        let kind = detect_kind(data, Path::new("unknown"));
        assert_eq!(kind, FileKind::Yaml, "YAML frontmatter should stay YAML, not diff");
    }

    #[test]
    fn test_detect_source_rust() {
        let data = b"fn main() {}\n";
        let kind = detect_kind(data, Path::new("main.rs"));
        assert_eq!(kind, FileKind::SourceCode, "rust by extension");
    }

    #[test]
    fn test_detect_source_python() {
        let data = b"import os\n";
        let kind = detect_kind(data, Path::new("script.py"));
        assert_eq!(kind, FileKind::SourceCode, "python by extension");
    }

    #[test]
    fn test_detect_dockerfile() {
        let data = b"FROM ubuntu\nRUN apt-get update\n";
        let kind = detect_kind(data, Path::new("Dockerfile"));
        assert_eq!(kind, FileKind::SourceCode, "Dockerfile by name");
    }

    #[test]
    fn test_detect_gzip_by_magic() {
        let data = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03compressed";
        let kind = detect_kind(data, Path::new("file.gz"));
        assert_eq!(kind, FileKind::Gzip, "gzip by magic bytes");
    }

    #[test]
    fn test_detect_plain_text() {
        let data = b"Just some regular text with no special format";
        let kind = detect_kind(data, Path::new("readme.txt"));
        assert_eq!(kind, FileKind::PlainText, "plain text fallback");
    }

    #[test]
    fn test_detect_empty_data() {
        let data = b"";
        let kind = detect_kind(data, Path::new("empty.txt"));
        assert_eq!(kind, FileKind::PlainText, "empty data is plain text");
    }

    #[test]
    fn test_detect_unknown_extension() {
        let data = b"some random content here 12345";
        let kind = detect_kind(data, Path::new("output.bin"));
        assert_eq!(kind, FileKind::PlainText, "unknown extension + ascii = plain text");
    }

    // ── is_binary tests ──

    #[test]
    fn test_is_binary_ascii_text() {
        let data = b"Hello, this is plain text\nwith multiple lines.\n";
        assert!(!is_binary(data), "ascii text is not binary");
    }

    #[test]
    fn test_is_binary_with_nulls() {
        let data = b"\x00\x01\x02\x03Hello\x00world";
        assert!(is_binary(data), "content with nulls is binary");
    }

    #[test]
    fn test_is_binary_empty() {
        let data = b"";
        assert!(!is_binary(data), "empty is not binary");
    }

    #[test]
    fn test_is_binary_unicode_text() {
        // The UTF-8 multi-byte characters are now excluded from the
        // non-printable count (bytes >= 0x80 are skipped).
        let data = "Hello 世界\n".as_bytes();
        assert!(!is_binary(data), "utf-8 text is not binary");
    }

    #[test]
    fn test_is_binary_high_nonprintable() {
        let mut data = vec![0u8; 100];
        for i in 0..100 { data[i] = 0x01; }
        assert!(is_binary(&data), "high non-printable ratio = binary");
    }

    // ── is_elf tests ──

    #[test]
    fn test_is_elf_valid_header() {
        let data: &[u8] = b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        assert!(is_elf(data), "valid ELF header detected");
    }

    #[test]
    fn test_is_elf_too_short() {
        let data: &[u8] = b"\x7fEL";
        assert!(!is_elf(data), "too short for ELF");
    }

    #[test]
    fn test_is_elf_wrong_magic() {
        let data: &[u8] = b"not an ELF file at all!";
        assert!(!is_elf(data), "no ELF magic");
    }

    #[test]
    fn test_is_elf_empty() {
        let data: &[u8] = b"";
        assert!(!is_elf(data), "empty is not ELF");
    }

    #[test]
    fn test_is_elf_almost_elf() {
        let data: &[u8] = b"\x7fELF but it's actually just text";
        assert!(is_elf(data), "starts with ELF magic");
    }

    // ── looks_like_markdown tests ──

    #[test]
    fn test_looks_like_markdown_h1_and_list() {
        let data = b"# Title\n\n- item 1\n- item 2\n";
        assert!(looks_like_markdown(data));
    }

    #[test]
    fn test_looks_like_markdown_h2_and_code() {
        let data = b"## Section\n\n```rust\nfn main() {}\n```\n";
        assert!(looks_like_markdown(data));
    }

    #[test]
    fn test_looks_like_markdown_blockquote() {
        let data = b"# Quote\n\n> This is a quote\n";
        assert!(looks_like_markdown(data));
    }

    #[test]
    fn test_looks_like_markdown_no_heading() {
        let data = b"Just a paragraph\nwith no heading\n";
        assert!(!looks_like_markdown(data));
    }

    #[test]
    fn test_looks_like_markdown_heading_only() {
        let data = b"# Heading only\nno markdown constructs\n";
        assert!(!looks_like_markdown(data), "heading alone = not md");
    }

    #[test]
    fn test_looks_like_markdown_empty() {
        let data = b"";
        assert!(!looks_like_markdown(data));
    }

    #[test]
    fn test_looks_like_markdown_numbered_list() {
        let data = b"# Steps\n\n1. First\n2. Second\n";
        assert!(looks_like_markdown(data));
    }

    #[test]
    fn test_looks_like_markdown_table() {
        let data = b"# Data\n\n| Col1 | Col2 |\n|------|------|\n| A    | B    |\n";
        assert!(looks_like_markdown(data));
    }

    #[test]
    fn test_looks_like_markdown_horizontal_rule() {
        let data = b"# Sections\n\n---\n";
        assert!(looks_like_markdown(data));
    }

    // ── describe_kind tests ──

    #[test]
    fn test_describe_kind_markdown() {
        let data = b"# Hello\n- list\n";
        let desc = describe_kind(data, Path::new("test.md"));
        assert!(desc.contains("markdown"), "should mention markdown, got: {desc}");
    }

    #[test]
    fn test_describe_kind_json() {
        let data = b"{\"a\": 1}";
        let desc = describe_kind(data, Path::new("test.json"));
        assert!(desc.contains("JSON"), "should mention JSON, got: {desc}");
    }

    #[test]
    fn test_describe_kind_plain_text() {
        let data = b"plain text here";
        let desc = describe_kind(data, Path::new("test.txt"));
        assert!(desc.contains("ASCII text") || desc.contains("text/plain"), "got: {desc}");
    }

    #[test]
    fn test_describe_kind_rust_source() {
        let data = b"fn main() {}";
        let desc = describe_kind(data, Path::new("main.rs"));
        assert!(desc.contains("Rust source"), "got: {desc}");
    }

    // ── readable_file_kind tests ──

    #[test]
    fn test_readable_file_kind_png() {
        assert_eq!(readable_file_kind("image/png", Path::new("i.png")), "PNG image");
    }

    #[test]
    fn test_readable_file_kind_pdf() {
        assert_eq!(readable_file_kind("application/pdf", Path::new("doc.pdf")), "PDF document");
    }

    #[test]
    fn test_readable_file_kind_docx_vs_zip() {
        assert_eq!(readable_file_kind("application/zip", Path::new("report.docx")), "Word document");
        assert_eq!(readable_file_kind("application/zip", Path::new("archive.zip")), "ZIP archive");
    }

    #[test]
    fn test_readable_file_kind_mp3() {
        assert_eq!(readable_file_kind("audio/mpeg", Path::new("song.mp3")), "MP3 audio");
    }

    #[test]
    fn test_readable_file_kind_unknown() {
        assert_eq!(readable_file_kind("application/octet-stream", Path::new("file.bin")), "application/octet-stream");
    }

    // ── cat_inspect tests ──

    #[test]
    fn test_shannon_entropy_empty() {
        assert_eq!(cat_inspect::shannon_entropy(b""), 0.0);
    }

    #[test]
    fn test_shannon_entropy_constant() {
        // All same byte → log2(1) = 0
        let e = cat_inspect::shannon_entropy(&[0x41; 100]);
        assert!(e.abs() < 1e-10, "constant input should have ~0 entropy, got {e}");
    }

    #[test]
    fn test_shannon_entropy_maximum() {
        // All 256 bytes evenly → ~8.0 bits/byte
        let data: Vec<u8> = (0u8..=255).cycle().take(256 * 10).collect();
        let e = cat_inspect::shannon_entropy(&data);
        assert!((e - 8.0).abs() < 0.1, "uniform input should have ~8.0 entropy, got {e}");
    }

    #[test]
    fn test_shannon_entropy_typical_text() {
        // English text lower entropy
        let text = b"The quick brown fox jumps over the lazy dog. ";
        let e = cat_inspect::shannon_entropy(text);
        assert!(e > 3.0 && e < 5.5, "English text entropy should be 3-5.5, got {e}");
    }

    #[test]
    fn test_detect_encoding_ascii() {
        assert_eq!(cat_inspect::detect_encoding(b"hello"), "ASCII");
    }

    #[test]
    fn test_detect_encoding_utf8() {
        assert_eq!(cat_inspect::detect_encoding("héllo".as_bytes()), "UTF-8");
    }

    #[test]
    fn test_detect_encoding_utf8_bom() {
        let data = &[0xef, 0xbb, 0xbf, b'h', b'i'];
        assert_eq!(cat_inspect::detect_encoding(data), "UTF-8 with BOM");
    }

    #[test]
    fn test_detect_encoding_utf16le() {
        let data = &[0xff, 0xfe, b'h', 0x00, b'i', 0x00];
        assert_eq!(cat_inspect::detect_encoding(data), "UTF-16 LE");
    }

    #[test]
    fn test_detect_encoding_utf16be() {
        let data = &[0xfe, 0xff, 0x00, b'h', 0x00, b'i'];
        assert_eq!(cat_inspect::detect_encoding(data), "UTF-16 BE");
    }

    #[test]
    fn test_detect_encoding_binary() {
        // Bytes that don't form a BOM and are invalid UTF-8
        let data = &[0x80, 0x81, 0x82];
        assert_eq!(cat_inspect::detect_encoding(data), "Binary");
    }

    #[test]
    fn test_human_size_bytes() {
        assert_eq!(cat_inspect::human_size(0), "0 B");
        assert_eq!(cat_inspect::human_size(1), "1 B");
    }

    #[test]
    fn test_human_size_kib() {
        let s = cat_inspect::human_size(2048);
        assert!(s.contains("2.0") && s.contains("KiB"), "got: {s}");
    }

    #[test]
    fn test_human_size_mib() {
        let s = cat_inspect::human_size(3 * 1024 * 1024);
        assert!(s.contains("3.0") && s.contains("MiB"), "got: {s}");
    }

    #[test]
    fn test_compute_text_stats_empty() {
        let s = cat_inspect::compute_text_stats(b"");
        assert_eq!(s.lines, 0);
        assert_eq!(s.words, 0);
        assert_eq!(s.chars, 0);
    }

    #[test]
    fn test_compute_text_stats_simple() {
        let s = cat_inspect::compute_text_stats(b"hello world\nhow are you\n");
        assert_eq!(s.lines, 2);
        assert_eq!(s.words, 5);
        assert_eq!(s.chars, 24); // including newlines
    }

    #[test]
    fn test_compute_text_stats_blank_lines() {
        let s = cat_inspect::compute_text_stats(b"line1\n\n\nline4\n");
        assert_eq!(s.lines, 4);
        assert_eq!(s.blank_lines, 2);
    }

    #[test]
    fn test_format_structured_info_json() {
        let data = br#"{"name": "test", "items": [1, 2, 3]}"#;
        let path = Path::new("test.json");
        let result = cat_inspect::format_structured_info(data, path);
        assert!(result.is_some(), "should detect JSON");
        let (keys, depth, doc_type) = result.unwrap();
        assert_eq!(keys, 2, "should have 2 keys (name + items)");
        assert!(depth >= 2, "depth should be >= 2 (root + items array)");
        assert!(doc_type.contains("JSON"), "should say JSON");
    }

    #[test]
    fn test_format_structured_info_yaml() {
        let data = b"name: test\nversion: 1\n";
        let path = Path::new("test.yaml");
        let result = cat_inspect::format_structured_info(data, path);
        assert!(result.is_some(), "should detect YAML");
        let (_, _, doc_type) = result.unwrap();
        assert!(doc_type.contains("YAML"), "should say YAML");
    }

    #[test]
    fn test_format_structured_info_toml() {
        let data = b"[package]\nname = \"test\"\nversion = \"1.0\"\n";
        let path = Path::new("test.toml");
        let result = cat_inspect::format_structured_info(data, path);
        assert!(result.is_some(), "should detect TOML");
        let (_, _, doc_type) = result.unwrap();
        assert!(doc_type.contains("TOML"), "should say TOML");
    }

    #[test]
    fn test_format_structured_info_plain_text() {
        // Data that won't parse as JSON, YAML, or TOML
        // YAML can parse bare strings, so use something that confuses it
        let data = b"@@@ plain text with special! @@@ no key:value pairs";
        let path = Path::new("test.txt");
        let result = cat_inspect::format_structured_info(data, path);
        assert!(result.is_none(), "plain text should not be detected as structured");
    }
}
