use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single git blame entry for one line.
#[derive(Debug, Clone)]
pub(crate) struct BlameEntry {
    /// Line number in the file (1-based)
    pub line: usize,
    /// Full commit hash (40 hex chars), or "0000000000000000000000000000000000000000" for uncommitted
    pub commit: String,
    /// Author name
    pub author: String,
    /// Unix timestamp of author time
    pub author_time: u64,
    /// Commit summary (first line of message) — stored for potential future use
    #[allow(dead_code)]
    pub summary: String,
}

/// Run `git blame --porcelain` on a file and parse the output.
/// Returns a Vec of BlameEntry, one per line in the file, indexed by line number.
pub(crate) fn run_git_blame(path: &str) -> Result<Vec<BlameEntry>, String> {
    let output = Command::new("git")
        .args(["blame", "--porcelain", path])
        .output()
        .map_err(|e| format!("failed to run git blame: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Not a git repo, or file not tracked
        if stderr.contains("fatal: not a git repository")
            || stderr.contains("fatal: no such path")
        {
            return Err(format!("not a git repository or file not tracked"));
        }
        return Err(format!("git blame failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_porcelain(&stdout)
}

/// Parse git blame --porcelain output.
///
/// Format:
///   <hash> <orig-line> <final-line>
///   author <name>
///   author-mail <mail>
///   author-time <timestamp>
///   author-tz <tz>
///   committer <name>
///   committer-mail <mail>
///   committer-time <timestamp>
///   committer-tz <tz>
///   summary <message>
///   filename <name>
///   \t<line content>
///
/// For subsequent lines of the same commit, only the header line and \t<content> are emitted.
fn parse_porcelain(output: &str) -> Result<Vec<BlameEntry>, String> {
    let mut entries: Vec<BlameEntry> = Vec::new();
    let mut lines = output.lines().peekable();

    let mut current_commit = String::new();
    let mut current_author = String::new();
    let mut current_time: u64 = 0;
    let mut current_summary = String::new();

    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }

        // If line starts with tab, it's a content line — skip
        if line.starts_with('\t') {
            continue;
        }

        // Parse header: <hash> <orig-line> <final-line>
        let parts: Vec<&str> = line.splitn(3, ' ').collect();
        if parts.len() < 3 {
            continue;
        }

        let hash = parts[0];
        let final_line: usize = parts[2].parse().map_err(|_| {
            format!("invalid line number in git blame output: {}", parts[2])
        })?;

        if hash.len() < 8 {
            continue;
        }

        current_commit = hash.to_string();

        // Parse metadata lines until we hit the next header or content line
        loop {
            let peeked = match lines.peek() {
                Some(p) => *p,
                None => break,
            };

            if peeked.starts_with('\t') {
                // Content line — advance past it
                lines.next();
                break;
            }

            // Metadata line
            let meta_line = lines.next().unwrap_or("");
            if meta_line.starts_with("author ") {
                current_author = meta_line[7..].to_string();
            } else if meta_line.starts_with("author-time ") {
                current_time = meta_line[12..].parse().unwrap_or(0);
            } else if meta_line.starts_with("summary ") {
                current_summary = meta_line[8..].to_string();
            }
            // Skip other metadata (author-mail, author-tz, committer, etc.)
        }

        entries.push(BlameEntry {
            line: final_line,
            commit: current_commit.clone(),
            author: current_author.clone(),
            author_time: current_time,
            summary: current_summary.clone(),
        });
    }

    Ok(entries)
}

/// Format a relative time string from a Unix timestamp.
fn relative_time(timestamp: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let diff = now.saturating_sub(timestamp);

    if diff < 60 {
        format!("{}s", diff)
    } else if diff < 3600 {
        format!("{}m", diff / 60)
    } else if diff < 86400 {
        format!("{}h", diff / 3600)
    } else if diff < 604800 {
        format!("{}d", diff / 86400)
    } else if diff < 2592000 {
        format!("{}w", diff / 604800)
    } else if diff < 31536000 {
        format!("{}mo", diff / 2592000)
    } else {
        format!("{}y", diff / 31536000)
    }
}

/// ANSI colors for the blame margin
const COLOR_RECENT: &str = "\x1b[38;2;255;80;80m";    // <1d: bright red
const COLOR_WEEK: &str = "\x1b[38;2;255;200;50m";     // <1w: orange-yellow
const COLOR_MONTH: &str = "\x1b[38;2;80;160;255m";    // <1m: blue
const COLOR_QUARTER: &str = "\x1b[38;2;80;200;120m";  // <3mo: green
const COLOR_OLD: &str = "\x1b[38;2;100;100;100m";     // >=3mo: dim gray
const COLOR_UNCOMMITTED: &str = "\x1b[38;2;255;50;50m";
const COLOR_MARGIN_LINE: &str = "\x1b[38;2;80;80;90m"; // the │ separator
const RESET: &str = "\x1b[0m";

/// Choose a color for the commit hash based on how recent it is.
fn age_color(timestamp: u64) -> &'static str {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let diff = now.saturating_sub(timestamp);
    if diff < 86400 {
        COLOR_RECENT       // < 1 day
    } else if diff < 604800 {
        COLOR_WEEK         // < 1 week
    } else if diff < 2592000 {
        COLOR_MONTH        // < 1 month
    } else if diff < 7776000 {
        COLOR_QUARTER      // < 3 months
    } else {
        COLOR_OLD          // >= 3 months
    }
}

/// Build a blame margin for a given line entry.
/// Format: `abc1234  author  │ `
/// Where hash is 7 chars and author is truncated to 10.
pub(crate) fn format_blame_margin(entry: Option<&BlameEntry>, max_author_width: usize) -> String {
    match entry {
        Some(e) => {
            let is_uncommitted = e.commit.starts_with("0000000");
            if is_uncommitted {
                format!(
                    "{color}--------  {RESET}{margin}│{RESET} ",
                    color = COLOR_UNCOMMITTED,
                    margin = COLOR_MARGIN_LINE,
                    RESET = RESET,
                )
            } else {
                let short_hash = &e.commit[..7.min(e.commit.len())];
                let author = truncate_author(&e.author, max_author_width);
                let time_str = relative_time(e.author_time);

                // Show timestamp only in a compact way: no time shown for simplicity
                // We color the hash by age
                format!(
                    "{color}{hash}{RESET} {author_pad}{margin}│{RESET} ",
                    color = age_color(e.author_time),
                    hash = short_hash,
                    RESET = RESET,
                    author_pad = format!("{:<width$}", author, width = max_author_width),
                    margin = COLOR_MARGIN_LINE,
                )
            }
        }
        None => {
            // Line outside blame range (shouldn't happen, but be safe)
            format!(
                "{color}.......  {RESET}{margin}│{RESET} ",
                color = COLOR_OLD,
                margin = COLOR_MARGIN_LINE,
                RESET = RESET,
            )
        }
    }
}

/// Truncate or pad an author name to the target width.
fn truncate_author(name: &str, width: usize) -> String {
    if name.len() <= width {
        format!("{:<width$}", name)
    } else {
        // Truncate and add ellipsis
        if width >= 3 {
            format!("{}..", &name[..width.saturating_sub(2)])
        } else {
            name[..width].to_string()
        }
    }
}

/// Compute the maximum author width across all entries.
pub(crate) fn max_author_width(entries: &[BlameEntry]) -> usize {
    entries
        .iter()
        .map(|e| e.author.len())
        .max()
        .unwrap_or(0)
        .min(14) // cap at 14 chars
        .max(8)  // minimum 8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_porcelain_single_line() {
        let output = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9 1 1
author Alice
author-mail <alice@example.com>
author-time 1700000000
author-tz +0800
committer Alice
committer-mail <alice@example.com>
committer-time 1700000000
committer-tz +0800
summary Initial commit
filename test.rs
\tfn main() {
";
        let entries = parse_porcelain(output).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].line, 1);
        assert_eq!(entries[0].commit, "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9");
        assert_eq!(entries[0].author, "Alice");
        assert_eq!(entries[0].author_time, 1700000000);
    }

    #[test]
    fn test_parse_porcelain_multi_line() {
        let output = "abc1234def5678abc1234def5678abc1234def5678 1 1
author Bob
author-mail <bob@example.com>
author-time 1700000000
author-tz +0000
committer Bob
committer-mail <bob@example.com>
committer-time 1700000000
committer-tz +0000
summary Add feature
filename test.rs
\tline 1
abc1234def5678abc1234def5678abc1234def5678 2 2
\tline 2
deadbeefcafebabedeadbeefcafebabedeadbeef 3 3
author Charlie
author-mail <charlie@example.com>
author-time 1700100000
author-tz +0000
committer Charlie
committer-mail <charlie@example.com>
committer-time 1700100000
committer-tz +0000
summary Fix bug
filename test.rs
\tline 3
";
        let entries = parse_porcelain(output).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].author, "Bob");
        assert_eq!(entries[1].author, "Bob");
        assert_eq!(entries[1].line, 2);
        assert_eq!(entries[2].author, "Charlie");
        assert_eq!(entries[2].line, 3);
    }

    #[test]
    fn test_parse_porcelain_uncommitted() {
        let output = "0000000000000000000000000000000000000000 1 1
author Not Committed Yet
author-mail <not.committed.yet>
author-time 0
author-tz +0000
committer Not Committed Yet
committer-mail <not.committed.yet>
committer-time 0
committer-tz +0000
summary Not committed yet
filename test.rs
\tuncommitted line
";
        let entries = parse_porcelain(output).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].commit.starts_with("0000000"));
    }

    #[test]
    fn test_truncate_author_short() {
        assert_eq!(truncate_author("Bob", 10), "Bob       ");
    }

    #[test]
    fn test_truncate_author_exact() {
        assert_eq!(truncate_author("abcdefghij", 10), "abcdefghij");
    }

    #[test]
    fn test_truncate_author_long() {
        let result = truncate_author("Alexander the Great", 10);
        assert_eq!(result.len(), 10);
        assert!(result.ends_with(".."));
    }

    #[test]
    fn test_format_blame_margin_normal() {
        let entry = BlameEntry {
            line: 1,
            commit: "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9".to_string(),
            author: "Alice".to_string(),
            author_time: 1700000000,
            summary: "commit msg".to_string(),
        };
        let margin = format_blame_margin(Some(&entry), 10);
        // Should contain the short hash and author and the │ separator
        assert!(margin.contains("a1b2c3d"));
        assert!(margin.contains("Alice"));
        assert!(margin.contains("│"));
    }

    #[test]
    fn test_format_blame_margin_uncommitted() {
        let entry = BlameEntry {
            line: 1,
            commit: "0000000000000000000000000000000000000000".to_string(),
            author: "Not Committed Yet".to_string(),
            author_time: 0,
            summary: "".to_string(),
        };
        let margin = format_blame_margin(Some(&entry), 10);
        assert!(margin.contains("--------"));
        assert!(margin.contains("│"));
    }

    #[test]
    fn test_max_author_width() {
        let entries = vec![
            BlameEntry {
                line: 1,
                commit: "a".to_string(),
                author: "Alice".to_string(),
                author_time: 0,
                summary: "".to_string(),
            },
            BlameEntry {
                line: 2,
                commit: "b".to_string(),
                author: "Alexandria the Great".to_string(),
                author_time: 0,
                summary: "".to_string(),
            },
        ];
        let w = max_author_width(&entries);
        assert!(w >= 10); // "Alexandria t.." truncated
        assert!(w <= 14);
    }

    #[test]
    fn test_relative_time() {
        // Very old
        let old = relative_time(1000000);
        assert!(old.ends_with('y'));

        // Recent (within seconds)
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let recent = relative_time(now);
        assert!(recent.ends_with('s'));
    }
}
