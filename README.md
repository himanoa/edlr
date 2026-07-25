# edlr — EliteDangerousLogRouter

Elite Dangerous の Journal / Status.json を監視し、イベントをドライバとプラグインへ配るルーター。
設計の全体像は [spec.md](spec.md) を参照。

## 構成

- `core/` — Rust 製カーネル。Journal tail(inotify + ポーリング常時併用)、JSON Lines パース、
  Status.json 監視、broadcast によるイベント配信。バイナリ名 `edlr`
- `drivers/` — 特権 capability を持つドライバ層(http / channel、現在はスケルトン)
- `ui/` — GUI クライアント。`frontend/`(React + Vite の SPA: Logs / Plugins / Dashboard)と
  `src-tauri/`(Tauri 2 の薄い皮)。デーモンとは WebSocket(既定 `ws://127.0.0.1:8137/ws`)で通信

## 使い方

    cargo run -p edlr-core --bin edlr -- --journal-dir <JournalディレクトリのPATH>

`--journal-dir` 省略時は Proton の既定パスを探索する。イベントは 1 行 1 JSON で stdout に流れる。

## プラグイン

`edlr` は起動時に `--plugins-dir` 配下を走査し、見つかった各プラグイン(WASM
コンポーネント)をロードして専用スレッドで駆動する。プラグインホスト
(wasmtime エンジン)の初期化に失敗した場合はその旨を warn ログに出し、
プラグイン機能なしでデーモン本体は動き続ける。

### plugins-dir のレイアウト

    <plugins-dir>/
      <id>/
        manifest.toml
        plugin.wasm   (manifest.toml の entry で指すファイル名。任意の名前でよい)

- ディレクトリ名(`<id>`)は `manifest.toml` の `id` と一致していなければならない
- `id` は `[a-z0-9-]+` にマッチする必要がある

`manifest.toml` の主なフィールド:

| フィールド | 必須 | 説明 |
| --- | --- | --- |
| `id` | ✓ | プラグイン ID。ディレクトリ名と一致必須 |
| `name` | ✓ | 表示名 |
| `version` | ✓ | バージョン文字列 |
| `description` | | 説明文(省略可) |
| `entry` | ✓ | `plugins-dir/<id>/` からの相対パスで wasm ファイルを指す |
| `events` | | 購読するイベント名の配列。`"*"` は全 journal イベント、`"status"` は Status.json 更新にマッチ(省略時は空 = 何も受け取らない) |
| `[[settings]]` | | 設定項目。`type` は `boolean` / `string` / `number` / `select` のいずれかで、それぞれ `key` / `label` / `default`(select はさらに `options`)を持つ |

設定値は `<settings-dir>/<id>.json` に保存され、未保存キーは manifest の
`default` にフォールバックする。

### hello-logger サンプルのビルドと配置

`examples/plugins/hello-logger` は購読したイベントをそのまま `host-log` へ
ログ出力するサンプルプラグイン。

    rustup target add wasm32-wasip2   # 未追加なら
    cd examples/plugins/hello-logger
    cargo build --release --target wasm32-wasip2

    # plugins-dir に配置する
    mkdir -p ~/.config/edlr/plugins/hello-logger
    cp target/wasm32-wasip2/release/hello_logger.wasm \
       ~/.config/edlr/plugins/hello-logger/plugin.wasm
    cat > ~/.config/edlr/plugins/hello-logger/manifest.toml <<'EOF'
    id = "hello-logger"
    name = "Hello Logger"
    version = "0.1.0"
    entry = "plugin.wasm"
    events = ["FSDJump"]

    [[settings]]
    key = "enabled"
    label = "Enabled"
    type = "boolean"
    default = true
    EOF

    cargo run -p edlr-core --bin edlr -- --journal-dir <PATH>

`--plugins-dir` / `--settings-dir` を省略した場合、既定はそれぞれ
`$XDG_CONFIG_HOME/edlr/plugins` / `$XDG_CONFIG_HOME/edlr/settings`
(`XDG_CONFIG_HOME` 未設定なら `~/.config/edlr/...`)。`--plugins-dir` が
指すディレクトリが存在しなくてもエラーにはならず、プラグイン 0 件で起動する。

    cargo run -p edlr-core --bin edlr -- \
      --journal-dir <PATH> \
      --plugins-dir <PATH> \
      --settings-dir <PATH>

### プラグイン設定 RPC

`/ws` は journal/status イベントの配信と同じ WebSocket 接続上で、プラグイン
一覧取得・設定の読み書きを行う RPC を多重化している。

リクエストは次の形式で送る:

    {"type": "rpc", "id": <任意の値>, "method": "<method>", "params": {...}}

レスポンスは成功時 `rpc-result`、失敗時 `rpc-error`(`error` は文字列)で、
`id` はリクエストの値をそのまま返す:

    {"type": "rpc-result", "id": <同じ id>, "result": {...}}
    {"type": "rpc-error", "id": <同じ id>, "error": "<message>"}

不正な JSON や `type: "rpc"` 以外のメッセージは黙って無視され、接続は切れない。

サポートするメソッド:

- **`plugins/list`**(`params` 不要) — `plugins-dir` のパスと、ロード済み全プラグインの
  状態を返す。

      result: {
        "pluginsDir": "<path>",
        "plugins": [
          {
            "id": "hello-logger", "name": "Hello Logger", "version": "0.1.0",
            "description": "...",
            "state": "running" | "disabled",
            "reason": "<disabled のときのみ>",
            "settings": [ /* manifest.toml の [[settings]] 定義 */ ],
            "values": { "enabled": true, ... }  /* 現在の設定値 */
          }
        ]
      }

- **`plugins/get-settings`** `{"plugin": "<id>"}` → 現在の設定値オブジェクト
  (`{"enabled": true, ...}`)。未知の `plugin` は `rpc-error`。
- **`plugins/set-settings`** `{"plugin": "<id>", "values": {"<key>": <value>, ...}}`
  → 更新後の設定値オブジェクト。未知の `plugin` / 未知の `key` は
  `rpc-error` になり、値は変更されない。設定は `<settings-dir>/<id>.json`
  に永続化され、稼働中のプラグインは次に受け取るイベントから新しい値を
  読み取る(再起動不要)。

UI の Plugins 画面はこの 3 メソッドのみでプラグイン一覧・設定フォームを
描画しており、localStorage やモックデータには依存していない。設定変更は
即座にデーモンへ送られ、`<settings-dir>/<id>.json` に反映される。

## UI

    # デーモン(WS サーバ込み)を起動
    cargo run -p edlr-core --bin edlr -- --journal-dir <PATH>

    # ブラウザ版(開発)
    cd ui/frontend && pnpm install && pnpm dev   # http://localhost:5173

    # デーモンに静的配信させる場合
    cd ui/frontend && pnpm build
    cargo run -p edlr-core --bin edlr -- --journal-dir <PATH> --ui-dir ui/frontend/dist

    # Tauri(要 libwebkit2gtk-4.1-dev ほかシステム依存)
    cd ui/src-tauri && cargo tauri dev

    # Tauri アプリはデーモン未起動なら自動で spawn し、終了時に道連れで止める。
    # 既に起動済みのデーモンには手を出さない。EDLR_BIN / EDLR_JOURNAL_DIR で上書き可。
