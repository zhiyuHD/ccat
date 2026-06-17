use bytes::Bytes;
use std::fmt;

/// HTTP status codes.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Continue = 100,
    SwitchingProtocols = 101,
    Ok = 200,
    Created = 201,
    Accepted = 202,
    NoContent = 204,
    MovedPermanently = 301,
    Found = 302,
    BadRequest = 400,
    Unauthorized = 401,
    Forbidden = 403,
    NotFound = 404,
    MethodNotAllowed = 405,
    RequestTimeout = 408,
    Conflict = 409,
    NotImplemented = 501,
    InternalServerError = 500,
    BadGateway = 502,
    ServiceUnavailable = 503,
    Unknown(u16),
}

impl StatusCode {
    pub fn reason_phrase(&self) -> &'static str {
        match self {
            StatusCode::Continue => "Continue",
            StatusCode::SwitchingProtocols => "Switching Protocols",
            StatusCode::Ok => "OK",
            StatusCode::Created => "Created",
            StatusCode::Accepted => "Accepted",
            StatusCode::NoContent => "No Content",
            StatusCode::MovedPermanently => "Moved Permanently",
            StatusCode::Found => "Found",
            StatusCode::BadRequest => "Bad Request",
            StatusCode::Unauthorized => "Unauthorized",
            StatusCode::Forbidden => "Forbidden",
            StatusCode::NotFound => "Not Found",
            StatusCode::MethodNotAllowed => "Method Not Allowed",
            StatusCode::RequestTimeout => "Request Timeout",
            StatusCode::Conflict => "Conflict",
            StatusCode::InternalServerError => "Internal Server Error",
            StatusCode::BadGateway => "Bad Gateway",
            StatusCode::ServiceUnavailable => "Service Unavailable",
            StatusCode::NotImplemented => "Not Implemented",
            StatusCode::Unknown(_) => "Unknown",
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(
            self,
            StatusCode::Ok | StatusCode::Created | StatusCode::Accepted | StatusCode::NoContent
        )
    }

    pub fn is_informational(&self) -> bool {
        self.as_u16() >= 100 && self.as_u16() < 200
    }

    pub fn as_u16(&self) -> u16 {
        match self {
            StatusCode::Continue => 100,
            StatusCode::SwitchingProtocols => 101,
            StatusCode::Ok => 200,
            StatusCode::Created => 201,
            StatusCode::Accepted => 202,
            StatusCode::NoContent => 204,
            StatusCode::MovedPermanently => 301,
            StatusCode::Found => 302,
            StatusCode::BadRequest => 400,
            StatusCode::Unauthorized => 401,
            StatusCode::Forbidden => 403,
            StatusCode::NotFound => 404,
            StatusCode::MethodNotAllowed => 405,
            StatusCode::RequestTimeout => 408,
            StatusCode::Conflict => 409,
            StatusCode::InternalServerError => 500,
            StatusCode::BadGateway => 502,
            StatusCode::ServiceUnavailable => 503,
            StatusCode::NotImplemented => 501,
            StatusCode::Unknown(code) => *code,
        }
    }
}

impl fmt::Display for StatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.as_u16(), self.reason_phrase())
    }
}

impl std::str::FromStr for StatusCode {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "100" => Ok(StatusCode::Continue),
            "101" => Ok(StatusCode::SwitchingProtocols),
            "200" => Ok(StatusCode::Ok),
            "201" => Ok(StatusCode::Created),
            "202" => Ok(StatusCode::Accepted),
            "204" => Ok(StatusCode::NoContent),
            "301" => Ok(StatusCode::MovedPermanently),
            "302" => Ok(StatusCode::Found),
            "400" => Ok(StatusCode::BadRequest),
            "401" => Ok(StatusCode::Unauthorized),
            "403" => Ok(StatusCode::Forbidden),
            "404" => Ok(StatusCode::NotFound),
            "405" => Ok(StatusCode::MethodNotAllowed),
            "408" => Ok(StatusCode::RequestTimeout),
            "409" => Ok(StatusCode::Conflict),
            "500" => Ok(StatusCode::InternalServerError),
            "501" => Ok(StatusCode::NotImplemented),
            "502" => Ok(StatusCode::BadGateway),
            "503" => Ok(StatusCode::ServiceUnavailable),
            _ => s.parse::<u16>().map(StatusCode::Unknown).map_err(|_| ()),
        }
    }
}

impl From<u16> for StatusCode {
    fn from(code: u16) -> Self {
        match code {
            100 => StatusCode::Continue,
            101 => StatusCode::SwitchingProtocols,
            200 => StatusCode::Ok,
            201 => StatusCode::Created,
            202 => StatusCode::Accepted,
            204 => StatusCode::NoContent,
            301 => StatusCode::MovedPermanently,
            302 => StatusCode::Found,
            400 => StatusCode::BadRequest,
            401 => StatusCode::Unauthorized,
            403 => StatusCode::Forbidden,
            404 => StatusCode::NotFound,
            405 => StatusCode::MethodNotAllowed,
            408 => StatusCode::RequestTimeout,
            409 => StatusCode::Conflict,
            500 => StatusCode::InternalServerError,
            501 => StatusCode::NotImplemented,
            502 => StatusCode::BadGateway,
            503 => StatusCode::ServiceUnavailable,
            _ => StatusCode::Unknown(code),
        }
    }
}

/// Builder for constructing HTTP responses.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: StatusCode,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
    pub keep_alive: bool,
}

impl Response {
    pub fn new(status: StatusCode) -> Self {
        Response {
            status,
            headers: Vec::new(),
            body: Bytes::new(),
            keep_alive: true,
        }
    }

    /// Create a 200 OK with text body.
    pub fn ok(text: &str) -> Self {
        let mut r = Response::new(StatusCode::Ok);
        r.set_text(text);
        r
    }

    /// Create a 200 OK with JSON body.
    pub fn json(data: &str) -> Self {
        let mut r = Response::new(StatusCode::Ok);
        r.headers
            .push(("Content-Type".to_string(), "application/json".to_string()));
        r.body = Bytes::from(data.to_string());
        r.set_content_length();
        r
    }

    /// Create a 404 Not Found.
    pub fn not_found(text: &str) -> Self {
        let mut r = Response::new(StatusCode::NotFound);
        r.set_text(text);
        r
    }

    /// Create a 500 Internal Server Error.
    pub fn internal_error(text: &str) -> Self {
        let mut r = Response::new(StatusCode::InternalServerError);
        r.set_text(text);
        r
    }

    /// Create a redirect response.
    pub fn redirect(location: &str) -> Self {
        let mut r = Response::new(StatusCode::Found);
        r.headers
            .push(("Location".to_string(), location.to_string()));
        r.keep_alive = false;
        r
    }

    /// Set body as plain text.
    pub fn set_text(&mut self, text: &str) {
        self.body = Bytes::from(text.to_string());
        self.headers.push((
            "Content-Type".to_string(),
            "text/plain; charset=utf-8".to_string(),
        ));
        self.set_content_length();
    }

    /// Add a header.
    pub fn set_header(&mut self, name: &str, value: &str) {
        self.headers.push((name.to_string(), value.to_string()));
    }

    /// Set body and compute content length.
    pub fn set_body(&mut self, body: Bytes) {
        self.body = body;
        self.set_content_length();
    }

    fn set_content_length(&mut self) {
        self.headers.retain(|(k, _)| k != "Content-Length");
        self.headers
            .push(("Content-Length".to_string(), self.body.len().to_string()));
    }

    /// Serialize to HTTP/1.1 response bytes.
    pub fn serialize(&self) -> Bytes {
        let mut response = Vec::new();

        response.extend_from_slice(b"HTTP/1.1 ");
        response.extend_from_slice(self.status.as_u16().to_string().as_bytes());
        response.extend_from_slice(b" ");
        response.extend_from_slice(self.status.reason_phrase().as_bytes());
        response.extend_from_slice(b"\r\n");

        if !self.keep_alive {
            response.extend_from_slice(b"Connection: close\r\n");
        }

        response.extend_from_slice(b"Date: ");
        response.extend_from_slice(&chrono::Utc::now().to_rfc2822().as_bytes());
        response.extend_from_slice(b"\r\n");

        for (name, value) in &self.headers {
            response.extend_from_slice(name.as_bytes());
            response.extend_from_slice(b": ");
            response.extend_from_slice(value.as_bytes());
            response.extend_from_slice(b"\r\n");
        }

        response.extend_from_slice(b"\r\n");
        response.extend_from_slice(&self.body);

        Bytes::from(response)
    }
}

impl fmt::Display for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "HTTP/1.1 {}", self.status)?;
        for (name, value) in &self.headers {
            writeln!(f, "{}: {}", name, value)?;
        }
        writeln!(f)?;
        write!(f, "{}", String::from_utf8_lossy(&self.body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_codes() {
        assert_eq!(StatusCode::Ok.as_u16(), 200);
        assert_eq!(StatusCode::NotFound.as_u16(), 404);
        assert_eq!(StatusCode::InternalServerError.as_u16(), 500);
        assert!(StatusCode::Ok.is_success());
        assert!(!StatusCode::NotFound.is_success());
    }

    #[test]
    fn test_response_serialization() {
        let resp = Response::ok("Hello, World!");
        let serialized = resp.serialize();
        let text = String::from_utf8_lossy(&serialized);
        assert!(text.contains("HTTP/1.1 200 OK"));
        assert!(text.contains("Hello, World!"));
        assert!(text.contains("Content-Length: 13"));
        assert!(text.contains("Content-Type: text/plain"));
    }

    #[test]
    fn test_json_response() {
        let resp = Response::json(r#"{"status":"ok"}"#);
        let serialized = resp.serialize();
        let text = String::from_utf8_lossy(&serialized);
        assert!(text.contains("Content-Type: application/json"));
        assert!(text.contains(r#"{"status":"ok"}"#));
    }

    #[test]
    fn test_redirect_response() {
        let resp = Response::redirect("/new-location");
        let serialized = resp.serialize();
        let text = String::from_utf8_lossy(&serialized);
        assert!(text.contains("HTTP/1.1 302 Found"));
        assert!(text.contains("Location: /new-location"));
    }

    #[test]
    fn test_status_from_str() {
        let code: StatusCode = "404".parse().unwrap();
        assert_eq!(code, StatusCode::NotFound);
        let code: StatusCode = "999".parse().unwrap();
        assert!(matches!(code, StatusCode::Unknown(999)));
    }

    #[test]
    fn test_no_content() {
        let resp = Response::new(StatusCode::NoContent);
        let serialized = resp.serialize();
        let text = String::from_utf8_lossy(&serialized);
        assert!(text.contains("204 No Content"));
        assert!(!text.contains("Content-Length"));
    }
}
