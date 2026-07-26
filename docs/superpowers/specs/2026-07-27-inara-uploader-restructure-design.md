# inara-uploader 再構成 設計書

2026-07-27 承認。`examples/plugins/inara-uploader` の振る舞いと構造を作り直す。

このプラグインは Journal イベントを INARA API v1 へアップロードする Go(TinyGo)製の例で、
WIT(`edlr:plugin@0.2.0`)への追随そのものは済んでいる。問題は、`replay` フラグ対応
(`a3013f4`)を既存の送信経路へ後付けしたことで、条件分岐が歪んだまま残っている点と、
`package main` に判断が集中していてテストが 1 行も書けない点。

## 現状の問題

### 振る舞い

1. **replay バックログが「保険」経由でしか流れない**。replay も live も同じ経路
   (`batchSize=10` 以上かつ前回送信から `minIntervalSeconds=60` 経過)を通るため、
   デーモンを数日止めた後の起動では「`maxQueued=200` を超えたので強制送信」が延々と
   続く。メモリ保護のための上限が、実質のバッチサイズとして機能している
2. **replay 中の `Shutdown` で強制フラッシュしてしまう**。これは過去のゲーム終了ログで
   あって「もうイベントは来ない」を意味しない。`Shutdown` フラッシュの前提と噛み合わない
3. **キュー上限が機能していない(実質バグ)**。`flush()` はコマンダー名が未確定、または
   `apiKey` 未設定のときキューを保持したまま return する。`maxQueued` の判定は
   `flushIfDue` 側にしかなく、`flush()` が何もせず返してもキューは切り詰められない。
   **API キーを入れずにデーモンを回すとキューが無限に伸びる**
4. **スキップログの間引きがベタ書き**。`uploadHistorical=false` のとき「1 件目と 100 件
   ごと」という条件が `onEvent` に直接書かれている

### 構造

5. **テストが書けない**。`mapping.go`(370 行)を含む判断のほぼ全部が `package main` に
   あり、`//go:wasmimport` を含むためネイティブでリンクできない。テストがあるのは
   `settings/` だけ
6. **1 つの関心が 3 か所に散っている**。コマンダー識別子の学習は `main.go`、変換は
   `mapping.go`、`Shutdown` 判定は `flushIfDue` にある
7. **購読イベントが二重管理**。`manifest.toml` の `events` と `mapping.go` の `switch` に
   同じ 12 個が並び、README に「両方直すこと」と注記されている
8. **`map[string]any` を突っつく手続き的なコード**。全マッパーが `str(p, "StarSystem")`
   のように文字列キーで掘り、取り出すたびに `ok` を確認する分岐が積み上がる。どのイベント
   がどんな形なのかコードから読み取れない
9. **ログがフロー制御を兼ねている**。`flush()` の内側から `logf(...)` を呼び、「ログを
   出して黙って return」で制御している

## 中核の方針

- **replay と live は別モード**。replay はバックログを流し切るための経路、live は INARA へ
  の配慮(最短間隔)を守るための経路として分ける
- **キュー上限は「保持する直近件数」**。保険ではなく、超えたら古いものから捨てる仕様として
  定義する
- **判断はすべて `main` の外**。`main` はホスト境界のアダプタに徹し、設定を渡す・バイト列を
  送る・返り値をログに整形する、だけを行う
- **ログは依存にしない**。ロジック層は何が起きたかを値で返し、文字列の整形とレベル付けは
  `main` が行う
- **JSON は型で受ける**。`map[string]any` を廃し、イベントごとの構造体へデコードする

## パッケージ構成

```
examples/plugins/inara-uploader/
  main.go            ホスト境界のアダプタのみ(判断を持たない)
  manifest.toml
  settings/          設定値の解釈(既存のまま)
  mapping/           Journal → INARA 変換 + ハンドラレジストリ
  inara/             リクエスト/レスポンスの型、応答の解釈
  uploader/          キュー・モード・フラッシュ判定
```

依存は一方向: `main` → `uploader` → `mapping` / `inara` / `settings`。

`main.go` に残すのは 3 つだけ。

- `hostsettings.GetAll()` の文字列を `settings.Parse` へ渡す
- `driverhttp.Send` を呼び、**HTTP ステータスと本文バイト列をそのまま返す**
- `uploader.Outcome` を文字列とログレベルへ整形し、`hostlog.Log` へ流す

ステータスコードの成否判定もレスポンス JSON の解釈も `inara` に置く。`main` に判断を
残すと、そこはテストできない場所になる。

## 振る舞い

### replay / live モード

`uploader` は `sawLive bool` を持ち、`ev.Replay == false` のイベントを初めて見た時点で
live へ確定する(以後は戻らない)。

**replay モード**

- フラッシュ条件は「キューが `replayBatchSize`(定数 100)以上」のみ
- `minIntervalSeconds` は無視する
- `Shutdown` を受けても強制フラッシュしない

**replay → live の遷移時**

- 溜まっている端数を無条件でフラッシュする
- `uploadHistorical=false` でスキップした件数を、ここで合計 1 回だけ返す
  (現行の「1 件目と 100 件ごと」の間引きは廃止)

**live モード**(現行と同じ)

- `Shutdown` を受けた → 強制フラッシュ
- キューが `batchSize` 以上、かつ前回送信から `minIntervalSeconds` 経過 → フラッシュ

`replayBatchSize`(100)< `maxQueued`(200)なので、送信可能な状態なら上限に達する前に
必ず流れる。

### キュー(リングバッファ)

`maxQueued = 200` を「保持する直近件数」として定義し直す。

- 追加時に上限を超えたら**先頭(古いもの)から捨てる**
- 捨てた件数は `Outcome.Dropped` で返す(`main` がログに出す)
- `apiKey` 未設定や capability 未承認でフラッシュできない状態が続いてもメモリは伸びない

古いものから捨てるのは、INARA が「現在の状態」を反映するサービスであり、古い travel ログ
を落として最新を残すほうが実害が小さいため。

### 送信結果の扱い(現行維持)

| 状況 | 扱い |
|---|---|
| 送信失敗(ネットワーク・タイムアウト・未承認) | キューを保持して次のイベントで再試行 |
| INARA がバッチ全体を拒否(`header.eventStatus != 200`) | キューを破棄。API キー不正など恒久エラーで、送り直しても通らない |
| INARA が個々のイベントを拒否 | 該当分のみ `Outcome.Rejected` で報告。再送しない |
| ペイロードの組み立て失敗 | キューを破棄(残しても次回同じ失敗をする) |
| `dryRun` | 送信せず、組み立てた JSON を `Outcome` に載せる |
| コマンダー名が未確定 / `apiKey` 未設定 | 送信せずキューを保持(上限を超えた分は破棄される) |

## 構造

### `uploader`

グローバル変数 `var st = &state{...}` と、状態を引数で回すフリー関数群
(`flush(cfg)` / `flushIfDue(cfg, name)`)を `Uploader` 構造体のメソッドへ畳む。

```go
// 消費側で定義する最小のインターフェース
type Sender interface {
	Send(body []byte) (status int, body []byte, err error)
}

type Uploader struct {
	now    func() time.Time
	sender Sender
	// 以下は内部状態
}

func New(now func() time.Time, sender Sender) *Uploader

func (u *Uploader) Handle(cfg settings.Settings, ev Event) Outcome
```

ログは吐かず、何が起きたかを値で返す。

```go
type Outcome struct {
	Queued   int                // このイベントで積んだ INARA イベント数
	Sent     int                // 送信できた件数
	Dropped  int                // 上限超過で捨てた件数
	Skipped  int                // uploadHistorical=false で送らなかった件数(遷移時に合計)
	Held     string             // 送信を見送った理由(コマンダー名未確定など)。空なら見送りなし
	DryRun   []byte             // dryRun のとき、組み立てた JSON
	Rejected []inara.Rejection  // INARA が個別に拒否したイベント
	Err      error              // 送信失敗・デコード失敗
}
```

テストは captured log buffer の文字列照合ではなく、返り値のフィールドを直接検証する。
注入する依存は `now` と `Sender` の 2 つだけ。

### `mapping` — ハンドラレジストリ

`switch` を単一のレジストリへ置き換え、散っていた 3 つの関心を 1 か所に並べる。

```go
type handler struct {
	// learn はコマンダー名など、送信対象ではない情報の取り込み(不要なら nil)
	learn func(raw json.RawMessage, st *State) error
	// convert は INARA イベントへの変換(送信しないイベントは nil)
	convert func(raw json.RawMessage, st *State) ([]inara.Event, error)
	// flushLive は live モードで即時フラッシュを促すか(Shutdown のみ true)
	flushLive bool
}

var handlers = map[string]handler{...}
```

| 例 | learn | convert | flushLive |
|---|---|---|---|
| `Commander` | ✓ | — | — |
| `LoadGame` | ✓ | ✓ | — |
| `FSDJump` | — | ✓ | — |
| `Shutdown` | — | — | ✓ |

### JSON を型で受ける

`payloadJSON` は `json.RawMessage` のまま持ち回り、**マッチしたハンドラだけが自分の型へ
デコードする**。対応外のイベントはデコードすらしない(現行は全イベントを
`map[string]any` に開いてから捨てている)。

```go
type fsdJump struct {
	StarSystem string      `json:"StarSystem"`
	JumpDist   float64     `json:"JumpDist"`
	StarPos    *[3]float64 `json:"StarPos"`
}

func (j fsdJump) convert(st *State) []inara.Event {
	if j.StarSystem == "" {
		return nil
	}
	st.LastSystem = j.StarSystem
	return []inara.Event{inara.New("addCommanderTravelFSDJump", travelFSDJump{
		System:   j.StarSystem,
		Distance: j.JumpDist,
		Coords:   j.StarPos,
	})}
}
```

デコードのボイラープレートはジェネリクスで 1 回だけ書く(TinyGo 0.34 以降で使える)。

```go
func decoder[T interface{ convert(*State) []inara.Event }]() convertFunc {
	return func(raw json.RawMessage, st *State) ([]inara.Event, error) {
		var v T
		if err := json.Unmarshal(raw, &v); err != nil {
			return nil, err
		}
		return v.convert(st), nil
	}
}

var handlers = map[string]handler{
	"FSDJump": {convert: decoder[fsdJump]()},
}
```

送信側(`inara`)も `map[string]any` の組み立てをやめ、イベントごとの型に `json` タグを
付ける。省略可能な項目は `omitempty`、`0` が有効値のもの(`Loan` など)はポインタで表す。
README の対応表とコードが 1 対 1 で読めるようになる。

### エラー

`json.Unmarshal` の失敗を `logf(WARN, ...)` して return する箇所などは `error` として
上へ返す。判断を持つのは `Handle` の一段だけにする。

### 細部

`lower()` の手書き実装は `strings.ToLower` に置き換える(TinyGo で問題なく使える)。

## テスト

| パッケージ | 内容 |
|---|---|
| `settings/` | 既存のまま |
| `mapping/` | 各ハンドラの変換結果。特に `Rank`/`Progress` の相互待ち合わせ、`EngineerProgress` の配列形式と単体形式、`Died` の星系補完 |
| `inara/` | 応答の解釈(バッチ拒否、個別イベント拒否、壊れた JSON、非 2xx) |
| `uploader/` | 本命。固定時計とスタブ `Sender` を注入し、下表を検証 |

`uploader` で検証すること:

- replay 中は `minIntervalSeconds` を無視して `replayBatchSize` ごとに送る
- replay 中の `Shutdown` ではフラッシュしない
- replay → live の遷移で端数がフラッシュされる
- 遷移時に `Skipped` の合計が 1 回だけ返る
- 上限超過で古いものから落ち、`Dropped` に載る
- `apiKey` 未設定でもキュー長が `maxQueued` を超えない(現行バグの回帰テスト)
- 送信失敗でキューが残り、次のイベントで再試行される
- バッチ拒否でキューが破棄される

### manifest との整合テスト

`manifest_test.go` で `manifest.toml` を読み、`events` の集合が `handlers` のキー集合と
一致することを検証する。ズレたらテストが落ちるので、README の「両方直すこと」の注記が
不要になる。

TOML パーサの依存追加は避け、`events = [ ... ]` ブロックだけを抜き出すヘルパーで済ませる。
テスト専用かつ対象が自分の管理下にあるファイル 1 つなので、`go.sum` に依存を増やすほうが
割に合わない。

### 購読イベントの決め方

`manifest.toml` の列挙は維持する。ホスト側は `events = ["*"]` も受け付けるが
(`core/src/plugin/manifest.rs:537`。`status` は `*` に含まれない)、全 Journal イベントが
毎回 WASM 境界を越えることになり、ホストのイベントキュー(32 件、溢れると破棄)を圧迫する。
「宣言したものしか受け取らない」は capability と同じ設計方針でもある。二重管理は上の
整合テストで解消する。

## ドキュメント更新

- `README.md`: 送信タイミングの節を replay / live の 2 モードで書き直し、キュー上限の意味
  (直近 200 件を保持、超過分は古いものから破棄)を明記。「イベントを増やすときは両方直す
  こと」の注記を「テストがズレを検出する」に差し替え
- 「不足している実装」の 1 番(定期実行・終了フックが無い)は `Shutdown` 頼みの回避策で
  ある点が変わらないので維持。2 番(`secret` 型が無い)以降もそのまま

## スコープ外

- **キューのファイル永続化**(`driver-fs` の利用)。デーモン再起動をまたいだ復元は魅力的
  だが、`manifest.toml` に fs capability を足してユーザーに承認を求める必要があり、
  「未送信キューを保存したいだけ」で「フォルダの読み書き・削除」の承認画面を出すのは
  割に合わない。edlr は Journal 位置を永続化しているので、再起動後は replay で取り直せる
- **INARA イベントの追加対応**(`Market` / ミッション / 探査など)。構造の入れ替えとは独立
- **`plugin.wasm` の再ビルド**。作業環境に TinyGo と `wasm-tools` が無く、検証は
  `go build` / `go vet` / `go test` までとなる。ビルドと実機確認は別途行う
