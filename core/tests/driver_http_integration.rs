//! Integration tests for the real `driver-http` networking wired up in
//! `core/src/plugin/host.rs`. These exercise `HostCtx::send` directly (the
//! same WIT-facing entry point a plugin's `driver-http.send` call goes
//! through), against a real local HTTP server, without going through wasm --
//! matching the style of `host.rs`'s own unit tests, which the module docs
//! explain is deliberate: the permission decision and the driver dispatch
//! are both made from `HostCtx` alone, so a direct call exercises exactly
//! the same path a real guest call would.

use edlr_core::host::plugin::{
    capabilities_json_string, HostCtx, PluginJobs, WitDriverHttpHost as _, WitHttpError,
    WitHttpRequest, FS_LIST_LIMIT, FS_READ_LIMIT, HTTP_MAX_BODY, HTTP_TIMEOUT,
};
use edlr_core::runner::plugin::queue::PluginWorkReceiver;
use edlr_core::runner::plugin::PluginWork;
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
    let (ctx, rx) = ctx_with_driver_and_queue(driver, hosts);
    // submit 系を使わないテストでは受信側を forget で生かしたままにする
    // (drop すると push が Disconnected になるため)。
    std::mem::forget(rx);
    ctx
}

/// `ctx_with_driver` の submit 系テスト用: 作業キューの受信側も返す。
/// `submit-send` の完了通知(`PluginWork::JobComplete`)はこの受信側に届く。
fn ctx_with_driver_and_queue(
    driver: Arc<HttpDriver>,
    hosts: &[&str],
) -> (HostCtx, PluginWorkReceiver) {
    let hosts: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
    let capabilities_json = capabilities_json_string(&hosts);
    let (work_tx, work_rx) = edlr_core::runner::plugin::queue::channel();
    let ctx = HostCtx::new(
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
        work_tx,
        PluginJobs::new(),
    );
    (ctx, work_rx)
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

/// submit-send: 許可済みホストへの submit は即 job-id を返し、完了通知
/// (`PluginWork::JobComplete`)が作業キューへ届く。届いた `result-json` は
/// `{"ok":{"status":..,"headers":..,"body-base64":..}}` の形で、body は
/// base64 で運ばれる。
#[test]
fn submit_send_delivers_a_job_complete_to_the_work_queue() {
    use base64::Engine as _;

    let base = spawn_test_server();
    let (mut ctx, work_rx) = ctx_with_driver_and_queue(default_driver(), &[base.as_str()]);

    let job_id = ctx
        .submit_send(request("GET", &format!("{base}/echo-get"), Vec::new(), None), None)
        .expect("submit to a granted host should be accepted");
    assert_eq!(job_id, 1, "job ids start at 1");

    let work = work_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the spawned job must push a completion to the work queue");
    let PluginWork::JobComplete {
        generation,
        job_id: completed_id,
        result_json,
    } = work
    else {
        panic!("expected PluginWork::JobComplete, got {work:?}");
    };
    assert_eq!(generation, 0, "first instance generation is 0");
    assert_eq!(completed_id, job_id);

    let value: serde_json::Value = serde_json::from_str(&result_json).expect("valid result json");
    assert_eq!(value["ok"]["status"], 200);
    let body = base64::engine::general_purpose::STANDARD
        .decode(value["ok"]["body-base64"].as_str().expect("body-base64"))
        .expect("valid base64 body");
    assert_eq!(body, b"hello from GET");
}

/// submit-send: 許可の無いホストは同期の permission-denied(spawn すら
/// されず、job-id も消費されない)。
#[test]
fn submit_send_without_a_grant_is_a_synchronous_permission_denied() {
    let base = spawn_test_server();
    let (mut ctx, work_rx) = ctx_with_driver_and_queue(default_driver(), &[]);

    let err = ctx
        .submit_send(request("GET", &format!("{base}/echo-get"), Vec::new(), None), None)
        .expect_err("submit without a grant must be rejected synchronously");
    assert!(matches!(err, WitHttpError::PermissionDenied(_)));

    // 完了通知も何も届かない(ジョブ自体が始まっていない)。
    assert!(work_rx.recv_timeout(Duration::from_millis(100)).is_err());
}

/// submit-send のエラー結果も `on-job-complete` 経路で届く(同期エラーに
/// なるのは受付時の判定だけ)。閉じたポートへの submit は受付は成功し、
/// 完了通知が `{"err":{"kind":"transport",..}}` で届く。
#[test]
fn submit_send_transport_failures_arrive_as_err_results() {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind throwaway listener");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);

    let url = format!("http://{addr}/");
    let (mut ctx, work_rx) =
        ctx_with_driver_and_queue(default_driver(), &[format!("http://{addr}").as_str()]);

    ctx.submit_send(request("GET", &url, Vec::new(), None), None)
        .expect("submission itself succeeds; the failure arrives asynchronously");

    let work = work_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the failed job must still push a completion");
    let PluginWork::JobComplete { result_json, .. } = work else {
        panic!("expected PluginWork::JobComplete, got {work:?}");
    };
    let value: serde_json::Value = serde_json::from_str(&result_json).expect("valid result json");
    assert_eq!(value["err"]["kind"], "transport");
}
