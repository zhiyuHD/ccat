/// `ccat --bundle` — Bundle source/text files for LLM context.
///
/// Takes a list of files or directories and outputs them in a structured format
/// (markdown code fences, plain `=====` headers, or compact single-line) with
/// truncation controls. Skips binary files, images, archives, media automatically.
///
/// Useful for feeding code context into an LLM prompt.
///
/// Examples:
/// ```sh
/// ccat --bundle src/*.rs                    # bundle all Rust sources
/// ccat --bundle src/                        # recursively bundle all text files
/// ccat --bundle src/main.rs Cargo.toml      # specific files
/// ccat --bundle src/ --bundle-max-lines 50  # truncate large files
/// ccat --bundle src/ --bundle-format plain  # plain === headers instead of markdown
/// ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{detect_kind, FileKind};

/// Output format for bundled files.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BundleFormat {
    /// Markdown code fences with language hint.
    /// ```rust
    /// ...
    /// ```
    Markdown,
    /// Plain `===== filename =====` headers (no markdown).
    Plain,
    /// Compact: `# file:path` single-line header, raw content.
    Compact,
}

impl BundleFormat {
    pub fn from_str(s: &str) -> Self {
        match s {
            "plain" => BundleFormat::Plain,
            "compact" => BundleFormat::Compact,
            _ => BundleFormat::Markdown,
        }
    }
}

/// Options for the bundle operation.
pub struct BundleOptions<'a> {
    /// File and directory paths to bundle.
    pub paths: &'a [String],
    /// Output format: "markdown", "plain", "compact".
    pub format: BundleFormat,
    /// Max lines per file (truncates if exceeded). 0 = unlimited.
    pub max_lines: usize,
    /// Max total lines across all files. 0 = unlimited.
    pub max_total: usize,
    /// Glob pattern to exclude (simple prefix/suffix matching).
    pub exclude: Option<&'a str>,
    /// Whether to prepend a directory tree.
    pub show_tree: bool,
    /// Show file sizes and line counts.
    pub show_stats: bool,
}

/// Run the bundle command.
pub fn cat_bundle(opts: &BundleOptions) -> io::Result<()> {
    let mut files = Vec::new();

    // Expand paths: directories are recursively scanned for text files.
    for p in opts.paths {
        let path = Path::new(p);
        if path.is_dir() {
            collect_text_files(path, &mut files, 0)?;
        } else if path.is_file() {
            files.push(path.to_path_buf());
        }
    }

    if files.is_empty() {
        eprintln!("ccat --bundle: no text files found");
        return Ok(());
    }

    // Sort for reproducible output
    files.sort();

    // Apply exclude filter
    let files: Vec<&PathBuf> = if let Some(excl) = opts.exclude {
        files.iter().filter(|p| !path_matches_exclude(p, excl)).collect()
    } else {
        files.iter().collect()
    };

    if files.is_empty() {
        eprintln!("ccat --bundle: all files excluded");
        return Ok(());
    }

    // Show directory tree if requested
    if opts.show_tree {
        let root = find_common_root(&files);
        print_tree(&root, &files, 0);
        println!();
    }

    // Track total lines emitted
    let mut total_lines: usize = 0;
    let max_total = opts.max_total;

    for (i, file) in files.iter().enumerate() {
        if max_total > 0 && total_lines >= max_total {
            let remaining = files.len() - i;
            eprintln!("\nccat: reached max total lines ({max_total}), skipped {remaining} file(s)");
            break;
        }

        let data = match fs::read(file) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("ccat --bundle: {}: {e}", file.display());
                continue;
            }
        };

        if data.is_empty() {
            continue;
        }

        // Skip binary-looking files
        if data.iter().take(8192).any(|&b| b == 0) {
            continue;
        }

        let kind = detect_kind(&data, file);

        // Skip non-text file kinds
        if !is_bundleable_kind(&kind) {
            continue;
        }

        let content = String::from_utf8_lossy(&data);
        let lines: Vec<&str> = content.lines().collect();
        let total_file_lines = lines.len();

        // Truncate if max_lines is set
        let truncated = opts.max_lines > 0 && lines.len() > opts.max_lines;
        let display_lines: Vec<&str> = if truncated {
            lines[..opts.max_lines].to_vec()
        } else {
            lines.clone()
        };

        // Calculate how many lines we can actually emit given max_total budget
        let budget = if max_total > 0 {
            max_total.saturating_sub(total_lines)
        } else {
            display_lines.len()
        };
        if budget == 0 {
            break;
        }
        let emit_lines: Vec<&str> = if display_lines.len() > budget && max_total > 0 {
            display_lines[..budget].to_vec()
        } else {
            display_lines
        };

        if emit_lines.is_empty() && !truncated {
            continue;
        }

        // Print file header + content — use file extension for markdown language hints
        match opts.format {
            BundleFormat::Markdown => {
                if i > 0 {
                    println!();
                }
                let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
                println!("## {path}", path = file.display());
                if opts.show_stats {
                    println!(
                        "> {} — {} lines{}",
                        file_size_str(&data),
                        total_file_lines,
                        if truncated {
                            format!(" (showing first {})", opts.max_lines)
                        } else {
                            String::new()
                        }
                    );
                }
                println!();

                // Use the file extension as the language hint
                if !ext.is_empty() {
                    println!("```{ext}");
                } else {
                    println!("```");
                }
                for line in &emit_lines {
                    println!("{line}");
                }
                if truncated {
                    println!("// ... truncated {}/{} lines", total_file_lines - opts.max_lines, total_file_lines);
                }
                println!("```");
            }
            BundleFormat::Plain => {
                if i > 0 {
                    println!();
                }
                let bar = "=".repeat(72.min(file.to_string_lossy().len() + 10));
                println!("{bar}");
                println!("  FILE: {path}", path = file.display());
                if opts.show_stats {
                    println!(
                        "  SIZE: {size}  LINES: {lines}{trunc}",
                        size = file_size_str(&data),
                        lines = total_file_lines,
                        trunc = if truncated {
                            format!(" (showing {})", opts.max_lines)
                        } else {
                            String::new()
                        }
                    );
                }
                println!("{bar}");
                for line in &emit_lines {
                    println!("{line}");
                }
                if truncated {
                    println!("-- truncated {}/{} lines", total_file_lines - opts.max_lines, total_file_lines);
                }
            }
            BundleFormat::Compact => {
                println!("# file:{path}", path = file.display());
                if opts.show_stats {
                    println!("# size:{size} lines:{lines}{trunc}",
                        size = file_size_str(&data),
                        lines = total_file_lines,
                        trunc = if truncated {
                            format!(" truncated-to:{}", opts.max_lines)
                        } else {
                            String::new()
                        }
                    );
                }
                for line in &emit_lines {
                    println!("{line}");
                }
                if truncated {
                    println!("# truncated {}/{} lines", total_file_lines - opts.max_lines, total_file_lines);
                }
                println!(); // blank line between files
            }
        }

        total_lines += emit_lines.len();
    }

    // Print summary
    if files.len() > 1 {
        let skipped = opts.paths.iter()
            .filter(|p| Path::new(p).is_file())
            .count()
            .saturating_sub(files.len());
        let note = if opts.show_stats {
            format!(" — {skipped} non-text file(s) skipped")
        } else {
            String::new()
        };
        eprintln!("\n\x1b[2mccat --bundle: {n} file(s) bundled, {lines} lines total{note}\x1b[0m", n = files.len(), lines = total_lines);
    }

    Ok(())
}

/// Recursively collect text files from a directory.
fn collect_text_files(dir: &Path, files: &mut Vec<PathBuf>, depth: usize) -> io::Result<()> {
    if depth > 64 {
        return Ok(()); // safety limit
    }
    if !dir.is_dir() {
        return Ok(());
    }

    // Skip common non-source directories
    let dirname = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let skip_dirs = [
        ".git", "node_modules", "target", "vendor", ".venv", "venv",
        "__pycache__", ".cache", ".npm", ".cargo", ".rustup",
        ".gem", "bundle", ".bundle", "deps", "_build",
        "dist", "build", "out", ".next", ".nuxt",
    ];
    if skip_dirs.contains(&dirname) {
        return Ok(());
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_text_files(&path, files, depth + 1)?;
        } else if path.is_file() {
            // Quick size check — skip obviously huge files (>10MB)
            if let Ok(meta) = path.metadata() {
                if meta.len() > 10_000_000 {
                    continue;
                }
            }
            files.push(path);
        }
    }
    Ok(())
}

/// Determine if a FileKind is text-based and suitable for bundling.
fn is_bundleable_kind(kind: &FileKind) -> bool {
    matches!(kind,
        FileKind::SourceCode | FileKind::PlainText | FileKind::Log
        | FileKind::Json | FileKind::Yaml | FileKind::Toml
        | FileKind::Csv | FileKind::Markdown | FileKind::UnifiedDiff
    )
}

/// Check if a path matches the exclude pattern.
fn path_matches_exclude(path: &Path, pattern: &str) -> bool {
    let s = path.to_string_lossy();
    if let Some(stripped) = pattern.strip_prefix('*') {
        // Suffix match: *.log → ends with .log
        s.ends_with(stripped)
    } else if let Some(stripped) = pattern.strip_suffix('*') {
        // Prefix match: test_* → starts with test_
        s.starts_with(stripped)
    } else {
        // Exact match or path contains pattern
        s.contains(pattern)
    }
}

/// Format file size in human-readable form.
fn file_size_str(data: &[u8]) -> String {
    let len = data.len();
    if len < 1024 {
        format!("{len} B")
    } else if len < 1024 * 1024 {
        format!("{:.1} KiB", len as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", len as f64 / (1024.0 * 1024.0))
    }
}

/// Find the common ancestor directory of a set of files for tree display.
fn find_common_root(files: &[&PathBuf]) -> PathBuf {
    if files.is_empty() {
        return PathBuf::from(".");
    }
    if files.len() == 1 {
        if let Some(parent) = files[0].parent() {
            return parent.to_path_buf();
        }
        return PathBuf::from(".");
    }

    let first = files[0].components().collect::<Vec<_>>();
    let mut common_len = first.len();

    for file in &files[1..] {
        let comps = file.components().collect::<Vec<_>>();
        let mut i = 0;
        while i < common_len && i < comps.len() && first[i] == comps[i] {
            i += 1;
        }
        common_len = i;
    }

    if common_len == 0 {
        return PathBuf::from(".");
    }
    first[..common_len].iter().collect()
}

/// Print a simple ASCII tree of the bundled files.
fn print_tree(root: &Path, files: &[&PathBuf], _indent: usize) {
    println!("📦 Bundle tree (root: {})", root.display());

    let mut sorted = files.to_vec();
    sorted.sort();

    for file in sorted {
        let relative = if let Ok(rel) = file.strip_prefix(root) {
            rel.display().to_string()
        } else {
            file.display().to_string()
        };
        println!("  📄 {relative}");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_bundleable_kind_source() {
        assert!(is_bundleable_kind(&FileKind::SourceCode));
    }

    #[test]
    fn test_is_bundleable_kind_binary() {
        assert!(!is_bundleable_kind(&FileKind::Image));
        assert!(!is_bundleable_kind(&FileKind::Gzip));
        assert!(!is_bundleable_kind(&FileKind::Docx));
        assert!(!is_bundleable_kind(&FileKind::Pdf));
        assert!(!is_bundleable_kind(&FileKind::Media));
        assert!(!is_bundleable_kind(&FileKind::Archive));
    }

    #[test]
    fn test_is_bundleable_kind_text() {
        assert!(is_bundleable_kind(&FileKind::Json));
        assert!(is_bundleable_kind(&FileKind::Yaml));
        assert!(is_bundleable_kind(&FileKind::Toml));
        assert!(is_bundleable_kind(&FileKind::PlainText));
        assert!(is_bundleable_kind(&FileKind::Log));
        assert!(is_bundleable_kind(&FileKind::Markdown));
        assert!(is_bundleable_kind(&FileKind::Csv));
        assert!(is_bundleable_kind(&FileKind::UnifiedDiff));
    }

    #[test]
    fn test_format_from_str_default() {
        assert_eq!(BundleFormat::from_str("markdown"), BundleFormat::Markdown);
        assert_eq!(BundleFormat::from_str("plain"), BundleFormat::Plain);
        assert_eq!(BundleFormat::from_str("compact"), BundleFormat::Compact);
        assert_eq!(BundleFormat::from_str("unknown"), BundleFormat::Markdown);
        assert_eq!(BundleFormat::from_str(""), BundleFormat::Markdown);
    }

    #[test]
    fn test_file_size_str_bytes() {
        assert_eq!(file_size_str(b""), "0 B");
        assert_eq!(file_size_str(&[0u8; 512]), "512 B");
    }

    #[test]
    fn test_file_size_str_kib() {
        let result = file_size_str(&[0u8; 2048]);
        assert!(result.contains("2.0"));
        assert!(result.contains("KiB"));
    }

    #[test]
    fn test_file_size_str_mib() {
        let result = file_size_str(&[0u8; 3 * 1024 * 1024]);
        assert!(result.contains("3.0"));
        assert!(result.contains("MiB"));
    }

    #[test]
    fn test_path_matches_exclude_suffix() {
        assert!(path_matches_exclude(Path::new("test.log"), "*.log"));
        assert!(!path_matches_exclude(Path::new("test.txt"), "*.log"));
    }

    #[test]
    fn test_path_matches_exclude_prefix() {
        assert!(path_matches_exclude(Path::new("target/debug/ccat"), "target*"));
        assert!(!path_matches_exclude(Path::new("src/main.rs"), "target*"));
    }

    #[test]
    fn test_path_matches_exclude_contains() {
        assert!(path_matches_exclude(Path::new("src/test_data.rs"), "test"));
        assert!(!path_matches_exclude(Path::new("src/main.rs"), "test"));
    }

    #[test]
    fn test_find_common_root_same_dir() {
        let files = vec![
            PathBuf::from("/home/user/project/src/main.rs"),
            PathBuf::from("/home/user/project/src/lib.rs"),
        ];
        let refs: Vec<&PathBuf> = files.iter().collect();
        let root = find_common_root(&refs);
        assert!(root.ends_with("project/src"));
    }

    #[test]
    fn test_find_common_root_single_file() {
        let files = vec![PathBuf::from("/home/user/project/src/main.rs")];
        let refs: Vec<&PathBuf> = files.iter().collect();
        let root = find_common_root(&refs);
        assert!(root.ends_with("src"));
    }

    #[test]
    fn test_find_common_root_empty() {
        let files: Vec<&PathBuf> = vec![];
        let root = find_common_root(&files);
        assert_eq!(root, PathBuf::from("."));
    }

    #[test]
    fn test_collect_text_files_skip_dot_git() {
        // Test that .git is skipped by collect_text_files
        let git_path = Path::new(".git");
        assert!(!git_path.exists() || {
            let mut files = Vec::new();
            collect_text_files(git_path, &mut files, 0).is_ok() && files.is_empty()
        });
    }
}
