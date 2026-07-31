---
paths:
  - "core/**/*.rs"
---

# モジュール構成と依存方向

core は**機能名モジュール**で構成する。レイヤーディレクトリ
(`domain/`、`ports/`、`infra/` など)は作らない。

## モジュール一覧

| モジュール | 種別 | 責務 |
|---|---|---|
| `manifest/` | 純粋 | TOML → Manifest のパースと全体整合の検証(I/O は `load_manifest` だけ端に) |
| `capability/` | 純粋 | capability の要求と承認(Request 型・fingerprint・GrantState + Storage trait。`grants.rs` のディスク実装は公認の例外、→ pure-imperative-boundary.md) |
| `settings/` | 純粋 | 設定の検証・マージ + Storage trait(`store.rs`/`filesystem.rs`/`sidecar.rs` のディスク実装は公認の例外、→ pure-imperative-boundary.md) |
| `schedule/` | 純粋 | 発火計算 + 永続化 |
| `rpc/` | 純粋 | RPC 解釈・JSON 整形(純粋関数群) |
| `journal/` | 純粋 | discovery/parser/position/tailer |
| `runtime/` | 純粋 | HostCtx と Registry が共有するランタイムバッファの JSON 形式 + DropCounters |
| `registry/` | 命令的 | プラグイン・ドライバの facade と各サービス |
| `runner/` | 命令的 | プラグインスレッドとイベントループ |
| `host/` | 命令的 | wasmtime 配線 |
| `server/` | 命令的 | axum/WS。rpc/ を呼ぶだけの薄い層 |

## 新規モジュールを足すとき

1. **機能名**で切る(`grants/` ではなく `capability/grants.rs` のように、1概念は1モジュールにまとめる)
2. 純粋か命令的かを最初に決める。迷ったら「ディスク・プロセス・スレッド・チャネル・wasm・ネットワークを触るか」で判定する
3. 純粋モジュールにできないか先に検討する(判断を値イン値アウトに切り出せば大半は純粋にできる)

## core 外の新規コード

drivers/ など core 外でも、**新規に書くコード**には同じ作法
(機能名モジュール・判断と実行の分離)を推奨する。既存の大ファイルへの
遡及適用は別タスクで行う。
