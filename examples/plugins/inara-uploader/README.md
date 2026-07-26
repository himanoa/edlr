# inara-uploader

Journal イベントを [INARA](https://inara.cz) の API v1 へアップロードする edlr プラグイン。
**Go (TinyGo) 製**で、`examples/plugins/hello-logger`(Rust)と同じ `world plugin` の
インターフェースを実装している(ビルド時の対象は WASI を含む `world plugin-guest`。下記参照)。

動作確認済み: 実際の `edlr` デーモンにロードされ、Journal から
`setCommanderCredits` / `setCommanderRankPilot` / `addCommanderTravelFSDJump` /
`addCommanderTravelDock` / `setCommanderReputationMajorFaction` /
`addCommanderCombatDeath` を組み立てて送るところまで。

## ビルド

必要なもの:

- [TinyGo](https://tinygo.org/getting-started/install/) 0.34 以降(0.41.1 で確認)
- Go 1.23 以降(TinyGo が内部で使う)

```
cd examples/plugins/inara-uploader
./build.sh              # plugin.wasm を出力
```

ビルド対象の world は `plugin` ではなく **`plugin-guest`**(= `plugin` に WASI の
import 一式を足したもの)。Go/TinyGo の標準ライブラリはプラグインが何も呼ばなくても
WASI を import するため、`plugin` を直接対象にすると `wasm-tools component new` が
「world に無い import がある」として弾く。詳細は `core/wit/plugin.wit` の
`world plugin-guest` のコメントを参照。

### 配置

```
mkdir -p ~/.config/edlr/plugins/inara-uploader
cp plugin.wasm manifest.toml ~/.config/edlr/plugins/inara-uploader/
```

デーモンを起動したら Plugins 画面で:

1. **API キー**を入力する(INARA の Settings → API keys で発行)
2. capability(`https://inara.cz` への HTTP)を**承認**する — 未承認のうちは
   送信が `permission-denied` になり、イベントはキューに残る
3. 動作確認が済むまでは `isBeingDeveloped`(既定 true)を そのままにしておく

### バインディングの再生成

`core/wit/plugin.wit` を変更したときだけ必要。生成物(`gen/`)はリポジトリに
コミットしてあるので、通常のビルドには不要。

```
go install go.bytecodealliance.org/cmd/wit-bindgen-go@v0.6.2
wit-bindgen-go generate --world plugin --out gen ../../../core/wit
```

`wit-bindgen-go` は `wasm-tools` を PATH に要求する。

## 設定

| キー | 型 | 既定 | 説明 |
| --- | --- | --- | --- |
| `enabled` | boolean | `true` | 無効にすると何もしない |
| `apiKey` | string | `""` | INARA の API キー。**平文で保存される**(下記 2 番) |
| `commanderName` | string | `""` | 空なら Journal の `LoadGame` / `Commander` から学習 |
| `isBeingDeveloped` | boolean | `true` | INARA 側で「開発中クライアント」として扱わせる |
| `batchSize` | number | `10` | この件数溜まったら送る |
| `minIntervalSeconds` | number | `60` | 送信の最短間隔。INARA は高頻度の送信を控えるよう求めている |
| `uploadHistorical` | boolean | `false` | プラグイン起動より前のイベントも送る(下記 3 番) |
| `dryRun` | boolean | `false` | 送信せず、組み立てた JSON をログに出す |

送信は「イベントを受け取ったついで」に行い、次のいずれかで実際に飛ぶ:

- Journal の `Shutdown` を受け取った(ゲーム終了。以後イベントは来ない)
- キューが 200 件を超えた(メモリ保護)
- キューが `batchSize` 以上あり、かつ前回送信から `minIntervalSeconds` 経過

送信に失敗した場合(ネットワーク不通・未承認など)はキューを保持して次の
イベントで再試行する。INARA が個々のイベントを拒否した場合はログに出すだけで
再送はしない(同じ内容を送り直しても通らないため)。

## 対応イベント

| Journal | INARA | 備考 |
| --- | --- | --- |
| `LoadGame` | `setCommanderCredits` | 所持金・ローン |
| `Commander` / `LoadGame` | (ヘッダのみ) | コマンダー名と Frontier ID の学習 |
| `Rank` + `Progress` | `setCommanderRankPilot` | 段位と進捗は別イベントで来るため、**`Progress` を見て初めて送る**(段位だけ送ると INARA 側の進捗が 0 に潰れるため) |
| `Reputation` | `setCommanderReputationMajorFaction` | Journal の -100..100 を -1..1 に換算 |
| `FSDJump` | `addCommanderTravelFSDJump` | 星系名・跳躍距離・座標 |
| `CarrierJump` | `addCommanderTravelCarrierJump` | |
| `Docked` | `addCommanderTravelDock` | |
| `Location` | `setCommanderTravelLocation` | |
| `EngineerProgress` | `setCommanderRankEngineer` | 起動時の配列形式・単体形式の両方に対応 |
| `Materials` | `setCommanderInventoryMaterials` | Raw / Manufactured / Encoded を 1 本にまとめる |
| `Statistics` | `setCommanderGameStatistics` | Journal の中身をそのまま |
| `Died` | `addCommanderCombatDeath` | 星系名は直近の移動イベントから補う(`Died` 自体には入っていない) |
| `Shutdown` | (送信トリガのみ) | 溜まっているぶんを送り切る |

`manifest.toml` の `events` に列挙したイベントしかプラグインへ届かない。
イベントを増やすときは `mapping.go` と `manifest.toml` の両方を直すこと。

## 不足している実装

**このプラグインを実用品にするために、edlr 本体側に足りていないもの。**
どれもプラグイン単体では回避しきれない。

### 1. 定期実行・終了フックが無い

プラグインは**イベントが届いたときにしか動けない**。そのため:

- 最後の Journal イベントの後に溜まったぶんは、次のイベントが来るまで送れない
- ゲームが `Shutdown` を書かずに落ちた場合、その未送信分は失われる
- デーモンだけを止めた場合も同じ

`Shutdown` イベントで最後のフラッシュを行うことで通常のゲーム終了はカバーして
いるが、これは Journal 頼みの回避策にすぎない。`world plugin` に
「一定間隔で呼ばれるフック」か「プラグイン停止時に呼ばれるフック」が欲しい。

### 2. プラグインの永続ストレージが無い(二重送信 / 取りこぼし)

`host-settings` は読み取り専用(`get-all` のみ)で、プラグインが自分の状態を
残す手段が無い。そのため「どこまで INARA へ送ったか」を再起動をまたいで
覚えられない。

これは edlr の Journal tailer の挙動と噛み合わせが悪い: **デーモン起動時、
tailer は現行 Journal ファイルを先頭から読み直す**(`core/src/journal/tailer.rs`
の `pos = 0` 起点)。つまり素朴に実装すると、デーモンを再起動するたびに
その日のイベントを丸ごと INARA へ再送する。

回避として、既定では**プラグイン起動時刻より古いタイムスタンプのイベントを
捨てている**(`uploadHistorical = false`)。副作用として、デーモンが止まって
いた間に発生したイベントは送られない。

必要なもの: プラグインごとの小さな key-value ストア(あるいは
`host-settings.set`)。これがあれば「最後に送ったイベントのタイムスタンプ」を
持てて、二重送信も取りこぼしも無くせる。

### 3. 秘密情報向けの設定型が無い

INARA の API キーは `[[settings]]` の `string` として扱うしかない。結果:

- `<settings-dir>/inara-uploader.json` に**平文**で保存される
- Plugins 画面で普通のテキスト入力として**そのまま表示される**
- `plugins/list` / `plugins/get-settings` RPC の応答にも平文で載る

`SettingField` に `secret` 型(UI ではマスク表示、値は RPC 応答に含めない、
ログに出さない)が要る。設計書(capability HTTP)では「認証情報の保管は
スコープ外(プラグインが自前でヘッダに載せる想定)」とされているが、
**プラグインが自前で持つ手段そのものが無い**ため、現状は平文設定しかない。

### 4. `driver-http` のタイムアウトが 1.5 秒固定

`HTTP_TIMEOUT` はホスト側の定数で、プラグインからは変えられない。INARA が
混んでいると 1.5 秒では返らず `transport` エラーになる。プラグインは
キューを保持して再試行するが、キューは揮発(2 番)なのでデーモンが落ちれば
失われる。プラグインごとにタイムアウトを設定できるか、送信を非同期に
投げられる仕組みが欲しい。

### 5. 送信中はイベントを取りこぼしうる

`driver-http.send` は同期呼び出しで、その間プラグイン専用スレッドは
イベントを読まない。キューは 32 件で、溢れた分はホスト側で捨てられる
(`PLUGIN_EVENT_QUEUE_CAPACITY`)。最大 1.5 秒 × 送信回数のあいだイベントが
流れ続けると欠落しうる。戦闘中など高頻度の場面で顕在化する。

### 6. マッピングが INARA のごく一部

INARA API v1 のイベントは 100 種類以上ある。このプラグインが対応しているのは
上の表の 12 種類だけ。少なくとも次は未対応:

- 市場・カーゴ・資産(`Market`, `Cargo`, `Loadout`, `ShipyardBuy` ...)
- ミッション(`MissionAccepted` / `MissionCompleted` / `MissionFailed`)
- 探査(`Scan`, `SAASignalsFound`, `FSSDiscoveryScan`)
- 戦闘(`PVPKill`, `Bounty`, `FactionKillBond`)
- コミュニティゴール、艦隊キャリア、パワープレイ

`Market` / `Cargo` などは Journal ではなく別ファイル(`Market.json`,
`Cargo.json`)に書かれる。**edlr は現在 `Journal.*.log` と `Status.json` しか
監視していない**ので、これらを扱うにはコア側の監視対象を増やす必要がある。

### 7. Go プラグインのビルドがリポジトリに組み込まれていない

`hello-logger` は cargo で普通にビルドできるが、Go プラグインは TinyGo と
`wit-bindgen-go`(と `wasm-tools`)を各自で用意する必要がある。CI も無い。
Go を一級市民にするなら、ツールチェーンの固定と CI が要る。
