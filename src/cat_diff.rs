use std::io::Write;

use console::Style;
use similar::{ChangeTag, TextDiff};

use crate::pager;

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
    let (term_height, _) = pager::terminal_size();
    let page_size = term_height.saturating_sub(2).max(5);
    let total_pages = lines.len().div_ceil(page_size);
    let mut current_page: usize = 0;

    loop {
        let start = current_page * page_size;
        let end = (start + page_size).min(lines.len());

        for line in &lines[start..end] {
            let _ = writeln!(stdout, "{}", line);
        }

        if total_pages > 1 {
            let action = pager::page_footer(
                &mut stdout, current_page, total_pages,
                start, end, lines.len(),
            );
            match action {
                pager::PageAction::Quit => break,
                pager::PageAction::Next => {
                    if current_page + 1 < total_pages {
                        current_page += 1;
                    }
                }
                pager::PageAction::Prev => {
                    if current_page > 0 {
                        current_page -= 1;
                    }
                }
                pager::PageAction::None => {}
            }
        } else {
            break;
        }
    }
}
