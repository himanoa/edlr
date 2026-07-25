# edlr UI フェーズ 1(ログビューワー + プラグイン設定画面)設計書

2026-07-25 のブレインストーミングで承認された設計。上位仕様は `spec.md`、監視コアは
`2026-07-25-edlr-monorepo-core-design.md` を参照。

## スコープ

- **core**: WebSocket サーバ + 静的ファイル配信の追加(axum)。既存の監視コアは無変更
- **ui/frontend**: React + TypeScript + Vite。Logs / Plugins / Dashboard(プレースホルダ)の 3 画面
- **ui/src-tauri**: Tauri 2 の薄い皮。ウィンドウを出してフロントエンドを表示するだけ
- プラグイン設定画面は**ダミーデータ**(モックマニフェスト)で UI 先行。プラグイン基盤は別フェーズ
- ダッシュボードの「プラグインで拡張可能」な中身は後回し(プレースホルダのみ)

## core: HTTP/WS サーバ

- 依存追加: axum(ws feature)、tower-http(fs feature、静的配信用)
- CLI 追加: `--listen <ADDR>`(既定 `127.0.0.1:8137`)、`--ui-dir <PATH>`(任意。指定時のみ静的配信)
- `/ws`: WebSocket エンドポイント。Router を subscribe してイベントを JSON テキストフレームで配信
- `--ui-dir` 指定時、`/` 以下でディレクトリを配信(SPA フォールバックは index.html)
- stdout への 1 行 1 JSON 出力は従来どおり維持(WS と並存)

### WS プロトコル最小版(未決定事項の部分確定)

JSON テキストフレーム。フェーズ 1 はサーバ→クライアントの配信のみ(制御系 RPC は将来拡張)。

```jsonc
// 接続直後
{ "type": "hello", "protocol": 1 }
// リプレイ(接続時にリングバッファの内容を古い順に送る)と、以降のライブイベント。形式は同一:
{ "type": "event", "kind": "journal", "timestamp": "2026-07-25T12:00:00Z", "event": "FSDJump", "raw": { ... } }
{ "type": "event", "kind": "status", "raw": { ... } }
```

- **リプレイ**: サーバはイベントのリングバッファ(容量 1000)を保持し、新規接続時にまず全量を
  送ってからライブ配信に移行する。ログビューワーが開いた瞬間に空にならないため
- クライアント→サーバのメッセージは無視する(将来の RPC 用に予約)
- broadcast の Lagged はそのクライアントへの配信欠落として許容(warn ログ)
- **Origin チェック**: `/ws` へのアップグレード要求は Origin ヘッダが localhost 系
  (`127.0.0.1` / `localhost` / `[::1]`、任意ポート・任意スキーム)または Tauri のオリジン
  (`tauri://localhost`、`http(s)://tauri.localhost`)以外であれば 403 で拒否する。
  Origin ヘッダ無しは非ブラウザクライアント(tokio-tungstenite・curl 等)として許可する。

## ui/frontend(React + TypeScript + Vite)

タブナビゲーション 3 画面:

| 画面 | 内容 |
|---|---|
| Dashboard | プレースホルダのみ(後フェーズでプラグイン拡張可能にする) |
| Logs | WS 接続しイベントを時系列表示。イベント名・テキストフィルタ、行クリックで raw JSON 展開、自動スクロール + 一時停止、接続状態インジケータ、自動再接続 |
| Plugins | モックマニフェスト(名前・説明・設定スキーマ)から設定フォームを自動生成。値は localStorage に永続化 |

- モック設定スキーマのフィールド型: `boolean` / `string` / `number` / `select`
- モックデータは `src/mock/plugins.ts` に分離し、将来の本物のマニフェストスキーマの叩き台とする
- WS 接続先: 既定 `ws://127.0.0.1:8137/ws`(ブラウザ配信時は location から導出、Tauri 時は既定値)
- パッケージマネージャ: pnpm(インストール済みの 10.x)。node は mise 管理(26.x)

## ui/src-tauri(Tauri 2)

- フロントエンドのビルド成果物をバンドルしてウィンドウ表示するだけ。独自コマンド・独自ロジックなし
- デーモンへは WebSocket で接続する純粋なクライアント(spec の分離方針どおり)
- Tauri の Rust crate はルート Cargo workspace から **除外**する(Tauri 側は独自 workspace)。
  理由: システム依存(webkit2gtk-4.1-dev 等)が無い環境でも `cargo test --workspace` を壊さないため
- 留意: ビルドには libwebkit2gtk-4.1-dev / libgtk-3-dev / libsoup-3.0-dev / librsvg2-dev が必要

## テスト方針

- core: tokio-tungstenite クライアントによる WS 統合テスト(hello 受信 → publish → event 受信、
  リプレイ動作、複数クライアント)
- frontend: vitest + Testing Library。フィルタロジック、モックマニフェスト→フォーム生成、
  WS メッセージのパース(WebSocket 自体はモック)
- Tauri: システム依存が入るまでビルド検証は保留(雛形と設定のみ作成)

## スコープ外

- プラグイン基盤(wasmtime、本物のマニフェスト)
- WS の制御系 RPC(設定の読み書き、プラグイン管理など)
- ダッシュボードの中身
- 認証(localhost バインドのみ)
