//! RPC 応答の JSON 整形と params 解釈(純粋関数群)。
//!
//! 値イン値アウトのみ。`Registry` などの命令的サービスはここから
//! 参照しない(呼び出すのは server/ の仕事)。
//!
//! 注: `BusInfo` など plugin/registry 配下の**データ型**の import は
//! Phase 2 時点の公認例外(型の所属整理は Phase 4。issue
//! rules-capability-grants-rs-i-o-manifest-99dq 参照)。

pub mod params;
pub mod render;
