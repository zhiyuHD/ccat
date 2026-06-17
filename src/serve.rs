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

    // Read and discard headers
    let mut header_line = String::new();
    loop {
        header_line.clear();
        if reader.read_line(&mut header_line)? == 0 || header_line.trim().is_empty() {
            break;
        }
    }

    // Parse request path
    let request_path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/");

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

// ── Directory mode ──

fn serve_from_directory(request_path: &str, root: &Path) -> String {
    // Sanitize and resolve path
    let clean_path = sanitize_path(request_path);
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

    // ".." link if not at root
    if dir != root {
        // Compute parent URL path
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

        rows.push_str(&format!(
            r#"<tr class="{}"><td class="icon">{}</td><td><a href="/{}">{}</a></td><td>{}</td><td class="dim">{}</td><td class="date">{}</td></tr>"#,
            if entry.is_dir { "dir" } else { "file" },
            icon,
            url_path,
            html_escape(&entry.name),
            size_str,
            html_escape(&entry.description),
            mtime_str,
        ));
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

    let body = format!(
        r#"<div class="breadcrumbs">{}</div>
<div class="summary">{}</div>
<table>
<tr><th></th><th>Name</th><th>Size</th><th>Type</th><th>Modified</th></tr>
{}</table>"#,
        breadcrumbs, summary, rows,
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
            let html = cat_html::cat_file_html(&data, kind, path);

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
    for (i, root) in roots.iter().enumerate() {
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

fn breadcrumb_for_multi(roots: &[RootEntry], current_idx: usize) -> String {
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
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{title} — ccat</title>
<style>
*, *::before, *::after {{ box-sizing: border-box; margin: 0; padding: 0; }}
body {{
    background: #2b303b;
    color: #c0c5ce;
    font-family: system-ui, -apple-system, sans-serif;
    font-size: 14px;
    line-height: 1.5;
    padding: 0;
    margin: 0;
    min-height: 100vh;
}}
.header {{
    position: sticky;
    top: 0;
    z-index: 10;
    background: #1f2229;
    border-bottom: 1px solid #37404a;
    padding: 12px 24px;
    display: flex;
    align-items: center;
    justify-content: space-between;
}}
.header .title {{
    font-size: 15px;
    font-weight: bold;
    color: #96b5b4;
}}
.header .meta {{
    font-size: 11px;
    color: #65737e;
}}
.content {{
    max-width: 1000px;
    margin: 0 auto;
    padding: 16px 24px;
}}
.breadcrumbs {{
    padding: 10px 0;
    font-size: 13px;
    color: #65737e;
    border-bottom: 1px solid #37404a;
    overflow-x: auto;
    white-space: nowrap;
}}
.breadcrumbs a {{
    color: #8fa1b3;
    text-decoration: none;
}}
.breadcrumbs a:hover {{
    color: #c0c5ce;
    text-decoration: underline;
}}
.breadcrumbs .sep {{
    margin: 0 4px;
    color: #4f5b66;
}}
.summary {{
    padding: 8px 0;
    font-size: 12px;
    color: #65737e;
}}
table {{
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
}}
tr {{
    border-bottom: 1px solid #1f2229;
}}
tr:hover {{
    background: #22262f;
}}
th {{
    text-align: left;
    padding: 6px 8px;
    font-weight: 600;
    color: #8fa1b3;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    border-bottom: 1px solid #37404a;
    position: sticky;
    top: 48px;
    background: #2b303b;
}}
td {{
    padding: 6px 8px;
    white-space: nowrap;
}}
td.icon {{
    width: 24px;
    text-align: center;
    font-size: 15px;
}}
tr.file td:first-child {{ font-size: 14px; }}
tr.dir td:first-child {{ font-size: 14px; }}
td:last-child {{ width: 140px; }}
td:nth-child(3) {{ width: 80px; text-align: right; font-variant-numeric: tabular-nums; }}
td:nth-child(4) {{ width: 100px; }}
a {{
    color: #96b5b4;
    text-decoration: none;
}}
a:hover {{
    color: #c0c5ce;
    text-decoration: underline;
}}
.dim {{
    color: #4f5b66;
    font-size: 12px;
}}
.date {{
    color: #4f5b66;
    font-size: 12px;
    font-variant-numeric: tabular-nums;
}}
.search-bar {{
    margin: 8px 0;
    padding: 8px 12px;
    width: 100%;
    max-width: 400px;
    border: 1px solid #37404a;
    border-radius: 4px;
    background: #1f2229;
    color: #c0c5ce;
    font-size: 13px;
    outline: none;
}}
.search-bar:focus {{
    border-color: #8fa1b3;
}}
.hidden {{ display: none; }}
.footer {{
    text-align: center;
    padding: 20px;
    color: #4f5b66;
    font-size: 11px;
    border-top: 1px solid #37404a;
    margin-top: 2em;
}}
.msg {{
    text-align: center;
    padding: 40px 20px;
    color: #65737e;
}}
.msg h2 {{ color: #8fa1b3; margin-bottom: 8px; }}
.msg p {{ font-size: 13px; }}
</style>
</head>
<body>
<div class="header">
    <span class="title">{title}</span>
    <span class="meta"><a href="/" style="color:#65737e;">ccat</a> file browser</span>
</div>
<div class="content">
<div class="search-bar" id="search" placeholder="Filter files…" oninput="filterFiles(this.value)" autofocus>Filter files…</div>
<script>
function filterFiles(q) {{
    let re = new RegExp(q.replace(/[.*+?^${{}}()|[\]\\\/]/g, '\\\\$&'), 'i');
    document.querySelectorAll('tbody tr').forEach(function(r) {{
        r.classList.toggle('hidden', q && !re.test(r.querySelector('td:nth-child(2)')?.textContent || ''));
    }});
    let visible = document.querySelectorAll('tbody tr:not(.hidden)').length;
    document.querySelector('.msg').classList.toggle('hidden', visible > 0 || !q);
}}
document.getElementById('search').addEventListener('focus', function() {{ this.select(); }});
</script>
<table>
<thead>
<tr><th></th><th>Name</th><th>Size</th><th>Type</th><th>Modified</th></tr>
</thead>
<tbody>
{body}
</tbody>
</table>
<div class="msg hidden"><h2>🔍 No matches</h2><p>Try a different search term</p></div>
</div>
<div class="footer">Generated by <a href="https://github.com/ccat" style="color:#65737e;">ccat</a></div>
</body>
</html>"#,
        title = html_escape(title),
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
    let days = secs / 86400;
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
