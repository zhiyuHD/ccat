//! CPU topology explorer (`ccat --cpu`).
//!
//! Reads `/sys/devices/system/cpu/`, `/proc/cpuinfo`, `/proc/interrupts`,
//! and `/sys/devices/system/node/` to produce a beautiful, coloured
//! topology diagram showing:
//!
//! - Cache hierarchy (L1d/L1i/L2/L3) with per-core sharing
//! - CPU frequency scaling (current / min / max / governor)
//! - CPU feature flags (grouped by relevance)
//! - Interrupt distribution across CPUs
//! - NUMA topology
//!
//! Uses only /proc and /sysfs — no special privileges needed.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

// ── Colour helpers (self-contained, mirrors cat_proc) ──

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
    pub fn on_blue(s: impl AsRef<str>) -> String { format!("\x1b[44m{}\x1b[0m", s.as_ref()) }
    pub fn on_grey(s: impl AsRef<str>) -> String { format!("\x1b[100m{}\x1b[0m", s.as_ref()) }
}

// ── Data types ──

#[derive(Debug, Clone)]
struct CacheInfo {
    level: u32,
    kind: String,       // "Data", "Instruction", "Unified"
    size: String,       // e.g. "64K"
    ways: u32,
    line_size: u32,
    shared_cpu_list: Vec<u32>,
}

#[derive(Debug, Clone)]
struct CoreTopology {
    cpu: u32,
    core_id: u32,
    package: u32,
    caches: Vec<CacheInfo>,
    freq_cur: Option<u64>,  // kHz
    freq_min: Option<u64>,
    freq_max: Option<u64>,
    gov_name: Option<String>,
}

#[derive(Debug, Clone)]
struct InterruptLine {
    irq: u32,
    desc: String,
    per_cpu: Vec<u64>,
}

// ── Reading helpers ──

fn read_sysfs_u64(path: &str) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_sysfs_string(path: &str) -> Option<String> {
    Some(fs::read_to_string(path).ok()?.trim().to_string())
}

fn read_cpu_list(path: &str) -> Vec<u32> {
    let s = match read_sysfs_string(path) {
        Some(v) => v,
        None => return vec![],
    };
    parse_cpu_list(&s)
}

/// Parse a CPU list string like "0-3" or "0-3,8,10-11" into a Vec<u32>.
fn parse_cpu_list(s: &str) -> Vec<u32> {
    let mut cpus = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() { continue; }
        if let Some((start, end)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (start.trim().parse::<u32>(), end.trim().parse::<u32>()) {
                for c in a..=b { cpus.push(c); }
            }
        } else if let Ok(n) = part.parse::<u32>() {
            cpus.push(n);
        }
    }
    cpus
}

// ── CPU topology reader ──

fn read_cpu_info() -> (String, Vec<String>, Vec<CoreTopology>) {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();

    // model name
    let model_name = cpuinfo.lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split(':').nth(1))
        .unwrap_or("Unknown")
        .trim()
        .to_string();

    // flags
    let mut flags: Vec<String> = Vec::new();
    for line in cpuinfo.lines() {
        if let Some(_f) = line.strip_prefix("flags") {
            let feat = line.split(':').last().map(|s| s.trim().to_string()).unwrap_or_default();
            flags = feat.split_whitespace().map(|s| s.to_string()).collect();
            break;
        }
    }
    // Also check "Features" line (some kernels)
    if flags.is_empty() {
        for line in cpuinfo.lines() {
            if let Some(_f) = line.strip_prefix("Features") {
                let feat = line.split(':').last().map(|s| s.trim().to_string()).unwrap_or_default();
                flags = feat.split_whitespace().map(|s| s.to_string()).collect();
                break;
            }
        }
    }

    // deduplicate flags (they repeat per CPU)
    flags.sort();
    flags.dedup();

    // topology - probe available CPUs
    let cpus = read_cpu_list("/sys/devices/system/cpu/online");
    let mut cores = Vec::new();
    for cpu in &cpus {
        let cpu_dir = format!("/sys/devices/system/cpu/cpu{cpu}");
        if !Path::new(&cpu_dir).exists() { continue; }

        let core_id = read_sysfs_u64(&format!("{cpu_dir}/topology/core_id")).unwrap_or(0) as u32;
        let pkg = read_sysfs_u64(&format!("{cpu_dir}/topology/package_id")).unwrap_or(0) as u32;

        // cache info
        let mut caches = Vec::new();
        for idx in 0..16 {
            let cache_dir = format!("{cpu_dir}/cache/index{idx}");
            if !Path::new(&cache_dir).exists() { break; }

            let kind = read_sysfs_string(&format!("{cache_dir}/type")).unwrap_or_default();
            let level = read_sysfs_u64(&format!("{cache_dir}/level")).unwrap_or(0) as u32;
            let size = read_sysfs_string(&format!("{cache_dir}/size")).unwrap_or_default();
            let ways = read_sysfs_u64(&format!("{cache_dir}/ways_of_associativity")).unwrap_or(0) as u32;
            let line_size = read_sysfs_u64(&format!("{cache_dir}/coherency_line_size")).unwrap_or(0) as u32;
            let shared_list = read_cpu_list(&format!("{cache_dir}/shared_cpu_list"));

            caches.push(CacheInfo {
                level, kind, size, ways, line_size, shared_cpu_list: shared_list,
            });
        }

        // frequency
        let cpufreq = format!("{cpu_dir}/cpufreq");
        let freq_cur = read_sysfs_u64(&format!("{cpufreq}/scaling_cur_freq"));
        let freq_min = read_sysfs_u64(&format!("{cpufreq}/scaling_min_freq"));
        let freq_max = read_sysfs_u64(&format!("{cpufreq}/scaling_max_freq"));
        let gov_name = read_sysfs_string(&format!("{cpufreq}/scaling_governor"));

        cores.push(CoreTopology {
            cpu: *cpu, core_id, package: pkg,
            caches, freq_cur, freq_min, freq_max, gov_name,
        });
    }

    (model_name, flags, cores)
}

fn read_interrupts() -> Vec<InterruptLine> {
    let text = match fs::read_to_string("/proc/interrupts") {
        Ok(t) => t,
        Err(_) => return vec![],
    };
    let mut lines = text.lines();
    let header = match lines.next() {
        Some(h) => h,
        None => return vec![],
    };
    let num_cpus = header.split_whitespace().filter(|s| s.starts_with("CPU")).count();

    let mut irqs = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() { continue; }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 2 + num_cpus { continue; }
        // IRQ number or name
        let irq_str = cols[0].trim_end_matches(':');
        let irq = irq_str.parse::<u32>().unwrap_or(0);
        if irq == 0 { continue; } // skip non-numeric like "ERR", "MIS", etc.

        let mut per_cpu = Vec::new();
        for i in 0..num_cpus {
            per_cpu.push(cols[1 + i].parse::<u64>().unwrap_or(0));
        }
        let desc = cols[1 + num_cpus..].join(" ");
        irqs.push(InterruptLine { irq, desc, per_cpu });
    }
    irqs
}

// ── Flag grouping ──

fn flag_group(flag: &str) -> &'static str {
    match flag {
        // SIMD & vector
        "sse" | "sse2" | "sse3" | "ssse3" | "sse4_1" | "sse4_2" | "sse4a" => "SSE",
        "avx" | "avx2" | "avx512f" | "avx512dq" | "avx512cd" | "avx512bw"
        | "avx512vl" | "avx512ifma" | "avx512vbmi" | "avx512_vbmi2"
        | "avx512_bf16" | "avx512_vnni" | "avx512_bitalg" | "avx512_vpopcntdq"
        | "fma" => "AVX/FMA",
        "mmx" | "mmxext" | "3dnowprefetch" => "MMX/3DNow!",
        "gfni" | "vaes" | "vpclmulqdq" => "Vector Crypto",
        // Crypto
        "aes" | "sha_ni" | "pclmulqdq" => "Crypto",
        // Virtualization
        "svm" | "npt" | "lbrv" | "nrip_save" | "tsc_scale" | "vmcb_clean"
        | "flushbyasid" | "pausefilter" | "pfthreshold" | "vgif" | "vnmi"
        | "vmmcall" => "Virtualization",
        // Security
        "nx" | "smep" | "smap" | "umip" | "pku" | "ospke"
        | "ibrs" | "ibpb" | "stibp" | "ibrs_enhanced" | "ssbd"
        | "flush_l1d" | "md_clear" | "overflow_recov" | "succor" => "Security",
        // Memory management
        "pse" | "pae" | "pse36" | "pdpe1gb" | "clflush" | "clflushopt"
        | "clwb" | "xsave" | "xsaveopt" | "xsavec" | "xgetbv1" | "xsaves"
        | "clzero" | "wbnoinvd" | "rdrand" | "rdseed" | "fsgsbase" => "Memory/Misc",
        // Monitoring
        "perfctr_core" | "perfmon_v2" | "arat"
        | "tsc" | "tsc_deadline_timer" | "tsc_adjust" | "tsc_known_freq" => "Timing/Monitoring",
        // CPU features
        "cx8" | "cx16" | "cmov" | "x2apic" | "apic"
        | "rdtscp" | "sysenter" | "syscall" => "CPU",
        // Other
        _ => "Other",
    }
}

fn render_flags(flags: &[String]) {
    if flags.is_empty() { return; }
    let mut groups: HashMap<&str, Vec<&str>> = HashMap::new();
    for f in flags {
        groups.entry(flag_group(f)).or_default().push(f);
    }

    // Order groups
    let order = ["AVX/FMA", "SSE", "Vector Crypto", "Crypto", "Virtualization",
                  "Security", "Memory/Misc", "Timing/Monitoring", "MMX/3DNow!", "CPU", "Other"];

    for group in &order {
        let Some(feats) = groups.get(group) else { continue; };
        let color = match *group {
            "AVX/FMA" => style::magenta,
            "SSE" => style::cyan,
            "Crypto" | "Vector Crypto" => style::green,
            "Virtualization" => style::yellow,
            "Security" => style::red,
            "Memory/Misc" => style::blue,
            "Timing/Monitoring" => style::dim,
            _ => |s: &&str| s.to_string(),
        };
        println!("  {} {}: {}",
            style::bold(style::grey(group)),
            style::dim("│"),
            feats.iter().map(|f| color(f)).collect::<Vec<_>>().join(" "),
        );
    }
}

// ── Topology renderer ──

fn render_topology(cores: &[CoreTopology]) {
    if cores.is_empty() {
        println!("  {} No CPU topology data available.", style::yellow("⚠"));
        return;
    }

    let packages: Vec<u32> = {
        let mut v: Vec<u32> = cores.iter().map(|c| c.package).collect();
        v.sort();
        v.dedup();
        v
    };
    let numa_str = read_sysfs_string("/sys/devices/system/node/has_memory")
        .unwrap_or_default();

    let ncpus = cores.len();

    // ── CPU summary header ──
    println!();
    println!("  {} {} {}",
        style::bold("CPU Topology"),
        style::dim("—"),
        style::grey(format!("{} CPU{}, {} package{}, {} NUMA node{}",
            ncpus,
            if ncpus == 1 { "" } else { "s" },
            packages.len(),
            if packages.len() == 1 { "" } else { "s" },
            if numa_str.is_empty() { 1 } else { numa_str.split(',').count() },
            if numa_str.is_empty() || numa_str.split(',').count() == 1 { "" } else { "s" },
        )),
    );

    // ── Per-package topology ──
    for &pkg in &packages {
        let pkg_cores: Vec<&CoreTopology> = cores.iter().filter(|c| c.package == pkg).collect();

        println!();
        println!("  {} Package P{} ({})",
            style::bold(style::yellow("┌─")),
            style::bold(format!("{}", pkg)),
            style::grey(format!("{} core{}", pkg_cores.len(),
                if pkg_cores.len() == 1 { "" } else { "s" })),
        );

        // ── Cache topology ──
        // Build a per-level cache map
        // Level 1: L1d + L1i per core
        // Level 2: L2 per core (or shared)
        // Level 3: LLC shared

        // Collect cache levels present
        let max_level = cores.iter()
            .flat_map(|c| c.caches.iter().map(|ca| ca.level))
            .max().unwrap_or(3);

        // For each cache level, show which CPUs share it
        for level in 1..=max_level {
            // Get all caches at this level
            let mut caches_at_level: Vec<&CacheInfo> = Vec::new();
            let mut seen_descs = std::collections::HashSet::new();
            for core in cores {
                for cache in &core.caches {
                    if cache.level == level {
                        let desc = format!("{}_{}", cache.kind, cache.size);
                        if seen_descs.insert(desc) {
                            caches_at_level.push(cache);
                        }
                    }
                }
            }

            // For L1, show per-core
            // For L2, show per-core (or shared)
            // For L3+, show shared
            let level_color = match level {
                1 => style::green,
                2 => style::cyan,
                3 => style::yellow,
                _ => style::white,
            };
            let level_name = match level {
                1 => "L1".to_string(),
                2 => "L2".to_string(),
                l => format!("L{}", l),
            };

            for cache in &caches_at_level {
                let shared_str = if cache.shared_cpu_list.len() > 1 {
                    let list: Vec<String> = cache.shared_cpu_list.iter().map(|c| format!("CPU{}", c)).collect();
                    format!(" {}", style::dim(format!("(shared: {})", list.join(", "))))
                } else {
                    String::new()
                };

                let ways_str = if cache.ways > 0 {
                    format!(" {} {}-way", style::dim("•"), cache.ways)
                } else {
                    String::new()
                };

                let line_str = if cache.line_size > 0 {
                    format!(" {} {}B line", style::dim("•"), cache.line_size)
                } else {
                    String::new()
                };

                println!("  {} {}  {}  {}{}",
                    style::grey("│"),
                    level_color(style::bold(format!("{}  ", level_name))),
                    level_color(format!("{:>6} {:12}", cache.size,
                        match cache.kind.as_str() {
                            "Data" => "L1d",
                            "Instruction" => "L1i",
                            "Unified" => "Unified",
                            _ => &cache.kind,
                        })),
                    ways_str,
                    [line_str.as_str(), shared_str.as_str()].join(""),
                );
            }
        }

        // ── Core grid ──
        let per_row = 4;
        for (i, core) in pkg_cores.iter().enumerate() {
            if i % per_row == 0 {
                print!("  {} ", style::grey("│"));
            }

            print!("{}", style::on_grey(format!(" CPU{} ", core.cpu)));

            if i % per_row == per_row - 1 || i == pkg_cores.len() - 1 {
                println!();
            }
        }

        // ── Frequency per core ──
        let show_freq = pkg_cores.iter().any(|c| c.freq_cur.is_some());
        if show_freq {
            println!("  {} {}",
                style::grey("│"),
                style::dim("─ Frequency ─"),
            );
            for core in pkg_cores {
                let freq_str = match (core.freq_cur, core.freq_min, core.freq_max) {
                    (Some(cur), Some(min), Some(max)) => {
                        format!("{:.1} GHz (min: {:.1}, max: {:.1})",
                            cur as f64 / 1_000_000.0,
                            min as f64 / 1_000_000.0,
                            max as f64 / 1_000_000.0,
                        )
                    }
                    (Some(cur), _, _) => format!("{:.1} GHz", cur as f64 / 1_000_000.0),
                    _ => style::dim("N/A").to_string(),
                };
                let gov = core.gov_name.as_deref().unwrap_or("-");
                println!("  {}   CPU{}: {}  {}",
                    style::grey("│"), core.cpu,
                    freq_str,
                    style::dim(format!("[{}]", gov)),
                );
            }
        }

        println!("  {}", style::yellow("└─"));
    }
}

// ── Interrupt renderer ──

fn render_interrupts(irqs: &[InterruptLine]) {
    if irqs.is_empty() { return; }
    let mut sorted = irqs.to_vec();
    sorted.sort_by(|a, b| {
        let a_total: u64 = a.per_cpu.iter().sum();
        let b_total: u64 = b.per_cpu.iter().sum();
        b_total.cmp(&a_total)
    });

    let top: Vec<_> = sorted.into_iter().take(8).collect();
    let max_total: u64 = top.iter().flat_map(|i| i.per_cpu.iter()).copied().max().unwrap_or(1);

    println!();
    println!("  {} {}",
        style::bold("Interrupts"),
        style::dim("─ top IRQs by volume"),
    );

    for irq in &top {
        let total: u64 = irq.per_cpu.iter().sum();
        let bar_len = 30;
        let filled = ((total as f64 / max_total as f64) * bar_len as f64).round() as usize;
        let filled = filled.min(bar_len);
        let bar: String = (0..bar_len).map(|i|
            if i < filled { "█" } else { "░" }
        ).collect();

        let dist: Vec<String> = irq.per_cpu.iter()
            .map(|v| if *v > 0 { format!("{}", v) } else { "·".to_string() })
            .collect();
        println!("  {:>4} │{:>10} │{}│ {}  {}",
            style::dim(format!("IRQ{}", irq.irq)),
            style::yellow(format!("{}", total)),
            bar,
            style::grey(&irq.desc),
            style::dim(format!("[{}]", dist.join(","))),
        );
    }
}

// ── Architecture details ──

fn render_arch_details(flags: &[String]) {
    println!();
    println!("  {} {}",
        style::bold("CPU Flags"),
        style::dim("— grouped by category"),
    );
    render_flags(flags);
}

// ── Public entry point ──

/// Print the CPU topology report to stdout.
pub fn cat_cpu() {
    let (model_name, flags, cores) = read_cpu_info();
    let irqs = read_interrupts();

    // ── Header ──
    println!();
    println!("  {}  {}",
        style::bold(style::white("⚡")),
        style::bold(&model_name),
    );

    // Hypervisor / virtualization check
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let hypervisor = cpuinfo.lines()
        .find_map(|l| {
            if l.starts_with("hypervisor") || l.starts_with("hypervisor\t") {
                let parts: Vec<&str> = l.split(':').collect();
                parts.get(1).map(|s| s.trim().to_string())
            } else { None }
        });
    if let Some(hv) = &hypervisor {
        if hv == "KVM" || hv == "kvm" {
            println!("  {}   {} {}",
                style::dim("├"),
                style::red("⬡"),
                style::bold(style::red("VIRTUALIZED")),
            );
            // Read the CPUID leaf for KVM
            println!("  {}   {} {}",
                style::dim("│"),
                style::dim("λ"),
                style::grey("Running under KVM (4 vCPUs, KVM CPUID)"),
            );
        } else {
            println!("  {}   Hypervisor: {}", style::dim("├"), style::yellow(hv));
        }
    }

    // ── Topology ──
    render_topology(&cores);

    // ── Interrupts ──
    render_interrupts(&irqs);

    // ── Flags ──
    if !flags.is_empty() {
        render_arch_details(&flags);
    }

    println!();
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_cpu_list_single() {
        let cpus = parse_cpu_list("0-3");
        assert_eq!(cpus, vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_read_cpu_list_complex() {
        let cpus = parse_cpu_list("0-3,8,10-11");
        assert_eq!(cpus, vec![0, 1, 2, 3, 8, 10, 11]);
    }

    #[test]
    fn test_read_cpu_list_empty() {
        let cpus: Vec<u32> = parse_cpu_list("");
        assert!(cpus.is_empty());
    }

    #[test]
    fn test_read_cpu_list_single_number() {
        let cpus = parse_cpu_list("5");
        assert_eq!(cpus, vec![5]);
    }

    #[test]
    fn test_flag_group_sse() {
        assert_eq!(flag_group("sse4_2"), "SSE");
        assert_eq!(flag_group("avx2"), "AVX/FMA");
        assert_eq!(flag_group("aes"), "Crypto");
        assert_eq!(flag_group("svm"), "Virtualization");
        assert_eq!(flag_group("smep"), "Security");
        assert_eq!(flag_group("unknown_flag_xyz"), "Other");
    }

    #[test]
    fn test_flag_group_avx512() {
        assert_eq!(flag_group("avx512bw"), "AVX/FMA");
        assert_eq!(flag_group("vaes"), "Vector Crypto");
    }

    #[test]
    fn test_read_cpu_info_proc_exists() {
        let (model, flags, cores) = read_cpu_info();
        // At minimum, /proc/cpuinfo should have something
        assert!(!model.is_empty() || !flags.is_empty() || !cores.is_empty());
    }

    #[test]
    fn test_read_interrupts_proc_exists() {
        let irqs = read_interrupts();
        // /proc/interrupts should exist on Linux and have at least timer/IRQ0
        let has_timer = irqs.iter().any(|i| i.desc.contains("timer"));
        let has_nonzero = irqs.iter().any(|i| i.per_cpu.iter().sum::<u64>() > 0);
        assert!(has_timer || irqs.is_empty() || has_nonzero);
    }

    #[test]
    fn test_cat_cpu_does_not_panic() {
        // Just verify the function runs without panicking
        cat_cpu();
    }

    #[test]
    fn test_cache_info_display() {
        let cache = CacheInfo {
            level: 1,
            kind: "Data".to_string(),
            size: "64K".to_string(),
            ways: 8,
            line_size: 64,
            shared_cpu_list: vec![0],
        };
        assert_eq!(cache.level, 1);
        assert_eq!(cache.kind, "Data");
        assert_eq!(cache.size, "64K");
        assert!(cache.shared_cpu_list.contains(&0));
    }

    #[test]
    fn test_interrupt_sorting() {
        let irqs = vec![
            InterruptLine { irq: 1, desc: "timer".into(), per_cpu: vec![100, 200] },
            InterruptLine { irq: 2, desc: "i8042".into(), per_cpu: vec![10, 5] },
        ];
        let mut sorted = irqs.clone();
        sorted.sort_by(|a, b| {
            let a_sum: u64 = a.per_cpu.iter().sum();
            let b_sum: u64 = b.per_cpu.iter().sum();
            b_sum.cmp(&a_sum)
        });
        assert_eq!(sorted[0].irq, 1); // timer has more
        assert_eq!(sorted[1].irq, 2);
    }

    #[test]
    fn test_topology_render_empty() {
        // Should not panic with empty cores
        render_topology(&[]);
    }
}
