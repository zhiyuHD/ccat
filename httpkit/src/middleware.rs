use std::sync::Arc;
use std::pin::Pin;
use std::future::Future;
use crate::request::Request;
use crate::response::Response;

/// Middleware trait for intercepting requests/responses.
pub trait Middleware: Send + Sync {
    /// Called before routing. Return (modified_req, should_skip).
    fn pre_process<'a>(
        &'a self,
        req: &'a Request,
    ) -> Pin<Box<dyn Future<Output = (&'a Request, bool)> + Send + 'a>>;

    /// Called after routing, before sending response.
    fn post_process<'a>(
        &'a self,
        resp: &'a Response,
    ) -> Pin<Box<dyn Future<Output = bytes::Bytes> + Send + 'a>>;
}

/// Built-in logger middleware.
#[derive(Debug)]
pub struct Logger;

impl Logger {
    pub fn new() -> Self {
        Logger
    }
}

impl Default for Logger {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for Logger {
    fn pre_process<'a>(
        &'a self,
        req: &'a Request,
    ) -> Pin<Box<dyn Future<Output = (&'a Request, bool)> + Send + 'a>> {
        let method = req.method.to_string();
        let path = req.path.clone();
        let version = req.version.clone();
        Box::pin(async move {
            println!("  [{}] {} {}", method, path, version);
            (req, false)
        })
    }

    fn post_process<'a>(
        &'a self,
        resp: &'a Response,
    ) -> Pin<Box<dyn Future<Output = bytes::Bytes> + Send + 'a>> {
        let status = resp.status.as_u16();
        let body_len = resp.body.len();
        let body_preview = if body_len < 200 {
            String::from_utf8_lossy(&resp.body).to_string()
        } else {
            format!("<{} bytes>", body_len)
        };
        let marker = if status >= 400 { "⚠️" } else { "✓" };
        Box::pin(async move {
            println!("  [{}] {} {} ({}) {}", status, resp.status.reason_phrase(), body_len, body_preview, marker);
            resp.serialize()
        })
    }
}

/// Middleware chain for composing multiple middlewares.
#[derive(Clone, Default)]
pub struct MiddlewareChain {
    middlewares: Arc<Vec<Arc<dyn Middleware>>>,
}

impl MiddlewareChain {
    pub fn new() -> Self {
        MiddlewareChain {
            middlewares: Arc::new(Vec::new()),
        }
    }

    pub fn add<M: Middleware + 'static>(&mut self, mw: M) -> &mut Self {
        Arc::get_mut(&mut self.middlewares)
            .unwrap()
            .push(Arc::new(mw));
        self
    }

    pub async fn pre_process<'a>(
        &'a self,
        req: &'a Request,
    ) -> (&'a Request, bool) {
        let mut current = req;
        let mut skip = false;

        for mw in self.middlewares.iter() {
            if skip {
                break;
            }
            let (r, s) = mw.pre_process(current).await;
            current = r;
            skip = s;
        }

        (current, skip)
    }

    pub async fn post_process<'a>(
        &'a self,
        resp: &'a Response,
    ) -> bytes::Bytes {
        let result = resp.serialize();
        for mw in self.middlewares.iter() {
            let _ = mw.post_process(resp).await;
            // Logger middleware prints but doesn't modify response
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logger_creation() {
        let logger = Logger::new();
        assert_eq!(format!("{:?}", logger), "Logger");
    }

    #[test]
    fn test_middleware_chain() {
        let mut chain = MiddlewareChain::new();
        chain.add(Logger::new());
        assert_eq!(chain.middlewares.len(), 1);
    }
}
