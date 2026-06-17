/// Minimal HTTP/1.1 server & client toolkit.
///
/// No heavy frameworks. Just tokio + manual protocol parsing.
/// Built for learning and lightweight use cases.

pub mod request;
pub mod response;
pub mod route;
pub mod server;
pub mod middleware;
pub mod client;
pub mod util;

pub use request::Request;
pub use response::Response;
pub use route::{Router, Handler};
pub use server::HttpServer;
pub use middleware::{Middleware, MiddlewareChain, Logger};
pub use client::HttpClient;
