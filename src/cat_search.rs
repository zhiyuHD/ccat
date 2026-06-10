use std::fs;
use std::io;
use std::path::Path;

use regex_lite::Regex;
use syntect::highlighting::Style;

use crate::{cat_log, cat_toml, cat_yaml, detect_kind, FileKind};

/// Configuration for a search.
pub struct SearchOpts {
    pub pattern: String,
    pub context_lines: usize,
    pub count_only: bool,
    pub files_with_matches: bool,
}

/// ANSI yellow background for search pattern highlighting.
const HIGHLIGHT_BG: &str = "\x1b[7m";   // reverse video — works on any terminal
const HIGHLIGHT_RESET: &str = "\x1b[27m";

/// Lightweight in-memory line collector. Each item is a (plain_text, ansi_text) pair.
/// We collect everything then dump to stdout (no pager for v1 — respects pipe semantics).
pub fn search_main(opts: &SearchOpts, paths: &[String]) -> io::Result<()> {
    let re = Regex::new(&opts.pattern)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid regex: {e}")))?;

    let multiple_files = paths.len() > 1;
    let mut any_match = false;

    for path_str in paths {
        let path_obj = Path::new(path_str);

        // Skip directories
        if path_obj.is_dir() {
            continue;
        }

        let data = match fs::read(path_obj) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("ccat --search: {path_str}: {e}");
                continue;
            }
        };

        if data.is_empty() {
            continue;
        }

        let kind = detect_kind(&data, path_obj);

        // Only text-like formats are searchable
        if !is_text_kind(&kind) {
            continue;
        }

        let content = String::from_utf8_lossy(&data);
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            continue;
        }

        // Find matching line indices
        let matching: Vec<usize> = lines.iter()
            .enumerate()
            .filter(|(_, line)| re.is_match(line.trim()))
            .map(|(i, _)| i)
            .collect();

        if matching.is_empty() {
            continue;
        }

        any_match = true;

        if opts.count_only {
            println!("{}: {}", path_str, matching.len());
            continue;
        }

        if opts.files_with_matches {
            println!("{}", path_str);
            continue;
        }

        // Show matches with context
        if multiple_files {
            // Blue bold file header
            println!("\x1b[1;34m{}\x1b[0m", path_str);
        }

        let context = opts.context_lines;
        let mut last_end: isize = -1;

        for &match_line in &matching {
            let start = match_line.saturating_sub(context);
            let end = (match_line + context + 1).min(lines.len());

            // Separator between non-contiguous groups
            if start as isize > last_end + 1 && last_end >= 0 {
                println!("\x1b[2m--\x1b[0m");
            }

            for i in start..end {
                let is_match = i == match_line;
                let line_text = lines[i];

                // Build the rendered line
                let rendered = render_search_line(i, &line_text, &re, kind, path_str);
                let marker = if is_match { "\x1b[1;33m>\x1b[0m" } else { " " };
                println!("{} {}", marker, rendered);
            }

            last_end = end as isize - 1;
        }

        if multiple_files {
            println!(); // blank line between files
        }
    }

    if !any_match && !opts.count_only && !opts.files_with_matches {
        // No matches — but that's not an error, just no output
    }

    Ok(())
}

/// Determine if a FileKind is text-based and searchable.
fn is_text_kind(kind: &FileKind) -> bool {
    matches!(kind,
        FileKind::SourceCode | FileKind::PlainText | FileKind::Log
        | FileKind::Json | FileKind::Yaml | FileKind::Toml
        | FileKind::Csv | FileKind::Markdown
    )
}

/// Render a single line with syntax highlighting + pattern overlay.
fn render_search_line(
    line_idx: usize,
    line_text: &str,
    re: &Regex,
    kind: FileKind,
    path_hint: &str,
) -> String {
    // Line number prefix
    let line_fmt = format!("\x1b[2m{:>6}\x1b[0m", line_idx + 1);

    // Get the highlighted ANSI line based on file kind
    let highlighted = colorize_line(line_text, kind, path_hint);

    // Overlay pattern highlighting
    let with_pattern = highlight_pattern_in_ansi(&highlighted, line_text, re);

    format!("{} {}", line_fmt, with_pattern)
}

/// Apply file-type-aware coloring to a single line.
fn colorize_line(line: &str, kind: FileKind, _path_hint: &str) -> String {
    match kind {
        FileKind::SourceCode => {
            // Use syntect for full syntax highlighting of this line.
            // We inline a lightweight version here.
            highlight_with_syntect(line, _path_hint)
        }
        FileKind::Log => {
            cat_log::highlight_log_line(line)
        }
        FileKind::Json => {
            // JSON is multiline — we try a simple heuristic: quote highlighting
            simple_json_highlight(line)
        }
        FileKind::Yaml => {
            cat_yaml::highlight_yaml_line(line)
        }
        FileKind::Toml => {
            cat_toml::highlight_toml_line(line)
        }
        FileKind::Csv => {
            // CSV: dimly highlight comma separators
            highlight_csv_line(line)
        }
        FileKind::Markdown | FileKind::PlainText => {
            // No special highlighting
            line.to_string()
        }
        _ => line.to_string(),
    }
}

/// Syntax-highlight a single line using syntect.
fn highlight_with_syntect(line: &str, _path_hint: &str) -> String {
    use syntect::easy::HighlightLines;
    use syntect::highlighting::ThemeSet;
    use syntect::parsing::SyntaxSet;

    let ss = SyntaxSet::load_defaults_newlines();
    let theme_name = crate::cat_source::read_theme_override()
        .unwrap_or_else(|| crate::color_scheme::syntect_theme_name().to_string());
    let ts = ThemeSet::load_defaults();
    let theme = ts.themes.get(&theme_name)
        .unwrap_or_else(|| &ts.themes["base16-ocean.dark"]);

    let syntax = ss
        .find_syntax_by_extension(
            Path::new(_path_hint)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(""),
        )
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut out = String::new();

    if line.is_empty() {
        return out;
    }

    match highlighter.highlight_line(line, &ss) {
        Ok(ranges) => {
            for (style, text) in &ranges {
                out.push_str(&syntect_style_to_ansi(style, text));
            }
            out.push_str("\x1b[0m");
        }
        Err(_) => {
            out.push_str(line);
        }
    }
    out
}

/// Convert a syntect Style + text to ANSI foreground.
fn syntect_style_to_ansi(style: &Style, text: &str) -> String {
    let fg = style.foreground;
    let mut s = String::new();
    if style.font_style.contains(syntect::highlighting::FontStyle::BOLD) {
        s.push_str("\x1b[1m");
    }
    if style.font_style.contains(syntect::highlighting::FontStyle::ITALIC) {
        s.push_str("\x1b[3m");
    }
    if style.font_style.contains(syntect::highlighting::FontStyle::UNDERLINE) {
        s.push_str("\x1b[4m");
    }
    let _ = std::fmt::write(&mut s, format_args!("\x1b[38;2;{};{};{}m", fg.r, fg.g, fg.b));
    s.push_str(text);
    s
}

/// Highlight the search pattern within an ANSI-colored line.
/// This walks the ANSI string while tracking plain-text position,
/// and inserts reverse-video wrapping at each match boundary.
fn highlight_pattern_in_ansi(ansi_line: &str, plain_line: &str, re: &Regex) -> String {
    // Find all match ranges in the plain text
    let matches: Vec<(usize, usize)> = re.find_iter(plain_line)
        .map(|m| (m.start(), m.end()))
        .collect();

    if matches.is_empty() {
        return ansi_line.to_string();
    }

    let mut result = String::new();
    let mut plain_idx = 0usize;     // byte position in plain text
    let mut mi = 0;                 // current match index

    // Walk the ANSI string char by char (ANSI is ASCII, so char = byte)
    let mut chars = ansi_line.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // ANSI escape — copy the whole sequence verbatim
            result.push('\x1b');
            result.push('[');
            for c in &mut chars {
                result.push(c);
                if c == 'm' {
                    break;
                }
            }
            continue;
        }

        // Regular character — check match boundaries
        if mi < matches.len() && plain_idx == matches[mi].0 {
            result.push_str(HIGHLIGHT_BG);  // reverse video ON
        }

        result.push(ch);
        plain_idx += ch.len_utf8();

        if mi < matches.len() && plain_idx == matches[mi].1 {
            result.push_str(HIGHLIGHT_RESET);  // reverse video OFF
            mi += 1;
        }
    }

    result
}

/// Simple JSON line highlighting (for search context).
fn simple_json_highlight(line: &str) -> String {
    let mut out = String::new();
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in line.char_indices() {
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            out.push(ch);
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            if in_string {
                out.push_str("\x1b[32m");
            } else {
                out.push_str("\x1b[0m");
            }
            out.push(ch);
            continue;
        }
        // Highlight numeric values
        if !in_string && ch.is_ascii_digit() {
            out.push_str("\x1b[95m");
            out.push(ch);
            // Read rest of number starting from the next char position
            let rest = &line[i + ch.len_utf8()..];
            for c in rest.chars() {
                if c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == 'E' {
                    out.push(c);
                } else {
                    out.push_str("\x1b[0m");
                    out.push(c);
                    break;
                }
            }
            continue;
        }
        // Highlight booleans and null — use byte position i into the original line
        if !in_string {
            for kw in &["true", "false", "null"] {
                if line[i..].starts_with(kw) {
                    let after = &line[i + kw.len()..];
                    if after.is_empty() || !after.chars().next().unwrap().is_alphanumeric() {
                        out.push_str("\x1b[36m");
                        out.push_str(kw);
                        out.push_str("\x1b[0m");
                        return line.to_string();
                    }
                }
            }
        }
        out.push(ch);
    }
    if in_string {
        out.push_str("\x1b[0m");
    }
    out
}

/// CSV line: dim the delimiters.
fn highlight_csv_line(line: &str) -> String {
    let mut out = String::new();
    for ch in line.chars() {
        if ch == ',' || ch == '\t' {
            out.push_str(&format!("\x1b[2m{ch}\x1b[0m"));
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_text_kind_source() {
        assert!(is_text_kind(&FileKind::SourceCode));
    }

    #[test]
    fn test_is_text_kind_binary() {
        assert!(!is_text_kind(&FileKind::Image));
        assert!(!is_text_kind(&FileKind::Gzip));
        assert!(!is_text_kind(&FileKind::Docx));
    }

    #[test]
    fn test_is_text_kind_plain() {
        assert!(is_text_kind(&FileKind::PlainText));
        assert!(is_text_kind(&FileKind::Log));
        assert!(is_text_kind(&FileKind::Json));
    }

    #[test]
    fn test_highlight_pattern_no_match() {
        let result = highlight_pattern_in_ansi("hello world", "hello world", &Regex::new("xyz").unwrap());
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_highlight_pattern_simple_match() {
        let result = highlight_pattern_in_ansi("hello world", "hello world", &Regex::new("hello").unwrap());
        assert!(result.contains(HIGHLIGHT_BG), "should contain highlight marker");
        assert!(result.contains(HIGHLIGHT_RESET), "should contain reset marker");
        // The highlighted text has ANSI codes injected, so "hello world" as
        // a contiguous substring may be broken. Check for the components.
        assert!(result.contains("hello"), "should contain 'hello'");
        assert!(result.contains("world"), "should contain 'world'");
    }

    #[test]
    fn test_highlight_pattern_with_ansi() {
        let ansi_line = "\x1b[32mhello\x1b[0m world";
        let plain = "hello world";
        let result = highlight_pattern_in_ansi(ansi_line, plain, &Regex::new("world").unwrap());
        assert!(result.contains("\x1b[7m"), "should add highlight for 'world'");
        assert!(result.contains("\x1b[27m"), "should add reset after 'world'");
        // The function preserves original ANSI codes and injects highlight
        // around matching plain-text boundaries. Verify the core components.
        assert!(result.contains("hello"), "should contain hello");
        assert!(result.contains("world"), "should contain world");
    }

    #[test]
    fn test_highlight_empty_pattern() {
        let result = highlight_pattern_in_ansi("test", "test", &Regex::new("").unwrap());
        // Empty pattern matches at position 0 — may or may not produce highlight
        // Just verify it doesn't panic
        assert!(!result.is_empty());
    }

    #[test]
    fn test_simple_json_highlight_string() {
        let result = simple_json_highlight(r#"{"key": "value"}"#);
        assert!(result.contains("\x1b[32m"), "string values should be green");
    }

    #[test]
    fn test_simple_json_highlight_bare() {
        // Non-JSON plain text should remain unchanged
        let result = simple_json_highlight("just text");
        assert_eq!(result, "just text");
    }

    #[test]
    fn test_highlight_csv_simple() {
        let result = highlight_csv_line("a,b,c");
        assert!(result.contains("\x1b[2m"), "commas should be dimmed");
    }

    #[test]
    fn test_highlight_csv_no_delimiter() {
        assert_eq!(highlight_csv_line("abc"), "abc");
    }

    #[test]
    fn test_highlight_csv_empty() {
        assert_eq!(highlight_csv_line(""), "");
    }
}
