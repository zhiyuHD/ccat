use std::fs;
use std::io::{self, Read};
use std::path::Path;

use clap::Parser;
use flate2::read::GzDecoder;

mod cat_markdown;
mod cat_docx;
mod cat_image;

/// ccat - An enhanced cat tool with automatic file type detection
#[derive(Parser)]
#[command(name = "ccat", version, about = "Enhanced cat: auto-detect and display markdown, docx, images, and gz files")]
struct Cli {
    /// File(s) to display
    files: Vec<String>,

    /// Force plain text output
    #[arg(short = 'A', long = "ascii")]
    ascii: bool,

    /// Display raw bytes (no processing)
    #[arg(short = 'B', long = "binary")]
    binary: bool,

    /// Show detected file type
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
    for line in &first_lines {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") || trimmed.starts_with("## ")
            || trimmed.starts_with("### ") || trimmed.starts_with("```")
            || trimmed.starts_with("---") || trimmed.starts_with("| ")
            || trimmed.starts_with("[") || trimmed.starts_with("> ")
        {
            return true;
        }
    }
    false
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

    if show_type {
        let kind = detect_kind(&data, path_obj);
        let type_str = match kind {
            FileKind::Markdown => "markdown",
            FileKind::Docx => "word (docx)",
            FileKind::Gzip => "gzip compressed",
            FileKind::Image => "image",
            FileKind::PlainText => "plain text",
        };
        eprintln!("ccat: {path}: detected as {type_str}");
    }

    if force_binary {
        cat_plain(&data);
        return Ok(());
    }

    let raw = detect_kind(&data, path_obj);

    if force_ascii {
        match raw {
            FileKind::Gzip => cat_gz(&data),
            _ => cat_plain(&data),
        }
        return Ok(());
    }

    match raw {
        FileKind::Markdown => cat_markdown::cat_markdown(&data),
        FileKind::Docx => cat_docx::cat_docx(&data),
        FileKind::Gzip => cat_gz(&data),
        FileKind::Image => cat_image::cat_image(&data),
        FileKind::PlainText => cat_plain(&data),
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
                let type_str = match kind {
                    FileKind::Markdown => "markdown",
                    FileKind::Docx => "word (docx)",
                    FileKind::Gzip => "gzip compressed",
                    FileKind::Image => "image",
                    FileKind::PlainText => "plain text",
                };
                eprintln!("ccat: stdin: detected as {type_str}");
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
                FileKind::PlainText => cat_plain(&buf),
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
