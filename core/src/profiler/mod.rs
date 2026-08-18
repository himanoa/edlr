//! プロファイラ(issue: docs/superpowers/specs/2026-08-18-profiler-tab-design.md)。
//! このモジュール直下と sample/bucket は純粋(値イン値アウト)。
pub mod sample;
pub mod bucket;
pub use sample::{CallKind, CallSample, GaugeSample, Outcome, Sample, Subject};
