// cat_git — Pretty-printer for Git objects (blob, tree, commit, tag)
//
// Usage:
//   ccat --git <sha|ref>          Display a git object
//   ccat --git <repo-path>        Auto-detect: show git log for the repo
//   ccat --git <sha> --stat       Show commit with diff stat
//   ccat --git tree <path>        Show tree structure of a path in the repo

use flate2::read::GzDecoder;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

// ── Constants ──

const C_RESET: &str = "\x1b[0m";
const C_BOLD: &str = "\x1b[1m";
const C_GREEN: &str = "\x1b[32m";
const C_BLUE: &str = "\x1b[34m";
const C_CYAN: &str = "\x1b[36m";
const C_YELLOW: &str = "\x1b[33m";
const C_RED: &str = "\x1b[31m";
const C_MAGENTA: &str = "\x1b[35m";
const C_DIM: &str = "\x1b[2m";
const C_WHITE: &str = "\x1b[37m";

fn c(color: &str, text: impl std::fmt::Display) -> String {
    format!("{color}{text}{C_RESET}")
}

// ── Types ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitObjType {
    Blob,
    Tree,
    Commit,
    Tag,
}

impl GitObjType {
    fn from_str(s: &str) -> Option<GitObjType> {
        match s {
            "blob" => Some(GitObjType::Blob),
            "tree" => Some(GitObjType::Tree),
            "commit" => Some(GitObjType::Commit),
            "tag" => Some(GitObjType::Tag),
            _ => None,
        }
    }
}

struct GitObject {
    obj_type: GitObjType,
    data: Vec<u8>,
}

#[derive(Debug, Clone)]
struct TreeEntry {
    mode: String,
    name: String,
    oid: [u8; 20],
}

#[derive(Debug, Clone)]
struct CommitEntry {
    tree_sha: String,
    parents: Vec<String>,
    author: String,
    committer: String,
    message: String,
}

#[derive(Debug)]
pub enum GitMode {
    Auto,
    Log,
    Tree(String),
    Cat,
}

#[derive(Debug)]
pub struct GitOpts {
    pub input: String,
    pub mode: GitMode,
    pub show_stat: bool,
}

// ── Git object reading ──

fn find_git_dir(path: &Path) -> Option<PathBuf> {
    let search_base = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    };

    let mut current = search_base.canonicalize().ok()?;
    loop {
        let git_dir = current.join(".git");
        if git_dir.exists() {
            return Some(git_dir);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

fn read_loose_object(git_dir: &Path, sha: &str) -> Option<GitObject> {
    let sha_lower = sha.to_lowercase();
    let prefix = &sha_lower[..2];
    let suffix = &sha_lower[2..];

    let obj_path = git_dir.join("objects").join(prefix).join(suffix);
    let raw = match fs::read(&obj_path) {
        Ok(d) => d,
        Err(_) => return None,
    };

    let mut decoder = GzDecoder::new(&raw[..]);
    let mut content = Vec::new();
    if decoder.read_to_end(&mut content).is_err() {
        return None;
    }

    let null_pos = content.iter().position(|&b| b == b'\0')?;
    let header = std::str::from_utf8(&content[..null_pos]).ok()?;
    let content_data = content[null_pos + 1..].to_vec();

    let parts: Vec<&str> = header.split_whitespace().collect();
    if parts.len() != 2 {
        return None;
    }

    let obj_type = GitObjType::from_str(parts[0])?;
    Some(GitObject { obj_type, data: content_data })
}

fn resolve_ref(git_dir: &Path, ref_path: &str) -> Option<String> {
    // Check refs/heads, refs/tags, refs/remotes
    for ref_base in &["refs/heads", "refs/tags", "refs/remotes"] {
        let path = git_dir.join(ref_base).join(ref_path);
        if path.is_file() {
            return fs::read_to_string(&path)
                .ok()
                .map(|s| s.trim().to_string());
        }
    }

    // Check HEAD
    if ref_path == "HEAD" || ref_path == "HEAD{}" {
        let head_path = git_dir.join("HEAD");
        if head_path.is_file() {
            let content = fs::read_to_string(&head_path).ok()?;
            let content = content.trim();
            if content.starts_with("ref: ") {
                let target = &content[5..];
                return resolve_ref(git_dir, target);
            } else {
                return Some(content.to_string());
            }
        }
    }

    // Check packed-refs
    let packed_refs = git_dir.join("packed-refs");
    if packed_refs.is_file() {
        if let Ok(content) = fs::read_to_string(&packed_refs) {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let sha = parts[0];
                    let ref_name = parts[1];
                    if ref_name == ref_path || ref_name.ends_with(&format!("/{ref_path}")) {
                        return Some(sha.to_string());
                    }
                }
            }
        }
    }

    None
}

fn resolve_sha(git_dir: &Path, input: &str) -> Option<String> {
    if input.len() == 40 && input.chars().all(|c| c.is_ascii_hexdigit()) {
        // Check packed-refs first for full SHA
        let packed_refs = git_dir.join("packed-refs");
        if packed_refs.is_file() {
            if let Ok(content) = fs::read_to_string(&packed_refs) {
                for line in content.lines() {
                    let line = line.trim();
                    if line.starts_with('#') || line.is_empty() {
                        continue;
                    }
                    if let Some(sha) = line.split_whitespace().next() {
                        if sha == input {
                            return Some(input.to_string());
                        }
                    }
                }
            }
        }

        // Check loose objects
        if read_loose_object(git_dir, input).is_some() {
            return Some(input.to_string());
        }

        // Try packed object database
        let pack_dir = git_dir.join("objects").join("pack");
        if pack_dir.is_dir() {
            if let Ok(entries) = fs::read_dir(&pack_dir) {
                for entry in entries.flatten() {
                    let fname = entry.file_name();
                    if fname.to_string_lossy().starts_with("pack") && fname.to_string_lossy().ends_with(".idx") {
                        // Packed objects - try reading from pack
                        if let Some(resolved) = resolve_packed_sha(git_dir, input) {
                            return Some(resolved);
                        }
                    }
                }
            }
        }

        return Some(input.to_string());
    }

    // Short SHA (4+ hex chars) - brute force scan
    if input.len() >= 4 && input.chars().all(|c| c.is_ascii_hexdigit()) {
        let objects_dir = git_dir.join("objects");
        if objects_dir.is_dir() {
            if let Ok(prefix_entries) = fs::read_dir(&objects_dir) {
                for prefix_entry in prefix_entries.flatten() {
                    let prefix = prefix_entry.file_name();
                    let prefix_str = prefix.to_string_lossy();
                    if prefix_str.len() == 2 {
                        let prefix_dir = prefix_entry.path();
                        if let Ok(suffix_entries) = fs::read_dir(&prefix_dir) {
                            for suffix_entry in suffix_entries.flatten() {
                                let suffix = suffix_entry.file_name();
                                let full_sha = format!("{prefix_str}{}", suffix.to_string_lossy());
                                if full_sha.starts_with(input) {
                                    return Some(full_sha);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Try as a ref name
    resolve_ref(git_dir, input)
}

/// Resolve a SHA from packed objects by scanning pack indices
fn resolve_packed_sha(git_dir: &Path, short_sha: &str) -> Option<String> {
    let pack_dir = git_dir.join("objects").join("pack");
    if !pack_dir.is_dir() {
        return None;
    }

    // Read pack files and their idx files
    if let Ok(entries) = fs::read_dir(&pack_dir) {
        let mut pack_files: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let fs = fname.to_string_lossy();
            if fs.starts_with("pack") && fs.ends_with(".pack") {
                pack_files.push(entry.path());
            }
        }

        for pack_path in pack_files {
            // Read corresponding idx file
            let mut idx_path = pack_path.clone();
            idx_path.set_extension("idx");

            if let Ok(idx_data) = fs::read(&idx_path) {
                // v2 idx file: magic (4 bytes) + version (4 bytes) + entries (4 bytes) + checksum (20 bytes)
                if idx_data.len() > 24 && &idx_data[0..4] == b"\xfftOc" {
                    let num_entries = u32::from_be_bytes([
                        idx_data[8], idx_data[9], idx_data[10], idx_data[11],
                    ]) as usize;

                    // Fan-out table: 256 entries of 4 bytes each
                    let fanout_start = 24;
                    let last_fanout = num_entries; // fanout[255] = total entries

                    // Binary search through fan-out for the right bucket
                    let bucket = u8::from_str_radix(&short_sha[..2], 16).unwrap_or(0) as usize;
                    let fanout_bucket = u32::from_be_bytes([
                        idx_data[fanout_start + bucket * 4],
                        idx_data[fanout_start + bucket * 4 + 1],
                        idx_data[fanout_start + bucket * 4 + 2],
                        idx_data[fanout_start + bucket * 4 + 3],
                    ]) as usize;
                    let fanout_next = if bucket < 255 {
                        u32::from_be_bytes([
                            idx_data[fanout_start + (bucket + 1) * 4],
                            idx_data[fanout_start + (bucket + 1) * 4 + 1],
                            idx_data[fanout_start + (bucket + 1) * 4 + 2],
                            idx_data[fanout_start + (bucket + 1) * 4 + 3],
                        ]) as usize
                    } else {
                        last_fanout
                    };

                    // SHA table starts after fan-out
                    let sha_start = fanout_start + 256 * 4;
                    for i in fanout_bucket..fanout_next {
                        let offset = sha_start + i * 20;
                        if offset + 20 <= idx_data.len() {
                            let sha_bytes = &idx_data[offset..offset + 20];
                            let sha_hex: String = sha_bytes.iter().map(|b| format!("{b:02x}")).collect();
                            if sha_hex.starts_with(short_sha) {
                                return Some(sha_hex);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn read_git_object(git_dir: &Path, input: &str) -> Option<(GitObject, String)> {
    let sha = resolve_sha(git_dir, input)?;
    let obj = read_loose_object(git_dir, &sha).or_else(|| {
        // Try packed object - decompress from pack
        resolve_packed_object(git_dir, &sha)
    });
    obj.map(|o| (o, sha))
}

/// Read a packed git object by decompressing from packfile
fn resolve_packed_object(git_dir: &Path, sha: &str) -> Option<GitObject> {
    let pack_dir = git_dir.join("objects").join("pack");
    if !pack_dir.is_dir() {
        return None;
    }

    let mut pack_path = None;
    let mut idx_path = None;

    if let Ok(entries) = fs::read_dir(&pack_dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let fs = fname.to_string_lossy();
            if fs.starts_with("pack") && fs.ends_with(".pack") {
                pack_path = Some(entry.path());
            }
            if fs.starts_with("pack") && fs.ends_with(".idx") {
                idx_path = Some(entry.path());
            }
        }
    }

    let pack_path = pack_path?;
    let idx_path = idx_path?;

    // Find the offset of this SHA in the pack index
    let idx_data = fs::read(&idx_path).ok()?;
    let mut offset = None;

    if idx_data.len() > 24 && &idx_data[0..4] == b"\xfftOc" {
        let num_entries = u32::from_be_bytes([
            idx_data[8], idx_data[9], idx_data[10], idx_data[11],
        ]) as usize;

        let fanout_start = 24;
        let last_fanout = num_entries;
        let bucket = u8::from_str_radix(&sha[..2], 16).unwrap_or(0) as usize;
        let fanout_bucket = u32::from_be_bytes([
            idx_data[fanout_start + bucket * 4],
            idx_data[fanout_start + bucket * 4 + 1],
            idx_data[fanout_start + bucket * 4 + 2],
            idx_data[fanout_start + bucket * 4 + 3],
        ]) as usize;
        let fanout_next = if bucket < 255 {
            u32::from_be_bytes([
                idx_data[fanout_start + (bucket + 1) * 4],
                idx_data[fanout_start + (bucket + 1) * 4 + 1],
                idx_data[fanout_start + (bucket + 1) * 4 + 2],
                idx_data[fanout_start + (bucket + 1) * 4 + 3],
            ]) as usize
        } else {
            last_fanout
        };

        let sha_start = fanout_start + 256 * 4;
        for i in fanout_bucket..fanout_next {
            let s_offset = sha_start + i * 20;
            if s_offset + 20 <= idx_data.len() {
                let sha_bytes = &idx_data[s_offset..s_offset + 20];
                let sha_hex: String = sha_bytes.iter().map(|b| format!("{b:02x}")).collect();
                if sha_hex == sha {
                    // CRC32 table follows SHAs
                    let crc_start = sha_start + num_entries * 20;
                    let crc_offset = crc_start + i * 4;
                    if crc_offset + 4 <= idx_data.len() {
                        let crc = u32::from_be_bytes([
                            idx_data[crc_offset],
                            idx_data[crc_offset + 1],
                            idx_data[crc_offset + 2],
                            idx_data[crc_offset + 3],
                        ]);
                        // Offset table follows CRCs
                        let offset_start = crc_start + num_entries * 4;
                        let off = offset_start + i * 4;
                        if off + 4 <= idx_data.len() {
                            let pack_offset = u32::from_be_bytes([
                                idx_data[off],
                                idx_data[off + 1],
                                idx_data[off + 2],
                                idx_data[off + 3],
                            ]) as usize;
                            offset = Some(pack_offset);
                        }
                    }
                    break;
                }
            }
        }
    }

    let pack_offset = offset?;

    // Read the pack file and find the object
    let pack_data = fs::read(&pack_path).ok()?;
    let mut pos = pack_offset;

    // Read object header (variable-length encoding)
    let mut obj_type_num = 0;
    let mut size = 0usize;
    let mut shift = 0;
    let mut first_byte = true;

    while pos < pack_data.len() {
        let byte = pack_data[pos];
        pos += 1;

        if first_byte {
            obj_type_num = (byte >> 4) & 0x07;
            first_byte = false;
        }

        size |= ((byte & 0x7f) as usize) << shift;
        shift += 7;

        if (byte & 0x80) == 0 {
            break;
        }
    }

    let obj_type = match obj_type_num {
        1 => GitObjType::Commit,
        2 => GitObjType::Tree,
        3 => GitObjType::Tag,
        4 => GitObjType::Blob,
        6 => GitObjType::Blob,
        7 => GitObjType::Blob,
        _ => return None,
    };

    if obj_type == GitObjType::Blob {
        // OFS_DELTA: read negative offset
        let mut neg_offset = 0usize;
        let mut s = 0;
        while pos < pack_data.len() {
            let byte = pack_data[pos];
            pos += 1;
            neg_offset |= ((byte & 0x7f) as usize) << s;
            s += 7;
            if (byte & 0x80) == 0 {
                break;
            }
        }
        pos = pos.saturating_sub(neg_offset);
    } else if obj_type == GitObjType::Blob {
        // REF_DELTA: skip 20-byte SHA reference
        pos += 20;
    }

    // Decompress from pack
    let mut decoder = GzDecoder::new(&pack_data[pos..]);
    let mut content = Vec::new();
    if decoder.read_to_end(&mut content).is_err() {
        return None;
    }

    Some(GitObject { obj_type, data: content })
}

// ── Tree parsing ──

fn parse_tree(data: &[u8]) -> Option<Vec<TreeEntry>> {
    let mut entries = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        let space_pos = data[pos..].iter().position(|&b| b == b' ')?;
        let space_pos = pos + space_pos;

        let mode = std::str::from_utf8(&data[pos..space_pos]).ok()?.to_string();

        let null_pos = data[space_pos + 1..].iter().position(|&b| b == b'\0')?;
        let null_pos = space_pos + 1 + null_pos;

        let name = std::str::from_utf8(&data[space_pos + 1..null_pos]).ok()?.to_string();

        if null_pos + 20 > data.len() {
            break;
        }
        let mut oid = [0u8; 20];
        oid.copy_from_slice(&data[null_pos..null_pos + 20]);

        entries.push(TreeEntry { mode, name, oid });
        pos = null_pos + 20;
    }

    Some(entries)
}

// ── Commit parsing ──

fn parse_commit(data: &[u8]) -> Option<CommitEntry> {
    let text = std::str::from_utf8(data).ok()?;

    let mut tree_sha = String::new();
    let mut parents = Vec::new();
    let mut author = String::new();
    let mut committer = String::new();
    let mut in_headers = true;
    let mut message_lines = Vec::new();

    for line in text.lines() {
        if in_headers {
            if line.is_empty() {
                in_headers = false;
                continue;
            }
            if line.starts_with("tree ") {
                tree_sha = line[5..].trim().to_string();
            } else if line.starts_with("parent ") {
                parents.push(line[7..].trim().to_string());
            } else if line.starts_with("author ") {
                author = line[7..].trim_end().to_string();
            } else if line.starts_with("committer ") {
                committer = line[10..].trim_end().to_string();
            }
        } else {
            message_lines.push(line);
        }
    }

    let message = message_lines.join("\n");

    Some(CommitEntry {
        tree_sha,
        parents,
        author,
        committer,
        message,
    })
}

// ── Display helpers ──

fn truncate_sha(sha: &str) -> String {
    if sha.len() > 12 {
        format!("{}…{}", &sha[..7], &sha[sha.len() - 5..])
    } else {
        sha.to_string()
    }
}

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn format_relative_time(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = now - ts;

    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else if diff < 2592000 {
        format!("{}d ago", diff / 86400)
    } else if diff < 31536000 {
        format!("{}mo ago", diff / 2592000)
    } else {
        format!("{}y ago", diff / 31536000)
    }
}

// ── Display: Tree ──

fn display_tree(obj: &GitObject, sha: &str) {
    let entries = match parse_tree(&obj.data) {
        Some(e) => e,
        None => {
            eprintln!("ccat: error: failed to parse tree object");
            return;
        }
    };

    println!(
        "{}\n{}",
        c(C_CYAN, "┌─ tree"),
        c(C_DIM, format!("{sha}  ({} entries)\n", entries.len()))
    );

    for (i, entry) in entries.iter().enumerate() {
        let is_last = i == entries.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };

        let mut icon = "";
        let mut name_color = C_WHITE;
        let mut extra = "";

        match entry.mode.as_str() {
            "040000" => {
                icon = "📁 ";
                name_color = C_BLUE;
                extra = " (directory)";
            }
            "120000" => {
                icon = "🔗 ";
                name_color = C_YELLOW;
                extra = " (symlink)";
            }
            "160000" => {
                icon = "📦 ";
                name_color = C_MAGENTA;
                extra = " (submodule)";
            }
            _ => {
                if entry.mode.starts_with("10") {
                    let perm = u16::from_str_radix(&entry.mode, 8).unwrap_or(0);
                    if perm & 0o111 != 0 {
                        icon = "💥 ";
                        name_color = C_RED;
                    }
                }
            }
        }

        let name_display = format!(
            "{}{}{}{}{}",
            c(C_DIM, connector),
            icon,
            c(name_color, &entry.name),
            extra,
            if is_last { "" } else { "" }
        );
        println!("{}", name_display);
    }

    println!("{C_CYAN}└─{C_RESET}");
}

// ── Display: Commit ──

fn display_commit(obj: &GitObject, sha: &str, show_stat: bool) {
    let commit = match parse_commit(&obj.data) {
        Some(c) => c,
        None => {
            eprintln!("ccat: error: failed to parse commit object");
            return;
        }
    };

    println!(
        "{}\n{} {}\n{}\n",
        c(C_CYAN, "┌─ commit"),
        c(C_BOLD, sha),
        c(C_DIM, "•"),
        c(C_DIM, format!("{} parents", commit.parents.len()))
    );

    // Author line
    let author_name = commit.author.split('<').next().unwrap_or("").trim();
    let author_email = commit.author.split('<').nth(1).and_then(|s| s.split('>').next()).unwrap_or("");
    let author_time = parse_author_timestamp(&commit.author);
    let relative = author_time.map(format_relative_time).unwrap_or_default();

    println!(
        "{}{} {}{}  {}  {}",
        c(C_DIM, "Author:"),
        c(C_GREEN, author_name),
        c(C_DIM, "<"),
        c(C_DIM, author_email),
        c(C_DIM, ">"),
        c(C_DIM, relative)
    );

    // Committer line
    let committer_name = commit.committer.split('<').next().unwrap_or("").trim();
    let committer_email = commit.committer.split('<').nth(1).and_then(|s| s.split('>').next()).unwrap_or("");

    println!(
        "{}{} {}{}  {}",
        c(C_DIM, "Commit:"),
        c(C_GREEN, committer_name),
        c(C_DIM, "<"),
        c(C_DIM, committer_email),
        c(C_DIM, ">")
    );
    println!();

    // Parents
    if !commit.parents.is_empty() {
        println!("{}parents:", C_DIM);
        for (pi, parent) in commit.parents.iter().enumerate() {
            let conn = if pi == commit.parents.len() - 1 { "└── " } else { "├── " };
            println!("  {} {}", c(C_DIM, conn), c(C_BLUE, truncate_sha(parent)));
        }
        println!();
    }

    // Tree
    println!("{}tree: {}", C_DIM, c(C_BLUE, truncate_sha(&commit.tree_sha)));
    println!();

    // Subject line
    let subject = commit.message.lines().next().unwrap_or("");
    println!("{} {}", c(C_BOLD, subject), c(C_DIM, truncate_sha(sha)));
    println!();

    // Body
    let body_lines: Vec<&str> = commit.message.lines().skip(1).collect();
    if !body_lines.is_empty() {
        for line in &body_lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                println!();
            } else {
                println!("  {}", trimmed);
            }
        }
    }

    if show_stat {
        println!();
        println!("{}── diff stat ──{}", C_DIM, C_RESET);
        println!("{}Run: git show --stat {}{}", C_DIM, sha, C_RESET);
    }

    println!("{C_CYAN}└─{C_RESET}");
}

fn parse_author_timestamp(field: &str) -> Option<i64> {
    // Format: "Name <email> timestamp timezone"
    let parts: Vec<&str> = field.split_whitespace().collect();
    if parts.len() >= 3 {
        parts[parts.len() - 2].parse().ok()
    } else {
        None
    }
}

// ── Display: Tag ──

fn display_tag(obj: &GitObject, sha: &str) {
    let text = match std::str::from_utf8(&obj.data) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("ccat: error: tag content is not valid UTF-8");
            return;
        }
    };

    println!(
        "{}\n{} {}\n",
        c(C_CYAN, "┌─ tag"),
        c(C_BOLD, sha),
        c(C_DIM, "annotated tag")
    );

    let mut tag_name = String::new();
    let mut tagged_sha = String::new();
    let mut tagger = String::new();
    let mut message = String::new();

    let mut in_headers = true;
    for line in text.lines() {
        if in_headers {
            if line.is_empty() {
                in_headers = false;
                continue;
            }
            if let Some(rest) = line.strip_prefix("object ") {
                tagged_sha = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("type ") {
                // blob, tree, commit, tag
            } else if let Some(rest) = line.strip_prefix("tag ") {
                tag_name = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("tagger ") {
                tagger = rest.to_string();
            }
        } else {
            message.push_str(line);
            message.push('\n');
        }
    }

    println!("{}object: {}", c(C_DIM, "object"), c(C_GREEN, truncate_sha(&tagged_sha)));

    if !tag_name.is_empty() {
        println!("{}tag: {}", c(C_DIM, "tag"), c(C_YELLOW, &tag_name));
    }

    if !tagger.is_empty() {
        let tagger_name = tagger.split('<').next().unwrap_or("").trim();
        println!(
            "{}{} {}",
            c(C_DIM, "tagger"),
            c(C_GREEN, tagger_name),
            c(C_DIM, tagger.split('<').nth(1).and_then(|s| s.split('>').next()).unwrap_or(""))
        );
    }

    let message = message.trim();
    if !message.is_empty() {
        println!();
        for line in message.lines().take(5) {
            println!("  {}", line);
        }
        if message.lines().count() > 5 {
            println!("  …");
        }
    }

    println!("{C_CYAN}└─{C_RESET}");
}

// ── Display: Blob ──

fn display_blob(obj: &GitObject, sha: &str) {
    println!(
        "{}{}",
        c(C_CYAN, "┌─ blob"),
        c(C_DIM, format!(" {sha}  {}\n", format_size(obj.data.len())))
    );

    let is_text = obj.data.iter().all(|&b| b == b'\n' || (b >= 0x20 && b < 0x7f) || b == 0x09);

    if is_text {
        let text = String::from_utf8_lossy(&obj.data);
        print!("{text}");
    } else {
        println!("{}(binary blob, {} bytes){}", C_DIM, obj.data.len(), C_RESET);
        let preview_len = obj.data.len().min(256);
        println!("{}", c(C_DIM, format!("First {} bytes (hex preview):", preview_len)));
        println!();

        for offset in (0..preview_len).step_by(16) {
            let end = (offset + 16).min(preview_len);
            let chunk = &obj.data[offset..end];

            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            let hex_str = hex.chunks(4).map(|c| c.join(" ")).collect::<Vec<_>>().join("  ");

            let ascii: String = chunk.iter()
                .map(|&b| if b >= 0x20 && b < 0x7f { b as char } else { '.' })
                .collect();

            print!("{} ", format!("{offset:08x}"));
            print!("{hex_str:<48}");
            println!(" │{}│", ascii);
        }

        if obj.data.len() > preview_len {
            println!(
                "{}… ({:.1} more bytes) {}",
                C_DIM,
                (obj.data.len() - preview_len) as f64 / 1024.0,
                C_RESET
            );
        }
    }

    println!("{C_CYAN}└─{C_RESET}");
}

// ── Display: Git log ──

fn display_git_log(git_dir: &Path, max_commits: usize) {
    let head_sha = match resolve_ref(git_dir, "HEAD") {
        Some(s) => s,
        None => {
            eprintln!("ccat: unable to resolve HEAD");
            return;
        }
    };

    let mut visited = HashSet::new();
    let mut commits = Vec::new();
    let mut stack = vec![head_sha];

    while let Some(sha) = stack.pop() {
        if visited.contains(&sha) || commits.len() >= max_commits {
            break;
        }
        visited.insert(sha.clone());

        if let Some((obj, resolved_sha)) = read_git_object(git_dir, &sha) {
            match obj.obj_type {
                GitObjType::Commit => {
                    if let Some(commit) = parse_commit(&obj.data) {
                        let subject: String = commit.message.lines().next().map(|s| s.to_string()).unwrap_or_default();
                        let author_name: String = commit.author.split('<').next().unwrap_or("").trim().to_string();
                        let ts = parse_author_timestamp(&commit.author);
                        let rel = ts.map(format_relative_time).unwrap_or_default();

                        commits.push((resolved_sha, subject, author_name, rel));
                        for parent in commit.parents {
                            stack.push(parent);
                        }
                    }
                }
                GitObjType::Tag => {
                    // Resolve tag to its object
                    if let Ok(text) = std::str::from_utf8(&obj.data) {
                        for line in text.lines() {
                            if let Some(target_sha) = line.strip_prefix("object ") {
                                stack.push(target_sha.trim().to_string());
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if commits.is_empty() {
        eprintln!("ccat: no commits found in this repo");
        return;
    }

    println!("{}", c(C_BOLD, "┌─ git log\n"));

    for (i, (sha, subject, author, _rel)) in commits.iter().enumerate() {
        let is_last = i == commits.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };

        println!(
            "{} {}  {}  {}",
            c(C_DIM, connector),
            c(C_DIM, truncate_sha(sha)),
            c(C_GREEN, author),
            subject
        );

        if i < commits.len() - 1 {
            println!("{}", C_DIM);
        }
    }

    println!("{C_CYAN}└─{C_RESET} {}", c(C_DIM, format!("{} commits shown", commits.len())));
}

// ── Display: Generic git object ──

fn display_git_object(obj: &GitObject, sha: &str, show_stat: bool) {
    match obj.obj_type {
        GitObjType::Tree => display_tree(obj, sha),
        GitObjType::Commit => display_commit(obj, sha, show_stat),
        GitObjType::Tag => display_tag(obj, sha),
        GitObjType::Blob => display_blob(obj, sha),
    }
}

// ── Main entry point ──

/// Entry point for --git mode
pub fn cat_git(opts: &GitOpts) {
    let input = &opts.input;
    let path = Path::new(input);

    // Try to find a git directory
    let git_dir = match find_git_dir(path) {
        Some(d) => d,
        None => {
            // Maybe the input IS a git object file (compressed)
            if path.exists() {
                if let Some(obj) = read_object_from_file(path) {
                    display_git_object(&obj, "stdin", opts.show_stat);
                    return;
                }
            }
            eprintln!("ccat: '{}' is not a git repository", input);
            eprintln!("ccat: pass a git SHA, branch name, tag, or a path inside a git repo");
            return;
        }
    };

    match &opts.mode {
        GitMode::Auto => {
            // Check if input looks like a SHA or ref
            if input.len() == 40 && input.chars().all(|c| c.is_ascii_hexdigit()) {
                // Full SHA - try to read as git object
                if let Some((obj, sha)) = read_git_object(&git_dir, input) {
                    display_git_object(&obj, &sha, opts.show_stat);
                } else {
                    // Try packed
                    eprintln!("ccat: object '{}' not found (may be in packfile)", input);
                }
            } else if input.len() >= 4 && input.chars().all(|c| c.is_ascii_hexdigit()) {
                // Short SHA
                if let Some((obj, sha)) = read_git_object(&git_dir, input) {
                    display_git_object(&obj, &sha, opts.show_stat);
                } else {
                    eprintln!("ccat: ambiguous or missing object '{}'", input);
                }
            } else if let Some(sha) = resolve_ref(&git_dir, input) {
                // It's a ref name (branch, tag, HEAD)
                if let Some((obj, resolved_sha)) = read_git_object(&git_dir, input) {
                    display_git_object(&obj, &resolved_sha, opts.show_stat);
                } else {
                    eprintln!("ccat: object '{}' not found", input);
                }
            } else {
                // Assume it's a repo path, show log
                display_git_log(&git_dir, 20);
            }
        }
        GitMode::Log => {
            display_git_log(&git_dir, 20);
        }
        GitMode::Tree(tree_path) => {
            // Show tree for a specific path
            let head_sha = match resolve_ref(&git_dir, "HEAD") {
                Some(s) => s,
                None => {
                    eprintln!("ccat: unable to resolve HEAD");
                    return;
                }
            };

            if let Some(tree_sha) = resolve_tree_sha(&git_dir, &head_sha) {
                let path_parts: Vec<&str> = tree_path.split('/').collect();
                display_tree_for_path(&git_dir, &tree_sha, &path_parts, "");
            }
        }
        GitMode::Cat => {
            // Show blob content for a SHA/ref
            if let Some((obj, sha)) = read_git_object(&git_dir, input) {
                display_blob(&obj, &sha);
            } else {
                eprintln!("ccat: unable to read object '{}'", input);
            }
        }
    }
}

fn read_object_from_file(path: &Path) -> Option<GitObject> {
    let raw = fs::read(path).ok()?;
    let mut decoder = GzDecoder::new(&raw[..]);
    let mut content = Vec::new();
    if decoder.read_to_end(&mut content).is_err() {
        return None;
    }

    let null_pos = content.iter().position(|&b| b == b'\0')?;
    let header = std::str::from_utf8(&content[..null_pos]).ok()?;
    let content_data = content[null_pos + 1..].to_vec();

    let parts: Vec<&str> = header.split_whitespace().collect();
    if parts.len() != 2 {
        return None;
    }

    let obj_type = GitObjType::from_str(parts[0])?;
    Some(GitObject { obj_type, data: content_data })
}

fn resolve_tree_sha(git_dir: &Path, commit_sha: &str) -> Option<String> {
    if let Some((obj, _)) = read_git_object(git_dir, commit_sha) {
        if obj.obj_type == GitObjType::Commit {
            if let Ok(text) = std::str::from_utf8(&obj.data) {
                for line in text.lines() {
                    if let Some(sha) = line.strip_prefix("tree ") {
                        return Some(sha.trim().to_string());
                    }
                }
            }
        }
    }
    None
}

fn display_tree_for_path(
    git_dir: &Path,
    tree_sha: &str,
    path_segments: &[&str],
    current_path: &str,
) {
    if let Some((obj, obj_sha)) = read_git_object(git_dir, tree_sha) {
        if obj.obj_type == GitObjType::Tree {
            if let Some(entries) = parse_tree(&obj.data) {
                if path_segments.is_empty() {
                    // Show entire tree
                    println!(
                        "{}{} (tree)\n",
                        c(C_CYAN, "┌─ "),
                        c(C_DIM, obj_sha)
                    );
                    for entry in &entries {
                        display_tree_entry(git_dir, entry, 0);
                    }
                    println!("{C_CYAN}└─{C_RESET}");
                } else {
                    // Navigate to the specific path
                    let next_seg = path_segments[0];
                    for entry in &entries {
                        if entry.name == next_seg {
                            let new_path = if current_path.is_empty() {
                                next_seg.to_string()
                            } else {
                                format!("{current_path}/{next_seg}")
                            };

                            match entry.mode.as_str() {
                                "040000" => {
                                    // Directory - recurse
                                    let child_sha = oid_to_sha(&entry.oid);
                                    if path_segments.len() > 1 {
                                        display_tree_for_path(
                                            git_dir,
                                            &child_sha,
                                            &path_segments[1..],
                                            &new_path,
                                        );
                                    } else {
                                        println!("{} (directory)", c(C_BLUE, &new_path));
                                        display_tree_for_path(git_dir, &child_sha, &[], "");
                                    }
                                }
                                _ => {
                                    // File
                                    println!("{} ({})", c(C_WHITE, &new_path), entry.mode);
                                    let child_sha = oid_to_sha(&entry.oid);
                                    if let Some((blob_obj, _)) = read_git_object(git_dir, &child_sha) {
                                        if blob_obj.obj_type == GitObjType::Blob {
                                            println!("{}", c(C_DIM, format!("{} bytes", blob_obj.data.len())));
                                            let is_text = blob_obj.data.iter().all(|&b| b == b'\n' || (b >= 0x20 && b < 0x7f) || b == 0x09);
                                            if is_text {
                                                let text = String::from_utf8_lossy(&blob_obj.data);
                                                for line in text.lines().take(20) {
                                                    println!("  {}", line);
                                                }
                                            } else {
                                                println!("  {}", c(C_DIM, "(binary content)"));
                                            }
                                        }
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
}

fn display_tree_entry(git_dir: &Path, entry: &TreeEntry, depth: usize) {
    let indent = "  ".repeat(depth);
    let connector = "├── ";

    match entry.mode.as_str() {
        "040000" => {
            // Directory
            println!(
                "{}{}{}",
                c(C_DIM, format!("{indent}{connector}")),
                c(C_BLUE, "📁 "),
                c(C_BLUE, &entry.name)
            );
            let child_sha = oid_to_sha(&entry.oid);
            if let Some((child_obj, _)) = read_git_object(git_dir, &child_sha) {
                if child_obj.obj_type == GitObjType::Tree {
                    if let Some(entries) = parse_tree(&child_obj.data) {
                        for child in &entries {
                            display_tree_entry(git_dir, child, depth + 1);
                        }
                    }
                }
            }
        }
        _ => {
            let mut icon = "";
            if entry.mode.starts_with("12") {
                icon = "🔗 ";
            } else if entry.mode.starts_with("10") {
                if u16::from_str_radix(&entry.mode, 8).unwrap_or(0) & 0o111 != 0 {
                    icon = "💥 ";
                }
            }
            println!(
                "{}{}{}{}",
                c(C_DIM, format!("{indent}{connector}")),
                icon,
                c(C_WHITE, &entry.name),
                c(C_DIM, format!(" ({})", entry.mode))
            );
        }
    }
}

/// Convert a 20-byte binary OID to a hex SHA string
fn oid_to_sha(oid: &[u8; 20]) -> String {
    oid.iter().map(|b| format!("{b:02x}")).collect()
}
