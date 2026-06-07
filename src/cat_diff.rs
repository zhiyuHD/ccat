use std::io::Write;

use console::Style;
use similar::{ChangeTag, DiffTag, TextDiff};

use crate::pager;

/// Line pair for side-by-side rendering.
struct SxsPair {
    left: Option<String>,
    right: Option<String>,
    left_tag: ChangeTag,
    right_tag: ChangeTag,
}

/// Unified diff output (existing behavior).
pub fn cat_diff(data: &[u8], path_a: &str, path_b: &str) {
    let text_a = String::from_utf8_lossy(data);
    let text_b = match std::fs::read_to_string(path_b) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ccat: {path_b}: {e}");
            return;
        }
    };

    let diff = TextDiff::from_lines(&text_a, &text_b);

    let style_ins = Style::new().green();
    let style_del = Style::new().red();
    let style_header = Style::new().bold();

    let mut stdout = std::io::stdout();
    let mut lines: Vec<String> = Vec::new();

    // Header
    lines.push(format!(
        "{}  {}",
        style_header.apply_to("---"),
        path_a,
    ));
    lines.push(format!(
        "{}  {}",
        style_header.apply_to("+++"),
        path_b,
    ));

    for (idx, group) in diff.grouped_ops(3).iter().enumerate() {
        if idx > 0 {
            lines.push(format!("{}", Style::new().dim().apply_to("@@ ... @@")));
        }

        for op in group {
            for change in diff.iter_changes(op) {
                let (sign, style) = match change.tag() {
                    ChangeTag::Delete => ("-", &style_del),
                    ChangeTag::Insert => ("+", &style_ins),
                    ChangeTag::Equal => (" ", &Style::new()),
                };
                let value = change.value();
                let display = value.strip_suffix('\n').unwrap_or(value);
                for line in display.split('\n') {
                    let styled = format!("{}{}", style.apply_to(sign), style.apply_to(line));
                    if !styled.trim_end().is_empty() || sign == " " {
                        lines.push(styled);
                    }
                }
            }
        }
    }

    if lines.len() <= 2 {
        writeln!(&mut stdout, "ccat: files are identical").ok();
        return;
    }

    // Paged output
    if lines.len() > 20 {
        pager::run_pager(&lines);
    } else {
        for line in &lines {
            let _ = writeln!(stdout, "{}", line);
        }
    }
}

/// Side-by-side diff output (new).
pub fn cat_diff_sxs(data: &[u8], path_a: &str, path_b: &str) {
    let text_a = String::from_utf8_lossy(data);
    let text_b = match std::fs::read_to_string(path_b) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ccat: {path_b}: {e}");
            return;
        }
    };

    if text_a == text_b {
        eprintln!("ccat: files are identical");
        return;
    }

    let diff = TextDiff::from_lines(&text_a, &text_b);
    let (_, term_w) = pager::terminal_size();
    // Each side gets half the terminal, minus 3 for " │ " separator
    let col_w = term_w.saturating_sub(3) / 2;
    let col_w = col_w.max(10); // at least 10 chars per side

    let old_lines: Vec<&str> = text_a.lines().collect();
    let new_lines: Vec<&str> = text_b.lines().collect();

    let style_ins = Style::new().green();
    let style_del = Style::new().red();
    let style_dim = Style::new().dim();
    let style_bold = Style::new().bold();

    let mut out_lines: Vec<String> = Vec::new();

    // Header
    out_lines.push(format!(
        "{:width$} {} {:<width$}",
        style_bold.apply_to(format!("--- {}", path_a)),
        style_dim.apply_to("│"),
        style_bold.apply_to(format!("+++ {}", path_b)),
        width = col_w,
    ));
    let sep = style_dim.apply_to("─");
    let hrule: String = std::iter::repeat(sep.to_string())
        .take(col_w)
        .collect();
    out_lines.push(format!(
        "{} {} {}",
        hrule,
        style_dim.apply_to("┼"),
        hrule,
    ));

    // Build side-by-side pairs from diff ops
    let mut pairs: Vec<SxsPair> = Vec::new();
    let mut old_pos = 0usize;
    let mut new_pos = 0usize;

    for op in diff.ops() {
        let (old_range, new_range) = (op.old_range(), op.new_range());

        // Equal context before this op
        while old_pos < old_range.start && new_pos < new_range.start {
            let line = truncate(old_lines[old_pos], col_w);
            pairs.push(SxsPair {
                left: Some(line.clone()),
                right: Some(line),
                left_tag: ChangeTag::Equal,
                right_tag: ChangeTag::Equal,
            });
            old_pos += 1;
            new_pos += 1;
        }

        match op.tag() {
            DiffTag::Equal => {
                for i in old_range {
                    let line = truncate(old_lines[i], col_w);
                    pairs.push(SxsPair {
                        left: Some(line.clone()),
                        right: Some(line),
                        left_tag: ChangeTag::Equal,
                        right_tag: ChangeTag::Equal,
                    });
                    old_pos += 1;
                    new_pos += 1;
                }
            }
            DiffTag::Delete => {
                for i in old_range {
                    pairs.push(SxsPair {
                        left: Some(truncate(old_lines[i], col_w)),
                        right: None,
                        left_tag: ChangeTag::Delete,
                        right_tag: ChangeTag::Equal,
                    });
                    old_pos += 1;
                }
            }
            DiffTag::Insert => {
                for i in new_range {
                    pairs.push(SxsPair {
                        left: None,
                        right: Some(truncate(new_lines[i], col_w)),
                        left_tag: ChangeTag::Equal,
                        right_tag: ChangeTag::Insert,
                    });
                    new_pos += 1;
                }
            }
            DiffTag::Replace => {
                let old_count = old_range.len();
                let new_count = new_range.len();
                let max = old_count.max(new_count);
                for i in 0..max {
                    let left = if i < old_count {
                        Some(truncate(old_lines[old_range.start + i], col_w))
                    } else {
                        None
                    };
                    let right = if i < new_count {
                        Some(truncate(new_lines[new_range.start + i], col_w))
                    } else {
                        None
                    };
                    pairs.push(SxsPair {
                        left,
                        right,
                        left_tag: ChangeTag::Delete,
                        right_tag: ChangeTag::Insert,
                    });
                }
                old_pos = old_range.end;
                new_pos = new_range.end;
            }
        }
    }

    // Remaining equal lines
    while old_pos < old_lines.len() && new_pos < new_lines.len() {
        let line = truncate(old_lines[old_pos], col_w);
        pairs.push(SxsPair {
            left: Some(line.clone()),
            right: Some(line),
            left_tag: ChangeTag::Equal,
            right_tag: ChangeTag::Equal,
        });
        old_pos += 1;
        new_pos += 1;
    }

    if pairs.is_empty() {
        writeln!(&mut std::io::stdout(), "ccat: files are identical").ok();
        return;
    }

    // Render all pairs into display lines
    let mut display_lines: Vec<String> = Vec::new();
    for pair in &pairs {
        let left_rendered = match &pair.left {
            Some(text) => {
                let styled = match pair.left_tag {
                    ChangeTag::Delete => style_del.apply_to(text).to_string(),
                    ChangeTag::Insert => style_ins.apply_to(text).to_string(),
                    _ => text.to_string(),
                };
                pad_right(&styled, col_w)
            }
            None => " ".repeat(col_w),
        };

        // Show a marker in the gutter
        let gutter = match (pair.left_tag, pair.right_tag) {
            (ChangeTag::Delete, ChangeTag::Equal) => style_del.apply_to("┊").to_string(),
            (ChangeTag::Equal, ChangeTag::Insert) => style_ins.apply_to("┊").to_string(),
            (ChangeTag::Delete, ChangeTag::Insert) => style_bold.apply_to("┃").to_string(),
            _ => style_dim.apply_to("│").to_string(),
        };

        let right_rendered = match &pair.right {
            Some(text) => {
                let styled = match pair.right_tag {
                    ChangeTag::Delete => style_del.apply_to(text).to_string(),
                    ChangeTag::Insert => style_ins.apply_to(text).to_string(),
                    _ => text.to_string(),
                };
                // Right side: truncate to col_w, no padding needed (last on line)
                truncate_ansi(&styled, col_w)
            }
            None => String::new(),
        };

        display_lines.push(format!("{} {} {}", left_rendered, gutter, right_rendered));
    }

    // Paged output
    if display_lines.len() > pager::terminal_size().0.saturating_sub(3) {
        pager::run_pager(&display_lines);
    } else {
        let mut stdout = std::io::stdout();
        for line in &display_lines {
            let _ = writeln!(stdout, "{}", line);
        }
    }
}

/// Truncate a string to at most `max` visible characters (ignoring ANSI codes).
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Count grapheme-visible-width chars approximately
    let mut count = 0usize;
    let mut result = String::with_capacity(max);
    for ch in s.chars() {
        if count >= max {
            break;
        }
        result.push(ch);
        count += 1;
    }
    if count < s.chars().count() {
        result.push('…');
    }
    result
}

/// Truncate an ANSI-styled string to max visible width, preserving color codes.
fn truncate_ansi(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut visible = 0usize;
    let mut result = String::with_capacity(max);
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Collect entire escape sequence
            result.push(ch);
            while let Some(&next) = chars.peek() {
                result.push(next);
                chars.next();
                if next == 'm' {
                    break;
                }
            }
            continue;
        }
        if visible >= max {
            // Skip remaining visible characters but keep collecting escape codes
            while let Some(&next) = chars.peek() {
                if next == '\x1b' {
                    result.push(next);
                    chars.next();
                    while let Some(n) = chars.next() {
                        result.push(n);
                        if n == 'm' {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
            result.push('…');
            break;
        }
        result.push(ch);
        visible += 1;
    }
    result
}

/// Pad a string (which may contain ANSI codes) to `width` visible columns.
fn pad_right(s: &str, width: usize) -> String {
    let visible = visible_width(s);
    if visible >= width {
        truncate_ansi(s, width)
    } else {
        format!("{}{}", s, " ".repeat(width - visible))
    }
}

/// Count visible characters in a string that may contain ANSI escape codes.
fn visible_width(s: &str) -> usize {
    let mut count = 0usize;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip until 'm'
            while let Some(next) = chars.next() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            count += 1;
        }
    }
    count
}
