//! Demo: a minimal HTTP API server with routing and middleware.

use httpkit::{HttpServer, Router, Response};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = Router::new();

    router.get("/", |_req| {
        Response::ok("Welcome to httpkit!\n\nEndpoints:\n  GET  /health           — health check\n  GET  /api/users         — list users\n  GET  /api/users/:id     — get user by id\n  POST /api/echo          — echo body back")
    });

    router.get("/api/users", |_req| {
        Response::json(r#"{"users":[{"id":"1","name":"Alice"},{"id":"2","name":"Bob"}],"total":2}"#)
    });

    router.get("/api/users/:id", |req| {
        let id = req.params.get("id").cloned().unwrap_or_default();
        Response::json(&format!(r#"{{"id":"{}","name":"User {}","email":"user{}@example.com"}}"#, id, id, id))
    });

    router.post("/api/echo", |req| {
        let body = req.text_body().unwrap_or_default();
        Response::json(&format!(r#"{{"echo":"{}","method":"POST"}}"#,
            body.chars().take(80).collect::<String>()))
    });

    router.get("/health", |_req| {
        Response::json(r#"{"status":"ok"}"#)
    });

    let server = HttpServer::new();
    let handle = server.listen("127.0.0.1:18080").await?;
    let addr = handle.addr();

    println!("\n🌐 httpkit demo server");
    println!("   http://{}\n", addr);

    // Start the accept loop in the background
    let serve_handle = tokio::spawn(handle.serve());

    // Give the server a moment to bind
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Self-test
    println!("🧪 Running self-test...\n");
    run_self_test(&addr).await;

    println!("\n✅ Server running. Ctrl+C to stop.\n");

    // Wait for serve to complete (until Ctrl+C)
    serve_handle.await??;
    Ok(())
}

async fn run_self_test(addr: &std::net::SocketAddr) {
    use httpkit::HttpClient;

    let base = format!("http://localhost:{}", addr.port());

    let tests: Vec<(&str, String)> = vec![
        ("GET /", format!("{}/", base)),
        ("GET /health", format!("{}/health", base)),
        ("GET /api/users", format!("{}/api/users", base)),
        ("GET /api/users/42", format!("{}/api/users/42", base)),
        ("GET /nonexistent (404)", format!("{}/nonexistent", base)),
    ];

    let mut passed = 0;
    let mut failed = 0;

    for (label, url) in &tests {
        match HttpClient::get(url) {
            Ok(r) => {
                let body = String::from_utf8_lossy(&r.body);
                let preview = if body.len() > 60 {
                    format!("{}...", &body[..60])
                } else {
                    body.to_string()
                };
                println!("  ✓ {} → {} [{} bytes]", label, r.status.as_u16(), r.body.len());
                println!("    {}", preview);
                passed += 1;
            }
            Err(e) => {
                println!("  ✗ {} → ERROR: {}", label, e);
                failed += 1;
            }
        }
    }

    // POST test
    match HttpClient::post(&format!("{}/api/echo", base), "Hello from self-test!") {
        Ok(r) => {
            let body = String::from_utf8_lossy(&r.body);
            println!("  ✓ POST /api/echo → {} [{} bytes]", r.status.as_u16(), r.body.len());
            println!("    {}", body.chars().take(60).collect::<String>());
            passed += 1;
        }
        Err(e) => {
            println!("  ✗ POST /api/echo → ERROR: {}", e);
            failed += 1;
        }
    }

    println!("\n  Results: {} passed, {} failed", passed, failed);
}
