# ドライバ(プラグイン間連携)

**ドライバはプラグインとは別レイヤーの wasm コンポーネント**で、以下の点が
プラグインと異なる:

- **ドライバは journal / status イベントを受け取らない**(`export` するのは
  `init` と `on-message` のみで、`on-event` が無い)。プラグインが `driver-http`
  / `driver-process` / `driver-fs` を使って外部と話すのと違い、ドライバは
  プラグイン同士を仲介する結節点に徹する
- **1 ドライバにつき常駐インスタンスは 1 つ**。複数プラグインが同じドライバへ
  同時に `publish` しても、宛先は常にこの単一インスタンス(プラグインが
  複数インスタンスに分かれてロードされるのとの違い)
- **ドライバ間の通信は無い**。ドライバがビルド時に対象にする world
  (`driver`)は `bus`(発行 API)を import しない設計になっており、
  あるドライバが別のドライバの `[[topics]]` へ触ることは構造的にできない
  (`core/wit/plugin.wit` の `world driver` のドキュメントコメント参照)。
  ドライバが公開できるのは自分の `[[topics]]` への `emit` だけ

ドライバは `--drivers-dir`(既定 `$XDG_CONFIG_HOME/edlr/drivers`、未設定なら
`~/.config/edlr/drivers`)から読み込まれる。`--plugins-dir` と同様、
指すディレクトリが存在しなくてもエラーにはならず、ドライバ 0 件で起動する。

ドライバの設定・承認はプラグインとは別の名前空間に保存される — プラグイン
`id` とドライバ `id` は衝突しうる別物であるため。設定は
`<settings-dir>/drivers/<id>.json`(通常のプラグイン設定
`<settings-dir>/<id>.json` とは別ディレクトリ)、承認は
`<grants-dir>/drivers/<id>.json`(通常のプラグイン承認 `<grants-dir>/<id>.json`
とは別ディレクトリ)に保存される。

## drivers-dir のレイアウト

    <drivers-dir>/
      <id>/
        driver.toml
        driver.wasm   (driver.toml の entry で指すファイル名。任意の名前でよい)

- ディレクトリ名(`<id>`)は `driver.toml` の `id` と一致していなければならない
- `id` は `[a-z0-9-]+` にマッチする必要がある(プラグイン `id` と同じ字種)

`driver.toml` の主なフィールド:

| フィールド | 必須 | 説明 |
| --- | --- | --- |
| `id` | ✓ | ドライバ ID。ディレクトリ名と一致必須 |
| `name` | ✓ | 表示名 |
| `version` | ✓ | バージョン文字列 |
| `description` | | 説明文(省略可) |
| `entry` | ✓ | `drivers-dir/<id>/` からの相対パスで wasm ファイルを指す |
| `[[topics]]` | | 公開するトピックの配列。各要素は `name`(`[a-z0-9-]+`、ドライバ内で一意)・`retain`(bool、既定 `false`)・`description` を持つ |

`retain = true` のトピックは、直近 `emit` した値をドライバが持ち続け
(「retained 値」)、承認済みプラグインが `bus.get` でいつでも読める。
`retain = false` のトピックは配信専用で、値は保持されない
(`bus.get` は常に `none`)。

retained 値は **`select` 設定の候補源**としても使える。プラグインや他の
ドライバの `[[settings]]` が `options-from = { driver = "...", topic = "..." }`
でトピックを指すと、設定画面のドロップダウンがその値から作られる。「選べる
ものはインストール環境で決まる」設定(音声合成の話者一覧など)のための経路で、
プラグイン側の `[[bus]]` 宣言・承認は要らない(読むのはデーモンであって
プラグインではないため)。詳細は
[plugins.md](plugins.md#ドロップダウンtype--select)。

`driver.toml` は `manifest.toml` と同じく、`[[settings]]` / `[[capabilities]]` /
`[[sidecar]]` / `[[filesystem]]` / `[[topics]]` の知らないキーを拒否する。
トップレベルキーをテーブルヘッダより後ろに書いてしまう事故については
[plugins.md](plugins.md#トップレベルキーはテーブルヘッダより前に書く)を参照。

ドライバも `driver.toml` と同じディレクトリに `layout.kdl` / `layout.json` を
置けば、設定画面をセクション分けして描画できる。仕組み・語彙・lenient な
エラー処理はプラグインと共通なので、詳細は
[plugins.md](plugins.md#設定画面のレイアウトlayoutkdl--layoutjson)を参照。

## プラグイン側の `[[bus]]` 書式と承認フロー

プラグインが特定のドライバと話すには、`manifest.toml` に接続先ごとの
`[[bus]]` を宣言する:

    [[bus]]
    driver = "ed-state"
    publish = ["set-system"]
    subscribe = ["current-system"]
    reason = "why this plugin needs to talk to this driver"

- `driver` はドライバ ID
- `publish` / `subscribe` はそれぞれ `bus.publish` / `bus.get` ・購読配信で
  触ってよいトピック名の配列(少なくとも一方は 1 件以上必須)。宣言していない
  トピックへのアクセスは、承認済みでも `permission-denied` になる
- `reason` は空文字不可。承認画面でユーザーに表示される、人間可読の理由文
- 同じプラグインが同じ `driver` を 2 回以上宣言することはできない

承認フローは `driver-http` capability と同じ形(`plugins/set-bus-grant` RPC、
Plugins UI):

- **既定では未承認**。未承認の間、その接続先への `bus.publish` / `bus.get`
  呼び出しは全て `permission-denied` になる(プラグイン自体は動き続ける)
- 承認/取消は接続先(`driver`)単位。マニフェストの要求内容(`publish` /
  `subscribe` の集合)が変わると、以前の承認は自動的に失効(stale)する
- 承認/取消は稼働中のプラグインにも即座に反映される(再起動不要)。ただし
  **`subscribe` の購読登録自体は承認の有無に関わらず起動時に行う** —
  配信のたびに承認を再確認して転送するかどうかを決めるので、後から承認
  されても購読を登録し直す必要が無い
- **プラグインが宣言する `driver` がインストールされていない、または宣言
  したトピックをそのドライバが公開していない場合(「未解決」)、プラグインは
  それでも `Running` のままロードされる**。warn ログには出るが、ロード
  自体は失敗しない(ドライバの後入れ・入れ替えを許すため)。未解決の接続
  への `bus.publish` / `bus.get` は、まず承認チェック(`check_bus`)を通る
  ため、**未承認のままなら(未解決かどうかに関わらず)`permission-denied`
  になる**。承認済みで、なお未解決の場合にだけ、そのドライバ/トピックが
  実在しないことに起因する `unknown-driver` / `unknown-topic` になる

## `publish` は fire-and-forget、`get` はホスト側の retained 値を読む

- **`bus.publish(driver, topic, payload)`** はドライバの `on-message` へ
  メッセージを 1 件投げ込むだけで、ドライバの処理結果を待たない
  (fire-and-forget)。呼び出し元プラグインが受け取れるのは「キューに
  積めたか」だけで、ドライバが実際にどう処理したかは知らない
- **`bus.get(driver, topic)`** はドライバを一切呼び出さない。ホスト
  (`edlr_driver_channel::Bus`)側に保持されている retained 値
  (直近その `topic` へ `emit` された値)をそのまま返すだけの、同期・
  非ブロッキングな読み取り。`retain = false` のトピックは常に `none`

ドライバ側が「配り直す」経路(`bus-host.emit`)と、プラグインが「渡す」
経路(`bus.publish`)は別方向・別 API であることに注意: `examples/drivers/ed-state`
は `set-system` を `publish` で受け取り、`current-system` として `emit` で
配り直す 2 段構成になっている(下記参照)。

## キュー方針の非対称

`publish` 方向(プラグイン → ドライバ)と `emit` の配信方向(ドライバ →
購読プラグイン)とでは、キューが溢れたときの挙動が非対称:

- **`publish` はキューが満杯なら `queue-full` エラーを返し、メッセージを
  捨てない**。`publish` は結果を返せる同期呼び出しなので、呼び出し側
  (プラグイン)が再送するか諦めるかを選べる
- **`emit` の配信(購読プラグインへの転送)は、宛先プラグインのキューが
  満杯なら黙って捨てる。ただし捨てるのは新しい方(今まさに送ろうとしている
  配信)であって、古い方(キューに既に並んでいる配信)ではない。**
  `emit` は内部で `std::sync::mpsc::SyncSender::try_send` を使っており、
  `Full` のとき捨てられる対象は必ず送信しようとした値そのもの
  (`TrySendError::Full` が呼び出し元に返す値)である -- `try_send` には
  キューの中身を覗いて既に並んでいる古い要素を追い出す手段が無いため、
  「古い方から捨てる」形は実装できない。`emit` はドライバ起点のプッシュ
  配信で呼び出し元に返すエラーが無く、遅い/詰まった 1 プラグインのために
  ドライバ全体を止めるわけにもいかないため、黙って捨てる方針自体は変えない
- **この非対称は `retain = false` のトピックで特に効く**。詰まった購読
  プラグインは、キューが空くまでの間に来た更新のうち最新のものから順に
  取りこぼし続け、受け取れるのは古いプレフィックスだけになる。しかも
  `retain = false` のトピックには `bus.get` のフォールバックが無い(常に
  `none`)ので、その間に来た更新を後から拾い直す手段が一切無い。プラグイン
  作者はこの挙動を踏まえ、詰まりうる用途では `retain = true` にするか、
  受信側のキュー処理を速く保つ必要がある
- **一方 retained 値の更新は、配信の成否とは独立に必ず行われる**。配信を
  取りこぼした購読プラグインも、`retain = true` のトピックなら次の `bus.get`
  で最新値を拾い直せる

## 非同期 HTTP(`driver-http.submit-send` / `on-job-complete`)

WIT 0.6.0 から、ドライバもプラグインと同じ submit/complete プロトコルを
使える。`driver-http.submit-send(req, timeout-ms)` は即 job-id を返し、
結果は `world driver` の `on-job-complete` export へ非同期に届く。
API・`result-json` の形・in-flight 上限(8)・タイムアウト規定は
プラグイン側と同一なので [plugins.md の「非同期 HTTP」](plugins.md#非同期-httpdriver-httpsubmit-send--on-job-complete)
を参照。

ドライバ固有の注意:

- 完了通知は `on-message` と同じ 1 本の作業キューに FIFO で混ざる
  (メッセージ処理が 1 スレッドに直列化される性質はそのまま)
- 同期 `send` のタイムアウトはドライバでは 25 秒(呼び出し期限 30 秒)
  なので、プラグインほど切迫はしないが、`on-message` の処理中に同期で
  待った時間はそのままキューの詰まりになる。TTS のような遅い呼び出しは
  `submit-send` に逃がすとキューが流れ続ける

## 予約 driver id: `host` と `dashboard`

プラグインの `on-message(driver, topic, payload)` に届く `driver` には、
実ドライバ以外に 2 つの予約 id がある(なりすましを塞ぐため、manifest 検証は
この id を持つプラグイン/ドライバを拒否する):

- **`host`** — ホストが合成する通知(`sidecar-ready` など)の送信元
- **`dashboard`** — ダッシュボードウィジェットのボタン等から
  `api.action(name)`(ウィジェットの `mount(el, api)` に渡る API)で
  要求されたアクション。
  `plugins/dashboard-action` RPC 経由で、そのウィジェットが属するプラグイン
  自身へ `on-message("dashboard", name, [])` として届く。grant 済み
  ウィジェットからのみ受け付け、他プラグインへは送れない。配送は
  fire-and-forget で、キュー満杯時は RPC がエラーを返す

## ドライバ無効化時の retained 値の破棄

ドライバが `on-message`/`init` の失敗で無効化(`Disabled`)されると、その
ドライバが持っていた retained 値は全て破棄される。以後そのドライバへの
`bus.get` / `bus.publish` はどちらも `driver-unavailable` エラーになる。

これは意図的な fail-open 対策: 死んだドライバの古い値を `bus.get` が返し
続けると、プラグイン側は「まだ更新が来ていないだけ」なのか「もう誰も
更新しない」なのかを区別できず、古い値を握ったまま動き続けてしまう。

## `ed-state` サンプルの使い方

`examples/drivers/ed-state` は受け取った `set-system` メッセージを
retained トピック `current-system` として配り直すだけのサンプルドライバ、
`examples/plugins/state-reader` は `FSDJump` を見たら `ed-state` へシステム名を
`publish` し、配り直された `current-system` を `on-message` で受け取る
サンプルプラグイン。

    rustup target add wasm32-wasip2   # 未追加なら
    cd examples/drivers/ed-state && cargo build --release --target wasm32-wasip2
    cd ../../plugins/state-reader && cargo build --release --target wasm32-wasip2

    # drivers-dir に配置する
    mkdir -p ~/.config/edlr/drivers/ed-state
    cp examples/drivers/ed-state/target/wasm32-wasip2/release/ed_state.wasm \
       ~/.config/edlr/drivers/ed-state/driver.wasm
    cp examples/drivers/ed-state/driver.toml ~/.config/edlr/drivers/ed-state/

    # plugins-dir に配置する
    mkdir -p ~/.config/edlr/plugins/state-reader
    cp examples/plugins/state-reader/target/wasm32-wasip2/release/state_reader.wasm \
       ~/.config/edlr/plugins/state-reader/plugin.wasm
    cp examples/plugins/state-reader/manifest.toml ~/.config/edlr/plugins/state-reader/

    cargo run -p edlr-core --bin edlr -- --journal-dir <PATH>

起動後、Plugins UI(または `plugins/set-bus-grant` RPC)で `state-reader` の
`ed-state` への接続を承認すると、`FSDJump` のたびにシステム名が `ed-state`
経由で配り直され、`state-reader` の `on-message` ログに現れる。
