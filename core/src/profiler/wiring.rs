//! 計測点の配線が共通で使う小さな関数群。
//!
//! `call_sample` は純関数(値イン値アウト、→ trait-di.md「時刻は trait に
//! しない」): 時刻は `now`/`started` を引数で受け取り、内部で時計を読まない。
//! `now_ts` は時計を読むだけの薄いラッパーで、呼び出し側(runner)が
//! `Instant::now()` と一緒に都度呼ぶ。

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use wasmtime::Trap;

use crate::host::plugin::PluginCallError;

use super::{CallKind, CallSample, Outcome, Subject};

/// 現在時刻を UNIX 秒(f64)で返す。
pub fn now_ts() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// `outcome` が確定した後の組み立てだけを担う純関数。`call_sample`
/// (`PluginCallError` 用)と `call_sample_from_outcome`(driver 側の
/// `anyhow::Result` 用、outcome 判定は `driver_call_outcome` が別に行う)の
/// 両方がここへ収束する -- outcome の判定方法はエラー型ごとに違うが、
/// `CallSample` の組み立ては 1 箇所でよい。
fn build_call_sample(
    subject: Subject,
    id: &str,
    call: CallKind,
    detail: &str,
    started: Instant,
    outcome: Outcome,
    now: f64,
) -> CallSample {
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

/// wasm 呼び出し 1 回分の `CallSample` を組み立てる純関数(プラグイン側:
/// `PluginCallError` を返す呼び出し用)。
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
    build_call_sample(subject, id, call, detail, started, outcome, now)
}

/// ドライバ呼び出し(`anyhow::Result<()>`)の outcome を判定する純関数。
///
/// `DriverInstance` の呼び出しは `PluginCallError` のような期限超過/trap の
/// 判別式を持たない生の `anyhow::Result` を返す。epoch 割り込み
/// (`CALL_DEADLINE` 到達)は `wasmtime::Trap::Interrupt` として現れる点は
/// プラグイン側と同じなので、`PluginCallError::classify` と同じ 1 行判定を
/// ここに置く(仕様: epoch deadline trap = `Timeout`)。
pub fn driver_call_outcome(result: &anyhow::Result<()>) -> Outcome {
    match result {
        Ok(()) => Outcome::Ok,
        Err(e) if e.downcast_ref::<Trap>() == Some(&Trap::Interrupt) => Outcome::Timeout,
        Err(_) => Outcome::Error,
    }
}

/// `outcome` が呼び出し側で判定済みのときに使う `CallSample` の組み立て
/// (driver 側用)。
pub fn call_sample_from_outcome(
    subject: Subject,
    id: &str,
    call: CallKind,
    detail: &str,
    started: Instant,
    outcome: Outcome,
    now: f64,
) -> CallSample {
    build_call_sample(subject, id, call, detail, started, outcome, now)
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

    #[test]
    fn driver_call_outcome_classifies_an_interrupt_trap_as_timeout() {
        let result: anyhow::Result<()> = Err(anyhow::Error::from(Trap::Interrupt));
        assert!(matches!(driver_call_outcome(&result), Outcome::Timeout));
    }

    #[test]
    fn driver_call_outcome_classifies_ok_and_other_errors() {
        assert!(matches!(driver_call_outcome(&Ok(())), Outcome::Ok));
        let result: anyhow::Result<()> = Err(anyhow::anyhow!("boom"));
        assert!(matches!(driver_call_outcome(&result), Outcome::Error));
    }
}
