use std::fmt;

/// Represents an HTTP method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    GET,
    HEAD,
    POST,
    PUT,
    DELETE,
    PATCH,
    OPTIONS,
    CONNECT,
    TRACE,
    Unknown(String),
}

impl Method {
    pub fn is_safe(&self) -> bool {
        matches!(
            self,
            Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
        )
    }

    pub fn is_idempotent(&self) -> bool {
        matches!(
            self,
            Method::GET
                | Method::HEAD
                | Method::PUT
                | Method::DELETE
                | Method::OPTIONS
                | Method::TRACE
        )
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Method::GET => write!(f, "GET"),
            Method::HEAD => write!(f, "HEAD"),
            Method::POST => write!(f, "POST"),
            Method::PUT => write!(f, "PUT"),
            Method::DELETE => write!(f, "DELETE"),
            Method::PATCH => write!(f, "PATCH"),
            Method::OPTIONS => write!(f, "OPTIONS"),
            Method::CONNECT => write!(f, "CONNECT"),
            Method::TRACE => write!(f, "TRACE"),
            Method::Unknown(s) => write!(f, "{}", s),
        }
    }
}

impl std::str::FromStr for Method {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "GET" => Ok(Method::GET),
            "HEAD" => Ok(Method::HEAD),
            "POST" => Ok(Method::POST),
            "PUT" => Ok(Method::PUT),
            "DELETE" => Ok(Method::DELETE),
            "PATCH" => Ok(Method::PATCH),
            "OPTIONS" => Ok(Method::OPTIONS),
            "CONNECT" => Ok(Method::CONNECT),
            "TRACE" => Ok(Method::TRACE),
            _ => Ok(Method::Unknown(s.to_uppercase())),
        }
    }
}

/// A parsed HTTP request.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub uri: String,
    pub path: String,
    pub query_string: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Parsed query parameters.
    pub params: std::collections::HashMap<String, String>,
}

impl Request {
    /// Parse a raw HTTP request from bytes.
    pub fn parse(raw: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(raw).ok()?;
        let (head, body_part) = text.split_once("\r\n\r\n")?;
        let mut lines = head.lines();

        let parts: Vec<&str> = lines.next()?.splitn(3, ' ').collect();
        if parts.len() < 3 {
            return None;
        }

        let method: Method = parts[0]
            .parse()
            .unwrap_or(Method::Unknown(parts[0].to_string()));
        let version = parts[2].to_string();

        let (path, query_string) = if let Some(idx) = parts[1].find('?') {
            (
                parts[1][..idx].to_string(),
                parts[1][(idx + 1)..].to_string(),
            )
        } else {
            (parts[1].to_string(), String::new())
        };

        let mut headers = Vec::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }

        let mut params = std::collections::HashMap::new();
        if !query_string.is_empty() {
            for pair in query_string.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    params.insert(urldecode(k), urldecode(v));
                } else {
                    params.insert(urldecode(pair), String::new());
                }
            }
        }

        let body = body_part.as_bytes().to_vec();

        Some(Request {
            method,
            uri: parts[1].to_string(),
            path,
            query_string,
            version,
            headers,
            body,
            params,
        })
    }

    /// Get header value by name (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }

    /// Check if content type starts with given MIME prefix.
    pub fn content_type_is(&self, mime: &str) -> bool {
        self.header("content-type")
            .map(|ct| ct.starts_with(mime))
            .unwrap_or(false)
    }

    /// Parse body as UTF-8 string.
    pub fn text_body(&self) -> Option<String> {
        String::from_utf8(self.body.clone()).ok()
    }

    pub fn content_length(&self) -> usize {
        self.header("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.body.len())
    }
}

fn urldecode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_get_request() {
        let raw = b"GET /index.html HTTP/1.1\r\nHost: localhost\r\nUser-Agent: test\r\n\r\n";
        let req = Request::parse(raw).expect("should parse");
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path, "/index.html");
        assert_eq!(req.version, "HTTP/1.1");
        assert_eq!(req.header("Host"), Some("localhost"));
        assert_eq!(req.header("User-Agent"), Some("test"));
    }

    #[test]
    fn test_parse_post_with_body() {
        let body = b"name=alice&age=30";
        let mut req = Vec::new();
        req.extend_from_slice(b"POST /form HTTP/1.1\r\nHost: localhost\r\nContent-Length: ");
        req.extend_from_slice(body.len().to_string().as_bytes());
        req.extend_from_slice(b"\r\n\r\n");
        req.extend_from_slice(body);

        let parsed = Request::parse(&req).expect("should parse");
        assert_eq!(parsed.method, Method::POST);
        assert_eq!(parsed.path, "/form");
        // Body is in parsed.body, not params (params are from query string)
        assert_eq!(parsed.body, body);
        assert_eq!(parsed.text_body(), Some("name=alice&age=30".to_string()));
    }

    #[test]
    fn test_parse_unknown_method() {
        let raw = b"FOOBAR /test HTTP/1.1\r\n\r\n";
        let req = Request::parse(raw).expect("should parse");
        assert!(matches!(req.method, Method::Unknown(_)));
    }

    #[test]
    fn test_header_case_insensitive() {
        let raw = b"GET / HTTP/1.1\r\nContent-Type: text/html\r\n\r\n";
        let req = Request::parse(raw).expect("should parse");
        assert_eq!(req.header("content-type"), Some("text/html"));
        assert_eq!(req.header("CONTENT-TYPE"), Some("text/html"));
        assert_eq!(req.header("Content-Type"), Some("text/html"));
    }

    #[test]
    fn test_query_params() {
        let raw = b"GET /search?q=rust&page=1&limit=10 HTTP/1.1\r\n\r\n";
        let req = Request::parse(raw).expect("should parse");
        assert_eq!(req.params.get("q"), Some(&"rust".to_string()));
        assert_eq!(req.params.get("page"), Some(&"1".to_string()));
        assert_eq!(req.params.get("limit"), Some(&"10".to_string()));
    }

    #[test]
    fn test_url_decode() {
        let raw = b"GET /path?a=%20hello%21&b=world+test HTTP/1.1\r\n\r\n";
        let req = Request::parse(raw).expect("should parse");
        assert_eq!(req.params.get("a"), Some(&" hello!".to_string()));
        assert_eq!(req.params.get("b"), Some(&"world test".to_string()));
    }

    #[test]
    fn test_missing_body_crlf() {
        let raw = b"GET / HTTP/1.1\r\n\r\n";
        let req = Request::parse(raw).expect("should parse");
        assert_eq!(req.body, Vec::<u8>::new());
    }

    #[test]
    fn test_method_properties() {
        assert!(Method::GET.is_safe());
        assert!(Method::GET.is_idempotent());
        assert!(!Method::POST.is_safe());
        assert!(!Method::POST.is_idempotent());
        assert!(Method::PUT.is_idempotent());
    }
}
