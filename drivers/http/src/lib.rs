//! HTTP driver for edlr.
//!
//! This crate owns the actual networking for the `driver-http` capability:
//! given an already-permission-checked request it performs a single HTTP
//! call and returns a typed result. It deliberately knows nothing about
//! plugins, capability grants, or allowlists -- that decision is made by
//! `core`'s host implementation *before* `HttpDriver::send` is ever called
//! (see `core/src/plugin/host.rs` and `core/src/plugin/allowlist.rs`). This
//! separation keeps the driver a plain, reusable HTTP client and keeps the
//! security-relevant decision in one place.

use std::time::Duration;
use tokio::runtime::Handle;

/// An HTTP request to perform. Mirrors the WIT `driver-http.request` record.
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// A successful HTTP response. Mirrors the WIT `driver-http.response` record.
#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Failure modes this driver can report. `InvalidRequest` covers request
/// construction problems the caller could fix by changing the request
/// (bad method, unparseable URL); everything else -- connect failure, TLS
/// failure, timeout, oversized response body -- is `Transport`.
#[derive(Debug)]
pub enum HttpError {
    InvalidRequest(String),
    Transport(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::InvalidRequest(msg) => write!(f, "invalid request: {msg}"),
            HttpError::Transport(msg) => write!(f, "transport error: {msg}"),
        }
    }
}

impl std::error::Error for HttpError {}

/// Request headers a guest is never allowed to set directly.
///
/// The capability allowlist controls which *connection target* (scheme +
/// host + port) a plugin may reach, not what it claims once connected. An
/// explicit `Host` header lets a plugin approved for one virtual host
/// address a different one behind the same frontend (domain fronting).
/// `Content-Length` / `Transfer-Encoding` / `Connection` are framing-relevant
/// and are the standard request-smuggling primitive; `Upgrade` and
/// `Proxy-*` headers are similarly connection/transport-layer, not
/// legitimate application request content. Comparison is case-insensitive
/// (see `send`).
const FORBIDDEN_REQUEST_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
];

fn is_forbidden_request_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    FORBIDDEN_REQUEST_HEADERS.contains(&lower.as_str()) || lower.starts_with("proxy-")
}

pub struct HttpDriver {
    client: reqwest::Client,
    /// Cap applied symmetrically to request bodies (checked up front in
    /// `send`, rejecting with `InvalidRequest`) and response bodies (checked
    /// while streaming the response, rejecting with `Transport` -- see
    /// `send` for why that one can't be enforced up front).
    max_body_bytes: usize,
    /// デーモンの tokio runtime のハンドル。同期 `send` はこれで `block_on`
    /// する。呼び出し元はプラグイン/ドライバスレッド(非ランタイムスレッド)
    /// である前提 — ランタイムのワーカースレッド上から呼ぶと panic する。
    handle: Handle,
}

impl HttpDriver {
    pub fn new(
        timeout: Duration,
        max_body_bytes: usize,
        handle: Handle,
    ) -> Result<Self, HttpError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| HttpError::Transport(format!("failed to build http client: {e}")))?;

        Ok(HttpDriver {
            client,
            max_body_bytes,
            handle,
        })
    }

    /// Performs `req` and returns the response, or a typed error.
    ///
    /// Response bodies are read chunk by chunk with the size cap enforced
    /// before each chunk is appended: the driver never buffers more than
    /// one chunk past the configured cap regardless of what the server
    /// claims or how it frames the body (`Content-Length`, chunked
    /// transfer, or none at all), so a malicious or misconfigured server
    /// cannot force unbounded memory growth. When the server *does* send a
    /// `Content-Length` header up front that already exceeds the cap, that
    /// is checked first as a fast path so nothing is downloaded at all.
    pub fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        let method: reqwest::Method = req.method.parse().map_err(|e| {
            HttpError::InvalidRequest(format!("invalid method {:?}: {e}", req.method))
        })?;
        let url = reqwest::Url::parse(&req.url)
            .map_err(|e| HttpError::InvalidRequest(format!("invalid url {:?}: {e}", req.url)))?;

        for (name, _) in &req.headers {
            if is_forbidden_request_header(name) {
                return Err(HttpError::InvalidRequest(format!(
                    "header not allowed: {name}"
                )));
            }
        }

        if let Some(body) = &req.body {
            if body.len() > self.max_body_bytes {
                return Err(HttpError::InvalidRequest(format!(
                    "request body of {len} bytes exceeds the {max} byte limit",
                    len = body.len(),
                    max = self.max_body_bytes
                )));
            }
        }

        let mut builder = self.client.request(method, url);
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }

        self.handle.block_on(async {
            let mut response = builder
                .send()
                .await
                .map_err(|e| HttpError::Transport(format!("request failed: {e}")))?;

            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_string(),
                        value.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();

            // Fast path: if the server declared an over-limit length up front,
            // reject without reading anything.
            if let Some(len) = response.content_length() {
                if len > self.max_body_bytes as u64 {
                    return Err(HttpError::Transport(format!(
                        "response body of {len} bytes exceeds the {max} byte limit",
                        max = self.max_body_bytes
                    )));
                }
            }

            // Primary guard: check the cap before appending each chunk, so
            // over-limit bodies fail as soon as they cross the line and we
            // never buffer more than one chunk past the cap, regardless of
            // what (if anything) the server claimed about its length.
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| {
                HttpError::Transport(format!("failed to read response body: {e}"))
            })? {
                if body.len() + chunk.len() > self.max_body_bytes {
                    return Err(HttpError::Transport(format!(
                        "response body exceeds the {max} byte limit",
                        max = self.max_body_bytes
                    )));
                }
                body.extend_from_slice(&chunk);
            }

            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    /// テスト全体で共有する runtime の Handle。関数ローカルで Runtime を
    /// 作って Handle だけ返すと drop された runtime への block_on で panic
    /// するため、static に生かしておく。
    fn test_handle() -> Handle {
        static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        RT.get_or_init(|| tokio::runtime::Runtime::new().expect("build test runtime"))
            .handle()
            .clone()
    }

    fn request(url: &str) -> HttpRequest {
        HttpRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
        }
    }

    /// Reads a minimal HTTP/1.1 request off `stream` (headers + a
    /// `Content-Length`-sized body, if any) and returns the parsed headers
    /// and body. Good enough for the hand-rolled test servers below; not a
    /// general-purpose HTTP parser (no chunked request bodies, no folding).
    fn read_request(stream: &mut TcpStream) -> (Vec<(String, String)>, Vec<u8>) {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            stream.read_exact(&mut byte).expect("read request head");
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&head);
        let mut lines = head.split("\r\n");
        let _request_line = lines.next();
        let headers: Vec<(String, String)> = lines
            .filter(|l| !l.is_empty())
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();

        let content_length: usize = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);

        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            stream.read_exact(&mut body).expect("read request body");
        }
        (headers, body)
    }

    fn write_response(stream: &mut TcpStream, status: u16, headers: &[(&str, &str)], body: &[u8]) {
        let mut resp = format!(
            "HTTP/1.1 {status} status\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (k, v) in headers {
            resp.push_str(&format!("{k}: {v}\r\n"));
        }
        resp.push_str("\r\n");
        stream.write_all(resp.as_bytes()).expect("write head");
        stream.write_all(body).expect("write body");
        stream.flush().expect("flush");
    }

    /// Spawns a server that accepts a single connection, hands the parsed
    /// request to `handler`, and writes back whatever `handler` returns.
    /// Returns the `http://host:port/` URL to hit it at.
    fn spawn_server(
        handler: impl FnOnce(
                Vec<(String, String)>,
                Vec<u8>,
            ) -> (u16, Vec<(&'static str, &'static str)>, Vec<u8>)
            + Send
            + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let (headers, body) = read_request(&mut stream);
                let (status, resp_headers, resp_body) = handler(headers, body);
                write_response(&mut stream, status, &resp_headers, &resp_body);
            }
        });
        format!("http://{addr}/")
    }

    /// Spawns a server that accepts a connection and then never responds
    /// (holds the connection open past any reasonable test timeout), to
    /// exercise `HttpDriver`'s own timeout rather than relying on the
    /// production 10s `HTTP_TIMEOUT` (which would make this test slow).
    fn spawn_stalling_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                std::thread::sleep(Duration::from_secs(5));
                drop(stream);
            }
        });
        format!("http://{addr}/")
    }

    #[test]
    fn timeout_fires_against_a_stalling_handler() {
        let url = spawn_stalling_server();
        // Deliberately short so the test doesn't take anywhere near the
        // production HTTP_TIMEOUT (10s); this exercises the same code path.
        let driver = HttpDriver::new(Duration::from_millis(150), 1024, test_handle()).expect("build driver");

        let started = std::time::Instant::now();
        let err = driver
            .send(request(&url))
            .expect_err("stalling server must time out");

        assert!(matches!(err, HttpError::Transport(_)));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "must fail close to the configured timeout, not the server's 5s stall"
        );
    }

    #[test]
    fn response_body_exactly_at_cap_succeeds() {
        let cap = 16usize;
        let body = vec![b'a'; cap];
        let url = spawn_server(move |_headers, _body| (200, vec![], body.clone()));
        let driver = HttpDriver::new(Duration::from_secs(5), cap, test_handle()).expect("build driver");

        let response = driver.send(request(&url)).expect("body at cap must pass");
        assert_eq!(response.body.len(), cap);
    }

    #[test]
    fn response_body_one_byte_over_cap_fails() {
        let cap = 16usize;
        let body = vec![b'a'; cap + 1];
        let url = spawn_server(move |_headers, _body| (200, vec![], body.clone()));
        let driver = HttpDriver::new(Duration::from_secs(5), cap, test_handle()).expect("build driver");

        let err = driver
            .send(request(&url))
            .expect_err("body one byte over cap must fail");
        assert!(matches!(err, HttpError::Transport(_)));
    }

    #[test]
    fn request_body_exactly_at_cap_succeeds() {
        let cap = 16usize;
        let url = spawn_server(move |_headers, body| {
            assert_eq!(body.len(), cap);
            (200, vec![], vec![])
        });
        let driver = HttpDriver::new(Duration::from_secs(5), cap, test_handle()).expect("build driver");

        let mut req = request(&url);
        req.method = "POST".to_string();
        req.body = Some(vec![b'x'; cap]);

        driver.send(req).expect("request body at cap must pass");
    }

    #[test]
    fn request_body_one_byte_over_cap_fails() {
        let cap = 16usize;
        // No server is spawned: the cap must be enforced before any
        // connection is attempted.
        let driver = HttpDriver::new(Duration::from_secs(5), cap, test_handle()).expect("build driver");

        let mut req = HttpRequest {
            method: "POST".to_string(),
            url: "http://127.0.0.1:1/unreachable".to_string(),
            headers: Vec::new(),
            body: Some(vec![b'x'; cap + 1]),
        };
        req.body = Some(vec![b'x'; cap + 1]);

        let err = driver
            .send(req)
            .expect_err("request body one byte over cap must be rejected");
        assert!(matches!(err, HttpError::InvalidRequest(_)));
    }

    #[test]
    fn header_mapping_round_trips_through_the_server() {
        let url = spawn_server(|headers, _body| {
            let saw_it = headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("x-plugin-test") && v == "hello");
            assert!(
                saw_it,
                "server must observe the request header sent by the driver"
            );
            (200, vec![("x-echo", "world")], vec![])
        });
        let driver = HttpDriver::new(Duration::from_secs(5), 1024, test_handle()).expect("build driver");

        let mut req = request(&url);
        req.headers
            .push(("x-plugin-test".to_string(), "hello".to_string()));

        let response = driver.send(req).expect("round trip should succeed");
        let saw_echo = response
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("x-echo") && v == "world");
        assert!(
            saw_echo,
            "response headers from the server must be preserved"
        );
    }

    #[test]
    fn host_header_is_rejected() {
        let driver = HttpDriver::new(Duration::from_secs(5), 1024, test_handle()).expect("build driver");
        let mut req = request("http://127.0.0.1:1/unreachable");
        req.headers
            .push(("Host".to_string(), "evil.example.com".to_string()));

        let err = driver.send(req).expect_err("Host header must be rejected");
        assert!(
            matches!(err, HttpError::InvalidRequest(msg) if msg.to_lowercase().contains("host"))
        );
    }

    #[test]
    fn content_length_header_is_rejected() {
        let driver = HttpDriver::new(Duration::from_secs(5), 1024, test_handle()).expect("build driver");
        let mut req = request("http://127.0.0.1:1/unreachable");
        req.headers
            .push(("Content-Length".to_string(), "0".to_string()));

        let err = driver
            .send(req)
            .expect_err("Content-Length header must be rejected");
        assert!(matches!(err, HttpError::InvalidRequest(_)));
    }

    #[test]
    fn transfer_encoding_header_is_rejected() {
        let driver = HttpDriver::new(Duration::from_secs(5), 1024, test_handle()).expect("build driver");
        let mut req = request("http://127.0.0.1:1/unreachable");
        req.headers
            .push(("Transfer-Encoding".to_string(), "chunked".to_string()));

        let err = driver
            .send(req)
            .expect_err("Transfer-Encoding header must be rejected");
        assert!(matches!(err, HttpError::InvalidRequest(_)));
    }

    #[test]
    fn connection_header_is_rejected() {
        let driver = HttpDriver::new(Duration::from_secs(5), 1024, test_handle()).expect("build driver");
        let mut req = request("http://127.0.0.1:1/unreachable");
        req.headers
            .push(("Connection".to_string(), "keep-alive".to_string()));

        let err = driver
            .send(req)
            .expect_err("Connection header must be rejected");
        assert!(matches!(err, HttpError::InvalidRequest(_)));
    }

    #[test]
    fn upgrade_header_is_rejected() {
        let driver = HttpDriver::new(Duration::from_secs(5), 1024, test_handle()).expect("build driver");
        let mut req = request("http://127.0.0.1:1/unreachable");
        req.headers
            .push(("Upgrade".to_string(), "websocket".to_string()));

        let err = driver
            .send(req)
            .expect_err("Upgrade header must be rejected");
        assert!(matches!(err, HttpError::InvalidRequest(_)));
    }

    #[test]
    fn proxy_prefixed_header_is_rejected() {
        let driver = HttpDriver::new(Duration::from_secs(5), 1024, test_handle()).expect("build driver");
        let mut req = request("http://127.0.0.1:1/unreachable");
        req.headers.push((
            "Proxy-Authorization".to_string(),
            "Basic deadbeef".to_string(),
        ));

        let err = driver
            .send(req)
            .expect_err("Proxy-* header must be rejected");
        assert!(matches!(err, HttpError::InvalidRequest(_)));
    }

    #[test]
    fn header_rejection_is_case_insensitive() {
        let driver = HttpDriver::new(Duration::from_secs(5), 1024, test_handle()).expect("build driver");
        let mut req = request("http://127.0.0.1:1/unreachable");
        req.headers
            .push(("hOsT".to_string(), "evil.example.com".to_string()));

        let err = driver
            .send(req)
            .expect_err("header rejection must be case-insensitive");
        assert!(matches!(err, HttpError::InvalidRequest(_)));
    }
}
