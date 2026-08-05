//! `driver-http.submit-send` のコールバック式ヘルパー
//! (issue sdk-send-async-response-await-lvn3)。
//!
//! job-id → コールバックの pending マップを SDK が持ち、`on-job-complete`
//! の配送(`register!` の shim → `dispatch_job_complete`)で解決する。
//! demux は job-id だけを見る(job 種別に依存しない)ので、HTTP 以外の
//! job が将来増えても構造は変わらない。SDK を経由せず自前で `submit-send`
//! を呼んだ job の完了は、pending に無い id として `Plugin::on_job_complete`
//! へ委譲される。

use std::cell::RefCell;
use std::collections::HashMap;

use base64::Engine as _;

use crate::bindings::edlr::plugin::driver_http::{submit_send, DriverError, Request};
use crate::Plugin;

/// デコード済みの submit 結果。`body` は base64 を復元したバイト列。
#[derive(Debug, PartialEq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// submit したジョブが失敗したときの結果。
/// `Malformed` はホストの `result-json` が想定形でなかった場合
/// (通常起こらない。SDK と core のバージョン不一致など)。
#[derive(Debug, PartialEq)]
pub enum JobError {
    Transport(String),
    InvalidRequest(String),
    Malformed(String),
}

type Callback = Box<dyn FnOnce(Result<Response, JobError>)>;

thread_local! {
    // wasm ゲストはシングルスレッドなので thread_local + RefCell で足りる。
    static PENDING: RefCell<HashMap<u64, Callback>> = RefCell::new(HashMap::new());
}

/// リクエストを非同期に投げ、完了時に `callback` を(次以降のいずれかの
/// export 呼び出しの中で)起動する。受付が拒否された場合(未承認 /
/// in-flight 上限)はコールバックを登録せず同期の `Err` を返す。
pub fn submit(
    request: Request,
    timeout_ms: Option<u32>,
    callback: impl FnOnce(Result<Response, JobError>) + 'static,
) -> Result<u64, DriverError> {
    let job_id = submit_send(&request, timeout_ms)?;
    register_pending(job_id, Box::new(callback));
    Ok(job_id)
}

fn register_pending(job_id: u64, callback: Callback) {
    PENDING.with(|pending| pending.borrow_mut().insert(job_id, callback));
}

fn take_pending(job_id: u64) -> Option<Callback> {
    PENDING.with(|pending| pending.borrow_mut().remove(&job_id))
}

/// `register!` の `on-job-complete` shim 専用。SDK 利用者は呼ばない。
#[doc(hidden)]
pub fn dispatch_job_complete<P: Plugin>(job_id: u64, result_json: String) {
    match take_pending(job_id) {
        Some(callback) => callback(parse_job_result(&result_json)),
        None => P::on_job_complete(job_id, result_json),
    }
}

/// `result-json`(docs/plugins.md「非同期 HTTP」の形)を値へ変換する純関数。
fn parse_job_result(result_json: &str) -> Result<Response, JobError> {
    let value: serde_json::Value = serde_json::from_str(result_json)
        .map_err(|e| JobError::Malformed(format!("result-json is not JSON: {e}")))?;
    if let Some(err) = value.get("err") {
        let message = err["message"].as_str().unwrap_or_default().to_string();
        return Err(match err["kind"].as_str() {
            Some("transport") => JobError::Transport(message),
            Some("invalid-request") => JobError::InvalidRequest(message),
            other => JobError::Malformed(format!("unknown err kind: {other:?}")),
        });
    }
    let Some(ok) = value.get("ok") else {
        return Err(JobError::Malformed("neither ok nor err".to_string()));
    };
    let status = ok["status"]
        .as_u64()
        .and_then(|s| u16::try_from(s).ok())
        .ok_or_else(|| JobError::Malformed("missing/invalid status".to_string()))?;
    let headers = ok["headers"]
        .as_array()
        .map(|hs| {
            hs.iter()
                .filter_map(|h| {
                    Some((h[0].as_str()?.to_string(), h[1].as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    let body = base64::engine::general_purpose::STANDARD
        .decode(ok["body-base64"].as_str().unwrap_or_default())
        .map_err(|e| JobError::Malformed(format!("invalid body-base64: {e}")))?;
    Ok(Response {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_result_decodes_status_headers_and_base64_body() {
        let json = r#"{"ok":{"status":200,"headers":[["x-a","b"]],"body-base64":"aGVsbG8="}}"#;
        let response = parse_job_result(json).expect("ok result should parse");
        assert_eq!(response.status, 200);
        assert_eq!(response.headers, vec![("x-a".to_string(), "b".to_string())]);
        assert_eq!(response.body, b"hello");
    }

    #[test]
    fn err_result_maps_kind_to_job_error() {
        let json = r#"{"err":{"kind":"transport","message":"boom"}}"#;
        assert!(matches!(
            parse_job_result(json),
            Err(JobError::Transport(m)) if m == "boom"
        ));
        let json = r#"{"err":{"kind":"invalid-request","message":"bad"}}"#;
        assert!(matches!(
            parse_job_result(json),
            Err(JobError::InvalidRequest(m)) if m == "bad"
        ));
    }

    #[test]
    fn garbage_json_is_malformed_not_a_panic() {
        assert!(matches!(parse_job_result("not json"), Err(JobError::Malformed(_))));
        assert!(matches!(parse_job_result("{}"), Err(JobError::Malformed(_))));
        // ok はあるが base64 が壊れている
        let json = r#"{"ok":{"status":200,"headers":[],"body-base64":"%%%"}}"#;
        assert!(matches!(parse_job_result(json), Err(JobError::Malformed(_))));
    }

    #[test]
    fn pending_map_resolves_once_and_delegates_unknown_ids() {
        use std::cell::Cell;
        let resolved = std::rc::Rc::new(Cell::new(0u32));
        {
            let resolved = resolved.clone();
            register_pending(7, Box::new(move |_| resolved.set(resolved.get() + 1)));
        }
        // 登録済み id は解決され、二度目は「未知」になる。
        assert!(take_pending(7).is_some());
        assert!(take_pending(7).is_none());
        assert!(take_pending(999).is_none());
    }
}
