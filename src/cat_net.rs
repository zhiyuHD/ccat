//! Network connection viewer (`ccat --netstat`).
//!
//! Reads `/proc/net/tcp`, `/proc/net/tcp6`, `/proc/net/udp`, `/proc/net/udp6`
//! and cross-references socket inodes against `/proc/*/fd/` symlinks to
//! identify the owning process.
//!
//! Displays a coloured, paged table of sockets with:
//! - protocol (TCP/TCP6/UDP/UDP6)
//! - local address and port
//! - remote address and port
//! - connection state (LISTEN, ESTABLISHED, TIME_WAIT, …)
//! - owning PID and process name
//! - socket inode
//! - send/receive queue depth

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
}

// ── Data types ──

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Protocol {
    Tcp,
    Tcp6,
    Udp,
    Udp6,
}

impl Protocol {
    fn as_str(&self) -> &'static str {
        match self {
            Protocol::Tcp => "TCP",
            Protocol::Tcp6 => "TCP6",
            Protocol::Udp => "UDP",
            Protocol::Udp6 => "UDP6",
        }
    }

    fn color(&self) -> &'static str {
        match self {
            Protocol::Tcp => "\x1b[36m",   // cyan
            Protocol::Tcp6 => "\x1b[94m",  // bright blue
            Protocol::Udp => "\x1b[33m",   // yellow
            Protocol::Udp6 => "\x1b[93m",  // bright yellow
        }
    }
}

/// TCP state byte → human-readable label.
fn tcp_state_str(st: u8) -> &'static str {
    match st {
        0x01 => "ESTABLISHED",
        0x02 => "SYN_SENT",
        0x03 => "SYN_RECV",
        0x04 => "FIN_WAIT1",
        0x05 => "FIN_WAIT2",
        0x06 => "TIME_WAIT",
        0x07 => "CLOSE",
        0x08 => "CLOSE_WAIT",
        0x09 => "LAST_ACK",
        0x0A => "LISTEN",
        0x0B => "CLOSING",
        _ => "UNKNOWN",
    }
}

/// Colour a TCP state string for display.
fn color_state(state: &str) -> String {
    match state {
        "LISTEN"     => style::green(state),
        "ESTABLISHED" => style::yellow(state),
        "TIME_WAIT"  => style::red(state),
        "CLOSE_WAIT" => style::magenta(state),
        "FIN_WAIT1" | "FIN_WAIT2" | "CLOSING" | "LAST_ACK" => style::blue(state),
        _ => state.to_string(),
    }
}

/// A parsed socket entry.
#[derive(Debug, Clone)]
struct SocketEntry {
    protocol: Protocol,
    local_addr: String,
    local_port: u16,
    remote_addr: String,
    remote_port: u16,
    state: u8,        // 0 for UDP (stateless)
    tx_queue: u32,
    rx_queue: u32,
    uid: u32,
    inode: u64,
}

/// Process info matched to a socket.
#[derive(Debug, Clone)]
struct ProcMatch {
    pid: u32,
    comm: String,
}

/// A fully-resolved connection to display.
#[derive(Debug, Clone)]
struct Connection {
    protocol: Protocol,
    local: String,
    local_port: u16,
    remote: String,
    remote_port: u16,
    state: String,
    proc: Option<ProcMatch>,
    inode: u64,
    tx_queue: u32,
    rx_queue: u32,
}

// ── Hex address parsing for /proc/net/[tcp|udp] ──

/// Parse a hex-encoded IPv4 address from /proc/net/tcp.
/// Format: "0100007F" (little-endian hex of 32-bit network-order IP) → "127.0.0.1"
fn parse_ipv4(hex: &str) -> String {
    let bytes = hex_to_bytes(hex);
    if bytes.len() < 4 {
        return hex.to_string();
    }
    // /proc/net/tcp stores IPv4 addresses as hex dump of the in_addr struct.
    // On little-endian x86, bytes[0] is the least significant byte of the
    // 32-bit network-order value, so we reverse: bytes[3].bytes[2].bytes[1].bytes[0]
    format!("{}.{}.{}.{}", bytes[3], bytes[2], bytes[1], bytes[0])
}

/// Parse a hex-encoded IPv6 address from /proc/net/tcp6.
/// Format: "0000000000000000FFFF00000100007F" → "::ffff:127.0.0.1"
///
/// The kernel stores IPv6 addresses as 4 32-bit words in host byte order.
/// On little-endian x86, each word's bytes are reversed in the hex dump,
/// so we first convert from LE-grouped to network byte order.
fn parse_ipv6(hex: &str) -> String {
    // Pad to 32 hex chars (16 bytes)
    let padded = format!("{:0>32}", hex);
    let raw_bytes = hex_to_bytes(&padded);
    if raw_bytes.len() < 16 {
        return hex.to_string();
    }

    // Convert from LE word-grouped bytes to network byte order.
    // /proc/net/tcp6 dumps each 32-bit word in host byte order (LE on x86).
    // Reverse each 4-byte group to get proper in-network-order bytes.
    let mut bytes = [0u8; 16];
    for (i, chunk) in raw_bytes.chunks(4).enumerate() {
        let base = i * 4;
        bytes[base] = chunk[3];
        bytes[base + 1] = chunk[2];
        bytes[base + 2] = chunk[1];
        bytes[base + 3] = chunk[0];
    }

    // IPv4-mapped IPv6: ::ffff:a.b.c.d
    if bytes[0..10].iter().all(|&b| b == 0)
        && bytes[10] == 0xff && bytes[11] == 0xff
    {
        return format!(
            "::ffff:{}.{}.{}.{}",
            bytes[12], bytes[13], bytes[14], bytes[15]
        );
    }

    // All zero
    if bytes.iter().all(|&b| b == 0) {
        return "::".to_string();
    }

    // ::1 (loopback) — only last byte set
    if bytes[0..15].iter().all(|&b| b == 0) && bytes[15] == 1 {
        return "::1".to_string();
    }

    // Full IPv6: group 16 bytes into 8 colon-separated hex groups
    let groups: Vec<String> = bytes.chunks(2).map(|c| {
        format!("{:02x}{:02x}", c[0], c[1])
    }).collect();

    // Simplify: remove leading zeros, compress longest zero sequence
    let simplified: Vec<&str> = groups.iter().map(|g| {
        g.trim_start_matches('0')
    }).collect();
    let simplified: Vec<&str> = simplified.iter().map(|g| {
        if g.is_empty() { "0" } else { g }
    }).collect();

    // Find longest run of "0" groups for :: compression
    let mut best_start = 0usize;
    let mut best_len = 0usize;
    let mut cur_start = None;
    let mut cur_len = 0usize;

    for (i, g) in simplified.iter().enumerate() {
        if *g == "0" {
            if cur_start.is_none() {
                cur_start = Some(i);
                cur_len = 1;
            } else {
                cur_len += 1;
            }
            if cur_len > best_len || (cur_len == best_len && cur_start == Some(0usize)) {
                best_start = cur_start.unwrap();
                best_len = cur_len;
            }
        } else {
            cur_start = None;
            cur_len = 0;
        }
    }

    if best_len >= 2 {
        let before: Vec<&str> = simplified[..best_start].iter().copied().collect();
        let after: Vec<&str> = simplified[best_start + best_len..].iter().copied().collect();
        let mut parts = Vec::new();
        if !before.is_empty() {
            parts.push(before.join(":"));
        }
        parts.push(String::new()); // empty for ::
        if !after.is_empty() {
            parts.push(after.join(":"));
        }
        parts.join(":")
    } else {
        simplified.join(":")
    }
}

/// Convert hex string to byte vec (little-endian within each byte pair).
fn hex_to_bytes(hex: &str) -> Vec<u8> {
    let s = hex.trim();
    let mut bytes = Vec::with_capacity(s.len() / 2);
    let chars: Vec<char> = s.chars().collect();
    for chunk in chars.chunks(2) {
        if chunk.len() == 2 {
            if let Ok(b) = u8::from_str_radix(&chunk.iter().collect::<String>(), 16) {
                bytes.push(b);
            }
        }
    }
    bytes
}

/// Parse /proc/net/[tcp|tcp6|udp|udp6] formatted lines.
/// Returns list of SocketEntry.
fn parse_proc_net(data: &str, protocol: Protocol) -> Vec<SocketEntry> {
    let mut sockets = Vec::new();
    for line in data.lines().skip(1) {
        // Skip header line
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 12 {
            continue;
        }

        // Parse local address: hex_ip:hex_port
        let local_parts: Vec<&str> = parts[1].split(':').collect();
        if local_parts.len() < 2 {
            continue;
        }
        let local_ip_hex = local_parts[0];
        let local_port = u16::from_str_radix(local_parts[1], 16).unwrap_or(0);

        // Parse remote address
        let remote_parts: Vec<&str> = parts[2].split(':').collect();
        if remote_parts.len() < 2 {
            continue;
        }
        let remote_ip_hex = remote_parts[0];
        let remote_port = u16::from_str_radix(remote_parts[1], 16).unwrap_or(0);

        // Parse state (hex byte)
        let state = u8::from_str_radix(parts[3], 16).unwrap_or(0);

        // Parse queue sizes: tx:rx
        let queue_parts: Vec<&str> = parts[4].split(':').collect();
        let tx_queue = u32::from_str_radix(queue_parts.get(0).unwrap_or(&"0"), 16).unwrap_or(0);
        let rx_queue = u32::from_str_radix(queue_parts.get(1).unwrap_or(&"0"), 16).unwrap_or(0);

        // Parse UID
        let uid = parts.get(7).and_then(|s| s.parse().ok()).unwrap_or(0);

        // Parse inode
        let inode = parts.get(9).and_then(|s| s.parse().ok()).unwrap_or(0);

        // Format addresses based on protocol
        let local_addr = match protocol {
            Protocol::Tcp | Protocol::Udp => parse_ipv4(local_ip_hex),
            Protocol::Tcp6 | Protocol::Udp6 => parse_ipv6(local_ip_hex),
        };
        let remote_addr = match protocol {
            Protocol::Tcp | Protocol::Udp => parse_ipv4(remote_ip_hex),
            Protocol::Tcp6 | Protocol::Udp6 => parse_ipv6(remote_ip_hex),
        };

        sockets.push(SocketEntry {
            protocol,
            local_addr,
            local_port,
            remote_addr,
            remote_port,
            state,
            tx_queue,
            rx_queue,
            uid,
            inode,
        });
    }
    sockets
}

// ── Process-to-socket matching ──

/// Build a map from socket inode → (pid, comm) by scanning /proc/*/fd/.
fn build_inode_map() -> HashMap<u64, ProcMatch> {
    let mut map = HashMap::new();

    let proc_dir = Path::new("/proc");
    let entries = match fs::read_dir(proc_dir) {
        Ok(e) => e,
        Err(_) => return map,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };

        // Only numeric PID directories
        let pid: u32 = match name_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Read process comm
        let comm = read_comm(pid);

        // Read fd/ directory
        let fd_dir = entry.path().join("fd");
        let fd_entries = match fs::read_dir(&fd_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for fd_entry in fd_entries.flatten() {
            let link_path = fd_entry.path();
            let link = match fs::read_link(&link_path) {
                Ok(l) => l,
                Err(_) => continue,
            };
            let link_str = link.to_string_lossy();

            // Socket symlinks look like: socket:[12345]
            if let Some(inode_str) = link_str.strip_prefix("socket:[") {
                if let Some(inode_end) = inode_str.find(']') {
                    if let Ok(inode) = inode_str[..inode_end].parse::<u64>() {
                        map.entry(inode).or_insert(ProcMatch {
                            pid,
                            comm: comm.clone(),
                        });
                    }
                }
            }
        }
    }

    map
}

/// Read /proc/<pid>/comm to get the process name.
fn read_comm(pid: u32) -> String {
    let path = format!("/proc/{pid}/comm");
    fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| String::from("?"))
}

// ── Port-to-service mapping (well-known ports) ──

/// Build a map of port → service name from /etc/services.
fn load_service_map() -> HashMap<u16, String> {
    let mut map = HashMap::new();
    if let Ok(data) = fs::read_to_string("/etc/services") {
        for line in data.lines() {
            let line = line.trim();
            // Skip comments and empty
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Some(port_end) = parts[1].find('/') {
                    if let Ok(port) = parts[1][..port_end].parse::<u16>() {
                        // Only add if not already mapped (first wins)
                        map.entry(port).or_insert_with(|| parts[0].to_string());
                    }
                }
            }
        }
    }
    map
}

// ── Display ──

/// Options for filtering the connection table.
#[derive(Debug, Clone, Default)]
pub struct NetstatOptions {
    /// Show TCP only
    pub tcp_only: bool,
    /// Show UDP only
    pub udp_only: bool,
    /// Show listening sockets only
    pub listening_only: bool,
    /// Filter by PID
    pub pid_filter: Option<u32>,
}

/// Main entry point: read /proc/net/*, resolve processes, render output.
pub fn cat_netstat(opts: &NetstatOptions) {
    // Read all socket tables
    let mut connections: Vec<Connection> = Vec::new();

    if !opts.udp_only {
        if let Ok(data) = fs::read_to_string("/proc/net/tcp") {
            for sock in parse_proc_net(&data, Protocol::Tcp) {
                if opts.listening_only && sock.state != 0x0A { continue; }
                connections.push(sock_to_connection(sock));
            }
        }
        if let Ok(data) = fs::read_to_string("/proc/net/tcp6") {
            for sock in parse_proc_net(&data, Protocol::Tcp6) {
                if opts.listening_only && sock.state != 0x0A { continue; }
                connections.push(sock_to_connection(sock));
            }
        }
    }

    if !opts.tcp_only {
        if let Ok(data) = fs::read_to_string("/proc/net/udp") {
            for sock in parse_proc_net(&data, Protocol::Udp) {
                if opts.listening_only {
                    // UDP is stateless — treat any socket with only local addr bound as "listening"
                    if sock.remote_addr == "0.0.0.0" && sock.remote_port == 0 {
                        // This is a listening UDP socket
                    } else {
                        continue;
                    }
                }
                connections.push(sock_to_connection(sock));
            }
        }
        if let Ok(data) = fs::read_to_string("/proc/net/udp6") {
            for sock in parse_proc_net(&data, Protocol::Udp6) {
                if opts.listening_only {
                    if sock.remote_addr == "::" && sock.remote_port == 0 {
                        // listening UDP6
                    } else {
                        continue;
                    }
                }
                connections.push(sock_to_connection(sock));
            }
        }
    }

    // Build inode→process map once
    let inode_map = build_inode_map();
    let service_map = load_service_map();

    // Resolve processes
    for conn in &mut connections {
        conn.proc = inode_map.get(&conn.inode).cloned();
    }

    // Apply PID filter
    if let Some(pid) = opts.pid_filter {
        connections.retain(|c| c.proc.as_ref().map(|p| p.pid) == Some(pid));
    }

    // Header info
    let tcp_count = connections.iter().filter(|c| matches!(c.protocol, Protocol::Tcp | Protocol::Tcp6)).count();
    let udp_count = connections.iter().filter(|c| matches!(c.protocol, Protocol::Udp | Protocol::Udp6)).count();
    let est_count = connections.iter().filter(|c| c.state == "ESTABLISHED").count();
    let listen_count = connections.iter().filter(|c| c.state == "LISTEN").count();
    let time_wait_count = connections.iter().filter(|c| c.state == "TIME_WAIT").count();

    eprintln!(
        "{}  {}",
        style::bold("Network Connections"),
        style::dim(&format!("{:3} TCP, {:3} UDP", tcp_count, udp_count)),
    );
    eprintln!(
        " {}",
        format!(
            " │ {} {} {} {}",
            style::green(&format!("{:3} LISTEN", listen_count)),
            style::yellow(&format!("{:3} ESTAB", est_count)),
            style::red(&format!("{:3} TIME_WAIT", time_wait_count)),
            if connections.len() > est_count + listen_count + time_wait_count {
                format!("{:3} other", connections.len() - est_count - listen_count - time_wait_count)
            } else {
                String::new()
            }
        ),
    );
    eprintln!("{}", style::dim(&format!(" {} total sockets", connections.len())));

    if connections.is_empty() {
        println!("{}", style::dim("(no matching connections)"));
        return;
    }

    // Compute column widths
    let mut max_proto = 5;
    let mut max_local = 23;
    let mut max_remote = 23;
    let mut max_state = 11;
    let mut max_proc = 10;

    for conn in &connections {
        max_proto = max_proto.max(conn.protocol.as_str().len());
        let local_fmt = format!("{}:{}", conn.local, conn.local_port);
        let remote_fmt = if conn.state == "LISTEN" {
            "-".to_string()
        } else {
            format!("{}:{}", conn.remote, conn.remote_port)
        };
        max_local = max_local.max(local_fmt.len());
        if remote_fmt.len() > max_remote {
            max_remote = remote_fmt.len();
        }
        if let Some(ref p) = conn.proc {
            let proc_str = format!("{}({})", p.comm, p.pid);
            max_proc = max_proc.max(proc_str.len());
        }
    }

    // ── Render table ──
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    let use_pager = atty::is(atty::Stream::Stdout);
    let mut lines: Vec<String> = Vec::new();

    // Table header
    let header = format!(
        "{:<proto$} │ {:<local$} │ {:<remote$} │ {:<state$} │ {:<proc$}",
        "PROTO",
        "LOCAL",
        "REMOTE",
        "STATE",
        "PROCESS",
        proto = max_proto + 4,
        local = max_local,
        remote = max_remote,
        state = max_state,
        proc = max_proc,
    );
    let sep = style::dim(&"─".repeat(header.len()));
    lines.push(format!("{}", sep));
    lines.push(format!("{}", style::dim(&header)));
    lines.push(format!("{}", sep));

    for conn in &connections {
        let local_fmt = format!("{}:{}", conn.local, conn.local_port);
        let remote_fmt = if conn.state == "LISTEN" {
            style::dim("*").to_string()
        } else {
            let port_label = service_map.get(&conn.remote_port)
                .map(|svc| format!("({})", svc))
                .unwrap_or_default();
            format!("{}:{}{}", conn.remote, conn.remote_port, port_label)
        };

        let proto_colored = format!(
            "{}{}\x1b[0m",
            conn.protocol.color(),
            conn.protocol.as_str()
        );

        let state_colored = color_state(&conn.state);

        let proc_str = match conn.proc {
            Some(ref p) => format!("{}({})", p.comm, p.pid),
            None => style::dim("-").to_string(),
        };

        // Queue indicator
        let queue_str = if conn.tx_queue > 0 || conn.rx_queue > 0 {
            format!(" ⬆{}⬇{}", conn.tx_queue, conn.rx_queue)
        } else {
            String::new()
        };

        let line = format!(
            "{:<proto$} │ {:<local$} │ {:<remote$} │ {:<state$} │ {:<proc$}{}",
            proto_colored,
            local_fmt,
            remote_fmt,
            state_colored,
            proc_str,
            queue_str,
            proto = max_proto + 4,
            local = max_local,
            remote = max_remote,
            state = max_state,
            proc = max_proc,
        );
        lines.push(line);
    }

    lines.push(format!("{}", sep));

    // Either page or print
    if use_pager && lines.len() > 20 {
        // Use pager
        #[cfg(not(test))]
        super::pager::run_pager(&lines);
        #[cfg(test)]
        for line in &lines {
            println!("{line}");
        }
    } else {
        for line in &lines {
            let _ = writeln!(handle, "{line}");
        }
    }
}

fn sock_to_connection(sock: SocketEntry) -> Connection {
    let state_str = match sock.protocol {
        Protocol::Tcp | Protocol::Tcp6 => tcp_state_str(sock.state).to_string(),
        Protocol::Udp | Protocol::Udp6 => String::from("UDP"),
    };

    Connection {
        protocol: sock.protocol,
        local: sock.local_addr,
        local_port: sock.local_port,
        remote: sock.remote_addr,
        remote_port: sock.remote_port,
        state: state_str,
        proc: None,
        inode: sock.inode,
        tx_queue: sock.tx_queue,
        rx_queue: sock.rx_queue,
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ipv4_localhost() {
        assert_eq!(parse_ipv4("0100007F"), "127.0.0.1");
    }

    #[test]
    fn test_parse_ipv4_any() {
        assert_eq!(parse_ipv4("00000000"), "0.0.0.0");
    }

    #[test]
    fn test_parse_ipv4_gateway() {
        assert_eq!(parse_ipv4("0201A8C0"), "192.168.1.2");
    }

    #[test]
    fn test_parse_ipv6_loopback() {
        assert_eq!(parse_ipv6("00000000000000000000000001000000"), "::1");
    }

    #[test]
    fn test_parse_ipv6_v4mapped() {
        assert_eq!(parse_ipv6("0000000000000000FFFF00000100007F"), "::ffff:127.0.0.1");
    }

    #[test]
    fn test_parse_ipv6_all_zero() {
        assert_eq!(parse_ipv6("00000000000000000000000000000000"), "::");
    }

    #[test]
    fn test_parse_ipv6_short() {
        // The real format from /proc/net/tcp6 for :: is actually just empty/zeros
        let result = parse_ipv6("00000000000000000000000000000000");
        assert_eq!(result, "::");
    }

    #[test]
    fn test_tcp_state_names() {
        assert_eq!(tcp_state_str(0x01), "ESTABLISHED");
        assert_eq!(tcp_state_str(0x0A), "LISTEN");
        assert_eq!(tcp_state_str(0x06), "TIME_WAIT");
        assert_eq!(tcp_state_str(0x04), "FIN_WAIT1");
        assert_eq!(tcp_state_str(0xFF), "UNKNOWN");
    }

    #[test]
    fn test_parse_proc_net_tcp() {
        // Sample /proc/net/tcp content
        let sample = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
                      0: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12729 1 0000000000000000 100 0 0 10 0\n\
                      1: 0100007F:0277 00000000:0000 0A 00000000:00000000 00:00000000 00000000   999        0 23456 1 0000000000000000 100 0 0 10 0\n";
        let sockets = parse_proc_net(sample, Protocol::Tcp);
        assert_eq!(sockets.len(), 2);

        // First socket: 0.0.0.0:22 LISTEN
        assert_eq!(sockets[0].local_addr, "0.0.0.0");
        assert_eq!(sockets[0].local_port, 22);
        assert_eq!(sockets[0].state, 0x0A);
        assert_eq!(sockets[0].inode, 12729);

        // Second socket: 127.0.0.1:631 LISTEN
        assert_eq!(sockets[1].local_addr, "127.0.0.1");
        assert_eq!(sockets[1].local_port, 631);
        assert_eq!(sockets[1].state, 0x0A);
        assert_eq!(sockets[1].inode, 23456);
        assert_eq!(sockets[1].uid, 999);
    }

    #[test]
    fn test_parse_proc_net_tcp_established() {
        let sample = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
                      0: 0100007F:C350 0201A8C0:01BB 01 00000000:00000000 00:00000000 00000000  1000        0 54321 1 0000000000000000 100 0 0 10 0\n";
        let sockets = parse_proc_net(sample, Protocol::Tcp);
        assert_eq!(sockets.len(), 1);
        assert_eq!(sockets[0].local_addr, "127.0.0.1");
        assert_eq!(sockets[0].local_port, 50000);
        assert_eq!(sockets[0].remote_addr, "192.168.1.2");
        assert_eq!(sockets[0].remote_port, 443);
        assert_eq!(sockets[0].state, 0x01); // ESTABLISHED
        assert_eq!(sockets[0].inode, 54321);
    }

    #[test]
    fn test_parse_proc_net_udp() {
        let sample = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
                      0: 00000000:0044 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 34567 1 0000000000000000 100 0 0 10 0\n";
        let sockets = parse_proc_net(sample, Protocol::Udp);
        assert_eq!(sockets.len(), 1);
        assert_eq!(sockets[0].local_addr, "0.0.0.0");
        assert_eq!(sockets[0].local_port, 68); // DHCP client
        assert_eq!(sockets[0].state, 0x07); // CLOSE for UDP
        assert_eq!(sockets[0].inode, 34567);
    }

    #[test]
    fn test_empty_proc_net() {
        let sockets = parse_proc_net("", Protocol::Tcp);
        assert!(sockets.is_empty());
    }

    #[test]
    fn test_header_only() {
        let sockets = parse_proc_net("  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n", Protocol::Tcp);
        assert!(sockets.is_empty());
    }

    #[test]
    fn test_hex_to_bytes() {
        assert_eq!(hex_to_bytes("0100007F"), vec![1, 0, 0, 127]);
        assert_eq!(hex_to_bytes("00000000"), vec![0, 0, 0, 0]);
    }

    #[test]
    fn test_fmt_localhost_port() {
        let conn = Connection {
            protocol: Protocol::Tcp,
            local: "127.0.0.1".to_string(),
            local_port: 22,
            remote: "0.0.0.0".to_string(),
            remote_port: 0,
            state: "LISTEN".to_string(),
            proc: Some(ProcMatch { pid: 1234, comm: "sshd".to_string() }),
            inode: 99999,
            tx_queue: 0,
            rx_queue: 0,
        };
        assert_eq!(conn.local, "127.0.0.1");
        assert_eq!(conn.proc.as_ref().unwrap().pid, 1234);
        assert_eq!(conn.proc.as_ref().unwrap().comm, "sshd");
    }

    #[test]
    fn test_parse_ipv6_link_local() {
        // fe80::1 → kernel hex: s6_addr32 in host byte order
        let result = parse_ipv6("000080FE000000000000000001000000");
        assert_eq!(result, "fe80::1");
    }

    #[test]
    fn test_protocol_colors() {
        assert!(Protocol::Tcp.color().contains("36m"));   // cyan
        assert!(Protocol::Udp.color().contains("33m"));   // yellow
    }

    #[test]
    fn test_state_coloring() {
        let colored = color_state("LISTEN");
        assert!(colored.contains("\x1b[32m")); // green
        assert!(colored.contains("LISTEN"));

        let est = color_state("ESTABLISHED");
        assert!(est.contains("\x1b[33m")); // yellow
    }

    #[test]
    fn test_proc_net_tcp_with_queue() {
        // Line with non-zero queue
        let sample = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
                      0: 0100007F:C350 0201A8C0:01BB 01 0000000A:00000005 00:00000000 00000000  1000        0 54321 1 0000000000000000 100 0 0 10 0\n";
        let sockets = parse_proc_net(sample, Protocol::Tcp);
        assert_eq!(sockets.len(), 1);
        assert_eq!(sockets[0].tx_queue, 10);
        assert_eq!(sockets[0].rx_queue, 5);
    }
}
