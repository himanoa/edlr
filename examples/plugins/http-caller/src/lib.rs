#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "plugin",
});

use edlr::plugin::driver_http::{send, submit_send, DriverError, Request};
use edlr::plugin::host_log::{log, Level};

struct HttpCaller;

impl Guest for HttpCaller {
    fn init() {
        log(Level::Info, "http-caller initialized");
    }

    fn on_event(_ev: Event) {
        let settings = edlr::plugin::host_settings::get_all();
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

        // `submit-send` は同じリクエストを非同期に投げる例。呼び出しは即
        // job-id を返し、結果は後から `on-job-complete` に届くので、同期
        // `send` と違い呼び出し期限(2 秒)を HTTP の待ち時間で使わない。
        // 受付時に判定されるのは許可と in-flight 上限だけで、どちらも typed
        // `result` で返る(trap しない)。
        match submit_send(&request, None) {
            Ok(job_id) => {
                log(
                    Level::Info,
                    &format!("http-caller: submitted {url} as job {job_id}"),
                );
            }
            Err(e) => {
                log(Level::Warn, &format!("http-caller: submit failed: {e:?}"));
            }
        }
    }

    fn on_message(_driver: String, _topic: String, _payload: Vec<u8>) {}

    /// `submit-send` の結果。`result-json` は
    /// `{"ok":{"status":..,"headers":..,"body-base64":..}}` か
    /// `{"err":{"kind":..,"message":..}}`(docs/plugins.md 参照)。
    fn on_job_complete(job_id: u64, result_json: String) {
        let status = serde_json::from_str::<serde_json::Value>(&result_json)
            .ok()
            .and_then(|v| v["ok"]["status"].as_u64());
        match status {
            Some(status) => log(
                Level::Info,
                &format!("http-caller: job {job_id} -> status {status}"),
            ),
            None => log(
                Level::Warn,
                &format!("http-caller: job {job_id} failed: {result_json}"),
            ),
        }
    }

    fn on_schedule(_name: String) {}

    fn on_stop() {}
}

export!(HttpCaller);
