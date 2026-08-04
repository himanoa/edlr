//! Integration tests for the real `driver-http` networking wired up in
//! `core/src/plugin/host.rs`. These exercise `HostCtx::send` directly (the
//! same WIT-facing entry point a plugin's `driver-http.send` call goes
//! through), against a real local HTTP server, without going through wasm --
//! matching the style of `host.rs`'s own unit tests, which the module docs
//! explain is deliberate: the permission decision and the driver dispatch
//! are both made from `HostCtx` alone, so a direct call exercises exactly
//! the same path a real guest call would.

use edlr_core::host::plugin::{
    capabilities_json_string, HostCtx, WitDriverHttpHost as _, WitHttpError, WitHttpRequest,
    FS_LIST_LIMIT, FS_READ_LIMIT, HTTP_MAX_BODY, HTTP_TIMEOUT,
};
use edlr_driver_http::HttpDriver;
use std::net::TcpListener as StdTcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;

/// Starts the test axum app on `127.0.0.1:0` on a dedicated OS thread with
/// its own single-threaded tokio runtime, and returns its base URL
/// (`http://127.0.0.1:PORT`).
///
/// Running the server on its own thread/runtime (rather than inside a
/// `#[tokio::test]` runtime shared with the test body) matters here: the
/// driver under test blocks on its own runtime handle (`test_handle()`), and
/// the tests below call it synchronously from a plain `#[test]` function.
/// Keeping the server fully independent avoids any risk of the blocking call
/// and the server sharing (and contending for) the same runtime thread.
fn spawn_test_server() -> String {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("local_addr");
    listener.set_nonblocking(true).expect("set_nonblocking");

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build test server runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("adopt listener");
            axum::serve(listener, app()).await.expect("serve");
        });
    });

    format!("http://{addr}")
}

async fn echo_get() -> impl IntoResponse {
    (
        [(header::HeaderName::from_static("x-server"), "edlr-test")],
        "hello from GET",
    )
}

async fn echo_post(headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    let echoed = headers
        .get("x-echo")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    (
        [(header::HeaderName::from_static("x-echo-received"), echoed)],
        body,
    )
}

async fn redirect() -> impl IntoResponse {
    (StatusCode::FOUND, [(header::LOCATION, "/echo-get")], ())
}

/// Body far bigger than the small cap the oversized-body test configures its
/// driver with, but still tiny in absolute terms so the test is fast.
async fn big() -> Vec<u8> {
    vec![b'x'; 10_000]
}

fn app() -> Router {
    Router::new()
        .route("/echo-get", get(echo_get))
        .route("/echo-post", post(echo_post))
        .route("/redirect", get(redirect))
        .route("/big", get(big))
}

/// Builds a `HostCtx` with the given driver and effective allowlisted hosts,
/// mirroring how `runner.rs` builds one for a real plugin (minus settings,
/// which these tests don't touch).
fn ctx_with_driver(driver: Arc<HttpDriver>, hosts: &[&str]) -> HostCtx {
    let hosts: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
    let capabilities_json = capabilities_json_string(&hosts);
    HostCtx::new(
        "test-plugin".to_string(),
        Arc::new(Mutex::new("{}".to_string())),
        Arc::new(Mutex::new(capabilities_json)),
        Arc::new(Mutex::new("[]".to_string())),
        Arc::new(Mutex::new("[]".to_string())),
        Arc::new(Mutex::new("[]".to_string())),
        edlr_driver_channel::Bus::new(),
        driver,
        Arc::new(edlr_driver_process::ProcessDriver::new(
            Duration::from_secs(3),
            Duration::from_secs(1),
        )),
        Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT)),
    )
}

/// テスト全体で共有する runtime の Handle。`HttpDriver` の同期 `send` は
/// この runtime で `block_on` する。テスト本体は plain `#[test]`(非ランタイム
/// スレッド)なので合法。関数ローカルの Runtime だと drop 後の `block_on` で
/// panic するため static に生かす。
fn test_handle() -> tokio::runtime::Handle {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().expect("build test runtime"))
        .handle()
        .clone()
}

fn default_driver() -> Arc<HttpDriver> {
    Arc::new(
        HttpDriver::new(HTTP_TIMEOUT, HTTP_MAX_BODY, test_handle())
            .expect("build default http driver"),
    )
}

fn request(
    method: &str,
    url: &str,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
) -> WitHttpRequest {
    WitHttpRequest {
        method: method.to_string(),
        url: url.to_string(),
        headers,
        body,
    }
}

/// Case 1: allowed host, GET -> 200 + body.
#[test]
fn allowed_get_returns_status_and_body() {
    let base = spawn_test_server();
    let url = format!("{base}/echo-get");
    let mut ctx = ctx_with_driver(default_driver(), &[base.as_str()]);

    let response = ctx
        .send(request("GET", &url, Vec::new(), None))
        .expect("allowed GET should succeed");

    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"hello from GET");
    assert!(response
        .headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("x-server") && v == "edlr-test"));
}

/// Case 2: POST round-trips both body and headers.
#[test]
fn allowed_post_round_trips_body_and_headers() {
    let base = spawn_test_server();
    let url = format!("{base}/echo-post");
    let mut ctx = ctx_with_driver(default_driver(), &[base.as_str()]);

    let response = ctx
        .send(request(
            "POST",
            &url,
            vec![("x-echo".to_string(), "round-trip-me".to_string())],
            Some(b"request body payload".to_vec()),
        ))
        .expect("allowed POST should succeed");

    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"request body payload");
    assert!(response
        .headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("x-echo-received") && v == "round-trip-me"));
}

/// Case 3: a host outside the grant's allowlist is denied without ever
/// connecting. No server is started for the target host at all -- it's a
/// non-routable TEST-NET-1 address (RFC 5737), so if the driver *did* try to
/// connect, the call would hang until the driver's timeout instead of
/// returning promptly. Asserting a tight wall-clock bound is the proof that
/// no connection was attempted.
#[test]
fn disallowed_host_is_denied_without_connecting() {
    let mut ctx = ctx_with_driver(default_driver(), &["https://allowed.example.com"]);

    let start = Instant::now();
    let err = ctx
        .send(request("GET", "https://192.0.2.1/", Vec::new(), None))
        .expect_err("disallowed host must be denied");
    let elapsed = start.elapsed();

    assert!(matches!(err, WitHttpError::PermissionDenied(_)));
    assert!(
        elapsed < Duration::from_millis(500),
        "denial took {elapsed:?}; a real connection attempt to a non-routable \
         address would not fail this fast, so this suggests the driver was \
         invoked instead of being short-circuited by the allowlist check"
    );
}

/// Case 4: a 3xx response is returned as-is, not followed.
#[test]
fn redirect_is_returned_without_following() {
    let base = spawn_test_server();
    let url = format!("{base}/redirect");
    let mut ctx = ctx_with_driver(default_driver(), &[base.as_str()]);

    let response = ctx
        .send(request("GET", &url, Vec::new(), None))
        .expect("redirect response should still be a successful driver call");

    assert_eq!(response.status, 302);
    assert!(response
        .headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("location") && v == "/echo-get"));
}

/// Case 5: a response body over the configured cap is a transport error.
#[test]
fn oversized_body_is_a_transport_error() {
    let base = spawn_test_server();
    let url = format!("{base}/big");
    // Small cap so the test doesn't need to move megabytes to prove the
    // point; the `/big` handler serves 10_000 bytes, well over this.
    let small_cap_driver =
        Arc::new(HttpDriver::new(HTTP_TIMEOUT, 1024, test_handle()).expect("build small-cap http driver"));
    let mut ctx = ctx_with_driver(small_cap_driver, &[base.as_str()]);

    let err = ctx
        .send(request("GET", &url, Vec::new(), None))
        .expect_err("oversized body should be rejected");

    assert!(matches!(err, WitHttpError::Transport(_)));
}

/// Case 6: an unreachable address is a transport error, not a panic.
/// The listener is bound (to get a free, syntactically valid address) and
/// immediately dropped, so the port is guaranteed to refuse connections.
#[test]
fn unreachable_address_is_a_transport_error() {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind throwaway listener");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);

    let url = format!("http://{addr}/");
    let mut ctx = ctx_with_driver(default_driver(), &[format!("http://{addr}").as_str()]);

    let err = ctx
        .send(request("GET", &url, Vec::new(), None))
        .expect_err("connection to a closed port should fail");

    assert!(matches!(err, WitHttpError::Transport(_)));
}
