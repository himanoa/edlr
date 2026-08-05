#![allow(clippy::too_many_arguments)]

use edlr_plugin_sdk as sdk;
use sdk::driver_http::{send, DriverError, Request};
use sdk::host_log::{log, Level};

struct HttpCaller;

impl sdk::Plugin for HttpCaller {
    fn init() {
        log(Level::Info, "http-caller initialized");
    }

    fn on_event(_ev: sdk::Event) {
        let settings = sdk::host_settings::get_all();
        let url = serde_json::from_str::<serde_json::Value>(&settings)
            .ok()
            .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(str::to_string))
            .unwrap_or_else(|| "https://api.example.com/ping".to_string());

        let request = Request {
            method: "GET".to_string(),
            url: url.clone(),
            headers: Vec::new(),
            body: None,
        };

        // `send` never panics or traps on denial/failure -- it returns a
        // typed `result`, so the plugin can (and, to avoid taking itself
        // down, must) simply handle both cases and keep running.
        match send(&request) {
            Ok(response) => {
                log(
                    Level::Info,
                    &format!("http-caller: {url} -> status {}", response.status),
                );
            }
            Err(DriverError::PermissionDenied(msg)) => {
                log(
                    Level::Warn,
                    &format!("http-caller: {url} -> permission-denied: {msg}"),
                );
            }
            Err(DriverError::InvalidRequest(msg)) => {
                log(
                    Level::Warn,
                    &format!("http-caller: {url} -> invalid-request: {msg}"),
                );
            }
            Err(DriverError::Transport(msg)) => {
                log(
                    Level::Warn,
                    &format!("http-caller: {url} -> transport: {msg}"),
                );
            }
        }

        // `sdk::http::submit` は同じリクエストを非同期に投げる例。呼び出しは
        // 即 job-id を返し、結果はコールバックへ届くので、同期 `send` と違い
        // 呼び出し期限(2 秒)を HTTP の待ち時間で使わない。受付時に判定され
        // るのは許可と in-flight 上限だけで、どちらも typed `result` で返る
        // (trap しない)。
        match sdk::http::submit(request, None, move |result| match result {
            Ok(response) => log(
                Level::Info,
                &format!("http-caller: job completed -> status {}", response.status),
            ),
            Err(e) => log(Level::Warn, &format!("http-caller: job failed: {e:?}")),
        }) {
            Ok(job_id) => log(Level::Info, &format!("http-caller: submitted as job {job_id}")),
            Err(e) => log(Level::Warn, &format!("http-caller: submit failed: {e:?}")),
        }
    }
}

sdk::register!(HttpCaller);
