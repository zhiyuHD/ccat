use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cat_html;
use crate::{detect_kind, FileKind};

/// Maximum directory depth to traverse when serving for security.
/// No one needs to serve files at depth > 32.
const MAX_DEPTH: usize = 32;

// ── Public API ──

/// Start a single-threaded HTTP server that serves files as HTML pages.
///
/// If a single path is a directory, serves the whole directory tree as a file
/// browser.  Otherwise serves the given files individually (backward-compat
/// with old index-based routing).
///
/// If `paths` is empty, defaults to serving the current directory.
pub fn serve_files(paths: &[String], port: u16) -> std::io::Result<()> {
    let roots: Vec<RootEntry> = if paths.is_empty() {
        vec![RootEntry::Dir(std::env::current_dir()?)]
    } else if paths.len() == 1 {
        let p = Path::new(&paths[0]);
        if p.is_dir() {
            vec![RootEntry::Dir(p.canonicalize()?)]
        } else {
            paths.iter().map(|s| RootEntry::File(s.clone())).collect()
        }
    } else {
        // Multiple paths — could be all files, or mix of files + dirs
        let mut entries = Vec::new();
        for p in paths {
            let path = Path::new(p);
            if path.is_dir() {
                entries.push(RootEntry::Dir(path.canonicalize()?));
            } else {
                entries.push(RootEntry::File(p.clone()));
            }
        }
        entries
    };

    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr)
        .map_err(|e| {
            eprintln!("ccat: --serve: cannot bind to {addr}: {e}");
            e
        })?;

    let mode_desc = match &roots[..] {
        [RootEntry::Dir(d)] => format!("directory: {}", d.display()),
        list => {
            let n = list.len();
            format!("{n} item(s)")
        }
    };

    eprintln!(
        "\x1b[2mccat: serving {} at http://localhost:{port}/\x1b[0m",
        mode_desc
    );

    let state = ServeState { roots };

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = handle_connection(stream, &state) {
                    eprintln!("ccat: --serve: connection error: {e}");
                }
            }
            Err(e) => {
                eprintln!("ccat: --serve: accept error: {e}");
                break;
            }
        }
    }

    Ok(())
}

// ── Internal state ──

enum RootEntry {
    File(String),
    Dir(PathBuf),
}

struct ServeState {
    roots: Vec<RootEntry>,
}

// ── Connection handling ──

fn handle_connection(mut stream: TcpStream, state: &ServeState) -> std::io::Result<()> {
    let peer = stream.peer_addr();
    let mut reader = BufReader::new(&mut stream);
    let mut request_line = String::new();

    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }

    let request_line = request_line.trim().to_string();

    // Read and parse headers (need to keep for SSE origin checks and query params)
    let mut header_line = String::new();
    loop {
        header_line.clear();
        if reader.read_line(&mut header_line)? == 0 || header_line.trim().is_empty() {
            break;
        }
    }

    // Parse request path and method
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    let _method = parts.first().copied().unwrap_or("GET");
    let request_path = parts.get(1).copied().unwrap_or("/");

    // SSE watching endpoint
    if request_path.starts_with("/__watch__") {
        // Extract the ?path= query parameter
        let watch_target = request_path
            .split('?')
            .nth(1)
            .and_then(|q| {
                q.split('&').find_map(|pair| {
                    let mut kv = pair.splitn(2, '=');
                    let key = kv.next()?;
                    let val = kv.next()?;
                    if key == "path" { Some(val) } else { None }
                })
            })
            .unwrap_or("");

        let path = urlencoding_decode(watch_target);
        handle_sse_watch(stream, &path, state)
    } else {
        let response = match &state.roots[..] {
            // Single directory root — map URL paths directly to filesystem
            [RootEntry::Dir(root)] => serve_from_directory(request_path, root),
            // Multiple roots — backward compat with index-based + filename routing
            roots => serve_from_roots(request_path, roots),
        };

        stream.write_all(response.as_bytes())?;
        stream.flush()?;

        if let Ok(addr) = &peer {
            let now = format_timestamp();
            eprintln!(
                "\x1b[2m[{}] {} {} {}\x1b[0m",
                now,
                addr,
                request_line,
                response.lines().next().unwrap_or("HTTP/1.1 ???")
            );
        }

        Ok(())
    }
}

// ── Directory mode ──

fn serve_from_directory(request_path: &str, root: &Path) -> String {
    // Strip query parameters (e.g., ?thumb=1)
    let clean_request = request_path.split('?').next().unwrap_or(request_path);
    // Sanitize and resolve path
    let clean_path = sanitize_path(clean_request);
    let target = root.join(&clean_path);

    // Security: ensure we haven't escaped the root
    match target.canonicalize() {
        Ok(canon) => {
            if !canon.starts_with(root) {
                return forbidden_response("Path traversal detected");
            }
            if canon.is_dir() {
                serve_directory_listing(&canon, root)
            } else if canon.is_file() {
                serve_file(&canon)
            } else {
                not_found_response(request_path)
            }
        }
        Err(_) => not_found_response(request_path),
    }
}

fn sanitize_path(request_path: &str) -> PathBuf {
    let path = request_path.trim_start_matches('/');
    if path.is_empty() {
        return PathBuf::new();
    }

    let mut result = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            std::path::Component::Normal(c) => {
                if result.components().count() < MAX_DEPTH {
                    result.push(c);
                }
            }
            std::path::Component::ParentDir => {
                // Allow ".." but bound it
                if result.components().count() > 0 {
                    result.pop();
                }
            }
            _ => {} // Skip root, prefix, curdir
        }
    }
    result
}

fn serve_directory_listing(dir: &Path, root: &Path) -> String {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => {
            let mut items: Vec<_> = entries
                .filter_map(|e| e.ok())
                .map(|e| DirEntry::from_dir_entry(e, root))
                .collect();
            // Sort: directories first, then files, then alphabetical
            items.sort_by(|a, b| {
                a.is_dir
                    .cmp(&b.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                    .reverse() // reverse so dirs come first (true > false)
                    .then_with(|| a.name.cmp(&b.name))
            });
            items
        }
        Err(e) => {
            return internal_error_response(&format!("Cannot read directory: {e}"));
        }
    };

    // Breadcrumb navigation
    let breadcrumbs = breadcrumb_html(dir, root);

    // Compute relative path from root for the page title
    let rel_path = dir
        .strip_prefix(root)
        .unwrap_or(dir)
        .display()
        .to_string();
    let title = if rel_path.is_empty() || rel_path == "." {
        format!("📁 {}", root.file_name().unwrap_or(root.as_os_str()).to_string_lossy())
    } else {
        format!("📁 {}", rel_path)
    };

    // Generate table rows
    let mut rows = String::new();
    // Grid view items
    let mut grid_items = String::new();
    let mut has_images = false;

    // ".." link if not at root
    if dir != root {
        let parent_url = parent_url_path(dir, root);
        rows.push_str(&format!(
            r#"<tr class="dir"><td class="icon">📂</td><td><a href="{}">..</a></td><td class="dim">—</td><td class="dim">parent directory</td><td></td></tr>"#,
            parent_url
        ));
    }

    for entry in &entries {
        let icon = if entry.is_dir {
            "📁"
        } else {
            file_icon(&entry.name)
        };
        let url_path = url_encode_path(&entry.rel_path);
        let size_str = if entry.is_dir {
            String::from("—")
        } else {
            human_size(entry.size)
        };
        let mtime_str = if entry.is_dir {
            String::new()
        } else {
            format_mtime(entry.mtime)
        };

        // Image thumbnail for non-directory image files
        let icon_cell = if !entry.is_dir {
            let _path_obj = Path::new(&entry.rel_path);
            let full_path = root.join(&entry.rel_path);
            if is_image_file(&full_path) {
                has_images = true;
                // Thumbnail using the query param ?thumb=1
                let thumb = file_thumbnail_html(&full_path, &entry.rel_path);
                format!(r#"<td class="icon">{}{}</td>"#, thumb, icon)
            } else {
                format!(r#"<td class="icon">{}</td>"#, icon)
            }
        } else {
            format!(r#"<td class="icon">{}</td>"#, icon)
        };

        rows.push_str(&format!(
            r#"<tr class="{}"><td class="icon">{}</td><td><a href="/{}">{}</a></td><td data-sort="{}">{}</td><td class="dim">{}</td><td class="date" data-sort="{}">{}</td></tr>"#,
            if entry.is_dir { "dir" } else { "file" },
            icon_cell,
            url_path,
            html_escape(&entry.name),
            entry.size,
            size_str,
            html_escape(&entry.description),
            entry.mtime,
            mtime_str,
        ));

        // Grid view item (for image files)
        if !entry.is_dir {
            let full_path = root.join(&entry.rel_path);
            if is_image_file(&full_path) {
                grid_items.push_str(&format!(
                    r#"<div class="grid-item"><a href="/{}"><img class="thumb" src="/{}?thumb=1" alt="{}" loading="lazy"><div class="info"><div class="name">{}</div><div class="meta">{} · {}</div></div></a></div>"#,
                    url_path, url_path, html_escape(&entry.name), html_escape(&entry.name), size_str, html_escape(&entry.description),
                ));
            }
        }
    }

    // Summary
    let dir_count = entries.iter().filter(|e| e.is_dir).count();
    let file_count = entries.iter().filter(|e| !e.is_dir).count();
    let total_size: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
    let summary = if entries.is_empty() {
        "Empty directory".to_string()
    } else {
        format!(
            "{} dir{}, {} file{} · {}",
            dir_count,
            if dir_count == 1 { "" } else { "s" },
            file_count,
            if file_count == 1 { "" } else { "s" },
            human_size(total_size),
        )
    };

    let view_toggle = if has_images {
        r#"<div class="view-toggle"><button class="view-btn active" data-view="table">📋 Table</button><button class="view-btn" data-view="grid">🔲 Grid</button></div>"#.to_string()
    } else {
        String::new()
    };

    let grid_section = if has_images {
        format!(r#"<div class="grid-container">{}</div>"#, grid_items)
    } else {
        String::new()
    };

    let body = format!(
        r#"<div class="breadcrumbs">{}</div>
<div class="toolbar">
    <div class="summary">{}</div>
    <div class="search-wrapper">
        <span class="search-icon">🔍</span>
        <input class="search-bar" id="search" placeholder="Filter files…" autofocus>
    </div>
    {}
</div>
<table>
<thead>
<tr><th></th><th data-col="name">Name <span class="sort-arrow"></span></th><th data-col="size">Size <span class="sort-arrow"></span></th><th data-col="type">Type <span class="sort-arrow"></span></th><th data-col="date">Modified <span class="sort-arrow"></span></th></tr>
</thead>
<tbody>
{}
</tbody>
</table>
{}
<div class="msg hidden"><h2>🔍 No matches</h2><p>Try a different search term</p></div>"#,
        breadcrumbs, summary, view_toggle, rows, grid_section,
    );

    render_page(&title, &body)
}

// ── File serving ──

fn serve_file(path: &Path) -> String {
    match fs::read(path) {
        Ok(data) => {
            let kind = if data.is_empty() {
                FileKind::PlainText
            } else {
                detect_kind(&data, path)
            };

            // For images and media, serve raw bytes with proper Content-Type
            match kind {
                FileKind::Image => {
                    let mime = mime_for_image(path);
                    media_response(&data, mime, path)
                }
                _ => {
                    let html = cat_html::cat_file_html(&data, kind, path);

                    // Add copy and watch buttons before the first code block
                    let rel_path = path.to_string_lossy();
                    let actions_html = format!(
                        r#"<div class="file-actions"><button class="watch-btn" id="watchBtn" data-path="{}">🔴 Watch</button><button class="theme-btn" id="copyBtn">📋 Copy</button></div>"#,
                        html_escape(&rel_path)
                    );
                    let html = html.replace("<pre", &format!("{}<pre", actions_html));

                    format!(
                        "HTTP/1.1 200 OK\r\n\
                         Content-Type: text/html; charset=utf-8\r\n\
                         Content-Length: {}\r\n\
                         Access-Control-Allow-Origin: *\r\n\
                         Cache-Control: no-cache\r\n\
                         \r\n\
                         {}",
                        html.len(),
                        html
                    )
                }
            }
        }
        Err(e) => {
            let body = format!(
                "<!DOCTYPE html><html><body><h1>500</h1><p>{}: {}</p></body></html>",
                path.display(),
                e
            );
            format!(
                "HTTP/1.1 500 Internal Server Error\r\n\
                 Content-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\n\
                 \r\n\
                 {}",
                body.len(),
                body
            )
        }
    }
}

// ── Multi-root mode (backward compat) ──

fn serve_from_roots(request_path: &str, roots: &[RootEntry]) -> String {
    let clean_path = request_path.trim_start_matches('/');

    // Try index-based routing: /1, /2, etc.
    if let Ok(idx) = clean_path.parse::<usize>() {
        if idx > 0 && idx <= roots.len() {
            return match &roots[idx - 1] {
                RootEntry::File(p) => serve_file(Path::new(p)),
                RootEntry::Dir(p) => serve_directory_listing(p, p),
            };
        }
    }

    // Try filename matching
    for (_i, root) in roots.iter().enumerate() {
        let path_str = match root {
            RootEntry::File(p) => p.clone(),
            RootEntry::Dir(p) => p.to_string_lossy().to_string(),
        };
        let name = Path::new(&path_str)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if name == clean_path {
            return match root {
                RootEntry::File(p) => serve_file(Path::new(p)),
                RootEntry::Dir(p) => serve_directory_listing(p, p),
            };
        }
    }

    // Multi-root directory mode: /1/sub/path
    if let Some((idx_str, sub_path)) = clean_path.split_once('/') {
        if let Ok(idx) = idx_str.parse::<usize>() {
            if idx > 0 && idx <= roots.len() {
                if let RootEntry::Dir(root) = &roots[idx - 1] {
                    return serve_from_directory(&format!("/{sub_path}"), root);
                }
            }
        }
    }

    not_found_response(request_path)
}

// ── SSE file watching ──

/// Handle a Server-Sent Events connection for live file watching.
/// Polls the file's modification time every 500ms and sends a 'refresh'
/// event when the file changes.
fn handle_sse_watch(mut stream: TcpStream, watch_path: &str, state: &ServeState) -> std::io::Result<()> {
    // Resolve the watch path relative to the root
    let resolved = match &state.roots[..] {
        [RootEntry::Dir(root)] => {
            let clean = sanitize_path(watch_path);
            root.join(&clean)
        }
        _ => {
            // For multi-root, try to find the file
            let path = Path::new(watch_path);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir().unwrap_or_default().join(watch_path)
            }
        }
    };

    // Canonicalize for security
    let target = match resolved.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Type: text/event-stream\r\n\r\nevent: error\ndata: File not found\n\n");
            return Ok(());
        }
    };

    if !target.is_file() {
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/event-stream\r\n\r\nevent: error\ndata: Not a file\n\n");
        return Ok(());
    }

    // Send SSE headers
    let headers = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         Connection: keep-alive\r\n\
         Access-Control-Allow-Origin: *\r\n\
         \r\n"
    );
    stream.write_all(headers.as_bytes())?;
    stream.flush()?;

    // Send initial connected event
    let connected = "event: connected\ndata: ok\n\n";
    stream.write_all(connected.as_bytes())?;
    stream.flush()?;

    // Poll for changes
    let poll_interval = std::time::Duration::from_millis(500);
    let mut last_mtime = target.metadata().ok().and_then(|m| m.modified().ok());
    let mut last_size = target.metadata().ok().map(|m| m.len());

    loop {
        std::thread::sleep(poll_interval);

        let meta = match target.metadata() {
            Ok(m) => m,
            Err(_) => {
                // File deleted or inaccessible
                let _ = stream.write_all(b"event: error\ndata: File lost\n\n");
                let _ = stream.flush();
                break;
            }
        };

        let current_mtime = meta.modified().ok();
        let current_size = Some(meta.len());

        if current_mtime != last_mtime || current_size != last_size {
            last_mtime = current_mtime;
            last_size = current_size;

            // Send refresh event
            let event = format!("event: refresh\ndata: {}\n\n", 
                current_mtime.map(|t| format!("{:?}", t)).unwrap_or_default()
            );
            if stream.write_all(event.as_bytes()).is_err() {
                break; // Client disconnected
            }
            if stream.flush().is_err() {
                break;
            }
        }

        // Check if stream is still writable (client didn't disconnect)
        // We detect this by checking write readiness via a non-blocking write of empty
        let buf = [0u8; 0];
        if stream.write(&buf).is_err() {
            break;
        }
    }

    Ok(())
}

/// Simple URL percent-decoding (enough for file paths).
fn urlencoding_decode(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                    continue;
                }
            }
            // If decoding fails, keep the % as-is
            result.push('%');
            result.push_str(&hex);
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    result
}

// ── Data types ──

struct DirEntry {
    name: String,
    rel_path: String,
    is_dir: bool,
    size: u64,
    mtime: u64,
    description: String,
}

impl DirEntry {
    fn from_dir_entry(entry: fs::DirEntry, root: &Path) -> Self {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let full_path = entry.path();
        let rel_path = full_path
            .strip_prefix(root)
            .unwrap_or(&full_path)
            .to_string_lossy()
            .to_string();

        let description = if is_dir {
            String::from("directory")
        } else {
            // Describe by extension
            match full_path.extension().and_then(|e| e.to_str()) {
                Some(ext) => ext.to_uppercase(),
                None => {
                    if is_executable(&meta) {
                        "EXECUTABLE".into()
                    } else {
                        String::new()
                    }
                }
            }
        };

        DirEntry {
            name,
            rel_path,
            is_dir,
            size,
            mtime,
            description,
        }
    }
}

fn is_executable(meta: &Option<fs::Metadata>) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.as_ref()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

// ── HTML helpers ──

fn breadcrumb_html(current: &Path, root: &Path) -> String {
    let mut crumbs = String::new();
    crumbs.push_str(&format!(
        r#"<a href="/">📁 {}</a>"#,
        html_escape(&root.file_name().unwrap_or(root.as_os_str()).to_string_lossy())
    ));

    if let Ok(rel) = current.strip_prefix(root) {
        let mut accumulated = PathBuf::new();
        for component in rel.components() {
            let part = component.as_os_str().to_string_lossy();
            accumulated.push(&*part);
            let url = format!("/{}", accumulated.to_string_lossy());
            crumbs.push_str(&format!(
                r#" <span class="sep">›</span> <a href="{}">{}</a>"#,
                url,
                html_escape(&part)
            ));
        }
    }

    crumbs
}

fn breadcrumb_for_multi(_roots: &[RootEntry], current_idx: usize) -> String {
    let mut crumbs = String::new();
    crumbs.push_str(r#"<a href="/">📁 ccat serve</a>"#);
    crumbs.push_str(&format!(
        r#" <span class="sep">›</span> <a href="/{}">item {}</a>"#,
        current_idx + 1,
        current_idx + 1
    ));
    crumbs
}

fn parent_url_path(dir: &Path, root: &Path) -> String {
    if let Ok(rel) = dir.strip_prefix(root) {
        if let Some(parent) = rel.parent() {
            let s = parent.to_string_lossy();
            if s.is_empty() {
                String::from("/")
            } else {
                format!("/{}", s)
            }
        } else {
            String::from("/")
        }
    } else {
        String::from("/")
    }
}

fn render_page(title: &str, body: &str) -> String {
    let escaped_title = html_escape(title);
    let escaped_title_attr = title.replace('"', "&quot;").replace('&', "&amp;");
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en" data-theme="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{title} — ccat</title>
<style>
:root {{
    /* Dark theme (default) */
    --bg: #1a1b26;
    --bg2: #24253a;
    --surface: #1e1f33;
    --border: #2d2e44;
    --fg: #c0caf5;
    --fg2: #a9b1d6;
    --dim: #565f89;
    --accent: #7aa2f7;
    --accent2: #89ddff;
    --green: #9ece6a;
    --orange: #ff9e64;
    --red: #f7768e;
    --link: #7dcfff;
    --header-bg: #1a1b26;
    --header-border: #2d2e44;
    --search-bg: #1e1f33;
    --hover: #2a2b41;
    --table-header: #24253a;
    --table-border: #2d2e44;
    --scrollbar: #2d2e44;
    --scrollbar-hover: #3d3e5c;
    --code-bg: #1a1b26;
    --code-border: #2d2e44;
    --badge-bg: #24253a;
    --badge-height: 32px;
}}

[data-theme="light"] {{
    --bg: #f5f5f9;
    --bg2: #e8e8f0;
    --surface: #ffffff;
    --border: #d0d0da;
    --fg: #1a1b26;
    --fg2: #3b3c4e;
    --dim: #8888a0;
    --accent: #2e7de9;
    --accent2: #0099cc;
    --green: #2ea043;
    --orange: #d96c00;
    --red: #d02939;
    --link: #2e7de9;
    --header-bg: #ffffff;
    --header-border: #d0d0da;
    --search-bg: #ffffff;
    --hover: #e8e8f0;
    --table-header: #ededf4;
    --table-border: #d0d0da;
    --scrollbar: #d0d0da;
    --scrollbar-hover: #b0b0c0;
    --code-bg: #f5f5f9;
    --code-border: #d0d0da;
    --badge-bg: #e8e8f0;
}}

*, *::before, *::after {{ box-sizing: border-box; margin: 0; padding: 0; }}
html {{ scroll-behavior: smooth; }}
body {{
    background: var(--bg);
    color: var(--fg);
    font-family: system-ui, -apple-system, 'Segoe UI', sans-serif;
    font-size: 14px;
    line-height: 1.6;
    padding: 0;
    margin: 0;
    min-height: 100vh;
}}
body {{ font-family: 'Inter', system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif; }}
::selection {{ background: var(--accent); color: #fff; }}
::-webkit-scrollbar {{ width: 8px; height: 8px; }}
::-webkit-scrollbar-track {{ background: var(--bg); }}
::-webkit-scrollbar-thumb {{ background: var(--scrollbar); border-radius: 4px; }}
::-webkit-scrollbar-thumb:hover {{ background: var(--scrollbar-hover); }}

/* ── Header ── */
.header {{
    position: sticky;
    top: 0;
    z-index: 100;
    background: var(--header-bg);
    border-bottom: 1px solid var(--header-border);
    padding: 0 24px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 52px;
    backdrop-filter: blur(8px);
}}
.header .title {{
    font-size: 14px;
    font-weight: 600;
    color: var(--fg);
    display: flex;
    align-items: center;
    gap: 8px;
}}
.header .title svg {{ width: 18px; height: 18px; flex-shrink: 0; }}
.header .meta {{ font-size: 11px; color: var(--dim); }}
.header-actions {{
    display: flex;
    align-items: center;
    gap: 8px;
}}

/* ── Theme toggle ── */
.theme-btn {{
    background: none;
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--dim);
    cursor: pointer;
    font-size: 14px;
    padding: 4px 8px;
    line-height: 1;
    transition: all 0.15s;
}}
.theme-btn:hover {{ color: var(--fg); border-color: var(--accent); }}

/* ── Live watch button ── */
.watch-btn {{
    background: none;
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--dim);
    cursor: pointer;
    font-size: 12px;
    padding: 4px 10px;
    line-height: 1;
    transition: all 0.15s;
}}
.watch-btn:hover {{ color: var(--green); border-color: var(--green); }}
.watch-btn.active {{ color: var(--green); border-color: var(--green); background: rgba(158,206,106,0.08); }}

/* ── Content ── */
.content {{
    max-width: 1100px;
    margin: 0 auto;
    padding: 0 24px 40px;
}}

/* ── Breadcrumbs ── */
.breadcrumbs {{
    padding: 12px 0 8px;
    font-size: 13px;
    color: var(--dim);
    overflow-x: auto;
    white-space: nowrap;
    border-bottom: 1px solid var(--border);
    margin-bottom: 8px;
}}
.breadcrumbs a {{ color: var(--link); text-decoration: none; }}
.breadcrumbs a:hover {{ color: var(--accent); text-decoration: underline; }}
.breadcrumbs .sep {{ margin: 0 6px; color: var(--dim); font-weight: 300; }}

/* ── Summary bar ── */
.toolbar {{
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 0 12px;
    flex-wrap: wrap;
}}
.summary {{
    font-size: 12px;
    color: var(--dim);
}}
.search-wrapper {{
    position: relative;
    flex: 1;
    max-width: 320px;
    min-width: 160px;
}}
.search-wrapper .search-icon {{
    position: absolute;
    left: 10px;
    top: 50%;
    transform: translateY(-50%);
    color: var(--dim);
    font-size: 13px;
    pointer-events: none;
}}
.search-bar {{
    width: 100%;
    padding: 7px 12px 7px 30px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--search-bg);
    color: var(--fg);
    font-size: 13px;
    outline: none;
    transition: border-color 0.15s;
}}
.search-bar:focus {{ border-color: var(--accent); box-shadow: 0 0 0 2px rgba(122,162,247,0.15); }}
.search-bar::placeholder {{ color: var(--dim); }}

/* ── View toggle ── */
.view-toggle {{
    display: flex;
    gap: 4px;
}}
.view-btn {{
    background: none;
    border: 1px solid var(--border);
    border-radius: 4px;
    color: var(--dim);
    cursor: pointer;
    font-size: 12px;
    padding: 4px 8px;
    line-height: 1;
    transition: all 0.15s;
    font-family: inherit;
}}
.view-btn:hover {{ color: var(--fg); }}
.view-btn.active {{ color: var(--accent); border-color: var(--accent); background: rgba(122,162,247,0.08); }}

/* ── Table ── */
table {{
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
}}
thead {{
    position: sticky;
    top: 52px;
    z-index: 10;
}}
th {{
    text-align: left;
    padding: 8px 10px;
    font-weight: 600;
    color: var(--dim);
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    border-bottom: 1px solid var(--table-border);
    background: var(--table-header);
    cursor: pointer;
    user-select: none;
    white-space: nowrap;
}}
th:hover {{ color: var(--fg); }}
th.sorted {{ color: var(--accent); }}
th .sort-arrow {{ margin-left: 4px; opacity: 0.5; }}
tr {{ border-bottom: 1px solid var(--border); transition: background 0.1s; }}
tr:hover {{ background: var(--hover); }}
td {{
    padding: 7px 10px;
    white-space: nowrap;
    vertical-align: middle;
}}
td.icon {{ width: 28px; text-align: center; font-size: 16px; }}
td:last-child {{ width: 150px; }}
td:nth-child(3) {{ width: 90px; text-align: right; font-variant-numeric: tabular-nums; }}
td:nth-child(4) {{ width: 110px; }}
tr.file td:first-child {{ font-size: 15px; }}
tr.dir td:first-child {{ font-size: 15px; }}
a {{ color: var(--link); text-decoration: none; }}
a:hover {{ color: var(--accent); text-decoration: underline; }}
.dim {{ color: var(--dim); font-size: 12px; }}
.date {{ color: var(--dim); font-size: 12px; font-variant-numeric: tabular-nums; }}
.hidden {{ display: none !important; }}

/* ── Image thumbnail in listing ── */
.img-preview {{
    display: inline-block;
    width: 28px;
    height: 28px;
    border-radius: 4px;
    overflow: hidden;
    vertical-align: middle;
    margin-right: 6px;
    background: var(--bg2);
    border: 1px solid var(--border);
}}
.img-preview img {{
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
}}

/* ── Grid view for images ── */
.grid-view table {{ display: none; }}
.grid-view .grid-container {{ display: grid !important; }}
.grid-container {{
    display: none;
    grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
    gap: 16px;
    padding: 16px 0;
}}
.grid-item {{
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
    transition: transform 0.15s, box-shadow 0.15s;
}}
.grid-item:hover {{ transform: translateY(-2px); box-shadow: 0 4px 12px rgba(0,0,0,0.3); }}
.grid-item a {{ display: block; text-decoration: none; color: inherit; }}
.grid-item .thumb {{
    width: 100%;
    height: 140px;
    object-fit: cover;
    display: block;
    background: var(--bg2);
}}
.grid-item .info {{
    padding: 8px 10px;
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}}
.grid-item .info .name {{ color: var(--fg); font-weight: 500; }}
.grid-item .info .meta {{ color: var(--dim); font-size: 11px; }}

/* ── Empty state ── */
.msg {{
    text-align: center;
    padding: 60px 20px;
    color: var(--dim);
}}
.msg h2 {{ color: var(--fg2); margin-bottom: 8px; font-weight: 500; }}
.msg p {{ font-size: 13px; }}

/* ── File view (code/markdown/text) ── */
.file-view {{
    padding: 0;
}}
.file-actions {{
    display: flex;
    gap: 8px;
    padding: 12px 0;
    border-bottom: 1px solid var(--border);
    margin-bottom: 0;
    flex-wrap: wrap;
}}
.file-actions button {{
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--dim);
    cursor: pointer;
    font-size: 12px;
    padding: 5px 12px;
    font-family: inherit;
    transition: all 0.15s;
}}
.file-actions button:hover {{ color: var(--fg); border-color: var(--accent); }}
.file-actions button.copied {{ color: var(--green); border-color: var(--green); }}

/* ── Code blocks ── */
pre {{
    padding: 16px 20px;
    overflow-x: auto;
    tab-size: 4;
    -moz-tab-size: 4;
    background: var(--code-bg) !important;
    border-radius: 0;
    margin: 0;
}}
pre code {{ counter-reset: line; }}
pre code .line {{
    display: block;
    line-height: 1.6;
    min-height: 1.2em;
}}
pre code .line::before {{
    counter-increment: line;
    content: counter(line);
    display: inline-block;
    width: 3em;
    padding-right: 1.5em;
    text-align: right;
    color: var(--dim);
    user-select: none;
    opacity: 0.6;
}}

/* ── Markdown body ── */
.markdown-body {{
    padding: 24px 0;
    max-width: 900px;
    margin: 0 auto;
}}
.markdown-body h1, .markdown-body h2, .markdown-body h3, .markdown-body h4 {{
    color: var(--fg);
    margin-top: 1.5em;
    margin-bottom: 0.5em;
    font-weight: 600;
}}
.markdown-body h1 {{ font-size: 1.6em; border-bottom: 1px solid var(--border); padding-bottom: 0.3em; }}
.markdown-body h2 {{ font-size: 1.3em; border-bottom: 1px solid var(--border); padding-bottom: 0.2em; }}
.markdown-body h3 {{ font-size: 1.1em; }}
.markdown-body p, .markdown-body li {{ line-height: 1.8; color: var(--fg2); }}
.markdown-body a {{ color: var(--accent); text-decoration: underline; }}
.markdown-body a:hover {{ color: var(--link); }}
.markdown-body code {{
    background: var(--bg2);
    border-radius: 4px;
    padding: 2px 6px;
    font-family: 'JetBrains Mono', 'Fira Code', 'Consolas', monospace;
    font-size: 0.9em;
    color: var(--orange);
}}
.markdown-body pre code {{
    background: none;
    padding: 0;
    color: inherit;
    font-size: inherit;
}}
.markdown-body pre {{
    background: var(--code-bg);
    border: 1px solid var(--code-border);
    border-radius: 8px;
    padding: 14px 18px;
    overflow-x: auto;
}}
.markdown-body blockquote {{
    border-left: 3px solid var(--accent);
    color: var(--dim);
    padding-left: 14px;
    margin: 1em 0;
}}
.markdown-body table {{
    border-collapse: collapse;
    width: 100%;
    margin: 1em 0;
}}
.markdown-body th, .markdown-body td {{
    border: 1px solid var(--border);
    padding: 8px 12px;
    text-align: left;
}}
.markdown-body th {{ background: var(--table-header); color: var(--fg2); font-weight: 600; }}
.markdown-body img {{ max-width: 100%; border-radius: 6px; }}

/* ── Footer ── */
.footer {{
    text-align: center;
    padding: 24px;
    color: var(--dim);
    font-size: 11px;
    border-top: 1px solid var(--border);
    margin-top: 2em;
}}

/* ── Keyboard hint ── */
.kbd {{
    display: inline-block;
    padding: 1px 5px;
    font-size: 10px;
    font-family: inherit;
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 3px;
    color: var(--dim);
    line-height: 1.4;
}}

/* ── Toast notification ── */
.toast {{
    position: fixed;
    bottom: 24px;
    left: 50%;
    transform: translateX(-50%);
    background: var(--surface);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px 20px;
    font-size: 13px;
    box-shadow: 0 4px 12px rgba(0,0,0,0.3);
    opacity: 0;
    transition: opacity 0.3s;
    z-index: 200;
    pointer-events: none;
}}
.toast.show {{ opacity: 1; }}

/* ── Responsive ── */
@media (max-width: 768px) {{
    .content {{ padding: 0 12px 24px; }}
    .header {{ padding: 0 12px; }}
    td:last-child {{ width: 100px; }}
    td:nth-child(3) {{ width: 60px; }}
    td:nth-child(4) {{ width: 80px; }}
    .grid-container {{ grid-template-columns: repeat(auto-fill, minmax(140px, 1fr)); gap: 10px; }}
}}

/* ── SSE watching indicator ── */
.watch-indicator {{
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--green);
}}
.watch-dot {{
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--green);
    animation: pulse 2s infinite;
}}
@keyframes pulse {{
    0%, 100% {{ opacity: 1; }}
    50% {{ opacity: 0.3; }}
}}
</style>
</head>
<body>
<div class="header">
    <div class="title">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20"/><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z"/><path d="M8 7h8"/><path d="M8 11h6"/><path d="M16 11h1"/><path d="M8 15h3"/></svg>
        {escaped_title}
    </div>
    <div class="header-actions">
        <span id="watchIndicator" class="watch-indicator hidden">
            <span class="watch-dot"></span> watching
        </span>
        <button class="theme-btn" id="themeToggle" title="Toggle theme (Ctrl+Shift+T)">◐</button>
    </div>
</div>
<div class="content">
{body}
</div>
<div id="toast" class="toast"></div>
<script>
(function() {{
    // ── Theme system ──
    const html = document.documentElement;
    const themeBtn = document.getElementById('themeToggle');
    const saved = localStorage.getItem('ccat-theme') || 'dark';
    html.setAttribute('data-theme', saved);
    themeBtn.textContent = saved === 'dark' ? '☀' : '☾';

    themeBtn.addEventListener('click', function(e) {{
        const current = html.getAttribute('data-theme');
        const next = current === 'dark' ? 'light' : 'dark';
        html.setAttribute('data-theme', next);
        localStorage.setItem('ccat-theme', next);
        themeBtn.textContent = next === 'dark' ? '☀' : '☾';
    }});

    // Ctrl+Shift+T toggles theme
    document.addEventListener('keydown', function(e) {{
        if (e.ctrlKey && e.shiftKey && (e.key === 'T' || e.key === 't')) {{
            e.preventDefault();
            themeBtn.click();
        }}
    }});

    // ── Keyboard shortcuts ──
    // '/' to focus search, Escape to blur
    document.addEventListener('keydown', function(e) {{
        if (e.key === '/' && !['INPUT', 'TEXTAREA'].includes(e.target.tagName)) {{
            e.preventDefault();
            const search = document.querySelector('.search-bar');
            if (search) {{ search.focus(); search.select(); }}
        }}
        if (e.key === 'Escape') {{
            const search = document.querySelector('.search-bar');
            if (search && document.activeElement === search) {{
                search.blur();
                search.value = '';
                filterFiles('');
            }}
        }}
        // j/k for navigating rows (only when not focused on input)
        if (!['INPUT', 'TEXTAREA'].includes(e.target.tagName)) {{
            const rows = document.querySelectorAll('tbody tr:not(.hidden)');
            if (rows.length === 0) return;
            let currentIdx = -1;
            rows.forEach((r, i) => {{
                if (r.classList.contains('focused')) {{ currentIdx = i; }}
            }});
            if (e.key === 'j' || e.key === 'J') {{
                e.preventDefault();
                const next = Math.min(currentIdx + 1, rows.length - 1);
                rows.forEach(r => r.classList.remove('focused'));
                if (next >= 0) {{
                    rows[next].classList.add('focused');
                    rows[next].scrollIntoView({{ block: 'nearest' }});
                }}
            }}
            if (e.key === 'k' || e.key === 'K') {{
                e.preventDefault();
                const prev = Math.max(currentIdx - 1, 0);
                rows.forEach(r => r.classList.remove('focused'));
                rows[prev].classList.add('focused');
                rows[prev].scrollIntoView({{ block: 'nearest' }});
            }}
            if (e.key === 'Enter' && currentIdx >= 0) {{
                const link = rows[currentIdx].querySelector('a');
                if (link) {{ window.location.href = link.href; }}
            }}
        }}
    }});

    // ── Search / filter ──
    window.filterFiles = function(q) {{
        let visible = 0;
        const searchInput = document.querySelector('.search-bar');
        const grid = document.querySelector('.grid-container');
        const table = document.querySelector('table');
        const msg = document.querySelector('.msg');

        if (!q) {{
            // Show all
            document.querySelectorAll('tbody tr').forEach(function(r) {{
                r.classList.remove('hidden');
                visible++;
            }});
            if (grid) {{
                document.querySelectorAll('.grid-item').forEach(function(r) {{
                    r.classList.remove('hidden');
                }});
            }}
        }} else {{
            let re;
            try {{
                re = new RegExp(q.replace(/[.*+?^${{}}()|[\\]\\\\\\/]/g, '\\\\$&'), 'i');
            }} catch(e) {{
                re = null;
            }}
            if (re) {{
                document.querySelectorAll('tbody tr').forEach(function(r) {{
                    const name = r.querySelector('td:nth-child(2)')?.textContent || '';
                    const match = re.test(name);
                    r.classList.toggle('hidden', !match);
                    if (match) visible++;
                }});
                if (grid) {{
                    document.querySelectorAll('.grid-item').forEach(function(r) {{
                        const name = r.querySelector('.name')?.textContent || '';
                        r.classList.toggle('hidden', !re.test(name));
                    }});
                }}
            }}
        }}
        if (msg) {{
            msg.classList.toggle('hidden', visible > 0 || !q);
            if (visible === 0 && q) {{
                const total = document.querySelectorAll('tbody tr').length;
                msg.querySelector('h2').textContent = '🔍 No matches';
                msg.querySelector('p').textContent = visible === 0 ? 'Try a different search term (' + total + ' items total)' : '';
            }}
        }}
    }};

    const searchInput = document.querySelector('.search-bar');
    if (searchInput) {{
        searchInput.addEventListener('input', function() {{
            window.filterFiles(this.value);
        }});
        searchInput.addEventListener('focus', function() {{ this.select(); }});
    }}

    // ── Column sorting ──
    document.querySelectorAll('th[data-col]').forEach(function(th) {{
        th.addEventListener('click', function() {{
            const col = this.dataset.col;
            const table = this.closest('table');
            const tbody = table.querySelector('tbody');
            const rows = Array.from(tbody.querySelectorAll('tr'));
            
            // Determine sort direction
            const isAsc = this.classList.contains('sorted-asc');
            document.querySelectorAll('th').forEach(t => {{
                t.classList.remove('sorted', 'sorted-asc', 'sorted-desc');
                const arrow = t.querySelector('.sort-arrow');
                if (arrow) arrow.textContent = '';
            }});
            this.classList.add('sorted', isAsc ? 'sorted-desc' : 'sorted-asc');
            const arrow = this.querySelector('.sort-arrow') || (() => {{
                const s = document.createElement('span');
                s.className = 'sort-arrow';
                this.appendChild(s);
                return s;
            }})();
            arrow.textContent = isAsc ? ' ▲' : ' ▼';

            rows.sort(function(a, b) {{
                let va, vb;
                if (col === 'name') {{
                    va = a.querySelector('td:nth-child(2)')?.textContent?.toLowerCase() || '';
                    vb = b.querySelector('td:nth-child(2)')?.textContent?.toLowerCase() || '';
                }} else if (col === 'size') {{
                    va = parseFloat(a.querySelector('td:nth-child(3)')?.dataset.sort || '0');
                    vb = parseFloat(b.querySelector('td:nth-child(3)')?.dataset.sort || '0');
                }} else if (col === 'type') {{
                    va = a.querySelector('td:nth-child(4)')?.textContent?.toLowerCase() || '';
                    vb = b.querySelector('td:nth-child(4)')?.textContent?.toLowerCase() || '';
                }} else if (col === 'date') {{
                    va = a.querySelector('td:nth-child(5)')?.dataset.sort || '0';
                    vb = b.querySelector('td:nth-child(5)')?.dataset.sort || '0';
                }}
                const cmp = typeof va === 'string' ? va.localeCompare(vb) : (va - vb);
                return isAsc ? -cmp : cmp;
            }});

            rows.forEach(r => tbody.appendChild(r));
        }});
    }});

    // ── Toast notification ──
    window.showToast = function(msg) {{
        const toast = document.getElementById('toast');
        if (!toast) return;
        toast.textContent = msg;
        toast.classList.add('show');
        setTimeout(() => toast.classList.remove('show'), 2000);
    }};

    // ── View toggle ──
    document.querySelectorAll('.view-btn').forEach(function(btn) {{
        btn.addEventListener('click', function() {{
            document.querySelectorAll('.view-btn').forEach(b => b.classList.remove('active'));
            this.classList.add('active');
            const view = this.dataset.view;
            const container = document.querySelector('.content');
            container.classList.toggle('grid-view', view === 'grid');
            localStorage.setItem('ccat-view', view);
        }});
    }});
    const savedView = localStorage.getItem('ccat-view');
    if (savedView === 'grid') {{
        const gbtn = document.querySelector('.view-btn[data-view="grid"]');
        if (gbtn) gbtn.click();
    }}

    // ── Copy code button ──
    const copyBtn = document.getElementById('copyBtn');
    if (copyBtn) {{
        copyBtn.addEventListener('click', function() {{
            const code = document.querySelector('pre code');
            if (!code) return;
            const text = Array.from(code.querySelectorAll('.line')).map(l => l.textContent).join('\\n');
            navigator.clipboard.writeText(text).then(function() {{
                copyBtn.classList.add('copied');
                copyBtn.textContent = '✓ Copied';
                setTimeout(function() {{
                    copyBtn.classList.remove('copied');
                    copyBtn.textContent = '📋 Copy';
                }}, 2000);
            }}).catch(function() {{
                showToast('Failed to copy');
            }});
        }});
    }}

    // ── Live watch via SSE ──
    const watchBtn = document.getElementById('watchBtn');
    const watchIndicator = document.getElementById('watchIndicator');
    let eventSource = null;

    if (watchBtn) {{
        watchBtn.addEventListener('click', function() {{
            if (eventSource) {{
                eventSource.close();
                eventSource = null;
                watchBtn.classList.remove('active');
                watchBtn.textContent = '🔴 Watch';
                watchIndicator.classList.add('hidden');
                return;
            }}
            const watchPath = watchBtn.dataset.path;
            if (!watchPath) return;
            watchBtn.textContent = '◌ Connecting…';
            eventSource = new EventSource('/__watch__?path=' + encodeURIComponent(watchPath));
            eventSource.onopen = function() {{
                watchBtn.classList.add('active');
                watchBtn.textContent = '⏹ Stop';
                watchIndicator.classList.remove('hidden');
            }};
            eventSource.addEventListener('refresh', function() {{
                showToast('🔃 File changed, reloading…');
                setTimeout(function() {{ window.location.reload(); }}, 500);
            }});
            eventSource.onerror = function() {{
                if (eventSource) {{
                    eventSource.close();
                    eventSource = null;
                    watchBtn.classList.remove('active');
                    watchBtn.textContent = '🔴 Watch';
                    watchIndicator.classList.add('hidden');
                    showToast('Watch connection lost');
                }}
            }};
        }});
    }}
}})();
</script>
</body>
</html>"#,
        title = escaped_title_attr,
        escaped_title = escaped_title,
        body = body,
    );
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        html.len(),
        html
    )
}

fn media_response(data: &[u8], mime: &str, path: &Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: {}\r\n\
         Content-Disposition: inline; filename=\"{}\"\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-cache\r\n\
         Access-Control-Allow-Origin: *\r\n\
         \r\n",
        mime,
        name,
        data.len(),
    )
}

fn mime_for_image(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "tiff" | "tif" => "image/tiff",
        _ => "application/octet-stream",
    }
}

fn is_image_file(path: &Path) -> bool {
    let mime = mime_for_image(path);
    mime.starts_with("image/")
}

fn file_thumbnail_html(_path: &Path, rel_path: &str) -> String {
    let url_path = url_encode_path(rel_path);
    format!(
        r#"<a href="/{}" class="img-preview"><img src="/{}?thumb=1" alt="" loading="lazy"></a>"#,
        url_path, url_path
    )
}

fn render_page_for_multi(body: &str) -> String {
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>ccat file index</title>
<style>
*, *::before, *::after {{ box-sizing: border-box; margin: 0; padding: 0; }}
body {{
    background: #2b303b;
    color: #c0c5ce;
    font-family: system-ui, -apple-system, sans-serif;
    font-size: 14px;
    line-height: 1.5;
    padding: 2em;
    max-width: 800px;
    margin: 0 auto;
}}
h1 {{ color: #8fa1b3; border-bottom: 1px solid #37404a; padding-bottom: 0.5em; }}
table {{ width: 100%; border-collapse: collapse; }}
td {{ padding: 8px 12px; border-bottom: 1px solid #1f2229; }}
a {{ color: #96b5b4; text-decoration: none; }}
a:hover {{ text-decoration: underline; }}
.dim {{ color: #4f5b66; font-size: 0.9em; }}
.footer {{ text-align: center; margin-top: 2em; color: #4f5b66; font-size: 0.85em; border-top: 1px solid #37404a; padding-top: 1em; }}
</style>
</head>
<body>
<h1>📄 ccat — file index</h1>
<table>
{body}
</table>
<div class="footer">Generated by ccat</div>
</body>
</html>"#,
        body = body
    );
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        html.len(),
        html
    )
}

// ── Error responses ──

fn not_found_response(request_path: &str) -> String {
    let body = format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>404</title>
<style>body{{background:#2b303b;color:#c0c5ce;font-family:monospace;padding:2em;}}a{{color:#96b5b4;}}h1{{color:#bf616a;}}</style>
</head>
<body>
<h1>404</h1>
<p>Not found: {}</p>
<p><a href="/">← back</a></p>
</body></html>"#,
        html_escape(request_path)
    );
    format!(
        "HTTP/1.1 404 Not Found\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        body.len(),
        body
    )
}

fn forbidden_response(reason: &str) -> String {
    let body = format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>403</title>
<style>body{{background:#2b303b;color:#c0c5ce;font-family:monospace;padding:2em;}}h1{{color:#bf616a;}}</style>
</head>
<body>
<h1>403 Forbidden</h1>
<p>{}</p>
</body></html>"#,
        html_escape(reason)
    );
    format!(
        "HTTP/1.1 403 Forbidden\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        body.len(),
        body
    )
}

fn internal_error_response(reason: &str) -> String {
    let body = format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>500</title>
<style>body{{background:#2b303b;color:#c0c5ce;font-family:monospace;padding:2em;}}h1{{color:#bf616a;}}</style>
</head>
<body>
<h1>500 Internal Server Error</h1>
<p>{}</p>
</body></html>"#,
        html_escape(reason)
    );
    format!(
        "HTTP/1.1 500 Internal Server Error\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        body.len(),
        body
    )
}

// ── Utility functions ──

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn url_encode_path(path: &str) -> String {
    let mut result = String::new();
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => {
                result.push(byte as char);
            }
            b' ' => result.push_str("%20"),
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

fn human_size(size: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if size == 0 {
        return "0 B".into();
    }
    let mut size = size as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} {}", size as u64, UNITS[unit_idx])
    } else if size >= 100.0 {
        format!("{:.0} {}", size, UNITS[unit_idx])
    } else if size >= 10.0 {
        format!("{:.1} {}", size, UNITS[unit_idx])
    } else {
        format!("{:.2} {}", size, UNITS[unit_idx])
    }
}

fn format_mtime(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;

    let (year, month, day) = days_to_date(days as i64);

    // Show time only if today, date otherwise
    let now_days = now / 86400;
    if days == now_days {
        format!("{:02}:{:02}", hours, minutes)
    } else if days + 6 >= now_days {
        // Within a week — show day name
        let weekday = day_of_week(days as i64);
        format!("{} {:02}:{:02}", weekday, hours, minutes)
    } else if year == 1970_i64 + (now / 31536000) as i64 {
        // Same year — show month/day
        format!("{:02}-{:02}", month, day)
    } else {
        format!("{:04}-{:02}-{:02}", year, month, day)
    }
}

fn file_icon(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext.to_lowercase().as_str() {
        "rs" => "🦀",
        "py" => "🐍",
        "js" | "ts" | "jsx" | "tsx" => "🟨",
        "go" => "🔷",
        "md" | "markdown" => "📝",
        "json" => "📋",
        "yaml" | "yml" | "toml" => "⚙️",
        "html" | "htm" | "css" => "🌐",
        "sh" | "bash" | "zsh" => "🐚",
        "c" | "h" | "cpp" | "hpp" | "cc" | "hh" => "⚡",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => "🖼️",
        "pdf" => "📕",
        "csv" | "tsv" => "📊",
        "lock" | "sum" => "🔒",
        "gitignore" | "gitattributes" | "gitmodules" => "🔗",
        "env" | "envrc" => "🔐",
        "mp3" | "flac" | "wav" | "ogg" | "m4a" => "🎵",
        "mp4" | "mkv" | "webm" | "avi" => "🎬",
        "zip" | "tar" | "gz" | "xz" | "bz2" | "7z" | "rar" => "🗜️",
        "exe" | "bin" | "wasm" => "⚙️",
        "deb" | "rpm" | "pkg" => "📦",
        _ => "📄",
    }
}

fn day_of_week(days: i64) -> &'static str {
    // 1970-01-01 was Thursday (0)
    match days.rem_euclid(7) {
        0 => "Thu",
        1 => "Fri",
        2 => "Sat",
        3 => "Sun",
        4 => "Mon",
        5 => "Tue",
        _ => "Wed",
    }
}

// Howard Hinnant's civil-from-days algorithm
fn days_to_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, m as u32, d as u32)
}

fn format_timestamp() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let _days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;
    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_path() {
        assert_eq!(sanitize_path("/"), PathBuf::new());
        assert_eq!(sanitize_path("foo"), PathBuf::from("foo"));
        assert_eq!(sanitize_path("/foo/bar"), PathBuf::from("foo/bar"));
        // ParentDir at root is a no-op, then etc/passwd is appended
        assert_eq!(sanitize_path("/../../../etc/passwd"), PathBuf::from("etc/passwd"));
        // Deep nesting is bounded
        let deep = "/a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z/0/1/2/3/4";
        let sanitized = sanitize_path(deep);
        assert!(sanitized.components().count() <= 32);
        // Actual security is enforced by canonicalize in serve_from_directory
    }

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.00 KB");
        assert_eq!(human_size(1536), "1.50 KB");
        assert_eq!(human_size(1_048_576), "1.00 MB");
        assert_eq!(human_size(1_073_741_824), "1.00 GB");
    }

    #[test]
    fn test_html_escape_basic() {
        assert_eq!(html_escape("hello"), "hello");
        assert_eq!(html_escape("<tag>"), "&lt;tag&gt;");
        assert_eq!(html_escape("a&b"), "a&amp;b");
        assert_eq!(html_escape("\"'"), "&quot;&#39;");
    }

    #[test]
    fn test_url_encode_path() {
        assert_eq!(url_encode_path("hello.txt"), "hello.txt");
        assert_eq!(url_encode_path("a b.txt"), "a%20b.txt");
        assert_eq!(url_encode_path("dir/file.rs"), "dir/file.rs");
        assert_eq!(url_encode_path("file#1.rs"), "file%231.rs");
    }

    #[test]
    fn test_days_to_date() {
        let (y, m, d) = days_to_date(0);
        assert_eq!(y, 1970);
        assert_eq!(m, 1);
        assert_eq!(d, 1);
        let (y2, m2, _d2) = days_to_date(20000);
        assert_eq!(y2, 2024);
        assert_eq!(m2, 10);
    }

    #[test]
    fn test_file_icon() {
        assert_eq!(file_icon("main.rs"), "🦀");
        assert_eq!(file_icon("index.html"), "🌐");
        assert_eq!(file_icon("README.md"), "📝");
        assert_eq!(file_icon("image.png"), "🖼️");
        assert_eq!(file_icon("unknown.xyz"), "📄");
    }

    #[test]
    fn test_day_of_week() {
        // 1970-01-01 was a Thursday
        assert_eq!(day_of_week(0), "Thu");
        // 1970-01-05 was a Monday
        assert_eq!(day_of_week(4), "Mon");
    }

    #[test]
    fn test_parent_url_path_simple() {
        let root = Path::new("/home/user");
        let dir = Path::new("/home/user/projects");
        assert_eq!(parent_url_path(dir, root), "/");
    }

    #[test]
    fn test_parent_url_path_nested() {
        let root = Path::new("/home/user");
        let dir = Path::new("/home/user/projects/ccat/src");
        assert_eq!(parent_url_path(dir, root), "/projects/ccat");
    }

    #[test]
    fn test_error_responses() {
        let resp = not_found_response("/missing");
        assert!(resp.contains("404"));
        assert!(resp.contains("/missing"));

        let resp = forbidden_response("nope");
        assert!(resp.contains("403"));
        assert!(resp.contains("nope"));

        let resp = internal_error_response("disk full");
        assert!(resp.contains("500"));
        assert!(resp.contains("disk full"));
    }

    #[test]
    fn test_format_mtime() {
        // Just verify it doesn't panic and returns something
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let formatted = format_mtime(now);
        assert!(!formatted.is_empty());
        assert_eq!(formatted.len(), 5); // HH:MM
    }
}
