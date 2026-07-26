# edlr — EliteDangerousLogRouter

Elite Dangerous の Journal / Status.json を監視し、イベントをドライバとプラグインへ配るルーター。
設計の全体像は [spec.md](spec.md) を参照。

## 構成

- `core/` — Rust 製カーネル。Journal tail(inotify + ポーリング常時併用)、JSON Lines パース、
  Status.json 監視、broadcast によるイベント配信。バイナリ名 `edlr`
- `drivers/` — 特権 capability を持つドライバ層(http / channel、現在はスケルトン)
- `config/` — `edlr-config` クレート。設定ファイル(`$XDG_CONFIG_HOME/edlr/config.json`)
  のパス解決とシリアライズ、Proton 既定 Journal パスの探索を担う、依存の薄い
  純粋ロジック。デーモンの自動検出(`core/`)・Tauri アプリの設定管理(`ui/`)
  の両方から使われる共有クレートで、`edlr-core` は `edlr_core::config` として
  再エクスポートしている(呼び出し側のコードは変わらない)
- `ui/` — GUI クライアント。`frontend/`(React + Vite の SPA: Logs / Plugins /
  Dashboard / Settings)と `src-tauri/`(Tauri 2 の薄い皮)。デーモンとは
  WebSocket(既定 `ws://127.0.0.1:8137/ws`)で通信

## 使い方

    cargo run -p edlr-core --bin edlr -- --journal-dir <JournalディレクトリのPATH>

`--journal-dir` 省略時は Proton の既定パスを探索する。イベントは 1 行 1 JSON で stdout に流れる。

主な CLI フラグ:

| フラグ | 既定 | 説明 |
| --- | --- | --- |
| `--journal-dir` | Proton の既定パスを探索 | Journal ディレクトリ |
| `--poll-interval-ms` | `1000` | ポーリング間隔(ミリ秒) |
| `--listen` | `127.0.0.1:8137` | HTTP/WebSocket の listen アドレス |
| `--ui-dir` | (未指定なら配信しない) | UI 静的ファイルのディレクトリ |
| `--plugins-dir` | `$XDG_CONFIG_HOME/edlr/plugins`(未設定なら `~/.config/edlr/plugins`) | プラグインディレクトリ |
| `--settings-dir` | `$XDG_CONFIG_HOME/edlr/settings`(未設定なら `~/.config/edlr/settings`) | プラグイン設定の保存先 |
| `--grants-dir` | `$XDG_CONFIG_HOME/edlr/grants`(未設定なら `~/.config/edlr/grants`) | capability 承認の保存先 |
| `--state-dir` | `$XDG_STATE_HOME/edlr`(未設定なら `~/.local/state/edlr`) | Journal 読み取り位置の保存先(下記参照) |

### Journal 読み取り位置の永続化

`edlr` は Journal の読み取り位置を `<state-dir>/journal-position.json` に
(監視している Journal ディレクトリをキーにして)保存し、デーモンを再起動した
ときはその位置から読み取りを再開する。これが無かった旧バージョンでは、
再起動のたびに現行 Journal ファイルを先頭から読み直し、その日のイベントを
丸ごと再配信していた。

保存は行の配信直後に行われ、書き込みに失敗しても(`state-dir` に書けない、
ディスクフルなど)デーモンは止まらない。警告ログを 1 度だけ出し、以後は
**永続化なしで**(= 従来どおり毎回先頭から読む挙動で)動き続ける。

**同じ Journal ディレクトリを複数の `edlr` デーモンで同時に監視する構成は
サポートしない**。読み取り位置ファイルへの書き込みは 1 プロセス内でしか
直列化されないため、複数プロセスが同じキーへ保存すると競合し、位置が
壊れたり巻き戻ったりし得る。

### `replay` フラグ

デーモンが動き出す前に、監視対象の Journal ファイルへ**既に書かれていた**
イベントには、配信されるイベントの `replay` フィールドが `true` になる
(初回起動でファイルを先頭から読む場合も、保存済み位置から再開してその位置
より前の内容を読む場合も同様)。デーモン起動後に新しく書かれたイベントは
常に `replay: false`。WebSocket で流れる `journal` 種別のイベントにも
同じ `replay` が乗る(`status` 種別は「今の状態のスナップショット」なので
`replay` の概念自体が無く、常に `false`)。

用途の目安:

- 通知・音を鳴らす系のプラグインは `replay` のイベントを無視するのが自然
  (デーモン起動時にゲーム内の出来事が一斉に鳴り直すのを避けるため)
- 外部サービスへのアップロード・集計系のプラグインは、位置の永続化により
  再起動をまたいだ重複配信が起きなくなったので、`replay` のイベントも
  安全に処理してよい(取りこぼしを避けたいなら処理すべき)

## プラグイン

`edlr` は起動時に `--plugins-dir` 配下を走査し、見つかった各プラグイン(WASM
コンポーネント)をロードして専用スレッドで駆動する。プラグインホスト
(wasmtime エンジン)の初期化に失敗した場合はその旨を warn ログに出し、
プラグイン機能なしでデーモン本体は動き続ける。

インターフェースは `core/wit/plugin.wit` の 2 つの world:

- **`plugin`** — ホスト側(`bindgen!`)が使う。edlr が提供する 4 インターフェース
  (`host-log` / `host-settings` / `driver-http` / `driver-process`)と、プラグインが
  export する `init` / `on-event` だけを宣言する
- **`plugin-guest`** — **プラグイン(ゲスト)がビルド時に対象にする world**。
  `plugin` に WASI の import 一式(`wasi:cli/imports@0.2.0`)を足したもの。Go/TinyGo の
  標準ライブラリはプラグインが何も呼ばなくても WASI を import するため、`plugin` を
  直接対象にするとコンポーネント化が失敗する。Rust の `wasm32-wasip2` ターゲットは
  リンカが WASI import を自動で足すのでどちらでもビルドできる

WASI 自体はホストが `wasmtime_wasi` の `add_to_linker_sync` で提供するため、
`plugin-guest` でビルドしたコンポーネントはそのままロードできる。

WIT パッケージは `edlr:plugin@0.2.0`(Journal 読み取り位置の永続化に伴い、
`event` レコードへ `replay: bool` を追加した ABI 破壊的変更で `0.1.0` から
上がった)。**旧 world(`@0.1.0`)でビルド済みのプラグインは新しいホストへの
ロードに失敗する**。プラグインを新しい `core/wit` に対して再ビルドすること
(Rust は `wit_bindgen::generate!` がパス指定なら自動追随、Go/TinyGo は
`wit-bindgen-go generate` での `gen/` 再生成が必要)。

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

### サイドカープロセス(driver-process)

プラグインは wasm サンドボックス内で動くため、外部プロセスを起動する
こともできない。ネイティブの外部プロセス(音声合成エンジンなど)を
サイドカーとして立ち上げたいプラグインは、`manifest.toml` に
`[[sidecar]]` でその要求を宣言し、ユーザーが Plugins UI で承認したものだけ
が実際に起動できる。

#### マニフェストの `[[sidecar]]` 書式

    [[sidecar]]
    name = "tts"
    reason = "why this plugin needs to run this process"
    args = ["--port", "{port}"]
    port = 50021
    scalable = true

- `name` はプラグイン内で一意なサイドカーの識別子
- `reason` は空文字不可。承認画面でユーザーに表示される、人間可読の理由文
- `args` はマニフェストが宣言する既定の引数列。`{port}` はサイドカーの
  実際の起動ポートに展開される
- `port` はマニフェストの既定ポート
- `scalable` が `true` のサイドカーのみ、ユーザーは Plugins UI から
  `replicas` を増やして複数インスタンスを起動できる
- **`command`(実行ファイルの実パス)はマニフェストには書かない**。
  ユーザーが Plugins UI の実行ファイルピッカー(または直接入力)で指定する

#### `{port}` 展開と `replicas` によるポート採番

- `replicas` が `N` のとき、インスタンスは設定した `port` から
  `port + N - 1` までを連番で使用する(インスタンス `i` は `port + i`)
- 各インスタンスの起動引数中の `{port}` は、そのインスタンス自身の
  ポート番号に展開される

#### 承認フローと `driver-http` への暗黙許可

- サイドカーを要求するプラグインは、**既定では未承認**の状態でロードされる。
  未承認の間、そのサイドカーは起動できない(プラグイン自体は動き続ける)
- 実行ファイルパス(`command`)が未設定の間は承認できない。この検証は
  `Registry::set_sidecar_grant` 自身が行う(UI のチェックボックスの
  `disabled` は補助であり、それだけには依存しない)。RPC を直接叩いても
  同じ検証を回避できない
- サイドカーを承認すると、そのサイドカーが実際に使用するポート範囲
  (`port` 〜 `port + replicas - 1`)への `driver-http` アクセスが
  **暗黙に許可される**(別途 `[[capabilities]]` での `http` 承認は不要)。
  この暗黙許可は http capability の承認とは独立に効き、**サイドカーを
  1 つも起動していなくても**成立する -- つまり承認した時点で、そのプラグインは
  `http://127.0.0.1:<port>` への通信を試みられる。`port` はマニフェストで
  プラグイン作者が指定した既定値で、ユーザーが Plugins UI で変更できる。
  既に他のプログラム(別プラグインのサイドカーやローカルの開発サーバなど)が
  同じポートで listen していた場合、承認するとそのプログラムとも通信できて
  しまう点に注意が必要。Plugins UI の承認画面はこの帰結と、承認によって
  実際に許可される具体的なポート一覧を表示する
- マニフェストの要求内容(`args` / `port` / `scalable` の集合)が変わると、
  `driver-http` capability と同様に以前の承認は自動的に失効(stale)する

#### プロセスのライフサイクル

- **自動再起動はしない**。プラグインが `ensure-started` を呼んで初めて
  起動を試みる。`ensure-started` の最短呼び出し間隔は 1 秒で、それより
  高頻度に呼んでもプロセスを何度も起動し直したりはしない
- **停止はプロセスグループごと**に行う。まず SIGTERM を送り、3 秒待って
  終了しなければ SIGKILL する。個々の子プロセスだけでなく、そのプロセスが
  さらに fork した子孫プロセスも道連れで止める
- ホストが強制的にサイドカーを停止する契機は、デーモン終了以外にもある:
  プラグインの無効化(`on-event` の trap など)、サイドカーの承認取消、
  サイドカー設定の変更。いずれも「走り続けてよい根拠が無くなった」瞬間に
  ホストが同期的に停止する
- デーモンが終了するときは、起動中の全サイドカーが必ず停止する。デーモン
  (`edlr` バイナリ)は SIGTERM/SIGINT(`Ctrl-C`)を捕捉し、受け取ったら
  終了前に全サイドカーを停止してから抜ける。Tauri デスクトップアプリを
  終了する経路も、デーモンへ(`Child::kill()` = SIGKILL でいきなり殺すの
  ではなく)まず SIGTERM を送って後始末の猶予(`STOP_GRACE`、既定 65 秒)を
  与え、応答がなければ SIGKILL にフォールバックする。捕捉できないシグナル
  (`SIGKILL` を外部から直接送るなど)でデーモン自体が即死した場合はこの
  保証の対象外(サイドカーは孤児になりうる)
- **`STOP_GRACE` はデーモン側の 1 インスタンスあたりの猶予(3 秒)と
  同じ値にしてはいけない**。デーモンの後始末はサイドカーのインスタンスごとに
  逐次(SIGTERM 無視の子がいれば 1 件につき最大 3 秒)行われるため、
  デーモン全体の後始末の最悪時間は 1 インスタンス分よりずっと長くなりうる。
  `STOP_GRACE` はこの最悪ケース(3 秒 × 想定インスタンス数上限 20 = 60 秒)を
  超える値(65 秒)にしてあり、`ui/src-tauri/src/daemon.rs` のコンパイル時
  アサーションでこの関係を固定している。デーモンが早く終了すれば
  `stop_child_gracefully` は即座に戻るので、通常の終了が 65 秒かかるわけ
  ではない

#### 設定・承認の保存先

- サイドカー設定(`command` / `args` / `port` / `replicas`)は
  `<settings-dir>/<id>.sidecars.json` に保存される(通常のプラグイン設定
  `<settings-dir>/<id>.json` とは別ファイル)
- サイドカーの承認は `<grants-dir>/<id>.json` に、`driver-http`
  capability の承認と同じファイルに保存される

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

### ファイルアクセス(driver-fs)

プラグインは wasm サンドボックス内で動くため、既定ではファイルシステムに
一切アクセスできない。ローカルのファイルを読み書きしたいプラグインは、
`manifest.toml` に `[[filesystem]]` でアクセスしたいルートを宣言し、
ユーザーが Plugins UI で承認したものだけが実際にアクセスできる。**アクセス
先のディレクトリ自体はマニフェストには書かない** — ユーザーが Plugins UI の
フォルダピッカー(または直接入力)で指定する。

#### マニフェストの `[[filesystem]]` 書式

    [[filesystem]]
    name = "exports"
    reason = "why this plugin needs to read/write files under this root"
    mode = "read-write"

- `name` はプラグイン内で一意なルートの識別子
- `reason` は空文字不可。承認画面でユーザーに表示される、人間可読の理由文
- `mode` は `"read"` または `"read-write"`。`read` は読み取りのみ、
  `read-write` は読み取りに加えて作成・上書き・削除も許可する

#### 承認フロー(Plugins UI)

- ファイルアクセスを要求するプラグインは、**既定では未承認**の状態で
  ロードされる。未承認の間、そのルートへの `driver-fs` 呼び出しは全て
  `permission-denied` エラーになる(プラグイン自体は動き続ける)
- 承認はルート(`name`)単位。1 つのプラグインが複数の `[[filesystem]]`
  ルートを宣言している場合、それぞれ個別に承認・取消できる
- ディレクトリパス(`config.path`)が未設定の間は承認できない。この検証は
  サーバ側(`Registry::set_filesystem_grant` 相当)が行う。UI のチェック
  ボックスの `disabled` は補助であり、それだけには依存しない — RPC を
  直接叩いても同じ検証を回避できない
- マニフェストの要求内容(`reason` / `mode` の集合)が変わると、
  `driver-http` / サイドカーと同様に以前の承認は自動的に失効(stale)する
- 承認/取消は稼働中のプラグインにも即座に反映される(再起動不要)
- 選べないディレクトリがある。システム上重要なディレクトリ「そのもの」
  (`/`、`/home`、`/etc`、`/usr`、`/var`、`/boot`、`/dev`、`/proc`、`/sys`、
  ホームディレクトリそのもの)と、**edlr 自身の状態ディレクトリ
  (`--settings-dir` / `--grants-dir` / `--plugins-dir`)およびその祖先・
  配下**は保存時に拒否される。後者を read-write で承認できると、承認の
  捏造やプラグイン本体の差し替えを通じて、ファイルアクセスの承認が他の
  capability への昇格経路になってしまうため

#### パス検証

ユーザーが指定したディレクトリパス、および `driver-fs` に渡される相対
パス(`rel`)は、3 段の検証を経て初めてアクセスが許可される:

1. **構文チェック** — ユーザーが指定するディレクトリは絶対パスであること。
   `driver-fs` に渡す `rel` は逆に**相対パス**でなければならず、`..` / `.` /
   空要素 / 末尾 `/` / NUL・制御文字 / バックスラッシュを含むものは
   ファイルシステムに触る前に拒否する
2. **配下チェック** — 解決後のパスが承認済みルートの配下に収まっていること
3. **`openat2`(`RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`)によるカーネル
   レベルの拘束** — 1・2 段目はアプリケーション側の事前チェックに過ぎず、
   確認してから開くまでの間に差し替えられる余地(TOCTOU)がある。
   `openat2` はルート fd からの相対パス解決をカーネルに強制させることで
   これを防ぐ。`openat2` が使えない環境(Linux 5.6 未満)では
   `openat(O_NOFOLLOW)` によるフォールバックで同等の制約を掛ける

**この 3 段目の帰結として、承認したルート内にあるシンボリックリンクも
拒否される**(`RESOLVE_NO_SYMLINKS`)。リンクの参照先がルート内・
ルート外のどちらであっても関係なく拒否する — シンボリックリンクを
経由した読み書きは、ユーザーが承認した範囲を静かに超えうるため

**限界(ハードリンク・バインドマウント)**: この 3 段が拘束するのは
パス解決であって、ルート内のディレクトリエントリがどの inode を指すかでは
ない。`driver-fs` に `link` 操作は無く `write` は tmp + `rename` なので、
**プラグインがルート内にハードリンクを作る経路は存在しない**。ただし、
承認したフォルダ内に**外部の主体が事前に張ったハードリンクやバインド
マウントが既に存在する場合、その内容(ルート外の実体)には到達しうる** —
`openat2` はパス解決の拘束なので、これを検出できない。保証の正確な形は
「プラグインがルートの外へ出る経路を作れない」であって、「ルート内の
どのエントリも必ずルート内の実体を指す」ではない

#### `read` / `read-range` の上限

- 1 回の読み取りで返せるバイト数は最大 8 MiB。超えるファイルは
  `read-range` を使って分割して読む
- 上限判定は開いた後の fd から取る(開く前の `metadata` を信じると、
  判定した相手と読む相手が別物になりうる)。読み取り中にファイルが
  伸びても、上限を超えて確保することはない
- 読めるのは**通常ファイルだけ**。FIFO・デバイス・ソケット・ディレクトリは
  拒否する(FIFO の `open` は reader / writer が現れるまで返らず、ホスト
  呼び出し中は呼び出し期限が効かないため、プラグインのスレッドが固まって
  しまう)。同じ理由で **`stat` と `append` も通常ファイルのみを受け付ける** —
  ディレクトリ・FIFO・デバイス・ソケットに対しては `invalid-path` になる
  (`list` が返すのも通常ファイルだけなので、対象は揃っている)

#### `list` の挙動

- 指定したディレクトリ配下を再帰的に列挙する
- 返るのはファイルのみ(ディレクトリ自体はエントリとして返らない)
- 返すエントリは 10,000 件まで。超えると打ち切らずに `too-large` を返す
  (途中まで返して「全部です」と誤解させないため)
- 走査したディレクトリエントリの総数にも 100,000 件の予算があり、超えると
  同じく `too-large`。返却数の上限だけでは、ディレクトリや FIFO しか
  含まない巨大ツリーが上限に触れないまま全走査されてしまう
- 返る名前は必ず他の操作にそのまま渡せる。構文チェックを通らない名前
  (バックスラッシュ・制御文字・非 UTF-8)は列挙しない

#### 書き込みの挙動

- `write` は原子的 — 同一ディレクトリに一時ファイルを作成してから
  `rename` する。書き込み中にプロセスが落ちても、書き込み先のファイルが
  中途半端な内容で残ることはない
- `append` は非原子的 — 既存ファイルをオープンしてそのまま追記する

#### 設定・承認の保存先

- ルートごとのディレクトリ設定(`config.path`)は
  `<settings-dir>/<id>.filesystem.json` に保存される(通常のプラグイン
  設定 `<settings-dir>/<id>.json` とは別ファイル)
- ファイルアクセスの承認は `<grants-dir>/<id>.json` に、`driver-http`
  capability やサイドカーの承認と同じファイルに保存される

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
  Proton 既定パスの自動探索に委ねる。一度設定した後で自動探索へ戻したい場合は
  Settings 画面の「自動検出に戻す」を使う(`journalDir` を消してデーモンを
  再起動する)
- デーモンの起動自体に失敗した場合(バイナリが見つからない等)は Settings
  画面にその理由を表示する。外部起動のデーモンが居るケースとは区別され、
  保存や「自動検出に戻す」で再度起動を試みる
- 環境変数 `EDLR_JOURNAL_DIR` が設定されている場合はそちらが常に優先される
  (spawn 時・Settings 画面での再起動時・Settings 画面の表示のすべてで
  同じ実効値になる)。設定ファイルに値があっても `EDLR_JOURNAL_DIR` が
  勝つので、Settings から保存しても実際の反映先は変わらない
