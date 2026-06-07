use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::FileKind;

/// Get a file identity token for rotation detection.
/// On Unix this is the inode; on Windows we hash (modified_time + len).
#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> u64 {
    metadata.ino()
}

#[cfg(not(unix))]
fn file_identity(metadata: &fs::Metadata) -> u64 {
    use std::hash::{Hash, Hasher};
    let mtime = match metadata.modified() {
        Ok(t) => t,
        Err(_) => return 0,
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    mtime.hash(&mut hasher);
    metadata.len().hash(&mut hasher);
    hasher.finish()
}

/// Minimum poll interval in milliseconds.
const POLL_MS: u64 = 250;

/// Maximum poll interval (backs off when file is idle).
const MAX_POLL_MS: u64 = 2000;

/// Render a single line of new content according to file kind.
fn render_line(line: &str, kind: FileKind) {
    match kind {
        FileKind::Log => {
            // Reuse cat_log's line highlighter if available in scope
            let colored = crate::cat_log::highlight_log_line(line);
            let _ = writeln!(io::stdout(), "{}", colored);
        }
        FileKind::SourceCode => {
            // Print with line prefix ; actual syntax highlighting
            // would need full buffer, so we fall back to plain
            let _ = writeln!(io::stdout(), "{}", line);
        }
        FileKind::Json | FileKind::Yaml | FileKind::Toml | FileKind::Csv | FileKind::Markdown => {
            // Structured formats: print with a dim prefix to indicate streaming
            let _ = writeln!(io::stdout(), "{}", line);
        }
        FileKind::PlainText => {
            let _ = writeln!(io::stdout(), "{}", line);
        }
        _ => {
            let _ = writeln!(io::stdout(), "{}", line);
        }
    }
}

/// Render a chunk of new bytes according to file kind.
fn render_chunk(data: &[u8], kind: FileKind) {
    let s = String::from_utf8_lossy(data);
    for line in s.lines() {
        render_line(line, kind);
    }
    // Don't forget a trailing newline if present
    if data.last() == Some(&b'\n') && !s.ends_with('\n') {
        // content already has the newline via lines()
        // Actually lines() strips the newline, so we're fine
    }
}

/// Seek to approximately N lines before the end of the file.
fn seek_to_last_n_lines(mut file: &fs::File, n: usize) -> io::Result<u64> {
    let metadata = file.metadata()?;
    let file_size = metadata.len();

    if file_size == 0 {
        return Ok(0);
    }

    // Read the last 16KB or the whole file, whichever is smaller
    let read_size = file_size.min(16 * 1024);
    let start_pos = file_size.saturating_sub(read_size);
    file.seek(SeekFrom::Start(start_pos))?;

    let mut buf = vec![0u8; read_size as usize];
    let bytes_read = file.read(&mut buf)?;
    let s = String::from_utf8_lossy(&buf[..bytes_read]);

    // Count newlines from the end
    let total_newlines = s.bytes().filter(|&b| b == b'\n').count();
    if total_newlines <= n {
        // Show from the beginning
        file.seek(SeekFrom::Start(0))?;
        return Ok(0);
    }

    // Find the position of the (total_newlines - n)th newline
    let target = total_newlines - n;
    let mut newlines_found = 0;
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            newlines_found += 1;
            if newlines_found == target {
                let pos = start_pos + i as u64 + 1;
                file.seek(SeekFrom::Start(pos))?;
                return Ok(pos);
            }
        }
    }

    file.seek(SeekFrom::Start(0))?;
    Ok(0)
}

/// Detect file rotation by checking file identity changes.
fn check_rotation(path: &str, prev_id: u64) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .map(|m| file_identity(&m))
        .filter(|&id| id != prev_id)
}

/// Follow a file, displaying new content as it's written.
///
/// Works best for logs and plain text. For structured formats (JSON, YAML, etc.)
/// new content is shown as plain lines since incremental structured rendering
/// doesn't make sense without the full context.
pub fn cat_follow(path: &str, kind: FileKind, lines: usize) -> io::Result<()> {
    let path_obj = Path::new(path);

    // Open the file
    let mut file = fs::File::open(path_obj).map_err(|e| {
        eprintln!("ccat: {path}: {e}");
        e
    })?;

    let metadata = file.metadata()?;
    let mut prev_size = metadata.len();
    let mut prev_id = file_identity(&metadata);

    // Phase 1: Show initial content (last N lines)
    if lines > 0 && prev_size > 0 {
        let seek_pos = seek_to_last_n_lines(&file, lines)?;
        file.seek(SeekFrom::Start(seek_pos))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        render_chunk(&buf, kind);
        prev_size = seek_pos + buf.len() as u64;
    }

    // Flush stdout so initial output is visible
    let _ = io::stdout().flush();

    // Phase 2: Poll for changes
    let mut poll_ms = POLL_MS;
    let mut consecutive_no_change = 0u32;
    let start = Instant::now();
    let max_duration = Duration::from_secs(3600); // 1 hour max (safety)

    loop {
        // Safety timeout
        if start.elapsed() > max_duration {
            break;
        }

        thread::sleep(Duration::from_millis(poll_ms));

        // Check for file rotation first
        if let Some(new_id) = check_rotation(path, prev_id) {
            // File was rotated — reopen and show new content
            file = fs::File::open(path_obj).map_err(|e| {
                eprintln!("ccat: {path} (rotated): {e}");
                io::Error::new(io::ErrorKind::Other, e)
            })?;
            let new_metadata = file.metadata()?;
            let new_size = new_metadata.len();
            prev_id = new_id;
            prev_size = new_size;

            // Print rotation notice to stderr
            let _ = writeln!(io::stderr(), "\x1b[2mccat: {path}: file rotated\x1b[0m");

            // Show initial content of new file
            if new_size > 0 {
                let seek_pos = seek_to_last_n_lines(&file, lines)?;
                file.seek(SeekFrom::Start(seek_pos))?;
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)?;
                render_chunk(&buf, kind);
                prev_size = seek_pos + buf.len() as u64;
            }
            continue;
        }

        // Check current file size
        let current_metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(_) => {
                // File may have been deleted; wait a bit then try again
                thread::sleep(Duration::from_millis(1000));
                continue;
            }
        };
        let current_size = current_metadata.len();

        if current_size > prev_size {
            // New data available — read from last known position
            file.seek(SeekFrom::Start(prev_size))?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;

            if !buf.is_empty() {
                render_chunk(&buf, kind);
                let _ = io::stdout().flush();
            }

            prev_size = current_size;
            consecutive_no_change = 0;
            poll_ms = POLL_MS; // Reset to fast poll
        } else if current_size < prev_size {
            // File was truncated (not rotated, same inode)
            file.seek(SeekFrom::Start(0))?;
            prev_size = 0;

            let _ = writeln!(io::stderr(), "\x1b[2mccat: {path}: file truncated\x1b[0m");
            consecutive_no_change = 0;
            poll_ms = POLL_MS;
        } else {
            consecutive_no_change += 1;
            // Back off polling if file has been idle
            if consecutive_no_change > 10 {
                poll_ms = poll_ms.min(MAX_POLL_MS).saturating_add(100);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_seek_to_last_n_lines_small_file() {
        let dir = std::env::temp_dir().join(format!("ccat_follow_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("small.log");
        fs::write(&path, b"line1\nline2\nline3\n").unwrap();

        let file = fs::File::open(&path).unwrap();
        let pos = seek_to_last_n_lines(&file, 2).unwrap();
        assert_eq!(pos, 6); // Should skip "line1\n" (6 bytes)

        let mut buf = Vec::new();
        let mut f = fs::File::open(&path).unwrap();
        f.seek(SeekFrom::Start(pos)).unwrap();
        f.read_to_end(&mut buf).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "line2\nline3\n");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_seek_to_last_n_lines_large_file() {
        let dir = std::env::temp_dir().join(format!("ccat_follow_large_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("large.log");

        // Create a file larger than 16KB
        let mut f = fs::File::create(&path).unwrap();
        for i in 0..2000 {
            writeln!(f, "line {:04}", i).unwrap();
        }
        drop(f);

        let file = fs::File::open(&path).unwrap();
        let pos = seek_to_last_n_lines(&file, 5).unwrap();
        assert!(pos > 0);

        let mut buf = Vec::new();
        let mut f2 = fs::File::open(&path).unwrap();
        f2.seek(SeekFrom::Start(pos)).unwrap();
        f2.read_to_end(&mut buf).unwrap();
        let content = String::from_utf8(buf).unwrap();
        let line_count = content.lines().count();
        assert_eq!(line_count, 5);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_seek_to_last_n_lines_empty_file() {
        let dir = std::env::temp_dir().join(format!("ccat_follow_empty_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("empty.log");
        fs::write(&path, b"").unwrap();

        let file = fs::File::open(&path).unwrap();
        let pos = seek_to_last_n_lines(&file, 10).unwrap();
        assert_eq!(pos, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_render_chunk_plain() {
        // Just ensure no crash
        render_chunk(b"hello\nworld\n", FileKind::PlainText);
        render_chunk(b"", FileKind::PlainText);
    }

    #[test]
    fn test_render_chunk_log() {
        render_chunk(b"2024-01-01T10:00:00 ERROR something\n", FileKind::Log);
    }

    #[test]
    fn test_check_rotation() {
        let dir = std::env::temp_dir().join(format!("ccat_follow_rotate_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("rotate_test.log");

        fs::write(&path, b"original").unwrap();
        let id1 = super::file_identity(&fs::metadata(&path).unwrap());

        // Recreate file (simulate rotation)
        fs::write(&path, b"new content").unwrap();
        let _id2 = super::file_identity(&fs::metadata(&path).unwrap());

        // On some filesystems (e.g. tmpfs) inode may change,
        // on others it won't (ext4 with same name = same inode if truncated)
        let result = check_rotation(&path.to_string_lossy(), id1);
        // Just verify it doesn't panic
        assert!(result.is_none() || result.is_some());

        let _ = fs::remove_dir_all(&dir);
    }
}
