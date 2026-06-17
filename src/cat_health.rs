use std::fs;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

/// ─── Health Score Types ───

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Severity {
    Critical, // 🔴
    Warning,  // 🟡
    Info,     // 🔵
    Good,     // 🟢
}

#[derive(Debug, Clone)]
pub struct HealthItem {
    pub subsystem: &'static str,
    pub score: u8,        // 0-100
    pub severity: Severity,
    pub summary: String,   // One-line status
    pub details: Vec<String>, // Bullet points with findings
}

#[derive(Debug, Default)]
pub struct HealthIssues {
    pub critical: Vec<String>,
    pub warnings: Vec<String>,
}

/// ─── /proc Readers ───

fn read_proc(path: &str) -> io::Result<String> {
    fs::read_to_string(path)
}

fn parse_key_value(data: &str) -> Vec<(&str, &str)> {
    data.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let mut parts = trimmed.splitn(2, ':');
            let key = parts.next()?.trim();
            let val = parts.next()?.trim();
            Some((key, val))
        })
        .collect()
}

fn parse_psi(path: &str) -> Option<(f64, f64, f64)> {
    let content = fs::read_to_string(path).ok()?;
    let line = content.lines().next()?;
    // Format: "some avg10=0.00 avg60=0.00 avg300=0.00 total=0"
    let parts: Vec<&str> = line.split_whitespace().collect();
    let avg10 = parts.get(1)?.split('=').nth(1)?.parse().ok()?;
    let avg60 = parts.get(2)?.split('=').nth(1)?.parse().ok()?;
    let avg300 = parts.get(3)?.split('=').nth(1)?.parse().ok()?;
    Some((avg10, avg60, avg300))
}

/// ─── Memory Health ───

fn assess_memory() -> HealthItem {
    let data = match read_proc("/proc/meminfo") {
        Ok(d) => d,
        Err(_) => return HealthItem {
            subsystem: "Memory",
            score: 0,
            severity: Severity::Critical,
            summary: "Cannot read /proc/meminfo".into(),
            details: vec![],
        },
    };

    let kv = parse_key_value(&data);
    let get_val = |prefix: &str| -> Option<u64> {
        kv.iter().find(|(k, _)| k.starts_with(prefix))?
            .1.split_whitespace().next()?.parse().ok()
    };

    let total = get_val("MemTotal").unwrap_or(1);
    let available = get_val("MemAvailable").unwrap_or(0);
    let buffers = get_val("Buffers").unwrap_or(0);
    let cached = get_val("Cached").unwrap_or(0);
    let swap_total = get_val("SwapTotal").unwrap_or(0);
    let swap_free = get_val("SwapFree").unwrap_or(0);
    let swap_cached = get_val("SwapCached").unwrap_or(0);
    let dirty = get_val("Dirty").unwrap_or(0);
    let anon_pages = get_val("AnonPages").unwrap_or(0);
    let mapped = get_val("Mapped").unwrap_or(0);
    let shmem = get_val("Shmem").unwrap_or(0);
    let slab = get_val("Slab").unwrap_or(0);
    let page_tables = get_val("PageTables").unwrap_or(0);
    let committed = get_val("Committed_AS").unwrap_or(0);
    let commit_limit = get_val("CommitLimit").unwrap_or(1);

    let avail_pct = available as f64 / total as f64 * 100.0;
    let swap_used = swap_total.saturating_sub(swap_free);
    let swap_pct = if swap_total > 0 {
        swap_used as f64 / swap_total as f64 * 100.0
    } else {
        0.0
    };
    let commit_pct = committed as f64 / commit_limit.max(1) as f64 * 100.0;
    let dirty_pct = dirty as f64 / total as f64 * 100.0;

    // Score computation
    let mut score = 100u8;

    // Available memory penalty
    if avail_pct < 5.0 {
        score = score.saturating_sub(40);
    } else if avail_pct < 10.0 {
        score = score.saturating_sub(25);
    } else if avail_pct < 20.0 {
        score = score.saturating_sub(10);
    }

    // Swap penalty
    if swap_total > 0 {
        if swap_pct > 95.0 {
            score = score.saturating_sub(25);
        } else if swap_pct > 80.0 {
            score = score.saturating_sub(15);
        } else if swap_pct > 50.0 {
            score = score.saturating_sub(5);
        }
    }

    // Commit ratio
    if commit_pct > 95.0 {
        score = score.saturating_sub(20);
    } else if commit_pct > 80.0 {
        score = score.saturating_sub(10);
    }

    // Dirty pages
    if dirty_pct > 10.0 {
        score = score.saturating_sub(10);
    } else if dirty_pct > 5.0 {
        score = score.saturating_sub(5);
    }

    let severity = if score < 30 { Severity::Critical }
    else if score < 60 { Severity::Warning }
    else if score < 80 { Severity::Info }
    else { Severity::Good };

    let summary = format!(
        "{} available / {} total ({:.1}%), swap {}/{} ({:.1}%)",
        human_size(available * 1024), human_size(total * 1024), avail_pct,
        if swap_total > 0 { human_size(swap_used * 1024) } else { "none".into() },
        if swap_total > 0 { human_size(swap_total * 1024) } else { "".into() },
        swap_pct,
    );

    let mut details = Vec::new();
    details.push(format!(
        "Buffers: {} | Cached: {} | Anon: {} | Slab: {}",
        human_size(buffers * 1024), human_size(cached * 1024),
        human_size(anon_pages * 1024), human_size(slab * 1024)
    ));
    details.push(format!(
        "Committed AS: {:.1}% of limit | Dirty: {} | Mapped: {}",
        commit_pct, human_size(dirty * 1024), human_size(mapped * 1024)
    ));
    if shmem > 0 {
        details.push(format!("Shmem: {} | PageTables: {}", human_size(shmem * 1024), human_size(page_tables * 1024)));
    }
    if swap_total > 0 && swap_pct > 80.0 {
        details.push(format!("⚠️  Swap nearly exhausted — OOM risk if memory pressure continues"));
    }
    if swap_total > 0 && swap_cached > 0 {
        details.push(format!("SwapCached: {} (pages swapped back but still in swap)", human_size(swap_cached * 1024)));
    }

    HealthItem { subsystem: "Memory", score, severity, summary, details }
}

/// ─── CPU Health ───

fn assess_cpu() -> HealthItem {
    let load_data = match read_proc("/proc/loadavg") {
        Ok(d) => d,
        Err(_) => return HealthItem {
            subsystem: "CPU",
            score: 0,
            severity: Severity::Critical,
            summary: "Cannot read /proc/loadavg".into(),
            details: vec![],
        },
    };

    // Read CPU count
    let cpu_count = match read_proc("/proc/cpuinfo") {
        Ok(d) => d.lines().filter(|l| l.starts_with("processor")).count().max(1),
        Err(_) => 1,
    };

    let parts: Vec<&str> = load_data.split_whitespace().collect();
    let load1: f64 = parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let load5: f64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let load15: f64 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let running: u32 = parts.get(3).and_then(|s| s.split('/').next().and_then(|x| x.parse().ok())).unwrap_or(0);
    let procs_total: u32 = parts.get(3).and_then(|s| s.split('/').nth(1).and_then(|x| x.parse().ok())).unwrap_or(0);

    // Read /proc/stat for context switches
    let ctx_switches = read_proc("/proc/stat")
        .ok()
        .and_then(|d| d.lines().find(|l| l.starts_with("ctxt")).map(|l| l.to_string()))
        .and_then(|l| {
            let parts: Vec<&str> = l.split_whitespace().collect();
            parts.get(1).and_then(|s| s.parse::<u64>().ok())
        })
        .unwrap_or(0);

    let procs_running = running;

    let load_ratio = load1 / cpu_count as f64;
    let mut score = 100u8;

    if load_ratio > 2.0 {
        score = score.saturating_sub(40);
    } else if load_ratio > 1.5 {
        score = score.saturating_sub(25);
    } else if load_ratio > 1.0 {
        score = score.saturating_sub(15);
    } else if load_ratio > 0.7 {
        score = score.saturating_sub(5);
    }

    if procs_running > (cpu_count as u32) * 4 {
        score = score.saturating_sub(10);
    }

    // Score deduction if running processes exceed 3x cpu count
    if procs_running > (cpu_count as u32) * 3 && procs_running > 20 {
        score = score.saturating_sub(10);
    }

    let severity = if score < 30 { Severity::Critical }
    else if score < 60 { Severity::Warning }
    else if score < 80 { Severity::Info }
    else { Severity::Good };

    let summary = format!(
        "load {:.2}/{:.2}/{:.2} ({} cores), {} running, {} total",
        load1, load5, load15, cpu_count, procs_running, procs_total
    );

    let mut details = Vec::new();
    details.push(format!("Context switches: {} | Total processes: {}", ctx_switches, procs_total));
    if load_ratio > 1.0 {
        details.push(format!("⚠️  Load exceeds CPU count — processes competing for CPU time"));
    }

    HealthItem { subsystem: "CPU", score, severity, summary, details }
}

/// ─── Pressure Stall Information (PSI) ───

fn assess_pressure() -> HealthItem {
    let mem_psi = parse_psi("/proc/pressure/memory");
    let cpu_psi = parse_psi("/proc/pressure/cpu");
    let io_psi = parse_psi("/proc/pressure/io");

    if mem_psi.is_none() && cpu_psi.is_none() && io_psi.is_none() {
        return HealthItem {
            subsystem: "Pressure",
            score: 100,
            severity: Severity::Good,
            summary: "No PSI data (kernel < 4.20 or CONFIG_PSI disabled)".into(),
            details: vec![],
        };
    }

    let mut score = 100u8;
    let mut details = Vec::new();

    // Memory PSI scoring
    if let Some((avg10, avg60, avg300)) = mem_psi {
        let max = avg10.max(avg60).max(avg300);
        if max > 10.0 {
            score = score.saturating_sub(40);
        } else if max > 5.0 {
            score = score.saturating_sub(25);
        } else if max > 1.0 {
            score = score.saturating_sub(10);
        } else if max > 0.1 {
            score = score.saturating_sub(3);
        }
        details.push(format!(
            "Memory PSI: some avg10={:.1}% avg60={:.1}% avg300={:.1}%",
            avg10, avg60, avg300
        ));
    }

    // IO PSI scoring
    if let Some((avg10, avg60, avg300)) = io_psi {
        let max = avg10.max(avg60).max(avg300);
        if max > 10.0 {
            score = score.saturating_sub(30);
        } else if max > 5.0 {
            score = score.saturating_sub(15);
        } else if max > 1.0 {
            score = score.saturating_sub(5);
        }
        details.push(format!(
            "I/O PSI:      some avg10={:.1}% avg60={:.1}% avg300={:.1}%",
            avg10, avg60, avg300
        ));
    }

    // CPU PSI — usually very low unless overcommitted
    if let Some((avg10, avg60, avg300)) = cpu_psi {
        if avg10 > 10.0 {
            score = score.saturating_sub(15);
        } else if avg10 > 5.0 {
            score = score.saturating_sub(5);
        }
        details.push(format!(
            "CPU PSI:      some avg10={:.1}% avg60={:.1}% avg300={:.1}%",
            avg10, avg60, avg300
        ));
    }

    let severity = if score < 30 { Severity::Critical }
    else if score < 60 { Severity::Warning }
    else if score < 80 { Severity::Info }
    else { Severity::Good };

    let (mem_max, io_max) = (
        mem_psi.map(|(a10, a60, a300)| a10.max(a60).max(a300)).unwrap_or(0.0),
        io_psi.map(|(a10, a60, a300)| a10.max(a60).max(a300)).unwrap_or(0.0),
    );
    let peak = mem_max.max(io_max);
    let summary = if peak > 5.0 {
        format!("⚠️  Significant stall pressure detected (peak {:.1}%)", peak)
    } else if peak > 1.0 {
        format!("Moderate stall pressure (peak {:.1}%)", peak)
    } else if peak > 0.1 {
        format!("Mild stall pressure (peak {:.1}%)", peak)
    } else {
        "No significant stall pressure".into()
    };

    HealthItem { subsystem: "Pressure", score, severity, summary, details }
}

/// ─── Disk / I/O Health ───

fn assess_disk() -> HealthItem {
    // Check disk I/O via /proc/diskstats
    let data = match read_proc("/proc/diskstats") {
        Ok(d) => d,
        Err(_) => return HealthItem {
            subsystem: "Disk",
            score: 100,
            severity: Severity::Good,
            summary: "Cannot read /proc/diskstats".into(),
            details: vec![],
        },
    };

    // Aggregate I/O time across all physical disks
    let mut total_io_time = 0u64;
    let mut total_weighted_time = 0u64;
    let mut total_reads = 0u64;
    let mut total_writes = 0u64;
    let mut disk_count = 0u32;

    for line in data.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 14 { continue; }
        let name = fields.get(2).unwrap_or(&"");
        // Skip partitions (contain digits) and ram/NVMe namespaces, keep main devices
        let is_main = name.chars().all(|c| c.is_ascii_alphabetic());
        if !is_main { continue; }
        // Skip loop, ram, dm (mapper) devices
        if name.starts_with("loop") || name.starts_with("ram") || name.starts_with("dm-") { continue; }

        let reads_completed: u64 = fields.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
        let writes_completed: u64 = fields.get(7).and_then(|s| s.parse().ok()).unwrap_or(0);
        let io_time: u64 = fields.get(12).and_then(|s| s.parse().ok()).unwrap_or(0);
        let weighted_time: u64 = fields.get(13).and_then(|s| s.parse().ok()).unwrap_or(0);

        total_io_time += io_time;
        total_weighted_time += weighted_time;
        total_reads += reads_completed;
        total_writes += writes_completed;
        disk_count += 1;
    }

    let mut score = 100u8;

    // I/O time scoring: high weighted I/O time indicates congestion
    if total_weighted_time > 10_000_000 {
        score = score.saturating_sub(30);
    } else if total_weighted_time > 1_000_000 {
        score = score.saturating_sub(15);
    } else if total_weighted_time > 100_000 {
        score = score.saturating_sub(5);
    }

    let severity = if score < 30 { Severity::Critical }
    else if score < 60 { Severity::Warning }
    else if score < 80 { Severity::Info }
    else { Severity::Good };

    let summary = format!(
        "{} disk(s): {}M reads, {}M writes, {:.0}s weighted I/O time",
        disk_count,
        total_reads / 1_000_000,
        total_writes / 1_000_000,
        total_weighted_time as f64 / 1000.0
    );

    let mut details: Vec<String> = Vec::new();
    details.push(format!(
        "Total I/O time: {}ms | Weighted: {}ms",
        total_io_time, total_weighted_time
    ));

    HealthItem { subsystem: "Disk", score, severity, summary, details }
}

/// ─── System / Uptime Health ───

fn assess_system() -> HealthItem {
    let mut score = 100u8;
    let mut findings: Vec<String> = Vec::new();

    // Uptime
    let uptime_info = read_proc("/proc/uptime").ok()
        .and_then(|d| d.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()))
        .unwrap_or(0.0);
    let uptime_days = uptime_info / 86400.0;

    // Process count
    let proc_count = match fs::read_dir("/proc") {
        Ok(entries) => entries.filter_map(|e| {
            e.ok().and_then(|e| e.file_name().to_str().map(|s| s.to_string()))
        }).filter(|s| s.chars().all(|c| c.is_ascii_digit()))
        .count(),
        Err(_) => 0,
    };

    // File descriptor usage
    let fd_count = read_proc("/proc/sys/fs/file-nr").ok()
        .and_then(|d| {
            let parts: Vec<&str> = d.split_whitespace().collect();
            let allocated: u64 = parts.get(0).and_then(|s| s.parse().ok())?;
            let max: u64 = parts.get(2).and_then(|s| s.parse().ok())?;
            Some((allocated, max))
        });

    let mut details = Vec::new();
    details.push(format!("Uptime: {:.0} days {:.0} hours", uptime_days, (uptime_info % 86400.0) / 3600.0));
    details.push(format!("Processes: {}", proc_count));

    // Entropy
    if let Ok(entropy) = read_proc("/proc/sys/kernel/random/entropy_avail") {
        if let Ok(val) = entropy.trim().parse::<u32>() {
            details.push(format!("Entropy: {}", val));
            if val < 100 {
                score = score.saturating_sub(5);
                findings.push("Low entropy pool".into());
            }
        }
    }

    if let Some((alloc, max)) = fd_count {
        let fd_pct = alloc as f64 / max as f64 * 100.0;
        details.push(format!("File descriptors: {}/{} ({:.1}%)", alloc, max, fd_pct));
        if fd_pct > 80.0 {
            score = score.saturating_sub(10);
            findings.push("File descriptor usage high".into());
        }
    }

    let severity = if score < 30 { Severity::Critical }
    else if score < 60 { Severity::Warning }
    else if score < 80 { Severity::Info }
    else { Severity::Good };

    let summary = format!(
        "Up {:.0}d {:.0}h, {} processes{}",
        uptime_days,
        (uptime_info % 86400.0) / 3600.0,
        proc_count,
        if findings.is_empty() { "".into() }
        else { format!(" — {}", findings.join(", ")) }
    );

    HealthItem { subsystem: "System", score, severity, summary, details }
}

/// ─── Network Health ───

fn assess_network() -> HealthItem {
    // Check /proc/net/dev for interface errors
    let data = match read_proc("/proc/net/dev") {
        Ok(d) => d,
        Err(_) => return HealthItem {
            subsystem: "Network",
            score: 100,
            severity: Severity::Good,
            summary: "Cannot read /proc/net/dev".into(),
            details: vec![],
        },
    };

    let mut total_errors = 0u64;
    let mut total_drops = 0u64;
    let mut total_rx = 0u64;
    let mut total_tx = 0u64;
    let mut active_interfaces = 0u32;
    let mut details = Vec::new();

    for line in data.lines().skip(2) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 17 { continue; }
        let name = parts[0].trim_matches(':');
        // Skip loopback
        if name == "lo" { continue; }

        let rx_bytes: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let rx_errors: u64 = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
        let rx_drop: u64 = parts.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
        let tx_bytes: u64 = parts.get(9).and_then(|s| s.parse().ok()).unwrap_or(0);
        let tx_errors: u64 = parts.get(11).and_then(|s| s.parse().ok()).unwrap_or(0);
        let tx_drop: u64 = parts.get(12).and_then(|s| s.parse().ok()).unwrap_or(0);

        total_rx += rx_bytes;
        total_tx += tx_bytes;
        total_errors += rx_errors + tx_errors;
        total_drops += rx_drop + tx_drop;

        if rx_bytes > 0 || tx_bytes > 0 {
            active_interfaces += 1;
        }

        if rx_errors > 0 || tx_errors > 0 || rx_drop > 0 || tx_drop > 0 {
            details.push(format!(
                "{}: RX err={} drop={} TX err={} drop={}",
                name, rx_errors, rx_drop, tx_errors, tx_drop
            ));
        }
    }

    let mut score = 100u8;
    if total_errors > 1000 {
        score = score.saturating_sub(20);
    } else if total_errors > 100 {
        score = score.saturating_sub(10);
    } else if total_errors > 10 {
        score = score.saturating_sub(5);
    }
    if total_drops > 10000 {
        score = score.saturating_sub(15);
    } else if total_drops > 1000 {
        score = score.saturating_sub(5);
    }

    let severity = if score < 30 { Severity::Critical }
    else if score < 60 { Severity::Warning }
    else if score < 80 { Severity::Info }
    else { Severity::Good };

    let summary = format!(
        "{} active interfaces, RX {} TX {}",
        active_interfaces, human_size(total_rx), human_size(total_tx)
    );

    if details.is_empty() && active_interfaces > 0 {
        details.push("No errors or drops on active interfaces".into());
    }
    if active_interfaces == 0 {
        details.push("No active non-loopback interfaces".into());
    }

    HealthItem { subsystem: "Network", score, severity, summary, details }
}

/// ─── ZRAM Health (if available) ───

fn assess_zram() -> Option<HealthItem> {
    let zram_dir = "/sys/block";
    let mut zram_found = false;
    let mut total_orig = 0u64;
    let mut total_comp = 0u64;
    let mut total_limit = 0u64;
    let mut algorithms = Vec::new();

    let entries = match fs::read_dir(zram_dir) {
        Ok(e) => e,
        Err(_) => return None,
    };

    for entry in entries {
        let entry = match entry { Ok(e) => e, _ => continue };
        let name = entry.file_name();
        let name = match name.to_str() { Some(n) => n, None => continue };
        if !name.starts_with("zram") { continue; }
        zram_found = true;

        let base = format!("{}/{}", zram_dir, name);
        if let Ok(orig) = fs::read_to_string(format!("{}/orig_data_size", base))
            .and_then(|s| s.trim().parse::<u64>().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "")))
        {
            total_orig += orig;
        }
        if let Ok(comp) = fs::read_to_string(format!("{}/compr_data_size", base))
            .and_then(|s| s.trim().parse::<u64>().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "")))
        {
            total_comp += comp;
        }
        if let Ok(limit) = fs::read_to_string(format!("{}/limit", base))
            .and_then(|s| s.trim().parse::<u64>().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "")))
        {
            total_limit += limit;
        }
        if let Ok(alg) = fs::read_to_string(format!("{}/comp_algorithm", base)) {
            algorithms.push(alg.trim().to_string());
        }
    }

    if !zram_found {
        return None;
    }

    let ratio = if total_comp > 0 {
        total_orig as f64 / total_comp as f64
    } else { 1.0 };

    let mut score = 100u8;
    if ratio < 1.5 {
        score = score.saturating_sub(15); // Poor compression
    }
    if total_limit > 0 {
        let used_pct = total_orig as f64 / total_limit as f64 * 100.0;
        if used_pct > 90.0 {
            score = score.saturating_sub(20);
        } else if used_pct > 75.0 {
            score = score.saturating_sub(10);
        }
    }

    let severity = if score < 30 { Severity::Critical }
    else if score < 60 { Severity::Warning }
    else if score < 80 { Severity::Info }
    else { Severity::Good };

    let algo_str = algorithms.join(", ");
    let summary = format!(
        "{:.1}x compression ({})",
        ratio, algo_str
    );

    let mut details = Vec::new();
    details.push(format!(
        "Original: {} → Compressed: {} (ratio {:.2}x)",
        human_size(total_orig), human_size(total_comp), ratio
    ));
    if total_limit > 0 {
        let used_pct = total_orig as f64 / total_limit as f64 * 100.0;
        details.push(format!("Usage: {:.1}% of {} limit", used_pct, human_size(total_limit)));
    }
    if ratio < 1.5 {
        details.push("⚠️  Poor compression ratio — consider switching algorithm".into());
    }

    Some(HealthItem { subsystem: "ZRAM", score, severity, summary, details })
}

/// ─── Overall Reporter ───

fn color_severity(sev: Severity) -> &'static str {
    match sev {
        Severity::Critical => "\x1b[1;31m", // bold red
        Severity::Warning  => "\x1b[1;33m", // bold yellow
        Severity::Info     => "\x1b[1;34m", // bold blue
        Severity::Good     => "\x1b[1;32m", // bold green
    }
}

fn sev_icon(sev: Severity) -> &'static str {
    match sev {
        Severity::Critical => "🔴",
        Severity::Warning  => "🟡",
        Severity::Info     => "🔵",
        Severity::Good     => "🟢",
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{:.0} {}", bytes, UNITS[unit_idx])
    } else if size >= 100.0 {
        format!("{:.0} {}", size, UNITS[unit_idx])
    } else if size >= 10.0 {
        format!("{:.1} {}", size, UNITS[unit_idx])
    } else {
        format!("{:.2} {}", size, UNITS[unit_idx])
    }
}

/// Gather all health assessments and return them with overall score.
fn collect_health() -> (Vec<HealthItem>, HealthIssues) {
    let mut items = Vec::new();
    let mut issues = HealthIssues::default();

    let assessments: Vec<HealthItem> = vec![
        assess_memory(),
        assess_cpu(),
        assess_pressure(),
        assess_disk(),
        assess_system(),
        assess_network(),
    ];

    for item in assessments {
        let severity = item.severity;
        items.push(item.clone());

        let prefix = format!("{}: ", item.subsystem);
        match severity {
            Severity::Critical => {
                issues.critical.push(format!("{}{} ({}/100)", prefix, item.summary, item.score));
                // Also add details as critical bullets
                for d in &item.details {
                    if d.starts_with("⚠️") || d.contains("OOM") || d.contains("exhausted") || d.contains("contention") {
                        issues.critical.push(format!("  {}", d));
                    }
                }
            }
            Severity::Warning => {
                issues.warnings.push(format!("{}{} ({}/100)", prefix, item.summary, item.score));
                for d in &item.details {
                    if d.starts_with("⚠️") {
                        issues.warnings.push(format!("  {}", d));
                    }
                }
            }
            _ => {}
        }
    }

    // ZRAM is optional — append if available
    if let Some(zram) = assess_zram() {
        if zram.severity == Severity::Warning || zram.severity == Severity::Critical {
            issues.warnings.push(format!("ZRAM: {} ({}/100)", zram.summary, zram.score));
        }
        items.push(zram);
    }

    (items, issues)
}

fn overall_score(items: &[HealthItem]) -> u8 {
    // Weighted scoring
    let weights: [(&str, f64); 6] = [
        ("Memory",   0.30),
        ("CPU",      0.20),
        ("Pressure", 0.20),
        ("Disk",     0.10),
        ("System",   0.10),
        ("Network",  0.10),
    ];

    let mut total = 0.0;
    for (name, weight) in &weights {
        if let Some(item) = items.iter().find(|i| i.subsystem == *name) {
            total += item.score as f64 * weight;
        }
    }
    total.round().min(100.0).max(0.0) as u8
}

/// ─── Public Entry Point ───

pub fn cat_health() {
    let (items, issues) = collect_health();
    let overall = overall_score(&items);

    let reset = "\x1b[0m";
    let dim = "\x1b[2m";
    let bold = "\x1b[1m";

    // ── Header ──
    println!();
    println!("  {}═══ System Health Report ═══{}", bold, reset);
    println!();

    // ── Overall score ──
    let sev = if overall < 30 { Severity::Critical }
    else if overall < 60 { Severity::Warning }
    else if overall < 80 { Severity::Info }
    else { Severity::Good };

    let sev_label = match sev {
        Severity::Critical => "CRITICAL",
        Severity::Warning  => "WARNING",
        Severity::Info     => "FAIR",
        Severity::Good     => "GOOD",
    };

    println!("  {}Overall:{}{} {:>3}/100{}  {}{}",
        bold, reset, color_severity(sev), overall, reset,
        sev_icon(sev), sev_label
    );
    println!();

    // ── Per-subsystem score bars ──
    for item in &items {
        let bar_len = 20usize;
        let filled = ((item.score as f64 / 100.0) * bar_len as f64).round() as usize;
        let empty = bar_len.saturating_sub(filled);
        let bar = format!(
            "{}{}{}{}{}",
            color_severity(item.severity),
            "█".repeat(filled),
            reset,
            dim,
            "░".repeat(empty),
        );

        println!("  {} {:>8} |{}{}| {:>3}/100  {}  {}",
            reset,
            item.subsystem,
            bar,
            reset,
            item.score,
            sev_icon(item.severity),
            item.summary
        );
    }
    println!();

    // ── Issues section ──
    if !issues.critical.is_empty() || !issues.warnings.is_empty() {
        println!("  {}── Issues ──{}", bold, reset);
        println!();

        if !issues.critical.is_empty() {
            for issue in &issues.critical {
                println!("  {}🔴 {}{}", bold, reset, issue);
            }
            println!();
        }

        if !issues.warnings.is_empty() {
            for issue in &issues.warnings {
                println!("  {}🟡 {}{}", dim, reset, issue);
            }
            println!();
        }
    } else {
        println!("  {}✅ All subsystems healthy{}", color_severity(Severity::Good), reset);
        println!();
    }

    // ── Detailed breakdown ──
    println!("  {}── Details ──{}", bold, reset);
    println!();

    for item in &items {
        if item.details.is_empty() { continue; }
        println!("  {}{}:{}", color_severity(item.severity), item.subsystem, reset);
        for detail in &item.details {
            println!("    {}{}{}", dim, detail, reset);
        }
        println!();
    }

    // ── Footer with timestamp ──
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("  {}Report generated at timestamp {} | ccat --health{}", dim, now, reset);
    println!();
}
