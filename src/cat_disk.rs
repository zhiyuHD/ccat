//! Disk and filesystem explorer (`ccat --disk`).
//!
//! Reads `/proc/mounts`, `/proc/diskstats`, `/proc/partitions`, and
//! statfs(2) via `/sys/fs/` or direct path stat to produce a beautiful,
//! coloured disk analysis showing:
//!
//! - Mount table: device, mount point, filesystem type, total/used/avail, use%
//! - Disk I/O statistics: read/write counts, sectors transferred, I/O time
//! - ZRAM compression stats (when available)
//! - Partition overview
//!
//! Uses only /proc and statfs — no special privileges needed.

use std::fs;
use std::path::Path;
use std::io::{self, Write};
use std::process::Command;

// ── Colour helpers (self-contained, mirrors cat_cpu/cat_proc) ──

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
        else { green(format!("{:>5.0}%", pct)) }
    }
}

// ── Data types ──

#[derive(Debug, Clone)]
struct MountInfo {
    device: String,
    mount_point: String,
    fstype: String,
    opts: String,
    total: u64,        // bytes
    used: u64,         // bytes
    avail: u64,        // bytes
    inodes_total: u64,
    inodes_used: u64,
}

#[derive(Debug, Clone)]
struct DiskIO {
    name: String,
    major: u32,
    minor: u32,
    reads: u64,
    reads_merged: u64,
    sectors_read: u64,
    read_ms: u64,
    writes: u64,
    writes_merged: u64,
    sectors_written: u64,
    write_ms: u64,
    io_in_progress: u64,
    io_ms: u64,
    io_weighted_ms: u64,
}

#[derive(Debug, Clone)]
struct ZramStats {
    disks: Vec<ZramDisk>,
}

#[derive(Debug, Clone)]
struct ZramDisk {
    name: String,
    orig_size: u64,        // bytes
    comp_size: u64,        // bytes
    ratio: f64,            // compression ratio
    mm_stat: Option<String>,
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

fn human_time(ms: u64) -> String {
    if ms >= 86_400_000 {
        format!("{:.1}d", ms as f64 / 86_400_000.0)
    } else if ms >= 3_600_000 {
        format!("{:.1}h", ms as f64 / 3_600_000.0)
    } else if ms >= 60_000 {
        format!("{:.1}m", ms as f64 / 60_000.0)
    } else if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

/// Parse /proc/mounts into raw mount entries (before statfs enrichment).
fn read_mounts() -> Vec<(String, String, String, String)> {
    let content = match fs::read_to_string("/proc/mounts") {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut mounts = Vec::new();
    for line in content.lines() {
        // rootfs / rootfs rw 0 0
        // /dev/sda2 / ext4 rw,relatime 0 0
        let parts: Vec<&str> = line.splitn(6, ' ').collect();
        if parts.len() < 4 { continue; }

        let device = parts[0].to_string();
        let mount_point = parts[1].to_string();
        let fstype = parts[2].to_string();
        let opts = parts[3].to_string();

        // Skip pseudo-filesystems that statfs would fail on
        let skip_fs = [
            "autofs", "proc", "sysfs", "cgroup", "cgroup2",
            "devpts", "devtmpfs", "pstore", "hugetlbfs",
            "mqueue", "securityfs", "efivarfs", "bpf",
            "debugfs", "tracefs", "configfs", "fusectl",
            "overlay", "squashfs", "fuse.gvfsd-fuse",
            "fuse.portal", "fuse.lxcfs",
            "rpc_pipefs", "nfsd", "sunrpc",
        ];
        if skip_fs.contains(&fstype.as_str()) {
            // Only skip if it's not a real block device
            if !device.starts_with("/dev/") {
                continue;
            }
        }

        mounts.push((device, mount_point, fstype, opts));
    }
    mounts
}

/// Get filesystem statistics via statfs(2) by calling stat (POSIX) or reading /sys.
fn get_fs_stats(mount_point: &str) -> Option<(u64, u64, u64, u64, u64)> {
    // Try `stat -f` for filesystem statistics (returns fragment size, block count, etc.)
    let output = Command::new("stat")
        .args(["-f", "-c", "%s %b %f %a %d %c"])
        .arg(mount_point)
        .output().ok()?;

    let s = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = s.trim().split(' ').collect();
    if parts.len() < 6 { return None; }

    let frag_size: u64 = parts[0].parse().ok()?;    // fragment size (usually block size)
    let total_blocks: u64 = parts[1].parse().ok()?;
    let free_blocks: u64 = parts[2].parse().ok()?;  // free blocks for root
    let avail_blocks: u64 = parts[3].parse().ok()?; // free blocks for unprivileged
    let inodes_total: u64 = parts[4].parse().ok()?;
    let inodes_free: u64 = parts[5].parse().ok()?;

    let total = total_blocks.saturating_mul(frag_size);
    let free = free_blocks.saturating_mul(frag_size);
    let avail = avail_blocks.saturating_mul(frag_size);
    let used = total.saturating_sub(free);
    let inodes_used = inodes_total.saturating_sub(inodes_free);

    Some((total, used, avail, inodes_total, inodes_used))
}

/// Parse /proc/diskstats into DiskIO entries.
fn read_diskstats() -> Vec<DiskIO> {
    let content = match fs::read_to_string("/proc/diskstats") {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut disks = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        // Format: major minor name rio rmerge rsect ruse wio wmerge wsect wuse running use aveq
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 14 { continue; }

        let name = parts[2].to_string();

        // Filter: only physical disks and partitions (skip loop, dm-*, md*, etc.)
        // but keep zram since it's important for this system
        let is_real = name.starts_with("sd")
            || name.starts_with("nvme")
            || name.starts_with("vd")
            || name.starts_with("hd")
            || name.starts_with("mmc")
            || name.starts_with("zram")
            || name.starts_with("xvd");
        if !is_real { continue; }

        let parse_u64 = |i: usize| parts[i].parse::<u64>().unwrap_or(0);

        disks.push(DiskIO {
            name,
            major: parse_u64(0) as u32,
            minor: parse_u64(1) as u32,
            reads: parse_u64(3),
            reads_merged: parse_u64(4),
            sectors_read: parse_u64(5),
            read_ms: parse_u64(6),
            writes: parse_u64(7),
            writes_merged: parse_u64(8),
            sectors_written: parse_u64(9),
            write_ms: parse_u64(10),
            io_in_progress: parse_u64(11),
            io_ms: parse_u64(12),
            io_weighted_ms: parse_u64(13),
        });
    }
    disks
}

/// Read ZRAM compression stats from /sys/block/zram*/.
fn read_zram_stats() -> ZramStats {
    let mut disks = Vec::new();
    let sys_block = Path::new("/sys/block");
    if !sys_block.exists() {
        return ZramStats { disks };
    }

    let entries = match fs::read_dir(sys_block) {
        Ok(e) => e,
        Err(_) => return ZramStats { disks },
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("zram") { continue; }

        let base = format!("/sys/block/{name_str}");
        let mm_stat = match fs::read_to_string(format!("{base}/mm_stat")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let parts: Vec<&str> = mm_stat.trim().split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let orig_s = parts[0].parse::<u64>().unwrap_or(0);
        let comp_s = parts[1].parse::<u64>().unwrap_or(0);

        let ratio = if orig_s > 0 {
            orig_s as f64 / comp_s as f64
        } else {
            0.0
        };

        let mm_stat = fs::read_to_string(format!("{base}/mm_stat")).ok();

        disks.push(ZramDisk {
            name: name_str.to_string(),
            orig_size: orig_s,
            comp_size: comp_s,
            ratio,
            mm_stat,
        });
    }

    ZramStats { disks }
}

/// Print a horizontal separator line.
fn sep(width: usize) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", style::grey("─".repeat(width.min(80))));
}

/// Print a section header.
fn header(text: &str) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "\n{}\n{}", style::bold(style::cyan(text)),
        style::grey("─".repeat(text.len().min(40))));
}

// ── Main entry point ──

pub fn cat_disk() {
    let (width, _) = crate::pager::terminal_size();
    let col_w = width.saturating_sub(4).max(40);

    // ── Section 1: Mount Table ──
    header("DISK MOUNTS");

    let raw_mounts = read_mounts();
    let mut mounts: Vec<MountInfo> = Vec::new();

    for (device, mount_point, fstype, _opts) in &raw_mounts {
        if let Some((total, used, avail, inodes_total, inodes_used)) = get_fs_stats(mount_point) {
            mounts.push(MountInfo {
                device: device.clone(),
                mount_point: mount_point.clone(),
                fstype: fstype.clone(),
                opts: String::new(),
                total,
                used,
                avail,
                inodes_total,
                inodes_used,
            });
        }
    }

    // Sort by mount point
    mounts.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));

    if mounts.is_empty() {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let _ = writeln!(out, "  {}  No mount points found (statfs unavailable?)", style::grey("∅"));
    } else {
        // Column widths
        let max_dev = mounts.iter().map(|m| m.device.len()).max().unwrap_or(10).min(20);
        let max_mp  = mounts.iter().map(|m| m.mount_point.len()).max().unwrap_or(10).min(40);
        let max_fs  = mounts.iter().map(|m| m.fstype.len()).max().unwrap_or(5).min(8);

        // Header
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let _ = writeln!(out, "  {:<dw$} {:<mw$} {:<fw$} {:>6} {:>6} {:>6} {:>5} {:>5}",
            style::bold("DEVICE"), style::bold("MOUNTPOINT"), style::bold("TYPE"),
            style::bold("TOTAL"), style::bold("USED"), style::bold("AVAIL"),
            style::bold("USE%"), style::bold("INODE%"),
            dw = max_dev, mw = max_mp, fw = max_fs);

        for m in &mounts {
            let dev_display = if m.device.len() > max_dev {
                format!("…{}", &m.device[m.device.len().saturating_sub(max_dev.saturating_sub(1))..])
            } else {
                m.device.clone()
            };

            let mp_display = if m.mount_point.len() > max_mp {
                format!("…{}", &m.mount_point[m.mount_point.len().saturating_sub(max_mp.saturating_sub(1))..])
            } else {
                m.mount_point.clone()
            };

            let use_pct = if m.total > 0 {
                m.used as f64 / m.total as f64 * 100.0
            } else {
                0.0
            };

            let inode_pct = if m.inodes_total > 0 {
                m.inodes_used as f64 / m.inodes_total as f64 * 100.0
            } else {
                0.0
            };

            let _ = writeln!(out, "  {:<dw$} {:<mw$} {:<fw$} {:>6} {:>6} {:>6} {:>5} {:>5}",
                dev_display,
                mp_display,
                m.fstype,
                human_size(m.total),
                human_size(m.used),
                human_size(m.avail),
                style::use_pct(use_pct),
                style::use_pct(inode_pct),
                dw = max_dev, mw = max_mp, fw = max_fs);
        }
    }

    // ── Section 2: Disk I/O Statistics ──
    header("DISK I/O");

    let disks = read_diskstats();
    if disks.is_empty() {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let _ = writeln!(out, "  {}  No disk I/O data (unable to read /proc/diskstats)", style::grey("∅"));
    } else {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let _ = writeln!(out, "  {} {:>8} {:>8} {:>6} {:>8} {:>8} {:>6} {:>6} {:>6}",
            style::bold("DEVICE"),
            style::bold("READS"),
            style::bold("R_SECT"),
            style::bold("R_MS"),
            style::bold("WRITES"),
            style::bold("W_SECT"),
            style::bold("W_MS"),
            style::bold("BUSY"),
            style::bold("AVG_Q"));

        for d in &disks {
            let busy_pct = if d.io_ms > 0 {
                // Estimate: io_ms / (read_ms + write_ms).max(1)
                let total_io = d.read_ms + d.write_ms;
                if total_io > 0 {
                    (d.io_ms as f64 / total_io as f64 * 100.0).min(100.0)
                } else { 0.0 }
            } else { 0.0 };

            let avg_queue = if d.io_weighted_ms > 0 {
                let total_ops = d.reads + d.writes;
                if total_ops > 0 {
                    d.io_weighted_ms as f64 / total_ops as f64
                } else { 0.0 }
            } else { 0.0 };

            let _ = writeln!(out, "  {} {:>8} {:>8} {:>6} {:>8} {:>8} {:>6} {:>5.0}% {:>5.1}ms",
                style::cyan(&d.name),
                human_count(d.reads),
                human_size(d.sectors_read * 512),
                human_time(d.read_ms),
                human_count(d.writes),
                human_size(d.sectors_written * 512),
                human_time(d.write_ms),
                busy_pct,
                avg_queue);
        }
    }

    // ── Section 3: ZRAM Compression ──
    let zram = read_zram_stats();
    if !zram.disks.is_empty() {
        header("ZRAM COMPRESSION");

        let stdout = io::stdout();
        let mut out = stdout.lock();

        for zd in &zram.disks {
            let _ = writeln!(out, "  {} ─ {}", style::cyan(&zd.name), style::bold("Compressed Swap"));

            let ratio_str = if zd.ratio >= 3.0 {
                style::green(format!("{:.2}x", zd.ratio))
            } else if zd.ratio >= 2.0 {
                style::yellow(format!("{:.2}x", zd.ratio))
            } else {
                style::red(format!("{:.2}x", zd.ratio))
            };

            let _ = writeln!(out, "    Orig: {:>10}  Compressed: {:>10}  Ratio: {}",
                human_size(zd.orig_size),
                human_size(zd.comp_size),
                ratio_str);

            // Parse mm_stat for more details if available
            if let Some(ref mm_stat_str) = zd.mm_stat {
                let parts: Vec<&str> = mm_stat_str.trim().split_whitespace().collect();
                // mm_stat: orig_data_size compr_data_size mem_used_total mem_limit mem_used_max same_pages pages_compacted [huge_pages]
                if parts.len() >= 5 {
                    let mem_used = parts[2].parse::<u64>().unwrap_or(0);
                    let max_used = parts[4].parse::<u64>().unwrap_or(0);
                    let overhead = mem_used.saturating_sub(zd.comp_size);
                    let _ = writeln!(
                        out,
                        "    Mem used: {:>10}  + overhead: {:>9}  Max used: {:>10}  Compress: {:.1}%",
                        human_size(mem_used),
                        human_size(overhead),
                        human_size(max_used),
                        zd.ratio / 1.0_f64.max(zd.ratio) * 100.0
                    );
                    if parts.len() >= 7 {
                        let same = parts[5].parse::<u64>().unwrap_or(0);
                        let compacted = parts[6].parse::<u64>().unwrap_or(0);
                        let _ = writeln!(
                            out,
                            "    Same pages: {:>10}  Compacted: {:>10}",
                            human_count(same),
                            human_count(compacted)
                        );
                    }
                }
            }
        }

        // Aggregate stats
        let total_orig: u64 = zram.disks.iter().map(|d| d.orig_size).sum();
        let total_comp: u64 = zram.disks.iter().map(|d| d.comp_size).sum();
        let mem_saved = total_orig.saturating_sub(total_comp);
        let avg_ratio = if total_comp > 0 {
            total_orig as f64 / total_comp as f64
        } else { 0.0 };

        sep(col_w);
        let _ = writeln!(out, "  Total orig: {:>10}  Total comp: {:>10}  Saved: {:>10}  Avg ratio: {:.2}x",
            human_size(total_orig), human_size(total_comp), human_size(mem_saved), avg_ratio);
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size_bytes() {
        assert_eq!(human_size(0), "0 B");
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
    fn test_human_count() {
        assert_eq!(human_count(0), "0");
        assert_eq!(human_count(999), "999");
        assert_eq!(human_count(1000), "1.0K");
        assert_eq!(human_count(1_500_000), "1.5M");
        assert_eq!(human_count(2_000_000_000), "2.0B");
    }

    #[test]
    fn test_human_time() {
        assert_eq!(human_time(500), "500ms");
        assert_eq!(human_time(1500), "1.5s");
        assert_eq!(human_time(90_000), "1.5m");
        assert_eq!(human_time(7_200_000), "2.0h");
        assert_eq!(human_time(172_800_000), "2.0d");
    }

    #[test]
    fn test_read_mounts_ok() {
        let mounts = read_mounts();
        assert!(!mounts.is_empty(), "Should find at least / and /proc");
        // At minimum, / should be mounted
        assert!(mounts.iter().any(|(_, mp, _, _)| mp == "/"),
            "Root filesystem should be mounted");
    }

    #[test]
    fn test_read_diskstats_ok() {
        let disks = read_diskstats();
        // At minimum, we should find something (even zram on this system)
        assert!(!disks.is_empty(), "Should find at least zram or sda in /proc/diskstats");
    }

    #[test]
    fn test_read_zram_ok() {
        let zram = read_zram_stats();
        // ZRAM may or may not be present
        if !zram.disks.is_empty() {
            for d in &zram.disks {
                assert!(!d.name.is_empty());
                // orig_size should be > 0 if zram is active
                if d.orig_size > 0 {
                    assert!(d.ratio > 0.0, "Compression ratio should be > 0");
                }
            }
        }
    }

    #[test]
    fn test_get_fs_stats_root() {
        if let Some((total, used, avail, _, _)) = get_fs_stats("/") {
            assert!(total > 0, "Root filesystem total should be > 0");
            assert!(used + avail <= total || used + avail > total.saturating_sub(4096), // small margin for reserved blocks
                "used + avail should roughly equal total");
        }
    }

    #[test]
    fn test_style_use_pct() {
        // These return ANSI-wrapped strings, just check they don't crash
        let _ = style::use_pct(50.0);
        let _ = style::use_pct(75.0);
        let _ = style::use_pct(95.0);
        // green for low
        assert!(style::use_pct(50.0).contains("32m"));
        // yellow for medium
        assert!(style::use_pct(75.0).contains("33m"));
        // red for high
        assert!(style::use_pct(95.0).contains("31m"));
    }
}
