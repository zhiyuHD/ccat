use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use crate::route::Router;
use std::net::SocketAddr;

/// A minimal HTTP server.
pub struct HttpServer {
    router: Router,
    addr: SocketAddr,
}

impl HttpServer {
    pub fn new() -> Self {
        HttpServer {
            router: Router::new(),
            addr: ([127, 0, 0, 1], 0).into(),
        }
    }

    pub async fn listen(mut self, addr: &str) -> Result<ListenHandle, std::io::Error> {
        let listener = TcpListener::bind(addr).await?;
        self.addr = listener.local_addr()?;
        Ok(ListenHandle {
            listener,
            router: self.router,
            addr: self.addr,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn router(&self) -> &Router {
        &self.router
    }
}

impl Default for HttpServer {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ListenHandle {
    listener: TcpListener,
    router: Router,
    addr: SocketAddr,
}

impl ListenHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn serve(self) -> Result<(), std::io::Error> {
        let welcome = format!(
            "\n\
             ╔══════════════════════════════════════╗\n\
             ║     🌐 httpkit HTTP Server           ║\n\
             ║     Listening on http://{}            ║\n\
             ╚══════════════════════════════════════╝",
            self.addr
        );
        println!("{}", welcome);

        loop {
            let (socket, peer_addr) = self.listener.accept().await?;
            let router = self.router.clone();

            tokio::spawn(async move {
                println!("  ← Connection from {}", peer_addr);
                if let Err(e) = handle_connection(socket, router).await {
                    eprintln!("  ✗ Error handling {}: {}", peer_addr, e);
                }
            });
        }
    }
}

async fn handle_connection(
    mut socket: tokio::net::TcpStream,
    router: Router,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = [0u8; 65536];
    let mut nread = 0;

    loop {
        let n = socket.read(&mut buf[nread..]).await?;
        if n == 0 {
            return Ok(());
        }
        nread += n;

        if nread >= 4 && &buf[nread - 4..nread] == b"\r\n\r\n" {
            break;
        }

        if nread >= buf.len() {
            let mut resp = crate::response::Response::new(
                crate::response::StatusCode::BadRequest
            );
            resp.set_text("Request too large");
            socket.write_all(&resp.serialize()).await?;
            return Ok(());
        }
    }

    let req = match crate::request::Request::parse(&buf[..nread]) {
        Some(r) => r,
        None => {
            let mut resp = crate::response::Response::new(
                crate::response::StatusCode::BadRequest
            );
            resp.set_text("Bad Request");
            socket.write_all(&resp.serialize()).await?;
            return Ok(());
        }
    };

    println!("  [{}] {} {}", req.method, req.path, req.version);

    let handler_result = router.match_request(&req).await;

    let response = match handler_result {
        Some((handler, params)) => {
            let mut req_clone = req.clone();
            req_clone.params = params;
            handler(&req_clone)
        }
        None => {
            crate::response::Response::not_found("404 Not Found")
        }
    };

    let resp_bytes = response.serialize();
    println!("  → Sending {} bytes (status {})", resp_bytes.len(), response.status.as_u16());

    socket.write_all(&resp_bytes).await?;
    socket.shutdown().await?; // Close the connection

    Ok(())
}
