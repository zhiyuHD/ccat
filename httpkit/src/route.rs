use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::request::Method;
use crate::response::Response;

/// A request handler is an async function that takes a request and returns a response.
pub type Handler = Arc<dyn Fn(&crate::request::Request) -> Response + Send + Sync>;

/// A parameterized route: method + pattern + handler.
#[derive(Clone)]
pub struct Route {
    pub method: Method,
    pub pattern: String,
    pub param_names: Vec<String>,
    pub handler: Handler,
}

/// Pattern matching for URL paths with parameters.
/// `/users/:id/posts/:postId` → splits into segments
fn parse_pattern(pattern: &str) -> (Vec<String>, Vec<&str>) {
    let segments: Vec<&str> = pattern
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let mut param_names = Vec::new();
    let mut static_segs = Vec::new();

    for seg in &segments {
        if seg.starts_with(':') {
            param_names.push(seg[1..].to_string());
        }
        static_segs.push(*seg);
    }

    (param_names, static_segs)
}

/// Match a request path against a route pattern.
fn match_pattern(
    _param_names: &[String],
    pattern_segs: &[&str],
    request_path: &str,
) -> Option<HashMap<String, String>> {
    let req_segs: Vec<&str> = request_path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    if req_segs.len() != pattern_segs.len() {
        return None;
    }

    let mut map = HashMap::new();
    for (p, r) in pattern_segs.iter().zip(req_segs.iter()) {
        if p.starts_with(':') {
            let key = p[1..].to_string();
            map.insert(key, r.to_string());
        } else if p != r {
            return None;
        }
    }
    Some(map)
}

/// A thread-safe router that matches paths to handlers.
#[derive(Clone, Default)]
pub struct Router {
    inner: Arc<RwLock<Vec<Route>>>,
}

impl Router {
    pub fn new() -> Self {
        Router::default()
    }

    /// Register a handler for a specific method and path.
    pub fn handle<F>(&self, method: Method, path: &str, handler: F)
    where
        F: Fn(&crate::request::Request) -> Response + Send + Sync + 'static,
    {
        let (param_names, _) = parse_pattern(path);
        let handler: Handler = Arc::new(handler);
        let route = Route {
            method,
            pattern: path.to_string(),
            param_names,
            handler,
        };
        tokio::task::block_in_place(|| {
            let mut inner = self.inner.blocking_write();
            inner.push(route);
        });
    }

    pub fn get<F>(&self, path: &str, handler: F)
    where
        F: Fn(&crate::request::Request) -> Response + Send + Sync + 'static,
    {
        self.handle(Method::GET, path, handler);
    }

    pub fn post<F>(&self, path: &str, handler: F)
    where
        F: Fn(&crate::request::Request) -> Response + Send + Sync + 'static,
    {
        self.handle(Method::POST, path, handler);
    }

    pub fn put<F>(&self, path: &str, handler: F)
    where
        F: Fn(&crate::request::Request) -> Response + Send + Sync + 'static,
    {
        self.handle(Method::PUT, path, handler);
    }

    pub fn delete<F>(&self, path: &str, handler: F)
    where
        F: Fn(&crate::request::Request) -> Response + Send + Sync + 'static,
    {
        self.handle(Method::DELETE, path, handler);
    }

    /// Catch-all handler for any method.
    pub fn any<F>(&self, path: &str, handler: F)
    where
        F: Fn(&crate::request::Request) -> Response + Send + Sync + 'static,
    {
        self.handle(Method::Unknown("*".to_string()), path, handler);
    }

    /// Match a request to a handler. Returns (handler, url_params).
    pub async fn match_request(
        &self,
        req: &crate::request::Request,
    ) -> Option<(Handler, HashMap<String, String>)> {
        let inner = self.inner.read().await;

        for route in inner.iter() {
            // Method match: wildcard matches everything
            if !matches!(req.method, Method::Unknown(_)) {
                if route.method != req.method {
                    continue;
                }
            }

            // Path match
            let (param_names, pattern_segs) = parse_pattern(&route.pattern);
            if let Some(params) = match_pattern(&param_names, &pattern_segs, &req.path) {
                let handler = route.handler.clone();
                return Some((handler, params));
            }
        }
        None
    }

    /// Get the list of registered routes (for debugging).
    pub async fn routes(&self) -> Vec<String> {
        let inner = self.inner.read().await;
        inner
            .iter()
            .map(|r| format!("{} {}", r.method, r.pattern))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pattern() {
        let (params, segs) = parse_pattern("/users/:id/posts/:postId");
        assert_eq!(params, vec!["id", "postId"]);
        assert_eq!(segs.len(), 4);
    }

    #[test]
    fn test_match_pattern_exact() {
        let (keys, segs) = parse_pattern("/users/:id");
        let result = match_pattern(&keys, &segs, "/users/42");
        assert!(result.is_some());
        assert_eq!(result.unwrap().get("id"), Some(&"42".to_string()));
    }

    #[test]
    fn test_match_pattern_wrong_length() {
        let (keys, segs) = parse_pattern("/users/:id/posts/:postId");
        let result = match_pattern(&keys, &segs, "/users/42");
        assert!(result.is_none());
    }

    #[test]
    fn test_match_pattern_static_mismatch() {
        let (keys, segs) = parse_pattern("/users/:id");
        let result = match_pattern(&keys, &segs, "/admins/42");
        assert!(result.is_none());
    }

    #[test]
    fn test_router_registration() {
        let router = Router::new();
        router.get("/test", |_req| Response::ok("test"));
        router.post("/data", |_req| Response::ok("data"));

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let routes = rt.block_on(router.routes());
        assert_eq!(routes.len(), 2);
        assert!(routes.iter().any(|r| r == "GET /test"));
        assert!(routes.iter().any(|r| r == "POST /data"));
    }

    #[test]
    fn test_router_param_extraction() {
        let router = Router::new();
        router.get("/users/:id", |req| {
            let uid = req.params.get("id").cloned().unwrap_or_default();
            Response::ok(&format!("user={}", uid))
        });

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let raw = b"GET /users/42 HTTP/1.1\r\n\r\n";
        let req = crate::request::Request::parse(raw).unwrap();
        let result = rt.block_on(router.match_request(&req));

        assert!(result.is_some());
        let (handler, params) = result.unwrap();
        assert_eq!(params.get("id"), Some(&"42".to_string()));
        // The handler expects params to be set on the request
        let mut req_for_handler = req.clone();
        req_for_handler.params = params;
        let resp = handler(&req_for_handler);
        assert_eq!(
            String::from_utf8_lossy(&resp.body),
            "user=42"
        );
    }
}
