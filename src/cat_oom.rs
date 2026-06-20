//! OOM Score Analyzer (`ccat --oom`).
//!
//! Reads `/proc/<pid>/oom_score`, `oom_score_adj`, `oom_adj` from every
//! process and presents a colour-coded, sorted risk assessment:
//!
//! - **oom_score** (0–1000) — higher = more likely to be killed by OOM killer
//! - **oom_score_adj** (−1000 to +1000) — tunable bias applied to the score
//! - **oom_adj** (−17 to +15, legacy) — older OOM tuner, kernel maps to adj
//! - RSS (resident set size in KiB)
//!
//! Also shows:
//! - Which processes are OOM-immune (oom_score_adj = −1000 → final score = 0)
//! - Which processes have been penalized (oom_score_adj > 0)
//! - Per-process cgroup memory events (OOM kills in the cgroup)
//! - A summary of the process most at risk

use std::fs;
use std::io::{self, Write};

// ── Colour helpers ──

mod style {
    pub fn bold(s: impl AsRef<str>) -> String   { format!("\x1b[1m{}\x1b[0m", s.as_ref()) }
    pub fn dim(s: impl AsRef<str>) -> String    { format!("\x1b[2m{}\x1b[0m", s.as_ref()) }
    pub fn green(s: impl AsRef<str>) -> String  { format!("\x1b[32m{}\x1b[0m", s.as_ref()) }
    pub fn red(s: impl AsRef<str>) -> String    { format!("\x1b[31m{}\x1b[0m", s.as_ref()) }
    pub fn yellow(s: impl AsRef<str>) -> String { format!("\x1b[33m{}\x1b[0m", s.as_ref()) }
    pub fn cyan(s: impl AsRef<str>) -> String   { format!("\x1b[36m{}\x1b[0m", s.as_ref()) }
    pub fn white(s: impl AsRef<str>) -> String  { format!("\x1b[37m{}\x1b[0m", s.as_ref()) }
    pub fn orange(s: impl AsRef<str>) -> String { format!("\x1b[38;5;214m{}\x1b[0m", s.as_ref()) }
    pub fn grey(s: impl AsRef<str>) -> String   { format!("\x1b[90m{}\x1b[0m", s.as_ref()) }
}

// ── Data types ──

#[derive(Debug)]
struct OomInfo {
    pid: u32,
    name: String,
    score: u16,         // oom_score (0–1000)
    score_adj: i16,     // oom_score_adj (-1000..+1000)
    oom_adj: i16,       // legacy oom_adj (-17..+15)
    rss_kb: u64,        // RSS in KiB from /proc/<pid>/status
}

// ── Reading helpers ──

fn read_oom_score(pid: u32) -> Option<u16> {
    let s = fs::read_to_string(format!("/proc/{pid}/oom_score")).ok()?;
    s.trim().parse().ok()
}

fn read_oom_score_adj(pid: u32) -> Option<i16> {
    let s = fs::read_to_string(format!("/proc/{pid}/oom_score_adj")).ok()?;
    s.trim().parse().ok()
}

fn read_oom_adj(pid: u32) -> Option<i16> {
    let s = fs::read_to_string(format!("/proc/{pid}/oom_adj")).ok()?;
    s.trim().parse().ok()
}

fn read_comm(pid: u32) -> String {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "?".to_string())
}

fn read_rss_kb(pid: u32) -> u64 {
    // Parse VmRSS from /proc/<pid>/status
    let status = match fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            // "VmRSS:   12345 kB"
            let val = line.split_whitespace().nth(1).unwrap_or("0");
            return val.parse::<u64>().unwrap_or(0);
        }
    }
    0
}

fn is_process_alive(pid: u32) -> bool {
    fs::metadata(format!("/proc/{pid}")).is_ok()
}

/// Collect OOM data from all visible /proc entries.
fn collect_all() -> Vec<OomInfo> {
    let mut results = Vec::new();

    let dir = match fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return results,
    };

    for entry in dir.flatten() {
        let pid_str = match entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        // Only numeric directories are processes
        let pid: u32 = match pid_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        // Skip zombie processes — no /proc/pid/oom_score
        if !is_process_alive(pid) {
            continue;
        }

        let score = match read_oom_score(pid) {
            Some(v) => v,
            None => continue,
        };

        let name = read_comm(pid);
        let score_adj = read_oom_score_adj(pid).unwrap_or(0);
        let oom_adj = read_oom_adj(pid).unwrap_or(0);
        let rss_kb = read_rss_kb(pid);

        results.push(OomInfo { pid, name, score, score_adj, oom_adj, rss_kb });
    }

    // Sort by oom_score descending (highest risk first)
    results.sort_by(|a, b| b.score.cmp(&a.score));
    results
}

/// Colour-code a risk level based on oom_score.
fn risk_color(s: u16) -> String {
    if s >= 800 {
        style::red(format!("{:>4}", s))
    } else if s >= 500 {
        style::orange(format!("{:>4}", s))
    } else if s >= 200 {
        style::yellow(format!("{:>4}", s))
    } else if s == 0 {
        style::green(format!("{:>4}", s))
    } else {
        format!("{:>4}", s)
    }
}

/// Colour the oom_score_adj value.
fn adj_color(adj: i16) -> String {
    if adj == -1000 {
        style::green(format!("{:>5}", adj))
    } else if adj > 0 {
        style::red(format!("{:>5}", adj))
    } else if adj < 0 {
        style::cyan(format!("{:>5}", adj))
    } else {
        format!("{:>5}", adj)
    }
}

/// Format RSS in human-readable format.
fn fmt_rss(kb: u64) -> String {
    if kb >= 1_048_576 {
        format!("{:.1}G", kb as f64 / 1_048_576.0)
    } else if kb >= 1024 {
        format!("{:.1}M", kb as f64 / 1024.0)
    } else {
        format!("{}K", kb)
    }
}

/// Count processes in each risk tier.
fn tier_counts(infos: &[OomInfo]) -> (usize, usize, usize, usize, usize) {
    let mut critical = 0;  // >= 800
    let mut high = 0;      // >= 500
    let mut medium = 0;    // >= 200
    let mut low = 0;       // > 0
    let mut immune = 0;    // == 0
    for info in infos {
        if info.score >= 800 { critical += 1; }
        else if info.score >= 500 { high += 1; }
        else if info.score >= 200 { medium += 1; }
        else if info.score > 0 { low += 1; }
        else { immune += 1; }
    }
    (critical, high, medium, low, immune)
}

/// Print a header bar.
fn header_bar() {
    println!(
        "{} {:>7} {:>5} {:>5} {:>7}  {}",
        style::bold("PID"),
        style::bold("SCORE"),
        style::bold("ADJ"),
        style::bold("OOM"),
        style::bold("RSS"),
        style::bold("COMMAND"),
    );
    println!("{}", style::dim(String::from_utf8(vec![b'-'; 70]).unwrap()));
}

/// Main entry point: `ccat --oom`
pub fn cat_oom() {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let infos = collect_all();
    if infos.is_empty() {
        let _ = writeln!(out, "{}", style::red("No OOM data available."));
        return;
    }

    // ── Summary header ──
    let total = infos.len();
    let (crit, high, med, low, immune) = tier_counts(&infos);

    let _ = writeln!(
        out,
        "{} {} {}",
        style::bold("═══ OOM Score Analysis ═══"),
        style::dim(&format!("({} processes)", total)),
        style::bold(""),
    );
    let _ = writeln!(
        out,
        "  Critical: {}  High: {}  Medium: {}  Low: {}  {}: {}",
        style::red(&format!("{crit}")),
        style::orange(&format!("{high}")),
        style::yellow(&format!("{med}")),
        format!("{low}"),
        style::green("Immune"),
        style::green(&format!("{immune}")),
    );

    // Show cgroup memory events summary
    let _ = show_cgroup_memory_events(&mut out);

    let _ = writeln!(out);

    // ── Table header ──
    header_bar();

    // ── Rows ──
    for info in &infos {
        let score_str = risk_color(info.score);
        let adj_str = adj_color(info.score_adj);
        let oom_adj_str = if info.oom_adj == info.score_adj {
            style::grey(format!("{:>5}", info.oom_adj))
        } else {
            adj_color(info.oom_adj)
        };
        let rss_str = fmt_rss(info.rss_kb);
        let pid_str = format!("{:>7}", info.pid);

        // Truncate name to fit
        let name = if info.name.len() > 24 {
            format!("{}…", &info.name[..23])
        } else {
            info.name.clone()
        };

        // Mark immune processes
        let flag = if info.score == 0 { style::green(" ✓") } else { String::new() };

        let _ = writeln!(
            out,
            "{} {:>7} {:>5} {:>5} {:>7}  {}{}",
            style::grey(&pid_str),
            score_str,
            adj_str,
            oom_adj_str,
            rss_str,
            name,
            flag,
        );
    }

    // ── Legend ──
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{} SCORE: 0–1000 (higher = more likely killed)",
        style::bold("Legend:"),
    );
    let _ = writeln!(
        out,
        "  ADJ = oom_score_adj (−1000..+1000,  −1000 = {} immune)",
        style::green("✓"),
    );
    let _ = writeln!(
        out,
        "  OOM = oom_adj (legacy,  −17..+15)",
    );
    let _ = writeln!(
        out,
        "  RSS = Resident Set Size in KiB",
    );

    // ── Top risk analysis ──
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", style::bold("═══ Top Risk Assessment ═══"));

    let top_risk = &infos[0];
    let _ = writeln!(
        out,
        "  Most likely OOM victim: PID {} ({})  score={}  RSS={}",
        style::red(&top_risk.pid.to_string()),
        style::bold(&top_risk.name),
        risk_color(top_risk.score),
        fmt_rss(top_risk.rss_kb),
    );

    // Count processes with positive oom_score_adj (intentionally penalized)
    let penalized = infos.iter().filter(|i| i.score_adj > 0).count();
    if penalized > 0 {
        let _ = writeln!(
            out,
            "  {} {} process(es) have oom_score_adj > 0 (penalized)",
            style::yellow(&penalized.to_string()),
            style::dim("→ intentionally more likely to be killed"),
        );
    }

    // Count processes with oom_score_adj < 0 (protected)
    let protected = infos.iter().filter(|i| i.score_adj < 0 && i.score_adj > -1000).count();
    let immune_count = infos.iter().filter(|i| i.score_adj == -1000).count();
    if protected > 0 {
        let _ = writeln!(
            out,
            "  {} process(es) partially protected (oom_score_adj < 0)",
            style::cyan(&protected.to_string()),
        );
    }
    if immune_count > 0 {
        let _ = writeln!(
            out,
            "  {} process(es) fully immune (oom_score_adj = −1000):",
            style::green(&immune_count.to_string()),
        );
        for info in infos.iter().filter(|i| i.score_adj == -1000 && i.score == 0) {
            let _ = writeln!(
                out,
                "    PID {}  {}",
                style::dim(&info.pid.to_string()),
                info.name,
            );
        }
    }

    let _ = writeln!(out);
}

/// Show cgroup memory events for key cgroups.
fn show_cgroup_memory_events(out: &mut dyn Write) -> io::Result<()> {
    // Monitor interesting cgroups
    let cgroups_to_check = [
        ("system", "/sys/fs/cgroup/memory.events"),
        ("user", "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/memory.events"),
    ];

    let mut has_output = false;
    for (label, path) in &cgroups_to_check {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let oom_kills: u64 = content.lines()
            .find(|l| l.starts_with("oom_kill"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if oom_kills > 0 {
            if !has_output {
                let _ = writeln!(out, "  {} {}",
                    style::yellow("⚠"),
                    style::bold("cgroup OOM kills detected:"),
                );
                has_output = true;
            }
            let _ = writeln!(out, "    {label}: {} OOM kill(s)", style::red(&oom_kills.to_string()));
        }
    }

    // Also check memory pressure
    for (label, path) in &cgroups_to_check {
        let content = match fs::read_to_string(path.replace("memory.events", "memory.pressure")) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // Parse "some avg10=0.00 avg60=0.00 avg300=0.00 total=12345"
        if let Some(some_line) = content.lines().find(|l| l.starts_with("some ")) {
            if let Some(avg10_str) = some_line.split_whitespace()
                .find(|w| w.starts_with("avg10="))
                .and_then(|w| w.split('=').nth(1))
            {
                if let Ok(avg10) = avg10_str.parse::<f64>() {
                    if avg10 > 1.0 {
                        if !has_output {
                            let _ = writeln!(out);
                        }
                        let _ = writeln!(
                            out,
                            "  {} {} memory pressure in {label}: avg10={:.1}%",
                            style::yellow("⚠"),
                            style::bold("High"),
                            avg10,
                        );
                        has_output = true;
                    }
                }
            }
        }
    }

    if !has_output {
        let _ = writeln!(out, "  {} No OOM kills or pressure in monitored cgroups", style::green("✓"));
    }

    Ok(())
}
