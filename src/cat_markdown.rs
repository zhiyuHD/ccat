use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use pulldown_cmark::Options as CmarkOptions;
use syntect::easy::HighlightLines;
use syntect::highlighting::{ThemeSet, Style};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use termcolor::{ColorChoice, StandardStream};
use std::fmt::Write as FmtWrite;
use std::io::Write;

/// Flush accumulated cell text into the current row, then flush row into header/table.
fn flush_table_cell(
    cell: &mut String,
    row: &mut Vec<String>,
    header: &mut Vec<String>,
    rows: &mut Vec<Vec<String>>,
    is_header: &mut bool,
) {
    if !cell.is_empty() {
        row.push(std::mem::take(cell));
    }
    if !row.is_empty() {
        let r = std::mem::take(row);
        if *is_header {
            *header = r;
            *is_header = false;
        } else {
            rows.push(r);
        }
    }
}

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
    write!(&mut s, "\x1b[38;2;{};{};{}m", fg.r, fg.g, fg.b).ok();
    s.push_str(text);
    s
}

/// Render markdown to terminal with ANSI colors and syntax highlighting.
pub fn cat_markdown(data: &[u8]) {
    let s = String::from_utf8_lossy(data);

    let mut opts = CmarkOptions::empty();
    opts.insert(CmarkOptions::ENABLE_TABLES);
    opts.insert(CmarkOptions::ENABLE_FOOTNOTES);
    opts.insert(CmarkOptions::ENABLE_STRIKETHROUGH);
    opts.insert(CmarkOptions::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(&s, opts);

    let ss = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let theme = &ts.themes["base16-ocean.dark"];
    let mut stdout = StandardStream::stdout(ColorChoice::Auto);

    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();
    let mut in_list = false;

    // Table tracking
    let mut in_table = false;
    let mut in_table_head = false;
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell: String = String::new();
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut table_header: Vec<String> = Vec::new();

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    let ansi = match level {
                        HeadingLevel::H1 => "\x1b[1;38;5;220m",
                        HeadingLevel::H2 => "\x1b[1;38;5;215m",
                        HeadingLevel::H3 => "\x1b[1;38;5;114m",
                        _ => "\x1b[1;38;5;146m",
                    };
                    write!(&mut stdout, "\n{ansi}").ok();
                }
                Tag::Paragraph => {}
                Tag::CodeBlock(kind) => {
                    in_code_block = true;
                    code_lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(lang) => lang.to_string(),
                        pulldown_cmark::CodeBlockKind::Indented => String::new(),
                    };
                    code_buf.clear();
                    write!(
                        &mut stdout,
                        "\x1b[2mCode Type: \"{}\"\x1b[0m\n",
                        code_lang
                    )
                    .ok();
                }
                Tag::List(..) => {
                    in_list = true;
                }
                Tag::Item => {
                    write!(&mut stdout, "  \x1b[38;5;220m•\x1b[0m ").ok();
                }
                Tag::Emphasis => {
                    write!(&mut stdout, "\x1b[3m").ok();
                }
                Tag::Strong => {
                    write!(&mut stdout, "\x1b[1m").ok();
                }
                Tag::Link { dest_url: _, .. } => {
                    write!(&mut stdout, "\x1b[4;94m").ok();
                }
                Tag::BlockQuote(_) => {
                    write!(&mut stdout, "\x1b[2;37m│ ").ok();
                }
                Tag::Strikethrough => {
                    write!(&mut stdout, "\x1b[9m").ok();
                }
                Tag::Image { .. } | Tag::TableCell | Tag::TableHead | Tag::TableRow => {}
                Tag::Table(..) => {
                    in_table = true;
                    table_rows.clear();
                    table_header.clear();
                    current_row.clear();
                    current_cell.clear();
                    in_table_head = true;
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    write!(&mut stdout, "\x1b[0m\n\n").ok();
                }
                TagEnd::Paragraph => {
                    writeln!(&mut stdout).ok();
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    let syntax = ss
                        .find_syntax_by_token(&code_lang)
                        .unwrap_or_else(|| ss.find_syntax_plain_text());
                    let mut highlighter = HighlightLines::new(syntax, theme);

                    let bg = "\x1b[48;5;235m";
                    for line in LinesWithEndings::from(&code_buf) {
                        // Strip newline; highlight_line works fine without it
                        let line_text = line.trim_end_matches('\n').trim_end_matches('\r');

                        write!(&mut stdout, "{bg}").ok();
                        if !line_text.is_empty() {
                            if let Ok(ranges) = highlighter.highlight_line(line_text, &ss) {
                                for (style, text) in &ranges {
                                    write!(&mut stdout, "{}", style_to_ansi(style, text)).ok();
                                    write!(&mut stdout, "{bg}").ok();
                                }
                            }
                        }
                        // Clear to end of line so background fills the whole row
                        // Must be done BEFORE newline
                        write!(&mut stdout, "\x1b[K").ok();
                        // Reset, then newline
                        writeln!(&mut stdout, "\x1b[0m").ok();
                    }
                    code_buf.clear();
                }
                TagEnd::List(_) => {
                    if in_list {
                        writeln!(&mut stdout).ok();
                        in_list = false;
                    }
                }
                TagEnd::Item => {
                    writeln!(&mut stdout).ok();
                }
                TagEnd::Emphasis => {
                    write!(&mut stdout, "\x1b[23m").ok();
                }
                TagEnd::Strong => {
                    write!(&mut stdout, "\x1b[22m").ok();
                }
                TagEnd::Link => {
                    write!(&mut stdout, "\x1b[0m").ok();
                }
                TagEnd::BlockQuote(_) => {}
                TagEnd::Strikethrough => {
                    write!(&mut stdout, "\x1b[29m").ok();
                }
                TagEnd::Table => {
                    // Push final row if any
                    flush_table_cell(&mut current_cell, &mut current_row, &mut table_header, &mut table_rows, &mut in_table_head);
                    in_table_head = false;
                    // Render the collected table with aligned columns
                    if !table_header.is_empty() || !table_rows.is_empty() {
                        let all_rows: Vec<&Vec<String>> = table_rows.iter().collect();
                        let header_ref: Option<&Vec<String>> = if !table_header.is_empty() { Some(&table_header) } else { None };

                        // Calculate column widths
                        let num_cols = if let Some(h) = header_ref {
                            h.len()
                        } else if let Some(r) = all_rows.first() {
                            r.len()
                        } else { 0 };

                        let mut widths = vec![0usize; num_cols];
                        if let Some(h) = header_ref {
                            for (i, cell) in h.iter().enumerate() {
                                if i < widths.len() { widths[i] = widths[i].max(cell.len()); }
                            }
                        }
                        for row in &all_rows {
                            for (i, cell) in row.iter().enumerate() {
                                if i < widths.len() { widths[i] = widths[i].max(cell.len()); }
                            }
                        }

                        // Separator line
                        let total_w = widths.iter().sum::<usize>() + widths.len() * 3 + 1;
                        writeln!(&mut stdout, "\x1b[2m{}\x1b[0m", "─".repeat(total_w)).ok();

                        // Header row
                        if let Some(h) = header_ref {
                            write!(&mut stdout, "\x1b[2m│\x1b[0m ").ok();
                            for (i, cell) in h.iter().enumerate() {
                                write!(&mut stdout, "\x1b[1m{:width$}\x1b[0m", cell, width = widths[i]).ok();
                                if i + 1 < h.len() {
                                    write!(&mut stdout, " \x1b[2m│\x1b[0m ").ok();
                                }
                            }
                            writeln!(&mut stdout, " \x1b[2m│\x1b[0m").ok();
                            writeln!(&mut stdout, "\x1b[2m{}\x1b[0m", "─".repeat(total_w)).ok();
                        }

                        // Data rows
                        for row in &all_rows {
                            write!(&mut stdout, "\x1b[2m│\x1b[0m ").ok();
                            for (i, cell) in row.iter().enumerate() {
                                write!(&mut stdout, "{:width$}", cell, width = widths[i]).ok();
                                if i + 1 < row.len() {
                                    write!(&mut stdout, " \x1b[2m│\x1b[0m ").ok();
                                }
                            }
                            writeln!(&mut stdout, " \x1b[2m│\x1b[0m").ok();
                            writeln!(&mut stdout, "\x1b[2m{}\x1b[0m", "─".repeat(total_w)).ok();
                        }
                        writeln!(&mut stdout).ok();
                    }
                    in_table = false;
                    in_table_head = false;
                }
                TagEnd::TableHead => {
                    flush_table_cell(&mut current_cell, &mut current_row, &mut table_header, &mut table_rows, &mut in_table_head);
                }
                TagEnd::TableRow => {
                    flush_table_cell(&mut current_cell, &mut current_row, &mut table_header, &mut table_rows, &mut in_table_head);
                }
                TagEnd::TableCell => {
                    current_row.push(std::mem::take(&mut current_cell));
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_code_block {
                    code_buf.push_str(&text);
                } else if in_table {
                    current_cell.push_str(&text);
                } else {
                    write!(&mut stdout, "{text}").ok();
                }
            }
            Event::Code(text) => {
                if in_table {
                    current_cell.push_str(&text);
                } else {
                    write!(
                        &mut stdout,
                        "\x1b[48;5;236m\x1b[38;5;215m{text}\x1b[0m"
                    )
                    .ok();
                }
            }
            Event::SoftBreak => {
                if in_table {
                    current_cell.push('\n');
                } else {
                    writeln!(&mut stdout).ok();
                }
            }
            Event::HardBreak => {
                if in_table {
                    current_cell.push('\n');
                } else {
                    writeln!(&mut stdout).ok();
                }
            }
            Event::Rule => {
                writeln!(&mut stdout, "\x1b[2m{}\x1b[0m", "─".repeat(80)).ok();
            }
            Event::FootnoteReference(_) => {}
            Event::TaskListMarker(checked) => {
                if checked {
                    write!(&mut stdout, "\x1b[92m☑ \x1b[0m").ok();
                } else {
                    write!(&mut stdout, "\x1b[90m☐ \x1b[0m").ok();
                }
            }
            Event::InlineMath(_) | Event::DisplayMath(_) | Event::InlineHtml(_) | Event::Html(_) => {
            }
        }
    }

    print!("\x1b[0m");
}
