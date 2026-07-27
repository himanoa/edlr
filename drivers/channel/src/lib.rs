//! プラグイン間バスの純ロジック。購読表・retained ストア・キュー方針を持つ。
//!
//! **wasmtime にも tokio にも依存しない**。承認(grants)の判定は `core` 側の
//! 責務で、このクレートは「誰が誰に送れるか」を知らない。`core` が承認済みの
//! 呼び出しだけをここに通す(`drivers/fs` が mode を知らないのと同じ分担)。

pub mod topic;

pub use topic::TopicSpec;

/// 1 メッセージのペイロード上限(256 KiB)。バスは制御メッセージの経路で
/// あり、大きなデータの受け渡しは `driver-fs` の担当という切り分け。
pub const BUS_MAX_PAYLOAD: usize = 256 * 1024;
