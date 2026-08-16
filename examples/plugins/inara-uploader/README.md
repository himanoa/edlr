# inara-uploader

Journal イベントを [INARA](https://inara.cz) の API v1 へアップロードする edlr プラグイン。
**Go (TinyGo) 製**で、`examples/plugins/hello-logger`(Rust)と同じ `world plugin` の
インターフェースを実装している(ビルド時の対象は WASI を含む `world plugin-guest`。下記参照)。

(旧実装で)動作確認済み: 実際の `edlr` デーモンにロードされ、Journal から
`setCommanderCredits` / `setCommanderRankPilot` / `addCommanderTravelFSDJump` /
`addCommanderTravelDock` / `setCommanderReputationMajorFaction` /
`addCommanderCombatDeath` を組み立てて送るところまで。

パッケージ再構成後(現在の実装)は、TinyGo 0.41.1 でのビルドと
`wasm-tools validate` によるコンポーネント検証、および **TinyGo でコンパイルした
テストを wasm 上で実行**(`tinygo test -target=wasip1 ./...`、4 パッケージとも通過)
まで確認済み。**デーモンへロードして実際に INARA へ送るところは未確認。**

## ビルド

必要なもの:

- [TinyGo](https://tinygo.org/getting-started/install/) 0.34 以降(0.41.1 で確認)
- Go 1.23 以降(TinyGo が内部で使う)

```
cd examples/plugins/inara-uploader
./build.sh              # plugin.wasm を出力
```

判断を持つコードは `main` の外にある。`main` はホスト境界のアダプタ(設定を読む・
`driver-http` を呼ぶ・結果をログへ整形する)だけを担い、`uploader`(キューと送信
判断)・`mapping`(Journal → INARA 変換)・`inara`(リクエスト組み立てと応答解釈)・
`settings`(設定値の解釈)はいずれも `go test ./...` で検証できる。`main` パッケージは
`//go:wasmimport` を含むためネイティブでリンクできず、テストを書けない。

```
go test ./...   # ロジック層のテスト
go vet ./...    # main を含む全パッケージの型チェック
```

ネイティブの `go test` に加えて、**TinyGo でコンパイルして wasm 上でも走らせておくと
よい**(要 wasm ランタイム、下は wasmtime を PATH に置いた場合):

```
tinygo test -target=wasip1 ./inara/ ./mapping/ ./settings/ ./uploader/
```

TinyGo の `reflect` は標準ライブラリと差があり、`encoding/json` の挙動が変わりうる。
特に `mapping` は無名埋め込みによるフィールド昇格(`EngineerProgress` の単体形式)、
配列ポインタ(`*[3]float64`)、named map type を使っているので、ネイティブで緑でも
wasm 上で壊れることがありうる。

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
3. filesystem(`state`、read-write)を**承認して保存先フォルダを設定**する。
   ここに Journal から学習した状態(コマンダー名・gameversion)を
   `state.json` として保存する。edlr は Journal の読み取り位置を永続化して
   いて `LoadGame` は再配信されないため、未設定のままだと**セッション途中で
   デーモンを再起動したとき、次にゲームへログインし直すまで何も送信されない**
   (INARA が Live 版のデータしか受け付けないことによるゲートが閉じたままになる)
4. 手動同期を使う場合は、filesystem(`journal`、read)に **Journal
   ディレクトリを指定して承認**し、Plugins 画面でダッシュボードウィジェット
   「INARA 同期」も承認する
5. 動作確認が済むまでは `isBeingDeveloped`(既定 true)を そのままにしておく

### 手動同期(現行セッションの再送)

ダッシュボードの「INARA 同期」ウィジェットのボタンを押すと、**現行セッションの
Journal ファイルを先頭から読み直して INARA へ送り直す**。セッション途中で
デーモンを再起動した・送信できない時間帯があった、などで欠測した分を
取り戻すための手動操作。

- 経路: ボタン → `plugins/dashboard-action` RPC →
  `on-message(driver="dashboard", topic="resync")` → driver-fs で最新
  `Journal.*.log` をチャンク読み → 変換 → 非同期送信(`submit-send`)の
  数珠つなぎ(1 バッチ 100 件)
- 変換は専用の状態で行い、ファイル先頭の `LoadGame` から学習し直す。INARA が
  受け付けない Legacy セッションは送らない(通常経路と同じゲート)
- `minIntervalSeconds` は適用しない。`enabled` off・`apiKey` 未設定・
  `dryRun` on のときは開始を拒否して理由をログに出す
- 進行状態はメモリのみ(再起動したら押し直す)。実行中の再押下は無視
- **重複について**: 変換先の大半は `set*` 系(所持金・ランク・素材など)で
  再送しても冪等。`addCommanderTravelDock` などの履歴系は INARA 側の
  重複破棄に委ねる。進捗と結果はログ画面に `resync:` 接頭辞で出る

### バインディング

WIT バインディング(`gen/`)は `sdk/go`(`edlrplugin`)が同梱しているので、
このプラグイン側での生成・コミットは不要。再生成が要る場合は
`sdk/go/README.md` を参照。

## 設定

| キー | 型 | 既定 | 説明 |
| --- | --- | --- | --- |
| `enabled` | boolean | `true` | 無効にすると何もしない |
| `apiKey` | string | `""` | INARA の API キー。**平文で保存される**(下記 2 番) |
| `commanderName` | string | `""` | 空なら Journal の `LoadGame` / `Commander` から学習 |
| `isBeingDeveloped` | boolean | `true` | INARA 側で「開発中クライアント」として扱わせる |
| `batchSize` | number | `10` | この件数溜まったら送る(live モードのみ。replay 中は無視される) |
| `minIntervalSeconds` | number | `60` | 送信の最短間隔。INARA は高頻度の送信を控えるよう求めている(replay 中は原則無視されるが、送れない状態が続く間の再試行間隔としては効く) |
| `uploadHistorical` | boolean | `true` | デーモン起動より前に既に Journal へ書かれていたイベント(`event.replay = true`)も送る。edlr は Journal の読み取り位置を永続化しており、再起動しても続きから配信するため、既定で送っても重複送信にはならない。**`false` にすると、デーモンが止まっていた間に書かれたイベントは INARA へ送られない**(その分は欠測になる) |
| `dryRun` | boolean | `false` | 送信せず、組み立てた JSON をログに出す |

送信は「イベントを受け取ったついで」に行う。デーモンが動き出す前に書かれていた
イベント(`event.replay`)と、動き出した後のイベントでは経路が分かれる。

**replay(バックログを流し切る)**

- キューが 100 件以上たまったら送る
- `minIntervalSeconds` は適用しない(バックログを流し切ることを優先する)。ただし
  直前の送信試行でキューを空にできなかった場合(API キー未設定・capability
  未承認・通信失敗など)は、`minIntervalSeconds` の間隔を空けてから再試行する
- `Shutdown` を受け取っても送信は促さない。過去のゲーム終了ログであって
  「もうイベントは来ない」ことを意味しないため
- live のイベントを初めて受け取った時点で、溜まっている端数を送り切る

**live(通常の運用)**

- Journal の `Shutdown` を受け取った(ゲーム終了。以後イベントは来ない)
- キューが `batchSize` 以上あり、かつ前回の送信試行から `minIntervalSeconds` 経過

キューは**直近 200 件だけを保持する**。API キー未設定や capability 未承認で送れない
状態が続くと、古いものから順に捨てられる(捨てた件数はログに出る)。INARA は
「現在の状態」を反映するサービスなので、古い履歴を落として最新を残す。

送信に失敗した場合(ネットワーク不通・未承認など)はキューを保持して次の
イベントで再試行する。INARA がバッチ全体を拒否した場合(API キー不正など)は
恒久的な失敗なのでキューを捨てる。個々のイベントが拒否された場合はログに出す
だけで再送はしない(同じ内容を送り直しても通らないため)。

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
| `FSDJump` / `Location` の `Factions` | `setCommanderReputationMinorFaction` | `MyReputation` の -100..100 を -1..1 に換算 |
| `Touchdown` | `addCommanderTravelLand` | INARA が船種を必須にするため、現在の船を学習するまでは送らない |
| `Promotion` | `setCommanderRankPilot` | `Rank` と同じ形なので同じ変換を流用 |
| `Powerplay` | `setCommanderRankPower` | |
| `Cargo` | `setCommanderInventoryCargo` | イベント本体に `Inventory` が入っているときだけ(以降は `Cargo.json` 参照のみで在庫が入っていない) |
| `Died` | `addCommanderCombatDeath` | 星系名は直近の移動イベントから補う(`Died` 自体には入っていない) |
| `PVPKill` | `addCommanderCombatKill` | 星系名は直近の移動イベントから補う(未学習なら送らない) |
| `Interdiction` / `Interdicted` / `EscapeInterdiction` | `addCommanderCombatInterdiction` / `...Interdicted` / `...InterdictionEscape` | 星系名は直近の移動イベントから補う |
| `Loadout` | `setCommanderShip` + `setCommanderShipLoadout` | 現在の船の学習も兼ねる |
| `ShipyardNew` / `ShipyardSell` / `ShipyardSwap` | `addCommanderShip` / `delCommanderShip` / `setCommanderShip` | |
| `ShipyardTransfer` | `setCommanderShipTransfer` | 輸送先(= 今いるステーション)が未学習なら送らない |
| `SetUserShipName` | `setCommanderShip` | 船名・識別子の更新 |
| `StoredShips` | `setCommanderShip` ×N | 保管中の船を場所つきで列挙 |
| `StoredModules` | `setCommanderStorageModules` | 保管モジュールの全量 |
| `MissionAccepted` | `addCommanderMission` | 受注地は直近の移動イベントから補う |
| `MissionCompleted` | `setCommanderMissionCompleted` + `addCommanderPermit` ×N | 報酬の許可証は独立したイベントとして送る |
| `MissionFailed` / `MissionAbandoned` | `setCommanderMissionFailed` / `setCommanderMissionAbandoned` | |
| `SuitLoadout` / `SwitchSuitLoadout` / `CreateSuitLoadout` | `setCommanderSuitLoadout` | いずれも装備一式の全量なので同じ変換 |
| `RenameSuitLoadout` / `DeleteSuitLoadout` | `updateCommanderSuitLoadout` / `delCommanderSuitLoadout` | |
| `CommunityGoal` | `setCommunityGoal` + `setCommanderCommunityGoalProgress` | ゴールごとに 2 イベント |
| `Friends` | `addCommanderFriend` / `delCommanderFriend` | `Added` / `Lost` のみ(在席通知は送らない) |
| `Shutdown` | (送信トリガのみ) | 溜まっているぶんを送り切る |

`manifest.toml` の `events` に列挙したイベントしかプラグインへ届かない。イベントを
増やすときは `mapping/` のレジストリと `manifest.toml` の両方を直す。片方だけ直すと
`mapping/manifest_test.go` が落ちる。

## 不足している実装

**このプラグインを実用品にするために、edlr 本体側に足りていないもの。**
どれもプラグイン単体では回避しきれない。

### 1. 定期実行・終了フックが無い(解消済み)

`edlr:plugin@0.4.0` で manifest の `[[schedule]]` と `on-stop` エクスポートが
追加され、解消した。

- `manifest.toml` に `[[schedule]] name = "flush", interval-seconds = 60` を
  宣言してあり、`HandleSchedule` が `minIntervalSeconds` を尊重して端数を
  定期的に拾い上げる(`Shutdown` を待たずに、最後の Journal イベントから
  最大 60 秒でフラッシュされる)
- デーモンの graceful shutdown では `on-stop` が一度だけ呼ばれ、`HandleStop`
  が間隔を無視して無条件にフラッシュする(Ctrl-C などの通常停止をカバー)

残る制約:

- `on-stop` は **graceful shutdown のときだけ**呼ばれる。trap による無効化
  (disable)の後には呼ばれず、SIGKILL やクラッシュでも当然呼ばれない
- graceful shutdown であっても `on-stop` は有界の猶予時間内(既定 5 秒)に
  限った best-effort でしかない。停止の合図は作業キューを追い越すので、
  積み残しが多いだけなら `on-stop` へ到達できるが、終了時にちょうど
  実行中だった wasm 呼び出し(例: 応答しないホストへの `driver-http.send`)が
  猶予時間内に返らない場合は、フラッシュされないまま終了する(warn ログのみ)
- キューはメモリ上にしか無いため、`on-stop` が呼ばれない終わり方(SIGKILL・
  クラッシュ・ゲームが `Shutdown` を書かずに落ちた場合)では未送信分が
  失われ、**その分は欠測になる**。edlr の Journal 読み取り位置はプラグインへの
  配信時点で進む(送信の成否は関知しない)ため、配信済み・未送信のまま消えた
  イベントが再起動後の replay で取り直されることは無い

### 2. 秘密情報向けの設定型(解決済み)

INARA の API キーは `[[settings]]` の `secret` 型で宣言している:

- Plugins 画面ではマスク入力(`<input type="password">`)になる
- `plugins/list` / `plugins/get-settings` の応答には**含まれない**
  (write-only)。UI が知れるのは「設定済みかどうか」だけ
- プラグイン自身は `host-settings.get-all` で通常どおり受け取る
  (渡す相手はこのプラグインなので、ここで隠したら意味が無い)

**残る制約**: 保存先の `<settings-dir>/inara-uploader.json` 上は依然として
平文である。ファイル自体の保護は OS のパーミッションに委ねており、
OS のキーリング等との連携は未対応。

`driver-fs` で「承認したフォルダに自前で保存する」形も書けるが、それは平文を
ユーザーのフォルダへ移すだけで、しかも API キーを持つためだけにファイル
アクセスの承認を要求することになる(承認画面には「フォルダ内のファイルを
読み取り・作成・上書き・削除できます」と出る)ので、こちらは採らない。

### 3. `driver-http` のタイムアウトが 1.5 秒固定

`HTTP_TIMEOUT` はホスト側の定数で、プラグインからは変えられない。INARA が
混んでいると 1.5 秒では返らず `transport` エラーになる。プラグインは
キューを保持して再試行するが、キューはメモリ上にしか無いので、デーモンが
落ちれば失われる(1 番)。プラグインごとにタイムアウトを設定できるか、送信を非同期に
投げられる仕組みが欲しい。

さらに `on-event` 呼び出し全体には `CALL_DEADLINE`(ホスト側定数、2 秒)の予算
しかない。`driver-http.send` の 1.5 秒に加えて、`inara.Encode` の JSON
マーシャル(バッチは最大 200 件、`Statistics` は Journal の中身をそのまま
載せるため単体でも数十 KB になりうる。TinyGo の `encoding/json` は reflect
経由で速くない)がこの予算を食う。

`CALL_DEADLINE` を超えた場合、ホスト(`core/src/plugin/runner.rs`)は
これを**トラップとは区別して**扱う: 一時的な遅さ(応答しないホストなど)は
プラグインの故障ではないため、プラグインを作り直して(`init` からやり直して)
処理を続ける。**3 回連続**で超過して初めて `set_disabled` になり、その理由には
期限超過であることが明記される(Plugins ページにそのまま表示される)。

作り直しの副作用として、プラグインが wasm 線形メモリ上に持っていた未送信
キューは失われる。ただしこれは以前の挙動(1 回の超過で恒久 `Disabled`)でも
同じく失われたうえに以後の仕事も全部止まっていたので、厳密に改善である。

### 4. 送信中はイベントを取りこぼしうる

`driver-http.send` は同期呼び出しで、その間プラグイン専用スレッドは
イベントを読まない。キューは 64 件(journal イベントとバス配信が共有する
`PLUGIN_WORK_QUEUE_CAPACITY`)で、溢れた分はホスト側で捨てられる。最大
1.5 秒 × 送信回数のあいだイベントが流れ続けると欠落しうる。戦闘中など
高頻度の場面で顕在化する。

**replay(バックログ流し切り)中はこれが構造的に必ず起きる。** replay は
Journal を最高速で読み進める場面であり、`ReplayBatchSize`(100 件)ごとに
最大 1.5 秒の送信でブロックするため、そのあいだにホスト側の 64 件キューは
確実に溢れる。しかも edlr の Journal 読み取り位置はプラグインへの配信成否
とは無関係に進む(1 番)ため、**この経路でホスト側キューから溢れて
配信されなかったイベントも、デーモンを再起動しても replay で取り直せない**
(読み取り位置はすでに進んでいるため再配信の対象にならない)。

取りこぼし自体は今も起きるが、**黙って失われることは無くなった**:
ホストはプラグインごとに破棄件数を数えており、`plugins/list` の `dropped`
(`events` / `busDeliveries`)として返す。Plugins ページのプラグインカードにも
非ゼロのときだけ "Dropped" として表示される。これで
`PLUGIN_WORK_QUEUE_CAPACITY` のチューニングが当て推量でなくなる。

### 5. マッピングの対象範囲

INARA API v1 が文書化している送信イベントのうち、Journal から作れるものは
上の対応表ですべてカバーしている(Journal 側で 40 イベント超を購読)。
意図的に送らないものは:

- `get*` 系(クエリであり、送信イベントではない)
- 素材・カーゴの add/del アイテム差分(`setCommanderInventoryMaterialsItem`
  など)。スナップショット(`Materials` / `Cargo`)が全量を上書きするので、
  差分も送ると二重になるだけ
- 汎用の `setCommanderInventory` 系(cargo / materials 特化版でカバー済み)

なお `Cargo` の在庫はイベント本体に `Inventory` が入っているときしか送れない。
在庫の実体は `Cargo.json` に書かれるが、**edlr は現在 `Journal.*.log` と
`Status.json` しか監視していない**ため、常時追従するにはコア側の監視対象を
増やす必要がある。

### 6. Go プラグインのビルドがリポジトリに組み込まれていない

`hello-logger` は cargo で普通にビルドできるが、Go プラグインは TinyGo と
`wit-bindgen-go`(と `wasm-tools`)を各自で用意する必要がある。CI も無い。
Go を一級市民にするなら、ツールチェーンの固定と CI が要る。
