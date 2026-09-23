//! A minimal static file server, so serving the example needs nothing
//! installed.
//!
//! Built on hyper's HTTP/1 server, with `async-io` as the reactor and
//! `async-executor` as the runtime: hyper parses and encodes the protocol,
//! the server only maps a request to a file. WebGPU requires a secure
//! context, which `http://localhost` is, so the listener binds to loopback
//! only.

use std::path::{Path, PathBuf};

use async_executor::Executor;
use async_io::Async;
use futures_lite::future;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use smol_hyper::rt::{FuturesIo, SmolTimer};

/// Serve `dir` until stopped.
///
/// Binds to loopback, trying the preferred port first and its successors
/// after, and prints the URL to open once listening.
pub fn serve(dir: &Path, preferred_port: u16) -> Result<(), String> {
    let listener = bind(preferred_port)?;
    let url = listener
        .get_ref()
        .local_addr()
        .map_err(|error| format!("reading the listener's address: {error}"))?;
    println!(
        "serving {} on http://{url}/index.html — Ctrl-C stops the server",
        dir.display()
    );

    let executor = Executor::new();
    future::block_on(executor.run(async {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    eprintln!("accepting a connection: {error}");
                    continue;
                }
            };
            let dir = dir.to_path_buf();
            executor
                .spawn(async move {
                    if let Err(error) = serve_connection(stream, dir).await {
                        eprintln!("{peer}: {error}");
                    }
                })
                .detach();
        }
    }))
}

/// Bind to loopback at `preferred_port` or, if taken, the ports after it.
fn bind(preferred_port: u16) -> Result<Async<std::net::TcpListener>, String> {
    let loopback = std::net::Ipv4Addr::LOCALHOST;
    let last = preferred_port.saturating_add(10);
    (preferred_port..=last)
        .find_map(|port| Async::<std::net::TcpListener>::bind((loopback, port)).ok())
        .ok_or_else(|| format!("no free port in {preferred_port}..={last} on loopback"))
}

/// Answer every request on one connection.
///
/// hyper keeps the connection alive between requests by default, which is
/// what a browser expects; a connection error ends it.
async fn serve_connection(stream: Async<std::net::TcpStream>, dir: PathBuf) -> Result<(), String> {
    // `FuturesIo` bridges `async-io`'s futures-io traits to hyper's own; the
    // timer lets hyper enforce its header-read timeout.
    let service = service_fn(move |request: Request<Incoming>| {
        let dir = dir.clone();
        async move { Ok::<_, std::convert::Infallible>(answer(request, &dir)) }
    });

    http1::Builder::new()
        .timer(SmolTimer::new())
        .serve_connection(FuturesIo::new(stream), service)
        .await
        .map_err(|error| format!("serving the connection: {error}"))
}

/// The response for one request.
fn answer(request: Request<Incoming>, dir: &Path) -> Response<Full<Bytes>> {
    if request.method() != hyper::Method::GET && request.method() != hyper::Method::HEAD {
        return plain(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }

    let Some(path) = resolve(request.uri().path(), dir) else {
        return plain(StatusCode::NOT_FOUND, "not found");
    };

    match std::fs::read(&path) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(hyper::header::CONTENT_TYPE, mime(&path))
            // So a rebuilt wasm is picked up on refresh rather than cached.
            .header(hyper::header::CACHE_CONTROL, "no-store")
            .body(Full::new(Bytes::from(bytes)))
            .expect("the response is well formed"),
        Err(error) => {
            eprintln!("reading {}: {error}", path.display());
            plain(StatusCode::NOT_FOUND, "not found")
        }
    }
}

/// A `text/plain` response, for the statuses that carry no file.
fn plain(status: StatusCode, message: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from_static(message.as_bytes())))
        .expect("the response is well formed")
}

/// The file `target` asks for, or `None` for a request to refuse.
///
/// The root serves the page; anything escaping `dir` (`..`) is refused rather
/// than resolved.
fn resolve(target: &str, dir: &Path) -> Option<PathBuf> {
    if target.contains("..") {
        return None;
    }
    let path = target.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let path = dir.join(path);
    // A directory request resolves to its index, the usual static-server
    // behavior; a missing file stays a 404.
    let path = if path.is_dir() {
        path.join("index.html")
    } else {
        path
    };
    path.is_file().then_some(path)
}

/// The response `Content-Type` for `path`, by extension.
fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        // `text/javascript` is the current RFC; browsers accept the older
        // `application/javascript` too.
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        // Required for `WebAssembly.instantiateStreaming` to work.
        Some("wasm") => "application/wasm",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use super::*;

    /// The directory the test server serves, with one file per case that
    /// matters: the page, a script, and a wasm binary.
    fn fixture() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("unlit3d-http-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the fixture directory is writable");
        std::fs::write(dir.join("index.html"), "<html></html>").expect("index is writable");
        std::fs::write(dir.join("app.js"), "console.log(1)").expect("script is writable");
        std::fs::write(dir.join("mod.wasm"), b"\0asm").expect("wasm is writable");
        dir
    }

    /// Start the server on a free port, returning the base URL.
    fn serve_fixture(dir: PathBuf) -> String {
        // Port 0 is not usable here: the URL has to be known to the client
        // before the server is up. A high port keeps collisions unlikely.
        let port = 8971;
        std::thread::spawn(move || {
            let _ = serve(&dir, port);
        });
        // The listener binds before the accept loop, but not before this
        // thread returns; retry the first connection until it is up.
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        format!("127.0.0.1:{port}")
    }

    /// Send one request and return the whole raw response.
    fn request(addr: &str, method: &str, target: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("the test server is listening");
        write!(
            stream,
            "{method} {target} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
        )
        .expect("the request is written");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("the response is read");
        String::from_utf8_lossy(&response).into_owned()
    }

    /// The status line of a response, e.g. `HTTP/1.1 200 OK`.
    fn status(response: &str) -> &str {
        response
            .lines()
            .next()
            .expect("a response has a status line")
    }

    /// The value of a header, matched case-insensitively.
    fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
        response
            .lines()
            .take_while(|line| !line.is_empty())
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case(name).then(|| value.trim())
            })
    }

    #[test]
    fn serves_files_with_the_mime_a_browser_needs() {
        let addr = serve_fixture(fixture());

        let index = request(&addr, "GET", "/");
        assert_eq!(status(&index), "HTTP/1.1 200 OK");
        assert_eq!(
            header(&index, "content-type"),
            Some("text/html; charset=utf-8")
        );
        assert!(index.ends_with("<html></html>"));

        let script = request(&addr, "GET", "/app.js");
        assert_eq!(
            header(&script, "content-type"),
            Some("text/javascript; charset=utf-8")
        );

        // The one that matters: without it `instantiateStreaming` refuses the
        // module, and the example fails to start.
        let wasm = request(&addr, "GET", "/mod.wasm");
        assert_eq!(header(&wasm, "content-type"), Some("application/wasm"));
    }

    #[test]
    fn refuses_what_is_not_there_and_what_escapes_the_directory() {
        let addr = serve_fixture(fixture());

        assert_eq!(
            status(&request(&addr, "GET", "/missing.html")),
            "HTTP/1.1 404 Not Found"
        );
        // A traversal that escapes the served directory is not resolved.
        assert_eq!(
            status(&request(&addr, "GET", "/../Cargo.toml")),
            "HTTP/1.1 404 Not Found"
        );
        assert_eq!(
            status(&request(&addr, "POST", "/")),
            "HTTP/1.1 405 Method Not Allowed"
        );
    }

    #[test]
    fn head_answers_with_headers_only() {
        let addr = serve_fixture(fixture());
        let response = request(&addr, "HEAD", "/index.html");

        assert_eq!(status(&response), "HTTP/1.1 200 OK");
        assert_eq!(header(&response, "content-length"), Some("13"));
        // The body is suppressed, so nothing follows the blank line.
        let body = response
            .split_once("\r\n\r\n")
            .expect("a response has a body separator")
            .1;
        assert!(body.is_empty(), "HEAD returned a body: {body:?}");
    }
}
