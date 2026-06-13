//! Linux virtual memory topology explorer.
//!
//! Two entry points:
//! - `cat_vmmap(pid)` — per-process /proc/<pid>/maps visualisation
//! - `cat_meminfo()`  — system-wide /proc/meminfo + ZRAM summary
//!
//! Both use colour-coded terminal output with pressure indicators.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

// ── Colour/style helpers (inlined to keep self-contained) ──

mod style {
    pub fn bold(s: &str) -> String  { format!("\x1b[1m{s}\x1b[0m") }
    pub fn dim(s: &str) -> String   { format!("\x1b[2m{s}\x1b[0m") }
    pub fn green(s: &str) -> String { format!("\x1b[32m{s}\x1b[0m") }
    pub fn red(s: &str) -> String   { format!("\x1b[31m{s}\x1b[0m") }
    pub fn cyan(s: &str) -> String  { format!("\x1b[36m{s}\x1b[0m") }
    pub fn yellow(s: &str) -> String { format!("\x1b[33m{s}\x1b[0m") }
    pub fn blue(s: &str) -> String  { format!("\x1b[34m{s}\x1b[0m") }
    pub fn magenta(s: &str) -> String { format!("\x1b[35m{s}\x1b[0m") }
}

// ── Data types ──

#[derive(Debug)]
struct VmRegion {
    start: u64,
    end: u64,
    perms: String,
    offset: u64,
    dev: String,
    inode: u64,
    pathname: String,
    // From smaps (optional)
    rss: u64,
    pss: u64,
    dirty: u64,
    anonymous: u64,
    swap: u64,
    vm_flags: String,
}

impl VmRegion {
    fn size_kb(&self) -> u64 {
        (self.end - self.start) / 1024
    }

    /// Human-readable size string.
    fn size_hr(&self) -> String {
        human_size(self.size_kb() * 1024)
    }

    fn region_type(&self) -> &'static str {
        let pn = self.pathname.as_str();
        if pn == "[heap]" { "heap" }
        else if pn == "[stack]" || pn.starts_with("[stack:") { "stack" }
        else if pn == "[vdso]" || pn == "[vvar]" || pn == "[vdso32]" { "vdso" }
        else if pn == "[vsyscall]" { "vsyscall" }
        else if pn.starts_with("[") && pn.ends_with("]") { "anon-special" }
        else if pn.is_empty() { "anon" }
        else if pn.starts_with("/") { "file" }
        else { "other" }
    }

    fn type_color(&self) -> String {
        match self.region_type() {
            "heap"  => style::yellow(&self.pathname),
            "stack" => style::cyan(&self.pathname),
            "vdso" | "vsyscall" | "anon-special" => style::dim(&self.pathname),
            "anon"  => style::green("anonymous"),
            "file"  => style::blue(&shorten_path(&self.pathname)),
            _       => self.pathname.clone(),
        }
    }

    fn perms_colored(&self) -> String {
        let mut out = String::with_capacity(4);
        for (i, ch) in self.perms.chars().enumerate() {
            let colored = match ch {
                'r' => style::green("r"),
                'w' => style::red("w"),
                'x' => style::yellow("x"),
                'p' | 's' => style::dim(&ch.to_string()),
                '-' => style::dim("-"),
                _   => ch.to_string(),
            };
            out.push_str(&colored);
        }
        out
    }
}

// ── Parsing ──

/// Parse /proc/<pid>/maps into `VmRegion` records.
fn parse_maps(pid: u32) -> io::Result<Vec<VmRegion>> {
    let path = format!("/proc/{pid}/maps");
    let file = fs::File::open(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::PermissionDenied {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("process {pid}: permission denied (try same-owner or root)"),
            )
        } else {
            io::Error::new(io::ErrorKind::NotFound, format!("process {pid} not found"))
        }
    })?;
    let reader = io::BufReader::new(file);
    let mut regions = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // Format: address           perms offset  dev   inode   pathname
        //         7f1234000000-7f1234001000 r-xp 00000000 00:1c 17768  /usr/lib/libc.so
        let parts: Vec<&str> = line.splitn(6, ' ').collect();
        if parts.len() < 5 {
            continue;
        }

        // Parse address range
        let addr_parts: Vec<&str> = parts[0].split('-').collect();
        if addr_parts.len() != 2 {
            continue;
        }
        let start = u64::from_str_radix(addr_parts[0], 16).unwrap_or(0);
        let end = u64::from_str_radix(addr_parts[1], 16).unwrap_or(0);

        let perms = parts[1].to_string();
        let offset = u64::from_str_radix(parts[2], 16).unwrap_or(0);
        // Skip parts[3] = dev, parts[4] = inode for basic parsing
        let inode = parts[4].parse::<u64>().unwrap_or(0);
        let pathname = if parts.len() >= 6 {
            parts[5].trim().to_string()
        } else {
            String::new()
        };

        regions.push(VmRegion {
            start, end, perms, offset,
            dev: parts[3].to_string(),
            inode, pathname,
            rss: 0, pss: 0, dirty: 0, anonymous: 0, swap: 0,
            vm_flags: String::new(),
        });
    }
    Ok(regions)
}

/// Parse /proc/<pid>/smaps for detailed per-region stats.
fn parse_smaps_detailed(pid: u32, regions: &mut [VmRegion]) {
    let path = format!("/proc/{pid}/smaps");
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let mut region_idx: usize = usize::MAX;
    for line in content.lines() {
        if line.contains("VmFlags:") {
            if let Some(flags) = line.strip_prefix("VmFlags: ") {
                if region_idx < regions.len() {
                    regions[region_idx].vm_flags = flags.trim().to_string();
                }
            }
            continue;
        }

        // Check if this line starts a new region (address-perms line)
        if line.len() > 10 && line.as_bytes()[..2].iter().all(|b| b.is_ascii_hexdigit()) && line.contains(" r") {
            // Extract start address to match with regions
            let addr_start = line.split('-').next().and_then(|s| u64::from_str_radix(s, 16).ok());
            if let Some(addr) = addr_start {
                if let Some(idx) = regions.iter().position(|r| r.start == addr) {
                    region_idx = idx;
                }
            }
            continue;
        }

        if region_idx >= regions.len() {
            continue;
        }
        let r = &mut regions[region_idx];

        if let Some(val) = parse_smaps_kb(line, "Rss:") {
            r.rss = val;
        } else if let Some(val) = parse_smaps_kb(line, "Pss:") {
            r.pss = val;
        } else if let Some(val) = parse_smaps_kb(line, "Private_Dirty:") {
            r.dirty += val;
        } else if let Some(val) = parse_smaps_kb(line, "Shared_Dirty:") {
            r.dirty += val;
        } else if let Some(val) = parse_smaps_kb(line, "Anonymous:") {
            r.anonymous = val;
        } else if let Some(val) = parse_smaps_kb(line, "Swap:") {
            r.swap = val;
        }
    }
}

fn parse_smaps_kb(line: &str, key: &str) -> Option<u64> {
    line.strip_prefix(key)
        .and_then(|rest| rest.trim().strip_suffix(" kB"))
        .and_then(|num| num.trim().parse::<u64>().ok())
}

// ── Helpers ──

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut size = bytes as f64;
    let mut idx = 0;
    while size >= 1024.0 && idx < UNITS.len() - 1 {
        size /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{bytes} B")
    } else {
        format!("{:.1} {} ({bytes} B)", size, UNITS[idx])
    }
}

fn shorten_path(path: &str) -> String {
    // For paths like /usr/lib/libfoo.so.1.2.3, show just the basename
    if let Some(base) = std::path::Path::new(path).file_name() {
        base.to_string_lossy().to_string()
    } else {
        path.to_string()
    }
}

/// Read a single value from /proc/<pid>/status
fn proc_status_value(pid: u32, key: &str) -> String {
    let path = format!("/proc/{pid}/status");
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return "N/A".into(),
    };
    for line in content.lines() {
        if let Some(val) = line.strip_prefix(key) {
            return val.trim().to_string();
        }
    }
    "N/A".into()
}

fn read_sysfs_u64(path: &str) -> Option<u64> {
    fs::read_to_string(path).ok().and_then(|s| {
        let trimmed = s.trim();
        // Some sysfs files have spaces (e.g. mm_stat is space-separated)
        if trimmed.contains(' ') {
            trimmed
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok())
        } else {
            trimmed.parse::<u64>().ok()
        }
    })
}

fn read_zram_stats() -> Vec<(String, String)> {
    let zram_dir = Path::new("/sys/block");
    let mut stats = Vec::new();

    let entries = match fs::read_dir(zram_dir) {
        Ok(e) => e,
        Err(_) => return stats,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("zram") {
            continue;
        }

        // mm_stat: orig_size compr_size mem_used_total 0 zero_pages ...
        if let Some(mm) = read_sysfs_u64(&format!("/sys/block/{name_str}/mm_stat")) {
            let orig_size = mm;
            let compr_size = read_sysfs_u64(&format!("/sys/block/{name_str}/mm_stat"))
                .and_then(|_| {
                    let content = fs::read_to_string(format!("/sys/block/{name_str}/mm_stat")).ok()?;
                    let parts: Vec<&str> = content.trim().split_whitespace().collect();
                    parts.get(1).and_then(|v| v.parse::<u64>().ok())
                });
            let mem_used = read_sysfs_u64(&format!("/sys/block/{name_str}/mm_stat"))
                .and_then(|_| {
                    let content = fs::read_to_string(format!("/sys/block/{name_str}/mm_stat")).ok()?;
                    let parts: Vec<&str> = content.trim().split_whitespace().collect();
                    parts.get(2).and_then(|v| v.parse::<u64>().ok())
                });
            let zero_pages = read_sysfs_u64(&format!("/sys/block/{name_str}/mm_stat"))
                .and_then(|_| {
                    let content = fs::read_to_string(format!("/sys/block/{name_str}/mm_stat")).ok()?;
                    let parts: Vec<&str> = content.trim().split_whitespace().collect();
                    parts.get(4).and_then(|v| v.parse::<u64>().ok())
                });

            stats.push(("Device".into(), format!("/dev/{name_str}")));
            if let Some(c) = compr_size {
                let ratio = if orig_size > 0 {
                    orig_size as f64 / c.max(1) as f64
                } else {
                    0.0
                };
                stats.push(("Original".into(), human_size(orig_size)));
                stats.push(("Compressed".into(), human_size(c)));
                stats.push(("Compression".into(), format!("{:.2}x", ratio)));
            }
            if let Some(mu) = mem_used {
                stats.push(("Mem used".into(), human_size(mu)));
            }
            if let Some(zp) = zero_pages {
                stats.push(("Zero pages".into(), format!("{zp}")));
            }

            // Compression algorithm
            if let Ok(algo) = fs::read_to_string(format!("/sys/block/{name_str}/comp_algorithm")) {
                stats.push(("Algorithm".into(), algo.trim().to_string()));
            }
            // Backing dev (if any)
            if let Ok(bd) = fs::read_to_string(format!("/sys/block/{name_str}/backing_dev")) {
                let bd = bd.trim().to_string();
                if !bd.is_empty() && bd != "none" {
                    stats.push(("Backing".into(), bd));
                }
            }
        }
    }
    stats
}

// ── Main entry: cat_vmmap ──

/// Show virtual memory map for a process.
pub fn cat_vmmap(pid: u32, detailed: bool) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Print process header
    let name = proc_status_value(pid, "Name:");
    let vmsize = proc_status_value(pid, "VmSize:");
    let vmrss = proc_status_value(pid, "VmRSS:");
    let threads = proc_status_value(pid, "Threads:");
    let state = proc_status_value(pid, "State:");

    let _ = writeln!(out,
        "{} {} {}",
        style::dim("┌─ ccat vmmap ─────────────────────────────────────────────────────────────────┐"),
        "", "");
    let _ = writeln!(out, " │ {} {} (PID {}) — {} {} {}",
        style::bold("Process:"), style::cyan(&name), pid,
        style::dim("["), &state, style::dim("]"));
    let _ = writeln!(out, " │ {} {}  {} {}  {} {}        {}",
        style::bold("VmSize:"), vmsize,
        style::bold("VmRSS:"), vmrss,
        style::bold("Threads:"), threads,
        style::dim("│"));

    // Parse maps
    let mut regions = match parse_maps(pid) {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(out, " │ {} {}",
                style::red("ERROR:"), e);
            let _ = writeln!(out, " {}", style::dim("└──────────────────────────────────────────────────────────────────────────┘"));
            return;
        }
    };

    if detailed {
        parse_smaps_detailed(pid, &mut regions);
    }

    // Compute summary stats
    let total_virtual: u64 = regions.iter().map(|r| r.size_kb()).sum();
    let total_anon: u64 = regions.iter()
        .filter(|r| r.pathname.is_empty() || r.pathname.starts_with('['))
        .map(|r| r.size_kb()).sum();
    let total_file: u64 = regions.iter()
        .filter(|r| !r.pathname.is_empty() && !r.pathname.starts_with('['))
        .map(|r| r.size_kb()).sum();
    let total_rss: u64 = regions.iter().map(|r| r.rss).sum();
    let total_pss: u64 = regions.iter().map(|r| r.pss).sum();
    let total_swap: u64 = regions.iter().map(|r| r.swap).sum();

    let _ = writeln!(out, " │ {} {:>8}  {} {:>8}  {} {:>8}",
        style::bold("Virtual:"), human_size(total_virtual * 1024),
        style::bold("Anon:"), human_size(total_anon * 1024),
        style::bold("File:"), human_size(total_file * 1024));

    if detailed {
        let _ = writeln!(out, " │ {} {:>8}  {} {:>8}  {} {:>8}",
            style::bold("RSS:"), human_size(total_rss * 1024),
            style::bold("PSS:"), human_size(total_pss * 1024),
            style::bold("Swap:"), human_size(total_swap * 1024));
    }

    let _ = writeln!(out, " │ {} {}",
        style::dim("│"), "");

    // ── Region table header ──
    let header = if detailed {
        format!("{:<16} {:<5} {:>9} {:>9} {:>9} {:>9}  {}",
            "ADDRESS", "PERMS", "SIZE", "RSS", "DIRTY", "SWAP", "REGION")
    } else {
        format!("{:<16} {:<5} {:>9}  {}",
            "ADDRESS", "PERMS", "SIZE", "REGION")
    };
    let _ = writeln!(out, " │ {} {}", style::dim(&header), style::dim("│"));

    // ── Aggregate regions by type for compact view ──
    // Group consecutive anon regions together
    let mut compact_regions: Vec<(String, u64, u64, u64, u64, u64, String)> = Vec::new();
    let mut i = 0;
    while i < regions.len() {
        let r = &regions[i];
        let rtype = r.region_type();
        let path = r.pathname.clone();

        if rtype == "anon" || rtype == "anon-special" {
            // Merge consecutive anonymous regions
            let mut total_size = r.size_kb();
            let mut total_rss = r.rss;
            let mut total_dirty = r.dirty;
            let mut total_swap = r.swap;
            let mut total_pss = r.pss;
            let start_addr = r.start;
            let mut end_addr = r.end;
            let mut merged = 0;
            let mut display_name = if r.pathname == "[vsyscall]" { path.clone() } else { "[anonymous]".to_string() };
            if !r.pathname.is_empty() && r.pathname.starts_with('[') {
                display_name = r.pathname.clone();
            }

            let mut special_count = 0;
            let mut j = i + 1;
            while j < regions.len() {
                let next = &regions[j];
                let next_type = next.region_type();
                // Merge consecutive anon regions regardless of gap
                // (small gaps between anonymous mappings are still interesting
                //  but showing them individually is noisy)
                if next_type == "anon" && next.pathname.is_empty() {
                    total_size += next.size_kb();
                    total_rss += next.rss;
                    total_dirty += next.dirty;
                    total_swap += next.swap;
                    total_pss += next.pss;
                    end_addr = next.end;
                    merged += 1;
                    j += 1;
                } else if next.pathname.starts_with("[") && next.pathname.ends_with("]") && next.pathname != "[heap]" && next.pathname != "[stack]" {
                    // Special anon regions like [vvar], [vdso], etc.
                    total_size += next.size_kb();
                    total_rss += next.rss;
                    total_dirty += next.dirty;
                    total_swap += next.swap;
                    total_pss += next.pss;
                    end_addr = next.end;
                    if special_count == 0 {
                        display_name = format!("[anon: {}]", path);
                    }
                    if !next.pathname.is_empty() {
                        display_name = format!("{} + {}", display_name, &next.pathname[1..next.pathname.len()-1]);
                    }
                    special_count += 1;
                    merged += 1;
                    j += 1;
                } else {
                    break;
                }
            }
            if merged > 0 {
                display_name = format!("{} +{} merged", display_name, merged);
            }

            let addr_str = format!("{:016x}-{:016x}", start_addr, end_addr);
            compact_regions.push((addr_str, total_size, total_rss, total_dirty, total_swap, total_pss, display_name));
            i = j;
        } else {
            let addr_str = format!("{:016x}-{:016x}", r.start, r.end);
            compact_regions.push((addr_str, r.size_kb(), r.rss, r.dirty, r.swap, r.pss, r.pathname.clone()));
            i += 1;
        }
    }

    // ── Render regions ──
    // Apply a pager-like approach: cap display at terminal height
    let display_count = compact_regions.len();
    let max_display = 40;
    let truncated = display_count > max_display;

    // Show first max_display regions
    let to_show = if truncated { &compact_regions[..max_display] } else { &compact_regions[..] };

    for (addr, size_kb, rss, dirty, swap, pss, pathname) in to_show {
        let rtype = if pathname == "[heap]" { "heap" }
            else if pathname == "[stack]" || pathname.starts_with("[stack:") { "stack" }
            else if pathname.starts_with("[") && pathname.ends_with("]") { "anon-special" }
            else if pathname.is_empty() || pathname.contains("[anonymous") { "anon" }
            else { "file" };

        let path_colored = match rtype {
            "heap"  => style::yellow(pathname),
            "stack" => style::cyan(pathname),
            "anon-special" => style::dim(pathname),
            "anon"  => style::green("[anonymous]"),
            "file"  => style::blue(&shorten_path(pathname)),
            _       => pathname.clone(),
        };

        // Determine perms string color
        let perms = if pathname == "[vdso]" || pathname == "[vvar]" {
            style::dim("----")
        } else if rtype == "file" && *size_kb < 4 {
            // Tiny file mappings = less interesting
            format!("{}{}", &regions[0].perms[..1], style::dim(&regions[0].perms[1..]))
        } else {
            // Default perms display
            format!("rwxp") // placeholder
        };

        // Actually use the original region's perms — we need to find it
        // For compact view we need perms from the first constituent region
        // Let's just use the perms from the starting address
        let original_perms = {
            let addr_start = addr.split('-').next().and_then(|s| u64::from_str_radix(s, 16).ok());
            if let Some(a) = addr_start {
                regions.iter()
                    .find(|r| r.start == a)
                    .map(|r| r.perms_colored())
                    .unwrap_or_else(|| "----".to_string())
            } else {
                "----".to_string()
            }
        };

        if detailed {
            let rss_str = if *rss > 0 { human_size(*rss * 1024) } else { "-".into() };
            let dirty_str = if *dirty > 0 { human_size(*dirty * 1024) } else { "-".into() };
            let swap_str = if *swap > 0 { human_size(*swap * 1024) } else { "-".into() };
            let _ = writeln!(out, " │ {addr} {} {:>9} {:>9} {:>9} {:>9}  {} {}",
                original_perms,
                human_size(*size_kb * 1024),
                rss_str,
                dirty_str,
                swap_str,
                path_colored,
                style::dim("│"));
        } else {
            let _ = writeln!(out, " │ {addr} {} {:>9}  {} {}",
                original_perms,
                human_size(*size_kb * 1024),
                path_colored,
                style::dim("│"));
        }
    }

    if truncated {
        let remaining = display_count - max_display;
        let _ = writeln!(out, " │ {} {} {}",
            style::dim("... and"), remaining, style::dim("more regions (use --vmmap for full list)"));
    }

    let _ = writeln!(out, " │ {}", style::dim("│"));
    // ── Legend ──
    let _ = writeln!(out, " │ {} {} {} {} {} {} {}",
        style::yellow("■ heap"),
        style::cyan("■ stack"),
        style::green("■ anon"),
        style::blue("■ file"),
        style::red("w writable"),
        style::yellow("x executable"),
        style::dim("│"));

    // Footer with key ratios
    let anon_pct = if total_virtual > 0 { total_anon as f64 / total_virtual as f64 * 100.0 } else { 0.0 };
    let _ = writeln!(out, " │ {} {:>8} regions, {}% anonymous",
        style::bold("Total:"), display_count, format!("{:.1}", anon_pct));

    // Draw footer border
    let _ = writeln!(out, " {}", style::dim("└──────────────────────────────────────────────────────────────────────────┘"));
}

// ── Main entry: cat_meminfo ──

/// Show system-wide memory summary with colour-coded pressure indicators.
pub fn cat_meminfo() {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let _ = writeln!(out, "{}",
        style::dim("┌─ ccat meminfo ────────────────────────────────────────────────────────────────┐"));

    // ── Parse /proc/meminfo ──
    let meminfo = match fs::read_to_string("/proc/meminfo") {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(out, " │ {} /proc/meminfo: {} {}",
                style::red("ERROR:"), e, style::dim("│"));
            let _ = writeln!(out, " {}", style::dim("└──────────────────────────────────────────────────────────────────────────┘"));
            return;
        }
    };

    // Parse into a map
    let mut mem = std::collections::HashMap::new();
    for line in meminfo.lines() {
        if let Some((key, val)) = line.split_once(':') {
            let val = val.trim().trim_end_matches(" kB").trim();
            if let Ok(num) = val.parse::<u64>() {
                mem.insert(key.trim().to_string(), num);
            }
        }
    }

    fn kb(k: u64) -> String { human_size(k * 1024) }

    let total = mem.get("MemTotal").copied().unwrap_or(0);
    let free = mem.get("MemFree").copied().unwrap_or(0);
    let available = mem.get("MemAvailable").copied().unwrap_or(0);
    let cached = mem.get("Cached").copied().unwrap_or(0);
    let buffers = mem.get("Buffers").copied().unwrap_or(0);
    let active = mem.get("Active").copied().unwrap_or(0);
    let inactive = mem.get("Inactive").copied().unwrap_or(0);
    let swap_total = mem.get("SwapTotal").copied().unwrap_or(0);
    let swap_free = mem.get("SwapFree").copied().unwrap_or(0);
    let anon_pages = mem.get("AnonPages").copied().unwrap_or(0);
    let mapped = mem.get("Mapped").copied().unwrap_or(0);
    let page_tables = mem.get("PageTables").copied().unwrap_or(0);
    let slab = mem.get("Slab").copied().unwrap_or(0);
    let sreclaimable = mem.get("SReclaimable").copied().unwrap_or(0);
    let commit_limit = mem.get("CommitLimit").copied().unwrap_or(0);
    let committed_as = mem.get("Committed_AS").copied().unwrap_or(0);
    let direct_map_2m = mem.get("DirectMap2M").copied().unwrap_or(0);
    let direct_map_1g = mem.get("DirectMap1G").copied().unwrap_or(0);
    let huge_anon = mem.get("AnonHugePages").copied().unwrap_or(0);

    let used = total.saturating_sub(free + buffers + cached);

    // ── Memory overview ──
    let _ = writeln!(out, " │ {} {:>25} {}",
        style::bold("MEMORY"), "", style::dim("│"));

    let avail_pct = if total > 0 { available as f64 / total as f64 * 100.0 } else { 0.0 };
    let used_gb = (total - available) as f64 / 1_048_576.0;
    let total_gb = total as f64 / 1_048_576.0;

    let _ = writeln!(out, " │ {:>6} {:>10}  {:>10}  {}",
        style::bold("Total:"), kb(total),
        style::bold("Available:"), pressure_color(avail_pct, &kb(available)));
    let _ = writeln!(out, " │ {:>6} {:>10}  {:>10}  {:>8.1}% busy  {} {}",
        style::bold("Used:"), kb(total - free),
        style::bold("Free:"), kb(free),
        100.0 - (free as f64 / total as f64 * 100.0),
        style::dim("│"));

    // Bar chart
    let used_ratio = if total > 0 { (total - available) as f64 / total as f64 } else { 0.0 };
    let bar = memory_bar(used_ratio, 40);
    let _ = writeln!(out, " │   {} {:>5.1}% {}",
        bar, used_ratio * 100.0, style::dim("│"));

    // ── Breakdown ──
    let _ = writeln!(out, " │ {} {:>30} {}",
        style::dim("├─ Breakdown ────────────────────────────────"), "", style::dim("│"));

    let anon_pct = if total > 0 { anon_pages as f64 / total as f64 * 100.0 } else { 0.0 };
    let cache_pct = if total > 0 { cached as f64 / total as f64 * 100.0 } else { 0.0 };
    let _ = writeln!(out, " │   {:>12} {:>10} ({:>5.1}%)",
        style::bold("AnonPages:"), kb(anon_pages), anon_pct);
    let _ = writeln!(out, " │   {:>12} {:>10} ({:>5.1}%)",
        style::bold("Cached:"), kb(cached), cache_pct);
    let _ = writeln!(out, " │   {:>12} {:>10}",
        style::bold("Buffers:"), kb(buffers));
    let _ = writeln!(out, " │   {:>12} {:>10}",
        style::bold("Mapped:"), kb(mapped));
    let _ = writeln!(out, " │   {:>12} {:>10}  (reclaimable: {})",
        style::bold("Slab:"), kb(slab), kb(sreclaimable));
    let _ = writeln!(out, " │   {:>12} {:>10}",
        style::bold("PageTables:"), kb(page_tables));
    let _ = writeln!(out, " │   {:>12} {:>10}",
        style::bold("Active:"), kb(active));
    let _ = writeln!(out, " │   {:>12} {:>10}",
        style::bold("Inactive:"), kb(inactive));

    // ── Swap ──
    let _ = writeln!(out, " │ {}", style::dim("│"));
    let _ = writeln!(out, " │ {} {:>30} {}",
        style::dim("├─ Swap ──────────────────────────────────────"), "", style::dim("│"));

    let swap_used = swap_total.saturating_sub(swap_free);
    let swap_pct = if swap_total > 0 { swap_used as f64 / swap_total as f64 * 100.0 } else { 0.0 };
    let swap_color = if swap_pct > 80.0 { style::red }
        else if swap_pct > 50.0 { style::yellow }
        else { |s: &str| s.to_string() };

    let _ = writeln!(out, " │ {:>12} {:>10}",
        style::bold("SwapTotal:"), kb(swap_total));
    let _ = writeln!(out, " │ {:>12} {:>10}",
        style::bold("SwapUsed:"), swap_color(&kb(swap_used)));
    let _ = writeln!(out, " │ {:>12} {:>10}",
        style::bold("SwapFree:"), kb(swap_free));

    if swap_total > 0 {
        let swap_bar = memory_bar(swap_pct / 100.0, 40);
        let _ = writeln!(out, " │   {} {:>5.1}% {}",
            swap_bar, swap_pct, style::dim("│"));
    }

    // SwapCached
    if let Some(&swap_cached) = mem.get("SwapCached") {
        if swap_cached > 0 {
            let _ = writeln!(out, " │ {:>12} {:>10}  (pages re-read into RAM, still in swap)",
                style::bold("SwapCached:"), kb(swap_cached));
        }
    }

    // ── ZRAM stats ──
    let zram_stats = read_zram_stats();
    if !zram_stats.is_empty() {
        let _ = writeln!(out, " │ {}", style::dim("│"));
        let _ = writeln!(out, " │ {} {:>30} {}",
            style::dim("├─ ZRAM ──────────────────────────────────────"), "", style::dim("│"));
        for (key, val) in &zram_stats {
            let _ = writeln!(out, " │   {:>10} {}", key, val);
        }
    }

    // ── Huge pages ──
    let _ = writeln!(out, " │ {}", style::dim("│"));
    let _ = writeln!(out, " │ {} {:>30} {}",
        style::dim("├─ Huge Pages & TLB ───────────────────────────"), "", style::dim("│"));
    let _ = writeln!(out, " │   {:>12} {:>10}",
        style::bold("AnonHuge:"), kb(huge_anon));
    let _ = writeln!(out, " │   {:>12} {:>10}  (2M: {}  |  1G: {})",
        style::bold("DirectMap:"), kb(direct_map_2m + direct_map_1g),
        kb(direct_map_2m), kb(direct_map_1g));

    // ── Commit ──
    let _ = writeln!(out, " │ {}", style::dim("│"));
    let _ = writeln!(out, " │ {} {:>30} {}",
        style::dim("├─ Commit ─────────────────────────────────────"), "", style::dim("│"));

    let commit_pct = if commit_limit > 0 { committed_as as f64 / commit_limit as f64 * 100.0 } else { 0.0 };
    let commit_color = if commit_pct > 100.0 { style::red }
        else if commit_pct > 80.0 { style::yellow }
        else { |s: &str| s.to_string() };

    let _ = writeln!(out, " │   {:>12} {:>10}",
        style::bold("Committed:"), kb(committed_as));
    let _ = writeln!(out, " │   {:>12} {:>10}",
        style::bold("Limit:"), kb(commit_limit));
    let _ = writeln!(out, " │   {:>12} {:>10}  {}",
        style::bold("OOM risk:"), commit_color(&format!("{:.1}%", commit_pct)),
        style::dim("│"));

    // ── Page fault stats (quick summary) ──
    if let Ok(vmstat) = fs::read_to_string("/proc/vmstat") {
        let mut pfault = 0u64;
        let mut majflt = 0u64;
        for line in vmstat.lines() {
            if let Some(n) = line.strip_prefix("pgfault ") {
                pfault = n.trim().parse().unwrap_or(0);
            } else if let Some(n) = line.strip_prefix("pgmajfault ") {
                majflt = n.trim().parse().unwrap_or(0);
            }
        }
        if pfault > 0 || majflt > 0 {
            let _ = writeln!(out, " │ {}", style::dim("│"));
            let _ = writeln!(out, " │ {} {:>30} {}",
                style::dim("├─ Page Faults ──────────────────────────────"), "", style::dim("│"));
            let maj_pct = if pfault > 0 { majflt as f64 / pfault as f64 * 100.0 } else { 0.0 };
            let maj_color = if maj_pct > 1.0 { style::red } else if maj_pct > 0.1 { style::yellow } else { |s: &str| s.to_string() };
            let _ = writeln!(out, " │   {:>12} {:>15}",
                style::bold("Total:"), pfault);
            let _ = writeln!(out, " │   {:>12} {:>15}  ({:.3}%)",
                style::bold("Major:"), maj_color(&format!("{}", majflt)), maj_pct);
        }
    }

    // ── Footer ──
    let _ = writeln!(out, " {}", style::dim("└──────────────────────────────────────────────────────────────────────────┘"));
}

// ── Memory pressure bar ──

fn memory_bar(ratio: f64, width: usize) -> String {
    let filled = (ratio * width as f64).round() as usize;
    let filled = filled.min(width);
    let empty = width.saturating_sub(filled);

    // Determine color based on threshold
    let color = if ratio > 0.9 {
        style::red
    } else if ratio > 0.75 {
        style::yellow
    } else {
        style::green
    };

    let fill_str: String = (0..filled).map(|_| '█').collect();
    let empty_str: String = (0..empty).map(|_| '░').collect();
    format!("{}{}", color(&fill_str), style::dim(&empty_str))
}

fn pressure_color(pct: f64, val: &str) -> String {
    if pct < 20.0 {
        style::red(val)
    } else if pct < 40.0 {
        style::yellow(val)
    } else {
        style::green(val)
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(500), "500 B");
        assert_eq!(human_size(1024), "1.0 KiB (1024 B)");
        assert_eq!(human_size(1536), "1.5 KiB (1536 B)");
        assert_eq!(human_size(1_048_576), "1.0 MiB (1048576 B)");
        assert_eq!(human_size(1_073_741_824), "1.0 GiB (1073741824 B)");
    }

    #[test]
    fn test_parse_smaps_kb() {
        assert_eq!(parse_smaps_kb("Rss:               8 kB", "Rss:"), Some(8));
        assert_eq!(parse_smaps_kb("Pss:               8 kB", "Pss:"), Some(8));
        assert_eq!(parse_smaps_kb("Private_Dirty:         0 kB", "Private_Dirty:"), Some(0));
        assert_eq!(parse_smaps_kb("Swap:               0 kB", "Swap:"), Some(0));
        assert_eq!(parse_smaps_kb("NotAThing: 42 kB", "Rss:"), None);
    }

    #[test]
    fn test_shorten_path() {
        assert_eq!(shorten_path("/usr/lib/libc.so.6"), "libc.so.6");
        assert_eq!(shorten_path("/usr/bin/cat"), "cat");
        assert_eq!(shorten_path("[heap]"), "[heap]");
    }

    #[test]
    fn test_memory_bar() {
        let bar = memory_bar(0.5, 10);
        // 5 filled + 5 empty visible chars, wrapped in ANSI codes
        let visible_count = bar.chars().filter(|&c| c == '█' || c == '░').count();
        assert_eq!(visible_count, 10);
        assert!(bar.contains('█'));
        assert!(bar.contains('░'));
    }

    #[test]
    fn test_memory_bar_full() {
        let bar = memory_bar(1.0, 5);
        assert!(!bar.contains('░')); // no empty at 100%
    }

    #[test]
    fn test_memory_bar_empty() {
        let bar = memory_bar(0.0, 5);
        assert!(!bar.contains('█')); // no filled at 0%
    }

    /// Parse a single maps line (helper for unit tests)
    fn parse_maps_line(line: &str) -> VmRegion {
        // Simulate what parse_maps does for a single entry
        let parts: Vec<&str> = line.splitn(6, ' ').collect();
        let addr_parts: Vec<&str> = parts[0].split('-').collect();
        let start = u64::from_str_radix(addr_parts[0], 16).unwrap_or(0);
        let end = u64::from_str_radix(addr_parts[1], 16).unwrap_or(0);
        let perms = parts[1].to_string();
        let offset = u64::from_str_radix(parts[2], 16).unwrap_or(0);
        let inode = parts[4].parse::<u64>().unwrap_or(0);
        let pathname = if parts.len() >= 6 { parts[5].trim().to_string() } else { String::new() };
        VmRegion {
            start, end, perms, offset,
            dev: parts[3].to_string(), inode, pathname,
            rss: 0, pss: 0, dirty: 0, anonymous: 0, swap: 0, vm_flags: String::new(),
        }
    }

    #[test]
    fn test_vm_region_size() {
        let r = parse_maps_line("7f0000000000-7f0000001000 r-xp 00000000 00:1c 17768 /usr/lib/libc.so");
        assert_eq!(r.size_kb(), 4); // 0x1000 = 4096 bytes = 4 KB
        assert!(r.size_hr().contains("4.0 KiB"));
    }

    #[test]
    fn test_vm_region_type_anon() {
        let r = parse_maps_line("7f0000000000-7f0000001000 rw-p 00000000 00:00 0 ");
        assert_eq!(r.region_type(), "anon");
    }

    #[test]
    fn test_vm_region_type_heap() {
        let r = parse_maps_line("555500000000-555500100000 rw-p 00000000 00:00 0 [heap]");
        assert_eq!(r.region_type(), "heap");
    }

    #[test]
    fn test_vm_region_type_stack() {
        let r = parse_maps_line("7fffc0000000-7fffc0001000 rw-p 00000000 00:00 0 [stack]");
        assert_eq!(r.region_type(), "stack");
    }

    #[test]
    fn test_vm_region_type_vdso() {
        let r = parse_maps_line("7fff00000000-7fff00001000 r-xp 00000000 00:00 0 [vdso]");
        assert_eq!(r.region_type(), "vdso");
    }

    #[test]
    fn test_vm_region_type_file() {
        let r = parse_maps_line("7f0000000000-7f0000001000 r-xp 00000000 00:1c 17768 /usr/bin/cat");
        assert_eq!(r.region_type(), "file");
    }

    #[test]
    fn test_parse_smaps_detailed_preserves_regions() {
        let mut regions = vec![
            parse_maps_line("560000000000-560000001000 r--p 00000000 00:1c 17768 /usr/bin/test"),
        ];
        // No smaps file exists, should silently no-op
        parse_smaps_detailed(9999999, &mut regions);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].rss, 0);
    }
}
