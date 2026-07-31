//! プラグインログの journal(discovery/parser/position/tailer)を扱う純粋
//! モジュール群。ファイル探索・パース・読み取り位置の計算・追跡のそれぞれを
//! サブモジュールに分ける。

pub mod discovery;
pub mod parser;
pub mod position;
pub mod tailer;
