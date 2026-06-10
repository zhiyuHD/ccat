/// ccat --elf: Comprehensive ELF binary introspection.
///
/// Displays ELF header, section headers, program headers, and symbol table
/// in a beautifully formatted terminal view. Zero new dependencies — uses the
/// existing `object` crate.
use object::elf;
use object::read::elf::FileHeader;
use object::Endianness;
use object::Object;
use object::ObjectKind;
use object::ObjectSection;
use object::ObjectSegment;
use object::ObjectSymbol;
use object::SectionKind;
use object::SymbolKind;
use object::SymbolScope;

use crate::pager;

// ── ELF type constants (mirror object::elf) ──────────────────────────────

fn elf_type_str(e_type: u16) -> &'static str {
    match e_type {
        elf::ET_NONE => "ET_NONE (No file type)",
        elf::ET_REL => "ET_REL (Relocatable object file)",
        elf::ET_EXEC => "ET_EXEC (Executable)",
        elf::ET_DYN => "ET_DYN (Shared object)",
        elf::ET_CORE => "ET_CORE (Core file)",
        _ => "Unknown",
    }
}

fn osabi_str(osabi: u8) -> &'static str {
    match osabi {
        elf::ELFOSABI_SYSV => "UNIX - System V",
        elf::ELFOSABI_HPUX => "HP-UX",
        elf::ELFOSABI_NETBSD => "NetBSD",
        elf::ELFOSABI_GNU => "UNIX - GNU/Linux",
        elf::ELFOSABI_HURD => "GNU/Hurd",
        elf::ELFOSABI_SOLARIS => "Solaris",
        elf::ELFOSABI_AIX => "AIX",
        elf::ELFOSABI_IRIX => "IRIX",
        elf::ELFOSABI_FREEBSD => "FreeBSD",
        elf::ELFOSABI_TRU64 => "TRU64 UNIX",
        elf::ELFOSABI_MODESTO => "Modesto",
        elf::ELFOSABI_OPENBSD => "OpenBSD",
        elf::ELFOSABI_OPENVMS => "OpenVMS",
        elf::ELFOSABI_NSK => "HP NonStop Kernel",
        elf::ELFOSABI_AROS => "AROS",
        elf::ELFOSABI_FENIXOS => "FenixOS",
        elf::ELFOSABI_CLOUDABI => "CloudABI",
        elf::ELFOSABI_ARM_AEABI => "ARM EABI",
        elf::ELFOSABI_ARM => "ARM",
        elf::ELFOSABI_STANDALONE => "Standalone (embedded)",
        _ => "Unknown",
    }
}

fn arch_str(arch: object::Architecture) -> &'static str {
    use object::Architecture::*;
    match arch {
        Unknown => "Unknown",
        Aarch64 => "AArch64 (ARM64)",
        Aarch64_Ilp32 => "AArch64 ILP32",
        Arm => "ARM",
        Avr => "AVR",
        Bpf => "BPF",
        Csky => "CSKY",
        I386 => "Intel 80386 (x86)",
        X86_64 => "x86-64",
        X86_64_X32 => "x86-64 X32 ABI",
        Hexagon => "Hexagon",
        LoongArch64 => "LoongArch64",
        Mips => "MIPS",
        Mips64 => "MIPS64",
        Msp430 => "MSP430",
        PowerPc => "PowerPC",
        PowerPc64 => "PowerPC64",
        Riscv32 => "RISC-V (32-bit)",
        Riscv64 => "RISC-V (64-bit)",
        S390x => "IBM S/390x",
        Sharc => "SHARC",
        Sparc => "SPARC",
        Sparc64 => "SPARC v9 (64-bit)",
        Wasm32 => "WebAssembly (32-bit)",
        Wasm64 => "WebAssembly (64-bit)",
        _ => "Other",
    }
}

fn section_type_str(kind: SectionKind) -> &'static str {
    use object::SectionKind::*;
    match kind {
        Unknown => "NULL",
        Text => "SHT_PROGBITS (text)",
        Data => "SHT_PROGBITS (data)",
        ReadOnlyData => "SHT_PROGBITS (rodata)",
        ReadOnlyDataWithRel => "SHT_PROGBITS (rodata with relocations)",
        ReadOnlyString => "SHT_STRTAB / SHT_PROGBITS (strings)",
        UninitializedData => "SHT_NOBITS (bss)",
        Tls => "SHT_PROGBITS (TLS data)",
        UninitializedTls => "SHT_NOBITS (TLS bss)",
        TlsVariables => "SHT_TLS_VAR",
        Common => "COMMON",
        Note => "SHT_NOTE",
        Linker => "Linker metadata",
        OtherString => "SHT_STRTAB (other strings)",
        Debug => "SHT_PROGBITS (debug)",
        DebugString => "SHT_STRTAB (debug strings)",
        Metadata => "Metadata",
        Other => "Other",
        Elf(_) => "ELF-specific",
        _ => "Unknown",
    }
}

fn symbol_kind_str(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Unknown => "NOTYPE",
        SymbolKind::Text => "FUNC",
        SymbolKind::Data => "OBJECT",
        SymbolKind::Section => "SECTION",
        SymbolKind::File => "FILE",
        SymbolKind::Label => "LABEL",
        SymbolKind::Tls => "TLS",
        _ => "UNKN",
    }
}

fn symbol_bind_str(st_info: u8) -> &'static str {
    let bind = st_info >> 4;
    match bind {
        0 => "LOCAL",
        1 => "GLOBAL",
        2 => "WEAK",
        10 => "GNU_UNIQUE",
        _ => "OTHER",
    }
}

fn symbol_vis_str(st_other: u8) -> &'static str {
    let vis = st_other & 3;
    match vis {
        0 => "DEFAULT",
        1 => "INTERNAL",
        2 => "HIDDEN",
        3 => "PROTECTED",
        _ => "UNKN",
    }
}

fn sh_flags_str(flags: u64) -> String {
    let mut s = String::with_capacity(16);
    if flags & elf::SHF_WRITE as u64 != 0 { s.push('W'); } else { s.push('-'); }
    if flags & elf::SHF_ALLOC as u64 != 0 { s.push('A'); } else { s.push('-'); }
    if flags & elf::SHF_EXECINSTR as u64 != 0 { s.push('X'); } else { s.push('-'); }
    if flags & elf::SHF_MERGE as u64 != 0 { s.push('M'); } else { s.push('-'); }
    if flags & elf::SHF_STRINGS as u64 != 0 { s.push('S'); } else { s.push('-'); }
    if flags & elf::SHF_TLS as u64 != 0 { s.push('T'); } else { s.push('-'); }
    if flags & elf::SHF_COMPRESSED as u64 != 0 { s.push('C'); } else { s.push('-'); }
    if flags & elf::SHF_GNU_RETAIN as u64 != 0 { s.push('R'); } else { s.push('-'); }
    if flags & elf::SHF_EXCLUDE as u64 != 0 { s.push('E'); } else { s.push('-'); }
    s
}

fn segment_type_str(p_type: u32) -> &'static str {
    match p_type {
        elf::PT_NULL => "NULL",
        elf::PT_LOAD => "LOAD",
        elf::PT_DYNAMIC => "DYNAMIC",
        elf::PT_INTERP => "INTERP",
        elf::PT_NOTE => "NOTE",
        elf::PT_SHLIB => "SHLIB",
        elf::PT_PHDR => "PHDR",
        elf::PT_TLS => "TLS",
        elf::PT_GNU_EH_FRAME => "GNU_EH_FRAME",
        elf::PT_GNU_STACK => "GNU_STACK",
        elf::PT_GNU_RELRO => "GNU_RELRO",
        elf::PT_GNU_PROPERTY => "GNU_PROPERTY",
        _ => {
            if p_type >= elf::PT_LOOS && p_type <= elf::PT_HIOS {
                "OS-specific"
            } else if p_type >= elf::PT_LOPROC && p_type <= elf::PT_HIPROC {
                "Processor-specific"
            } else {
                "Unknown"
            }
        }
    }
}

fn segment_flags_str(p_flags: u32) -> String {
    let mut s = String::with_capacity(3);
    s.push(if p_flags & elf::PF_R != 0 { 'R' } else { ' ' });
    s.push(if p_flags & elf::PF_W != 0 { 'W' } else { ' ' });
    s.push(if p_flags & elf::PF_X != 0 { 'E' } else { ' ' });
    s
}

/// Format size in human-readable form
fn fmt_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["", "K", "M", "G", "T"];
    if bytes == 0 {
        return "0".into();
    }
    let mut val = bytes as f64;
    let mut unit_idx = 0;
    while val >= 1024.0 && unit_idx < UNITS.len() - 1 {
        val /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} B", bytes)
    } else if val >= 100.0 {
        format!("{:.0}{}", val, UNITS[unit_idx])
    } else if val >= 10.0 {
        format!("{:.1}{}", val, UNITS[unit_idx])
    } else {
        format!("{:.2}{}", val, UNITS[unit_idx])
    }
}

/// Attempt to get the raw ELF e_type, e_machine, and program header type
/// by parsing the ELF header directly from raw data.
struct RawElfHeader {
    e_type: u16,
    e_machine: u16,
    phentries: Vec<RawPhdr>,
}

struct RawPhdr {
    p_type: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_flags: u32,
    p_align: u64,
}

fn parse_raw_elf(data: &[u8]) -> Option<RawElfHeader> {
    // Try 64-bit first
    if let Ok(header) = elf::FileHeader64::<Endianness>::parse(data) {
        let endian = header.endian().ok()?;
        let e_type = header.e_type(endian);
        let e_machine = header.e_machine(endian);
        let entry: u64 = header.e_entry(endian).into();
        let phoff: u64 = header.e_phoff(endian).into();
        let phnum = header.e_phnum(endian) as usize;
        let phentsize = header.e_phentsize(endian) as usize;

        let mut phentries = Vec::with_capacity(phnum);
        for i in 0..phnum {
            let offset = phoff as usize + i * phentsize;
            if offset + std::mem::size_of::<elf::ProgramHeader64<Endianness>>() > data.len() {
                break;
            }
            // Read the program header directly from raw data
            let phdr = data.get(offset..)?;
            let phdr: &elf::ProgramHeader64<Endianness> = object::from_bytes(phdr).ok()?.0;
            phentries.push(RawPhdr {
                p_type: phdr.p_type.get(endian),
                p_offset: phdr.p_offset.get(endian).into(),
                p_vaddr: phdr.p_vaddr.get(endian).into(),
                p_paddr: phdr.p_paddr.get(endian).into(),
                p_filesz: phdr.p_filesz.get(endian).into(),
                p_memsz: phdr.p_memsz.get(endian).into(),
                p_flags: phdr.p_flags.get(endian),
                p_align: phdr.p_align.get(endian).into(),
            });
        }
        return Some(RawElfHeader { e_type, e_machine, phentries });
    }

    // Fallback: 32-bit
    if let Ok(header) = elf::FileHeader32::<Endianness>::parse(data) {
        let endian = header.endian().ok()?;
        let e_type = header.e_type(endian);
        let e_machine = header.e_machine(endian);
        let phoff: u64 = header.e_phoff(endian).into();
        let phnum = header.e_phnum(endian) as usize;
        let phentsize = header.e_phentsize(endian) as usize;

        let mut phentries = Vec::with_capacity(phnum);
        for i in 0..phnum {
            let offset = phoff as usize + i * phentsize;
            if offset + std::mem::size_of::<elf::ProgramHeader32<Endianness>>() > data.len() {
                break;
            }
            let phdr = data.get(offset..)?;
            let phdr: &elf::ProgramHeader32<Endianness> = object::from_bytes(phdr).ok()?.0;
            phentries.push(RawPhdr {
                p_type: phdr.p_type.get(endian),
                p_offset: phdr.p_offset.get(endian).into(),
                p_vaddr: phdr.p_vaddr.get(endian).into(),
                p_paddr: phdr.p_paddr.get(endian).into(),
                p_filesz: phdr.p_filesz.get(endian).into(),
                p_memsz: phdr.p_memsz.get(endian).into(),
                p_flags: phdr.p_flags.get(endian),
                p_align: phdr.p_align.get(endian).into(),
            });
        }
        return Some(RawElfHeader { e_type, e_machine, phentries });
    }

    None
}

// ── Main entry point ────────────────────────────────────────────────────

pub fn cat_elf(data: &[u8]) {
    let file = match object::read::File::parse(data) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ccat: failed to parse ELF: {e}");
            return;
        }
    };

    let raw = parse_raw_elf(data);
    let is_64 = file.is_64();
    let class_name = if is_64 { "ELF64" } else { "ELF32" };
    let endian_str = if file.is_little_endian() { "Little Endian" } else { "Big Endian" };
    let arch = file.architecture();
    let entry = file.entry();
    let kind = file.kind();

    // ── Collect all output lines ───────────────────────────────────────
    let mut lines: Vec<String> = Vec::new();

    // ═══════════ ELF Header ═══════════
    lines.push("\x1b[1;34m┌─ ELF Header ──────────────────────────────────────────────┐\x1b[0m".into());
    lines.push(format!("\x1b[1m  Class:\x1b[0m         {class_name}"));
    lines.push(format!("\x1b[1m  Endian:\x1b[0m        {endian_str}"));
    if let Some(ref raw) = raw {
        lines.push(format!("\x1b[1m  Type:\x1b[0m           {}  \x1b[2m(e_type = 0x{:04x})\x1b[0m",
            elf_type_str(raw.e_type), raw.e_type));
        let e_machine = raw.e_machine;
        lines.push(format!("\x1b[1m  Machine:\x1b[0m        {}  \x1b[2m(EM = 0x{:04x})\x1b[0m",
            arch_str(arch), e_machine));
    } else {
        lines.push(format!("\x1b[1m  Type:\x1b[0m           {}  \x1b[2m(derived from ObjectKind)\x1b[0m",
            match kind {
                ObjectKind::Relocatable => "ET_REL (Relocatable object file)",
                ObjectKind::Executable => "ET_EXEC (Executable)",
                ObjectKind::Dynamic => "ET_DYN (Shared object)",
                ObjectKind::Core => "ET_CORE (Core file)",
                _ => "Unknown",
            }));
        lines.push(format!("\x1b[1m  Machine:\x1b[0m        {}", arch_str(arch)));
    }

    // OS/ABI from FileFlags
    if let object::FileFlags::Elf { os_abi, abi_version, e_flags } = file.flags() {
        lines.push(format!("\x1b[1m  OS/ABI:\x1b[0m         {}  \x1b[2m(os_abi = 0x{os_abi:02x})\x1b[0m", osabi_str(os_abi)));
        lines.push(format!("\x1b[1m  ABI Version:\x1b[0m    {abi_version}"));
        lines.push(format!("\x1b[1m  Flags:\x1b[0m          0x{e_flags:08x}"));
    }

    lines.push(format!("\x1b[1m  Entry point:\x1b[0m     0x{entry:016x}"));
    lines.push("└──────────────────────────────────────────────────────────────┘".into());
    lines.push(String::new());

    // ═══════════ Program Headers ═══════════
    {
        // Count segments
        let seg_count = file.segments().count();
        if seg_count > 0 {
            lines.push(format!("\x1b[1;34m┌─ Program Headers [{} entries] ─────────────────────────────┐\x1b[0m", seg_count));
            // Header
            lines.push(
                "  \x1b[1mType           Offset     VirtAddr          PhysAddr          FileSiz    MemSiz     Flg  Align\x1b[0m".into()
            );
            lines.push(
                "  \x1b[2m------------------------- -------- -------- -------- -------- -------- --- -----\x1b[0m".into()
            );

            // Use raw program headers for accurate p_type
            if let Some(ref raw) = raw {
                for (_i, phdr) in raw.phentries.iter().enumerate() {
                    let type_str = segment_type_str(phdr.p_type);
                    let flg = segment_flags_str(phdr.p_flags);
                    let align_str = format!("0x{:x}", phdr.p_align);
                    lines.push(format!(
                        "  \x1b[33m{:<14}\x1b[0m 0x{offset:08x} 0x{vaddr:016x} 0x{paddr:016x} 0x{filesz:08x} 0x{memsz:08x} {flg:<3} {align_str}",
                        type_str,
                        offset = phdr.p_offset,
                        vaddr = phdr.p_vaddr,
                        paddr = phdr.p_paddr,
                        filesz = phdr.p_filesz,
                        memsz = phdr.p_memsz,
                    ));
                    // Show annotation for L O A D / I N T E R P / D Y N A M I C
                    if phdr.p_type == elf::PT_LOAD {
                        lines.push(format!("  \x1b[2m    {}{}\x1b[0m",
                            " ".repeat(60),
                            format!("(loads to 0x{:x}, size {})", phdr.p_vaddr, fmt_size(phdr.p_memsz))));
                    } else if phdr.p_type == elf::PT_INTERP {
                        // Try to read the interpreter path
                        let interp_offset = phdr.p_offset as usize;
                        let interp_end = interp_offset + phdr.p_filesz.min(256) as usize;
                        if interp_end <= data.len() {
                            let interp_data = &data[interp_offset..interp_end];
                            let interp_str = String::from_utf8_lossy(interp_data).trim_end_matches('\0').to_string();
                            lines.push(format!("  \x1b[2m    {}interpreter: {}\x1b[0m", " ".repeat(60), interp_str));
                        }
                    } else if phdr.p_type == elf::PT_GNU_STACK {
                        let exec_str = if phdr.p_flags & elf::PF_X != 0 { "executable" } else { "non-executable" };
                        lines.push(format!("  \x1b[2m    {}stack is {exec_str}\x1b[0m", " ".repeat(60)));
                    } else if phdr.p_type == elf::PT_GNU_RELRO {
                        lines.push(format!("  \x1b[2m    {}read-only relocations after relocation\x1b[0m", " ".repeat(60)));
                    }
                }
            } else {
                // Fallback: use high-level API (no type info)
                for (_i, segment) in file.segments().enumerate() {
                    let addr = segment.address();
                    let size = segment.size();
                    let (file_off, file_size) = segment.file_range();
                    let seg_flags = segment.flags();
                    let p_flags = if let object::SegmentFlags::Elf { p_flags } = seg_flags {
                        p_flags
                    } else {
                        0
                    };
                    let flg = segment_flags_str(p_flags);
                    lines.push(format!(
                        "  \x1b[33m{:<14}\x1b[0m 0x{file_off:08x} 0x{addr:016x} 0x{addr:016x} 0x{file_size:08x} 0x{size:08x} {flg:<3}",
                        "SEGMENT",
                    ));
                }
            }
            lines.push("└──────────────────────────────────────────────────────────────┘".into());
            lines.push(String::new());
        }
    }

    // ═══════════ Section Headers ═══════════
    {
        let sections: Vec<_> = file.sections().collect();
        if !sections.is_empty() {
            lines.push(format!("\x1b[1;34m┌─ Section Headers [{} entries] ───────────────────────────────┐\x1b[0m", sections.len()));
            lines.push(
                "  \x1b[1m[Nr] Name                 Type                  Address          Offset    Size      Flags\x1b[0m".into()
            );
            lines.push(
                "  \x1b[2m------------------------------------------ -------- -------- -------- --------\x1b[0m".into()
            );

            for (i, section) in sections.iter().enumerate() {
                let name = section.name().unwrap_or("<?>").to_string();
                let addr = section.address();
                let size = section.size();
                let file_range = section.file_range();
                let (offset, _file_size) = file_range.unwrap_or((0, 0));
                let kind = section.kind();
                let sec_flags = section.flags();
                let sh_flags = if let object::SectionFlags::Elf { sh_flags } = sec_flags {
                    sh_flags
                } else {
                    0
                };
                let flags_str = sh_flags_str(sh_flags);

                let type_str = section_type_str(kind);
                // Truncate name to fit
                let name_display = if name.len() > 22 {
                    format!("{}…", &name[..21])
                } else {
                    format!("{:22}", name)
                };
                lines.push(format!(
                    "  \x1b[2m[{:3}]\x1b[0m {} \x1b[35m{:22}\x1b[0m 0x{addr:08x} 0x{offset:08x} 0x{size:08x} \x1b[33m{flags_str}\x1b[0m",
                    i,
                    name_display,
                    type_str,
                ));
            }
            lines.push("└──────────────────────────────────────────────────────────────┘".into());
            lines.push(String::new());
        }
    }

    // ═══════════ Symbol Table ═══════════
    {
        let symbols: Vec<_> = file.symbols().collect();
        let count = symbols.len();
        // Only show if there are meaningful symbols (skip pure-UNDEF tables)
        let meaningful = symbols.iter()
            .filter(|s| s.address() != 0 || s.size() != 0)
            .count();

        lines.push(format!("\x1b[1;34m┌─ Symbol Table [{} entries, {} non-zero] ─────────────────────┐\x1b[0m", count, meaningful));
        if count == 0 {
            lines.push("  \x1b[2m(no symbols found — stripped binary?)\x1b[0m".into());
        } else {
            lines.push(
                "  \x1b[1mValue              Size     Type   Bind    Vis       Name\x1b[0m".into()
            );
            lines.push(
                "  \x1b[2m-------------------- ------- ---- ------ --------- ------------------\x1b[0m".into()
            );

            for symbol in symbols.iter().take(500) {
                // Truncate at 500 to avoid huge output
                let addr = symbol.address();
                let size = symbol.size();
                let kind = symbol.kind();
                let scope = symbol.scope();
                let name = symbol.name().unwrap_or("<unnamed>").to_string();
                let flags = symbol.flags();

                let (st_info, st_other) = if let object::SymbolFlags::Elf { st_info, st_other } = flags {
                    (st_info, st_other)
                } else {
                    // Derive from SymbolKind and SymbolScope
                    let info = (match scope {
                        SymbolScope::Unknown => 0,
                        SymbolScope::Compilation => 0,
                        SymbolScope::Linkage => 1,
                        SymbolScope::Dynamic => 1,
                    }) << 4;
                    (info, 0)
                };

                let kind_str = symbol_kind_str(kind);
                let bind_str = symbol_bind_str(st_info);
                let vis_str = symbol_vis_str(st_other);

                // Color-code by scope/type
                let addr_color = if addr != 0 { "" } else { "\x1b[2m" };
                let reset = if addr != 0 { "" } else { "\x1b[0m" };
                let name_prefix = match kind {
                    SymbolKind::File => "\x1b[2m",
                    SymbolKind::Text => "\x1b[32m",
                    SymbolKind::Data => "\x1b[33m",
                    _ => "",
                };

                lines.push(format!(
                    "  {addr_color}0x{addr:016x}{reset} 0x{size:08x} {kind_str:<4} {bind_str:<6} {vis_str:<9} {name_prefix}{name}\x1b[0m",
                    addr_color = addr_color,
                    reset = reset,
                    addr = addr,
                    size = size,
                    kind_str = kind_str,
                    bind_str = bind_str,
                    vis_str = vis_str,
                    name_prefix = name_prefix,
                    name = name,
                ));
            }

            if symbols.len() > 500 {
                lines.push(format!("  \x1b[2m... and {} more symbols (truncated to 500)\x1b[0m", symbols.len() - 500));
            }
        }
        lines.push("└──────────────────────────────────────────────────────────────┘".into());
    }

    // ── Output with pager ──────────────────────────────────────────────
    // Remove ANSI escape sequences for pager line counting
    pager::run_pager(&lines);
}
