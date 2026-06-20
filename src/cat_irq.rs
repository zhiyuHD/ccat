//! IRQ and SoftIRQ Analyzer (`ccat --interrupts`).
//!
//! Reads `/proc/interrupts` and `/proc/softirqs` and presents per-CPU
//! interrupt distribution, sorted by rate, with balance indicators.
//!
//! Usage:
//!   ccat --interrupts              # show IRQ + SoftIRQ tables
//!   ccat --interrupts -w 2         # live refresh
//!
//! No root required — /proc/interrupts and /proc/softirqs are world-readable.

use std::fs;
use std::io::{self, Write};

// ── Colour helpers (matching style from other modules) ──

mod style {
    pub fn bold(s: impl AsRef<str>) -> String   { format!("\x1b[1m{}\x1b[0m", s.as_ref()) }
    pub fn dim(s: impl AsRef<str>) -> String    { format!("\x1b[2m{}\x1b[0m", s.as_ref()) }
    pub fn green(s: impl AsRef<str>) -> String  { format!("\x1b[32m{}\x1b[0m", s.as_ref()) }
    pub fn red(s: impl AsRef<str>) -> String    { format!("\x1b[31m{}\x1b[0m", s.as_ref()) }
    pub fn yellow(s: impl AsRef<str>) -> String { format!("\x1b[33m{}\x1b[0m", s.as_ref()) }
    pub fn grey(s: impl AsRef<str>) -> String   { format!("\x1b[90m{}\x1b[0m", s.as_ref()) }
}

// ── Data types ──

#[derive(Debug)]
struct IrqLine {
    irq: String,         // "0", "42", "LOC", "NMI", etc.
    counts: Vec<u64>,    // per-CPU counts
    kind: IrqKind,
    desc: String,
}

#[derive(Debug, PartialEq)]
enum IrqKind {
    Device,       // Actual hardware device IRQ (sourced from IR-IO-APIC, PCI-MSI, etc.)
    Special,      // NMI, LOC, RES, CAL, TLB, TRM, etc.
    ErrMis,       // ERR, MIS (error/misc counters)
}

#[derive(Debug)]
struct SoftIrqLine {
    kind: String,
    counts: Vec<u64>,
}

// ── Rounding / human-friendly formatting ──

fn fmt_count(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}G", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

/// Format as a right-aligned integer with comma separators.
fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.insert(0, ',');
        }
        out.insert(0, c);
    }
    format!("{:>10}", out)
}

// ── Parsing /proc/interrupts ──

/// Parse /proc/interrupts into structured lines.
fn parse_interrupts() -> (Vec<String>, Vec<IrqLine>) {
    // Returns (headers, lines) where headers[0] = "CPU0", etc.
    let content = match fs::read_to_string("/proc/interrupts") {
        Ok(c) => c,
        Err(_) => return (Vec::new(), Vec::new()),
    };

    let mut lines = content.lines();
    let header_line = match lines.next() {
        Some(l) => l,
        None => return (Vec::new(), Vec::new()),
    };

    let headers: Vec<String> = header_line
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let num_cpus = headers.len();

    let mut irqs = Vec::new();

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }

        // Split on first colon to get IRQ number
        let colon_pos = match line.find(':') {
            Some(p) => p,
            None => continue,
        };

        let irq_num = line[..colon_pos].trim().to_string();
        let rest = line[colon_pos + 1..].trim();

        // Parse per-CPU counts: space-separated numbers until we hit the first non-number
        let parts = rest.split_whitespace();
        let mut counts = Vec::new();
        let mut desc_parts = Vec::new();

        let mut seen_desc = false;
        for part in parts {
            if !seen_desc {
                if let Ok(n) = part.parse::<u64>() {
                    counts.push(n);
                    if counts.len() >= num_cpus {
                        seen_desc = true;
                    }
                } else {
                    seen_desc = true;
                    desc_parts.push(part);
                }
            } else {
                desc_parts.push(part);
            }
        }

        // Pad counts if short (e.g. ERR/MIS lines have no per-CPU counts)
        while counts.len() < num_cpus {
            counts.push(0);
        }

        let desc = desc_parts.join(" ");

        // Classify the IRQ
        let kind = if irq_num == "ERR" || irq_num == "MIS" {
            IrqKind::ErrMis
        } else if irq_num.parse::<u64>().is_ok() {
            IrqKind::Device
        } else {
            IrqKind::Special
        };

        irqs.push(IrqLine {
            irq: irq_num,
            counts,
            kind,
            desc,
        });
    }

    (headers, irqs)
}

// ── Parsing /proc/softirqs ──

fn parse_softirqs() -> (Vec<String>, Vec<SoftIrqLine>) {
    let content = match fs::read_to_string("/proc/softirqs") {
        Ok(c) => c,
        Err(_) => return (Vec::new(), Vec::new()),
    };

    let mut lines = content.lines();
    let header_line = match lines.next() {
        Some(l) => l,
        None => return (Vec::new(), Vec::new()),
    };

    let headers: Vec<String> = header_line
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let num_cpus = headers.len();

    let mut softirqs = Vec::new();

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }

        let colon_pos = match line.find(':') {
            Some(p) => p,
            None => continue,
        };

        let name = line[..colon_pos].trim().to_string();
        let rest = line[colon_pos + 1..].trim();

        let mut counts = Vec::new();
        for part in rest.split_whitespace() {
            if let Ok(n) = part.parse::<u64>() {
                counts.push(n);
                if counts.len() >= num_cpus {
                    break;
                }
            }
        }

        while counts.len() < num_cpus {
            counts.push(0);
        }

        softirqs.push(SoftIrqLine {
            kind: name,
            counts,
        });
    }

    (headers, softirqs)
}

// ── Balance indicator ──

/// Returns a simple balance assessment string.
/// "even" if all CPUs have roughly equal counts (ratio max/min < 2),
/// "skewed" if some CPUs handle most of the load.
fn balance_str(counts: &[u64]) -> String {
    let non_zero: Vec<u64> = counts.iter().copied().filter(|&c| c > 0).collect();
    if non_zero.is_empty() {
        return style::grey("idle").to_string();
    }
    let active_cpus = non_zero.len();
    let total_cpus = counts.len();
    if active_cpus < total_cpus {
        return style::yellow(&format!("pinned({}/{})", active_cpus, total_cpus)).to_string();
    }
    let min = *non_zero.iter().min().unwrap_or(&1);
    let max = *non_zero.iter().max().unwrap_or(&1);
    if min == 0 || max == 0 {
        return style::yellow("skewed").to_string();
    }
    let ratio = max as f64 / min as f64;
    if ratio < 1.5 {
        style::green("even").to_string()
    } else if ratio < 3.0 {
        style::yellow("mixed").to_string()
    } else {
        style::red("skewed").to_string()
    }
}

/// Sparkline-like bar showing relative distribution across CPUs.
fn balance_bar(counts: &[u64], width: usize) -> String {
    let max = *counts.iter().max().unwrap_or(&0);
    if max == 0 {
        return " ".repeat(width);
    }
    let mut bar = String::new();
    for &c in counts {
        let blocks = if c == 0 { 0 } else { ((c as f64 / max as f64) * width as f64).round() as usize };
        let block = if blocks >= width { "█".repeat(width) } else { "█".repeat(blocks.max(1)) };
        bar.push_str(&block);
    }
    bar
}

// ── Main entry point ──

/// Main entry point: `ccat --interrupts`
pub fn cat_interrupts() {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // ── Parse data ──
    let (cpu_headers, irqs) = parse_interrupts();
    let (_, softirqs) = parse_softirqs();

    if cpu_headers.is_empty() {
        let _ = writeln!(out, "{}", style::red("Cannot read /proc/interrupts (not a Linux system?)"));
        return;
    }

    let num_cpus = cpu_headers.len();

    // ── Header ──
    let _ = writeln!(
        out,
        "{}   CPUs: {}",
        style::bold("═══ Interrupt Analysis ═══"),
        style::yellow(&num_cpus.to_string()),
    );
    let _ = writeln!(out);

    // ── Device IRQs (hardware interrupts) ──
    let devices: Vec<&IrqLine> = irqs.iter().filter(|i| i.kind == IrqKind::Device).collect();

    if !devices.is_empty() {
        let _ = writeln!(out, "{}", style::bold("── Device IRQs ──"));
        let _ = writeln!(
            out,
            "{} {:>7}  {:>10}  {:>10} {:>10} {:>10}  {:>8}  {}  {}",
            style::dim("IRQ"),
            style::dim("TOTAL"),
            style::dim(&cpu_headers[0]),
            style::dim(if num_cpus >= 2 { &cpu_headers[1] } else { &cpu_headers[0] }),
            style::dim(if num_cpus >= 3 { &cpu_headers[2] } else { "" }),
            style::dim(if num_cpus >= 4 { &cpu_headers[3] } else { "" }),
            style::dim("BALANCE"),
            style::dim("CHIP"),
            style::dim("DEVICE"),
        );

        for irq in &devices {
            let total: u64 = irq.counts.iter().sum();
            let balance = balance_str(&irq.counts);

            // Extract chip type from description
            let chip = irq.desc.split_whitespace().next().unwrap_or("?");
            let device = irq.desc.split_whitespace().skip(4).collect::<Vec<&str>>().join(" ");
            let device_short = if device.len() > 28 {
                format!("{}…", &device[..27])
            } else {
                device
            };

            let _ = writeln!(
                out,
                "  {:>4}  {}  {} {} {} {}  {:>8}  {:>12}  {}",
                style::grey(&irq.irq),
                fmt_int(total),
                fmt_int(irq.counts.get(0).copied().unwrap_or(0)),
                fmt_int(irq.counts.get(1).copied().unwrap_or(0)),
                if num_cpus >= 3 { fmt_int(irq.counts.get(2).copied().unwrap_or(0)) } else { "          ".to_string() },
                if num_cpus >= 4 { fmt_int(irq.counts.get(3).copied().unwrap_or(0)) } else { "          ".to_string() },
                balance,
                style::dim(&chip),
                device_short,
            );
        }
        let _ = writeln!(out);
    }

    // ── Special IRQs (NMI, LOC, RES, CAL, TLB, etc.) ──
    let specials: Vec<&IrqLine> = irqs.iter().filter(|i| i.kind == IrqKind::Special).collect();

    if !specials.is_empty() {
        let _ = writeln!(out, "{}", style::bold("── System IRQs ──"));
        let _ = writeln!(
            out,
            "{} {:>7}  {:>10} {:>10} {:>10} {:>10}  {:>8}  {}",
            style::dim("IRQ"),
            style::dim("TOTAL"),
            style::dim(&cpu_headers[0]),
            style::dim(if num_cpus >= 2 { &cpu_headers[1] } else { &cpu_headers[0] }),
            style::dim(if num_cpus >= 3 { &cpu_headers[2] } else { "" }),
            style::dim(if num_cpus >= 4 { &cpu_headers[3] } else { "" }),
            style::dim("BALANCE"),
            style::dim("DESCRIPTION"),
        );

        for irq in &specials {
            let total: u64 = irq.counts.iter().sum();
            let balance = balance_str(&irq.counts);

            let desc_short = if irq.desc.len() > 32 {
                format!("{}…", &irq.desc[..31])
            } else {
                irq.desc.clone()
            };

            let _ = writeln!(
                out,
                "  {:>4}  {}  {} {} {} {}  {:>8}  {}",
                style::grey(&irq.irq),
                fmt_int(total),
                fmt_int(irq.counts.get(0).copied().unwrap_or(0)),
                fmt_int(irq.counts.get(1).copied().unwrap_or(0)),
                if num_cpus >= 3 { fmt_int(irq.counts.get(2).copied().unwrap_or(0)) } else { "          ".to_string() },
                if num_cpus >= 4 { fmt_int(irq.counts.get(3).copied().unwrap_or(0)) } else { "          ".to_string() },
                balance,
                style::dim(&desc_short),
            );
        }
        let _ = writeln!(out);
    }

    // ── ERR/MIS ──
    let err_mis: Vec<&IrqLine> = irqs.iter().filter(|i| i.kind == IrqKind::ErrMis).collect();
    if !err_mis.is_empty() {
        let _ = writeln!(out, "{}", style::bold("── Error / Misc Counters ──"));
        for irq in &err_mis {
            let total: u64 = irq.counts.iter().sum();
            let color = if total > 0 { style::red } else { style::green };
            let _ = writeln!(
                out,
                "  {}: {}  {}",
                style::grey(&irq.irq),
                color(&total.to_string()),
                irq.desc,
            );
        }
        let _ = writeln!(out);
    }

    // ── SoftIRQs ──
    if !softirqs.is_empty() {
        let _ = writeln!(out, "{}", style::bold("── SoftIRQs ──"));
        let _ = writeln!(
            out,
            "{} {:>10}  {:>10} {:>10} {:>10} {:>10}  {:>8}  {:width$}",
            style::dim("TYPE"),
            style::dim("TOTAL"),
            style::dim(&cpu_headers[0]),
            style::dim(if num_cpus >= 2 { &cpu_headers[1] } else { &cpu_headers[0] }),
            style::dim(if num_cpus >= 3 { &cpu_headers[2] } else { "" }),
            style::dim(if num_cpus >= 4 { &cpu_headers[3] } else { "" }),
            style::dim("BALANCE"),
            style::dim("DISTRIBUTION"),
            width = 12usize,
        );

        // Sort softirqs by total descending
        let mut sorted: Vec<&SoftIrqLine> = softirqs.iter().collect();
        sorted.sort_by(|a, b| {
            let a_total: u64 = a.counts.iter().sum();
            let b_total: u64 = b.counts.iter().sum();
            b_total.cmp(&a_total)
        });

        for si in &sorted {
            let total: u64 = si.counts.iter().sum();
            if total == 0 {
                continue;
            }
            let balance = balance_str(&si.counts);
            let bar = balance_bar(&si.counts, 12);

            let _ = writeln!(
                out,
                "  {:>6}  {}  {} {} {} {}  {:>8}  {}",
                style::bold(&si.kind),
                fmt_int(total),
                fmt_int(si.counts.get(0).copied().unwrap_or(0)),
                fmt_int(si.counts.get(1).copied().unwrap_or(0)),
                if num_cpus >= 3 { fmt_int(si.counts.get(2).copied().unwrap_or(0)) } else { "          ".to_string() },
                if num_cpus >= 4 { fmt_int(si.counts.get(3).copied().unwrap_or(0)) } else { "          ".to_string() },
                balance,
                style::dim(&bar),
            );
        }
        let _ = writeln!(out);
    }

    // ── Summary ──
    let total_device: u64 = devices.iter().map(|i| i.counts.iter().sum::<u64>()).sum();
    let total_special: u64 = specials.iter().map(|i| i.counts.iter().sum::<u64>()).sum();
    let total_soft: u64 = softirqs.iter().map(|s| s.counts.iter().sum::<u64>()).sum();

    let _ = writeln!(
        out,
        "{}  Device IRQs: {}   System IRQs: {}   SoftIRQs: {}",
        style::bold("Summary:"),
        style::yellow(&fmt_count(total_device)),
        style::yellow(&fmt_count(total_special)),
        style::yellow(&fmt_count(total_soft)),
    );
}
