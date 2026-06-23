//! Codebase annotation scanner (`ccat --todo`).
//!
//! Recursively scans files for common development annotations:
//!   FIXME, BUG       — bugs or issues (highest priority, red)
//!   HACK, HAX        — questionable/unsafe code (magenta)
//!   TODO              — planned work (yellow)
//!   OPTIMIZE, PERF    — performance concerns (blue)
//!   XXX, REVIEW, CHECK, FIX — review needed (cyan)
//!   NOTE, INFO        — informational (green)
//!   TEMP, WORKAROUND  — temporary code (dim/grey)
//!
//! Displays a tree grouped by directory, with file:line:col references,
//! annotation context lines, and an optional git-blame overlay showing
//! author and date.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Colour helpers ──

mod style {
    pub fn bold(s: &str) -> String  { format!("\x1b[1m{s}\x1b[0m") }
    pub fn red(s: &str) -> String   { format!("\x1b[31m{s}\x1b[0m") }
    pub fn green(s: &str) -> String { format!("\x1b[32m{s}\x1b[0m") }
    pub fn yellow(s: &str) -> String { format!("\x1b[33m{s}\x1b[0m") }
    pub fn blue(s: &str) -> String  { format!("\x1b[34m{s}\x1b[0m") }
    pub fn magenta(s: &str) -> String { format!("\x1b[35m{s}\x1b[0m") }
    pub fn cyan(s: &str) -> String  { format!("\x1b[36m{s}\x1b[0m") }
    pub fn dim(s: &str) -> String   { format!("\x1b[2m{s}\x1b[0m") }
    pub fn white(s: &str) -> String { format!("\x1b[37m{s}\x1b[0m") }
}

// ── Annotation types ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnotationKind {
    Fixme,       // FIXME: bug or issue
    Bug,         // BUG: known bug
    Hack,        // HACK: questionable/unsafe code
    Todo,        // TODO: planned work
    Optimize,    // OPTIMIZE/PERF: performance concern
    Review,      // XXX/REVIEW/CHECK/FIX: needs review
    Note,        // NOTE/INFO: informational
    Temp,        // TEMP/WORKAROUND: temporary/transitional
    Unknown,     // custom pattern
}

impl AnnotationKind {
    fn color(&self, text: &str) -> String {
        match self {
            AnnotationKind::Fixme | AnnotationKind::Bug => style::red(text),
            AnnotationKind::Hack => style::magenta(text),
            AnnotationKind::Todo => style::yellow(text),
            AnnotationKind::Optimize => style::blue(text),
            AnnotationKind::Review => style::cyan(text),
            AnnotationKind::Note => style::green(text),
            AnnotationKind::Temp => style::dim(text),
            AnnotationKind::Unknown => style::white(text),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            AnnotationKind::Fixme => "FIXME",
            AnnotationKind::Bug => "BUG",
            AnnotationKind::Hack => "HACK",
            AnnotationKind::Todo => "TODO",
            AnnotationKind::Optimize => "OPTIMIZE",
            AnnotationKind::Review => "REVIEW",
            AnnotationKind::Note => "NOTE",
            AnnotationKind::Temp => "TEMP",
            AnnotationKind::Unknown => "MARK",
        }
    }

    fn priority(&self) -> u8 {
        match self {
            AnnotationKind::Fixme => 0,
            AnnotationKind::Bug => 1,
            AnnotationKind::Hack => 2,
            AnnotationKind::Review => 3,
            AnnotationKind::Todo => 4,
            AnnotationKind::Optimize => 5,
            AnnotationKind::Note => 6,
            AnnotationKind::Temp => 7,
            AnnotationKind::Unknown => 8,
        }
    }
}

// ── Annotation data ──

#[derive(Debug, Clone)]
pub struct Annotation {
    pub kind: AnnotationKind,
    pub file: PathBuf,
    pub line: usize,
    pub col: usize,
    pub text: String,       // the rest of the line after the annotation marker
    pub full_line: String,  // the entire line
    pub context: Vec<String>, // surrounding lines
}

#[derive(Debug, Clone)]
pub struct TodoOptions {
    /// Root paths to scan (default: ["."])
    pub paths: Vec<PathBuf>,
    /// Include hidden files/dirs (default: false)
    pub include_hidden: bool,
    /// Custom annotation patterns to search for (added to defaults)
    pub custom_patterns: Vec<String>,
    /// Show git blame info (author + date) for each annotation
    pub show_blame: bool,
    /// Show summary statistics only
    pub show_stats_only: bool,
    /// Max depth for recursive scan (default: no limit)
    pub max_depth: Option<usize>,
    /// Only show annotations matching these kinds (empty = all)
    pub filter_kinds: Vec<String>,
}

impl Default for TodoOptions {
    fn default() -> Self {
        Self {
            paths: vec![PathBuf::from(".")],
            include_hidden: false,
            custom_patterns: vec![],
            show_blame: false,
            show_stats_only: false,
            max_depth: None,
            filter_kinds: vec![],
        }
    }
}

// ── Default pattern definitions ──

/// Returns the default annotation patterns and their kinds.
fn default_patterns() -> Vec<(&'static str, AnnotationKind)> {
    vec![
        ("FIXME", AnnotationKind::Fixme),
        ("BUG", AnnotationKind::Bug),
        ("HACK", AnnotationKind::Hack),
        ("HAX", AnnotationKind::Hack),
        ("TODO", AnnotationKind::Todo),
        ("OPTIMIZE", AnnotationKind::Optimize),
        ("PERF", AnnotationKind::Optimize),
        ("OPT", AnnotationKind::Optimize),
        ("XXX", AnnotationKind::Review),
        ("REVIEW", AnnotationKind::Review),
        ("CHECK", AnnotationKind::Review),
        ("FIX", AnnotationKind::Review),
        ("NOTE", AnnotationKind::Note),
        ("INFO", AnnotationKind::Note),
        ("TEMP", AnnotationKind::Temp),
        ("WORKAROUND", AnnotationKind::Temp),
        ("WARN", AnnotationKind::Review),
        ("TODO", AnnotationKind::Todo),
    ]
}

/// Build a combined pattern list from defaults + custom.
fn build_patterns(custom: &[String]) -> Vec<(&str, AnnotationKind)> {
    let mut patterns = default_patterns();
    for cp in custom {
        // Treat custom patterns as Review (unknown) kind
        patterns.push((cp.as_str(), AnnotationKind::Unknown));
    }
    // Sort by length descending so longer patterns match before substrings
    // (e.g., WORKAROUND before BUG, OPTIMIZE before OPT)
    patterns.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    patterns
}

// ── File scanning ──

/// Directories to always skip.
const SKIP_DIRS: &[&str] = &[
    ".git", ".svn", ".hg", ".bzr",
    "node_modules", "target", "build", "dist", ".next",
    "vendor", ".cache", "__pycache__", ".mypy_cache",
    ".pytest_cache", ".eggs", "eggs", ".tox",
    ".bundle", "bin", "obj",
    "third_party", "third-party",
    "coverage", ".coverage",
    ".hermes", ".obsidian",
];

/// File extensions to skip.
const SKIP_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "ico", "svg", "webp", "bmp", "tiff",
    "mp3", "mp4", "avi", "mov", "mkv", "flac", "ogg", "wav", "opus",
    "woff", "woff2", "ttf", "eot",
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
    "zip", "tar", "gz", "bz2", "xz", "zst", "7z", "rar",
    "o", "so", "dylib", "dll", "exe", "wasm",
    "pyc", "pyo",
    "class", "jar",
    "ttf", "otf",
];

/// Is this a binary/asset file we should skip?
fn is_skippable(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext_lower = ext.to_lowercase();
        if SKIP_EXTENSIONS.contains(&ext_lower.as_str()) {
            return true;
        }
    }
    // Check base name for common binary files
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let n = name.to_lowercase();
        if n == "package-lock.json" || n == "yarn.lock" || n == "pnpm-lock.yaml"
            || n == "go.sum" || n == "cargo.lock" || n == "gemfile.lock"
        {
            return true;
        }
    }
    false
}

/// Check if string content looks binary (null bytes or excessive control characters).
fn is_binary_content(content: &str) -> bool {
    let control_count = content.bytes().filter(|&b| b == 0 || b < 0x09 || (b > 0x0d && b < 0x20)).count();
    let total = content.len().max(1);
    // If > 5% of bytes are control chars (excluding common whitespace \t\n\r), it's binary
    control_count * 100 / total > 5
}

/// Scan a file for annotations.
fn scan_file(path: &Path, patterns: &[(&str, AnnotationKind)]) -> Vec<Annotation> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    // Skip if file is suspiciously large (> 1MB of text)
    if content.len() > 1_048_576 {
        return vec![];
    }

    // Skip if file looks binary (null bytes or excessive control characters)
    if is_binary_content(&content) {
        return vec![];
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut results = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let line_upper = line.to_uppercase();
        for &(pattern, kind) in patterns {
            if let Some(col) = line_upper.find(pattern) {
                // Check it's a word boundary — the annotation should be
                // at start of line, preceded by whitespace, or preceded by non-alphanumeric
                let is_word_start = col == 0
                    || !line[..col].chars().last().map_or(false, |c| c.is_alphanumeric());
                if !is_word_start {
                    continue;
                }

                // Also verify the pattern isn't inside a longer word
                let after = col + pattern.len();
                if after < line.len() {
                    let next_char = line[after..].chars().next().unwrap();
                    if next_char.is_alphanumeric() {
                        continue; // e.g., "FIXME" matches but "FIXMELATER" doesn't
                    }
                }

                let text_after = if after < line.len() {
                    line[after..].trim().to_string()
                } else {
                    String::new()
                };

                // Get context lines (up to 2 before, 1 after)
                let mut context = Vec::new();
                let ctx_start = if i >= 2 { i - 2 } else { 0 };
                let ctx_end = (i + 2).min(lines.len());
                for j in ctx_start..ctx_end {
                    if j != i {
                        context.push(lines[j].to_string());
                    }
                }

                results.push(Annotation {
                    kind,
                    file: path.to_path_buf(),
                    line: i + 1,
                    col: col + 1,
                    text: text_after,
                    full_line: line.to_string(),
                    context,
                });
                break; // only first match per line
            }
        }
    }

    results
}

/// Recursively scan a directory tree for annotations.
fn scan_directory(
    dir: &Path,
    patterns: &[(&str, AnnotationKind)],
    opts: &TodoOptions,
    depth: usize,
) -> Vec<Annotation> {
    let mut results = Vec::new();

    // Check max depth
    if let Some(max_depth) = opts.max_depth {
        if depth > max_depth {
            return results;
        }
    }

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return results,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // Skip hidden files/dirs unless opted in
        if !opts.include_hidden {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }
        }

        if path.is_dir() {
            // Check if we should skip this directory
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if SKIP_DIRS.contains(&name) {
                    continue;
                }
            }
            results.extend(scan_directory(&path, patterns, opts, depth + 1));
        } else if path.is_file() && !is_skippable(&path) {
            results.extend(scan_file(&path, patterns));
        }
    }

    results
}

// ── Git blame integration ──

#[derive(Debug)]
struct BlameInfo {
    author: String,
    timestamp: u64,
    relative: String,
}

/// Try to get git blame info for a specific line in a file.
fn get_blame_info(file: &Path, line: usize) -> Option<BlameInfo> {
    // Find git root
    let mut dir = file.parent()?;
    loop {
        if dir.join(".git").exists() {
            break;
        }
        dir = dir.parent()?;
    }

    let output = std::process::Command::new("git")
        .args(["-C", dir.to_str()?, "blame", "-L", &format!("{line},{line}"), "-p", "--", file.to_str()?])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut author = String::new();
    let mut timestamp: u64 = 0;

    for mut line in stdout.lines() {
        line = line.trim();
        if let Some(rest) = line.strip_prefix("author ") {
            author = rest.to_string();
        }
        if let Some(rest) = line.strip_prefix("author-time ") {
            timestamp = rest.parse().unwrap_or(0);
        }
        if line.is_empty() {
            break; // end of porcelain header
        }
    }

    let relative = if timestamp > 0 {
        format_relative_time(timestamp)
    } else {
        String::new()
    };

    if author.is_empty() && timestamp == 0 {
        // Not a committed line
        return None;
    }

    Some(BlameInfo { author, timestamp, relative })
}

fn format_relative_time(timestamp: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let diff = now.saturating_sub(timestamp);
    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else if diff < 604800 {
        format!("{}d ago", diff / 86400)
    } else if diff < 2592000 {
        format!("{}w ago", diff / 604800)
    } else {
        format!("{}mo ago", diff / 2592000)
    }
}

// ── Display ──

/// Display annotations grouped by directory.
pub fn display_todo(all: &[Annotation], opts: &TodoOptions) {
    if all.is_empty() {
        println!("{}", style::green("✓ No annotations found."));
        return;
    }

    // Count by kind
    let mut by_kind: HashMap<AnnotationKind, usize> = HashMap::new();
    for a in all {
        *by_kind.entry(a.kind).or_insert(0) += 1;
    }

    // Count by file
    let mut by_file: HashMap<&Path, usize> = HashMap::new();
    for a in all {
        *by_file.entry(&a.file).or_insert(0) += 1;
    }

    // Show summary first
    println!("{} {}\n", style::bold("🔍 Codebase Annotations"),
        style::dim(&format!("({} total in {} files)", all.len(), by_file.len())));

    let mut kind_counts: Vec<_> = by_kind.into_iter().collect();
    kind_counts.sort_by_key(|(k, _)| k.priority());

    for (kind, count) in &kind_counts {
        let bar = "■".repeat(*count.min(&20));
        let label = kind.label();
        println!("  {} {} {}", kind.color(label), style::dim(&format!("({count})")), kind.color(&bar));
    }
    println!();

    if opts.show_stats_only {
        // Show top files
        let mut file_counts: Vec<_> = by_file.into_iter().collect();
        file_counts.sort_by_key(|(_, c)| Reverse(*c));
        println!("{}", style::bold("📁 Top files:"));
        let max_show = file_counts.len().min(10);
        for (path, count) in &file_counts[..max_show] {
            let display = path.strip_prefix(".").unwrap_or(path).display();
            println!("  {}  {}", style::dim(&format!("{count:>3}")), display);
        }
        println!();
        return;
    }

    // Group by directory
    let mut dir_map: HashMap<PathBuf, Vec<&Annotation>> = HashMap::new();
    for a in all {
        let dir = a.file.parent().unwrap_or(Path::new(".")).to_path_buf();
        dir_map.entry(dir).or_default().push(a);
    }

    // Sort directories (root first)
    let mut dirs: Vec<_> = dir_map.keys().collect();
    dirs.sort_by_key(|d| (d.components().count(), d.display().to_string()));

    for dir in &dirs {
        let annotations = &dir_map[*dir];
        // Sort by file, then line
        let mut sorted: Vec<&Annotation> = annotations.iter().copied().collect();
        sorted.sort_by(|a, b| {
            a.file.cmp(&b.file)
                .then_with(|| a.line.cmp(&b.line))
        });

        let dir_display = dir.strip_prefix(".").unwrap_or(dir).display();
        println!("{}", style::bold(&format!("📁 ./{}", dir_display)));
        println!("{}", style::dim("│"));

        let mut current_file = PathBuf::new();
        for ann in &sorted {
            // Print file header when switching files
            if ann.file != current_file {
                current_file = ann.file.clone();
                let rel_path = current_file.strip_prefix(".").unwrap_or(&current_file);
                let count_in_file = sorted.iter().filter(|a| a.file == current_file).count();
                println!("  {} {}",
                    style::cyan(&format!("{}", rel_path.display())),
                    style::dim(&format!("({count_in_file})")));
            }

            // Format line:col
            let loc = style::dim(&format!("  │  {:>4}:{:<3}", ann.line, ann.col));

            // Kind badge
            let badge = ann.kind.color(&format!(" {} ", ann.kind.label()));

            // Blame info
            let blame_str = if opts.show_blame {
                if let Some(blame) = get_blame_info(&ann.file, ann.line) {
                    format!(" {}", style::dim(&format!("({} {})", blame.author, blame.relative)))
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            // The annotation text
            let text = if ann.text.is_empty() {
                ann.full_line.trim().to_string()
            } else {
                ann.text.to_string()
            };

            println!("{} {}{}  {}", loc, badge, blame_str, text);
        }
        println!();
    }

    // Final stats line
    println!("{}", style::dim(&format!("─")));
    println!("{} {} annotations in {} files across {} directories",
        style::bold(&all.len().to_string()),
        style::dim("total"),
        by_file.len(),
        dirs.len()
    );
}

// ── Public entry point ──

/// Main entry point for `ccat --todo`.
pub fn cat_todo(opts: &TodoOptions) {
    let patterns = build_patterns(&opts.custom_patterns);

    let mut all_annotations: Vec<Annotation> = Vec::new();

    for path in &opts.paths {
        let resolved = if path.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            path.clone()
        };

        if resolved.is_file() {
            all_annotations.extend(scan_file(&resolved, &patterns));
        } else if resolved.is_dir() {
            all_annotations.extend(scan_directory(&resolved, &patterns, opts, 0));
        } else {
            eprintln!("ccat --todo: path not found: {}", resolved.display());
        }
    }

    // Apply kind filter if specified
    let filtered = if opts.filter_kinds.is_empty() {
        all_annotations
    } else {
        let lower_filters: Vec<String> = opts.filter_kinds.iter()
            .map(|k| k.to_lowercase())
            .collect();
        let _patterns = default_patterns();
        all_annotations.into_iter().filter(|a| {
            let label = a.kind.label().to_lowercase();
            lower_filters.iter().any(|f| label.contains(f) || label.starts_with(f))
        }).collect()
    };

    // Sort by priority, then file, then line
    let mut sorted = filtered;
    sorted.sort_by(|a, b| {
        a.kind.priority().cmp(&b.kind.priority())
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });

    display_todo(&sorted, opts);
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(content: &str) -> (PathBuf, tempfile::NamedTempFile) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{content}").unwrap();
        (f.path().to_path_buf(), f)
    }

    #[test]
    fn test_scan_todo_basic() {
        let (path, _f) = temp_file("// TODO: implement this function\nfn foo() {}\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, AnnotationKind::Todo);
        assert_eq!(results[0].line, 1);
        assert!(results[0].text.contains("implement this function"));
    }

    #[test]
    fn test_scan_fixme() {
        let (path, _f) = temp_file("# FIXME: this crashes on edge case\nprint('hello')\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, AnnotationKind::Fixme);
        assert_eq!(results[0].line, 1);
    }

    #[test]
    fn test_scan_hack() {
        let (path, _f) = temp_file("// HACK: this is terrible but works\ndo_something();\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, AnnotationKind::Hack);
    }

    #[test]
    fn test_scan_bug() {
        let (path, _f) = temp_file("/* BUG: null pointer dereference */\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, AnnotationKind::Bug);
    }

    #[test]
    fn test_scan_optimize() {
        let (path, _f) = temp_file("// OPTIMIZE: this is O(n²)\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, AnnotationKind::Optimize);
    }

    #[test]
    fn test_scan_xxx() {
        let (path, _f) = temp_file("// XXX: this might not be correct\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, AnnotationKind::Review);
    }

    #[test]
    fn test_skip_binary_file() {
        let (path, _f) = temp_file("\x00\x01\x02\x03TODO: inside binary\n");
        let patterns = build_patterns(&[]);
        // Binary files should produce no results
        let results = scan_file(&path, &patterns);
        // The file has null bytes, but we use read_to_string which will fail -> empty
        assert!(results.is_empty() || results.len() == 0);
    }

    #[test]
    fn test_scan_multiple_matches() {
        let content = "\
// TODO: add error handling
fn main() {
    // FIXME: off-by-one in loop
    for i in 0..10 {
        // HACK: skip this case for now
        if i == 5 { continue; }
    }
    // BUG: doesn't handle negative numbers
}
";
        let (path, _f) = temp_file(content);
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 4, "expected 4 annotations, got {}", results.len());
        let kinds: Vec<&str> = results.iter().map(|a| a.kind.label()).collect();
        assert!(kinds.contains(&"TODO"));
        assert!(kinds.contains(&"FIXME"));
        assert!(kinds.contains(&"HACK"));
        assert!(kinds.contains(&"BUG"));
    }

    #[test]
    fn test_word_boundary_not_inside_longer_word() {
        // "FIXME" should NOT match inside "FIXMELATER" or "FIXMELATIONSHIP"
        let (path, _f) = temp_file("FIXMELATER\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert!(results.is_empty(), "should not match inside longer word");
    }

    #[test]
    fn test_word_boundary_annotation_start() {
        // "FIXME" at start of line should match
        let (path, _f) = temp_file("FIXME: starts at column 1\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].col, 1);
    }

    #[test]
    fn test_case_insensitive() {
        let (path, _f) = temp_file("// fixme: lowercase should also match\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, AnnotationKind::Fixme);
    }

    #[test]
    fn test_no_false_positives_in_comments_matching() {
        // "TODO" inside a word like "VITOdonto" should not match
        let (path, _f) = temp_file("some_vitodonto_value\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert!(results.is_empty());
    }

    #[test]
    fn test_annotation_kind_ordering() {
        assert!(AnnotationKind::Fixme.priority() < AnnotationKind::Todo.priority());
        assert!(AnnotationKind::Bug.priority() < AnnotationKind::Note.priority());
        assert!(AnnotationKind::Hack.priority() < AnnotationKind::Optimize.priority());
        assert!(AnnotationKind::Temp.priority() > AnnotationKind::Fixme.priority());
    }

    #[test]
    fn test_is_skippable_known_binary_ext() {
        assert!(is_skippable(Path::new("image.png")));
        assert!(is_skippable(Path::new("video.mp4")));
        assert!(is_skippable(Path::new("archive.zip")));
        assert!(!is_skippable(Path::new("main.rs")));
        assert!(!is_skippable(Path::new("style.css")));
        assert!(!is_skippable(Path::new("index.html")));
    }

    #[test]
    fn test_is_skippable_lock_files() {
        assert!(is_skippable(Path::new("Cargo.lock")));
        assert!(is_skippable(Path::new("package-lock.json")));
        assert!(!is_skippable(Path::new("Cargo.toml")));
    }

    #[test]
    fn test_temp_marker_detected() {
        let (path, _f) = temp_file("// TEMP: workaround for bug\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, AnnotationKind::Temp);
    }

    #[test]
    fn test_workaround_marker_detected() {
        let (path, _f) = temp_file("// WORKAROUND: library bug\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, AnnotationKind::Temp);
    }

    #[test]
    fn test_note_marker_detected() {
        let (path, _f) = temp_file("// NOTE: this is important\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, AnnotationKind::Note);
    }

    #[test]
    fn test_context_lines_included() {
        let content = "line1\nline2\n// TODO: do the thing\nline4\nline5\n";
        let (path, _f) = temp_file(content);
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        // Should have context from surrounding lines
        // Context excludes the annotation line itself
        assert!(results[0].context.len() >= 2, "should have context lines");
    }

    #[test]
    fn test_scan_directory_skips_node_modules() {
        let dir = tempfile::tempdir().unwrap();
        let node_modules = dir.path().join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        let bad_file = node_modules.join("bad.js");
        fs::write(&bad_file, "// TODO: bad code\n").unwrap();

        let opts = TodoOptions {
            paths: vec![dir.path().to_path_buf()],
            ..Default::default()
        };
        let patterns = build_patterns(&[]);
        let results = scan_directory(dir.path(), &patterns, &opts, 0);
        assert!(results.is_empty(), "node_modules should be skipped");
    }

    #[test]
    fn test_build_patterns_includes_custom() {
        let custom_patterns = vec!["CUSTOM".to_string(), "LEGACY".to_string()];
        let patterns = build_patterns(&custom_patterns);
        let found: Vec<&str> = patterns.iter().map(|(p, _)| *p).collect();
        assert!(found.contains(&"CUSTOM"));
        assert!(found.contains(&"LEGACY"));
        assert!(found.contains(&"TODO"));
    }

    #[test]
    fn test_perf_as_optimize() {
        let (path, _f) = temp_file("// PERF: slow code path\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, AnnotationKind::Optimize);
    }

    #[test]
    fn test_scan_empty_file() {
        let (path, _f) = temp_file("");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert!(results.is_empty());
    }

    #[test]
    fn test_pipe_annotation_detection() {
        // Check that "| TODO |" works (common in markdown tables)
        let (path, _f) = temp_file("| TODO | done |\n");
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        // "TODO" with non-alphanumeric after should match
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_annotation_kind_color_output() {
        // Just verify no panics
        let k = AnnotationKind::Fixme;
        let _ = k.color("test");
        let _ = k.label();
        let k2 = AnnotationKind::Todo;
        let _ = k2.color("test");
    }

    #[test]
    fn test_large_file_skipped() {
        let content = "x".repeat(2_000_000); // > 1MB
        let (path, _f) = temp_file(&content);
        let patterns = build_patterns(&[]);
        let results = scan_file(&path, &patterns);
        assert!(results.is_empty(), "large file should be skipped");
    }
}
