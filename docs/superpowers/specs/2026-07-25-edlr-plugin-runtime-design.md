# edlr プラグイン基盤 設計書

2026-07-25 承認。spec.md の未決定事項(プラグイン実行・マニフェスト・UI 連携)の部分確定。
HTTP ドライバ・チャネルドライバ・アーカイブ配布形式は次フェーズ。

## 方式

- **wasmtime Component Model + WIT**。プラグインは WASM component として実行される
- イベントペイロードは JSON 文字列のまま渡す(ルーターは配るだけ。型付けはプラグイン側の自由)
- 設定はプル型(プラグインが `host-settings.get-all` で取得。プッシュ通知は将来)
- 暴走・trap は当該プラグインのみ disabled 化し、カーネル・他プラグインには波及させない

## WIT(`core/wit/plugin.wit`)

```wit
package edlr:plugin@0.1.0;

interface host-log {
  enum level { debug, info, warn, error }
  log: func(level: level, message: string);
}

interface host-settings {
  get-all: func() -> string; // 設定値 JSON オブジェクト文字列(defaults マージ済み)
}

world plugin {
  import host-log;
  import host-settings;

  record event {
    kind: string,               // "journal" | "status"
    timestamp: option<string>,
    name: option<string>,       // journal イベント名
    payload-json: string,       // raw JSON
  }

  export init: func();
  export on-event: func(ev: event);
}
```

## マニフェストとレイアウト

ディレクトリ形式(アーカイブは将来。ディレクトリは解凍後の正でもある):

```
<plugins-dir>/<id>/
├── manifest.toml
└── plugin.wasm
```

`manifest.toml`:

| フィールド | 型 | 備考 |
|---|---|---|
| id | string | ディレクトリ名と一致必須。`[a-z0-9-]+` |
| name / version / description | string | 表示用 |
| entry | string | wasm ファイル名(ディレクトリ内相対) |
| events | string[] | 購読。journal イベント名、`"*"`(全 journal)、`"status"` |
| [[settings]] | table[] | key / label / type("boolean"/"string"/"number"/"select") / default / options(select のみ) |

capability 欄は予約(log・settings は暗黙)。検証エラーのあるマニフェストはそのプラグインのみロードスキップ + warn。

## ランタイム(core `plugin/` モジュール)

- `--plugins-dir <PATH>`(既定: `$XDG_CONFIG_HOME/edlr/plugins`、無ければ `~/.config/edlr/plugins`。不存在はプラグイン 0 件として正常起動)
- 起動時走査 → マニフェスト検証 → component ロード → `init()`
- プラグインごとに専用 tokio タスク + wasmtime Store。Router を subscribe し、`events` フィルタ通過分を `on-event` に直列で渡す(プラグイン間は並行)
- **epoch interruption**: 1 call のデッドライン既定 2 秒。超過 trap → disabled 化 + warn
- ロード失敗・実行時 trap・panic 相当は全て「当該プラグインのみ無効」。監視コアには不波及
- 設定永続化: `$XDG_CONFIG_HOME/edlr/settings/<id>.json`(`--settings-dir` で上書き可)。`get-all` は manifest defaults とマージして返す。壊れた保存 JSON は defaults 扱い

## WS RPC(プロトコル拡張)

予約済みだったクライアント→サーバ方向を開通。同一 WS でイベント配信と多重化:

```jsonc
{ "type": "rpc", "id": <number>, "method": "plugins/list" }
{ "type": "rpc", "id": …, "method": "plugins/get-settings", "params": { "plugin": "<id>" } }
{ "type": "rpc", "id": …, "method": "plugins/set-settings", "params": { "plugin": "<id>", "values": { … } } }
// 応答
{ "type": "rpc-result", "id": …, "result": … }
{ "type": "rpc-error", "id": …, "error": "<message>" }
```

- `plugins/list` の result: `{ pluginsDir: "<path>", plugins: [{ id, name, version, description, state: "running"|"disabled", reason?: "<disabled 理由>", settings: [ …スキーマ… ], values: { …現在値… } }] }`
  (当初は配列としていたが、プラグイン 0 件時に UI が plugins dir のパスを案内する要件のためオブジェクトに変更)
- 未知 method・不正 params・未知 plugin は `rpc-error`。パース不能なクライアントメッセージは従来どおり無視
- `set-settings` はスキーマの key に対する部分更新。未知 key は rpc-error

## UI

- Plugins 画面をモックから RPC 経由の本物データへ置換(`SettingField` 型はマニフェスト設定スキーマの TS 型に昇格)
- 設定変更は `plugins/set-settings`。localStorage 永続化は廃止
- プラグイン state(running/disabled)バッジ表示
- プラグイン 0 件時は案内文(plugins dir のパスを表示)

## サンプルプラグイン

`examples/plugins/hello-logger/`(Rust、`wit-bindgen` + `wasm32-wasip2` ターゲット)。
受信イベントを `host-log.log` に出す + `enabled` 設定(boolean)を尊重。統合テストのフィクスチャを兼ねる。

## テスト方針

- core: マニフェスト検証、イベントフィルタ、実 wasm での ロード→配信→ログ呼び出し統合テスト、
  無限ループ wasm の trap→disabled 化、設定の永続化・マージ・壊れ JSON 耐性、RPC ハンドラ
- UI: RPC クライアント(request/response 対応付け・タイムアウト)ユニット、RPC データからのフォーム生成

## 実行計画の分割

- Plan A: プラグインランタイム(WIT・ホスト・マニフェスト・イベント配信・設定・サンプル)
- Plan B: WS RPC + UI 接続

## スコープ外

- HTTP ドライバ・チャネルドライバ(capability 宣言の実運用はここから)
- アーカイブ配布形式・署名
- 設定変更のプラグインへのプッシュ通知
- プラグインの動的リロード(追加・削除はデーモン再起動)
