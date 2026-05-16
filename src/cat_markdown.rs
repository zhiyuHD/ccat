use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::{as_24_bit_terminal_escaped, LinesWithEndings};
use termcolor::{ColorChoice, StandardStream};
use std::io::Write;

/// Render markdown to terminal with ANSI colors and syntax highlighting.
pub fn cat_markdown(data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    let parser = Parser::new(&s);

    // Load syntax highlighting resources
    let ss = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    // Use a dark theme by default
    let theme = &ts.themes["base16-ocean.dark"];
    let mut stdout = StandardStream::stdout(ColorChoice::Auto);

    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();

    for event in parser {
        match event {
            Event::Start(tag) => {
                match tag {
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
                        // Print code block header
                        if !code_lang.is_empty() {
                            write!(&mut stdout, "\x1b[2m```{} \x1b[0m\n", code_lang).ok();
                        }
                    }
                    Tag::List(..) => {}
                    Tag::Item => {
                        write!(&mut stdout, "  \x1b[38;5;220m•\x1b[0m ").ok();
                    }
                    Tag::Emphasis => {
                        write!(&mut stdout, "\x1b[3m").ok(); // italic
                    }
                    Tag::Strong => {
                        write!(&mut stdout, "\x1b[1m").ok(); // bold
                    }
                    Tag::Link { dest_url: _, .. } => {
                        write!(&mut stdout, "\x1b[4;94m").ok(); // underline blue
                    }
                    Tag::BlockQuote(_) => {
                        write!(&mut stdout, "\x1b[2;37m│ ").ok(); // dim gray
                    }
                    Tag::Strikethrough => {
                        write!(&mut stdout, "\x1b[9m").ok(); // strikethrough
                    }
                    Tag::Image { .. } => {}
                    Tag::Table(..) => {}
                    Tag::TableHead => {}
                    Tag::TableRow => {}
                    Tag::TableCell => {}
                    _ => {}
                }
            }
            Event::End(tag_end) => {
                match tag_end {
                    TagEnd::Heading(_) => {
                        write!(&mut stdout, "\x1b[0m\n\n").ok();
                    }
                    TagEnd::Paragraph => {
                        writeln!(&mut stdout).ok();
                    }
                    TagEnd::CodeBlock => {
                        in_code_block = false;
                        // Highlight and print code buffer
                        let syntax = ss.find_syntax_by_token(&code_lang)
                            .unwrap_or_else(|| ss.find_syntax_plain_text());
                        let mut highlighter = HighlightLines::new(syntax, theme);
                        // Print dim border before code
                        write!(&mut stdout, "\x1b[2m\x1b[48;5;235m").ok();
                        for line in LinesWithEndings::from(&code_buf) {
                            if let Ok(ranges) = highlighter.highlight_line(line, &ss) {
                                let escaped = as_24_bit_terminal_escaped(&ranges[..], true);
                                write!(&mut stdout, "{escaped}").ok();
                            }
                        }
                        write!(&mut stdout, "\x1b[0m\n").ok();
                        code_buf.clear();
                    }
                    TagEnd::List(_) => {
                        writeln!(&mut stdout).ok();
                    }
                    TagEnd::Item => {}
                    TagEnd::Emphasis => {
                        write!(&mut stdout, "\x1b[23m").ok(); // un-italic
                    }
                    TagEnd::Strong => {
                        write!(&mut stdout, "\x1b[22m").ok(); // un-bold
                    }
                    TagEnd::Link => {
                        write!(&mut stdout, "\x1b[0m").ok(); // reset
                    }
                    TagEnd::BlockQuote(_) => {}
                    TagEnd::Strikethrough => {
                        write!(&mut stdout, "\x1b[29m").ok(); // no strikethrough
                    }
                    TagEnd::Table => {}
                    TagEnd::TableHead => {}
                    TagEnd::TableRow => {
                        writeln!(&mut stdout).ok();
                    }
                    TagEnd::TableCell => {
                        write!(&mut stdout, "  ").ok();
                    }
                    _ => {}
                }
            }
            Event::Text(text) => {
                if in_code_block {
                    code_buf.push_str(&text);
                } else {
                    write!(&mut stdout, "{text}").ok();
                }
            }
            Event::Code(text) => {
                // Inline code
                write!(&mut stdout, "\x1b[48;5;236m\x1b[38;5;215m{text}\x1b[0m").ok();
            }
            Event::SoftBreak => {
                writeln!(&mut stdout).ok();
            }
            Event::HardBreak => {
                writeln!(&mut stdout).ok();
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
            Event::InlineMath(_) | Event::DisplayMath(_) | Event::InlineHtml(_) | Event::Html(_) => {}
        }
    }

    // Ensure we end with reset
    print!("\x1b[0m");
}
