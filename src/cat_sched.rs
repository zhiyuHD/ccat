//! Linux Scheduler Analyzer (`ccat --sched`).
//!
//! Reads scheduler state from /proc and /sys to present a comprehensive
//! view of the Linux CFS/RT scheduler:
//!
//! - Scheduling policy distribution (OTHER/FIFO/RR/BATCH/IDLE/DEADLINE)
//! - Per-task scheduling statistics (vruntime, migrations, preemptions)
//! - Involuntary preemption leaders — processes suffering most contention
//! - Cgroup CPU bandwidth/throttling
//! - Scheduler tunables
//! - Real-time (FIFO) tasks with priorities
//! - Tasks with non-default nice values
//! - Kernel/scheduler feature overview

use std::collections::HashMap;
use std::fs;

// ── Colour helpers (matching cat_oom convention) ──

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
    pub fn magenta(s: impl AsRef<str>) -> String { format!("\x1b[35m{}\x1b[0m", s.as_ref()) }
    pub fn blue(s: impl AsRef<str>) -> String   { format!("\x1b[34m{}\x1b[0m", s.as_ref()) }
}

// ── Data types ──

/// Parsed scheduler data for a single process.
#[derive(Debug, Clone)]
struct SchedInfo {
    pid: u32,
    comm: String,
    /// Scheduling policy: 0=OTHER, 1=FIFO, 2=RR, 3=BATCH, 5=IDLE, 6=DEADLINE, 7=EXT
    policy: u32,
    /// static priority (0-139; 0-99 RT, 100-139 CFS/nice)
    prio: u32,
    /// nice value (-20..19, computed from prio)
    nice: i32,
    /// CFS vruntime in nanoseconds (approx — kernel uses nanosec granularity)
    vruntime: f64,
    /// Total CPU time consumed in milliseconds
    runtime_ms: f64,
    /// Number of context switches (total)
    nr_switches: u64,
    /// Voluntary context switches
    nr_voluntary: u64,
    /// Involuntary context switches (preemptions)
    nr_involuntary: u64,
    /// Number of migrations between CPUs
    nr_migrations: u64,
    /// CFS time slice in milliseconds
    slice_ms: f64,
    /// se.load.weight (NICE_0_LOAD = 1048576 internally)
    load_weight: u64,
    /// PELT util_avg (0-1024, scaled by 1024 for full utilization of one CPU)
    util_avg: f64,
}

/// Policy name helper.
fn policy_name(policy: u32) -> &'static str {
    match policy {
        0 => "OTHER",
        1 => "FIFO",
        2 => "RR",
        3 => "BATCH",
        5 => "IDLE",
        6 => "DEADLINE",
        7 => "EXT",
        _ => "?",
    }
}

// ── Reading helpers ──

/// Read and parse /proc/<pid>/sched for scheduling statistics.
fn read_sched(pid: u32) -> Option<SchedInfo> {
    let content = fs::read_to_string(format!("/proc/{pid}/sched")).ok()?;
    let mut map: HashMap<String, String> = HashMap::new();
    for line in content.lines() {
        if let Some(pos) = line.find(':') {
            let key = line[..pos].trim().to_string();
            let val = line[pos + 1..].trim().to_string();
            map.insert(key, val);
        }
    }

    // Extract comm from first line: "comm (pid, #threads: N)"
    let comm = content
        .lines()
        .next()
        .and_then(|l| l.split('(').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "?".to_string());

    let parse_f64 = |k: &str| map.get(k).and_then(|v| v.parse::<f64>().ok());
    let parse_u64 = |k: &str| map.get(k).and_then(|v| v.parse::<u64>().ok());
    let parse_u32 = |k: &str| map.get(k).and_then(|v| v.parse::<u32>().ok());

    let policy = parse_u32("policy").unwrap_or(0);
    let prio = parse_u32("prio").unwrap_or(120);
    let nice = prio as i32 - 120;

    Some(SchedInfo {
        pid,
        comm,
        policy,
        prio,
        nice,
        vruntime: parse_f64("se.vruntime").unwrap_or(0.0),
        runtime_ms: parse_f64("se.sum_exec_runtime").unwrap_or(0.0),
        nr_switches: parse_u64("nr_switches").unwrap_or(0),
        nr_voluntary: parse_u64("nr_voluntary_switches").unwrap_or(0),
        nr_involuntary: parse_u64("nr_involuntary_switches").unwrap_or(0),
        nr_migrations: parse_u64("se.nr_migrations").unwrap_or(0),
        slice_ms: parse_f64("se.slice").unwrap_or(0.0) / 1_000_000.0,
        load_weight: parse_u64("se.load.weight").unwrap_or(0),
        util_avg: parse_f64("se.avg.util_avg").unwrap_or(0.0),
    })
}

/// Read /proc/loadavg
fn read_loadavg() -> String {
    fs::read_to_string("/proc/loadavg")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Read a /proc/stat field
fn read_proc_stat_field(prefix: &str) -> u64 {
    let content = match fs::read_to_string("/proc/stat") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    for line in content.lines() {
        if line.starts_with(prefix) {
            let val = line.split_whitespace().nth(1).unwrap_or("0");
            return val.parse().unwrap_or(0);
        }
    }
    0
}

/// Read a sysctl value from /proc/sys/kernel/
fn read_sysctl(name: &str) -> Option<String> {
    fs::read_to_string(format!("/proc/sys/kernel/{name}"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Read /sys/fs/cgroup/cpu.stat
fn read_cgroup_cpu_stat() -> HashMap<String, u64> {
    let mut map = HashMap::new();
    let content = match fs::read_to_string("/sys/fs/cgroup/cpu.stat") {
        Ok(s) => s,
        Err(_) => return map,
    };
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 2 {
            if let Ok(val) = parts[1].parse::<u64>() {
                map.insert(parts[0].to_string(), val);
            }
        }
    }
    map
}

/// Read the current autogroup nice value for a process
fn read_autogroup_nice(pid: u32) -> Option<i32> {
    let content = fs::read_to_string(format!("/proc/{pid}/autogroup")).ok()?;
    // Format: "/autogroup-XXX nice Y"
    content.split_whitespace().last().and_then(|s| s.parse().ok())
}

/// Read the kernel version string
fn read_kernel_version() -> String {
    fs::read_to_string("/proc/version")
        .map(|s| {
            let v = s.trim().to_string();
            // Take just the first two words
            v.split_whitespace().take(3).collect::<Vec<_>>().join(" ")
        })
        .unwrap_or_else(|_| "?".to_string())
}

/// Read CPU count from /proc/cpuinfo
fn read_cpu_count() -> usize {
    let content = match fs::read_to_string("/proc/cpuinfo") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    content.lines()
        .filter(|l| l.starts_with("processor"))
        .count()
}

/// Format seconds as a human-friendly duration
fn human_duration(secs: f64) -> String {
    if secs < 60.0 {
        format!("{:.0}s", secs)
    } else if secs < 3600.0 {
        format!("{:.0}m {:.0}s", secs / 60.0, secs % 60.0)
    } else if secs < 86400.0 {
        format!("{:.0}h {:.0}m", secs / 3600.0, (secs % 3600.0) / 60.0)
    } else {
        format!("{:.1}d", secs / 86400.0)
    }
}

/// Format a number with comma separators
fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    let mut count = 0;
    for c in s.chars().rev() {
        if count > 0 && count % 3 == 0 {
            result.insert(0, ',');
        }
        result.insert(0, c);
        count += 1;
    }
    result
}

fn fmt_num_f64(n: f64) -> String {
    fmt_num(n as u64)
}

// ── Section formatters ──

fn print_header(title: &str) {
    println!();
    println!("{}", style::bold(&format!("── {} ──", title)));
}

fn print_subheader(title: &str) {
    println!("{}", style::cyan(&format!("▸ {}", title)));
}

/// Format a table row with aligned columns
fn print_row(label: &str, value: String) {
    println!("  {:30} {}", label, value);
}

// ── Main entry point ──

/// Run the scheduler analysis and print results to stdout.
pub fn cat_sched() {
    let kernel = read_kernel_version();
    let cpu_count = read_cpu_count();

    let preempt = read_sysctl("sched_energy_aware")
        .map(|_| {
            // Check for PREEMPT model
            let content = fs::read_to_string("/proc/version").unwrap_or_default();
            if content.contains("PREEMPT_DYNAMIC") { "DYNAMIC" }
            else if content.contains("PREEMPT_RT") { "RT" }
            else if content.contains("PREEMPT") { "VOLUNTARY" }
            else { "NONE" }
        })
        .unwrap_or("?");

    let sched_ext = fs::read_to_string("/sys/kernel/sched_ext/state")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "not compiled".to_string());

    let autogroup = read_sysctl("sched_autogroup_enabled")
        .map(|v| if v == "1" { "enabled" } else { "disabled" })
        .unwrap_or("?");

    // ── Phase 1: Gather all process scheduler data ──
    let mut all_procs: Vec<SchedInfo> = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let pid_str = entry.file_name();
            let pid: u32 = match pid_str.to_str().and_then(|s| s.parse().ok()) {
                Some(p) => p,
                None => continue,
            };
            if let Some(info) = read_sched(pid) {
                all_procs.push(info);
            }
        }
    }

    // Sort by PID for stable display
    all_procs.sort_by_key(|p| p.pid);

    // ── Phase 2: Compute statistics ──
    let total_tasks = all_procs.len();

    // Count by policy
    let mut policy_counts: HashMap<u32, usize> = HashMap::new();
    for p in &all_procs {
        *policy_counts.entry(p.policy).or_insert(0) += 1;
    }

    // Summary stats
    let ctx_sw_total = read_proc_stat_field("ctxt");
    let procs_created = read_proc_stat_field("processes");
    let procs_running = read_proc_stat_field("procs_running");
    let procs_blocked = read_proc_stat_field("procs_blocked");
    let loadavg = read_loadavg();

    // Involuntary switch ratio top
    let mut invol_leader: Vec<&SchedInfo> = all_procs.iter()
        .filter(|p| p.nr_switches > 50)
        .collect();
    invol_leader.sort_by(|a, b| {
        let ratio_a = a.nr_involuntary as f64 / a.nr_switches.max(1) as f64;
        let ratio_b = b.nr_involuntary as f64 / b.nr_switches.max(1) as f64;
        ratio_b.partial_cmp(&ratio_a).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Migration leaders
    let mut mig_leader: Vec<&SchedInfo> = all_procs.iter().collect();
    mig_leader.sort_by(|a, b| b.nr_migrations.cmp(&a.nr_migrations));

    // FIFO tasks
    let fifo_tasks: Vec<&SchedInfo> = all_procs.iter()
        .filter(|p| p.policy == 1)
        .collect();

    // Non-default nice tasks (excluding kernel threads with nice from FIFO mapping)
    let nondefault_nice: Vec<&SchedInfo> = all_procs.iter()
        .filter(|p| p.policy == 0 && p.nice != 0)
        .collect();

    // Cgroup CPU stats
    let cgroup_cpu = read_cgroup_cpu_stat();

    // ── Phase 3: Output ──

    // ── Header ──
    println!();
    println!("{}", style::bold("Linux Scheduler Analyzer"));
    println!("{}", style::grey(&format!("{} | {} vCPU | PREEMPT {} | sched_ext: {} | autogroup: {}",
        kernel, cpu_count, preempt, sched_ext, autogroup)));

    // ── 1. Policy distribution ──
    print_header("Scheduling Policy Distribution");
    let policy_order = [0, 1, 2, 3, 5, 6, 7];
    for pol in &policy_order {
        let count = policy_counts.get(pol).copied().unwrap_or(0);
        let pct = if total_tasks > 0 { count as f64 / total_tasks as f64 * 100.0 } else { 0.0 };
        let pname = policy_name(*pol);
        let colored = match pol {
            1 => style::red(&format!("{} ({})", pname, count)),
            2 => style::orange(&format!("{} ({})", pname, count)),
            6 => style::magenta(&format!("{} ({})", pname, count)),
            7 => style::blue(&format!("{} ({})", pname, count)),
            _ => style::green(&format!("{} ({})", pname, count)),
        };
        println!("  {:>12}: {:>4} ({:>4.1}%)", colored, count, pct);
    }

    // ── 2. Load overview ──
    print_header("System Load");
    println!("  {:30} {}", "Load average (1/5/15m)", loadavg);
    println!("  {:30} {}", "Running / Total processes",
        format!("{} / {}", procs_running, total_tasks));
    println!("  {:30} {}", "Blocked (D state)", procs_blocked);
    println!("  {:30} {}", "Context switches (total)", fmt_num(ctx_sw_total));
    println!("  {:30} {}", "Processes created (total)", fmt_num(procs_created));
    let proc_rate = if procs_created > 0 {
        let uptime_secs = fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .unwrap_or(1.0);
        format!("{:.1}/s", procs_created as f64 / uptime_secs)
    } else {
        "?".to_string()
    };
    println!("  {:30} {}", "Process creation rate", proc_rate);

    // ── 3. Scheduler tunables ──
    print_header("Scheduler Tunables");
    let tunables: Vec<(&str, &str)> = vec![
        ("CFS bandwidth slice", "sched_cfs_bandwidth_slice_us"),
        ("RR timeslice", "sched_rr_timeslice_ms"),
        ("RT period", "sched_rt_period_us"),
        ("RT runtime (per period)", "sched_rt_runtime_us"),
        ("Util clamp min", "sched_util_clamp_min"),
        ("Util clamp max", "sched_util_clamp_max"),
        ("Schedstats", "sched_schedstats"),
    ];
    let mut has_any_tunable = false;
    for (label, sysctl) in &tunables {
        if let Some(val) = read_sysctl(sysctl) {
            let formatted = match *sysctl {
                "sched_cfs_bandwidth_slice_us" => format!("{} µs", val),
                "sched_rr_timeslice_ms" => format!("{} ms", val),
                "sched_rt_period_us" => format!("{} µs ({} s)", val, val.parse::<f64>().map(|v| v/1_000_000.0).unwrap_or(0.0)),
                "sched_rt_runtime_us" => {
                    let v = val.parse::<f64>().unwrap_or(0.0);
                    let pct = v / 1_000_000.0 * 100.0;
                    format!("{} µs ({:.1}%)", val, pct)
                }
                "sched_util_clamp_min" | "sched_util_clamp_max" => {
                    let v = val.parse::<f64>().unwrap_or(0.0);
                    let pct = v / 1024.0 * 100.0;
                    format!("{} ({:.0}%)", val, pct)
                }
                "sched_schedstats" => {
                    if val == "0" { "disabled".to_string() } else { "enabled".to_string() }
                }
                _ => val,
            };
            println!("  {:30} {}", label, formatted);
            has_any_tunable = true;
        }
    }
    if !has_any_tunable {
        println!("  {} (none available)", style::dim("(no tunables readable)"));
    }

    // ── 4. CFS Bandwidth / Cgroup CPU ──
    print_header("CFS Bandwidth (root cgroup)");
    let usage_sec = *cgroup_cpu.get("usage_usec").unwrap_or(&0) as f64 / 1_000_000.0;
    let user_sec = *cgroup_cpu.get("user_usec").unwrap_or(&0) as f64 / 1_000_000.0;
    let sys_sec = *cgroup_cpu.get("system_usec").unwrap_or(&0) as f64 / 1_000_000.0;
    let throttled = *cgroup_cpu.get("nr_throttled").unwrap_or(&0);
    let throttled_usec = *cgroup_cpu.get("throttled_usec").unwrap_or(&0) as f64 / 1_000_000.0;
    let nr_periods = *cgroup_cpu.get("nr_periods").unwrap_or(&0);
    let nice_usec = *cgroup_cpu.get("nice_usec").unwrap_or(&0) as f64 / 1_000_000.0;

    println!("  {:30} {}", "Total CPU usage", human_duration(usage_sec));
    println!("  {:30} {}", "  user", human_duration(user_sec));
    if nice_usec > 0.0 {
        println!("  {:30} {}", "  nice (reniced tasks)", human_duration(nice_usec));
    }
    println!("  {:30} {}", "  system (kernel)", human_duration(sys_sec));
    if nr_periods > 0 {
        let throttled_pct = if nr_periods > 0 {
            throttled as f64 / nr_periods as f64 * 100.0
        } else {
            0.0
        };
        println!("  {:30} {} / {} ({:.1}%)", "Throttled periods", throttled, nr_periods, throttled_pct);
        if throttled_usec > 0.0 {
            println!("  {:30} {}", "  total throttled time", human_duration(throttled_usec));
        }
    } else {
        println!("  {:30} {}", "Throttled periods", style::green("none (no CFS bandwidth limit set)"));
    }

    // ── 5. Involuntary preemption leaders ──
    print_header("Involuntary Preemption Leaders");
    println!("{}", style::dim("  Processes with highest involuntary-to-voluntary switch ratio."));
    println!("{}", style::dim("  High ratio → frequent preemption, possible CPU contention."));
    println!();
    println!("  {:>6} {:24} {:>12} {:>10} {:>10} {:>5}", "PID", "COMM", "INVOLUNTARY", "TOTAL SW", "RATIO", "MIG");
    for p in invol_leader.iter().take(10) {
        let ratio = if p.nr_switches > 0 {
            p.nr_involuntary as f64 / p.nr_switches as f64 * 100.0
        } else {
            0.0
        };
        let ratio_str = if ratio > 50.0 {
            style::red(&format!("{:>5.1}%", ratio))
        } else if ratio > 20.0 {
            style::yellow(&format!("{:>5.1}%", ratio))
        } else {
            style::green(&format!("{:>5.1}%", ratio))
        };
        println!("  {:>6} {:24} {:>12} {:>10} {}", p.pid, p.comm, fmt_num(p.nr_involuntary), fmt_num(p.nr_switches), ratio_str);
    }

    // ── 6. Most migrated tasks ──
    print_header("Top Migrated Tasks");
    println!("{}", style::dim("  Processes with most CPU migrations. High = NUMA/cache overhead."));
    println!();
    println!("  {:>6} {:24} {:>10} {:>10} {:>8}", "PID", "COMM", "MIGRATIONS", "SWITCHES", "MIG/SW%");
    for p in mig_leader.iter().take(10) {
        let mig_pct = if p.nr_switches > 0 {
            p.nr_migrations as f64 / p.nr_switches as f64 * 100.0
        } else {
            0.0
        };
        let pct_str = if mig_pct > 10.0 {
            style::yellow(&format!("{:>7.1}%", mig_pct))
        } else {
            style::green(&format!("{:>7.1}%", mig_pct))
        };
        println!("  {:>6} {:24} {:>10} {:>10} {}", p.pid, p.comm, fmt_num(p.nr_migrations), fmt_num(p.nr_switches), pct_str);
    }

    // ── 7. FIFO (real-time) tasks ──
    if !fifo_tasks.is_empty() {
        print_header("Real-Time (SCHED_FIFO) Tasks");
        println!("{}", style::dim("  Tasks with real-time scheduling policy and their static priorities."));
        println!("{}", style::dim("  Priority 0 = highest RT priority, 99 = lowest."));
        println!();
        println!("  {:>6} {:24} {:>6} {:>10} {:>10}", "PID", "COMM", "PRIO", "RUNTIME", "SWITCHES");
        for p in &fifo_tasks {
            let prio_color = if p.prio < 50 {
                style::red(&p.prio.to_string())
            } else if p.prio < 80 {
                style::yellow(&p.prio.to_string())
            } else {
                style::green(&p.prio.to_string())
            };
            println!("  {:>6} {:24} {} {:>10.1}s {:>10}",
                p.pid, p.comm, prio_color, p.runtime_ms / 1000.0, fmt_num(p.nr_switches));
        }
    }

    // ── 8. Non-default nice values ──
    if !nondefault_nice.is_empty() {
        print_header("Fair Tasks with Non-Default Nice");
        println!("{}", style::dim("  SCHED_OTHER tasks running at nice != 0. Negative = higher priority."));
        println!();
        println!("  {:>6} {:24} {:>5} {:>10} {:>10} {:>8}", "PID", "COMM", "NICE", "RUNTIME", "SWITCHES", "MIG");
        for p in nondefault_nice.iter().take(15) {
            let nice_str = if p.nice < 0 {
                style::yellow(&format!("{:>4}", p.nice))
            } else if p.nice > 0 {
                style::dim(&format!("{:>4}", p.nice))
            } else {
                format!("{:>4}", p.nice)
            };
            println!("  {:>6} {:24} {} {:>10.0}s {:>10} {:>8}",
                p.pid, p.comm, nice_str, p.runtime_ms / 1000.0, fmt_num(p.nr_switches), fmt_num(p.nr_migrations));
        }
    }

    // ── 9. Insights ──
    print_header("Scheduler Insights");

    // Find the most preempted task
    if let Some(worst) = invol_leader.first() {
        if worst.nr_switches > 100 {
            let ratio = worst.nr_involuntary as f64 / worst.nr_switches.max(1) as f64 * 100.0;
            println!("  {} {} ({}) is preempted {:.1}% of the time — highest contention.",
                style::red("⚠ Most contended:"), style::bold(&worst.comm), worst.pid, ratio);
        }
    }

    // Check for RT throttling
    let rt_runtime = read_sysctl("sched_rt_runtime_us")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    let rt_period = read_sysctl("sched_rt_period_us")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1_000_000.0);
    if rt_runtime < rt_period {
        let pct = rt_runtime / rt_period * 100.0;
        println!("  {} RT tasks limited to {:.1}% CPU (sched_rt_runtime_us={:.0}, period={:.0})",
            style::green("✓"), pct, rt_runtime, rt_period);
    }

    // FIFO task count
    if !fifo_tasks.is_empty() {
        println!("  {} {} FIFO tasks — {} kernel, {} user",
            style::blue("ℹ"),
            fifo_tasks.len(),
            fifo_tasks.iter().filter(|p| p.pid < 1000).count(),
            fifo_tasks.iter().filter(|p| p.pid >= 1000).count(),
        );
    }

    // Check if schedstats is disabled (better performance)
    let schedstats = read_sysctl("sched_schedstats");
    if schedstats.as_deref() == Some("0") {
        println!("  {} schedstats disabled (lower overhead, but /proc/<pid>/sched still works via CONFIG_SCHED_DEBUG)",
            style::green("✓"));
    }

    // Check migration costs
    let total_migrations: u64 = all_procs.iter().map(|p| p.nr_migrations).sum();
    let avg_migrations = if total_tasks > 0 { total_migrations as f64 / total_tasks as f64 } else { 0.0 };
    println!("  {} {} total CPU migrations across {} tasks (avg {:.0}/task)",
        style::blue("ℹ"),
        fmt_num(total_migrations), total_tasks, avg_migrations);

    // Average involuntary ratio
    let avg_invol: f64 = all_procs.iter()
        .filter(|p| p.nr_switches > 0)
        .map(|p| p.nr_involuntary as f64 / p.nr_switches as f64 * 100.0)
        .sum::<f64>()
        / all_procs.iter().filter(|p| p.nr_switches > 0).count().max(1) as f64;
    let avg_invol_str = if avg_invol > 20.0 {
        style::yellow(&format!("{:.1}%", avg_invol))
    } else if avg_invol > 5.0 {
        format!("{:.1}%", avg_invol)
    } else {
        style::green(&format!("{:.1}%", avg_invol))
    };
    println!("  {} Average involuntary preemption ratio across all tasks: {}",
        style::blue("ℹ"), avg_invol_str);

    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_policy_name_other() {
        assert_eq!(policy_name(0), "OTHER");
    }

    #[test]
    fn test_policy_name_fifo() {
        assert_eq!(policy_name(1), "FIFO");
    }

    #[test]
    fn test_policy_name_rr() {
        assert_eq!(policy_name(2), "RR");
    }

    #[test]
    fn test_policy_name_batch() {
        assert_eq!(policy_name(3), "BATCH");
    }

    #[test]
    fn test_policy_name_idle() {
        assert_eq!(policy_name(5), "IDLE");
    }

    #[test]
    fn test_policy_name_deadline() {
        assert_eq!(policy_name(6), "DEADLINE");
    }

    #[test]
    fn test_policy_name_ext() {
        assert_eq!(policy_name(7), "EXT");
    }

    #[test]
    fn test_policy_name_unknown() {
        assert_eq!(policy_name(99), "?");
    }

    #[test]
    fn test_human_duration_seconds() {
        assert_eq!(human_duration(30.0), "30s");
    }

    #[test]
    fn test_human_duration_minutes() {
        assert_eq!(human_duration(125.0), "2m 5s");
    }

    #[test]
    fn test_human_duration_hours() {
        assert_eq!(human_duration(3661.0), "1h 1m");
    }

    #[test]
    fn test_human_duration_days() {
        let result = human_duration(90000.0);
        assert!(result.contains("1."));
        assert!(result.contains("d"));
    }

    #[test]
    fn test_fmt_num_small() {
        assert_eq!(fmt_num(0), "0");
        assert_eq!(fmt_num(42), "42");
    }

    #[test]
    fn test_fmt_num_thousands() {
        assert_eq!(fmt_num(1000), "1,000");
        assert_eq!(fmt_num(1234567), "1,234,567");
    }

    #[test]
    fn test_fmt_num_f64() {
        assert_eq!(fmt_num_f64(1234567.0), "1,234,567");
    }

    #[test]
    fn test_read_kernel_version_returns_something() {
        let v = read_kernel_version();
        assert!(!v.is_empty(), "kernel version should not be empty");
        assert!(v.len() > 3, "should be a reasonable version string");
    }

    #[test]
    fn test_cpu_count_positive() {
        let count = read_cpu_count();
        assert!(count > 0, "should detect at least 1 CPU");
    }

    #[test]
    fn test_read_sysctl_existing() {
        let val = read_sysctl("sched_autogroup_enabled");
        assert!(val.is_some(), "sched_autogroup_enabled should exist");
        if let Some(v) = val {
            assert!((v == "0" || v == "1"), "autogroup should be 0 or 1");
        }
    }

    #[test]
    fn test_read_sysctl_nonexistent() {
        let val = read_sysctl("nonexistent_tunable_xyz");
        assert!(val.is_none());
    }

    #[test]
    fn test_read_loadavg_format() {
        let la = read_loadavg();
        assert!(la.contains('/'), "loadavg should contain / separator");
    }

    #[test]
    fn test_read_sched_pid1() {
        let info = read_sched(1);
        assert!(info.is_some(), "PID 1 should have sched info");
        if let Some(i) = info {
            assert_eq!(i.pid, 1);
            assert!(i.nr_switches > 0, "PID 1 should have context switches");
        }
    }

    #[test]
    fn test_proc_stat_field_context_switches() {
        let cs = read_proc_stat_field("ctxt");
        assert!(cs > 0, "should have some context switches");
    }

    #[test]
    fn test_read_cgroup_cpu_stat_has_usage() {
        let stat = read_cgroup_cpu_stat();
        assert!(stat.contains_key("usage_usec"), "cpu.stat should have usage_usec");
        if let Some(usage) = stat.get("usage_usec") {
            assert!(*usage > 0, "CPU usage should be positive");
        }
    }

    #[test]
    fn test_read_autogroup_self() {
        let nice = read_autogroup_nice(std::process::id());
        // autogroup could be anything, but should parse
        assert!(nice.is_some(), "autogroup should be readable for self");
    }

    #[test]
    fn test_nice_from_prio() {
        assert_eq!(120i32 - 120, 0);
        assert_eq!(100i32 - 120, -20);
        assert_eq!(139i32 - 120, 19);
    }

    #[test]
    fn test_read_sched_chrome_or_system_process() {
        // Should be able to read sched for at least some processes
        let count: usize = (1..=10)
            .filter_map(|pid| read_sched(pid))
            .count();
        assert!(count > 0, "should read sched for at least some of PIDs 1-10");
    }

    #[test]
    fn test_read_sched_nonexistent_pid() {
        let info = read_sched(999_999_999);
        assert!(info.is_none(), "bogus PID should return None");
    }

    #[test]
    fn test_read_loadavg_parses() {
        let la = read_loadavg();
        // Format: "0.04 0.11 0.09 1/397 1620205"
        let parts: Vec<&str> = la.split_whitespace().collect();
        assert_eq!(parts.len(), 5, "loadavg should have 5 fields");
        let running_total: Vec<&str> = parts[3].split('/').collect();
        assert_eq!(running_total.len(), 2, "loadavg should have running/total");
    }

    #[test]
    fn test_read_sched_field_consistency() {
        // Verify that non-kernel processes have reasonable values
        if let Some(info) = read_sched(1) {
            assert!(info.runtime_ms > 0.0, "PID 1 should have runtime");
            assert!(info.slice_ms > 0.0, "PID 1 should have a time slice");
            assert!(info.load_weight > 0, "should have load weight");
        }
    }
}
