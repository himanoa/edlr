//! RPC 応答の JSON 整形と params 解釈(純粋関数群)。
//!
//! 値イン値アウトのみ。`Registry` などの命令的サービスはここから
//! 参照しない(呼び出すのは server/ の仕事)。

pub mod info;
pub mod params;
pub mod profiler;
pub mod render;
