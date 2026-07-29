# プラグインを書く(TinyGo)

edlr のプラグインを、何も無いところから 1 つ育てていく。最後まで進むと、
FSDJump を拾って設定でふるいに掛け、外部 API を叩き、定期実行で後処理をし、
自分で書いたドライバと会話するプラグインができる。

Rust で書きたい場合は [plugin-tutorial-rust.md](plugin-tutorial-rust.md)
を参照(内容は同じで、コードだけが違う)。書き上げた後に仕様を引くための
リファレンスは [plugins.md](plugins.md) にある。

完成形は `examples/plugins/tutorial-jump-log-go`(プラグイン)と
`examples/drivers/tutorial-tracker-go`(6 章で作るドライバ)にある。詰まったら
そちらと見比べればよい。

- [0. 前提](#0-前提)
- [1. プラグインの仕組み](#1-プラグインの仕組み)
- [2. FSDJump をログに出す](#2-fsdjump-をログに出す)
- [3. 設定を読む](#3-設定を読む)
- [4. HTTP で外に出る](#4-http-で外に出る)
- [5. 定期実行と終了フック](#5-定期実行と終了フック)
- [6. ドライバを書いて bus で繋ぐ](#6-ドライバを書いて-bus-で繋ぐ)
- [7. うまくいかないとき](#7-うまくいかないとき)
- [8. 次に読むもの](#8-次に読むもの)

## 0. 前提

必要なもの:

- [TinyGo](https://tinygo.org/getting-started/install/) 0.34 以降
  (このチュートリアルは 0.41.1 で確認している)
- Go 1.23 以降(TinyGo が内部で使う)
- [`wasm-tools`](https://github.com/bytecodealliance/wasm-tools) —
  バインディング生成が PATH に要求する
- バインディング生成器

      go install go.bytecodealliance.org/cmd/wit-bindgen-go@v0.6.2

- edlr のソース。プラグインは `core/wit` の WIT 定義に対してビルドするので、
  リポジトリを手元に置いておく

      git clone <edlr のリポジトリ> && cd edlr

**Elite Dangerous は要らない。** Journal はただのテキストファイルなので、
自分で書けばプラグインは動く(2 章でそうする)。

このチュートリアルではデーモンをリポジトリから直接動かし、プラグインは
専用のディレクトリへ入れる。既に使っている `~/.config/edlr` を汚さないよう、
作業用のディレクトリを決めておく:

    mkdir -p /tmp/edlr-tutorial/{plugins,drivers,settings,grants,state,journal}

## 1. プラグインの仕組み

プラグインは **WebAssembly コンポーネント**で、wasmtime のサンドボックスの
中で動く。ネットワークもファイルもプロセスも、既定では一切触れない。

ホストとのやり取りは `core/wit/plugin.wit` の定義が全て。プラグインが
**呼べるもの**(import)と、ホストから**呼ばれるもの**(export)がある。

| プラグインが呼べる | 用途 |
| --- | --- |
| `host-log` | ログ出力 |
| `host-settings` | 設定値の取得 |
| `driver-http` | HTTP リクエスト(承認が要る) |
| `driver-process` | サイドカープロセス(承認が要る) |
| `driver-fs` | ファイルアクセス(承認が要る) |
| `bus` | ドライバとの通信(承認が要る) |

| ホストから呼ばれる | いつ |
| --- | --- |
| `init` | ロード直後に 1 回 |
| `on-event` | 購読した Journal / Status イベントが届いたとき |
| `on-message` | 購読中のドライバのトピックに値が流れたとき |
| `on-schedule` | manifest で宣言した定期実行の時刻 |
| `on-stop` | デーモンの graceful shutdown 時に 1 回(best-effort) |

プラグイン 1 つは `<plugins-dir>/<id>/` というディレクトリで、中身は
`manifest.toml`(何者で、何を要求するか)と wasm ファイルの 2 つ。

### ビルド対象の world は `plugin-guest`

`plugin.wit` には `plugin` と `plugin-guest` の 2 つの world がある。
**Go では必ず `plugin-guest` を対象にすること。** `plugin-guest` は `plugin` に
WASI の import 一式を足したもので、Go の標準ライブラリはプラグインが何も
呼ばなくても WASI を import する(環境変数の初期化で `wasi:cli/environment`、
`time.Now()` で `wasi:clocks/wall-clock` など)。`plugin` を直接対象にすると
「world に無い import がモジュールに含まれている」としてコンポーネント化に
失敗する。

一方、**バインディングの生成は `--world plugin` で行う**。生成したいのは
edlr 独自のインターフェースのぶんだけで、WASI 側は TinyGo が面倒を見るため。
生成とビルドで指定する world が違うのは意図的である。

## 2. FSDJump をログに出す

**この章でできること**: プラグインがロードされ、ジャンプするたびに
ログが出るようになる。

### プロジェクトを作る

```
mkdir tutorial-jump-log-go && cd tutorial-jump-log-go

cat > go.mod <<'EOF'
module github.com/himanoa/edlr/examples/plugins/tutorial-jump-log-go

go 1.23.0

require go.bytecodealliance.org/cm v0.3.0
EOF

wit-bindgen-go generate --world plugin --out gen <edlr のパス>/core/wit
```

`gen/` の下に `edlr/plugin/host-log` などのパッケージができる。**生成物は
コミットしてよい**(普段のビルドでは再生成不要)。再生成が要るのは
`core/wit` が変わったときだけ。

### コード

`main.go`:

```go
package main

import (
	"encoding/json"
	"fmt"

	"go.bytecodealliance.org/cm"

	hostlog "github.com/himanoa/edlr/examples/plugins/tutorial-jump-log-go/gen/edlr/plugin/host-log"
	plugin "github.com/himanoa/edlr/examples/plugins/tutorial-jump-log-go/gen/edlr/plugin/plugin"
)

// init で export を登録する。ホストはここで登録した関数を直接呼ぶ。
func init() {
	plugin.Exports.Init = onInit
	plugin.Exports.OnEvent = onEvent
	plugin.Exports.OnMessage = func(string, string, cm.List[uint8]) {}
	plugin.Exports.OnSchedule = func(string) {}
	plugin.Exports.OnStop = func() {}
}

// main は TinyGo がコンポーネントをビルドするために要る。エントリポイント
// としては使われない。
func main() {}

func onInit() {
	hostlog.Log(hostlog.LevelInfo, "tutorial-jump-log started")
}

func onEvent(ev plugin.Event) {
	var v struct {
		StarSystem string  `json:"StarSystem"`
		JumpDist   float64 `json:"JumpDist"`
	}
	if err := json.Unmarshal([]byte(ev.PayloadJSON), &v); err != nil {
		hostlog.Log(hostlog.LevelWarn, "broken payload: "+err.Error())
		return
	}
	hostlog.Log(hostlog.LevelInfo,
		fmt.Sprintf("jumped to %s (%.2f ly)", v.StarSystem, v.JumpDist))
}
```

- **export は `init()` の中で `plugin.Exports.*` に代入して登録する**。
  5 つとも埋めること(使わないものは空の関数でよい)
- `main` は空でよいが、無いとビルドできない
- `option<string>` は `cm.Option[string]` になる。中身は `.Some()` が返す
  ポインタで取り出す(`nil` なら値なし)
- `list<u8>` は `cm.List[uint8]`。`.Slice()` で `[]byte` になる

`manifest.toml`:

```toml
id = "tutorial-jump-log-go"
name = "Jump Log (TinyGo tutorial)"
version = "0.1.0"
entry = "plugin.wasm"
events = ["FSDJump"]
```

- `id` は `[a-z0-9-]+`。**配置先のディレクトリ名と一致していないとロードに失敗する**
- `events` に書いたイベントしか `on-event` に届かない。`"*"` で全 journal
  イベント、`"status"` で `Status.json` の更新
- **トップレベルのキーは、テーブルヘッダ(`[[settings]]` など)より前に書く**。
  後ろに書くとそのテーブルの子として解釈され、ロードに失敗する

### ビルドして配置

ビルドコマンドは長いので `build.sh` にしておく:

```bash
#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
wit="<edlr のパス>/core/wit"
out="${1:-$here/plugin.wasm}"

cd "$here"
tinygo build -target=wasip2 \
  --wit-package "$wit" \
  --wit-world plugin-guest \
  -o "$out" .

echo "built: $out"
```

```
chmod +x build.sh && ./build.sh

mkdir -p /tmp/edlr-tutorial/plugins/tutorial-jump-log-go
cp plugin.wasm manifest.toml /tmp/edlr-tutorial/plugins/tutorial-jump-log-go/
```

できたものは自分で覗ける:

```
wasm-tools validate --features all plugin.wasm
wasm-tools component wit plugin.wasm | grep export
```

`init` / `on-event` / `on-message` / `on-schedule` / `on-stop` の 5 つが
export されていれば正しい。

### 動かす

Journal ファイルを 1 つ用意する。ファイル名は **`Journal.` で始まり `.log` で
終わる**必要がある:

```
cat > /tmp/edlr-tutorial/journal/Journal.2026-07-29T100000.01.log <<'EOF'
{"timestamp":"2026-07-29T10:00:00Z","event":"Fileheader","part":1}
EOF
```

デーモンを起動する(edlr のリポジトリで):

```
cargo run -p edlr-core --bin edlr -- \
  --journal-dir   /tmp/edlr-tutorial/journal \
  --plugins-dir   /tmp/edlr-tutorial/plugins \
  --drivers-dir   /tmp/edlr-tutorial/drivers \
  --settings-dir  /tmp/edlr-tutorial/settings \
  --grants-dir    /tmp/edlr-tutorial/grants \
  --state-dir     /tmp/edlr-tutorial/state
```

別の端末からジャンプを 1 つ書き足す:

```
echo '{"timestamp":"2026-07-29T10:00:10Z","event":"FSDJump","StarSystem":"Sol","JumpDist":8.19}' \
  >> /tmp/edlr-tutorial/journal/Journal.2026-07-29T100000.01.log
```

### 確認する

デーモンのログ(stderr)に次の 3 行が順に出れば成功:

```
INFO edlr_core::plugin::manifest: plugin manifest loaded id="tutorial-jump-log-go" events=1 settings=0 capabilities=0 ... schedules=0
INFO edlr_core::plugin::host: tutorial-jump-log started plugin_id="tutorial-jump-log-go"
INFO edlr_core::plugin::host: jumped to Sol (8.19 ly) plugin_id="tutorial-jump-log-go"
```

1 行目の `events=1` などは manifest の読み取り結果。**宣言したはずの項目が
0 になっていたら、そこが読めていない**(綴り違いなど)。

**`host-log` の `LevelDebug` はどこにも出ない。** デーモンのログレベルは
INFO 固定で(`RUST_LOG` も効かない)、GUI へ転送されるのも INFO 以上。
動作確認に使うログは `LevelInfo` 以上で出すこと。

### うまくいかないとき

| 症状 | 原因 |
| --- | --- |
| `component new` / TinyGo のビルドが import で失敗する | `--wit-world` に `plugin` を指定している。`plugin-guest` にする |
| `manifest loaded` が出ない | ディレクトリ名と `id` が違う / `manifest.toml` が無い |
| `plugin` の起動ログが出ない | `entry` が指す wasm が無い、または world 違いでロードに失敗している。デーモンのログに理由が出る |
| `jumped to` が出ない | `events` に `FSDJump` が入っていない / Journal ファイル名が `Journal.*.log` になっていない |
| 何を書き足しても反応しない | プラグインの入れ替えは**デーモンの再起動が要る**(ロードは起動時に一度だけ) |

Journal の読み取り位置は `--state-dir` に永続化される。**同じ行を二度は
配信しない**ので、確認をやり直すときは新しい行を書き足すこと(あるいは
`--state-dir` を消す)。デーモンが動き出す前に既に書かれていた行も届くが、
その場合は `ev.Replay` が `true` になる。

## 3. 設定を読む

**この章でできること**: 短いジャンプを無視するようになり、そのしきい値を
GUI から変えられるようになる。

### manifest に宣言する

```toml
[[settings]]
key = "enabled"
label = "有効にする"
type = "boolean"
default = true

[[settings]]
key = "minDistance"
label = "記録する最小跳躍距離 (ly)"
type = "number"
default = 0
```

`type` は `boolean` / `string` / `number` / `select` / `secret`。GUI の
Plugins ページはこの宣言だけを見てフォームを描く。値は
`<settings-dir>/<id>.json` に保存され、未保存のキーは `default` に落ちる。

### ロジックは `main` の外に置く

`main` パッケージは `//go:wasmimport` を含むためネイティブでリンクできず、
**テストが書けない**。判断を持つコードは別パッケージへ出しておくと
`go test` で確かめられる。

`jumplog/jumplog.go`:

```go
package jumplog

import "encoding/json"

type Settings struct {
	Enabled     bool
	MinDistance float64
}

// ParseSettings は host-settings.get-all() の JSON を解釈する。
// 壊れた JSON や未設定のキーは既定値へ倒す(プラグインを止めない)。
func ParseSettings(raw string) Settings {
	var v struct {
		Enabled     *bool    `json:"enabled"`
		MinDistance *float64 `json:"minDistance"`
	}
	s := Settings{Enabled: true, MinDistance: 0}
	if err := json.Unmarshal([]byte(raw), &v); err != nil {
		return s
	}
	if v.Enabled != nil {
		s.Enabled = *v.Enabled
	}
	if v.MinDistance != nil {
		s.MinDistance = *v.MinDistance
	}
	return s
}

type Jump struct {
	System   string
	Distance float64
}

func ParseJump(payloadJSON string) (Jump, bool) {
	var v struct {
		StarSystem string  `json:"StarSystem"`
		JumpDist   float64 `json:"JumpDist"`
	}
	if err := json.Unmarshal([]byte(payloadJSON), &v); err != nil {
		return Jump{}, false
	}
	if v.StarSystem == "" {
		return Jump{}, false
	}
	return Jump{System: v.StarSystem, Distance: v.JumpDist}, true
}
```

`main.go` 側:

```go
func onEvent(ev plugin.Event) {
	// manifest の events で絞ってはいるが、購読を増やしたときに壊れない
	// よう、ここでも名前を確かめる。
	if name := ev.Name.Some(); name == nil || *name != "FSDJump" {
		return
	}

	settings := jumplog.ParseSettings(hostsettings.GetAll())
	if !settings.Enabled {
		return
	}

	jump, ok := jumplog.ParseJump(ev.PayloadJSON)
	if !ok {
		logf(hostlog.LevelWarn, "could not read StarSystem from the payload")
		return
	}
	if jump.Distance < settings.MinDistance {
		logf(hostlog.LevelInfo, "skipping %s (%.2f ly < %.2f ly)",
			jump.System, jump.Distance, settings.MinDistance)
		return
	}

	logf(hostlog.LevelInfo, "jumped to %s (%.2f ly)", jump.System, jump.Distance)
}

func logf(level hostlog.Level, format string, args ...any) {
	hostlog.Log(level, fmt.Sprintf(format, args...))
}
```

**`init` で読んで持ち回らないこと。** 設定変更は稼働中のプラグインへ即座に
反映される仕様で、それが効くのは毎回読み直しているからである。

### TinyGo でもテストを走らせる

TinyGo の `reflect` は標準ライブラリと差があり、`encoding/json` の挙動が
変わりうる。ネイティブで緑でも wasm 上で壊れることがあるので、両方走らせる:

```
go test ./...                        # ネイティブ
tinygo test -target=wasip1 ./jumplog/ # wasm 上(要 wasm ランタイム)
go vet ./...                          # main を含む全パッケージの型チェック
```

### 確認する

ビルドして配置し直し、デーモンを**再起動**してから GUI の Plugins ページで
`minDistance` を `20` にする(GUI の動かし方は [ui.md](ui.md))。しきい値を
下回るジャンプを書き足すと:

```
INFO edlr_core::plugin::host: skipping Alioth (5.00 ly < 20.00 ly) plugin_id="tutorial-jump-log-go"
```

上回るジャンプなら従来どおり `jumped to ...` が出る。
`/tmp/edlr-tutorial/settings/tutorial-jump-log-go.json` に保存されているのも
確認できる。

### うまくいかないとき

| 症状 | 原因 |
| --- | --- |
| Plugins ページにフォームが出ない | `[[settings]]` がトップレベルキーより前に書かれていて、そのテーブルの子になっている。`manifest loaded` の `settings=` が 0 になっていないか見る |
| 値を変えても挙動が変わらない | `init` で読んで保持している / ビルドし直した wasm を配置していない |
| ネイティブでは通るのに wasm で値が取れない | TinyGo の `encoding/json` の差。`tinygo test -target=wasip1` で再現するか確かめる |

## 4. HTTP で外に出る

**この章でできること**: 跳んだ先の星系を [EDSM](https://www.edsm.net) に
問い合わせ、その結果をログに出せるようになる。

edlr のプラグインは既定でネットワークに触れない。使うには manifest で
**接続先を宣言し、ユーザーが GUI で承認する**必要がある。

### manifest に宣言する

```toml
[[capabilities]]
kind = "http"
hosts = ["https://www.edsm.net"]
reason = "跳んだ先の星系が EDSM に登録済みかを問い合わせるため"
```

- `hosts` はスキーム + ホスト(+ ポート)だけ。パスやクエリは書けない
- 判定はスキーム・ホスト・ポートの**完全一致**。サブドメインのワイルドカードは無い
- `reason` は承認画面にそのまま出る。空にはできない

### コード

```go
import (
	driverhttp "github.com/.../gen/edlr/plugin/driver-http"
)

const edsm = "https://www.edsm.net/api-v1/system"

func lookup(jump jumplog.Jump) {
	result := driverhttp.Send(driverhttp.Request{
		Method:  "GET",
		URL:     jumplog.EDSMURL(edsm, jump.System),
		Headers: cm.ToList([][2]string{{"accept", "application/json"}}),
		Body:    cm.None[cm.List[uint8]](),
	})

	if err := result.Err(); err != nil {
		logf(hostlog.LevelWarn, "%s: lookup failed: %s",
			jump.System, driverErrorMessage(err))
		return
	}

	resp := result.OK()
	if resp.Status != 200 {
		logf(hostlog.LevelWarn, "%s: EDSM returned HTTP %d", jump.System, resp.Status)
		return
	}

	body := strings.TrimSpace(string(resp.Body.Slice()))
	// 未知の星系では EDSM は空配列 `[]` を返す。
	if body == "[]" {
		logf(hostlog.LevelInfo, "%s: not known to EDSM", jump.System)
		return
	}
	logf(hostlog.LevelInfo, "%s: EDSM says %s", jump.System, body)
}

// driverErrorMessage は variant の中身(理由文字列)を取り出す。
func driverErrorMessage(err *driverhttp.DriverError) string {
	for _, m := range []*string{
		err.PermissionDenied(), err.InvalidRequest(), err.Transport(),
	} {
		if m != nil {
			return *m
		}
	}
	return "unknown driver error"
}
```

`result` の扱いに注意:

- `result.Err()` は `nil` かエラーへのポインタ。`result.OK()` は成功値への
  ポインタ
- **`Err()` はポインタレシーバなので、`Send(...).Err()` と繋げて書けない。**
  いったん変数に受けること
- variant(`driver-error` など)は「どの case か」を各アクセサが `nil` か
  どうかで返す

星系名にはスペースが入る(`Col 285 Sector AA-A a1`)ので、URL に載せる前に
パーセントエンコードが要る。`jumplog` 側に置いてテストできるようにしておく:

```go
func EDSMURL(endpoint, system string) string {
	return endpoint + "?systemName=" + urlEncode(system) + "&showId=1"
}

func urlEncode(s string) string {
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case c >= 'A' && c <= 'Z', c >= 'a' && c <= 'z', c >= '0' && c <= '9',
			c == '-', c == '_', c == '.', c == '~':
			b.WriteByte(c)
		default:
			fmt.Fprintf(&b, "%%%02X", c)
		}
	}
	return b.String()
}
```

`onEvent` の最後で `lookup(jump)` を呼べば動く。**ただしこれは 5 章で
やめる**(理由もそこで説明する)。

### 確認する

まず**承認しないまま**動かしてみる。ジャンプを書き足すと:

```
WARN edlr_core::plugin::host: Sol: lookup failed: capability not granted plugin_id="tutorial-jump-log-go"
```

未承認でもプラグインは止まらず、`driver-http.send` だけが拒否される。
GUI の Plugins ページで宣言した `hosts` と `reason` を確認して承認すると、
再起動しなくても次の呼び出しから通る:

```
INFO edlr_core::plugin::host: Sol: EDSM says {"name":"Sol","id":27,"id64":10477373803} plugin_id="tutorial-jump-log-go"
```

### 制約(踏む前に知っておくもの)

- **タイムアウトは 1.5 秒**、プラグインからは変えられない
- **リダイレクトを追わない**。3xx はそのまま返る
- `Host` / `Content-Length` / `Transfer-Encoding` / `Connection` などの
  ヘッダは設定できない(`invalid-request` になる)
- リクエスト・レスポンスとも本文は 8 MiB まで
- `Send` は同期呼び出しで、**返るまでこのプラグインは次のイベントを読まない**

### 承認が失効するとき

manifest の要求内容(`hosts` と `reason` の集合)を変えると、以前の承認は
自動的に失効する。GUI では「未承認」に見え、ユーザーが新しい内容を確認して
承認し直すまで通らない。**逆に、manifest を変えずに wasm だけ差し替えた
場合は承認が引き継がれる**(承認は要求内容に紐づいており、バイナリの
ハッシュは見ていない)。

### API キーが要るサービスなら

`type = "secret"` の設定を使う。GUI ではマスク入力になり、`plugins/list` など
RPC の応答には値が載らない(プラグイン自身は `host-settings.get-all` で
普通に受け取れる)。`default` は書けない。詳細は
[plugins.md](plugins.md#秘密情報type--secret)。実例は
`examples/plugins/inara-uploader`。

## 5. 定期実行と終了フック

**この章でできること**: HTTP をイベント処理から追い出し、定期実行の側で
少しずつ流すようになる。停止時には流し残しを報告する。

### なぜイベント処理から追い出すのか

4 章の書き方には 2 つ問題がある。

1. **`on-event` の呼び出し全体に 2 秒の期限がある**。`driver-http.send` の
   1.5 秒を使い切ると、JSON の処理を足しただけで期限を超える(TinyGo の
   `encoding/json` は reflect 経由で速くない)
2. **`Send` の間、このプラグインはイベントを読まない**。ホスト側の作業キューは
   64 件しかなく、溢れた分は捨てられる(捨てた数は GUI の "Dropped" に出る)。
   戦闘中や、起動直後にバックログを流している最中に効いてくる

そこで `on-event` はキューに積むだけにして、実際の問い合わせは定期実行で行う。

### manifest に宣言する

```toml
[[schedule]]
name = "flush"
interval-seconds = 10
```

- `name` は `[a-z0-9-]+`。同じ manifest 内で重複できない
- `interval-seconds` と `cron`(5 欄形式、ローカル時刻)は**どちらか一方だけ**
- 発火間隔の下限は 5 秒。下回る値は 5 秒に丸められ、warn が出る
- 発火を何度取りこぼしても、次の評価で 1 回だけ呼ばれる(まとめて連続では
  呼ばれない)。`cron` に限り `catch-up = true` で、デーモンが止まっていた間の
  定刻を起動時に 1 回だけ追い掛けられる

### コード

キューも `jumplog` 側に置けばテストできる:

```go
type Queue struct {
	capacity int
	items    []Jump
}

func NewQueue(capacity int) *Queue { return &Queue{capacity: capacity} }

// Push は末尾へ積む。上限を超えたら古いものから捨てる。
func (q *Queue) Push(j Jump) {
	if len(q.items) >= q.capacity {
		q.items = q.items[1:]
	}
	q.items = append(q.items, j)
}

func (q *Queue) Pop() (Jump, bool) {
	if len(q.items) == 0 {
		return Jump{}, false
	}
	j := q.items[0]
	q.items = q.items[1:]
	return j, true
}

func (q *Queue) Len() int { return len(q.items) }
```

`main.go` 側。`onEvent` の末尾は `lookup(jump)` をやめて `pending.Push(jump)` に
差し替え、問い合わせは `onSchedule` へ移す:

```go
var pending *jumplog.Queue

func onInit() {
	pending = jumplog.NewQueue(50)
	logf(hostlog.LevelInfo, "tutorial-jump-log started")
}

func onSchedule(name string) {
	// 宣言したスケジュールが 1 つでも、名前は確かめておく
	// (増やしたときにここで分岐することになる)。
	if name != "flush" {
		logf(hostlog.LevelWarn, "unknown schedule: %s", name)
		return
	}

	jump, ok := pending.Pop()
	if !ok {
		return
	}

	// 呼び出し期限 2 秒 / HTTP タイムアウト 1.5 秒。
	// 1 回の呼び出しで叩けるのは実質 1 回だけ。
	lookup(jump)

	if n := pending.Len(); n > 0 {
		logf(hostlog.LevelInfo, "%d jump(s) still queued", n)
	}
}

// onStop はデーモンの graceful shutdown で一度だけ呼ばれる。
func onStop() {
	if pending.Len() == 0 {
		return
	}
	logf(hostlog.LevelInfo, "stopping with %d unflushed: %s",
		pending.Len(), pending.Summary())
}
```

`init()` での登録も忘れずに差し替える:

```go
	plugin.Exports.OnSchedule = onSchedule
	plugin.Exports.OnStop = onStop
```

**`onStop` で HTTP を叩かないのは意図的。** 猶予は既定 5 秒しかなく、
応答しないホストを掴むとそこへ辿り着けないまま終わる。ここでやるべきなのは
「速く終わる後始末」だけ。

`on-stop` について確かなこと:

- 呼ばれるのは**デーモンの graceful shutdown のときだけ**。trap による無効化の
  後には呼ばれず、`SIGKILL` やクラッシュでも当然呼ばれない
- 猶予時間内に限った best-effort。停止の合図は作業キューを追い越すので、
  キューに積み残しがあるだけなら到達できる

### 確認する

ジャンプを 2 件、10 秒以内に書き足してから Ctrl-C で止める:

```
INFO edlr_core::plugin::host: jumped to Sol (8.19 ly)
INFO edlr_core::plugin::host: jumped to Achenar (3.20 ly)
INFO edlr_core::plugin::host: Sol: EDSM says {"name":"Sol","id":27,"id64":10477373803}
INFO edlr_core::plugin::host: 1 jump(s) still queued
INFO edlr: received SIGINT, shutting down
INFO edlr_core::plugin::host: stopping with 1 unflushed: Achenar (3.20 ly)
```

### うまくいかないとき

| 症状 | 原因 |
| --- | --- |
| `on-schedule` が呼ばれない | `[[schedule]]` が別テーブルの子になっている(`manifest loaded` の `schedules=` を見る)/ `interval-seconds` と `cron` を両方書いた(ロードに失敗する)/ `plugin.Exports.OnSchedule` を登録し忘れている |
| `unknown schedule` が出る | `name` の綴りが manifest とコードでずれている |
| 期限超過でプラグインが無効になる | 1 回の `on-schedule` で HTTP を 2 回以上叩いていないか。7 章を参照 |
| `on-stop` が出ない | キューが空(この実装では何も出さない)/ 停止時に wasm 呼び出しが実行中で猶予に間に合わなかった(warn が出る) |

## 6. ドライバを書いて bus で繋ぐ

**この章でできること**: プラグインが自分で書いたドライバへ訪問先を渡し、
ドライバが集計した値を受け取れるようになる。

### ドライバとは

プラグイン同士は直接話せない。間に立つのが**ドライバ**で、プラグインとは
別レイヤーの wasm コンポーネントである。

- ドライバは journal / status を受け取らない(`on-event` が無い)
- **1 ドライバにつき常駐インスタンスは 1 つ**。複数のプラグインが publish
  しても、宛先はこの 1 つ
- ドライバ同士は話せない(`driver` world は `bus` を import しない)

ドライバは `<drivers-dir>/<id>/` に `driver.toml` と wasm を置く。

### ドライバを書く

別のモジュールとして作り、**バインディングは `--world driver` で生成する**:

```
mkdir tutorial-tracker-go && cd tutorial-tracker-go

cat > go.mod <<'EOF'
module github.com/himanoa/edlr/examples/drivers/tutorial-tracker-go

go 1.23.0

require go.bytecodealliance.org/cm v0.3.0
EOF

wit-bindgen-go generate --world driver --out gen <edlr のパス>/core/wit
```

`main.go`:

```go
package main

import (
	"encoding/json"
	"fmt"

	"go.bytecodealliance.org/cm"

	bushost "github.com/.../gen/edlr/plugin/bus-host"
	driver "github.com/.../gen/edlr/plugin/driver"
	hostlog "github.com/.../gen/edlr/plugin/host-log"
)

var count int

func init() {
	driver.Exports.Init = onInit
	driver.Exports.OnMessage = onMessage
}

func main() {}

func onInit() {
	hostlog.Log(hostlog.LevelInfo, "tutorial-tracker started")
}

func onMessage(from string, topic string, payload cm.List[uint8]) {
	if topic != "visit" {
		return
	}

	system := string(payload.Slice())
	count++
	hostlog.Log(hostlog.LevelInfo,
		fmt.Sprintf("visit #%d from %s: %s", count, from, system))

	body, err := json.Marshal(struct {
		System string `json:"system"`
		Count  int    `json:"count"`
	}{System: system, Count: count})
	if err != nil {
		hostlog.Log(hostlog.LevelWarn, "could not encode the payload: "+err.Error())
		return
	}

	emitted := bushost.Emit("last-system", cm.ToList(body))
	if e := emitted.Err(); e != nil {
		hostlog.Log(hostlog.LevelWarn, "emit failed: "+e.String())
	}
}
```

export は `Init` と `OnMessage` の 2 つだけ。`bushost.Emit` で自分の
トピックへ値を流す。ビルドは `build.sh` の `--wit-world` を
**`driver-guest`** に変えるだけ(生成は `driver`、ビルドは `driver-guest`)。

`driver.toml`:

```toml
id = "tutorial-tracker-go"
name = "Jump Tracker (TinyGo tutorial)"
version = "0.1.0"
entry = "driver.wasm"

[[topics]]
name = "visit"
retain = false
description = "プラグインからの訪問報告(星系名)"

[[topics]]
name = "last-system"
retain = true
description = "最後に訪問した星系と通算訪問回数の JSON"
```

`retain = true` のトピックは直近の値をドライバが持ち続け、承認済みの
プラグインが `bus.Get` でいつでも読める。`retain = false` は配信専用で、
`bus.Get` は常に値なしを返す。

### プラグイン側

manifest に接続先を宣言する:

```toml
[[bus]]
driver = "tutorial-tracker-go"
publish = ["visit"]
subscribe = ["last-system"]
reason = "訪問した星系を tracker ドライバへ渡し、集計された最新値を受け取るため"
```

`publish` / `subscribe` に書いていないトピックは、承認済みでも
`permission-denied` になる。同じドライバを 2 回宣言することはできない。

送る側(`onEvent` の中):

```go
const tracker = "tutorial-tracker-go"

	published := bus.Publish(tracker, "visit", cm.ToList([]byte(jump.System)))
	if err := published.Err(); err != nil {
		logf(hostlog.LevelWarn, "publish failed: %s", busErrorMessage(err))
	}
```

受け取る側 — 購読しているトピックへ値が流れると `OnMessage` が呼ばれる:

```go
func onMessage(driver string, topic string, payload cm.List[uint8]) {
	logf(hostlog.LevelInfo, "%s/%s = %s", driver, topic, string(payload.Slice()))
}
```

retain された値は、配信を待たずいつでも読める(`onSchedule` の中など):

```go
	result := bus.Get(tracker, "last-system")
	if err := result.Err(); err != nil {
		logf(hostlog.LevelWarn, "get failed: %s", busErrorMessage(err))
	} else if v := result.OK().Some(); v != nil {
		logf(hostlog.LevelInfo, "tracker says: %s", string(v.Slice()))
	}
```

`bus.Get` は `result<option<list<u8>>, bus-error>` なので、**エラーの有無と
値の有無を 2 段階で見る**ことになる(`result.OK()` が `cm.Option`、その
`.Some()` が `nil` なら retain 値がまだ無い)。

### ビルドして配置

```
./build.sh   # --wit-world driver-guest に変えたもの

mkdir -p /tmp/edlr-tutorial/drivers/tutorial-tracker-go
cp driver.wasm driver.toml /tmp/edlr-tutorial/drivers/tutorial-tracker-go/
```

### 確認する

デーモンを再起動し、GUI の Plugins ページで bus 接続を承認してから
ジャンプを書き足すと、次のように流れる:

```
INFO edlr_core::driver::host: tutorial-tracker started driver_id="tutorial-tracker-go"
INFO edlr_core::plugin::host: jumped to Sol (8.19 ly)
INFO edlr_core::driver::host: visit #1 from tutorial-jump-log-go: Sol driver_id="tutorial-tracker-go"
INFO edlr_core::plugin::host: tutorial-tracker-go/last-system = {"system":"Sol","count":1}
INFO edlr_core::plugin::host: tracker says: {"system":"Sol","count":1}
```

上から、ドライバの起動 → プラグインがジャンプを見た → ドライバが受け取った
→ 配り直された値がプラグインへ届いた → 次の定期実行で retain 値を読んだ、
という流れになっている。

### うまくいかないとき

| 症状 | 原因 |
| --- | --- |
| `publish failed: bus access to ... is not granted` | 未承認。GUI の Plugins ページで承認する |
| `unknown driver` | `[[bus]]` の `driver` と `driver.toml` の `id` が違う / ドライバが `drivers-dir` に無い |
| `unknown topic` | `[[topics]]` に無いトピック、または `publish`/`subscribe` に書いていないトピック |
| `OnMessage` が呼ばれない | `subscribe` に入れていない / 登録し忘れている / ドライバが `Emit` していない(ドライバ側のログを見る) |
| `bus.Get` が常に値なし | そのトピックが `retain = false` |
| ドライバのログが出ない | `--drivers-dir` を渡し忘れている(存在しなくてもエラーにならず、ドライバ 0 件で起動する) |

承認の失効はプラグインの capability と同じで、`publish` / `subscribe` の集合を
変えると以前の承認は無効になる。

## 7. うまくいかないとき

### プラグインが `Disabled` になった

GUI の Plugins ページに理由が出る。原因は 2 つに分かれる:

- **トラップ**(panic、不正なメモリアクセスなど)— 次に呼んでも同じ結果に
  なる決定的な故障なので、**1 回で** `Disabled`
- **呼び出し期限(2 秒)の超過** — 応答しないホストなどプラグインの責任とは
  限らないので、`init` からやり直して処理を続ける。**3 回連続**で超過して
  初めて `Disabled`

作り直しでは wasm の線形メモリ上の状態(この章までに作ったキューなど)が
失われる。長時間の作業を 1 回の呼び出しに詰め込まないこと。

### ロード時に world が合わないと言われる

`core/wit` を更新したのに古いバインディングでビルドした wasm を置いている。
WIT パッケージは `edlr:plugin@0.4.0` で、**旧 world のプラグインは新しい
ホストへロードできない**。Go では `gen/` の再生成が要る:

```
wit-bindgen-go generate --world plugin --out gen <edlr のパス>/core/wit
```

(ドライバなら `--world driver`)

### イベントが減っている / 届かない

GUI のプラグインカードに "Dropped" が出ていないか見る。作業キュー(64 件、
journal イベントとバス配信で共有)が溢れた件数で、**溢れた分は二度と届かない**
(Journal の読み取り位置は配送の成否と関係なく進む)。同期呼び出しで長く
止まる処理がイベント処理の中にあると起きやすい(5 章)。

### manifest の書き間違い

- テーブルヘッダより後ろに書いたトップレベルキーは、そのテーブルの子に
  なる。edlr は知らないキーを拒否してロードを失敗させる
- トップレベル自体の綴り違い(`evens = [...]`)はロードを失敗させず、warn が
  出るだけ。`manifest loaded` のサマリで宣言が消えていないか確認する

### TinyGo 固有

| 症状 | 原因 |
| --- | --- |
| ビルドが「world に無い import」で落ちる | `--wit-world` が `plugin` / `driver` になっている。ゲストは `plugin-guest` / `driver-guest` |
| `Send(...).Err()` がコンパイルできない | `Err()` はポインタレシーバ。結果をいったん変数に受ける |
| ネイティブと wasm で挙動が違う | `reflect` / `encoding/json` の差。`tinygo test -target=wasip1` で確かめる |
| `main` パッケージのテストが書けない | 書けない(`//go:wasmimport` を含むため)。ロジックを別パッケージへ出す |

## 8. 次に読むもの

- [plugins.md](plugins.md) — manifest の全フィールド、設定 RPC、無効化の条件
- [capabilities.md](capabilities.md) — HTTP に加えて、サイドカープロセス
  (`driver-process`)とファイルアクセス(`driver-fs`)
- [drivers.md](drivers.md) — ドライバの詳細と承認フロー
- [ui.md](ui.md) — GUI の起動方法。`[[dashboard]]` で自前のウィジェットを
  出すこともできる
- [cli.md](cli.md) — デーモンのフラグ、読み取り位置の永続化、`replay`

サンプル:

- `examples/plugins/tutorial-jump-log-go` / `examples/drivers/tutorial-tracker-go`
  — このチュートリアルの完成形
- `examples/plugins/inara-uploader` — TinyGo 製の実用寄りのプラグイン
  (`secret` 設定、バッチ送信、`on-schedule` での flush)
- `examples/plugins/state-reader` — Rust だが、bus とダッシュボード
  ウィジェットの最小例

`examples` にあるものは `./scripts/install-examples.sh <名前>` でビルドと配置を
まとめて行える。
