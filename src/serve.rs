use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cat_html;
use crate::{detect_kind, FileKind};

/// Start a single-threaded HTTP server that serves files as HTML pages.
/// Blocks forever until Ctrl-C.
pub fn serve_files(paths: &[String], port: u16) -> std::io::Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr)
        .map_err(|e| {
            eprintln!("ccat: --serve: cannot bind to {addr}: {e}");
            e
        })?;

    eprintln!(
        "\x1b[2mccat: serving {} file(s) at http://localhost:{port}/\x1b[0m",
        paths.len()
    );

    // Single-threaded event loop
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = handle_connection(stream, paths) {
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

fn handle_connection(mut stream: TcpStream, paths: &[String]) -> std::io::Result<()> {
    let peer = stream.peer_addr();
    let mut reader = BufReader::new(&mut stream);
    let mut request_line = String::new();

    // Read the request line (e.g., "GET / HTTP/1.1")
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(()); // Connection closed
    }

    let request_line = request_line.trim().to_string();

    // Read and discard remaining request headers
    let mut header_line = String::new();
    loop {
        header_line.clear();
        if reader.read_line(&mut header_line)? == 0 || header_line.trim().is_empty() {
            break;
        }
    }

    // Parse the request path
    let request_path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/");

    // Determine which file to serve based on the path
    let response = if request_path == "/" || request_path.is_empty() {
        // Serve index (first file or directory listing)
        if paths.len() == 1 {
            serve_single_file(&paths[0], request_path)
        } else {
            serve_directory_listing(paths)
        }
    } else {
        // Try to serve a specific file by index or name
        let clean_path = request_path.trim_start_matches('/');
        if let Ok(idx) = clean_path.parse::<usize>() {
            if idx > 0 && idx <= paths.len() {
                serve_single_file(&paths[idx - 1], request_path)
            } else {
                not_found_response(request_path)
            }
        } else {
            // Try to match by filename
            let matched: Vec<&String> = paths
                .iter()
                .filter(|p| {
                    Path::new(p)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map_or(false, |n| n == clean_path)
                })
                .collect();
            if let Some(path) = matched.first() {
                serve_single_file(path, request_path)
            } else {
                not_found_response(request_path)
            }
        }
    };

    // Write the HTTP response
    stream.write_all(response.as_bytes())?;
    stream.flush()?;

    if let Ok(addr) = &peer {
        let now = format_timestamp();
        eprintln!(
            "\x1b[2m[{}] {} {} {}\x1b[0m",
            now,
            addr,
            request_line,
            response
                .lines()
                .next()
                .unwrap_or("HTTP/1.1 ???")
        );
    }

    Ok(())
}

fn serve_single_file(path: &str, _request_path: &str) -> String {
    match fs::read(path) {
        Ok(data) => {
            let path_obj = Path::new(path);
            let kind = if data.is_empty() {
                FileKind::PlainText
            } else {
                detect_kind(&data, path_obj)
            };

            let html = cat_html::cat_file_html(&data, kind, path_obj);

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
                path, e
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

fn serve_directory_listing(paths: &[String]) -> String {
    let mut items = String::new();
    for (i, path) in paths.iter().enumerate() {
        let name = Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path);
        let idx = i + 1;
        items.push_str(&format!(
            "<tr><td><a href=\"/{idx}\">{}</a></td><td class=\"dim\">{}</td></tr>\n",
            html_escape(name),
            html_escape(path)
        ));
    }

    let body = concat!(
        "<!DOCTYPE html>\n",
        "<html lang=\"en\">\n",
        "<head>\n",
        "<meta charset=\"utf-8\">\n",
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n",
        "<title>ccat — file index</title>\n",
        "<style>\n",
        "body { background: #2b303b; color: #c0c5ce; font-family: system-ui, sans-serif; padding: 2em; max-width: 800px; margin: 0 auto; }\n",
        "h1 { color: #8fa1b3; border-bottom: 1px solid #37404a; padding-bottom: 0.5em; }\n",
        "table { width: 100%; border-collapse: collapse; }\n",
        "td { padding: 8px 12px; border-bottom: 1px solid #1f2229; }\n",
        "a { color: #96b5b4; text-decoration: none; }\n",
        "a:hover { text-decoration: underline; }\n",
        ".dim { color: #4f5b66; font-size: 0.9em; }\n",
        ".footer { text-align: center; margin-top: 2em; color: #4f5b66; font-size: 0.85em; border-top: 1px solid #37404a; padding-top: 1em; }\n",
        "</style>\n",
        "</head>\n",
        "<body>\n",
        "<h1>📄 ccat — file index</h1>\n",
        "<table>\n",
        "<tr><th>File</th><th>Path</th></tr>\n"
    );
    let body = format!(
        "{body}{items}</table>\n<div class=\"footer\">Serving {count} file(s)</div>\n</body>\n</html>",
        body = body,
        items = items,
        count = paths.len()
    );

    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        body.len(),
        body
    )
}

fn not_found_response(request_path: &str) -> String {
    let body = format!(
        r#"<!DOCTYPE html>
<html><head><title>404</title></head>
<body style="background:#2b303b;color:#c0c5ce;font-family:monospace;padding:2em;">
<h1>404</h1>
<p>Not found: {}</p>
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

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Readable timestamp for access logs.
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
    let (_y, _m, _d) = days_to_date(days as i64);
    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}

fn days_to_date(mut days: i64) -> (i64, u32, u32) {
    days += 719468;
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let doe = days - era * 146097;
    let doy = doe - doe / 1460 + doe / 36524 - doe / 146096;
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = doy / 1460 + doe / 36524 - doe / 146096 + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m as u32, d as u32)
}
