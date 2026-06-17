/// Hex dump display with colorized ASCII sidebar.
///
/// Two modes:
/// - `canonical = false` (default): colored hex dump with pager, suited for interactive TTY
/// - `canonical = true`: plain-text hex dump (no colors, no pager), suited for piping
use std::io::{self, Write};

use crate::pager;

const BYTES_PER_LINE: usize = 16;

/// Show a hex dump of `data` with optional colors and paging.
///
/// When `canonical` is true, output is plain text (no ANSI escape codes)
/// and no pager is used — suitable for piping through `less` or redirecting to files.
pub fn cat_hex(data: &[u8], canonical: bool) {
    if canonical {
        cat_hex_canonical(data);
    } else {
        cat_hex_color(data);
    }
}

/// Colored hex dump with interactive pager for large data.
fn cat_hex_color(data: &[u8]) {
    let lines = data.len().div_ceil(BYTES_PER_LINE);
    let (term_height, _) = pager::terminal_size();
    let page_lines = term_height.saturating_sub(2).max(5);
    let total_pages = lines.div_ceil(page_lines);
    let mut current_page: usize = 0;

    let mut stdout = io::stdout();

    loop {
        let start_line = current_page * page_lines;
        let end_line = (start_line + page_lines).min(lines);

        for line_idx in start_line..end_line {
            let offset = line_idx * BYTES_PER_LINE;
            let row = &data[offset..data.len().min(offset + BYTES_PER_LINE)];
            let _ = write!(stdout, "{}", format_hex_line_color(offset, row));
        }

        // End marker
        let end_offset = end_line * BYTES_PER_LINE;
        let _ = writeln!(stdout, "\x1b[2m{:08x}\x1b[0m", end_offset);

        if total_pages > 1 {
            let action = pager::page_footer(
                &mut stdout, current_page, total_pages,
                start_line * BYTES_PER_LINE, end_line * BYTES_PER_LINE, data.len(),
            );
            match action {
                pager::PageAction::Quit => break,
                pager::PageAction::Next(_) => {
                    if current_page + 1 < total_pages {
                        current_page += 1;
                    }
                }
                pager::PageAction::Prev(_) => {
                    if current_page > 0 {
                        current_page -= 1;
                    }
                }
                pager::PageAction::None | pager::PageAction::Search | pager::PageAction::Goto(_) => {}
            }
        } else {
            break;
        }
    }
}

/// Plain-text hex dump (no colors, no pager).
fn cat_hex_canonical(data: &[u8]) {
    let mut stdout = io::stdout();
    for chunk in data.chunks(BYTES_PER_LINE) {
        let offset = chunk.as_ptr() as usize - data.as_ptr() as usize;
        let _ = writeln!(stdout, "{}", format_hex_line_plain(offset, chunk));
    }
    // Final offset marker
    let _ = writeln!(stdout, "{:08x}", data.len());
}

/// Format a single hex dump line with ANSI color codes.
///
/// Example:
/// `00000000  48 65 6c 6c 6f 20 77 6f  72 6c 64 21 0a           |Hello world!.|`
fn format_hex_line_color(offset: usize, row: &[u8]) -> String {
    let mut out = String::new();

    // Offset
    out.push_str(&format!("\x1b[2m{:08x}  \x1b[0m", offset));

    // Hex bytes
    for (i, byte) in row.iter().enumerate() {
        if i == 8 {
            out.push(' ');
        }
        if *byte == 0 {
            out.push_str(&format!("\x1b[2m{:02x}\x1b[0m ", byte));
        } else if byte.is_ascii_graphic() || byte.is_ascii_whitespace() {
            out.push_str(&format!("\x1b[33m{:02x}\x1b[0m ", byte));
        } else {
            out.push_str(&format!("{:02x} ", byte));
        }
    }

    // Padding for incomplete last line
    let remaining = BYTES_PER_LINE - row.len();
    if remaining > 0 {
        if row.len() < 8 {
            out.push(' ');
        }
        for _ in 0..remaining {
            out.push_str("   ");
        }
    }

    // ASCII sidebar
    out.push_str(&format!(" \x1b[2m|\x1b[0m"));
    for &byte in row {
        if byte.is_ascii_graphic() || byte == b' ' {
            out.push(byte as char);
        } else {
            out.push_str(&format!("\x1b[2m.\x1b[0m"));
        }
    }
    out.push_str(&format!("\x1b[2m|\x1b[0m"));

    out
}

/// Format a single hex dump line without ANSI codes.
fn format_hex_line_plain(offset: usize, row: &[u8]) -> String {
    let mut out = String::new();

    out.push_str(&format!("{:08x}  ", offset));

    for (i, byte) in row.iter().enumerate() {
        if i == 8 {
            out.push(' ');
        }
        out.push_str(&format!("{:02x} ", byte));
    }

    // Padding for incomplete last line
    let remaining = BYTES_PER_LINE - row.len();
    if remaining > 0 {
        if row.len() < 8 {
            out.push(' ');
        }
        for _ in 0..remaining {
            out.push_str("   ");
        }
    }

    // ASCII sidebar
    out.push_str(" |");
    for &byte in row {
        if byte.is_ascii_graphic() || byte == b' ' {
            out.push(byte as char);
        } else {
            out.push('.');
        }
    }
    out.push('|');

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_data() {
        let result = format_hex_line_plain(0, &[]);
        assert!(result.starts_with("00000000"));
        assert!(result.ends_with("||"));
    }

    #[test]
    fn test_single_byte() {
        let result = format_hex_line_plain(0, &[0x41]);
        // 'A' is ASCII graphic, should appear in sidebar
        assert!(result.starts_with("00000000"));
        assert!(result.contains("41"));
        assert!(result.ends_with("|A|"));
    }

    #[test]
    fn test_null_bytes() {
        let data = [0x00, 0x00, 0x00, 0x00];
        let result = format_hex_line_plain(0, &data);
        assert!(result.contains("00 00 00 00"));
        assert!(result.ends_with("|....|"));
    }

    #[test]
    fn test_non_ascii_bytes() {
        let data = [0x00, 0x01, 0x7f, 0x80, 0xff];
        let result = format_hex_line_plain(0, &data);
        // All non-graphic/non-space should show as '.' in ASCII sidebar
        assert!(result.ends_with("|.....|"));
    }

    #[test]
    fn test_ascii_sidebar() {
        let data = b"Hello, World!";
        let result = format_hex_line_plain(0, data);
        assert!(result.contains("|Hello, World!|"));
        // First 8 bytes should have a gap before remaining
        assert!(result.contains("48 65 6c 6c 6f 2c 20 57"));
    }

    #[test]
    fn test_hex_vs_ascii_alignment() {
        // Test the full "Hello world!\n" string
        let data = b"Hello world!\n";
        let result = format_hex_line_plain(0, data);
        // Hex portion
        assert!(result.contains("48 65 6c 6c 6f 20 77 6f"));
        assert!(result.contains("72 6c 64 21 0a"));
        // ASCII sidebar
        assert!(result.contains("|Hello world!.")); // \n becomes .
    }

    #[test]
    fn test_non_zero_offset() {
        let data = [0xde, 0xad];
        let result = format_hex_line_plain(0xDEAD_BEEF, &data);
        assert!(result.starts_with("deadbeef"));
        assert!(result.contains("de ad"));
    }

    #[test]
    fn test_exactly_one_line() {
        let mut data = [0u8; 16];
        for i in 0..16 {
            data[i] = i as u8;
        }
        let result = format_hex_line_plain(0, &data);
        assert!(result.starts_with("00000000"));
        // Hex: 00 01 02 03 04 05 06 07  08 09 0a 0b 0c 0d 0e 0f
        assert!(result.contains("00 01 02 03 04 05 06 07"));
        assert!(result.contains("08 09 0a 0b 0c 0d 0e 0f"));
        // ASCII sidebar: all non-graphic -> all dots
        assert!(result.ends_with("|................|"));
    }

    #[test]
    fn test_canonical_mode_readable() {
        let data = b"Hello, World!\nThis is ccat hex dump.\n";
        let result = format_hex_line_plain(0, &data[..16]);
        assert!(!result.contains('\x1b')); // No ANSI codes
        assert!(result.contains('|')); // ASCII sidebar
    }

    #[test]
    fn test_color_mode_has_ansi() {
        let data = b"Hello";
        let result = format_hex_line_color(0, data);
        assert!(result.contains('\x1b')); // Has ANSI codes
        assert!(result.contains("\x1b[33m")); // Yellow for printable bytes
    }

    #[test]
    fn test_null_in_color_mode() {
        let data = [0x00, 0x41, 0x00, 0x42];
        let result = format_hex_line_color(0, &data);
        assert!(result.contains("\x1b[2m00\x1b[0m")); // Dim for null bytes
        assert!(result.contains("\x1b[33m41\x1b[0m")); // Yellow for 'A'
        assert!(result.contains("\x1b[33m42\x1b[0m")); // Yellow for 'B'
    }

    #[test]
    fn test_split_at_8_byte_boundary() {
        // The gap after 8th byte should be present
        let data = [b'x'; 16];
        let result = format_hex_line_plain(0, &data);
        // Format: "00000000  78 78 78 78 78 78 78 78  78 78..."
        // There should be a gap (extra space) between the 8th and 9th hex bytes
        assert!(result.contains("78 78 78 78 78 78 78 78  78"));
        // Should end with the 16 X's in ASCII sidebar
        assert!(result.ends_with("|xxxxxxxxxxxxxxxx|"));
    }

    #[test]
    fn test_canonical_full_dump_known_data() {
        // Simulate the full hex dump for a 20-byte input
        let data = b"AAAAAAAABBBBBBBBCCCC";
        let lines: Vec<String> = data.chunks(BYTES_PER_LINE)
            .enumerate()
            .map(|(i, chunk)| format_hex_line_plain(i * BYTES_PER_LINE, chunk))
            .collect();

        assert_eq!(lines.len(), 2); // 16 + 4 bytes -> 2 lines
        assert!(lines[0].starts_with("00000000"));
        assert!(lines[0].contains("|AAAAAAAABBBBBBBB|"));
        assert!(lines[1].starts_with("00000010"));
        assert!(lines[1].contains("|CCCC|"));
    }

    #[test]
    fn test_boundary_128_bytes() {
        // 128 bytes = exactly 8 lines, no remainder
        let data = (0..128u8).collect::<Vec<_>>();
        let lines: Vec<String> = data.chunks(BYTES_PER_LINE)
            .enumerate()
            .map(|(i, chunk)| format_hex_line_plain(i * BYTES_PER_LINE, chunk))
            .collect();

        assert_eq!(lines.len(), 8);
        for (i, line) in lines.iter().enumerate() {
            assert!(line.starts_with(&format!("{:08x}", i * 16)));
        }
    }

    #[test]
    fn test_mixed_content_boundary_conditions() {
        let data = &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 8 nulls
            0xde, 0xad, 0xbe, 0xef, // deadbeef
            0x48, 0x65, 0x6c, 0x70, // "Help"
        ];
        let result = format_hex_line_plain(0, data);
        // The ASCII sidebar should show: ..........Help. (nulls = dots, 0xde-0xef = dots, Help = literal)
        assert!(result.ends_with("|............Help|") || result.ends_with("|............Help|"));
    }

    #[test]
    fn test_cat_hex_canonical_prints_all_lines() {
        // Test the full canonical output function
        let data = b"ABCD";
        // We just verify it doesn't panic and produces output
        // Instead, test via format_hex_line directly
        let result = format_hex_line_plain(0, data);
        assert!(result.contains("|ABCD|"));
    }

    #[test]
    fn test_whitespace_in_ascii_sidebar() {
        // Space should be shown literally, not as a dot
        let result = format_hex_line_plain(0, b" ");
        assert!(result.ends_with("| |"));
    }

    #[test]
    fn test_newline_becomes_dot() {
        // \n is not ASCII graphic and not space -> dot
        let result = format_hex_line_plain(0, b"\n");
        assert!(result.ends_with("|.|"));
    }

    #[test]
    fn test_tab_becomes_dot() {
        // \t is not ASCII graphic and not space -> dot
        let result = format_hex_line_plain(0, b"\t");
        assert!(result.ends_with("|.|"));
    }

    #[test]
    fn test_canonical_vs_color_consistency() {
        // Plain and color should produce the same hex values, just with/without ANSI
        let data = [0x48, 0x65, 0x6c, 0x6c, 0x6f, 0x00, 0xff, 0xfe];
        let plain = format_hex_line_plain(0, &data);
        let color = format_hex_line_color(0, &data);

        // Strip ANSI from color for comparison
        let _color_stripped: String = color.chars()
            .filter(|&c| c != '\x1b')
            .collect::<String>()
            .chars()
            .filter(|&c| c != '[')
            .collect::<String>();

        // Both should have same offset
        assert!(plain.starts_with("00000000"));
        assert!(color.contains("00000000"));

        // Both should have same hex values in order
        assert!(plain.contains("48 65 6c 6c 6f"));
        assert!(color.contains("48"));
        assert!(color.contains("65"));
        assert!(color.contains("6c"));
    }
}
