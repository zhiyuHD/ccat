use std::io::{Write, Read};
use std::net::TcpStream;
use crate::response::{Response, StatusCode};

/// A synchronous HTTP client.
pub struct HttpClient;

impl HttpClient {
    pub fn get(url: &str) -> Result<Response, ClientError> {
        Self::request("GET", url, None, None)
    }

    pub fn post(url: &str, body: &str) -> Result<Response, ClientError> {
        Self::request("POST", url, Some(body.to_string()), None)
    }

    pub fn post_json(url: &str, body: &str) -> Result<Response, ClientError> {
        Self::request(
            "POST",
            url,
            Some(body.to_string()),
            Some(("Content-Type", "application/json")),
        )
    }

    pub fn put(url: &str, body: &str) -> Result<Response, ClientError> {
        Self::request("PUT", url, Some(body.to_string()), None)
    }

    pub fn delete(url: &str) -> Result<Response, ClientError> {
        Self::request("DELETE", url, None, None)
    }

    fn request(
        method: &str,
        url: &str,
        body: Option<String>,
        extra_headers: Option<(&str, &str)>,
    ) -> Result<Response, ClientError> {
        let (host, path) = Self::parse_url(url)?;
        let mut stream = TcpStream::connect(&host)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;

        let mut request = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            method, path, host
        );

        if let Some(ref body) = body {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }

        if let Some((name, value)) = extra_headers {
            request.push_str(&format!("{}: {}\r\n", name, value));
        }

        request.push_str("User-Agent: httpkit/0.1\r\n");
        request.push_str("\r\n");

        if let Some(ref body) = body {
            request.push_str(body);
        }

        stream.write_all(request.as_bytes())?;
        stream.flush()?;

        // Read all response bytes
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => raw.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }

        // Debug: print raw response
        eprintln!("  DEBUG raw response ({} bytes): {:?}", raw.len(), raw);

        // Split on \r\n\r\n
        let sep = b"\r\n\r\n";
        let (head, body_bytes) = if let Some(pos) = raw.windows(4).position(|w| w == sep) {
            (&raw[..pos], &raw[pos + 4..])
        } else {
            // No separator found — try splitting on \n
            if let Some(pos) = raw.iter().position(|&b| b == b'\n') {
                (&raw[..pos], &raw[pos + 1..])
            } else {
                (&raw[..], &[][..])
            }
        };

        // Parse status line: "HTTP/1.1 200 OK\r\n"
        let status_line = std::str::from_utf8(head.split(|&b| b == b'\n').next().unwrap_or(&[]))
            .unwrap_or("");
        eprintln!("  DEBUG status line: {:?}", status_line);

        let status: StatusCode = if status_line.len() > 10 {
            status_line[9..12].parse().unwrap_or(0).into()
        } else {
            StatusCode::InternalServerError
        };

        // Parse headers
        let mut headers: Vec<(String, String)> = Vec::new();

        for line in head.split(|&b| b == b'\n').skip(1) {
            let trimmed: Vec<u8> = line.iter()
                .filter(|&&c| c != b'\r' && c != b'\n')
                .copied()
                .collect();
            let line_str = std::str::from_utf8(&trimmed).unwrap_or("");
            if let Some((name, val)) = line_str.split_once(':') {
                headers.push((name.trim().to_string(), val.trim().to_string()));
            }
        }

        eprintln!("  DEBUG headers: {:?}, body: {} bytes", headers, body_bytes.len());

        Ok(Response {
            status,
            headers,
            body: bytes::Bytes::from(body_bytes.to_vec()),
            keep_alive: false,
        })
    }

    fn parse_url(url: &str) -> Result<(String, String), ClientError> {
        let url = url.strip_prefix("http://").unwrap_or(url);
        let url = url.strip_prefix("https://").unwrap_or(url);

        let (host, path) = if let Some(idx) = url.find('/') {
            (&url[..idx], &url[idx..])
        } else {
            (url, "/")
        };

        let host = if host.contains(':') {
            host.to_string()
        } else {
            format!("{}:80", host)
        };

        Ok((host, path.to_string()))
    }
}

#[derive(Debug)]
pub enum ClientError {
    Io(std::io::Error),
    InvalidUrl(String),
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        ClientError::Io(e)
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "IO error: {}", e),
            ClientError::InvalidUrl(url) => write!(f, "Invalid URL: {}", url),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_url_http() {
        let (host, path) = HttpClient::parse_url("http://example.com/foo/bar").unwrap();
        assert_eq!(host, "example.com:80");
        assert_eq!(path, "/foo/bar");
    }

    #[test]
    fn test_parse_url_with_port() {
        let (host, path) = HttpClient::parse_url("http://localhost:3000/test").unwrap();
        assert_eq!(host, "localhost:3000");
        assert_eq!(path, "/test");
    }

    #[test]
    fn test_parse_url_default_path() {
        let (_host, path) = HttpClient::parse_url("http://example.com").unwrap();
        assert_eq!(path, "/");
    }
}
