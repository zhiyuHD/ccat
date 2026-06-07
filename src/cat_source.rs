use std::io::Write;

use syntect::easy::HighlightLines;
use syntect::highlighting::{ThemeSet, Style};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Convert a syntect Style + text segment to ANSI 24-bit foreground-only string.
fn style_to_ansi(style: &Style, text: &str) -> String {
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

/// Highlight source code using syntect, auto-detecting language by file extension.
///
/// Uses the filename (or a hint) to determine the syntax, falling back to
/// plain text if no matching syntax is found.
pub fn cat_source(data: &[u8], filename_hint: &str) {
    let ss = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let theme = &ts.themes["base16-ocean.dark"];

    let syntax = ss
        .find_syntax_by_extension(
            std::path::Path::new(filename_hint)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(""),
        )
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let s = String::from_utf8_lossy(data);
    let mut highlighter = HighlightLines::new(syntax, theme);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    for line in LinesWithEndings::from(&s) {
        let line_text = line.trim_end_matches('\n').trim_end_matches('\r');
        if line_text.is_empty() {
            let _ = writeln!(handle);
            continue;
        }
        match highlighter.highlight_line(line_text, &ss) {
            Ok(ranges) => {
                for (style, text) in &ranges {
                    let _ = write!(handle, "{}", style_to_ansi(style, text));
                }
                let _ = writeln!(handle, "\x1b[0m");
            }
            Err(_) => {
                let _ = writeln!(handle, "{line_text}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_style_to_ansi_plain() {
        let style = Style {
            foreground: syntect::highlighting::Color { r: 255, g: 0, b: 0, a: 255 },
            background: syntect::highlighting::Color::BLACK,
            font_style: syntect::highlighting::FontStyle::empty(),
        };
        let result = style_to_ansi(&style, "hello");
        assert!(result.contains("hello"));
        assert!(result.contains("38;2;255;0;0"));
    }

    #[test]
    fn test_style_to_ansi_bold() {
        let style = Style {
            foreground: syntect::highlighting::Color { r: 0, g: 255, b: 0, a: 255 },
            background: syntect::highlighting::Color::BLACK,
            font_style: syntect::highlighting::FontStyle::BOLD,
        };
        let result = style_to_ansi(&style, "bold");
        assert!(result.contains("\x1b[1m"));
    }

    #[test]
    fn test_cat_source_rust_syntax() {
        let code = b"fn main() {\n    println!(\"hello\");\n}\n";
        // Should not panic, should produce some output
        // We can't easily capture stdout, but we can test it doesn't crash
        cat_source(code, "test.rs");
    }

    #[test]
    fn test_cat_source_python_syntax() {
        let code = b"def hello():\n    print('world')\n";
        cat_source(code, "test.py");
    }

    #[test]
    fn test_cat_source_unknown_extension() {
        let code = b"some random text\n";
        cat_source(code, "test.xyz123");
    }

    #[test]
    fn test_cat_source_no_extension() {
        let code = b"plain text file\n";
        cat_source(code, "Makefile");
    }

    #[test]
    fn test_cat_source_empty() {
        cat_source(b"", "empty.rs");
        cat_source(b"", "");
    }
}
