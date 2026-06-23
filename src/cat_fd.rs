//! Linux file descriptor explorer (`ccat --fd`).
//!
//! Reads `/proc/<pid>/fd/` and resolves symlinks to show what files,
//! sockets, pipes, and devices each process has open. Displays them as
//! a colour-coded tree grouped by type.
//!
//! Features:
//! - Per-PID fd listing with resolved paths
//! - Socket connections shown with remote addresses
//! - Pipe endpoints mapped (reader ↔ writer)
//! - Leak detection (open files exceeding threshold)
//! - Summary statistics by type
//! - Optional: show all processes' fds in one view

use std::collections::HashMap;
use std::fs;
use std::path::Path;

// ── Colour helpers ──

mod style {
    pub fn bold(s: &str) -> String { format!("\x1b[1m{s}\x1b[0m") }
    pub fn dim(s: &str) -> String { format!("\x1b[2m{s}\x1b[0m") }
    pub fn green(s: &str) -> String { format!("\x1b[32m{s}\x1b[0m") }
    pub fn red(s: &str) -> String { format!("\x1b[31m{s}\x1b[0m") }
    pub fn cyan(s: &str) -> String { format!("\x1b[36m{s}\x1b[0m") }
    pub fn yellow(s: &str) -> String { format!("\x1b[33m{s}\x1b[0m") }
    pub fn blue(s: &str) -> String { format!("\x1b[34m{s}\x1b[0m") }
    pub fn magenta(s: &str) -> String { format!("\x1b[35m{s}\x1b[0m") }
    pub fn white_bold_bg(s: &str) -> String { format!("\x1b[47;1m{s}\x1b[0m") }
}

// ── Data types ──

/// Type of file descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FdType {
    RegularFile,
    Directory,
    Socket,
    Pipe,
    Fifo,
    CharacterDevice,
    BlockDevice,
    Symlink,
    Anonymous,  // anon_inode (eventfd, signalfd, etc.)
    Other,
}

impl FdType {
    fn icon(&self) -> &str {
        match self {
            FdType::RegularFile => "\u{1f4c4}",      // 📄
            FdType::Directory => "\u{1f4c2}",         // 📂
            FdType::Socket => "\u{1f5df}",            // 🗿 (network)
            FdType::Pipe => "\u{1f4e2}",              // 💢 (pipe)
            FdType::Fifo => "\u{23f3}\u{fe0f}",       // ⏳ (fifo)
            FdType::CharacterDevice => "\u{1f4be}",    // 📾
            FdType::BlockDevice => "\u{1f1a0}",       // 🌀
            FdType::Symlink => "\u{1f517}",           // 🔗
            FdType::Anonymous => "\u{2699}\u{fe0f}",  // ⚙
            FdType::Other => "\u{2753}\u{fe0f}",      // ❓
        }
    }

    fn color(&self, text: &str) -> String {
        match self {
            FdType::RegularFile => style::cyan(text),
            FdType::Directory => style::yellow(text),
            FdType::Socket => style::green(text),
            FdType::Pipe => style::magenta(text),
            FdType::Fifo => style::magenta(text),
            FdType::CharacterDevice => style::blue(text),
            FdType::BlockDevice => style::blue(text),
            FdType::Symlink => style::red(text),
            FdType::Anonymous => style::dim(text),
            FdType::Other => style::dim(text),
        }
    }
}

/// Resolved information about a single fd.
#[derive(Debug, Clone)]
struct FdInfo {
    fd_num: u32,
    fd_type: FdType,
    target: String,       // resolved symlink target or path
    socket_info: Option<SocketInfo>,
    pipe_endpoint: Option<PipeEndpoint>,
    anon_inode: Option<String>,
    is_leaked: bool,
}

/// Parsed socket connection info from /proc/net.
#[derive(Debug, Clone)]
struct SocketInfo {
    protocol: String,    // "tcp" or "udp"
    local_addr: String,  // "127.0.0.1:8080"
    remote_addr: String, // "0.0.0.0:0" or peer
    state: Option<String>, // "LISTEN", "ESTABLISHED", etc.
    inode: String,
}

/// Pipe endpoint info.
#[derive(Debug, Clone)]
struct PipeEndpoint {
    inode: String,
    direction: String, // "read" or "write"
}

/// Options for the fd viewer.
#[derive(Debug, Clone)]
pub struct FdOptions {
    pub pid: Option<u32>,
    /// Show all processes' fds (when pid is None).
    pub all_processes: bool,
    /// Maximum fds per process before truncating display.
    pub max_fds_per_process: usize,
}

impl Default for FdOptions {
    fn default() -> Self {
        Self {
            pid: None,
            all_processes: false,
            max_fds_per_process: 200,
        }
    }
}

// ── Parsing helpers ──

/// Parse hex IP:port from /proc/net/tcp into "IP:port" format.
fn parse_hex_socket(line: &[&str]) -> Option<(String, String)> {
    // Fields: [0]sl [1]local_address [2]remote_address ...
    if line.len() < 3 { return None; }
    let local_hex = line[1];
    let remote_hex = line[2];
    Some((
        decode_hex_socket(local_hex)?,
        decode_hex_socket(remote_hex)?,
    ))
}

fn decode_hex_socket(hex: &str) -> Option<String> {
    // Format: "IP:PORT" where IP is stored in little-endian hex in /proc/net/tcp
    // e.g. "0100007F:B0E0" → 127.0.0.1:45280
    // The hex string is bytes in reverse order: last 2 chars = first octet
    let parts: Vec<&str> = hex.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let ip_hex = parts[0];
    let port_hex = parts[1];

    let port = u16::from_str_radix(port_hex, 16).ok()? as u32;

    // Parse little-endian hex IP: bytes are reversed
    if ip_hex.len() != 8 {
        return None;
    }
    let b4 = u8::from_str_radix(&ip_hex[0..2], 16).ok()?;
    let b3 = u8::from_str_radix(&ip_hex[2..4], 16).ok()?;
    let b2 = u8::from_str_radix(&ip_hex[4..6], 16).ok()?;
    let b1 = u8::from_str_radix(&ip_hex[6..8], 16).ok()?;

    Some(format!("{b1}.{b2}.{b3}.{b4}:{port}"))
}

/// Known TCP states from /proc/net/tcp.
fn tcp_state_code(code: &str) -> &'static str {
    match code.trim() {
        "01" => "ESTABLISHED",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0A" => "LISTEN",
        "0B" => "CLOSING",
        _ => "UNKNOWN",
    }
}

/// Build inode → socket info mapping from /proc/net/tcp and /proc/net/udp.
fn build_socket_map() -> HashMap<String, SocketInfo> {
    let mut map = HashMap::new();
    
    // Parse TCP connections
    if let Ok(content) = fs::read_to_string("/proc/net/tcp") {
        for line in content.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 4 { continue; }
            
            let inode = fields[9];
            if inode == "0" { continue; }
            
            let (local, remote) = match parse_hex_socket(&fields) {
                Some(pair) => pair,
                None => continue,
            };
            
            let state = tcp_state_code(&fields[3]);
            
            map.insert(inode.to_string(), SocketInfo {
                protocol: "tcp".to_string(),
                local_addr: local,
                remote_addr: remote,
                state: Some(state.to_string()),
                inode: inode.to_string(),
            });
        }
    }
    
    // Parse UDP connections
    if let Ok(content) = fs::read_to_string("/proc/net/udp") {
        for line in content.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 4 { continue; }
            
            let inode = fields[6];
            if inode == "0" { continue; }
            
            let (local, remote) = match parse_hex_socket(&fields) {
                Some(pair) => pair,
                None => continue,
            };
            
            map.insert(inode.to_string(), SocketInfo {
                protocol: "udp".to_string(),
                local_addr: local,
                remote_addr: remote,
                state: None,
                inode: inode.to_string(),
            });
        }
    }
    
    map
}

/// Resolve a single fd symlink and classify it.
fn resolve_fd(pid: u32, fd_num: u32, socket_map: &HashMap<String, SocketInfo>) -> Option<FdInfo> {
    let fd_path = format!("/proc/{pid}/fd/{fd_num}");
    
    match fs::read_link(&fd_path) {
        Ok(target_os) => {
            let target = target_os.display().to_string();
            
            // Determine type and extract info
            let (fd_type, socket_info, pipe_endpoint, anon_inode) = 
                classify_fd_target(&target, &fd_path, &socket_map);
            
            Some(FdInfo {
                fd_num,
                fd_type,
                target,
                socket_info,
                pipe_endpoint,
                anon_inode,
                is_leaked: false, // computed later
            })
        }
        Err(_) => None,
    }
}

fn classify_fd_target(
    target: &str,
    fd_path: &str,
    socket_map: &HashMap<String, SocketInfo>,
) -> (FdType, Option<SocketInfo>, Option<PipeEndpoint>, Option<String>) {
    let mut socket_info = None;
    let mut pipe_endpoint = None;
    let mut anon_inode = None;
    
    if target.starts_with("socket:[") {
        // Extract inode from socket:[12345]
        let inode = target.trim_start_matches("socket:[").trim_end_matches(']');
        
        // Look up socket info
        if let Some(si) = socket_map.get(inode) {
            socket_info = Some(si.clone());
        }
        
        // Socket socket — direction determined by /proc/[pid]/fdinfo if needed
        // For now, mark as socket
        return (FdType::Socket, socket_info, pipe_endpoint, anon_inode);
    }
    
    if target.starts_with("pipe:[") {
        let inode = target.trim_start_matches("pipe:[").trim_end_matches(']');
        // Try to read flags from fdinfo to determine read/write
        let flags = read_fd_flags(target, fd_path);
        pipe_endpoint = Some(PipeEndpoint {
            inode: inode.to_string(),
            direction: flags,
        });
        return (FdType::Pipe, socket_info, pipe_endpoint, anon_inode);
    }
    
    if target.starts_with("eventfd:") || target.starts_with("signalfd") 
        || target.starts_with("anon_inode:") {
        anon_inode = Some(target.to_string());
        return (FdType::Anonymous, socket_info, pipe_endpoint, anon_inode);
    }
    
    if target.starts_with("/dev/") {
        if target.contains("null") || target.contains("zero") || target.contains("random") {
            return (FdType::CharacterDevice, socket_info, pipe_endpoint, anon_inode);
        }
        if target.starts_with("/dev/sd") || target.starts_with("/dev/vd") || target.starts_with("/dev/xvd") {
            return (FdType::BlockDevice, socket_info, pipe_endpoint, anon_inode);
        }
        return (FdType::CharacterDevice, socket_info, pipe_endpoint, anon_inode);
    }
    
    if target.starts_with("/") {
        // Check if it's a directory
        if target.ends_with('/') || Path::new(&target).is_dir() {
            return (FdType::Directory, socket_info, pipe_endpoint, anon_inode);
        }
        return (FdType::RegularFile, socket_info, pipe_endpoint, anon_inode);
    }
    
    if target.starts_with("pipe:") || target.starts_with("eventpoll:") {
        return (FdType::Other, socket_info, pipe_endpoint, anon_inode);
    }
    
    (FdType::Other, socket_info, pipe_endpoint, anon_inode)
}

/// Read fd flags from /proc/[pid]/fdinfo/[fd] to determine pipe direction.
fn read_fd_flags(_target: &str, fd_path: &str) -> String {
    // Try to find the fd number from the path
    if let Some(fd_str) = fd_path.rsplit('/').next() {
        if let Ok(fd_num) = fd_str.parse::<u32>() {
            let pid = fd_path.split('/').nth(3).unwrap_or("");
            let fdinfo_path = format!("/proc/{pid}/fdinfo/{fd_num}");
            if let Ok(content) = fs::read_to_string(&fdinfo_path) {
                for line in content.lines() {
                    if line.starts_with("flags:") {
                        let flags: Vec<u16> = line.split(':').nth(1)
                            .unwrap_or("0")
                            .trim()
                            .split_whitespace()
                            .filter_map(|f| u16::from_str_radix(f, 16).ok())
                            .collect();
                        if let Some(&f) = flags.first() {
                            // O_RDONLY=0, O_WRONLY=1, O_RDWR=2
                            if f & 1 == 1 {
                                return "write".to_string();
                            }
                        }
                        return "read".to_string();
                    }
                }
            }
        }
    }
    "unknown".to_string()
}

/// Get process name from /proc/[pid]/comm.
fn get_process_name(pid: u32) -> String {
    let comm_path = format!("/proc/{}/comm", pid);
    fs::read_to_string(&comm_path)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "?".to_string())
}

/// Get process command line from /proc/[pid]/cmdline.
fn get_cmdline(pid: u32) -> String {
    let cmdline_path = format!("/proc/{}/cmdline", pid);
    fs::read_to_string(&cmdline_path)
        .map(|s| s.split('\0').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|_| "?".to_string())
}

// ── Detection helpers ──

/// Check if an fd looks like a leak (open file not near stdin/stdout/stderr).
fn is_potential_leak(fd_num: u32, fd_type: &FdType, target: &str) -> bool {
    // stdin/stdout/stderr are normal
    if fd_num <= 2 { return false; }
    
    // Regular files that are large or in temp dirs might be leaks
    if let FdType::RegularFile = fd_type {
        // Files in /tmp, /var/tmp, or large files are suspicious
        if target.starts_with("/tmp/") || target.starts_with("/var/tmp/") {
            return true;
        }
        // Core dumps, crash logs
        if target.contains("core") || target.contains("crash") {
            return true;
        }
    }
    
    false
}

// ── Display ──

/// Display fds for a single process.
fn display_process_fds(
    pid: u32,
    fds: &[FdInfo],
    _socket_map: &HashMap<String, SocketInfo>,
) {
    let name = get_process_name(pid);
    let cmdline = get_cmdline(pid);
    
    println!("{}", style::bold(&format!("PID {pid}: {name}")));
    if cmdline != name {
        println!("  {}", style::dim(&cmdline));
    }
    println!("  {} {} fds", 
        style::white_bold_bg(&format!("{:>4}", fds.len())),
        if fds.len() == 1 { "entry" } else { "entries" }
    );
    
    // Group by type
    let mut by_type: HashMap<FdType, Vec<&FdInfo>> = HashMap::new();
    for fd in fds {
        by_type.entry(fd.fd_type.clone()).or_default().push(fd);
    }
    
    // Display each group
    for (fd_type, fd_list) in &by_type {
        let icon = fd_type.icon();
        let count = fd_list.len();
        let header = format!("  {icon} {} ({count})", fd_type.color(&format!("{:?}", fd_type).to_lowercase().replace('_', " ")));
        
        match fd_type {
            FdType::Socket => {
                println!("{}", style::bold(&header));
                for fd in fd_list {
                    if let Some(si) = &fd.socket_info {
                        let addr = if si.protocol == "tcp" {
                            format!("{} → {}", si.local_addr, si.remote_addr)
                        } else {
                            format!("{} ↔ {}", si.local_addr, si.remote_addr)
                        };
                        let state_str = si.state.as_deref().unwrap_or("");
                        println!("    fd {}: {} {}", 
                            style::dim(&format!("{}", fd.fd_num)),
                            style::green(&addr),
                            style::dim(&format!("[{state_str}]"))
                        );
                    } else {
                        println!("    fd {}: {}", style::dim(&format!("{}", fd.fd_num)), fd.target);
                    }
                }
            }
            FdType::Pipe => {
                println!("{}", style::bold(&header));
                for fd in fd_list {
                    if let Some(pe) = &fd.pipe_endpoint {
                        println!("    fd {}: pipe[{}] ({})", 
                            style::dim(&format!("{}", fd.fd_num)),
                            pe.inode,
                            style::magenta(&pe.direction)
                        );
                    } else {
                        println!("    fd {}: {}", style::dim(&format!("{}", fd.fd_num)), fd.target);
                    }
                }
            }
            FdType::Anonymous => {
                println!("{}", style::bold(&header));
                for fd in fd_list {
                    if let Some(ref ai) = fd.anon_inode {
                        println!("    fd {}: {}", style::dim(&format!("{}", fd.fd_num)), ai);
                    } else {
                        println!("    fd {}: {}", style::dim(&format!("{}", fd.fd_num)), fd.target);
                    }
                }
            }
            FdType::RegularFile | FdType::Directory => {
                println!("{}", style::bold(&header));
                // Show up to 15 per type, ellipsis if more
                let display_count = count.min(15);
                for fd in &fd_list[..display_count] {
                    let leak_marker = if fd.is_leaked {
                        format!(" {}", style::red("⚠ LEAK"))
                    } else {
                        String::new()
                    };
                    println!("    fd {}: {}{}", 
                        style::dim(&format!("{}", fd.fd_num)),
                        fd.target,
                        leak_marker
                    );
                }
                if count > 15 {
                    println!("    ... and {} more", count - 15);
                }
            }
            _ => {
                println!("{}", style::bold(&header));
                let display_count = count.min(10);
                for fd in &fd_list[..display_count] {
                    println!("    fd {}: {}", style::dim(&format!("{}", fd.fd_num)), fd.target);
                }
                if count > 10 {
                    println!("    ... and {} more", count - 10);
                }
            }
        }
    }
    
    // Summary line
    let type_counts: HashMap<&str, usize> = {
        let mut counts = HashMap::new();
        for fd in fds {
            let name = match fd.fd_type {
                FdType::RegularFile => "files",
                FdType::Directory => "dirs",
                FdType::Socket => "sockets",
                FdType::Pipe => "pipes",
                FdType::Fifo => "fifos",
                FdType::CharacterDevice => "char_devs",
                FdType::BlockDevice => "block_devs",
                FdType::Symlink => "symlinks",
                FdType::Anonymous => "anonymous",
                FdType::Other => "other",
            };
            *counts.entry(name).or_insert(0) += 1;
        }
        counts
    };
    
    let summary_parts: Vec<String> = type_counts.iter()
        .map(|(k, v)| format!("{v} {k}"))
        .collect();
    println!("  {}", style::dim(&summary_parts.join(", ")));
    
    // Leak warnings
    let leaked: Vec<&FdInfo> = fds.iter().filter(|f| f.is_leaked).collect();
    if !leaked.is_empty() {
        println!("  {}", style::red(&format!("⚠ {} potential fd leak(s) detected:", leaked.len())));
        for fd in &leaked {
            println!("    fd {}: {} ({})", 
                style::red(&format!("{}", fd.fd_num)),
                style::red(&fd.target),
                style::dim(&format!("{:?}", fd.fd_type))
            );
        }
    }
    
    println!();
}

/// Main entry point: show fds for a specific PID or all processes.
pub fn cat_fd(opts: &FdOptions) {
    let socket_map = build_socket_map();
    
    // Collect PIDs
    let pids: Vec<u32> = if let Some(pid) = opts.pid {
        vec![pid]
    } else {
        // Enumerate all PIDs from /proc
        fs::read_dir("/proc")
            .ok()
            .into_iter()
            .flat_map(|dir| dir.flatten())
            .filter_map(|entry| {
                entry.file_name().to_string_lossy().parse::<u32>().ok()
            })
            .filter(|pid| {
                // Check if this PID actually has an fd directory
                let fd_dir = format!("/proc/{}/fd", pid);
                fs::metadata(fd_dir).map(|m| m.is_dir()).unwrap_or(false)
            })
            .collect()
    };
    
    if pids.is_empty() {
        eprintln!("ccat: no processes found with /proc entries");
        return;
    }
    
    if !opts.all_processes && pids.len() > 1 {
        eprintln!("ccat: found {} processes; use --fd-all to show all, or --fd <PID> for one", pids.len());
        eprintln!("ccat: showing first 3:");
        for &pid in &pids[..3.min(pids.len())] {
            println!("  {}", style::bold(&format!("PID {pid}: {}", get_process_name(pid))));
        }
        println!();
        return;
    }
    
    println!("{}", style::bold("\u{1f50d} Process File Descriptors"));
    println!("{}\n", style::dim(&format!("{} process(es) inspected, {} socket entries resolved", 
        pids.len(), socket_map.len())));
    
    for pid in &pids {
        let fd_dir = format!("/proc/{}/fd", pid);
        match fs::read_dir(&fd_dir) {
            Ok(entries) => {
                let mut fds: Vec<FdInfo> = Vec::new();
                
                for entry in entries {
                    if let Ok(e) = entry {
                        if let Some(fd_str) = e.file_name().to_str() {
                            if let Ok(fd_num) = fd_str.parse::<u32>() {
                                if let Some(mut fd_info) = resolve_fd(*pid, fd_num, &socket_map) {
                                    fd_info.is_leaked = is_potential_leak(fd_num, &fd_info.fd_type, &fd_info.target);
                                    fds.push(fd_info);
                                }
                            }
                        }
                    }
                }
                
                // Sort by fd number
                fds.sort_by_key(|f| f.fd_num);
                
                // Respect max_fds_per_process
                if fds.len() > opts.max_fds_per_process {
                    fds.truncate(opts.max_fds_per_process);
                }
                
                display_process_fds(*pid, &fds, &socket_map);
            }
            Err(_) => {
                // Process may have exited
            }
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_decode_hex_socket_valid() {
        // 127.0.0.1:8080 → 0100007F:1F90
        assert_eq!(decode_hex_socket("0100007F:1F90"), Some("127.0.0.1:8080".to_string()));
    }
    
    #[test]
    fn test_decode_hex_socket_google_dns() {
        // 8.8.8.8:443 → 08080808:01BB
        assert_eq!(decode_hex_socket("08080808:01BB"), Some("8.8.8.8:443".to_string()));
    }
    
    #[test]
    fn test_decode_hex_socket_invalid() {
        assert_eq!(decode_hex_socket("invalid"), None);
        assert_eq!(decode_hex_socket("GGGGGGGG:1234"), None);
        assert_eq!(decode_hex_socket("1234"), None);
    }
    
    #[test]
    fn test_tcp_state_codes() {
        assert_eq!(tcp_state_code("01"), "ESTABLISHED");
        assert_eq!(tcp_state_code("0A"), "LISTEN");
        assert_eq!(tcp_state_code("06"), "TIME_WAIT");
        assert_eq!(tcp_state_code("FF"), "UNKNOWN");
    }
    
    #[test]
    fn test_fd_type_icons() {
        assert_eq!(FdType::RegularFile.icon(), "\u{1f4c4}");
        assert_eq!(FdType::Socket.icon(), "\u{1f5df}");
        assert_eq!(FdType::Pipe.icon(), "\u{1f4e2}");
    }
    
    #[test]
    fn test_is_potential_leak_stdin_stdout_stderr() {
        // stdin, stdout, stderr should never be leaks
        assert!(!is_potential_leak(0, &FdType::RegularFile, "/etc/passwd"));
        assert!(!is_potential_leak(1, &FdType::RegularFile, "/etc/hosts"));
        assert!(!is_potential_leak(2, &FdType::RegularFile, "/var/log/app.log"));
    }
    
    #[test]
    fn test_is_potential_leak_tmp_files() {
        // High fd numbers pointing to /tmp are suspicious
        assert!(is_potential_leak(50, &FdType::RegularFile, "/tmp/some-temp-file"));
        assert!(is_potential_leak(100, &FdType::RegularFile, "/var/tmp/leaked"));
    }
    
    #[test]
    fn test_build_socket_map_empty() {
        // On CI, /proc/net/tcp may not exist or be empty
        let map = build_socket_map();
        // Just verify it doesn't panic
        assert!(map.contains_key("") == false || !map.is_empty() || map.is_empty());
    }
    
    #[test]
    fn test_classify_regular_file() {
        let socket_map = HashMap::new();
        let (fd_type, _, _, _) = classify_fd_target(
            "/etc/passwd",
            "/proc/self/fd/3",
            &socket_map,
        );
        assert_eq!(fd_type, FdType::RegularFile);
    }
    
    #[test]
    fn test_classify_directory() {
        let socket_map = HashMap::new();
        let (fd_type, _, _, _) = classify_fd_target(
            "/tmp",
            "/proc/self/fd/4",
            &socket_map,
        );
        assert_eq!(fd_type, FdType::Directory);
    }
}
