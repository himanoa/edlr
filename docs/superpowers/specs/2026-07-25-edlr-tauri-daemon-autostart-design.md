# Tauri からの edlr デーモン自動起動 設計書

2026-07-25 承認。UI フェーズ 1(`2026-07-25-edlr-ui-phase1-design.md`)の追補。

## 方針

「アプリと道連れ」方式。Tauri アプリが起動時にデーモンの生存を確認し、未起動なら子プロセス
として spawn、アプリ終了時に kill する。**既に外部で起動済みのデーモンには一切手を出さない**
(spawn しない・kill しない)。spec 本体の分離設計(GUI なしでも動くデーモン)は維持され、
デーモンを常駐させたいユーザーは従来どおり手動起動すればよい。

## 動作

1. 起動時に `127.0.0.1:8137` へ TCP 接続(タイムアウト 300ms)して生存確認
2. 応答あり → 何もしない(終了時も殺さない)
3. 応答なし → `edlr` バイナリを spawn(stdout/stderr は継承)。`RunEvent::Exit` で kill + wait
4. spawn 失敗・バイナリ不発見はログ(stderr)のみでウィンドウは通常表示
   (フロントの切断バッジ + 自動再接続があるため、後からの手動起動で復帰できる)

## バイナリ探索順(`resolve_edlr_bin`)

1. 環境変数 `EDLR_BIN`(明示指定、存在チェックなしでそのまま使う)
2. アプリ実行ファイルと同じディレクトリの `edlr`(将来の配布レイアウト)
3. `PATH` 上の `edlr`
4. 開発ビルド(`debug_assertions`)のみ: ワークスペースの `target/debug/edlr`
   (`CARGO_MANIFEST_DIR` 起点。リポジトリ内 `tauri dev` 用)

## 引数

環境変数 `EDLR_JOURNAL_DIR` があれば `--journal-dir <値>` を付与。無ければデーモン自身の
既定探索(Proton パス)に任せる。

## テスト方針

- `daemon_running` / `resolve_edlr_bin` / `spawn_daemon` を `ui/src-tauri/src/daemon.rs` の
  純粋寄り関数に切り出しユニットテスト(tempdir・ローカル TcpListener・ダミー実行ファイル使用)
- spawn→kill の統合確認は `tauri dev` の手動スモーク

## スコープ外

- デーモンのヘルスチェック再起動(死活監視)
- GUI からのデーモン停止・再起動操作(将来の WS RPC で扱う)
