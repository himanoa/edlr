//! edlr デーモン本体。機能名モジュール構成で、純粋モジュールと命令的
//! モジュールを分ける(`.claude/rules/pure-imperative-boundary.md` 参照)。
//!
//! 純粋:
//! - [`manifest`] -- TOML → Manifest のパースと全体整合の検証
//! - [`capability`] -- capability の要求と承認(Request 型・fingerprint・GrantState)
//! - [`settings`] -- プラグイン設定の検証・マージ + 永続化の口
//! - [`schedule`] -- プラグインスケジュールの次回発火時刻計算
//! - [`rpc`] -- RPC 応答の JSON 整形と params 解釈
//! - [`journal`] -- プラグインログの discovery/parser/position/tailer
//! - [`runtime`] -- HostCtx と Registry が共有するランタイムバッファの JSON 形式 + DropCounters
//!
//! 命令的:
//! - [`registry`] -- プラグイン/ドライバの registry(facade と各サービス)
//! - [`runner`] -- プラグイン/ドライバの実行ランタイム(専用スレッド駆動)
//! - [`host`] -- プラグイン/ドライバの wasmtime 配線
//! - [`server`] -- axum/WS。rpc/ を呼ぶだけの薄い層

pub use edlr_config as config;
pub mod capability;
pub mod event;
pub mod host;
pub mod journal;
pub mod layout;
pub mod logs;
pub mod manifest;
pub mod monitor;
pub mod registry;
pub mod router;
pub mod rpc;
pub mod runner;
pub mod runtime;
pub mod schedule;
pub mod server;
pub mod settings;
pub mod status;
pub mod watch;
