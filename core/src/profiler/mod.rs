//! プロファイラ(issue: docs/superpowers/specs/2026-08-18-profiler-tab-design.md)。
//! sample/bucket は純粋(値イン値アウト)。collector は命令的(チャネル・
//! スレッド・ファイル IO、→ .claude/rules/pure-imperative-boundary.md)。
pub mod sample;
pub mod bucket;
pub mod collector;
pub mod wiring;
pub use collector::{GaugeSource, Profiler};
pub use sample::{CallKind, CallSample, GaugeSample, Outcome, Sample, Subject};
pub use wiring::{call_sample, call_sample_from_outcome, driver_call_outcome, now_ts};
