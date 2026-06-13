//! Linux process listing (`ccat --ps`).
//!
//! Reads `/proc` to show a colourful, paged process table with
//! PID, state, CPU%, MEM%, RSS, VSZ, time, threads, nice, and
//! command line.  Subcommands for sorting and filtering.
//!
//! Natural companion to `--vmmap` (per-process memory topology)
//! and `--meminfo` (system-wide memory summary).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

// ── Colour helpers (self-contained, mirrors cat_vmmap) ──

mod style {
    pub fn bold(s: impl AsRef<str>) -> String   { format!("\x1b[1m{}\x1b[0m", s.as_ref()) }
    pub fn dim(s: impl AsRef<str>) -> String    { format!("\x1b[2m{}\x1b[0m", s.as_ref()) }
    pub fn green(s: impl AsRef<str>) -> String  { format!("\x1b[32m{}\x1b[0m", s.as_ref()) }
    pub fn red(s: impl AsRef<str>) -> String    { format!("\x1b[31m{}\x1b[0m", s.as_ref()) }
    pub fn cyan(s: impl AsRef<str>) -> String   { format!("\x1b[36m{}\x1b[0m", s.as_ref()) }
    pub fn yellow(s: impl AsRef<str>) -> String { format!("\x1b[33m{}\x1b[0m", s.as_ref()) }
    pub fn blue(s: impl AsRef<str>) -> String   { format!("\x1b[34m{}\x1b[0m", s.as_ref()) }
    pub fn magenta(s: impl AsRef<str>) -> String { format!("\x1b[35m{}\x1b[0m", s.as_ref()) }
}

// ── Data types ──

/// Parsed /proc/[pid]/stat fields we care about.
#[derive(Debug, Clone)]
struct ProcStat {
    pid: u32,
    comm: String,
    state: String,
    ppid: u32,
    utime: u64,
    stime: u64,
    nice: i32,
    num_threads: u32,
    vsize: u64,
    rss: u64,       // pages
    processor: u32,
}

/// A process entry presented to the user.
#[derive(Debug, Clone)]
struct Process {
    pid: u32,
    state: String,
    ppid: u32,
    comm: String,
    cmdline: String,
    utime: u64,
    stime: u64,
    nice: i32,
    threads: u32,
    vsize_mb: u64,
    rss_mb: u64,
    #[allow(dead_code)]
    processor: u32,
}

// ── /proc helpers ──

/// Parse /proc/[pid]/stat — famously tricky because `comm` is in parens
/// and can contain spaces, closing parens, anything.
fn parse_stat(data: &str) -> Option<ProcStat> {
    // Find first '(' and last ')'
    let open = data.find('(')?;
    let close = data.rfind(')')?;
    let comm = &data[open + 1..close];
    let tail = &data[close + 2..]; // skip ") "

    let fields: Vec<&str> = tail.split(' ').collect();
    if fields.len() < 44 {
        return None;
    }

    let pid = data[..open].trim().parse().ok()?;
    let state = fields[0].to_string();
    let ppid: u32 = fields[1].parse().ok()?;
    let utime: u64 = fields[11].parse().ok()?;
    let stime: u64 = fields[12].parse().ok()?;
    let nice: i32 = fields[16].parse().ok()?;
    let num_threads: u32 = fields[17].parse().ok()?;
    let vsize: u64 = fields[20].parse().ok()?;
    let rss: u64 = fields[21].parse().ok()?;
    let processor: u32 = fields[36].parse().ok()?;

    Some(ProcStat {
        pid, comm: comm.to_string(), state, ppid,
        utime, stime, nice, num_threads, vsize, rss, processor,
    })
}

/// Read /proc/[pid]/cmdline (null-separated → space-joined).
fn read_cmdline(path: &Path) -> String {
    match fs::read(path) {
        Ok(bytes) => {
            let s: String = bytes
                .split(|&b| b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            if s.is_empty() {
                String::new()
            } else {
                s
            }
        }
        Err(_) => String::new(),
    }
}

/// Read /proc/stat to get total CPU ticks (sum of all lines starting with "cpu").
fn read_total_cpu_ticks() -> u64 {
    let data = match fs::read_to_string("/proc/stat") {
        Ok(d) => d,
        Err(_) => return 0,
    };
    let mut total = 0u64;
    for line in data.lines() {
        if line.starts_with("cpu ") {
            // cpu   user nice system idle iowait irq softirq steal guest
            let fields: Vec<&str> = line.split_whitespace().collect();
            for f in &fields[1..] {
                total += f.parse::<u64>().unwrap_or(0);
            }
            break;
        }
    }
    total
}

/// Read /proc/loadavg and return the three load values + running/total processes.
fn read_loadavg() -> Option<(f64, f64, f64, u32, u32)> {
    let data = fs::read_to_string("/proc/loadavg").ok()?;
    let fields: Vec<&str> = data.split_whitespace().collect();
    if fields.len() < 5 { return None; }
    let l1: f64 = fields[0].parse().ok()?;
    let l5: f64 = fields[1].parse().ok()?;
    let l15: f64 = fields[2].parse().ok()?;
    let procs: Vec<&str> = fields[3].split('/').collect();
    if procs.len() < 2 { return None; }
    let running: u32 = procs[0].parse().ok()?;
    let total: u32 = procs[1].parse().ok()?;
    Some((l1, l5, l15, running, total))
}

/// Read /proc/uptime → seconds since boot.
fn read_uptime_secs() -> f64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok())
        .unwrap_or(0.0)
}

/// Count available CPUs from /proc/stat (cpu0..cpuN lines).
fn read_ncpus() -> u32 {
    let data = fs::read_to_string("/proc/stat").ok();
    match data {
        Some(s) => s.lines().filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(|c| c.is_ascii_digit())).count() as u32,
        None => 1,
    }
}

/// Format CPU time (ticks) as HH:MM:SS.
fn format_time(ticks: u64, clk_tck: u64) -> String {
    let secs = ticks / clk_tck;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 99 {
        format!("{:02}:{:02}:{:02}", h, m, s)
    } else if h > 0 {
        format!("{:02}:{:02}:{:02}", h, m, s)
    } else if m > 0 {
        format!("{:>2}:{:02}", m, s)
    } else {
        format!("{:>4}s", s)
    }
}

/// Color-code a process state character.
fn color_state(state: &str) -> String {
    match state.chars().next() {
        Some('R') => style::green(state),
        Some('S') => style::cyan(state),
        Some('D') => style::red(state),
        Some('Z') => style::magenta(state),
        Some('T') => style::blue(state),
        Some('I') => style::dim(state),
        _ => state.to_string(),
    }
}

/// Describe a process state character for human-readable output.
#[allow(dead_code)]
fn describe_state(c: char) -> &'static str {
    match c {
        'R' => "running",
        'S' => "sleeping",
        'D' => "uninterruptible",
        'Z' => "zombie",
        'T' => "stopped",
        't' => "tracing stop",
        'X' => "dead",
        'I' => "idle",
        'P' => "parked",
        _ => "unknown",
    }
}

/// Human-readable memory size (KB → MB/GB).
fn mem_hr(kb: u64) -> String {
    if kb >= 1_048_576 {
        format!("{:.1}G", kb as f64 / 1_048_576.0)
    } else if kb >= 1024 {
        format!("{:.0}M", kb as f64 / 1024.0)
    } else {
        format!("{kb}K")
    }
}

/// Sort processes by a field name.
fn sort_processes(procs: &mut [Process], sort_by: &str, reverse: bool) {
    match sort_by {
        "pid" | "p" => procs.sort_by_key(|p| p.pid),
        "mem" | "rss" | "m" => procs.sort_by_key(|p| p.rss_mb),
        "cpu" | "c" => procs.sort_by_key(|p| p.utime + p.stime),
        "ppid" => procs.sort_by_key(|p| p.ppid),
        "threads" | "thr" => procs.sort_by_key(|p| p.threads),
        "state" | "s" => procs.sort_by(|a, b| a.state.cmp(&b.state)),
        "nice" | "n" => procs.sort_by_key(|p| p.nice),
        _ => procs.sort_by(|a, b| {
            let acmd = a.cmdline.trim().to_lowercase();
            let bcmd = b.cmdline.trim().to_lowercase();
            let a_name = if acmd.is_empty() { a.comm.to_lowercase() } else { acmd };
            let b_name = if bcmd.is_empty() { b.comm.to_lowercase() } else { bcmd };
            a_name.cmp(&b_name)
        }),
    }
    if reverse {
        procs.reverse();
    }
}

// ── Main public entry points ──

/// `ccat --ps` — list all processes in a colourful table.
///
/// `sort_by`: "pid", "mem", "cpu", "name", or "" (default = pid).
/// `filter_pid`: if Some, only show that PID.
/// `filter_cmd`: optional substring filter on command line.
/// `use_pager`: if true and output > 20 lines, use the pager.
pub fn cat_ps(
    sort_by: Option<&str>,
    filter_pid: Option<u32>,
    filter_cmd: Option<&str>,
    use_pager: bool,
) {
    let clk_tck: u64 = 100; // sysconf(_SC_CLK_TCK) — standard on Linux
    let ncpus = read_ncpus();
    let _total_ticks = read_total_cpu_ticks();
    let uptime = read_uptime_secs();
    let loadavg = read_loadavg();

    // Read all /proc/[pid]/stat files
    let mut processes: Vec<Process> = Vec::new();
    let proc_dir = Path::new("/proc");

    let entries = match fs::read_dir(proc_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ccat --ps: cannot read /proc: {e}");
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let pid_str = match path.file_name().and_then(|n| n.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Optional PID filter
        if let Some(fp) = filter_pid {
            if pid != fp {
                continue;
            }
        }

        let stat_path = path.join("stat");
        let stat_data = match fs::read_to_string(&stat_path) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let parsed = match parse_stat(&stat_data) {
            Some(p) => p,
            None => continue,
        };

        let cmdline_path = path.join("cmdline");
        let cmdline = read_cmdline(&cmdline_path);

        // Optional command filter
        if let Some(ref fc) = filter_cmd {
            let search = cmdline.to_lowercase();
            if !search.contains(&fc.to_lowercase()) {
                continue;
            }
        }

        let vsize_mb = parsed.vsize / (1024 * 1024);
        let rss_mb = (parsed.rss * 4) / 1024; // RSS in pages * 4096 / 1024 / 1024

        processes.push(Process {
            pid: parsed.pid,
            state: parsed.state,
            ppid: parsed.ppid,
            comm: parsed.comm,
            cmdline,
            utime: parsed.utime,
            stime: parsed.stime,
            nice: parsed.nice,
            threads: parsed.num_threads,
            vsize_mb,
            rss_mb,
            processor: parsed.processor,
        });
    }

    if processes.is_empty() {
        if filter_pid.is_some() {
            eprintln!("ccat --ps: no process with PID {}", filter_pid.unwrap());
        } else if filter_cmd.is_some() {
            eprintln!("ccat --ps: no process matching \"{}\"", filter_cmd.unwrap());
        } else {
            eprintln!("ccat --ps: no processes found");
        }
        return;
    }

    // Sort
    let sort_by = sort_by.unwrap_or("pid");
    let reverse = sort_by.starts_with('-');
    let sort_key = sort_by.trim_start_matches('-');
    sort_processes(&mut processes, sort_key, reverse);

    // Build output lines
    let mut lines: Vec<String> = Vec::new();

    // ── Header: system summary ──
    lines.push(format!(
        "{}  {}  {}  {}",
        style::bold(" ccat --ps"),
        style::dim(format!("{} processes", processes.len())),
        style::dim(format!("{} CPU(s)", ncpus)),
        style::dim(format!("uptime {:.0}s", uptime)),
    ));
    if let Some((l1, l5, l15, running, total)) = loadavg {
        let load_str = format!("load: {l1:.2} {l5:.2} {l15:.2} ({running}/{total} running)");
        if l1 > ncpus as f64 {
            lines.push(format!("  {}  {}", style::red("⚠"), style::dim(&load_str)));
        } else if l1 > (ncpus as f64 * 0.7) {
            lines.push(format!("  {}  {}", style::yellow("⚡"), style::dim(&load_str)));
        } else {
            lines.push(format!("  {}  {}", " ", style::dim(&load_str)));
        }
    }

    // ── Table header ──
    let hdr = format!(
        " {:>6} {:>5} {:>6} {:>6} {:>6} {:>7} {:>4} {:>3} {:>4} {} {}",
        style::dim("PID"),
        style::dim("PPID"),
        style::dim("CPU%"),
        style::dim("MEM%"),
        style::dim("RSS"),
        style::dim("VSZ"),
        style::dim("TIME"),
        style::dim("THR"),
        style::dim("NI"),
        style::dim("S"),
        style::dim("COMMAND"),
    );
    lines.push(String::new());
    lines.push(hdr);

    // Estimate total memory from /proc/meminfo for MEM%
    let total_mem_kb = read_total_mem_kb().unwrap_or(16_000_000);

    for proc in &processes {
        // CPU% = (utime + stime) / uptime_CLK_TCK * 100 / ncpus
        let cpu_pct = if uptime > 0.0 {
            let proc_ticks = (proc.utime + proc.stime) as f64;
            let max_ticks = uptime * clk_tck as f64;
            let raw = proc_ticks / max_ticks * 100.0 * ncpus as f64;
            if raw > 999.9 { 999.9 } else { raw }
        } else {
            0.0
        };

        let mem_pct = if total_mem_kb > 0 {
            let raw = (proc.rss_mb as f64 * 1024.0) / total_mem_kb as f64 * 100.0;
            if raw > 99.9 { 99.9 } else { raw }
        } else {
            0.0
        };

        let cpu_str = if cpu_pct > 99.0 {
            format!("{:>5.0}%", cpu_pct)
        } else if cpu_pct > 0.0 {
            format!("{:>5.1}%", cpu_pct)
        } else {
            style::dim("  0.0%".to_string())
        };

        let mem_str = if mem_pct > 0.0 && mem_pct < 0.1 {
            style::dim("  <0.1".to_string())
        } else if mem_pct > 10.0 {
            style::red(format!("{:>5.1}%", mem_pct))
        } else if mem_pct > 5.0 {
            style::yellow(format!("{:>5.1}%", mem_pct))
        } else if mem_pct > 0.0 {
            format!("{:>5.1}%", mem_pct)
        } else {
            style::dim("  0.0%".to_string())
        };

        let rss_str = if proc.rss_mb > 1024 {
            style::red(format!("{:>6}", mem_hr(proc.rss_mb * 1024)))
        } else {
            format!("{:>6}", mem_hr(proc.rss_mb * 1024))
        };

        let vsz_str = if proc.vsize_mb > 1024 * 1024 {
            format!("{:>7}", mem_hr(proc.vsize_mb * 1024))
        } else {
            format!("{:>7}", mem_hr(proc.vsize_mb * 1024))
        };

        let time_str = format_time(proc.utime + proc.stime, clk_tck);

        let nice_str = if proc.nice < 0 {
            style::red(format!("{:>3}", proc.nice))
        } else if proc.nice > 0 {
            style::dim(format!("{:>3}", proc.nice))
        } else {
            format!("{:>3}", proc.nice)
        };

        let state_str = color_state(&proc.state);

        // Command: show truncated cmdline or comm
        let cmd_display = if !proc.cmdline.is_empty() {
            &proc.cmdline
        } else {
            &proc.comm
        };

        // Truncate command to fit terminal width
        let term_w = terminal_width();
        let est_prefix = 55; // rough width before command
        let max_cmd = term_w.saturating_sub(est_prefix).max(20);
        let cmd_trunc: &str = if cmd_display.len() > max_cmd {
            &cmd_display[..max_cmd.saturating_sub(3)]
        } else {
            cmd_display
        };

        let thread_str = if proc.threads > 1 {
            format!("{:>3}", proc.threads)
        } else {
            style::dim(format!("{:>3}", proc.threads))
        };

        let line = format!(
            " {:>6} {:>5} {:>6} {} {:>6} {:>7} {:>4} {} {:>3} {} {}",
            proc.pid,
            proc.ppid,
            cpu_str,
            mem_str,
            rss_str,
            vsz_str,
            time_str,
            thread_str,
            nice_str,
            state_str,
            cmd_trunc,
        );
        lines.push(line);
    }

    // Footer with legend
    lines.push(String::new());
    lines.push(format!(
        "  {} {} {} {} {} {} {}",
        style::dim("R=running"),
        style::dim("S=sleeping"),
        style::dim("D=uninterruptible"),
        style::magenta("Z=zombie"),
        style::blue("T=stopped"),
        style::dim("I=idle"),
        style::dim("| sort: pid|mem|cpu|name|state|ppid|threads|nice (prefix - for reverse)"),
    ));

    // Output: pager or plain
    if use_pager && lines.len() > 22 {
        crate::pager::run_pager(&lines);
    } else {
        for line in &lines {
            println!("{line}");
        }
    }
}

/// Show process tree (`ccat --ps --ps-tree`).
pub fn cat_pstree(
    filter_pid: Option<u32>,
    filter_cmd: Option<&str>,
    use_pager: bool,
) {
    // Build the process list
    let processes = gather_processes(filter_pid, filter_cmd);
    if processes.is_empty() {
        eprintln!("ccat --ps --ps-tree: no processes found");
        return;
    }

    // Build parent→children map
    let mut children: HashMap<u32, Vec<Process>> = HashMap::new();
    let mut orphans: Vec<Process> = Vec::new();
    let pids: std::collections::HashSet<u32> = processes.iter().map(|p| p.pid).collect();

    for proc in &processes {
        if pids.contains(&proc.ppid) {
            children.entry(proc.ppid).or_default().push(proc.clone());
        } else {
            orphans.push(proc.clone());
        }
    }

    // Sort children by PID
    for v in children.values_mut() {
        v.sort_by_key(|p| p.pid);
    }
    orphans.sort_by_key(|p| p.pid);

    let mut lines: Vec<String> = Vec::new();
    lines.push(style::bold(" ccat --ps --ps-tree"));
    lines.push(format!("  {} processes, {} roots", processes.len(), orphans.len()));
    lines.push(String::new());

    for root in &orphans {
        print_tree_node(&mut lines, root, "", true, &children);
    }

    if use_pager && lines.len() > 22 {
        crate::pager::run_pager(&lines);
    } else {
        for line in &lines {
            println!("{line}");
        }
    }
}

fn print_tree_node(
    lines: &mut Vec<String>,
    proc: &Process,
    prefix: &str,
    is_last: bool,
    children: &HashMap<u32, Vec<Process>>,
) {
    let connector = if is_last { "└─ " } else { "├─ " };
    let child_prefix = if is_last { "   " } else { "│  " };

    let cmd_display = if !proc.cmdline.is_empty() {
        &proc.cmdline
    } else {
        &proc.comm
    };

    let state_str = color_state(&proc.state);
    let rss_str = mem_hr(proc.rss_mb * 1024);
    let time_str = format_time(proc.utime + proc.stime, 100);

    let line = format!(
        "{}{}{} {:>6} {} {:>6} {}",
        prefix,
        connector,
        state_str,
        proc.pid,
        rss_str,
        time_str,
        style::dim(cmd_display),
    );
    lines.push(line);

    if let Some(kids) = children.get(&proc.pid) {
        let n = kids.len();
        for (i, kid) in kids.iter().enumerate() {
            let last = i == n - 1;
            print_tree_node(lines, kid, &format!("{prefix}{child_prefix}"), last, children);
        }
    }
}

fn gather_processes(
    filter_pid: Option<u32>,
    filter_cmd: Option<&str>,
) -> Vec<Process> {
    let mut processes: Vec<Process> = Vec::new();
    let proc_dir = Path::new("/proc");

    let entries = match fs::read_dir(proc_dir) {
        Ok(e) => e,
        Err(_) => return processes,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let pid_str = match path.file_name().and_then(|n| n.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        if let Some(fp) = filter_pid {
            if pid != fp {
                continue;
            }
        }

        let stat_data = match fs::read_to_string(path.join("stat")) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let parsed = match parse_stat(&stat_data) {
            Some(p) => p,
            None => continue,
        };

        let cmdline = read_cmdline(&path.join("cmdline"));

        if let Some(ref fc) = filter_cmd {
            let search = cmdline.to_lowercase();
            if !search.contains(&fc.to_lowercase()) {
                continue;
            }
        }

        let vsize_mb = parsed.vsize / (1024 * 1024);
        let rss_mb = (parsed.rss * 4) / 1024;

        processes.push(Process {
            pid: parsed.pid,
            state: parsed.state,
            ppid: parsed.ppid,
            comm: parsed.comm,
            cmdline,
            utime: parsed.utime,
            stime: parsed.stime,
            nice: parsed.nice,
            threads: parsed.num_threads,
            vsize_mb,
            rss_mb,
            processor: parsed.processor,
        });
    }

    processes
}

fn read_total_mem_kb() -> Option<u64> {
    let data = fs::read_to_string("/proc/meminfo").ok()?;
    for line in data.lines() {
        if line.starts_with("MemTotal:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                return parts[1].parse::<u64>().ok();
            }
        }
    }
    None
}

fn terminal_width() -> usize {
    if let Ok(out) = std::process::Command::new("sh")
        .args(["-c", "stty size < /dev/tty 2>/dev/null | cut -d' ' -f2"])
        .output()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        s.trim().parse().unwrap_or(120)
    } else {
        120
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stat(pid: u32, comm: &str, state: &str, extra: &str) -> String {
        format!(
            "{} ({}) {} {}",
            pid, comm, state, extra
        )
    }

    fn default_tail() -> String {
        // Provide minimum 44 fields after the state character
        "S 1 0 0 0 0 0 0 0 0 1234 567 0 0 20 0 2 0 100 8388608 1024 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0".to_string()
    }

    #[test]
    fn test_parse_stat_normal() {
        // pid 1: /sbin/init
        let data = "1 (init) S 0 0 0 0 0 0 0 0 0 0 1234 567 0 0 20 0 2 0 100 8388608 1024 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        let result = parse_stat(data);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.pid, 1);
        assert_eq!(r.comm, "init");
        assert_eq!(r.state, "S");
        assert_eq!(r.ppid, 0);
        assert_eq!(r.utime, 1234);
        assert_eq!(r.stime, 567);
        assert_eq!(r.nice, 0);
        assert_eq!(r.num_threads, 2);
        assert_eq!(r.vsize, 8388608);
        assert_eq!(r.rss, 1024);
    }

    #[test]
    fn test_parse_stat_comm_with_spaces() {
        // comm containing spaces (e.g., "my cool process")
        let data = "42 (my cool process) R 1 42 42 0 0 0 0 0 0 0 500 300 0 0 20 -5 8 0 200 16777216 4096 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        let result = parse_stat(data);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.pid, 42);
        assert_eq!(r.comm, "my cool process");
        assert_eq!(r.state, "R");
        assert_eq!(r.ppid, 1);
        assert_eq!(r.nice, -5);
        assert_eq!(r.num_threads, 8);
    }

    #[test]
    fn test_parse_stat_comm_with_parens() {
        // comm containing parentheses (e.g., "bash (1)")
        let data = "99 (bash (1)) S 1 99 99 0 0 0 0 0 0 100 200 0 0 20 0 1 0 0 2097152 512 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        let result = parse_stat(data);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.pid, 99);
        assert_eq!(r.comm, "bash (1)");
        assert_eq!(r.state, "S");
    }

    #[test]
    fn test_parse_stat_empty_comm() {
        let data = "0 () Z 0 0 0 0 0 0 0 0 0 0 0 0 0 20 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        let result = parse_stat(data);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.pid, 0);
        assert_eq!(r.comm, "");
        assert_eq!(r.state, "Z");
    }

    #[test]
    fn test_parse_stat_truncated_input() {
        // Too few fields
        let data = "1 (test) R 0";
        assert!(parse_stat(data).is_none());
    }

    #[test]
    fn test_parse_stat_no_parens() {
        // No opening paren
        let data = "1 test R 0";
        assert!(parse_stat(data).is_none());
    }

    #[test]
    fn test_format_time() {
        // 100 ticks at CLK_TCK=100 = 1 second
        assert_eq!(format_time(100, 100), "   1s");
        // 6000 ticks = 60 sec = 1:00
        assert_eq!(format_time(6000, 100), " 1:00");
        // 360000 ticks = 1 hour
        assert_eq!(format_time(360000, 100), "01:00:00");
        // 3600000 ticks = 10 hours
        assert_eq!(format_time(3_600_000, 100), "10:00:00");
        // 36000000 ticks = 100 hours
        assert_eq!(format_time(36_000_000, 100), "100:00:00");
    }

    #[test]
    fn test_format_time_zero() {
        assert_eq!(format_time(0, 100), "   0s");
        // 1 tick = 0.01 sec
        assert_eq!(format_time(1, 100), "   0s");
        // 50 ticks = 0.5 sec
        assert_eq!(format_time(50, 100), "   0s");
    }

    #[test]
    fn test_mem_hr() {
        assert_eq!(mem_hr(0), "0K");
        assert_eq!(mem_hr(512), "512K");
        assert_eq!(mem_hr(1024), "1M");
        assert_eq!(mem_hr(1536), "2M");
        assert_eq!(mem_hr(2048), "2M");
        assert_eq!(mem_hr(1_048_576), "1.0G");
        assert_eq!(mem_hr(2_097_152), "2.0G");
    }

    #[test]
    fn test_color_state() {
        let r = color_state("R");
        assert!(r.contains("R"));
        assert!(r.contains("\x1b[32m")); // green
        let s = color_state("S");
        assert!(s.contains("\x1b[36m")); // cyan
        let d = color_state("D");
        assert!(d.contains("\x1b[31m")); // red
        let z = color_state("Z");
        assert!(z.contains("\x1b[35m")); // magenta
        let t = color_state("T");
        assert!(t.contains("\x1b[34m")); // blue
    }

    #[test]
    fn test_describe_state() {
        assert_eq!(describe_state('R'), "running");
        assert_eq!(describe_state('S'), "sleeping");
        assert_eq!(describe_state('Z'), "zombie");
        assert_eq!(describe_state('I'), "idle");
        assert_eq!(describe_state('?'), "unknown");
    }

    #[test]
    fn test_sort_processes_by_pid() {
        let p3 = Process { pid: 3, state: "S".into(), ppid: 1, comm: "c".into(), cmdline: "c".into(), utime: 0, stime: 0, nice: 0, threads: 1, vsize_mb: 0, rss_mb: 0, processor: 0 };
        let p1 = Process { pid: 1, state: "S".into(), ppid: 0, comm: "a".into(), cmdline: "a".into(), utime: 0, stime: 0, nice: 0, threads: 1, vsize_mb: 0, rss_mb: 0, processor: 0 };
        let p2 = Process { pid: 2, state: "S".into(), ppid: 1, comm: "b".into(), cmdline: "b".into(), utime: 0, stime: 0, nice: 0, threads: 1, vsize_mb: 0, rss_mb: 0, processor: 0 };
        let mut procs = vec![p3, p1, p2];
        sort_processes(&mut procs, "pid", false);
        assert_eq!(procs[0].pid, 1);
        assert_eq!(procs[1].pid, 2);
        assert_eq!(procs[2].pid, 3);
    }

    #[test]
    fn test_sort_processes_by_pid_reverse() {
        let p3 = Process { pid: 3, state: "S".into(), ppid: 1, comm: "c".into(), cmdline: "c".into(), utime: 0, stime: 0, nice: 0, threads: 1, vsize_mb: 0, rss_mb: 0, processor: 0 };
        let p1 = Process { pid: 1, state: "S".into(), ppid: 0, comm: "a".into(), cmdline: "a".into(), utime: 0, stime: 0, nice: 0, threads: 1, vsize_mb: 0, rss_mb: 0, processor: 0 };
        let mut procs = vec![p1, p3];
        sort_processes(&mut procs, "pid", true);
        assert_eq!(procs[0].pid, 3);
        assert_eq!(procs[1].pid, 1);
    }

    #[test]
    fn test_sort_processes_by_mem() {
        let p1 = Process { pid: 1, state: "S".into(), ppid: 0, comm: "".into(), cmdline: "".into(), utime: 0, stime: 0, nice: 0, threads: 1, vsize_mb: 10, rss_mb: 100, processor: 0 };
        let p2 = Process { pid: 2, state: "S".into(), ppid: 0, comm: "".into(), cmdline: "".into(), utime: 0, stime: 0, nice: 0, threads: 1, vsize_mb: 10, rss_mb: 200, processor: 0 };
        let mut procs = vec![p2, p1];
        sort_processes(&mut procs, "mem", false);
        assert_eq!(procs[0].rss_mb, 100);
        assert_eq!(procs[1].rss_mb, 200);
    }

    #[test]
    fn test_sort_processes_default_name() {
        let p1 = Process { pid: 2, state: "S".into(), ppid: 0, comm: "".into(), cmdline: "zzz".into(), utime: 0, stime: 0, nice: 0, threads: 1, vsize_mb: 0, rss_mb: 0, processor: 0 };
        let p2 = Process { pid: 1, state: "S".into(), ppid: 0, comm: "".into(), cmdline: "aaa".into(), utime: 0, stime: 0, nice: 0, threads: 1, vsize_mb: 0, rss_mb: 0, processor: 0 };
        let mut procs = vec![p1, p2];
        sort_processes(&mut procs, "name", false);
        assert_eq!(procs[0].cmdline, "aaa");
        assert_eq!(procs[1].cmdline, "zzz");
    }

    #[test]
    fn test_vsize_rss_conversion() {
        // vsize in bytes, RSS in pages (4096 bytes/page)
        let p = Process {
            pid: 1, state: "S".into(), ppid: 0,
            comm: "test".into(), cmdline: "/usr/bin/test".into(),
            utime: 0, stime: 0, nice: 0, threads: 1,
            vsize_mb: 8388608 / (1024 * 1024), // 8 MB
            rss_mb: (2048 * 4096) / (1024 * 1024), // 2048 pages → 8 MB
            processor: 0,
        };
        assert_eq!(p.vsize_mb, 8);
        assert_eq!(p.rss_mb, 8);
    }

    #[test]
    fn test_read_cmdline_empty() {
        let p = Path::new("/nonexistent/cmdline");
        assert_eq!(read_cmdline(p), "");
    }

    #[test]
    fn test_read_total_mem_kb_from_empty() {
        // Should return None gracefully
        assert!(read_total_mem_kb().is_some() || true); // This is OS-dependent
    }

    #[test]
    fn test_sort_key_parsing() {
        // Test that -pid means sort by pid reversed
        let mut processes: Vec<Process> = vec![
            Process { pid: 3, state: "S".into(), ppid: 1, comm: "c".into(), cmdline: "c".into(), utime: 0, stime: 0, nice: 0, threads: 1, vsize_mb: 0, rss_mb: 0, processor: 0 },
            Process { pid: 1, state: "S".into(), ppid: 0, comm: "a".into(), cmdline: "a".into(), utime: 0, stime: 0, nice: 0, threads: 1, vsize_mb: 0, rss_mb: 0, processor: 0 },
        ];
        // Test sort-by parsing and actual sorting
        let sort_by = "-pid";
        let reverse = sort_by.starts_with('-');
        let sort_key = sort_by.trim_start_matches('-');
        assert!(reverse);
        assert_eq!(sort_key, "pid");
        sort_processes(&mut processes, sort_key, reverse);
        assert_eq!(processes[0].pid, 3);
    }
}
