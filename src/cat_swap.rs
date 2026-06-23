//! Swap and zram analyzer (`ccat --swap`).
//!
//! Reads `/proc/swaps`, `/proc/meminfo`, `/proc/vmstat`, `/proc/pressure/memory`,
//! `/sys/block/zram0/*`, and `/proc/*/status` to produce a comprehensive,
//! colour-coded report of the system's swap situation:
//!
//! - System swap summary (total/used/free, usage bar)
//! - ZRAM deep dive (compression ratio, algorithm, zero-page efficiency, overhead)
//! - Swap I/O activity (page-in/out totals and rates)
//! - Top swap-consuming processes (top 15 by VmSwap)
//! - Swap cache & pressure correlation
//!
//! All data comes from /proc and /sys — no privileges or kernel modules needed.

use std::fs;

// ── Colour helpers (self-contained, mirrors cat_disk/cat_vmmap) ──

mod style {
    pub fn bold(s: impl AsRef<str>) -> String   { format!("\x1b[1m{}\x1b[0m", s.as_ref()) }
    pub fn dim(s: impl AsRef<str>) -> String    { format!("\x1b[2m{}\x1b[0m", s.as_ref()) }
    pub fn green(s: impl AsRef<str>) -> String  { format!("\x1b[32m{}\x1b[0m", s.as_ref()) }
    pub fn red(s: impl AsRef<str>) -> String    { format!("\x1b[31m{}\x1b[0m", s.as_ref()) }
    pub fn cyan(s: impl AsRef<str>) -> String   { format!("\x1b[36m{}\x1b[0m", s.as_ref()) }
    pub fn yellow(s: impl AsRef<str>) -> String { format!("\x1b[33m{}\x1b[0m", s.as_ref()) }
    pub fn blue(s: impl AsRef<str>) -> String   { format!("\x1b[34m{}\x1b[0m", s.as_ref()) }
    pub fn magenta(s: impl AsRef<str>) -> String { format!("\x1b[35m{}\x1b[0m", s.as_ref()) }
    pub fn white(s: impl AsRef<str>) -> String  { format!("\x1b[37m{}\x1b[0m", s.as_ref()) }
    pub fn grey(s: impl AsRef<str>) -> String   { format!("\x1b[90m{}\x1b[0m", s.as_ref()) }

    pub fn usage_bar(pct: f64, width: usize) -> String {
        let filled = ((pct / 100.0) * width as f64).round() as usize;
        let filled = filled.min(width);
        let empty = width.saturating_sub(filled);
        let fill_char = if pct > 90.0 { "█" } else if pct > 70.0 { "▓" } else { "█" };
        let color = if pct > 90.0 { red } else if pct > 70.0 { yellow } else { green };
        format!("{}{}",
            color(fill_char.repeat(filled)),
            grey("░".repeat(empty)))
    }
}

// ── Data structures ──

struct SwapInfo {
    total_kb: u64,
    used_kb: u64,
    free_kb: u64,
    swap_cached_kb: u64,
}

struct ZramInfo {
    orig_bytes: u64,
    comp_bytes: u64,
    mem_used_bytes: u64,
    zero_pages: u64,        // pages stored as single zero byte
    zero_bytes: u64,        // zero_page count in bytes
    max_used_bytes: u64,
    disksize_bytes: u64,
    algorithm: String,
    initstate: u8,
    // I/O
    reads_completed: u64,
    reads_merged: u64,
    sectors_read: u64,
    read_ticks: u64,
    writes_completed: u64,
    writes_merged: u64,
    sectors_written: u64,
    write_ticks: u64,
    // io_stat
    num_reads: u64,      // zram bd_stat covers backing device
    num_writes: u64,
    num_discards: u64,
    num_comp_err: u64,
}

struct SwapIoInfo {
    pswpin: u64,
    pswpout: u64,
    uptime_secs: f64,
}

struct SwapProcess {
    pid: u32,
    name: String,
    swap_kb: u64,
}

// ── Helpers ──

fn human_size(bytes: u64) -> String {
    if bytes == 0 { return "0 B".into(); }
    const UNITS: &[&str] = &["B", "K", "M", "G", "T"];
    let mut val = bytes as f64;
    let mut unit_idx = 0;
    while val >= 1024.0 && unit_idx < UNITS.len() - 1 {
        val /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[unit_idx])
    } else if val < 10.0 {
        format!("{:.1}{}", val, UNITS[unit_idx])
    } else {
        format!("{:.0}{}", val, UNITS[unit_idx])
    }
}

/// Read a u64 value from /proc/meminfo by key name.
fn read_meminfo(key: &str) -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if line.starts_with(key) {
            // "SwapTotal:     1995772 kB"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                return parts[1].parse::<u64>().ok();
            }
        }
    }
    None
}

/// Read a u64 from /proc/vmstat by key name.
fn read_vmstat(key: &str) -> Option<u64> {
    let text = fs::read_to_string("/proc/vmstat").ok()?;
    for line in text.lines() {
        if line.starts_with(key) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                return parts[1].parse::<u64>().ok();
            }
        }
    }
    None
}

/// Read a sysfs u64 value from a path.
fn read_sysfs_u64(path: &str) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse::<u64>().ok()
}

/// Read a sysfs string value.
fn read_sysfs_string(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

// ── Data collection ──

fn collect_swap_info() -> SwapInfo {
    // From /proc/swaps — needed for accurate used/free
    // But /proc/meminfo also has SwapTotal/SwapFree
    let total_kb = read_meminfo("SwapTotal").unwrap_or(0);
    let free_kb = read_meminfo("SwapFree").unwrap_or(0);
    let swap_cached_kb = read_meminfo("SwapCached").unwrap_or(0);
    let used_kb = total_kb.saturating_sub(free_kb);
    SwapInfo { total_kb, used_kb, free_kb, swap_cached_kb }
}

fn collect_zram_info() -> Option<ZramInfo> {
    // mm_stat (kernel 6.13+): orig_data_size compr_data_size mem_used_total
    //   mem_limit mem_used_max zero_pages writeback_limit [num_migrated...]
    // All values in bytes; zero_pages is a count (not bytes).
    let mm = read_sysfs_string("/sys/block/zram0/mm_stat")?;
    let fields: Vec<&str> = mm.split_whitespace().collect();
    if fields.len() < 7 { return None; }

    let orig_bytes = fields[0].parse::<u64>().ok()?;
    let comp_bytes = fields[1].parse::<u64>().ok()?;
    let mem_used_bytes = fields[2].parse::<u64>().ok()?;
    let _mem_limit = fields[3].parse::<u64>().ok()?;
    let max_used_bytes = fields[4].parse::<u64>().ok()?;  // mem_used_max, already in bytes
    let zero_pages = fields[5].parse::<u64>().ok()?;
    let _writeback_limit = fields[6].parse::<u64>().ok()?;

    // Zero_bytes: each zero page represents 4KB of uncompressed data
    let page_size: u64 = 4096;
    let zero_bytes = zero_pages * page_size;

    // disksize in bytes
    let disksize_bytes = read_sysfs_u64("/sys/block/zram0/disksize").unwrap_or(0);
    let algorithm = read_sysfs_string("/sys/block/zram0/comp_algorithm")
        .map(|s| {
            // Format: "lzo-rle lzo lz4 lz4hc [zstd] deflate 842"
            // Extract the active one (in brackets)
            if let Some(start) = s.find('[') {
                if let Some(end) = s[start+1..].find(']') {
                    return s[start+1..start+1+end].to_string();
                }
            }
            s
        })
        .unwrap_or_else(|| "unknown".into());
    let initstate = read_sysfs_u64("/sys/block/zram0/initstate").unwrap_or(0) as u8;

    // stat: I/O statistics (same format as /proc/diskstats)
    // fields: reads completed, reads merged, sectors read, read ticks,
    //         writes completed, writes merged, sectors written, write ticks,
    //         I/O in progress, I/O time, weighted I/O time, discards...
    let stat = read_sysfs_string("/sys/block/zram0/stat")?;
    let sfields: Vec<&str> = stat.split_whitespace().collect();

    let reads_completed = sfields.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
    let reads_merged = sfields.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let sectors_read = sfields.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let read_ticks = sfields.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let writes_completed = sfields.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
    let writes_merged = sfields.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
    let sectors_written = sfields.get(6).and_then(|s| s.parse().ok()).unwrap_or(0);
    let write_ticks = sfields.get(7).and_then(|s| s.parse().ok()).unwrap_or(0);

    // io_stat: num_reads num_writes num_discards num_comp_err etc(?)
    let iostat = read_sysfs_string("/sys/block/zram0/io_stat").unwrap_or_default();
    let ifields: Vec<&str> = iostat.split_whitespace().collect();
    let num_reads = ifields.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
    let num_writes = ifields.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let num_discards = ifields.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let num_comp_err = ifields.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);

    Some(ZramInfo {
        orig_bytes,
        comp_bytes,
        mem_used_bytes,
        zero_pages,
        zero_bytes,
        max_used_bytes,
        disksize_bytes,
        algorithm,
        initstate,
        reads_completed,
        reads_merged,
        sectors_read,
        read_ticks,
        writes_completed,
        writes_merged,
        sectors_written,
        write_ticks,
        num_reads,
        num_writes,
        num_discards,
        num_comp_err,
    })
}

fn collect_swap_io() -> SwapIoInfo {
    let pswpin = read_vmstat("pswpin").unwrap_or(0);
    let pswpout = read_vmstat("pswpout").unwrap_or(0);
    let uptime = fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(1.0);
    SwapIoInfo { pswpin, pswpout, uptime_secs: uptime }
}

fn collect_top_swap_processes(limit: usize) -> Vec<SwapProcess> {
    let mut procs = Vec::new();

    let dir = match fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return procs,
    };

    for entry in dir.flatten() {
        let pid_str = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        let status_path = entry.path().join("status");
        let status = match fs::read_to_string(&status_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut swap_kb: u64 = 0;
        let mut name = String::new();

        for line in status.lines() {
            if line.starts_with("VmSwap:") {
                // "VmSwap:    12345 kB"
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    swap_kb = parts[1].parse().unwrap_or(0);
                }
            } else if line.starts_with("Name:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    name = parts[1].to_string();
                }
            }
        }

        if swap_kb > 0 {
            procs.push(SwapProcess { pid, name, swap_kb });
        }
    }

    procs.sort_by(|a, b| b.swap_kb.cmp(&a.swap_kb));
    procs.truncate(limit);
    procs
}

fn collect_psi_memory() -> Option<(f64, f64, u64)> {
    // Format: "some avg10=0.00 avg60=0.00 avg300=0.00 total=1089379389"
    let text = fs::read_to_string("/proc/pressure/memory").ok()?;
    let mut some_total = 0u64;
    let mut full_total = 0u64;

    for line in text.lines() {
        if line.starts_with("some ") {
            if let Some(t) = line.split_whitespace()
                .find(|s| s.starts_with("total="))
                .and_then(|s| s[6..].parse::<u64>().ok())
            {
                some_total = t;
            }
        } else if line.starts_with("full ") {
            if let Some(t) = line.split_whitespace()
                .find(|s| s.starts_with("total="))
                .and_then(|s| s[6..].parse::<u64>().ok())
            {
                full_total = t;
            }
        }
    }

    Some((some_total as f64, full_total as f64, 0))
}

// ── Renderers ──

fn render_swap_summary(swap: &SwapInfo, width: usize) {
    let pct = if swap.total_kb > 0 {
        swap.used_kb as f64 / swap.total_kb as f64 * 100.0
    } else {
        0.0
    };

    let bar_width = width.saturating_sub(40).max(10).min(60);
    let bar = style::usage_bar(pct, bar_width);

    let severity = if pct > 90.0 { style::red("CRITICAL") }
                   else if pct > 70.0 { style::yellow("WARNING") }
                   else { style::green("OK") };

    println!();
    println!("  {}  {}", style::bold("Swap Usage"), severity);
    println!("  {} {} {:>9} / {:>9} ({:>4.0}%)",
        bar, style::dim("│"),
        human_size(swap.used_kb * 1024),
        human_size(swap.total_kb * 1024),
        pct,
    );
    println!("  {}  {}  {}",
        style::bold("Free:"), human_size(swap.free_kb * 1024),
        style::dim(format!("({} available)", human_size(swap.free_kb * 1024))),
    );

    if swap.swap_cached_kb > 0 {
        println!("  {}  {}  {}",
            style::bold("Cache:"),
            human_size(swap.swap_cached_kb * 1024),
            style::dim("(pages re-read into RAM, still resident in swap)"),
        );
    }
}

fn render_zram(zram: &ZramInfo, width: usize) {
    println!();
    println!("  {}  {} ({})",
        style::bold("ZRAM Compression"),
        style::green("active"),
        style::grey(&zram.algorithm),
    );

    if zram.initstate == 0 {
        println!("  {}  {}", style::yellow("⚠"), "ZRAM device not initialized");
        return;
    }

    if zram.orig_bytes == 0 {
        println!("  {}  {}", style::dim("•"), "No data stored in ZRAM");
        return;
    }

    let ratio = if zram.comp_bytes > 0 {
        zram.orig_bytes as f64 / zram.comp_bytes as f64
    } else {
        0.0
    };

    let bar_width = width.saturating_sub(40).max(10).min(60);

    // Disk usage bar
    let disk_pct = if zram.disksize_bytes > 0 {
        zram.mem_used_bytes as f64 / zram.disksize_bytes as f64 * 100.0
    } else {
        0.0
    };
    let disk_bar = style::usage_bar(disk_pct, bar_width);

    let mem_saved = zram.orig_bytes.saturating_sub(zram.mem_used_bytes);
    let overhead = zram.mem_used_bytes.saturating_sub(zram.comp_bytes);

    println!("  {}  {:>8}  {} {}", style::bold("Orig:"), human_size(zram.orig_bytes), style::dim("(uncompressed input)"), style::grey(format!("={}", zram.orig_bytes / 4096)).replace("=", " pages: "));
    println!("  {}  {:>8}  {}  x{:.2} compression",
        style::bold("Comp:"), human_size(zram.comp_bytes),
        style::dim("(after compression)"), ratio);
    println!("  {}  {:>8}  {}",
        style::bold("Saved:"), human_size(mem_saved),
        style::green(format!("(-{:.0}%)", (1.0 - zram.mem_used_bytes as f64 / zram.orig_bytes.max(1) as f64) * 100.0)));
    println!("  {}  {:>8}  {}",
        style::bold("Overhead:"), human_size(overhead),
        style::dim("(metadata & internal fragmentation)"));

    // Disk usage
    println!("  {}  {:>8}  {} {}",
        style::bold("Disk:"), human_size(zram.mem_used_bytes), style::dim("used of"), human_size(zram.disksize_bytes));
    println!("  {} {} {:>5.0}%", disk_bar, style::dim("│"), disk_pct);

    // Zero page analysis
    if zram.zero_pages > 0 {
        let zero_pct = zram.zero_bytes as f64 / zram.orig_bytes.max(1) as f64 * 100.0;
        println!("  {}  {:>8}  {}  {:.0}% of orig",
            style::bold("ZeroPg:"), human_size(zram.zero_bytes),
            style::cyan(format!("({} pages)", zram.zero_pages)),
            zero_pct);
    }

    // Max used
    if zram.max_used_bytes > 0 {
        let max_pct = zram.max_used_bytes as f64 / zram.disksize_bytes.max(1) as f64 * 100.0;
        println!("  {}  {:>8}  {}",
            style::bold("Peak:"), human_size(zram.max_used_bytes),
            style::dim(format!("(historical max, {:.0}% of disk)", max_pct)));
    }

    // I/O summary
    let read_mb = zram.sectors_read as f64 * 512.0 / 1_048_576.0;
    let write_mb = zram.sectors_written as f64 * 512.0 / 1_048_576.0;
    let avg_read = if zram.writes_completed > 0 {
        zram.write_ticks as f64 / zram.writes_completed as f64
    } else { 0.0 };

    println!();
    println!("  {}  I/O since boot", style::bold("ZRAM I/O"));
    println!("  {}  {} reads · {} writes  ({} MB read · {} MB written)",
        style::dim("▸"),
        human_size(zram.reads_completed * 4096).replace("B", "pages"),
        human_size(zram.writes_completed * 4096).replace("B", "pages"),
        format!("{:.0}", read_mb),
        format!("{:.0}", write_mb),
    );
    if avg_read > 0.0 {
        println!("  {}  avg I/O latency: {:.1}ms  (write)",
            style::dim("▸"), avg_read / 1000.0);
    }

    if zram.num_comp_err > 0 {
        println!("  {}  {} compression errors",
            style::red("⚠"), zram.num_comp_err);
    }
}

fn render_swap_io(io: &SwapIoInfo) {
    let rate_in = io.pswpin as f64 / io.uptime_secs;
    let rate_out = io.pswpout as f64 / io.uptime_secs;

    println!();
    println!("  {}  ({} uptime)", style::bold("Swap I/O"),
        style::grey(format_duration(io.uptime_secs as u64)));

    println!("  {}  {}  {:>9}  ({:.1}/s)",
        style::dim("▸"),
        style::bold("In:"),
        human_size((io.pswpin * 4096) as u64),
        rate_in,
    );
    println!("  {}  {}  {:>9}  ({:.1}/s)",
        style::dim("▸"),
        style::bold("Out:"),
        human_size((io.pswpout * 4096) as u64),
        rate_out,
    );

    let ratio = if io.pswpin > 0 {
        io.pswpout as f64 / io.pswpin as f64
    } else { 0.0 };
    println!("  {}  out:in ratio = {:.2}  {}",
        style::dim("▸"), ratio,
        if ratio > 2.0 { style::yellow("(more swapping out = pressure building)") }
        else if ratio < 0.5 { style::green("(swapins dominate = active faulting)") }
        else { style::dim("(balanced)") },
    );
}

fn render_top_processes(procs: &[SwapProcess]) {
    if procs.is_empty() {
        return;
    }

    println!();
    println!("  {}  (top {})", style::bold("Swap-Hungry Processes"),
        style::grey(procs.len().to_string()));

    // Header
    println!("  {} {:>7} {:>9}  {}",
        style::dim("PID"), style::dim("SWAP"), style::dim("PCT"), style::dim("NAME"));

    let total_swap_kb: u64 = procs.iter().map(|p| p.swap_kb).sum();

    for p in procs {
        let pct = if total_swap_kb > 0 {
            p.swap_kb as f64 / total_swap_kb as f64 * 100.0
        } else { 0.0 };

        let color = if p.swap_kb > 30_000 { style::red }
                    else if p.swap_kb > 10_000 { style::yellow }
                    else { style::white };

        // Shorten long process names
        let name = if p.name.len() > 24 {
            format!("{}…", &p.name[..23])
        } else {
            p.name.clone()
        };

        println!("  {:>7} {:>9} {:>5.0}%  {}",
            style::grey(p.pid.to_string()),
            color(human_size(p.swap_kb * 1024)),
            pct,
            style::grey(&name),
        );
    }
}

fn render_psi_memory(cat_swap: &SwapInfo, zram: Option<&ZramInfo>, io: &SwapIoInfo) {
    let text = match fs::read_to_string("/proc/pressure/memory") {
        Ok(t) => t,
        Err(_) => return,
    };

    println!();
    println!("  {}  (stall time under memory pressure)", style::bold("Memory Pressure"));

    for line in text.lines() {
        if line.is_empty() { continue; }
        // "some avg10=0.00 avg60=0.00 avg300=0.00 total=1089379389"
        let prefix = if line.starts_with("some") { "some" }
                     else if line.starts_with("full") { "full" }
                     else { continue };

        let label = if prefix == "some" { "Some" } else { "Full" };
        let color = if prefix == "full" { style::red } else { style::yellow };

        // Parse avgs
        let parts: Vec<&str> = line.split_whitespace().collect();
        let avg10 = parts.iter().find(|s| s.starts_with("avg10=")).and_then(|s| s[6..].parse::<f64>().ok()).unwrap_or(0.0);
        let avg60 = parts.iter().find(|s| s.starts_with("avg60=")).and_then(|s| s[6..].parse::<f64>().ok()).unwrap_or(0.0);
        let avg300 = parts.iter().find(|s| s.starts_with("avg300=")).and_then(|s| s[7..].parse::<f64>().ok()).unwrap_or(0.0);
        let total = parts.iter().find(|s| s.starts_with("total=")).and_then(|s| s[6..].parse::<f64>().ok()).unwrap_or(0.0);

        // Convert total stalled microseconds to human
        let total_str = format_duration_us(total as u64);

        // Color the pressure values
        let avg10_s = if avg10 > 5.0 { style::red(format!("{:.2}%", avg10)) }
                      else if avg10 > 1.0 { style::yellow(format!("{:.2}%", avg10)) }
                      else { style::green(format!("{:.2}%", avg10)) };

        println!("  {}  {:>4}  avg10={}  avg60={:.2}%  avg300={:.2}%  total={}",
            color("■"),
            color(label),
            avg10_s,
            avg60,
            avg300,
            style::grey(total_str),
        );

        // Correlation insight for full pressure
        if prefix == "full" && total > 0.0 {
            let uptime_micros = io.uptime_secs * 1_000_000.0;
            let stall_pct = (total / uptime_micros.max(1.0)) * 100.0;

            if stall_pct > 5.0 {
                println!("  {}  {} {:.1}% of uptime — swap saturation likely, {} swapped ({:.0}% of swap)",
                    style::dim("   ↳"),
                    style::red("⚠"),
                    stall_pct,
                    human_size(cat_swap.used_kb * 1024),
                    if cat_swap.total_kb > 0 { cat_swap.used_kb as f64 / cat_swap.total_kb as f64 * 100.0 } else { 0.0 },
                );
            }

            // ZRAM correlation
            if let Some(z) = zram {
                if stall_pct > 1.0 && z.writes_completed > 0 {
                    let write_mb = z.sectors_written as f64 * 512.0 / 1_048_576.0;
                    let read_mb = z.sectors_read as f64 * 512.0 / 1_048_576.0;
                    println!("  {}  ZRAM: {} written, {} read — swap is actively cycling",
                        style::dim("   ↳"),
                        style::dim(format!("{:.0} MB", write_mb)),
                        style::dim(format!("{:.0} MB", read_mb)),
                    );
                }
            }
        }
    }
}

fn format_duration(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("{}d {}h {}m", days, hours, mins)
    } else if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else {
        format!("{}m {}s", mins, secs % 60)
    }
}

fn format_duration_us(micros: u64) -> String {
    if micros < 1_000 {
        format!("{}µs", micros)
    } else if micros < 1_000_000 {
        format!("{:.0}ms", micros as f64 / 1_000.0)
    } else if micros < 1_000_000_000 {
        format!("{:.1}s", micros as f64 / 1_000_000.0)
    } else {
        let secs = micros as f64 / 1_000_000.0;
        let mins = secs / 60.0;
        if mins > 60.0 {
            format!("{:.1}h", mins / 60.0)
        } else {
            format!("{:.0}m {:.0}s", mins, secs % 60.0)
        }
    }
}

// ── Public entry point ──

/// Show comprehensive swap analysis: ZRAM, swap I/O, top consumers, pressure correlation.
pub fn cat_swap() {
    let (width, _) = crate::pager::terminal_size();
    let _ = width;

    println!();
    println!("  {}  {}", style::bold("ccat — Swap Analyzer"), style::dim("───"));

    // ── 1. System swap summary ──
    let swap = collect_swap_info();
    render_swap_summary(&swap, width);

    // ── 2. ZRAM deep dive ──
    let zram = collect_zram_info();
    if let Some(ref z) = zram {
        render_zram(z, width);
    } else {
        println!();
        println!("  {}  No ZRAM information available.", style::yellow("⚠"));
    }

    // ── 3. Swap I/O ──
    let io = collect_swap_io();
    render_swap_io(&io);

    // ── 4. Memory pressure ──
    render_psi_memory(&swap, zram.as_ref(), &io);

    // ── 5. Top swap processes ──
    let procs = collect_top_swap_processes(15);
    render_top_processes(&procs);

    // ── Summary insight ──
    println!();
    println!("  {}", style::bold("Insights"));

    let swap_pct = if swap.total_kb > 0 {
        swap.used_kb as f64 / swap.total_kb as f64 * 100.0
    } else { 0.0 };

    if swap_pct > 90.0 {
        println!("  {} Swap is {} full ({:.1}% — only {} free).",
            style::red("■"),
            style::bold("critically"),
            swap_pct,
            human_size(swap.free_kb * 1024),
        );
        println!("  {} Top suggestion: increase ZRAM disk size, reduce workload, or add swap file.",
            style::dim("   ↳"));
    }

    if let Some(ref z) = zram {
        if z.writes_completed > z.reads_completed * 2 {
            println!("  {} ZRAM writes ({}) are {} reads ({}) — more pages being evicted to swap than faulted back.",
                style::yellow("■"),
                z.writes_completed,
                style::bold(format!("{:.0}x", z.writes_completed as f64 / z.reads_completed.max(1) as f64)),
                z.reads_completed,
            );
            println!("  {} This indicates {} memory pressure: applications are actively being swapped out.",
                style::dim("   ↳"),
                style::yellow("ongoing"),
            );
        }
    }

    let io_ratio = if io.pswpin > 0 {
        io.pswpout as f64 / io.pswpin as f64
    } else { 0.0 };

    if io_ratio > 2.0 {
        let excess_pct = (1.0 - io.pswpin as f64 / io.pswpout.max(1) as f64) * 100.0;
        println!("  {} {} of swapped pages never get re-read — net memory loss to swap.",
            style::yellow("■"),
            format!("{:.0}%", excess_pct),
        );
    }

    if !procs.is_empty() {
        let _total_swap_kb: u64 = procs.iter().map(|p| p.swap_kb).sum();
        let top = &procs[0];
        println!("  {} {} consumes {} — the biggest swap user ({}).",
            style::cyan("■"),
            top.name,
            human_size(top.swap_kb * 1024),
            style::grey(format!("PID {}", top.pid)),
        );
        if procs.len() > 3 {
            let chrome_swap: u64 = procs.iter()
                .filter(|p| p.name.contains("chrome") || p.name.contains("MainThread"))
                .map(|p| p.swap_kb)
                .sum();
            if chrome_swap > 10_000 {
                println!("  {} {} used by browsers ({} Chrome processes).",
                    style::cyan("■"),
                    human_size(chrome_swap * 1024),
                    procs.iter().filter(|p| p.name.contains("chrome")).count(),
                );
            }
            let npm_swap: u64 = procs.iter()
                .filter(|p| p.name == "npm" || p.name == "node")
                .map(|p| p.swap_kb)
                .sum();
            if npm_swap > 20_000 {
                println!("  {} {} used by node/npm processes.",
                    style::cyan("■"),
                    human_size(npm_swap * 1024),
                );
            }
        }
    }

    println!();
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size_zero() {
        assert_eq!(human_size(0), "0 B");
    }

    #[test]
    fn test_human_size_bytes() {
        assert_eq!(human_size(1), "1 B");
        assert_eq!(human_size(1023), "1023 B");
    }

    #[test]
    fn test_human_size_kb() {
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(10240), "10K");
        assert_eq!(human_size(102400), "100K");
    }

    #[test]
    fn test_human_size_mb() {
        assert_eq!(human_size(1024 * 1024), "1.0M");
        assert_eq!(human_size(500 * 1024 * 1024), "500M");
    }

    #[test]
    fn test_human_size_gb() {
        let gb = 1024u64 * 1024 * 1024;
        assert_eq!(human_size(gb), "1.0G");
        assert_eq!(human_size(100 * gb), "100G");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(30), "0m 30s");
        assert_eq!(format_duration(3600), "1h 0m");
        assert_eq!(format_duration(86400), "1d 0h 0m");
        assert_eq!(format_duration(90061), "1d 1h 1m");
    }

    #[test]
    fn test_format_duration_us() {
        assert_eq!(format_duration_us(500), "500µs");
        assert_eq!(format_duration_us(1500), "2ms");
        assert_eq!(format_duration_us(1_500_000), "1.5s");
    }

    #[test]
    fn test_usage_bar_width() {
        let bar = style::usage_bar(50.0, 40);
        assert!(bar.len() >= 40);
        assert!(bar.contains('█') || bar.contains('▓') || bar.contains('░'));
    }

    #[test]
    fn test_usage_bar_full() {
        let bar = style::usage_bar(100.0, 20);
        assert_eq!(bar.chars().filter(|&c| c == '█').count(), 20);
    }

    #[test]
    fn test_usage_bar_zero() {
        let bar = style::usage_bar(0.0, 20);
        assert_eq!(bar.chars().filter(|&c| c == '░').count(), 20);
    }

    #[test]
    fn test_read_meminfo_key_exists() {
        // Should always be available on Linux
        let total = read_meminfo("SwapTotal");
        assert!(total.is_some(), "SwapTotal should exist in /proc/meminfo");
        assert!(total.unwrap() > 0, "SwapTotal should be > 0");
    }

    #[test]
    fn test_read_vmstat_key_exists() {
        let pswpin = read_vmstat("pswpin");
        assert!(pswpin.is_some(), "pswpin should exist in /proc/vmstat");
    }

    #[test]
    fn test_collect_swap_info() {
        let info = collect_swap_info();
        assert!(info.total_kb > 0, "Swap total should be > 0");
        assert_eq!(info.total_kb, info.used_kb + info.free_kb,
            "Swap total should equal used + free (or nearly so — up to 4KB rounding)");
        // Allow 4KB rounding
        let diff = (info.total_kb as i64 - (info.used_kb as i64 + info.free_kb as i64)).unsigned_abs();
        assert!(diff <= 4, "total vs used+free diff ≤ 4KB");
    }

    #[test]
    fn test_collect_zram_info() {
        let zram = collect_zram_info();
        assert!(zram.is_some(), "ZRAM info should be available on this system");
        if let Some(z) = zram {
            assert_eq!(z.algorithm.len(), 4, "Algorithm should be 'zstd' (4 chars)");
            assert!(z.orig_bytes > z.comp_bytes, "Compressed size should be < original");
            assert!(z.initstate == 1, "ZRAM should be initialized");
        }
    }

    #[test]
    fn test_collect_swap_io() {
        let io = collect_swap_io();
        assert!(io.uptime_secs > 0.0, "Uptime should be > 0");
        // On a system with swap, these should be > 0
        assert!(io.pswpout > 0 || io.pswpin >= 0, "pswpout or pswpin should exist");
    }

    #[test]
    fn test_collect_top_swap_processes() {
        let procs = collect_top_swap_processes(5);
        // We may or may not have swap-hungry processes, but the list should
        // be sorted correctly
        for w in procs.windows(2) {
            assert!(w[0].swap_kb >= w[1].swap_kb, "Processes should be sorted by swap descending");
        }
    }

    #[test]
    fn test_collect_top_swap_limit() {
        let procs = collect_top_swap_processes(3);
        assert!(procs.len() <= 3, "Should return at most 3 processes");
    }
}
