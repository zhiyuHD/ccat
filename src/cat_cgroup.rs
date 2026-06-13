//! Cgroup v2 hierarchy explorer (`ccat --cgroup`).
//!
//! Reads `/sys/fs/cgroup/`, `/proc/pressure/`, per-cgroup memory/CPU/PSI
//! stat files to produce a beautiful, coloured analysis of Linux cgroup v2
//! resource distribution:
//!
//! - Controller overview (available / delegated)
//! - Pressure Stall Information (CPU / memory / I/O)
//! - Top memory consumers by cgroup (with swap, type breakdown)
//! - Per-consumer memory breakdown (anon / file / kernel)
//! - CPU usage per consumer
//!
//! Uses only /proc and /sysfs — no special privileges needed.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

// ── Colour helpers (self-contained, mirrors cat_disk/cat_cpu) ──

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

    pub fn use_pct(pct: f64) -> String {
        if pct > 90.0 { red(format!("{:>5.0}%", pct)) }
        else if pct > 70.0 { yellow(format!("{:>5.0}%", pct)) }
        else if pct > 30.0 { format!("{:>5.0}%", pct as u32) }
        else { green(format!("{:>5.0}%", pct)) }
    }

    /// Colour a PSI avg value. Red if >10, yellow >1, else green.
    pub fn psi_val(v: f64) -> String {
        if v > 10.0 { red(format!("{:.2}", v)) }
        else if v > 1.0 { yellow(format!("{:.2}", v)) }
        else { green(format!("{:.2}", v)) }
    }
}

// ── Data types ──

#[derive(Debug, Clone)]
struct PsiStats {
    some_avg10: f64,
    some_avg60: f64,
    some_avg300: f64,
    some_total: u64,
    full_avg10: Option<f64>,
    full_avg60: Option<f64>,
    full_avg300: Option<f64>,
    full_total: Option<u64>,
}

#[derive(Debug, Clone)]
struct CgroupInfo {
    path: String,
    name: String,
    cgroup_type: String,
    mem_current: u64,
    mem_swap: Option<u64>,
    mem_low: Option<u64>,
    mem_high: Option<u64>,
    mem_max: Option<u64>,
    anon: Option<u64>,
    file: Option<u64>,
    kernel: Option<u64>,
    kernel_stack: Option<u64>,
    workingset_refault: Option<u64>,
    cpu_usage_usec: Option<u64>,
    cpu_user_usec: Option<u64>,
    cpu_system_usec: Option<u64>,
    nr_periods: Option<u64>,
    nr_throttled: Option<u64>,
    throttled_usec: Option<u64>,
    procs_count: Option<u32>,
    children: Vec<CgroupInfo>,
}

#[derive(Debug, Clone)]
struct Controllers {
    available: Vec<String>,
    subtree: Vec<String>,
}

// ── Helpers ──

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} B", bytes)
    } else if size >= 100.0 {
        format!("{:.0}{}", size, UNITS[unit])
    } else if size >= 10.0 {
        format!("{:.0}{}", size, UNITS[unit])
    } else {
        format!("{:.1}{}", size, UNITS[unit])
    }
}

fn human_time_us(usec: u64) -> String {
    if usec >= 3_600_000_000 {
        format!("{:.1}h", usec as f64 / 3_600_000_000.0)
    } else if usec >= 60_000_000 {
        format!("{:.1}m", usec as f64 / 60_000_000.0)
    } else if usec >= 1_000_000 {
        format!("{:.1}s", usec as f64 / 1_000_000.0)
    } else if usec >= 1_000 {
        format!("{:.1}ms", usec as f64 / 1_000.0)
    } else {
        format!("{}us", usec)
    }
}

fn human_count(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

fn read_sysfs_string(path: &str) -> Option<String> {
    Some(fs::read_to_string(path).ok()?.trim().to_string())
}

fn read_sysfs_u64(path: &str) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_space_list(path: &str) -> Vec<String> {
    match read_sysfs_string(path) {
        Some(s) => s.split_whitespace().map(|x| x.to_string()).collect(),
        None => vec![],
    }
}

/// Parse a single PSI file /proc/pressure/{cpu,memory,io}.
fn read_psi(kind: &str) -> Option<PsiStats> {
    let content = fs::read_to_string(format!("/proc/pressure/{}", kind)).ok()?;
    let mut some = None;
    let mut full = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Some(rest) = line.strip_prefix("some") {
            some = parse_psi_line(rest);
        } else if let Some(rest) = line.strip_prefix("full") {
            full = parse_psi_line(rest);
        }
    }

    let (some_avg10, some_avg60, some_avg300, some_total) = some?;

    let (full_avg10, full_avg60, full_avg300, full_total) = match full {
        Some((a10, a60, a300, tot)) => (Some(a10), Some(a60), Some(a300), Some(tot)),
        None => (None, None, None, None),
    };

    Some(PsiStats {
        some_avg10, some_avg60, some_avg300, some_total,
        full_avg10, full_avg60, full_avg300, full_total,
    })
}

fn parse_psi_line(line: &str) -> Option<(f64, f64, f64, u64)> {
    // Format: " avg10=0.00 avg60=0.00 avg300=0.00 total=12345"
    let mut avg10 = 0.0f64;
    let mut avg60 = 0.0f64;
    let mut avg300 = 0.0f64;
    let mut total = 0u64;

    for token in line.split_whitespace() {
        if let Some(val) = token.strip_prefix("avg10=") {
            avg10 = val.parse().unwrap_or(0.0);
        } else if let Some(val) = token.strip_prefix("avg60=") {
            avg60 = val.parse().unwrap_or(0.0);
        } else if let Some(val) = token.strip_prefix("avg300=") {
            avg300 = val.parse().unwrap_or(0.0);
        } else if let Some(val) = token.strip_prefix("total=") {
            total = val.parse().unwrap_or(0);
        }
    }

    Some((avg10, avg60, avg300, total))
}

/// Read controller info: available and subtree delegation.
fn read_controllers() -> Controllers {
    Controllers {
        available: read_space_list("/sys/fs/cgroup/cgroup.controllers"),
        subtree: read_space_list("/sys/fs/cgroup/cgroup.subtree_control"),
    }
}

/// Read all top-level cgroups (direct children of /sys/fs/cgroup/).
fn read_top_cgroups(cgroup_base: &Path) -> Vec<CgroupInfo> {
    let mut groups = Vec::new();
    let entries = match fs::read_dir(cgroup_base) {
        Ok(e) => e,
        Err(_) => return groups,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() { continue; }
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden / internal files
        if name.starts_with('.') || name.starts_with("cgroup.") { continue; }

        let info = read_cgroup(path.to_string_lossy().as_ref(), &name, 0);
        if let Some(cg) = info {
            groups.push(cg);
        }
    }

    // Sort by memory usage desc
    groups.sort_by(|a, b| b.mem_current.cmp(&a.mem_current));
    groups
}

/// Read a single cgroup at `full_path` with given `name`. Recursively read
/// children up to `depth` (0 = no recursion, 1 = one level).
fn read_cgroup(full_path: &str, name: &str, depth: usize) -> Option<CgroupInfo> {
    let cgroup_type = read_sysfs_string(&format!("{}/cgroup.type", full_path))
        .unwrap_or_default();

    let mem_current = read_sysfs_u64(&format!("{}/memory.current", full_path))
        .unwrap_or(0);
    let mem_swap = read_sysfs_u64(&format!("{}/memory.swap.current", full_path));
    let mem_low = read_sysfs_u64(&format!("{}/memory.low", full_path));
    let mem_high = read_sysfs_u64(&format!("{}/memory.high", full_path));
    let mem_max = read_sysfs_u64(&format!("{}/memory.max", full_path));

    // memory.stat
    let mem_stat_text = read_sysfs_string(&format!("{}/memory.stat", full_path));
    let mem_stat = mem_stat_text.as_deref().and_then(|s| parse_mem_stat(s));
    let (anon, file, kernel, kernel_stack, workingset_refault) = match &mem_stat {
        Some(m) => (m.get("anon").copied(), m.get("file").copied(),
                     m.get("kernel").copied(), m.get("kernel_stack").copied(),
                     m.get("workingset_refault_anon").or_else(|| m.get("workingset_refault")).copied()),
        None => (None, None, None, None, None),
    };

    // cpu.stat
    let cpu_stat_text = read_sysfs_string(&format!("{}/cpu.stat", full_path));
    let (cpu_usage_usec, cpu_user_usec, cpu_system_usec,
         nr_periods, nr_throttled, throttled_usec) = match &cpu_stat_text {
        Some(s) => parse_cpu_stat(s),
        None => (None, None, None, None, None, None),
    };

    // Count procs
    let procs_count = read_sysfs_string(&format!("{}/cgroup.procs", full_path))
        .map(|s| s.lines().count() as u32);

    // Children (limited depth)
    let children = if depth < 1 {
        let path = Path::new(full_path);
        if path.is_dir() {
            let mut child_info = Vec::new();
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    let child_path = entry.path();
                    if !child_path.is_dir() { continue; }
                    let child_name = entry.file_name().to_string_lossy().to_string();
                    // Skip internal
                    if child_name.starts_with('.') || child_name.starts_with("cgroup.") { continue; }

                    if let Some(cg) = read_cgroup(
                        child_path.to_string_lossy().as_ref(),
                        &child_name,
                        depth + 1,
                    ) {
                        child_info.push(cg);
                    }
                }
            }
            child_info.sort_by(|a, b| b.mem_current.cmp(&a.mem_current));
            child_info
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    Some(CgroupInfo {
        path: full_path.to_string(),
        name: name.to_string(),
        cgroup_type,
        mem_current,
        mem_swap,
        mem_low,
        mem_high,
        mem_max,
        anon,
        file,
        kernel,
        kernel_stack,
        workingset_refault,
        cpu_usage_usec,
        cpu_user_usec,
        cpu_system_usec,
        nr_periods,
        nr_throttled,
        throttled_usec,
        procs_count,
        children,
    })
}

fn parse_mem_stat(text: &str) -> Option<HashMap<String, u64>> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let mut parts = line.splitn(2, ' ');
        let key = parts.next()?;
        let val: u64 = parts.next()?.parse().ok()?;
        map.insert(key.to_string(), val);
    }
    Some(map)
}

fn parse_cpu_stat(text: &str) -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    let mut usage = None;
    let mut user = None;
    let mut system = None;
    let mut periods = None;
    let mut throttled = None;
    let mut throttled_usec = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let mut parts = line.splitn(2, ' ');
        let key = match parts.next() { Some(k) => k, None => continue };
        let val: u64 = match parts.next().and_then(|v| v.parse().ok()) { Some(v) => v, None => continue };

        match key {
            "usage_usec" => usage = Some(val),
            "user_usec" => user = Some(val),
            "system_usec" => system = Some(val),
            "nr_periods" => periods = Some(val),
            "nr_throttled" => throttled = Some(val),
            "throttled_usec" => throttled_usec = Some(val),
            _ => {}
        }
    }

    (usage, user, system, periods, throttled, throttled_usec)
}

/// Print a section header.
fn header(text: &str) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "\n{}\n{}", style::bold(style::cyan(text)),
        style::grey("─".repeat(text.len().min(50))));
}

fn sep(width: usize) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", style::grey("─".repeat(width.min(80))));
}

// ── Renderers ──

/// Render the controller overview.
fn render_controllers(ctrls: &Controllers) {
    header("CGROUP CONTROLLERS");

    let stdout = io::stdout();
    let mut out = stdout.lock();

    let _ = writeln!(out, "  {}  Available: {}",
        style::grey("◆"),
        ctrls.available.iter()
            .map(|c| style::green(c))
            .collect::<Vec<_>>()
            .join(" "));

    let active: Vec<String> = ctrls.available.iter()
        .filter(|c| ctrls.subtree.contains(c))
        .map(|c| style::bold(style::cyan(c)))
        .collect();
    let inactive: Vec<String> = ctrls.available.iter()
        .filter(|c| !ctrls.subtree.contains(c))
        .map(|c| style::dim(c))
        .collect();

    let _ = writeln!(out, "  {}  Active:    {}",
        style::grey("│"),
        if active.is_empty() { style::dim("(none)").to_string() } else { active.join(" ") });

    let _ = writeln!(out, "  {}  Inactive:  {}",
        style::grey("│"),
        if inactive.is_empty() { style::dim("(none)").to_string() } else { inactive.join(" ") });

    // cgroup.stat
    if let Some(stat) = read_sysfs_string("/sys/fs/cgroup/cgroup.stat") {
        for line in stat.lines() {
            let line = line.trim();
            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            if parts.len() == 2 {
                let key = parts[0];
                let val = parts[1];
                let colored = if key.starts_with("nr_dying") {
                    style::yellow(format!("{} {}", key, val))
                } else {
                    style::dim(format!("{} {}", key, val))
                };
                let _ = writeln!(out, "  {}  {}", style::grey("│"), colored);
            }
        }
    }
}

/// Render Pressure Stall Information.
fn render_psi() {
    header("PRESSURE STALL INFORMATION");

    let stdout = io::stdout();
    let mut out = stdout.lock();

    for resource in &["cpu", "memory", "io"] {
        let psi = match read_psi(resource) {
            Some(p) => p,
            None => {
                let _ = writeln!(out, "  {}  {}: No PSI data",
                    style::grey("◇"), resource);
                continue;
            }
        };

        let resource_color = match *resource {
            "cpu" => style::cyan,
            "memory" => style::yellow,
            "io" => style::blue,
            _ => |s: &str| s.to_string(),
        };

        let _ = write!(out, "  {}  {}  ",
            style::grey("│"),
            resource_color(style::bold(resource).as_str()));

        // some avg10 / avg60 / avg300
        let _ = write!(out, "some: {}/{}/{}",
            style::psi_val(psi.some_avg10),
            style::psi_val(psi.some_avg60),
            style::psi_val(psi.some_avg300));

        if let (Some(f10), Some(f60), Some(f300)) = (psi.full_avg10, psi.full_avg60, psi.full_avg300) {
            let _ = write!(out, "  full: {}/{}/{}  ",
                style::psi_val(f10),
                style::psi_val(f60),
                style::psi_val(f300));
        }

        let _ = writeln!(out, "{}", style::dim(format!("total: {}", human_count(psi.some_total))));
    }
}

/// Render the top memory consumers.
fn render_top_consumers(groups: &[CgroupInfo]) {
    header("TOP MEMORY CONSUMERS");

    let stdout = io::stdout();
    let mut out = stdout.lock();

    if groups.is_empty() {
        let _ = writeln!(out, "  {}  No cgroup data available.", style::grey("∅"));
        return;
    }

    // Collect all reachable cgroups into a flat list, sorted by mem_current
    let mut flat = Vec::new();
    collect_cgroups(groups, &mut flat, 0);
    flat.sort_by(|a, b| b.mem_current.cmp(&a.mem_current));

    // Show top 15
    let max_name = flat.iter().take(15).map(|c| c.name.len()).max().unwrap_or(20).min(40);

    // Header
    let _ = writeln!(out, "  {:<nw$} {:>8} {:>8} {:>6} {:>6} {:>6} {:>9} {}",
        style::bold("CGROUP"),
        style::bold("MEMORY"),
        style::bold("SWAP"),
        style::bold("ANON"),
        style::bold("FILE"),
        style::bold("CPU"),
        style::bold("WORKINGSET"),
        style::bold("TYPE"),
        nw = max_name);

    for (_i, cg) in flat.iter().take(15).enumerate() {
        let name_display = if cg.name.len() > max_name {
            let trunc = max_name.saturating_sub(1);
            format!("…{}", &cg.name[cg.name.len().saturating_sub(trunc)..])
        } else {
            cg.name.clone()
        };

        // Memory colouring based on severity
        let mem_str = style::use_pct(if cg.mem_current > 500_000_000 { 95.0 }
                                      else if cg.mem_current > 50_000_000 { 50.0 }
                                      else { 10.0 });

        // Swap indicator
        let swap_str = match cg.mem_swap {
            Some(s) if s > 0 => {
                let pct = if s > 500_000_000 { 95.0 }
                          else if s > 50_000_000 { 50.0 }
                          else { 10.0 };
                style::use_pct(pct)
            }
            _ => style::dim("   -").to_string(),
        };

        // Memory type breakdown
        let anon_str = match cg.anon {
            Some(a) => human_size(a),
            None => style::dim("-").to_string(),
        };
        let file_str = match cg.file {
            Some(f) => human_size(f),
            None => style::dim("-").to_string(),
        };

        // CPU usage
        let cpu_str = match cg.cpu_usage_usec {
            Some(u) => human_time_us(u),
            None => style::dim("-").to_string(),
        };

        // Working set refault (memory pressure indicator)
        let ws_str = match cg.workingset_refault {
            Some(r) if r > 0 => style::yellow(human_count(r)),
            Some(_) => style::dim("0").to_string(),
            None => style::dim("-").to_string(),
        };

        // Type
        let type_str = match cg.cgroup_type.as_str() {
            "domain" => style::dim("domain"),
            t => t.to_string(),
        };

        // Throttle indicator
        let throttle_mark = match (cg.nr_throttled, cg.throttled_usec) {
            (Some(n), Some(_u)) if n > 0 => {
                format!(" {} ", style::red(format!("⚡{}x", n)))
            }
            _ => String::new(),
        };

        let _ = writeln!(out, "  {:<nw$} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9}{} {}",
            name_display,
            mem_str,
            swap_str,
            anon_str,
            file_str,
            cpu_str,
            ws_str,
            throttle_mark,
            type_str,
            nw = max_name);
    }

    // Summary
    let total_mem: u64 = flat.iter().map(|c| c.mem_current).sum();
    let total_swap: u64 = flat.iter().filter_map(|c| c.mem_swap).sum();
    let avg_cpu: u64 = flat.iter().filter_map(|c| c.cpu_usage_usec).sum::<u64>() / flat.len().max(1) as u64;

    let (_width, _) = crate::pager::terminal_size();
    sep(_width);

    let _ = writeln!(out, "  {} {} total across {} cgroups  |  {} total swap  |  avg CPU {}",
        style::bold("∑"),
        style::bold(human_size(total_mem)),
        flat.len(),
        style::yellow(human_size(total_swap)),
        human_time_us(avg_cpu));
}

fn collect_cgroups(groups: &[CgroupInfo], acc: &mut Vec<CgroupInfo>, depth: usize) {
    for g in groups {
        // Skip leaf mount scopes that are just a few KB (noise)
        if depth > 0 && g.mem_current < 4096 && g.children.is_empty() {
            continue;
        }
        // Also skip empty intermediate groups at deeper levels with no data
        if depth > 0 && g.mem_current == 0 && g.cpu_usage_usec.is_none() && g.procs_count.unwrap_or(0) == 0 {
            continue;
        }
        acc.push(g.clone());
        collect_cgroups(&g.children, acc, depth + 1);
    }
}

/// Render per-cgroup detail for top N consumers (expanded view).
fn render_detail(groups: &[CgroupInfo]) {
    let mut flat = Vec::new();
    collect_cgroups(groups, &mut flat, 0);

    // Take top 5 for detailed view
    flat.sort_by(|a, b| b.mem_current.cmp(&a.mem_current));

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let (_width, _) = crate::pager::terminal_size();

    for cg in flat.iter().take(5) {
        let _ = writeln!(out);
        let _ = writeln!(out, "  {} {} {}",
            style::grey("┌─"),
            style::bold(style::cyan(&cg.name)),
            style::dim(format!("[{}]", cg.cgroup_type)));

        // Memory line
        let _ = write!(out, "  {} {} {}  {} {}",
            style::grey("│"),
            style::bold("Memory:"),
            style::bold(human_size(cg.mem_current)),
            style::dim("─"), "");

        if let Some(c) = cg.mem_swap.filter(|&s| s > 0) {
            let _ = write!(out, "  swap {}", style::yellow(human_size(c)));
        }

        // Memory limits
        let limits = match (cg.mem_low, cg.mem_high, cg.mem_max) {
            (Some(l), Some(h), Some(m)) if l > 0 || h < u64::MAX || m < u64::MAX => {
                let parts: Vec<String> = [
                    if l > 0 { Some(format!("low={}", human_size(l))) } else { None },
                    if h < u64::MAX { Some(format!("high={}", human_size(h))) } else { None },
                    if m < u64::MAX && m > 0 { Some(format!("max={}", human_size(m))) } else { None },
                ].into_iter().flatten().collect();
                if parts.is_empty() { None } else { Some(format!(" ({})", parts.join(", "))) }
            }
            _ => None,
        };
        if let Some(l) = limits {
            let _ = write!(out, " {}", style::dim(l));
        }

        let _ = writeln!(out);

        // Memory breakdown bar
        if let (Some(a), Some(f), Some(k)) = (cg.anon, cg.file, cg.kernel) {
            let total = a + f + k;
            if total > 0 {
                let bar_w = 30usize;
                let a_w = ((a as f64 / total as f64) * bar_w as f64).round() as usize;
                let f_w = ((f as f64 / total as f64) * bar_w as f64).round() as usize;
                // k_w = bar_w - a_w - f_w, but floor rounding may leave gaps, last one fills
                let k_w = bar_w.saturating_sub(a_w).saturating_sub(f_w);

                let anon_bar: String = std::iter::repeat("█").take(a_w.min(bar_w)).collect();
                let file_bar: String = std::iter::repeat("█").take(f_w.min(bar_w)).collect();
                let kern_bar: String = std::iter::repeat("█").take(k_w.min(bar_w)).collect();
                let _ = writeln!(
                    out,
                    "  {}  {} {}  {} {}  {} {}",
                    style::grey("│"),
                    style::green(anon_bar),
                    style::cyan(format!("anon {}", human_size(a))),
                    style::blue(file_bar),
                    style::cyan(format!("file {}", human_size(f))),
                    style::magenta(kern_bar),
                    style::dim(format!("kernel {}", human_size(k))));
            }
        }

        // CPU
        if let Some(cpu) = cg.cpu_usage_usec {
            let _ = writeln!(out, "  {}  {}  {}  user={}  sys={}",
                style::grey("│"),
                style::bold("CPU:"),
                human_time_us(cpu),
                cg.cpu_user_usec.map(human_time_us).unwrap_or_default(),
                cg.cpu_system_usec.map(human_time_us).unwrap_or_default());
        }

        // Throttling
        if let (Some(periods), Some(throttled), Some(throttled_usec)) =
            (cg.nr_periods, cg.nr_throttled, cg.throttled_usec)
        {
            if throttled > 0 {
                let throttle_pct = if periods > 0 {
                    throttled as f64 / periods as f64 * 100.0
                } else { 0.0 };
                let _ = writeln!(out, "  {}  {}  {} periods, {} throttled ({:.1}%), {} lost",
                    style::grey("│"),
                    style::red("⚡"),
                    style::dim(human_count(periods)),
                    style::yellow(human_count(throttled)),
                    throttle_pct,
                    human_time_us(throttled_usec));
            }
        }

        // Processes
        if let Some(procs) = cg.procs_count {
            if procs > 0 {
                let _ = writeln!(out, "  {}  {}  {} processes",
                    style::grey("│"),
                    style::bold("Procs:"),
                    procs);
            }
        }

        let _ = writeln!(out, "  {}", style::grey("└─"));
    }
}

// ── Main entry point ──

pub fn cat_cgroup() {
    let (_width, _) = crate::pager::terminal_size();

    // ── Section 1: Controllers ──
    let ctrls = read_controllers();
    render_controllers(&ctrls);

    // ── Section 2: PSI ──
    render_psi();

    // ── Section 3: Top Consumers ──
    let groups = read_top_cgroups(Path::new("/sys/fs/cgroup"));
    render_top_consumers(&groups);

    // ── Section 4: Detailed view for top 5 ──
    if groups.iter().any(|g| g.mem_current > 0) {
        render_detail(&groups);
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(1024 * 1024), "1.0M");
        assert_eq!(human_size(1024u64 * 1024 * 1024), "1.0G");
    }

    #[test]
    fn test_human_time_us() {
        assert_eq!(human_time_us(500), "500us");
        assert_eq!(human_time_us(1500), "1.5ms");
        assert_eq!(human_time_us(1_000_000), "1.0s");
        assert_eq!(human_time_us(60_000_000), "1.0m");
        assert_eq!(human_time_us(3_600_000_000), "1.0h");
    }

    #[test]
    fn test_parse_psi_line() {
        let line = " avg10=0.00 avg60=0.01 avg300=0.05 total=123456789";
        let result = parse_psi_line(line);
        assert!(result.is_some());
        let (a10, a60, a300, total) = result.unwrap();
        assert!((a10 - 0.0).abs() < 0.001);
        assert!((a60 - 0.01).abs() < 0.001);
        assert!((a300 - 0.05).abs() < 0.001);
        assert_eq!(total, 123456789);
    }

    #[test]
    fn test_parse_psi_line_no_total() {
        let line = " avg10=1.50 avg60=2.00 avg300=3.00";
        let result = parse_psi_line(line);
        assert!(result.is_some());
        let (a10, a60, a300, total) = result.unwrap();
        assert!((a10 - 1.50).abs() < 0.001);
        assert_eq!(total, 0);
    }

    #[test]
    fn test_parse_mem_stat() {
        let text = "anon 123456\nfile 789012\nkernel 3456\nkernel_stack 1024\n";
        let map = parse_mem_stat(text);
        assert!(map.is_some());
        let map = map.unwrap();
        assert_eq!(map.get("anon"), Some(&123456));
        assert_eq!(map.get("file"), Some(&789012));
        assert_eq!(map.get("kernel"), Some(&3456));
        assert_eq!(map.get("kernel_stack"), Some(&1024));
    }

    #[test]
    fn test_parse_mem_stat_empty() {
        let map = parse_mem_stat("");
        assert!(map.is_some());
        assert!(map.unwrap().is_empty());
    }

    #[test]
    fn test_parse_cpu_stat() {
        let text = "usage_usec 12345678\nuser_usec 10000000\nsystem_usec 2345678\nnr_periods 100\nnr_throttled 5\nthrottled_usec 50000\n";
        let (usage, user, system, periods, throttled, throttled_usec) = parse_cpu_stat(text);
        assert_eq!(usage, Some(12345678));
        assert_eq!(user, Some(10000000));
        assert_eq!(system, Some(2345678));
        assert_eq!(periods, Some(100));
        assert_eq!(throttled, Some(5));
        assert_eq!(throttled_usec, Some(50000));
    }

    #[test]
    fn test_parse_cpu_stat_partial() {
        let text = "usage_usec 1000\nnr_periods 50\n";
        let (usage, user, system, periods, throttled, throttled_usec) = parse_cpu_stat(text);
        assert_eq!(usage, Some(1000));
        assert_eq!(user, None);
        assert_eq!(periods, Some(50));
        assert_eq!(throttled, None);
        assert_eq!(throttled_usec, None);
    }

    #[test]
    fn test_read_space_list() {
        // This calls the real filesystem, might return empty in test env
        let list = read_space_list("/sys/fs/cgroup/cgroup.controllers");
        // Should at least parse something
        for item in &list {
            assert!(!item.is_empty());
        }
    }

    #[test]
    fn test_read_psi_cpu_exists() {
        let psi = read_psi("cpu");
        assert!(psi.is_some(), "/proc/pressure/cpu should exist");
    }

    #[test]
    fn test_collect_cgroups_empty() {
        let mut acc = Vec::new();
        collect_cgroups(&[], &mut acc, 0);
        assert!(acc.is_empty());
    }

    #[test]
    fn test_style_psi_val() {
        let _ = style::psi_val(0.0);
        let _ = style::psi_val(5.0);
        let _ = style::psi_val(20.0);
        assert!(style::psi_val(0.5).contains("32m"));  // green
        assert!(style::psi_val(5.0).contains("33m"));  // yellow
        assert!(style::psi_val(15.0).contains("31m")); // red
    }

    #[test]
    fn test_style_use_pct() {
        let _ = style::use_pct(10.0);
        let _ = style::use_pct(50.0);
        let _ = style::use_pct(95.0);
    }

    #[test]
    fn test_parse_cpu_stat_invalid() {
        let text = "not_a_number abc\nusage_usec 100\n";
        let (usage, _, _, _, _, _) = parse_cpu_stat(text);
        // Should skip invalid lines, still parse valid ones
        assert_eq!(usage, Some(100));
    }

    #[test]
    fn test_human_time_us_various() {
        assert_eq!(human_time_us(0), "0us");
        assert_eq!(human_time_us(999), "999us");
        assert_eq!(human_time_us(1_000_000), "1.0s");
        assert_eq!(human_time_us(120_000_000), "2.0m");
        assert_eq!(human_time_us(7_200_000_000), "2.0h");
    }
}
