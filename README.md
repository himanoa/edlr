# edlr — EliteDangerousLogRouter

Elite Dangerous の Journal / Status.json を監視し、イベントをドライバとプラグインへ配るルーター。
設計の全体像は [spec.md](spec.md) を参照。

## 構成

- `core/` — Rust 製カーネル。Journal tail(inotify + ポーリング常時併用)、JSON Lines パース、
  Status.json 監視、broadcast によるイベント配信。バイナリ名 `edlr`
- `drivers/` — 特権 capability を持つドライバ層(http / channel、現在はスケルトン)
- `config/` — `edlr-config` クレート。Tauri アプリが読み書きする設定ファイル
  (`$XDG_CONFIG_HOME/edlr/config.json`)のパス解決とシリアライズ、Proton 既定
  Journal パスの探索を担う純粋ロジック。デーモン本体(`core/`)はこのクレートに
  依存しない
- `ui/` — GUI クライアント。`frontend/`(React + Vite の SPA: Logs / Plugins /
  Dashboard / Settings)と `src-tauri/`(Tauri 2 の薄い皮)。デーモンとは
  WebSocket(既定 `ws://127.0.0.1:8137/ws`)で通信

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

    {"type": "rpc", "id": <数値>, "method": "<method>", "params": {...}}

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

### capability(driver-http)

プラグインは wasm サンドボックス内で動くため、既定では外部ネットワークに
一切アクセスできない。`driver-http` capability を使って HTTP 通信したい
プラグインは、`manifest.toml` に `[[capabilities]]` で要求する host を
宣言し、ユーザーが Plugins UI で承認したものだけが実際に通信できる。

#### マニフェストの `[[capabilities]]` 書式

    [[capabilities]]
    kind = "http"
    hosts = ["https://api.example.com", "https://api2.example.com:8443"]
    reason = "why this plugin needs to call these hosts"

- `kind` は現状 `"http"` のみ
- `hosts` は 1 件以上の bare origin(`http://` または `https://` + host
  (+ port))。path・query・fragment・userinfo は不可(末尾 `/` 一つだけは
  許容される)
- `reason` は空文字不可。承認画面でユーザーに表示される、人間可読の理由文

#### 承認フロー(Plugins UI)

- capability を要求するプラグインは、**既定では未承認**の状態でロードされる。
  未承認の間、その プラグインの `driver-http.send` 呼び出しは全て
  `permission-denied` エラーになる(プラグイン自体は動き続ける — ロードが
  失敗したり停止したりはしない)
- ユーザーは Plugins UI から `[[capabilities]]` の内容(`hosts` / `reason`)
  を確認して個別に承認・取消できる。承認/取消は `<grants-dir>/<id>.json`
  に永続化され、次回起動時にも引き継がれる
- マニフェストの capability 要求内容(`hosts` / `reason` の集合)が変わると、
  以前の承認は自動的に失効(stale)する。stale な承認は「未承認」として扱われ、
  ユーザーが変更後の内容を確認して再承認するまで通信できない
- 承認/取消は稼働中のプラグインにも即座に反映される(再起動不要)
- 承認は `manifest.toml` の capability 要求内容(`hosts` / `reason` の集合)
  に対して結び付けられており、プラグイン本体(`entry` が指す wasm
  バイナリ)には結び付けられていない。**`manifest.toml` を変更せず
  `plugin.wasm` だけを別物に差し替えた場合、既存の承認はそのまま引き継がれる**
  — フィンガープリントは capability 要求の内容だけから計算され、バイナリの
  ハッシュは含まれない

#### HTTP ドライバの制約

`driver-http.send` は承認済み URL に対してのみ、以下の制約付きで単発の
HTTP リクエストを実行する:

- **リダイレクトを追従しない** — サーバが返した 3xx はそのままプラグインに
  返る。承認された URL 以外への遷移が起きないようにするため
- **タイムアウト 1.5 秒** — 接続からレスポンス受信完了までの全体。epoch
  interruption による 2 秒の呼び出し期限(`PluginInstance::CALL_DEADLINE`)は
  wasm 命令境界でしか作動せず、ブロッキングな `driver-http.send` 呼び出し
  自体を打ち切ることはできないため、この呼び出し期限より厳密に短い値を
  ドライバ自身のタイムアウトとして設定している(`core/src/plugin/host.rs`
  の `HTTP_TIMEOUT` のコンパイル時アサーション参照)
- **リクエストヘッダの制限** — `Host` / `Content-Length` /
  `Transfer-Encoding` / `Connection` / `Upgrade` / `Proxy-*`
  ヘッダはプラグインから設定できない(大小文字を区別せず拒否、
  `invalid-request` エラー)。許可リストが制御するのは接続先だけであり、
  接続後にプラグインが何を名乗るかは制御しないため、これらの
  フレーミング/ルーティングに関わるヘッダを許すとドメインフロンティングや
  リクエストスマグリングの余地が生まれる
- **リクエストボディの上限 8 MiB** — レスポンスボディと同じ上限を
  リクエストボディにも適用する(超過時は `invalid-request`)
- **レスポンスボディの上限 8 MiB** — `Content-Length` を信用せず、実際の
  読み取りをストリーミングでこの上限にキャップする(不正・誤設定な
  サーバによる無制限メモリ確保を防ぐ)
- **許可判定はスキーム + ホスト + ポートの完全一致** — ホストの大文字小文字は
  無視するが、サブドメインのワイルドカードは無い(`api.example.com` の
  許可は `x.api.example.com` を許可しない)。path・query・fragment は
  判定に使わない

#### `--grants-dir`

capability 承認の保存先ディレクトリ。未指定時の既定は
`$XDG_CONFIG_HOME/edlr/grants`(`XDG_CONFIG_HOME` 未設定なら
`~/.config/edlr/grants`)。`--plugins-dir` / `--settings-dir` と同様、
存在しなくてもエラーにはならない(全プラグイン未承認として起動する)。

    cargo run -p edlr-core --bin edlr -- \
      --journal-dir <PATH> \
      --plugins-dir <PATH> \
      --settings-dir <PATH> \
      --grants-dir <PATH>

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

### Journal ディレクトリの設定(Tauri アプリ)

`edlr` バイナリ自身は `--journal-dir` を渡さない限り、Proton の既定パスを
自動探索する(前述)。Steam のセカンダリライブラリにゲームを入れている場合
などはこの既定パスに当たらず、探索は失敗する。

Tauri アプリはこれを設定ファイルで補う:

- 設定ファイルは `$XDG_CONFIG_HOME/edlr/config.json`(`XDG_CONFIG_HOME`
  未設定なら `~/.config/edlr/config.json`)。`journalDir` キー
  (文字列 or 省略)を持つ
- Settings 画面から Journal ディレクトリを選択・保存できる。保存すると
  Tauri が spawn したデーモンを保存先ディレクトリで再起動する
  (外部起動のデーモンを掴んでいる場合は保存のみで再起動はしない)
- `journalDir` が未設定なら、デーモンには `--journal-dir` を渡さず
  Proton 既定パスの自動探索に委ねる
- 環境変数 `EDLR_JOURNAL_DIR` が設定されている場合はそちらが常に優先される
  (spawn 時・Settings 画面での再起動時・Settings 画面の表示のすべてで
  同じ実効値になる)。設定ファイルに値があっても `EDLR_JOURNAL_DIR` が
  勝つので、Settings から保存しても実際の反映先は変わらない
