//! 計測点の配線が共通で使う小さな関数群。
//!
//! `call_sample` は純関数(値イン値アウト、→ trait-di.md「時刻は trait に
//! しない」): 時刻は `now`/`started` を引数で受け取り、内部で時計を読まない。
//! `now_ts` は時計を読むだけの薄いラッパーで、呼び出し側(runner)が
//! `Instant::now()` と一緒に都度呼ぶ。

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::host::plugin::PluginCallError;

use super::{CallKind, CallSample, Outcome, Subject};

/// 現在時刻を UNIX 秒(f64)で返す。
pub fn now_ts() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// wasm 呼び出し 1 回分の `CallSample` を組み立てる純関数。
///
/// outcome の判定: `Ok(())` は `Outcome::Ok`。`Err` は
/// `PluginCallError::is_deadline_exceeded()` で `Timeout` と `Error` に
/// 振り分ける(期限超過 = 一時的でありうる、trap = 決定的な故障、
/// `event_loop::handle_call_result!` と同じ区別)。
pub fn call_sample(
    subject: Subject,
    id: &str,
    call: CallKind,
    detail: &str,
    started: Instant,
    result: &Result<(), PluginCallError>,
    now: f64,
) -> CallSample {
    let outcome = match result {
        Ok(()) => Outcome::Ok,
        Err(e) if e.is_deadline_exceeded() => Outcome::Timeout,
        Err(_) => Outcome::Error,
    };
    CallSample {
        ts: now,
        subject,
        id: id.to_string(),
        call,
        detail: detail.to_string(),
        duration_us: started.elapsed().as_micros() as u64,
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_sample_classifies_outcomes_and_measures_duration() {
        let started = Instant::now();
        let s = call_sample(
            Subject::Plugin,
            "p1",
            CallKind::OnEvent,
            "FSDJump",
            started,
            &Ok(()),
            100.0,
        );
        assert!(matches!(s.outcome, Outcome::Ok));
        assert_eq!(s.id, "p1");
        assert_eq!(s.ts, 100.0);
        assert_eq!(s.detail, "FSDJump");
        assert!(matches!(s.call, CallKind::OnEvent));
    }

    #[test]
    fn call_sample_classifies_a_deadline_exceeded_error_as_timeout() {
        let started = Instant::now();
        let result: Result<(), PluginCallError> =
            Err(PluginCallError::DeadlineExceeded { call: "on-event" });
        let s = call_sample(Subject::Plugin, "p1", CallKind::OnEvent, "FSDJump", started, &result, 1.0);
        assert!(matches!(s.outcome, Outcome::Timeout));
    }

    #[test]
    fn call_sample_classifies_a_trap_as_error() {
        let started = Instant::now();
        let result: Result<(), PluginCallError> = Err(PluginCallError::Trap {
            call: "on-event",
            source: wasmtime::Error::msg("boom"),
        });
        let s = call_sample(Subject::Plugin, "p1", CallKind::OnEvent, "FSDJump", started, &result, 1.0);
        assert!(matches!(s.outcome, Outcome::Error));
    }
}
