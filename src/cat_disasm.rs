/// Disassemble x86/x86_64 ELF binary .text section.
use std::io::Write;

use iced_x86::Formatter;
use object::Object;
use object::ObjectSection;

use crate::pager;

pub fn disassemble_elf(data: &[u8]) {
    match object::File::parse(data) {
        Ok(file) => {
            let arch = file.architecture();
            match arch {
                object::Architecture::I386 | object::Architecture::X86_64 => {
                    disasm_x86(file, data);
                }
                _ => {
                    eprintln!("ccat: disassembly not supported for {:?}, showing hex dump", arch);
                    crate::cat_hex(data);
                }
            }
        }
        Err(e) => {
            eprintln!("ccat: failed to parse ELF: {e}");
        }
    }
}

fn disasm_x86(file: object::File<'_>, _data: &[u8]) {
    let is_64 = matches!(file.architecture(), object::Architecture::X86_64);
    let bits: u32 = if is_64 { 64 } else { 32 };

    let text_section = match file.section_by_name(".text") {
        Some(s) => s,
        None => {
            eprintln!("ccat: .text section not found");
            return;
        }
    };

    let text_data = match text_section.data() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ccat: failed to read .text section: {e}");
            return;
        }
    };

    if text_data.is_empty() {
        eprintln!("ccat: .text section is empty");
        return;
    }

    let file_addr = text_section.address();
    let end_addr = file_addr + text_data.len() as u64;

    let mut decoder = iced_x86::Decoder::with_ip(
        bits,
        text_data,
        file_addr,
        iced_x86::DecoderOptions::NONE,
    );
    let mut formatter = iced_x86::NasmFormatter::new();
    formatter.options_mut().set_uppercase_prefixes(false);
    formatter.options_mut().set_uppercase_keywords(false);
    let mut instruction = iced_x86::Instruction::default();

    let mut stdout = std::io::stdout();
    let mut total: Vec<String> = Vec::new();
    let mut pos: usize = 0;
    let total_size = text_data.len();

    // Estimate total instructions for progress (rough: avg 4 bytes per instr)
    let estimated = (total_size / 4).max(1);
    let progress_interval = (estimated / 100).max(1);
    let mut next_progress = progress_interval;

    // Show initial progress
    let mut stdout_progress = std::io::stdout();
    let _ = write!(&mut stdout_progress, "\x1b[2mDecoding (0/{total_size} bytes)...\x1b[0m");
    let _ = stdout_progress.flush();

    while decoder.can_decode() {
        decoder.decode_out(&mut instruction);
        let ip = instruction.ip();
        let instr_len = instruction.len() as usize;

        let mut buf = String::new();
        formatter.format(&instruction, &mut buf);

        let bytes_slice = &text_data[pos..pos + instr_len.min(text_data.len() - pos)];
        let bytes_str: String = bytes_slice
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        pos += instr_len;

        // Progress indicator
        if pos >= next_progress || !decoder.can_decode() {
            let pct = (pos as f64 / total_size as f64 * 100.0).min(100.0) as u32;
            let _ = write!(
                &mut stdout_progress,
                "\r\x1b[2mDecoding ({pos}/{total_size} bytes, {pct}%)...\x1b[0m"
            );
            let _ = stdout_progress.flush();
            next_progress = pos.saturating_add(progress_interval * (pos / progress_interval + 1).max(1) * 4);
        }

        let line = format!(
            "\x1b[2m{:08x}\x1b[0m  \x1b[33m{:<20}\x1b[0m {}",
            ip, bytes_str, buf
        );
        total.push(line);
    }

    // Clear progress line
    let _ = write!(&mut stdout_progress, "\r\x1b[K");
    let _ = stdout_progress.flush();

    if total.is_empty() {
        writeln!(&mut stdout, "ccat: no instructions found in .text").ok();
        return;
    }

    let (term_height, _) = pager::terminal_size();
    // Reserve: 1 header + 1 footer line + 1 prompt line
    let page_size = term_height.saturating_sub(3).max(5);
    let total_pages = total.len().div_ceil(page_size);
    let mut current_page: usize = 0;

    loop {
        let start = current_page * page_size;
        let end = (start + page_size).min(total.len());

        let _ = writeln!(
            stdout,
            "\x1b[1m.text ({} instructions, {} bytes at 0x{:x})\x1b[0m",
            total.len(),
            text_data.len(),
            file_addr
        );

        for line in &total[start..end] {
            let _ = writeln!(stdout, "{}", line);
        }

        let _ = writeln!(stdout, "\x1b[2m{:08x}  \x1b[0m", end_addr);

        if total_pages > 1 {
            let action = pager::page_footer(
                &mut stdout, current_page, total_pages,
                start, end, total.len(),
            );
            match action {
                pager::PageAction::Quit => break,
                pager::PageAction::Next(_) => {
                    if current_page + 1 < total_pages { current_page += 1; }
                }
                pager::PageAction::Prev(_) => {
                    if current_page > 0 { current_page -= 1; }
                }
                _ => {}
            }
        } else {
            break;
        }
    }
}
