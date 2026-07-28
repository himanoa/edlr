# edlr プラグイン間連携機構(bus とユーザー定義ドライバ)設計書

2026-07-27 承認。プラグイン同士が連携するための機構を、ホスト組み込みのブローカーではなく
**ユーザーが配布・インストールできるドライバ**として実現する。

ドライバはプラグインとは**別レイヤーの常駐 wasm コンポーネント**で、ホストが 1 インスタンス
だけ起動して所有する。ゲームのイベント(journal / status)は受け取らず、反応するのは
プラグインから届くメッセージだけ。プラグインとドライバの間はホストが仲介する固定の
ABI(`bus` / `bus-host`)で繋がり、宛先ドライバ ID・トピック名・ペイロードは文字列と
バイト列で運ぶ。

`driver-http`(2026-07-26)・`driver-process`(2026-07-26)・`driver-fs`(2026-07-26)が
「プラグインがホストの特権資源に触るための capability」だったのに対し、本設計は
「プラグイン同士が互いに触るための経路」を足すものになる。承認モデル(manifest 宣言 →
ユーザー承認 → 呼び出し時照合)は 3 者と同じ型に載せる。

## 動機

プラグイン間連携をホスト組み込みの固定ブローカーにせず、ユーザー定義ドライバにする理由は
4 つあり、いずれも「コードとスキーマとセマンティクスを一体で配布したい」に集約される。

- **中継の途中にロジックを挟みたい** — 単なる土管ではなく、複数プラグインの出力を集約し、
  変換・フィルタする「加工する中継者」。だからコードを書ける実体である必要がある
- **通信セマンティクスを差し替えたい** — 永続化する / しない、保持する / しない、といった
  選択を用途ごとに変えたい。ドライバの実装がその選択そのものになる
- **コアに機能を足さずに拡張したい** — edlr 本体をリリースし直さずにユーザーが連携機構を
  足せる状態が欲しい
- **プラグイン間の「共通語彙」を配布したい** — 例えば「ED の現在状態」というトピック体系を
  ドライバが定義し、それを介して互いを知らないプラグイン同士が噛み合う

## 中核の方針

- **ドライバはプラグインとは別の第一級の概念**。置き場・マニフェスト・設定・承認・UI を分ける。
  ドライバは journal / status イベントを購読しない。「イベントの消費者(プラグイン)」と
  「プラグイン間の仲介者(ドライバ)」を役割として切る
- **共有状態はドライバの中ではなくホストが持つ**。retained 値をホスト側のストアに置くことで、
  読み出し(`get`)がドライバの wasm を一切呼ばない
- **プラグイン → ドライバの送信は fire-and-forget**。キューに積んで即戻るので、呼び出し側が
  ドライバの処理時間に引きずられない
- **ドライバ間の呼び出しは無い**。ドライバは `bus` を import しないので循環が構造的に起きない
- **未解決の参照は起動を止めない**。参照先ドライバが未インストールでもプラグインは動く。
  ただし warn ログと UI バッジで必ず可視化する

### 責務の分担

| 置き場所 | 内容 | 誰が決めるか |
|---|---|---|
| ドライバの `driver.toml` の `[[topics]]` | トピックの**定義**(`name` / `retain` / `description`) | ドライバ作者 |
| プラグインの `manifest.toml` の `[[bus]]` | 接続の**要求**(`driver` / `publish` / `subscribe` / `reason`) | プラグイン作者 |
| `<grants-dir>/<plugin-id>.json` | プラグインのどの接続要求を承認したか | ユーザー(UI) |
| `<grants-dir>/drivers/<driver-id>.json` | ドライバ自身の capability をどこまで承認したか | ユーザー(UI) |
| `drivers/channel` crate | 購読解決・retained ストア・キュー方針 | — |

`drivers/http` / `drivers/process` / `drivers/fs` と対称の構造。純ロジックを `drivers/channel`
に閉じ込め、`core` 側は wasm インスタンスの生成と結線に集中する。

## ABI(WIT)

WIT パッケージは `edlr:plugin@0.2.0` から **`edlr:plugin@0.3.0`** に上げる。`world plugin` に
import と export が増えるため、**`@0.2.0` でビルド済みの既存プラグインは新しいホストへの
ロードに失敗する**。全プラグインの再ビルドが必要(`@0.1.0` → `@0.2.0` のときと同じ扱い)。

`bus-error` は関数を持たない `bus-types` interface に切り出す。`bus` や `bus-host` のように
関数を持つ interface から `use` で型だけを借りると WIT の `use` 意味論上その interface 全体への
依存になり、`bus-host` だけを import したいはずの `world driver` に `bus` ごと引き込まれて
ドライバから `bus.publish` を呼べてしまう。それを避けるため、型だけの `bus-types` を両者が
`use` する形にする。

```wit
interface bus-types {
  variant bus-error {
    permission-denied(string),
    unknown-driver(string),
    unknown-topic(string),
    driver-unavailable(string),
    queue-full(string),
    too-large(string),
  }
}

interface bus {
  use bus-types.{bus-error};

  publish: func(driver: string, topic: string, payload: list<u8>) -> result<_, bus-error>;
  get:     func(driver: string, topic: string) -> result<option<list<u8>>, bus-error>;
}

interface bus-host {
  use bus-types.{bus-error};

  emit: func(topic: string, payload: list<u8>) -> result<_, bus-error>;
}

world plugin {
  import host-log;
  import host-settings;
  import driver-http;
  import driver-process;
  import driver-fs;
  import bus;

  export init: func();
  export on-event: func(ev: event);
  export on-message: func(driver: string, topic: string, payload: list<u8>);
}

world driver {
  import host-log;
  import host-settings;
  import driver-http;
  import driver-process;
  import driver-fs;
  import bus-host;

  export init: func();
  export on-message: func(from: string, topic: string, payload: list<u8>);
}
```

ゲスト向けには既存の `plugin-guest` と同様に、WASI の import 一式を足した
`driver-guest` world を用意する(Go/TinyGo でドライバを書くために必要)。

- **ドライバは `bus` を import しない**。ドライバから他のドライバへは送れない
- **ドライバは `on-event` を持たない**。journal / status は届かない
- ペイロードの上限は **256 KiB**(超過は `too-large`)。バスは制御メッセージの経路であり、
  大きなデータの受け渡しは `driver-fs` の担当という切り分け

## データフロー

3 経路ある。

### 1. プラグイン → ドライバ(`publish`)

ホストがドライバのキューにメッセージを積んで**即座に戻る**。ドライバの処理完了は待たない。
承認されていなければ `permission-denied`、キューが満杯なら `queue-full` を返す。

### 2. ドライバ → プラグイン(`emit` → `on-message`)

ドライバが `emit` すると、ホストがそのトピックを `subscribe` 宣言し**かつ承認済み**の
プラグイン全部の `on-message` を呼ぶ。retain されたトピックなら、ホストの retained ストアも
同時に更新する。

`driver.toml` の `[[topics]]` に無いトピックへの `emit` は `unknown-topic` を返す。ドライバは
自分が宣言したトピックしか使えない(承認画面に出したトピック一覧が実際の通信と一致することを
保証するため)。購読者が 1 人もいない場合の `emit` は成功扱いで、retain されていれば値だけが
残る。

### 3. プラグイン → 最新値(`get`)

**ホスト内の retained ストアを読むだけで、ドライバの wasm を一切呼ばない。** これが設計の要で、
以下が構造的に成立する。

- プラグイン → ドライバ → プラグイン の再入によるデッドロックが起きない
- `get` が `PluginInstance::CALL_DEADLINE`(2 秒)を脅かさない
- ドライバが自分で状態を保持する必要がない(永続化したければ `driver-fs` を承認してもらう)

retain するかはトピックごとに `driver.toml` で宣言する。retain されたトピックは、後から起動・
後から承認されたプラグインにも**購読開始時点で最新値が 1 回配信される**。

## 配置とマニフェスト

### drivers-dir

`--drivers-dir` で指定する。未指定時の既定は `$XDG_CONFIG_HOME/edlr/drivers`
(`XDG_CONFIG_HOME` 未設定なら `~/.config/edlr/drivers`)。`--plugins-dir` などと同様、
存在しなくてもエラーにはならず、ドライバ 0 件で起動する。

```
<drivers-dir>/
  <id>/
    driver.toml    # plugins の manifest.toml とは別名にして取り違えを防ぐ
    driver.wasm    # driver.toml の entry が指すファイル名。任意の名前でよい
```

- ディレクトリ名(`<id>`)は `driver.toml` の `id` と一致していなければならない
- `id` は `[a-z0-9-]+` にマッチする必要がある
- **ID 空間はプラグインとドライバで別**。同名のプラグインとドライバが共存できる

### driver.toml

```toml
id = "ed-state"
name = "ED State"
version = "0.1.0"
description = "ゲームの現在状態を集約して配るドライバ"
entry = "driver.wasm"

[[topics]]
name = "current-system"
retain = true
description = "現在のスターシステム"

[[topics]]
name = "ship-status"
retain = false
description = "船の状態変化の通知"

[[capabilities]]   # 既存と同じ書式。ドライバ自身が使う特権
kind = "http"
hosts = ["https://api.example.com"]
reason = "..."

[[sidecar]]        # 既存と同じ書式
name = "engine"
reason = "..."
args = ["--port", "{port}"]
port = 50021
scalable = false

[[filesystem]]     # 既存と同じ書式
name = "cache"
reason = "..."
mode = "read-write"

[[settings]]       # 既存と同じ書式
key = "enabled"
label = "Enabled"
type = "boolean"
default = true
```

- `[[topics]]` の `name` はドライバ内で一意、`[a-z0-9-]+`
- `retain` の既定は `false`
- `description` は省略可(承認画面と Drivers UI に表示する)
- `[[capabilities]] / [[sidecar]] / [[filesystem]] / [[settings]]` はプラグインと同一の書式・
  同一の検証・同一の承認フローを使う。ドライバは複数プラグインの結節点だが、**承認の粒度や
  警告文をプラグインより重くはしない**(今回のスコープ外)

### プラグイン側の manifest.toml

```toml
[[bus]]
driver = "ed-state"
publish = ["ship-status"]
subscribe = ["current-system"]
reason = "現在システムを購読して翻訳先を切り替えるため"
```

- `[[bus]]` は複数書ける。`driver` はブロック間で一意
- `publish` / `subscribe` はどちらも省略可(両方空のブロックは検証エラー)
- **`get` は `subscribe` に宣言したトピックに対してのみ許される。** 「配信は要らないが最新値は
  読みたい」という区別は設けない(承認画面に出す情報を増やさないため)
- `reason` は必須・空文字不可。`capabilities` / `[[sidecar]]` / `[[filesystem]]` と同じく
  trim して制御文字・ゼロ幅文字を拒否する(承認画面に描画される文字列とフィンガープリントの
  入力を byte 単位で一致させるため)

## 承認モデル

既存 capability の型をそのまま踏襲する。

- `[[bus]]` を宣言したプラグインは**既定では未承認**。未承認の間、そのドライバへの `publish` と
  `get` は `permission-denied` を返し、購読しているトピックの配信も届かない。プラグイン自体は
  動き続ける(ロードが失敗したり停止したりはしない)
- ユーザーは Plugins UI から「どのドライバの、どのトピックに、publish するのか subscribe
  するのか」を確認して、ブロック単位で承認・取消できる
- 承認/取消は `<grants-dir>/<plugin-id>.json` に永続化され、次回起動時にも引き継がれる
- **承認/取消は稼働中のプラグインにも即座に反映される**(再起動不要)。照合は呼び出しごと・
  配信ごとに行う
- マニフェストの要求内容(`driver` / `publish` / `subscribe` / `reason` の集合)が変わると、
  以前の承認は自動的に失効(stale)する。stale は「未承認」として扱う
- 承認はマニフェストの要求内容に結び付いており、wasm バイナリのハッシュは含まない
  (`driver-http` と同じ性質)

ドライバ自身の `[[capabilities]]` / `[[sidecar]]` / `[[filesystem]]` は Drivers UI 側で承認する。
`driver-fs` が edlr 自身の設定・状態ディレクトリを掴めない既存の防御は、ドライバにもそのまま
適用する。

### 詐称不可の性質

プラグインは `publish` / `get` に自分の ID を渡さない。ドライバも `emit` に自分の ID を渡さない。
送信元 ID は `HostCtx`(wasm インスタンスごとに固有)が保持する値から取る。したがって
他プラグインを騙って publish することも、他ドライバを騙って emit することもできない。
ドライバの `on-message` が受け取る `from` はホストが埋めた値であり、常に信用してよい。

### 未解決の参照

参照先ドライバが未インストール、あるいは `driver.toml` に無いトピックを宣言している場合でも、
**プラグインは通常どおりロードされる**。ドライバは後から入れられるべきなので、起動時エラーには
しない。承認済みの接続に限り、実行時には `unknown-driver` / `unknown-topic` を返す。**承認は
`unknown-driver`/`unknown-topic` より先にチェックする**(`check_bus` が先に走る)ので、未承認の
接続は未解決かどうかに関わらず常に `permission-denied` になる -- 未解決かつ未承認の接続を承認
しても、その時点で初めて `unknown-driver`/`unknown-topic` が見えるようになる。

ただし黙って動くのは事故のもとなので、2 段で可視化する。

- **ロード時に warn ログ**を出す。プラグイン ID・ドライバ ID・トピック名を全て含める
- **UI にその接続を「未解決」バッジで常時表示**する(ログだけでは GUI 利用者が気づけない)

drivers-dir の走査は plugins-dir と同じく**起動時のみ**なので、後からドライバを入れた場合は
デーモンの再起動が必要になる。

## 並行性と失敗時の振る舞い

ドライバはプラグインと同様、**専用スレッド + 容量固定キュー**で駆動する。ドライバは複数
プラグインの結節点で溢れやすく、また 1 メッセージの処理が秒オーダーになり得る(TTS など)ため、
プラグイン側の作業キュー容量(`PLUGIN_WORK_QUEUE_CAPACITY`、既定 64)とは**別の定数**
(`DRIVER_MESSAGE_QUEUE_CAPACITY`、既定 64)を持たせる。プラグイン側のキューは journal
イベントとバス配信の 2 プロデューサが共有するようになったため、既定値は当初の 32 から
64 に引き上げてある(両者の間に公平性は無いままなので、これは緩和策に過ぎない --
「スコープ外」の節を参照)。

### キュー溢れの扱いは方向によって非対称にする

- **`publish`(プラグイン → ドライバ)は満杯なら捨てずに `queue-full` を返す。** publish は結果を
  返せる同期呼び出しなので、呼び出し側が再送するか諦めるかを選べる。黙って捨てるとプラグイン
  作者が気づけない
- **`emit`(ドライバ → プラグイン)の配信は、宛先のキューが満杯なら黙って捨てる。捨てるのは
  新しいものであって古いものではない。** 実装は `std::sync::mpsc::SyncSender::try_send` を使って
  おり、`Full` のとき返る(= 捨てられる)のは送信しようとした値そのものであって、キューに既に
  並んでいる古い値ではない -- `try_send` にキューの中身を入れ替える手段は無いので、この方向にしか
  実装できない。遅いプラグイン 1 個がドライバ全体を止める事態を避けるのが黙って捨てる理由で、その
  点は既存のイベント配信と同じだが、**どちらを捨てるかは既存のイベント配信(こちらは古い方から
  捨てる別実装)と対称ではない**。`retain = false` のトピックは `get` に読み直しの手段が無いため、
  詰まった購読プラグインはこの間の更新を(新しいものから順に)一切受け取れなくなる

**retained 値の更新はキューとは独立に必ず行われる。** 配信を取りこぼしたプラグインも `get` で
最新値を拾えるので、状態同期という用途では欠落が致命傷にならない。

### 順序保証

- 同一プラグインから同一ドライバへ: FIFO
- 同一ドライバから同一プラグインへ: FIFO
- 複数の送信元をまたいだグローバルな順序: **保証しない**

### ドライバの trap / 無効化

ドライバが trap した場合、そのドライバを無効化し、以降の `publish` / `get` は
`driver-unavailable` を返す。このとき **retained 値は破棄する**。死んだドライバの古い状態を
`get` が返し続けると、プラグイン側が「更新が止まっているだけ」と「もう誰も更新しない」を
区別できないため。

ドライバが無効化されると、そのドライバが起動していたサイドカーも既存の規則どおり停止する。

## 呼び出し期限(ドライバ専用の定数組)

プラグインの `HTTP_TIMEOUT`(1.5 秒)は `PluginInstance::CALL_DEADLINE`(2 秒)未満である
ことがコンパイル時アサーションで固定されている(`core/src/plugin/host.rs`)。epoch interruption
は wasm の命令境界でしか作動せず、ブロッキングな HTTP 呼び出し自体を打ち切れないためである。

ドライバはこの上限では足りない。典型例が音声合成エンジンの呼び出しで、長文や初回の合成には
数秒かかる。ドライバは専用スレッドで動きイベント配信のループを塞がないので、**ドライバ用に
別の定数組を定義し、不変条件の形だけを保つ**。

```rust
pub const DRIVER_HTTP_TIMEOUT: Duration = Duration::from_secs(25);
// DriverInstance::CALL_DEADLINE = 30s
const _: () = assert!(DRIVER_HTTP_TIMEOUT.as_millis() < DriverInstance::CALL_DEADLINE.as_millis());
```

代償を 2 つ設計に書き込む。

- **処理中そのドライバのキューは詰まる。** だからキュー容量をプラグインより大きく取り、
  溢れたら `queue-full` を返して呼び出し側が「今は無理」と判断できるようにする
- **デーモン終了時の後始末に効く。** ブロッキング HTTP 中のドライバは停止要求に即応できず、
  最悪 `DriverInstance::CALL_DEADLINE` ぶん待たされる。`ui/src-tauri/src/daemon.rs` の
  `STOP_GRACE`(95 秒)は、サイドカー停止の最悪ケース(`SIDECAR_SHUTDOWN_GRACE_SECS` ×
  `SIDECAR_SHUTDOWN_WORST_CASE_INSTANCES` = 3 秒 × 20 インスタンス = 60 秒)にドライバの呼び出し
  期限 1 回分(`DRIVER_CALL_DEADLINE_SECS` = 30 秒)を足した 90 秒を厳密に超える値にしてある
  (Minor: 最終レビューで見つかった記述違い。以前この節は「65 秒」と書いていたが、実装は既に
  この 90 秒の関係を超える 95 秒になっていた)。**この関係はコンパイル時アサーションで固定して
  ある**(`ui/src-tauri/src/daemon.rs` の `STOP_GRACE` 直下)。この見積もりが実際に効くのは、
  デーモンの shutdown シーケンス(`core/src/bin/edlr.rs`)が**プラグインとドライバ両方**の
  `stop_all_sidecars` を呼ぶ場合に限る -- 以前はドライバ側の呼び出しが漏れており(別の Critical
  な取りこぼし。上の「ドライバの trap / 無効化」節参照)、その間はこの見積もり自体がドライバの
  サイドカーには一切当てはまらない絵に描いた餅だった

## WebSocket RPC と UI

`/ws` に `plugins/*` と対称の RPC を追加する。

- `drivers/list` — drivers-dir のパスと、ロード済み全ドライバの ID / 名前 / バージョン /
  トピック一覧 / 承認状態 / 有効・無効
- `drivers/settings/get` / `drivers/settings/set`
- `drivers/grants/set` — ドライバ自身の capability 承認

UI には **Drivers タブ**を新設する(Plugins とは別タブ)。一覧、設定、capability 承認、
トピック一覧の表示を行う。retained 値の覗き見は今回のスコープ外。

Plugins タブ側には `[[bus]]` の承認 UI を追加し、未解決の接続に「未解決」バッジを出す。

## 実装の分割

| 置き場所 | 役割 |
|---|---|
| `drivers/channel`(現在は空のクレート) | バスのコアロジック。購読表の解決、retained ストア、キュー方針、承認フィルタ。**wasm にも wasmtime にも依存しない純ロジック**にしてユニットテストで固める |
| `core/src/driver/`(新設: `manifest.rs` / `registry.rs` / `runner.rs` / `host.rs`) | wasm インスタンスの生成、スレッド駆動、ホスト関数の実装。`core/src/plugin/` と対称に置く |
| `core/src/plugin/` | `[[bus]]` の manifest 検証と、`on-message` の呼び出し経路を追加 |
| `core/src/server.rs` | `drivers/*` RPC |
| `core/wit/plugin.wit` | `bus` / `bus-host` / `world driver` / `world driver-guest` |
| `ui/frontend` | Drivers タブ、Plugins タブの bus 承認 UI |

別レイヤーにした以上、`plugin` と `driver` の実装を無理に共通化しない。共有するのは
grants / settings の下位ユーティリティ程度に留める。

## テスト

3 層で担保する。

1. **`drivers/channel` のユニットテスト** — 購読解決、retain の設定と trap 時の破棄、
   `queue-full` の境界、未承認・stale のフィルタ、ペイロード上限
2. **ホスト統合テスト** — 既存のプラグインテストの流儀に合わせ、実際に wasm をロードして
   publish → `on-message` → `emit` → 購読プラグイン着信の 1 往復を通す。未解決参照が
   warn ログを出しつつ起動を妨げないことも確認する
3. **コンパイル時アサーション** — `DRIVER_HTTP_TIMEOUT < DriverInstance::CALL_DEADLINE`、
   および `STOP_GRACE` とドライバ期限の関係

**サンプル**として `examples/drivers/` に retained state ドライバを 1 つ置き、それを購読する
プラグインと組にして、両方向が動くことを示す。

## 想定ユースケース: 音声合成ドライバ

本設計が実際に成立するかの検証として、VOICEVOX 相当の音声合成ドライバを想定する。

- プラグインが `speak` トピックに publish → ドライバが `on-message` で受けて合成・再生
- ドライバは retained トピック `state`(`speaking` / `idle`)を emit し、publish が
  fire-and-forget でも呼び出し側が `get` で状況を見られる
- 複数プラグインが 1 つの音声エンジンを共有する。まさにドライバを別レイヤーにした動機どおり
- **合成エンジンと再生プレイヤーの両方を、このドライバ自身の `[[sidecar]]` として持つ。**
  ドライバ間呼び出しが無いため、「音声再生ドライバ」を別に立てて合成ドライバから呼ぶ構成は
  取れない。再生機能がこのドライバに閉じる代償を受け入れる
- 合成に数秒かかるため、上記のドライバ専用の期限(`DRIVER_HTTP_TIMEOUT` = 25 秒)が必要になる

## スコープ外

以下は今回やらない。必要になった時点で別途設計する。

- **ドライバ間の通信**。ドライバは `bus` を import しないため、ドライバ同士は繋がらない。
  共有機能を下位のドライバとして切り出したくなったら、publish のみの解禁 + 連鎖深さ上限による
  循環検出、という形で再検討する
- **ユーザー定義 WIT の動的リンク**(型安全な `import acme:state/store@0.1.0`)。install 時の
  合成ではプラグインごとに別インスタンスになり共有状態が壊れ、実行時 dynamic linker 経路は
  実装コストが釣り合わない。バス ABI を後から typed 経路で置き換えても、マニフェストと
  ライフサイクルは変わらないので移行余地は残る
- **スキーマからのコード生成**。トピックのペイロード形式はドライバ作者がドキュメントや
  同梱スキーマで示す規約に留める
- **retained 値のホスト側での永続化**。必要ならドライバが `driver-fs` で自前で行う
- **ドライバへの重い信頼レベルの導入**(接続元の制御、警告文の格上げ)
- **UI での retained 値の覗き見**
- **プラグインごとの作業キュー破棄をカウンタとして観測可能にすること、および `plugins/list`
  などの RPC 経由でそれを公開すること。** `PLUGIN_WORK_QUEUE_CAPACITY` は journal イベントと
  バス配信という 2 つの独立したプロデューサに公平性・優先度なく共有されており、容量を
  32 から 64 へ引き上げたのもあくまで緩和策(何がどれだけ捨てられているか分からないまま
  この数値だけを調整するのは当て推量でしかない)。**このキュー容量をユーザー設定可能に
  することは、この観測可能性が実装されるまで見送る**
- **承認済みサイドカー間のポート衝突の検出。** `implicit_http_hosts` は承認済みの各サイドカーに
  `http://127.0.0.1:<port>` を自動的に許可する(ユーザーが承認済みプラグイン/ドライバの
  サイドカーへ直接触れられる範囲を、バス承認モデルの外から広げないための設計)。しかしポート
  番号はユーザーが手入力する設定値で、プラグイン・ドライバをまたいだ一意性チェックは無い。
  そのため、あるプラグインのサイドカーが、あるドライバのエンジンサイドカーと同じポートで
  承認されると、そのプラグインは(バス経由の承認とは無関係に)`implicit_http_hosts` 経由で
  そのエンジンへ直接 HTTP で触れてしまい、バス承認モデルを迂回できる(既知の穴。最終レビューで
  指摘された未修正のギャップ -- 「閉じた穴」と誤解されないよう、ここに明記しておく)。塞ぐには
  ポート割り当てをプラグイン・ドライバ横断でグローバルに一意にする仕組みが要るが、今回は
  やらない
