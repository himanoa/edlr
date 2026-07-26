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

use std::io::Read;
use std::time::Duration;

/// An HTTP request to perform. Mirrors the WIT `driver-http.request` record.
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// A successful HTTP response. Mirrors the WIT `driver-http.response` record.
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

/// A reusable HTTP client with a fixed timeout, response body cap, and no
/// automatic redirect following. Construction is comparatively expensive
/// (it builds a `reqwest::blocking::Client`, which sets up TLS state and a
/// background runtime), so callers should build one `HttpDriver` and reuse
/// it across calls rather than constructing one per request.
pub struct HttpDriver {
    client: reqwest::blocking::Client,
    max_body_bytes: usize,
}

impl HttpDriver {
    /// Builds a driver that applies `timeout` to every request (covering
    /// connect + the full response) and caps response bodies at
    /// `max_body_bytes`. Redirects are never followed: `send` returns
    /// whatever 3xx the server sent, untouched, so the permission-checked
    /// URL a caller asked for is exactly the URL that gets fetched.
    ///
    /// `reqwest::blocking::Client::builder().build()` internally spins up
    /// its own background Tokio runtime and, while doing so, briefly parks
    /// the *calling* thread on a throwaway helper runtime of its own. If
    /// the calling thread already belongs to an existing Tokio runtime
    /// (e.g. this is called from `#[tokio::main]`, as `edlr`'s own `main`
    /// does, or from a `#[tokio::test]`), that nested runtime construction
    /// panics ("Cannot drop a runtime in a context where blocking is not
    /// allowed"). Building the client on a plain, freshly spawned
    /// `std::thread` -- which is never entered into any Tokio runtime --
    /// sidesteps this entirely, regardless of what kind of thread `new` is
    /// called from.
    pub fn new(timeout: Duration, max_body_bytes: usize) -> Result<Self, HttpError> {
        let client = std::thread::spawn(move || {
            reqwest::blocking::Client::builder()
                .timeout(timeout)
                .redirect(reqwest::redirect::Policy::none())
                .build()
        })
        .join()
        .map_err(|_| HttpError::Transport("http client builder thread panicked".to_string()))?
        .map_err(|e| HttpError::Transport(format!("failed to build http client: {e}")))?;

        Ok(HttpDriver {
            client,
            max_body_bytes,
        })
    }

    /// Performs `req` and returns the response, or a typed error.
    ///
    /// Response bodies are read via a size-capped [`Read`] adapter
    /// (`Read::take(max_body_bytes + 1)`): the driver never buffers more
    /// than one byte past the configured cap regardless of what the server
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

        let mut builder = self.client.request(method, url);
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }

        let response = builder
            .send()
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

        // Primary guard: cap the actual read at max_body_bytes + 1 so we can
        // tell "exactly at the limit" apart from "over the limit" without
        // ever buffering more than one byte past the cap, regardless of
        // what (if anything) the server claimed about its length.
        let cap = self.max_body_bytes as u64 + 1;
        let mut body = Vec::new();
        response
            .take(cap)
            .read_to_end(&mut body)
            .map_err(|e| HttpError::Transport(format!("failed to read response body: {e}")))?;

        if body.len() > self.max_body_bytes {
            return Err(HttpError::Transport(format!(
                "response body exceeds the {max} byte limit",
                max = self.max_body_bytes
            )));
        }

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}
