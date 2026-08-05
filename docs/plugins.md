# プラグイン

**初めてプラグインを書くなら、先に手順を追う形の入門を読むとよい**:
[plugin-tutorial-rust.md](plugin-tutorial-rust.md)(Rust)/
[plugin-tutorial-tinygo.md](plugin-tutorial-tinygo.md)(TinyGo)/
[plugin-tutorial-moonbit.md](plugin-tutorial-moonbit.md)(MoonBit)。
この文書は書き上げた後に仕様を引くためのリファレンスである。

`edlr` は起動時に `--plugins-dir` 配下を走査し、見つかった各プラグイン(WASM
コンポーネント)をロードして専用スレッドで駆動する。プラグインホスト
(wasmtime エンジン)の初期化に失敗した場合はその旨を warn ログに出し、
プラグイン機能なしでデーモン本体は動き続ける。

capability(HTTP 通信・サイドカープロセス・ファイルアクセス)については
[capabilities.md](capabilities.md)、ドライバとのバス連携については
[drivers.md](drivers.md) を参照。

## WIT インターフェース

インターフェースは `core/wit/plugin.wit` の 2 つの world:

- **`plugin`** — ホスト側(`bindgen!`)が使う。edlr が提供する 4 インターフェース
  (`host-log` / `host-settings` / `driver-http` / `driver-process`)と、プラグインが
  export する `init` / `on-event` / `on-message` / `on-schedule` / `on-stop` を宣言する
- **`plugin-guest`** — **プラグイン(ゲスト)がビルド時に対象にする world**。
  `plugin` に WASI の import 一式(`wasi:cli/imports@0.2.0`)を足したもの。Go/TinyGo の
  標準ライブラリはプラグインが何も呼ばなくても WASI を import するため、`plugin` を
  直接対象にするとコンポーネント化が失敗する。Rust の `wasm32-wasip2` ターゲットは
  リンカが WASI import を自動で足すのでどちらでもビルドできる

WASI 自体はホストが `wasmtime_wasi` の `add_to_linker_sync` で提供するため、
`plugin-guest` でビルドしたコンポーネントはそのままロードできる。

### WIT パッケージのバージョン

WIT パッケージは `edlr:plugin@0.5.0`。

- `0.1.0` → `0.2.0`: Journal 読み取り位置の永続化に伴い、`event` レコードへ
  `replay: bool` を追加した ABI 破壊的変更
- `0.2.0` → `0.3.0`: ドライバ機能の追加(`bus` / `bus-host` / `bus-types`
  インターフェースと `driver` / `driver-guest` world の新設、`plugin` world への
  `bus` import 追加)に伴う ABI 破壊的変更
- `0.3.0` → `0.4.0`: 定期実行・終了フックの追加(`plugin` world への
  `on-schedule` / `on-stop` export 追加)に伴う ABI 破壊的変更
- `0.4.0` → `0.5.0`: 非同期 HTTP の追加(`driver-http` への `submit-send`、
  `plugin` world への `on-job-complete` export 追加)に伴う ABI 破壊的変更
  (下記「非同期 HTTP」参照)

**旧 world でビルド済みのプラグインは新しいホストへのロードに失敗する**。
プラグインを新しい `core/wit` に対して再ビルドすること(Rust は
`wit_bindgen::generate!` がパス指定なら自動追随、Go/TinyGo は
`wit-bindgen-go generate` での `gen/` 再生成が必要)。

## ログレベル(`host-log`)

プラグイン/ドライバが `host-log` に出したログは、そのままデーモンの
`tracing` イベントになる(`core/src/plugin/host.rs`)。WIT のレベルは
1 対 1 で対応する:

| `host-log` のレベル | デーモンのログ |
| --- | --- |
| `error` | `ERROR` |
| `warn` | `WARN` |
| `info` | `INFO` |
| `debug` | `DEBUG` |

**既定では `info` 以上だけが出る。** 閾値は環境変数 `RUST_LOG` で上書きでき、
`RUST_LOG` が未設定・空・書式不正のときは `info` にフォールバックする
(ログ指定のミスで起動できなくなることはない)。

    # プラグインの debug ログまで出す
    RUST_LOG=debug cargo run -p edlr-core --bin edlr -- --journal-dir <PATH>

    # プラグインホストのログだけ debug、他は info のまま
    RUST_LOG=info,edlr_core::plugin::host=debug cargo run -p edlr-core --bin edlr

閾値はデーモンのレジストリ全体に掛かるので、**stderr と GUI の Logs 画面は
必ず同じものを見る**。`RUST_LOG=debug` を付ければ GUI にも debug 行が出る。

プラグイン単位でレベルを変える口は今のところ無い(`RUST_LOG` の
ターゲット指定はホスト側モジュール名に対するもので、プラグイン id では
絞れない)。

## 非同期 HTTP(`driver-http.submit-send` / `on-job-complete`)

```wit
// interface driver-http
submit-send: func(req: request, timeout-ms: option<u32>) -> result<u64, driver-error>;

// world plugin
export on-job-complete: func(job-id: u64, result-json: string);
```

`submit-send` はリクエストを受け付けて**即座に** job-id(u64、1 始まり)を
返す。ホストは呼び出しの中で HTTP を待たないので、同期 `send` と違い
呼び出し期限(2 秒)を通信時間で消費しない。結果は完了後に
`on-job-complete` export へ届く。

**同期で返るエラー**(受け付け自体の拒否。この場合ジョブは始まらず、
`on-job-complete` も呼ばれない):

- `permission-denied` — `send` と同一の承認判定(`[[capabilities]]` の
  hosts 許可リスト)
- `transport`(キュー満杯の旨)— 未完了の submit が **8 件**(1 インスタンス
  あたりの上限)ある間の新規 submit。ブロックはしない。完了を待ってから
  再試行すること

**`result-json` の形**(`on-job-complete` の第 2 引数):

```json
{"ok": {"status": 200, "headers": [["content-type", "application/json"]], "body-base64": "..."}}
{"err": {"kind": "transport", "message": "..."}}
```

- body はバイト列なので base64 で運ぶ。`kind` は `transport` か
  `invalid-request`(承認拒否は同期エラーになるためここには現れない)
- タイムアウト: `timeout-ms` を省略(`none`)すると 30_000ms。上限
  60_000ms でクランプ。レスポンスサイズ上限は同期 `send` と同じ
- **順序保証**: 完了通知は journal イベント・バス配信と同じ 1 本の作業
  キューに FIFO で混ざり、他の export 呼び出しと直列に届く。複数ジョブの
  完了順は HTTP の完了順であって submit 順ではない
- **旧世代の破棄**: プラグインインスタンスがホストにより作り直された場合
  (呼び出し期限超過からの復帰)、それ以前に submit したジョブの完了は
  **届かない**(wasm 線形メモリごと状態が失われているため)。届かなかった
  ジョブを追跡したい場合は、submit した内容をプラグイン側で永続化しておく
  こと

**同期 `send` は残る**。ただし `send` は呼び出し期限(2 秒)の中で応答を
待つので、lifecycle 呼び出し(`init` / `on-event` / `on-schedule` など)の
中で遅いホストを同期的に待つのは自己責任 — 期限を使い切ると呼び出しは
中断され、繰り返せばプラグインは無効化される。応答が 1.5 秒(同期 send の
タイムアウト)を超えうる相手には `submit-send` を使うこと。

使用例は `examples/plugins/http-caller` を参照。

## plugins-dir のレイアウト

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
| `[[settings]]` | | 設定項目。`type` は `boolean` / `string` / `number` / `select` / `secret` / `map` のいずれかで、それぞれ `key` / `label` / `default`(select はさらに `options` か `options-from`)を持つ。`secret` と `map` は `default` を取らない(下記) |
| `[[capabilities]]` | | HTTP 通信の要求([capabilities.md](capabilities.md#capabilitydriver-http)) |
| `[[sidecar]]` | | サイドカープロセスの要求([capabilities.md](capabilities.md#サイドカープロセスdriver-process)) |
| `[[filesystem]]` | | ファイルアクセスの要求([capabilities.md](capabilities.md#ファイルアクセスdriver-fs)) |
| `[[bus]]` | | ドライバとのバス接続の要求([drivers.md](drivers.md#プラグイン側の-bus-書式と承認フロー)) |
| `[[schedule]]` | | 定期実行の宣言(下記「スケジュール」参照) |

設定値は `<settings-dir>/<id>.json` に保存され、未保存キーは manifest の
`default` にフォールバックする。

### トップレベルキーはテーブルヘッダより前に書く

TOML では、テーブルヘッダ(`[[settings]]` など)より**後ろ**に書いたキーは
そのテーブルの子として解釈される。つまり:

    [[sidecar]]
    name = "worker"
    reason = "..."
    port = 51000

    events = ["FSDJump"]   # ← sidecar[0].events になる。トップレベルではない

edlr はこれを `[[sidecar]]` の知らないキーとしてロード時に拒否する
(`unknown field \`events\``)。同様に `[[settings]]` / `[[capabilities]]` /
`[[filesystem]]` / `[[bus]]` / `[[dashboard]]` / `[[schedule]]` /
`[[topics]]`(`driver.toml`)も知らないキーを拒否する。

トップレベル自体の綴り違い(`evens = [...]` など)はロードを失敗させず、
warn ログで報せる。ロード時には読み取り結果のサマリも info ログに出るので、
宣言したはずの項目が消えていないかはここで確認できる:

    plugin manifest loaded id="sample-plugin" events=1 settings=3 capabilities=0 ...

### ドロップダウン(`type = "select"`)

候補の書き方は 2 通りあり、**どちらか一方だけ**を書く(両方書いても、どちらも
書かなくてもロードは失敗する)。

**静的な候補(`options`)** — 選べる値がマニフェストを書く時点で決まっている場合:

    [[settings]]
    key = "mode"
    label = "モード"
    type = "select"
    default = "quiet"
    options = ["quiet", "verbose", { value = "debug", label = "デバッグ(冗長)" }]

要素は文字列でも `{ value, label }` でもよい。文字列を書いた場合は表示名と
保存値が同じになる。

**動的な候補(`options-from`)** — 選べる値がインストール環境で決まる場合。
ドライバが retain トピックへ載せた値を候補として引く:

    [[settings]]
    key = "default-speaker"
    label = "既定の話者"
    type = "select"
    default = ""
    options-from = { driver = "coeiroink", topic = "speakers" }

ドライバ側は、そのトピックへ JSON 配列を `emit` する。要素は文字列か
`{"value":..,"label":..}`:

    [{"value": "a1b2:0", "label": "アメノちゃん/ノーマル"},
     {"value": "a1b2:3", "label": "アメノちゃん/ギャル"}]

押さえておくべき点:

- **`[[bus]]` の宣言も承認も要らない。** ここで retained 値を読むのは
  プラグインではなくデーモンで、返す先は設定画面である。プラグイン自身が
  同じトピックを読みたいなら、これまで通り `[[bus]]` の宣言と承認が要る
- **候補は保存時に照合されない。** 候補は非同期に届き、ドライバの無効化で
  消えもするので、照合すると「ドライバが起動するまで設定を保存できない
  時間帯」ができてしまう。値は文字列であることだけ検証される
- **候補は設定画面を開いたときに取得される。** ドライバを起動した直後に
  候補を出したい場合は画面を開き直す
- 候補を取得できないとき(ドライバが未インストール・無効化済み・まだ一度も
  `emit` していない)、UI は現在値だけを表示して編集不可にし、どのドライバの
  どのトピックが未着かを表示する。保存済みの設定値は消えない
- `options-from` が指すドライバやトピックが実在するかは、マニフェストの
  ロード時には検証しない(ドライバの後入れ・入れ替えを許すため)

`options-from` は `driver.toml` の `[[settings]]` でも同じように使える。
ドライバが自分で `emit` したトピックを、自分の設定の候補源にしてよい。

### 秘密情報(`type = "secret"`)

API キーのように UI から読み出せてはいけない設定はこの型で宣言する。

    [[settings]]
    key = "apiKey"
    label = "INARA API キー"
    type = "secret"

`string` との違いは扱いだけで、保存形式は同じ文字列:

- **`default` を書けない**(マニフェストに秘密情報を書ける余地を作らないため)。
  値は常に空文字列から始まる
- UI ではマスク入力(`<input type="password">`)になる。保存済みでも入力欄は
  空のままで、プレースホルダが「設定済み」かどうかだけを示す。空のまま離れても
  保存はしない(開いて閉じるだけで消えてしまわないように)
- **`plugins/list` / `plugins/get-settings` / `plugins/set-settings` の応答には
  値が含まれない**(write-only)。代わりに `plugins/list` が `secretsSet` として
  「空でない値が保存済みのキー」の一覧を返す
- プラグイン自身は `host-settings.get-all` で通常どおり値を受け取る
  (渡す相手はそのプラグインなので、ここで隠したら意味が無い)

**ディスク上は平文である**。`<settings-dir>/<id>.json` の保護は OS の
パーミッションに委ねており、OS のキーリング等との連携は未対応。この型が守るのは
「UI や RPC 越しに秘密情報が読み出せてしまう」経路。

### 項目が増減する設定(`type = "map"`)

「システム名 → 表示名」の置き換え表のように、**何件になるかがユーザー次第**の
設定はこの型で宣言する。UI に行の追加・削除ボタンが出て、キーと値のペアを
その場で増やせる。

    [[settings]]
    key = "aliases"
    label = "表示名の置き換え"
    type = "map"

- 値は **`string -> string` の JSON オブジェクト**に限る。値に数値・真偽値・
  入れ子のオブジェクトは書けない(受け取る側が行ごとに形を場合分けせずに
  済むようにするため。必要になれば後から広げられる)
- **`default` を書けない**(書くとマニフェストのロードが失敗する)。何件の
  ペアが要るかを決めるのはユーザーなので、値は常に空オブジェクト `{}` から
  始まる
- **キー名に制約は課さない**(空白や記号を含んでよい)。ただし**空文字列の
  キーは拒否する**。UI 側でも、キーが空の行は保存対象から外し、キーが重複
  している間は保存せずその場で報せる
- プラグインへは `host-settings.get-all` の JSON にそのままオブジェクトとして
  載る(`{"aliases": {"Sol": "太陽系"}}`)。`secret` と違い読み出し応答からも
  隠されない

## スケジュール(`[[schedule]]`)

プラグインは Journal/Status イベントが届いたときにしか動けないが、
`[[schedule]]` を宣言すると、その名前を引数に `on-schedule` が定期的に
呼ばれる。デーモンの graceful shutdown 時には `on-stop` が一度だけ呼ばれる。

    [[schedule]]
    name = "flush"
    interval-seconds = 60

    [[schedule]]
    name = "daily-report"
    cron = "0 9 * * *"
    catch-up = true

- `name` は `[a-z0-9-]+` にマッチする必要があり、同一 manifest 内で重複しては
  ならない(違反時は manifest のロードに失敗する)
- `interval-seconds` と `cron` は**どちらか一方だけ**を指定する。両方指定・
  どちらも未指定はどちらも manifest のロード失敗になる
- `cron` は 5 欄形式(分 時 日 月 曜日)を **ローカル時刻**で評価する
  (edlr 内部では 7 欄形式の `cron` クレートを使っており、秒は常に 0・年は
  常に `*` を補って変換している)
- 発火間隔には 5 秒の下限があり、`interval-seconds` がそれを下回る場合は
  5 秒へ丸められ、warn ログが出る(manifest 自体は失敗しない)
- デーモンが長時間ブロックしていた等の理由で発火予定を複数回取りこぼしても、
  次に評価されたタイミングで 1 回だけ `on-schedule` が呼ばれる(取りこぼした
  回数ぶん連続では呼ばれない)
- `catch-up = true`(**`cron` にのみ指定可能**、既定は `false`)を宣言すると、
  **デーモンが動いていなかった間に過ぎた定刻**を次回起動時に 1 回だけ
  追い掛けて実行する。詳細は下記「打ち漏らしの追い掛け実行」参照
- `on-stop` は**ベストエフォート**であり、有界の猶予時間内(既定 5 秒)に
  収まった場合にしか呼ばれない。停止の合図はワークキューを**追い越す**ので、
  キューに積み残しが多いだけなら `on-stop` へ到達できる(積み残したイベント
  やバス配信は破棄される -- どのみちプロセスは直後に終了するため、最後の
  flush を優先する)。ただし終了時にちょうど wasm 呼び出しが実行中で、それが
  猶予時間を超えて返らない場合は、`on-stop` へ辿り着けずにデーモンが終了する
  ことがある(その場合 warn ログが出るのみ)
- `on-stop` は**デーモンの graceful shutdown のときだけ**呼ばれる保証で、
  trap によるプラグインの無効化(disable)の後には呼ばれない。`SIGKILL` や
  プロセスのクラッシュではシグナルハンドラそのものが動かないため、当然
  呼ばれない。graceful shutdown であっても上記のとおり有界の猶予時間内に
  限った best-effort であることに注意

### 打ち漏らしの追い掛け実行(`catch-up`)

スケジュール状態はプラグイン起動のたびに新規構築されるため、既定では
`cron = "0 9 * * *"` の日次レポートは、09:00 にデーモンが動いていなかった日は
単にスキップされ、ログにも UI にも痕跡が残らない。flush 系のスケジュールには
これで問題ないが、レポート系には不適切なので、`catch-up = true` で opt-in する。

    [[schedule]]
    name = "daily-report"
    cron = "0 9 * * *"
    catch-up = true

挙動:

- 発火するたびに、その時刻を `<settings-dir>/<plugin-id>.schedule.json` へ
  記録する(`catch-up` を宣言したスケジュールの分だけ)
- 起動時、記録済みの最終発火より後に過ぎた定刻があれば、直ちに 1 回
  `on-schedule` を呼ぶ
- **何回打ち漏らしていても発火は 1 回**。3 ヶ月動かしていなくても、起動時に
  日次レポートが 90 通飛ぶことはない
- **記録が無い場合(初回起動、ファイル破損)は追い掛けない**。「起動しただけで
  過去の定刻が 1 回走る」より「1 回取りこぼす」方が害が小さいため

`interval-seconds` には指定できない(manifest のロードに失敗する)。
`interval-seconds` は「前回から N 秒後」という経過時間の宣言であって
「何時に実行する」ではないため、追い掛けるべき定刻が存在しない。

## 設定画面のレイアウト(layout.kdl / layout.json)

`[[settings]]` の平坦なリストは、設定画面ではそのまま上から順に並ぶだけで
グループ化や説明文を挟めない。**データ構造(`[[settings]]`)はそのままに、
「どう見せるか」だけ**を `manifest.toml` と同じディレクトリに置く
`layout.kdl`(または `layout.json`)で別に記述できる。どちらも任意で、
無くても現行どおりの平坦フォームが表示される。

    <plugins-dir>/
      <id>/
        manifest.toml
        plugin.wasm
        layout.kdl      (任意)

`layout.kdl` の語彙は v1 では最小限で、セクションの入れ子と `field` 参照だけ:

    section "接続" description="COEIROINK サーバへの接続設定" {
        field "endpoint"
        field "api-key"
    }
    section "読み上げ" {
        field "voice"
        field "speed"
        section "詳細" {
            field "pitch"
        }
    }

- `section` — `title`(最初の位置引数、必須)、`description`(名前付き引数、
  任意)を持ち、子として `field` / `section` を任意個持てる(入れ子可)
- `field` — `[[settings]]` の `key` への参照。型・label・default はここには
  持たず、あくまで manifest 側の値を「どこに置くか」だけを指定する

機械生成向けに同形の `layout.json` も使える(内容は上と同じもの):

    {
      "sections": [
        {
          "title": "接続",
          "description": "COEIROINK サーバへの接続設定",
          "children": [{ "field": "endpoint" }, { "field": "api-key" }]
        },
        {
          "title": "読み上げ",
          "children": [
            { "field": "voice" },
            { "field": "speed" },
            { "title": "詳細", "children": [{ "field": "pitch" }] }
          ]
        }
      ]
    }

### lenient な挙動

manifest.toml と違い、layout ファイルの不備は**プラグインのロードを一切
妨げない**。壊れていれば警告ログを出して読める範囲を使うか、丸ごと諦めて
平坦フォームへフォールバックする:

| 状況 | 挙動 |
| --- | --- |
| layout ファイルが無い | `layout: null`。UI は現行どおり平坦フォーム |
| パース失敗(KDL/JSON とも) | 警告ログ + `layout: null` → 平坦フォーム |
| 未知のノード名・属性 | 警告ログを出してそのノード/属性を無視、残りは使う |
| 存在しない settings キーへの `field` 参照 | 警告ログを出してその参照を捨て、残りは描画 |
| `.kdl` と `.json` が両方存在 | 警告ログを出して `.kdl` を採用 |
| どのセクションにも載らなかったキー | 末尾の暗黙セクション(「その他」)に自動で入る。**書き忘れで項目が消えることはない** |
| 同じキーが複数回参照された | 警告ログを出して最初の出現だけ残す |

つまり、`layout.kdl` に書き忘れたキーがあっても設定項目そのものが消えることは
なく、末尾の「その他」セクションに現れる。実例は
[`examples/plugins/tutorial-jump-log-rs/layout.kdl`](../examples/plugins/tutorial-jump-log-rs/layout.kdl)
を参照。

## サンプルのビルドと配置(スクリプト)

`manifest.toml` / `driver.toml` を同梱しているサンプル(`state-reader`、
`inara-uploader`、ドライバの `ed-state`)は、まとめてビルド・配置できる:

    ./scripts/install-examples.sh              # 全件
    ./scripts/install-examples.sh state-reader # 指定したものだけ
    ./scripts/install-examples.sh --list       # 対象一覧
    ./scripts/install-examples.sh -n           # 何をするかだけ表示

配置先は既定で `$XDG_CONFIG_HOME/edlr/{plugins,drivers}`(未設定なら
`~/.config/edlr/...`)。`--plugins-dir`/`--drivers-dir` を付けて起動している
デーモンには `--prefix` を合わせる。

- **設定値と承認状態は消えない**(それらは settings-dir / grants-dir 側にあり、
  スクリプトが触るのは wasm と manifest だけ)
- **配置後はデーモンの再起動が要る**(プラグインのロードは起動時に一度だけ)

以下は手作業で行う場合の手順。

## hello-logger サンプルのビルドと配置

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

## プラグイン設定 RPC

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
            "values": { "enabled": true, ... },  /* 現在の設定値 */
            "dropped": { "events": 0, "busDeliveries": 0 }  /* 下記 */
          }
        ]
      }

  `dropped` は作業キューが満杯だったために捨てられた件数(デーモン起動時
  からの累計)。journal の読み取り位置は配送の成否と独立に進むため replay でも
  戻らず、バス配信も再送されないので、この数はそのまま失われたイベント数を
  意味する。

## プラグインが無効化される条件

wasm 呼び出しが失敗したとき、ホストは 2 つの原因を区別する:

- **トラップ**(不正なメモリアクセス、panic、ホスト側エラーなど)-- 次に
  呼んでも同じ結果になる決定的な故障なので、**1 回で** `Disabled` にする
- **呼び出し期限(2 秒)の超過** -- 原因はプラグイン作者の管理下にないこと
  (応答しない HTTP ホスト、レジューム直後の詰まり)でありうるので、
  プラグインを作り直して(`init` からやり直して)処理を続ける。
  **3 回連続**で超過して初めて `Disabled` にする

`Disabled` の理由は `plugins/list` の `reason` に入り、Plugins ページに
そのまま表示される(期限超過とトラップは文面で区別できる)。

期限超過からの作り直しでは、プラグインが wasm 線形メモリ上に持っていた状態は
失われる(epoch 割り込みでトラップしたインスタンスは wasmtime に毒扱いされ、
再利用できないため)。

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
