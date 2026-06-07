use std::fs;
use std::io;
use std::io::Read;
use std::path::Path;

/// Style constants for tree display.
const STYLE_DIR: &str = "\x1b[1;36m";     // bold cyan
const STYLE_SYM: &str = "\x1b[1;33m";     // bold yellow
const STYLE_CODE: &str = "\x1b[33m";      // yellow
const STYLE_IMG: &str = "\x1b[35m";       // magenta
const STYLE_DATA: &str = "\x1b[32m";      // green
const STYLE_BIN: &str = "\x1b[31m";       // red
const STYLE_ARCHIVE: &str = "\x1b[94m";   // bright blue
const STYLE_MEDIA: &str = "\x1b[95m";     // bright magenta
const STYLE_DIM: &str = "\x1b[2m";
const STYLE_RESET: &str = "\x1b[0m";

/// Simple file type categories for tree display.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FileCategory {
    Directory,
    Symlink,
    SourceCode,
    MarkdownDoc,
    Image,
    Archive,
    Media,
    Config,
    Data,
    Binary,
    Script,
    Plain,
}

/// Human-readable size.
fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{bytes}B")
    } else if size >= 100.0 {
        format!("{:.0}{}", size, UNITS[unit_idx])
    } else if size >= 10.0 {
        format!("{:.1}{}", size, UNITS[unit_idx])
    } else {
        format!("{:.2}{}", size, UNITS[unit_idx])
    }
}

/// Count text-ish lines in a file.
fn count_lines(path: &Path) -> Option<usize> {
    // Only count for files under 1MB
    let meta = fs::metadata(path).ok()?;
    if meta.len() > 1_048_576 {
        return None;
    }
    let data = fs::read(path).ok()?;
    if data.is_empty() {
        return Some(0);
    }
    // Quick binary check — if more than 1% null bytes, skip line counting
    let nulls = data.iter().take(8192).filter(|&&b| b == 0).count();
    if nulls > data.len().min(8192) / 100 {
        return None;
    }
    Some(data.iter().filter(|&&b| b == b'\n').count() + 1)
}

/// Classify a file by extension and first bytes.
fn classify(path: &Path) -> FileCategory {
    if path.is_symlink() {
        return FileCategory::Symlink;
    }
    if path.is_dir() {
        return FileCategory::Directory;
    }

    // Magic bytes check (read first 8 bytes)
    if let Ok(mut f) = fs::File::open(path) {
        let mut buf = [0u8; 8];
        if f.read_exact(&mut buf).is_ok() {
            // ELF
            if buf.starts_with(b"\x7fELF") {
                return FileCategory::Binary;
            }
            // PNG
            if buf.starts_with(b"\x89PNG") {
                return FileCategory::Image;
            }
            // JPEG
            if buf.starts_with(b"\xff\xd8") {
                return FileCategory::Image;
            }
            // GIF
            if buf.starts_with(b"GIF8") {
                return FileCategory::Image;
            }
            // WebP
            if &buf[..4] == b"RIFF" && &buf[..8] == b"RIFF WEBP" {
                return FileCategory::Image;
            }
            // PDF
            if buf.starts_with(b"%PDF") {
                return FileCategory::Data;
            }
            // Gzip
            if buf.starts_with(b"\x1f\x8b") {
                return FileCategory::Archive;
            }
            // Zip (includes docx, jar, etc.)
            if buf.starts_with(b"PK\x03\x04") {
                return FileCategory::Archive;
            }
            // BMP
            if buf.starts_with(b"BM") {
                return FileCategory::Image;
            }
            // TIFF
            if buf.starts_with(b"MM") || buf.starts_with(b"II") {
                return FileCategory::Image;
            }
        }
    }

    // Extension-based detection
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    match ext.as_deref() {
        // Source code
        Some(
            "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "go" | "java" | "kt"
            | "swift" | "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hh"
            | "cs" | "fs" | "clj" | "hs" | "ex" | "exs" | "erl" | "elm"
            | "php" | "rb" | "scala" | "dart" | "nim" | "cr" | "zig" | "odin"
            | "sass" | "scss" | "less" | "css" | "sql" | "r" | "m" | "mm",
        ) => FileCategory::SourceCode,
        // Scripts
        Some("sh" | "bash" | "zsh" | "fish" | "pl" | "lua" | "awk" | "sed"
            | "ps1" | "bat" | "tcl" | "ml" | "jl"
        ) => FileCategory::Script,
        // Markdown / docs
        Some("md" | "markdown" | "rst" | "asciidoc" | "adoc" | "txt" | "org") => {
            FileCategory::MarkdownDoc
        }
        // Config / data
        Some(
            "json" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf"
            | "xml" | "html" | "htm" | "svg" | "env" | "dockerfile"
            | "cmake" | "makefile" | "gnumakefile" | "gradle" | "lock",
        ) => FileCategory::Config,
        // Archives
        Some("tar" | "tgz" | "tbz2" | "xz" | "bz2" | "gz" | "zst" | "7z" | "rar") => {
            FileCategory::Archive
        }
        // Images
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif"
            | "ico" | "avif" | "heic" | "heif" | "svgz"
        ) => FileCategory::Image,
        // Media
        Some("mp3" | "flac" | "ogg" | "wav" | "aac" | "m4a" | "opus" | "wma") => {
            FileCategory::Media
        }
        Some("mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv") => {
            FileCategory::Media
        }
        // Binary
        Some("bin" | "exe" | "dll" | "so" | "dylib" | "wasm" | "o" | "obj") => {
            FileCategory::Binary
        }
        _ => FileCategory::Plain,
    }
}

/// Style string for a file category.
fn style_for(cat: FileCategory) -> &'static str {
    match cat {
        FileCategory::Directory => STYLE_DIR,
        FileCategory::Symlink => STYLE_SYM,
        FileCategory::SourceCode | FileCategory::Script => STYLE_CODE,
        FileCategory::Image => STYLE_IMG,
        FileCategory::Config | FileCategory::MarkdownDoc => STYLE_DATA,
        FileCategory::Archive => STYLE_ARCHIVE,
        FileCategory::Media => STYLE_MEDIA,
        FileCategory::Binary => STYLE_BIN,
        FileCategory::Data | FileCategory::Plain => "",
    }
}

/// Category label string.
fn label_for(cat: FileCategory) -> &'static str {
    match cat {
        FileCategory::Directory => "dir",
        FileCategory::Symlink => "link",
        FileCategory::SourceCode => "code",
        FileCategory::Script => "script",
        FileCategory::Image => "img",
        FileCategory::Config => "cfg",
        FileCategory::MarkdownDoc => "doc",
        FileCategory::Archive => "arc",
        FileCategory::Media => "media",
        FileCategory::Binary => "bin",
        FileCategory::Data => "data",
        FileCategory::Plain => "text",
    }
}

/// Recursively render directory tree.
fn render_tree(
    path: &Path,
    prefix: &str,
    max_depth: Option<usize>,
    current_depth: usize,
    show_hidden: bool,
    is_last: bool,
) -> io::Result<()> {
    // Check depth limit
    if let Some(max) = max_depth {
        if current_depth > max {
            return Ok(());
        }
    }

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string();

    let cat = classify(path);
    let style = style_for(cat);

    // Build connector
    let connector = if is_last { "└── " } else { "├── " };
    let child_prefix = if is_last { "    " } else { "│   " };

    // Size and line count for files
    let meta = fs::symlink_metadata(path).ok();
    let extra = if cat == FileCategory::Symlink {
        // Show symlink target
        if let Ok(target) = fs::read_link(path) {
            format!("{STYLE_DIM} → {}{STYLE_RESET}", target.display())
        } else {
            String::new()
        }
    } else if cat == FileCategory::Directory {
        // Show item count for directories
        let count = fs::read_dir(path)
            .ok()
            .map(|entries| entries.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        if count > 0 {
            format!("{STYLE_DIM} ({count} items){STYLE_RESET}")
        } else {
            format!("{STYLE_DIM} (empty){STYLE_RESET}")
        }
    } else if let Some(meta) = meta {
        let size = human_size(meta.len());
        let lines_str = if cat == FileCategory::SourceCode
            || cat == FileCategory::Script
            || cat == FileCategory::Config
            || cat == FileCategory::MarkdownDoc
            || cat == FileCategory::Plain
        {
            if let Some(lines) = count_lines(path) {
                format!(" · {lines} lines")
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let label = label_for(cat);
        let label_str = if cat == FileCategory::Plain || cat == FileCategory::Data {
            String::new()
        } else {
            format!(" · {label}")
        };
        format!("{STYLE_DIM} ({size}{label_str}{lines_str}){STYLE_RESET}")
    } else {
        String::new()
    };

    // Build the line
    let line = format!(
        "{prefix}{connector}{style}{}{STYLE_RESET}{extra}",
        file_name
    );
    println!("{line}");

    // Recurse into directories
    if cat == FileCategory::Directory {
        let mut entries: Vec<_> = match fs::read_dir(path) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| {
                    if show_hidden {
                        true
                    } else {
                        !e.file_name()
                            .to_str()
                            .map(|s| s.starts_with('.'))
                            .unwrap_or(false)
                    }
                })
                .collect(),
            Err(_) => return Ok(()),
        };

        // Sort: directories first, then by name
        entries.sort_by(|a, b| {
            let a_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let b_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if a_dir != b_dir {
                b_dir.cmp(&a_dir)
            } else {
                a.file_name().cmp(&b.file_name())
            }
        });

        let total = entries.len();
        for (i, entry) in entries.into_iter().enumerate() {
            render_tree(
                &entry.path(),
                &format!("{prefix}{child_prefix}"),
                max_depth,
                current_depth + 1,
                show_hidden,
                i == total - 1,
            )?;
        }
    }

    Ok(())
}

/// Print a directory tree.
pub fn print_tree(
    path_str: &str,
    max_depth: Option<usize>,
    show_hidden: bool,
) -> io::Result<()> {
    let path = Path::new(path_str);
    if !path.exists() {
        eprintln!("ccat: {path_str}: No such file or directory");
        return Ok(());
    }

    // If it's a file, just print its name with style
    if path.is_file() {
        let cat = classify(path);
        let style = style_for(cat);
        let meta = fs::metadata(path).ok();
        let size = meta.map(|m| human_size(m.len())).unwrap_or_default();
        let label = label_for(cat);
        println!("{style}{}{STYLE_RESET}  {STYLE_DIM}({size} · {label}){STYLE_RESET}", path_str);
        return Ok(());
    }

    // Print root directory name
    let display_name = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path_str);
    let item_count = fs::read_dir(path)
        .ok()
        .map(|entries| entries.filter_map(|e| e.ok()).count())
        .unwrap_or(0);
    println!(
        "\x1b[1;36m{}{STYLE_RESET}  {STYLE_DIM}({item_count} items){STYLE_RESET}",
        display_name
    );

    // Gather entries
    let mut entries: Vec<_> = match fs::read_dir(path) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| {
                if show_hidden {
                    true
                } else {
                    !e.file_name()
                        .to_str()
                        .map(|s| s.starts_with('.'))
                        .unwrap_or(false)
                }
            })
            .collect(),
        Err(e) => {
            eprintln!("ccat: {path_str}: {e}");
            return Ok(());
        }
    };

    // Sort: directories first, then by name
    entries.sort_by(|a, b| {
        let a_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let b_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if a_dir != b_dir {
            b_dir.cmp(&a_dir)
        } else {
            a.file_name().cmp(&b.file_name())
        }
    });

    let total = entries.len();
    for (i, entry) in entries.into_iter().enumerate() {
        render_tree(
            &entry.path(),
            "",
            max_depth,
            1,
            show_hidden,
            i == total - 1,
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size_bytes() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(1), "1B");
        assert_eq!(human_size(999), "999B");
    }

    #[test]
    fn test_human_size_kb() {
        let s = human_size(1024);
        assert!(s.ends_with('K'), "expected KB suffix, got {s}");
    }

    #[test]
    fn test_human_size_mb() {
        let s = human_size(1_048_576);
        assert!(s.ends_with('M'), "expected MB suffix, got {s}");
    }

    #[test]
    fn test_human_size_gb() {
        let s = human_size(1_073_741_824);
        assert!(s.ends_with('G'), "expected GB suffix, got {s}");
    }

    #[test]
    fn test_classify_source() {
        assert_eq!(classify(Path::new("main.rs")), FileCategory::SourceCode);
        assert_eq!(classify(Path::new("app.py")), FileCategory::SourceCode);
        assert_eq!(classify(Path::new("style.css")), FileCategory::SourceCode);
    }

    #[test]
    fn test_classify_image() {
        assert_eq!(classify(Path::new("photo.png")), FileCategory::Image);
        assert_eq!(classify(Path::new("photo.jpg")), FileCategory::Image);
        assert_eq!(classify(Path::new("photo.gif")), FileCategory::Image);
    }

    #[test]
    fn test_classify_config() {
        assert_eq!(classify(Path::new("config.json")), FileCategory::Config);
        assert_eq!(classify(Path::new("config.yaml")), FileCategory::Config);
        assert_eq!(classify(Path::new("Cargo.toml")), FileCategory::Config);
    }

    #[test]
    fn test_classify_plain() {
        assert_eq!(classify(Path::new("README")), FileCategory::Plain);
        assert_eq!(classify(Path::new("data.bak")), FileCategory::Plain);
    }

    #[test]
    fn test_classify_archive() {
        assert_eq!(classify(Path::new("data.tar.gz")), FileCategory::Archive);
        assert_eq!(classify(Path::new("data.tar")), FileCategory::Archive);
    }

    #[test]
    fn test_classify_archive_by_ext() {
        assert_eq!(classify(Path::new("data.7z")), FileCategory::Archive);
        assert_eq!(classify(Path::new("data.rar")), FileCategory::Archive);
    }

    #[test]
    fn test_count_lines_empty() {
        assert_eq!(count_lines(Path::new("nonexistent.rs")), None);
    }

    #[test]
    fn test_label_for_all_categories() {
        for cat in &[
            FileCategory::Directory,
            FileCategory::Symlink,
            FileCategory::SourceCode,
            FileCategory::Script,
            FileCategory::Image,
            FileCategory::Config,
            FileCategory::MarkdownDoc,
            FileCategory::Archive,
            FileCategory::Media,
            FileCategory::Binary,
            FileCategory::Data,
            FileCategory::Plain,
        ] {
            let label = label_for(*cat);
            assert!(!label.is_empty(), "label for {cat:?} should not be empty");
        }
    }
}
