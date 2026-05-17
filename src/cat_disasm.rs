/// Disassemble x86/x86_64 ELF binary .text section.
use std::io::{Read, Write};

use iced_x86::Formatter;
use object::Object;
use object::ObjectSection;

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
                    super::cat_hex(data);
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

    // Find .text section
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

    // Use iced-x86 disassembler
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
    let mut count: usize = 0;
    let page_size = 30; // instructions per page
    let mut total: Vec<String> = Vec::new();

    // Reconstruct bytes for each instruction by slicing from text_data
    // We need to track the position manually
    let mut pos: usize = 0;

    while decoder.can_decode() {
        decoder.decode_out(&mut instruction);
        let ip = instruction.ip();
        let instr_len = instruction.len() as usize;

        let mut buf = String::new();
        formatter.format(&instruction, &mut buf);

        // Get instruction bytes from the original text_data
        let bytes_slice = &text_data[pos..pos + instr_len.min(text_data.len() - pos)];
        let bytes_str: String = bytes_slice
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        pos += instr_len;

        let line = format!(
            "\x1b[2m{:08x}\x1b[0m  \x1b[33m{:<20}\x1b[0m {}",
            ip, bytes_str, buf
        );
        total.push(line);
        count += 1;
    }

    if total.is_empty() {
        writeln!(&mut stdout, "ccat: no instructions found in .text").ok();
        return;
    }

    // Interactive paged output
    let total_pages = total.len().div_ceil(page_size);
    let mut current_page: usize = 0;

    loop {
        let start = current_page * page_size;
        let end = (start + page_size).min(total.len());

        // Header
        let _ = writeln!(
            stdout,
            "\x1b[1m.text ({} instructions, {} bytes at 0x{:x})\x1b[0m",
            count,
            text_data.len(),
            file_addr
        );

        for line in &total[start..end] {
            let _ = writeln!(stdout, "{}", line);
        }

        let _ = writeln!(
            stdout,
            "\x1b[2m{:08x}  \x1b[0m",
            end_addr
        );

        if total_pages > 1 {
            let _ = write!(
                stdout,
                "\x1b[2m-- Page {}/{} ({}-{} / {}) -- [n]ext [p]rev [q]uit  \x1b[0m",
                current_page + 1,
                total_pages,
                start,
                end,
                total.len()
            );
            let _ = stdout.flush();

            let mut buf = [0u8; 1];
            let _ = std::process::Command::new("sh")
                .args(["-c", "stty raw -echo < /dev/tty 2>/dev/null"])
                .status();
            let _ = std::io::stdin().read_exact(&mut buf);
            let _ = std::process::Command::new("sh")
                .args(["-c", "stty sane < /dev/tty 2>/dev/null"])
                .status();

            match buf[0] {
                b'q' | 0x03 | 0x1b => break,
                b'n' | b' ' => {
                    if current_page + 1 < total_pages {
                        current_page += 1;
                    }
                }
                b'p' | b'b' => {
                    if current_page > 0 {
                        current_page -= 1;
                    }
                }
                _ => {}
            }
        } else {
            break;
        }
    }
}
