#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "plugin",
});

use edlr::plugin::driver_http::{send, DriverError, Request};
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
    }

    fn on_message(_driver: String, _topic: String, _payload: Vec<u8>) {}
}

export!(HttpCaller);
