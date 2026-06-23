//! Git churn analysis (`ccat --churn`).
//!
//! Shows which files change most frequently in git history — the
//! "hot spots" of a codebase. High-churn files are where bugs are
//! most likely to be introduced and where refactoring pays off most.
//!
//! Usage:
//!   ccat --churn                   # top 10 most-churned files (default)
//!   ccat --churn --churn-top 20   # top 20
//!   ccat --churn --churn-all      # all files
//!   ccat --churn --churn-since "2025-01-01"  # since a date
//!
//! Data comes from `git log --numstat`, which shows per-file
//! added/deleted line counts for every commit.

use std::collections::HashMap;
use std::process::Command;

// ── Colour helpers ──

mod style {
    pub fn bold(s: &str) -> String  { format!("\x1b[1m{s}\x1b[0m") }
    pub fn red(s: &str) -> String   { format!("\x1b[31m{s}\x1b[0m") }
    pub fn green(s: &str) -> String { format!("\x1b[32m{s}\x1b[0m") }
    pub fn yellow(s: &str) -> String { format!("\x1b[33m{s}\x1b[0m") }
    pub fn dim(s: &str) -> String   { format!("\x1b[2m{s}\x1b[0m") }
    pub fn white(s: &str) -> String { format!("\x1b[37m{s}\x1b[0m") }
    pub fn reset() -> &'static str  { "\x1b[0m" }
}

// ── Churn data ──

#[derive(Debug, Clone)]
pub struct FileChurn {
    pub file: String,
    pub commits: usize,
    pub added: u64,
    pub deleted: u64,
    /// Total modified lines = added + deleted
    pub total_lines: u64,
    /// Normalised churn score (0.0–1.0, for colour coding)
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct ChurnOptions {
    /// Paths to analyse (default: ["."])
    pub paths: Vec<String>,
    /// Max files to show (default: 10). 0 = all.
    pub top_n: usize,
    /// Git since filter (e.g. "2025-01-01", "1 year ago")
    pub since: Option<String>,
    /// Git until filter
    pub until: Option<String>,
    /// Show files with zero churn too (all files ever touched)
    pub show_all: bool,
    /// Hide the summary header
    pub no_header: bool,
    /// Filter by path prefix (only files matching this path)
    pub path_filter: Option<String>,
}

impl Default for ChurnOptions {
    fn default() -> Self {
        Self {
            paths: vec![".".to_string()],
            top_n: 10,
            since: None,
            until: None,
            show_all: false,
            no_header: false,
            path_filter: None,
        }
    }
}

// ── Git log parsing ──

/// Run `git log --numstat` and parse per-file churn data.
fn collect_churn(opts: &ChurnOptions) -> Result<Vec<FileChurn>, String> {
    let cwd = &opts.paths[0];

    // Ensure we're in a git repo
    let check = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;

    if !check.status.success() {
        return Err("not a git repository".into());
    }

    // Build the git log command
    let mut cmd = Command::new("git");
    cmd.args(["log", "--numstat", "--pretty=format:|||%H|||%an|||%ai|||%s|||"]);
    cmd.current_dir(cwd);

    if let Some(ref since) = opts.since {
        cmd.args(["--since", since]);
    }
    if let Some(ref until) = opts.until {
        cmd.args(["--until", until]);
    }

    let output = cmd
        .output()
        .map_err(|e| format!("failed to run git log: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git log failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse: for each commit block, process the --numstat lines
    // Format:
    //   |||<sha>|||<author>|||<date>|||<subject>|||
    //   <added>\t<deleted>\t<file>
    //   <added>\t<deleted>\t<file>

    let mut stats: HashMap<String, (usize, u64, u64)> = HashMap::new();

    for line in stdout.lines() {
        // Skip commit header lines (start with |||)
        if line.starts_with("|||") {
            continue;
        }
        // Skip blank lines
        if line.trim().is_empty() {
            continue;
        }

        // Parse numstat line: added\tdeleted\tfile
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 {
            continue;
        }

        let added = parts[0].parse::<u64>().unwrap_or(0);
        let deleted = parts[1].parse::<u64>().unwrap_or(0);
        let file = parts[2].to_string();

        // Apply path filter
        if let Some(ref filter) = opts.path_filter {
            if !file.starts_with(filter) {
                continue;
            }
        }

        let entry = stats.entry(file).or_insert((0, 0, 0));
        entry.0 += 1;       // commit count
        entry.1 += added;   // added lines
        entry.2 += deleted; // deleted lines
    }

    if stats.is_empty() {
        return Err("no churn data found (empty git history?)".into());
    }

    // Find max churn for normalisation
    let max_churn = stats
        .values()
        .map(|(c, a, d)| *c as f64 + (*a + *d) as f64 * 0.1)
        .fold(0.0, f64::max);

    // Convert to sorted Vec
    let mut result: Vec<FileChurn> = stats
        .into_iter()
        .map(|(file, (commits, added, deleted))| {
            let total_lines = added + deleted;
            let raw_score = commits as f64 + total_lines as f64 * 0.1;
            let score = if max_churn > 0.0 {
                (raw_score / max_churn).min(1.0)
            } else {
                0.0
            };
            FileChurn { file, commits, added, deleted, total_lines, score }
        })
        .collect();

    // Sort by churn score descending
    result.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(result)
}

// ── Display ──

/// Display churn analysis results.
pub fn display_churn(all: &[FileChurn], opts: &ChurnOptions) {
    if all.is_empty() {
        println!("{}", style::dim("No churn data found."));
        return;
    }

    let display_count = if opts.show_all {
        all.len()
    } else {
        opts.top_n.min(all.len())
    };

    // ── Header ──
    if !opts.no_header {
        println!("{}", style::bold("🔥 Git Churn Analysis"));
        println!("{}", style::dim(&format!(
            "   {} files changed · showing top {} (--churn-all for all)\n",
            all.len(),
            display_count,
        )));
    }

    // ── Determine column widths ──
    let max_commits = all
        .iter()
        .take(display_count)
        .map(|f| f.commits)
        .max()
        .unwrap_or(0);
    let max_lines = all
        .iter()
        .take(display_count)
        .map(|f| f.total_lines as usize)
        .max()
        .unwrap_or(0);
    let cw = max_commits.to_string().len().max(8);
    let lw = max_lines.to_string().len().max(8);

    // ── Column headers ──
    println!(
        "  {:>w1$}  {:>w2$}  {}  {}",
        style::dim("COMMITS"),
        style::dim("LINES"),
        style::dim(" ±CHURN "),
        style::dim("FILE"),
        w1 = cw, w2 = lw,
    );

    let sep = |w: usize| style::dim(&"─".repeat(w));
    println!(
        "  {}  {}  {}  {}",
        sep(cw), sep(lw),
        style::dim("───────"),
        style::dim("──────────────────────────────────"),
    );

    // ── Files ──
    for fc in &all[..display_count] {
        let churn_color: fn(&str) -> String = if fc.score >= 0.8 {
            style::red
        } else if fc.score >= 0.5 {
            style::yellow
        } else {
            |s| style::dim(s)
        };

        let bar_len = (fc.score * 20.0).round() as usize;
        let bar = "▓".repeat(bar_len);

        println!(
            "  {:>w1$}  {:>w2$}  {:>4.0}%  {}{}",
            fc.commits,
            fc.total_lines,
            fc.score * 100.0,
            churn_color(&bar),
            churn_color(&fc.file),
            w1 = cw, w2 = lw,
        );
    }

    // ── Summary ──
    if !opts.no_header && all.len() > display_count {
        println!();
        println!("  {}  ({} more files — use --churn-all)",
            style::dim("…"),
            all.len() - display_count,
        );
    }

    if !opts.no_header {
        println!();
        let total_commits: usize = all.iter().map(|f| f.commits).sum();
        let total_added: u64 = all.iter().map(|f| f.added).sum();
        let total_deleted: u64 = all.iter().map(|f| f.deleted).sum();
        println!(
            "  {} {} commits · {} added · {} deleted across {} files",
            style::dim("∑"),
            total_commits,
            style::green(&format!("+{}", total_added)),
            style::red(&format!("-{}", total_deleted)),
            all.len(),
        );
    }
}

// ── Public entry point ──

/// Run churn analysis and display results.
pub fn cat_churn(opts: &ChurnOptions) {
    match collect_churn(opts) {
        Ok(churn_data) => display_churn(&churn_data, opts),
        Err(e) => {
            if !opts.no_header {
                eprintln!("ccat: --churn: {e}");
            }
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// Helper: create a temp git repo with some history
    fn setup_git_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        // Init repo
        Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .unwrap();

        // Create initial file
        let mut f = std::fs::File::create(path.join("a.rs")).unwrap();
        writeln!(f, "fn main() {{}}").unwrap();
        drop(f);

        Command::new("git")
            .args(["add", "a.rs"])
            .current_dir(path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(path)
            .output()
            .unwrap();

        // Create and modify another file
        let mut f = std::fs::File::create(path.join("b.rs")).unwrap();
        writeln!(f, "fn hello() {{}}").unwrap();
        drop(f);

        Command::new("git")
            .args(["add", "b.rs"])
            .current_dir(path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["commit", "-m", "add b"])
            .current_dir(path)
            .output()
            .unwrap();

        // Modify b.rs again
        let mut f = std::fs::File::create(path.join("b.rs")).unwrap();
        writeln!(f, "fn hello() {{}}\nfn world() {{}}").unwrap();
        drop(f);

        Command::new("git")
            .args(["add", "b.rs"])
            .current_dir(path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["commit", "-m", "extend b"])
            .current_dir(path)
            .output()
            .unwrap();

        dir
    }

    #[test]
    fn test_churn_collects_data() {
        let dir = setup_git_repo();
        let opts = ChurnOptions {
            paths: vec![dir.path().to_string_lossy().to_string()],
            ..Default::default()
        };
        let result = collect_churn(&opts).unwrap();
        assert!(!result.is_empty(), "should have churn data");

        // b.rs should have 2 commits (higher churn than a.rs)
        let b = result.iter().find(|f| f.file == "b.rs").unwrap();
        assert_eq!(b.commits, 2, "b.rs should have 2 commits");
        assert!(b.total_lines > 0, "b.rs should have modified lines");

        let a = result.iter().find(|f| f.file == "a.rs").unwrap();
        assert_eq!(a.commits, 1, "a.rs should have 1 commit");
    }

    #[test]
    fn test_churn_sorts_by_score() {
        let dir = setup_git_repo();
        let opts = ChurnOptions {
            paths: vec![dir.path().to_string_lossy().to_string()],
            ..Default::default()
        };
        let result = collect_churn(&opts).unwrap();

        // b.rs should be first (higher churn than a.rs)
        assert_eq!(result[0].file, "b.rs", "b.rs has higher churn");
    }

    #[test]
    fn test_churn_top_n() {
        let dir = setup_git_repo();
        let result = collect_churn(&ChurnOptions {
            paths: vec![dir.path().to_string_lossy().to_string()],
            top_n: 1,
            ..Default::default()
        })
        .unwrap();

        assert_eq!(
            result.len(),
            2,
            "collect_churn returns all data regardless of top_n"
        );
        // top_n is applied at display time only
    }

    #[test]
    fn test_churn_not_git_repo() {
        let dir = TempDir::new().unwrap();
        let opts = ChurnOptions {
            paths: vec![dir.path().to_string_lossy().to_string()],
            ..Default::default()
        };
        let result = collect_churn(&opts);
        assert!(result.is_err(), "should fail on non-git dir");
    }
}
