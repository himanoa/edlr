# プラグインを書く(MoonBit)

edlr のプラグインを、何も無いところから 1 つ育てていく。最後まで進むと、
FSDJump を拾って設定でふるいに掛け、外部 API を叩き、定期実行で後処理をし、
自分で書いたドライバと会話するプラグインができる。

Rust で書きたい場合は [plugin-tutorial-rust.md](plugin-tutorial-rust.md)、
TinyGo なら [plugin-tutorial-tinygo.md](plugin-tutorial-tinygo.md) を参照
(内容は同じで、コードだけが違う)。書き上げた後に仕様を引くための
リファレンスは [plugins.md](plugins.md) にある。

完成形は `examples/plugins/tutorial-jump-log-mbt`(プラグイン)と
`examples/drivers/tutorial-tracker-mbt`(6 章で作るドライバ)にある。詰まったら
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

- [MoonBit toolchain](https://www.moonbitlang.com/download)
  (このチュートリアルは moon 0.1.20260309 で確認している)
- [`wasm-tools`](https://github.com/bytecodealliance/wasm-tools) —
  コンポーネント化に使う(1.254.0 で確認)

      cargo install wasm-tools

- バインディング生成器。**必ず 0.45 系を入れること**(理由は 7 章):

      cargo install wit-bindgen-cli --version 0.45.0

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
| `on-job-complete` | `driver-http.submit-send` したジョブが完了したとき |
| `on-stop` | デーモンの graceful shutdown 時に 1 回(best-effort) |

プラグイン 1 つは `<plugins-dir>/<id>/` というディレクトリで、中身は
`manifest.toml`(何者で、何を要求するか)と wasm ファイルの 2 つ。

### MoonBit からコンポーネントへの道のり

MoonBit のコンパイラが出すのは素の wasm モジュールで、そのままでは
コンポーネントではない。手順は 3 段になる:

1. `wit-bindgen moonbit` で `core/wit` から MoonBit のバインディングを生成する
2. `moon build --target wasm` でコアモジュールをビルドする
3. `wasm-tools component embed` + `wasm-tools component new` で
   コンポーネントへ包む

3 で **`--encoding utf16` を必ず付ける**。MoonBit の文字列は UTF-16 で、
これを落とすとビルドもロードも通るのに、ログや payload が全て文字化けする
(canonical ABI が UTF-8 として読むため)。

### 生成とコンポーネント化で指定する world が違う

`plugin.wit` には `plugin` と `plugin-guest` の 2 つの world がある。
`plugin-guest` は `plugin` に WASI の import 一式を足したもので、Go のように
標準ライブラリが WASI を import する言語のためにある。

- **バインディングの生成は `--world plugin`** で行う。生成したいのは
  edlr 独自のインターフェースのぶんだけで、WASI の import に対応する
  MoonBit コードは要らない
- **コンポーネント化(`component embed`)は `-w plugin-guest`** で行う。
  MoonBit の標準ライブラリは WASI を import しないので実は `plugin` でも
  通るが、world 側の余剰 import は無害なので、ゲスト共通の流儀
  (「ゲストは言語を問わず `plugin-guest`」)に合わせておく

## 2. FSDJump をログに出す

**この章でできること**: プラグインがロードされ、ジャンプするたびに
ログが出るようになる。

### プロジェクトを作る

```
mkdir tutorial-jump-log-mbt && cd tutorial-jump-log-mbt

wit-bindgen moonbit <edlr のパス>/core/wit --world plugin \
  --derive-show --derive-eq --out-dir .
```

moon プロジェクトが丸ごと生成される:

```
moon.mod.json            # モジュール定義(name は WIT パッケージ由来の "edlr/plugin")
ffi/                     # 文字列・配列を線形メモリと往復させる低レベル層
interface/edlr/plugin/   # import の呼び口(hostLog / driverHttp / bus など)
world/plugin/            # world の型(Event など)
gen/                     # export の配線と、リンク設定(moon.pkg.json)
gen/world/plugin/stub.mbt  # ★ 実装を書く場所
```

**生成物はコミットしてよい**(普段のビルドでは再生成不要)。再生成が要るのは
`core/wit` が変わったときだけで、その際は **`--ignore-stub` を付けないと
`stub.mbt`(これから書く実装)が上書きされる**。

生成直後に 2 か所だけ手を入れる。

1 つ目: `ffi/moon.pkg.json` の `warn-list` に `-55` を足す。現行の moonc は
FFI 引数の `#borrow`/`#owned` 注釈を警告し、`moon build` がエラー扱いに
するため(生成器が追いつくまでの措置):

```json
{ "warn-list": "-44-55", "supported-targets": "wasm" }
```

2 つ目: `gen/world/plugin/moon.pkg.json` に、実装から使うパッケージの
import を足す。まずはログだけ:

```json
{
  "import": [
    { "path" : "edlr/plugin/world/plugin", "alias" : "plugin" },
    { "path" : "edlr/plugin/interface/edlr/plugin/hostLog", "alias" : "hostLog" }
  ]
}
```

### コード

`gen/world/plugin/stub.mbt` に 6 つの export の `...` が並んでいる。
これを実装で置き換える:

```moonbit
///|
fn log_info(message : String) -> Unit {
  @hostLog.log(@hostLog.Level::INFO, message)
}

///|
fn log_warn(message : String) -> Unit {
  @hostLog.log(@hostLog.Level::WARN, message)
}

///|
pub fn init_() -> Unit {
  log_info("tutorial-jump-log started")
}

///|
pub fn on_event(ev : @plugin.Event) -> Unit {
  guard (try? @json.parse(ev.payload_json)) is Ok(Object(obj)) else {
    log_warn("broken payload")
    return
  }
  guard obj.get("StarSystem") is Some(String(system)) else {
    log_warn("could not read StarSystem from the payload")
    return
  }
  let distance = match obj.get("JumpDist") {
    Some(Number(n, ..)) => n
    _ => 0
  }
  log_info("jumped to \{system} (\{distance} ly)")
}

///|
pub fn on_message(_driver : String, _topic : String, _payload : FixedArray[Byte]) -> Unit {

}

///|
pub fn on_schedule(_name : String) -> Unit {

}

///|
pub fn on_job_complete(_job_id : UInt64, _result_json : String) -> Unit {

}

///|
pub fn on_stop() -> Unit {

}
```

- **6 つとも実装すること**(使わないものは空でよい)。`...` が残っていると
  `declaration_unimplemented` 警告のまま、呼ばれた時に trap する
- `@json.parse` を使うので `gen/world/plugin/moon.pkg.json` の import に
  `"moonbitlang/core/json"` も足す(この章のうちだけ。3 章で JSON の処理は
  別パッケージへ出す)
- wit の型は素直に MoonBit へ落ちる: `option<string>` は `String?`、
  `list<u8>` は `FixedArray[Byte]`、record は `pub(all) struct`。
  `ev.name is Some("FSDJump")` のようにパターンで剥がせる
- JSON は enum `Json` へのパターンマッチで読む(`Object(obj)` /
  `String(s)` / `Number(n, ..)`)。`Number` の 2 つ目は表記保持用の
  フィールドなので `..` で無視する

`manifest.toml`:

```toml
id = "tutorial-jump-log-mbt"
name = "Jump Log (MoonBit tutorial)"
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

手順が 3 段あるので `build.sh` にしておく:

```bash
#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
wit="<edlr のパス>/core/wit"
out="${1:-$here/plugin.wasm}"

cd "$here"
moon build --target wasm --release

core_wasm="$here/_build/wasm/release/build/gen/gen.wasm"
embedded="$(mktemp)"
trap 'rm -f "$embedded"' EXIT
wasm-tools component embed "$wit" "$core_wasm" \
  -w plugin-guest --encoding utf16 -o "$embedded"
wasm-tools component new "$embedded" -o "$out"

echo "built: $out"
```

```
chmod +x build.sh && ./build.sh

mkdir -p /tmp/edlr-tutorial/plugins/tutorial-jump-log-mbt
cp plugin.wasm manifest.toml /tmp/edlr-tutorial/plugins/tutorial-jump-log-mbt/
```

できたものは自分で覗ける:

```
wasm-tools validate --features all plugin.wasm
wasm-tools component wit plugin.wasm | grep export
```

`init` / `on-event` / `on-message` / `on-schedule` / `on-job-complete` / `on-stop` の 6 つが
export されていれば正しい。import に `wasi:` が並んでいないのも見ておくと
よい(未使用の import はコンポーネント化で落ちる)。

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
INFO edlr_core::manifest: plugin manifest loaded id="tutorial-jump-log-mbt" events=1 settings=0 capabilities=0 ... schedules=0
INFO edlr_core::host::plugin: tutorial-jump-log started plugin_id="tutorial-jump-log-mbt"
INFO edlr_core::host::plugin: jumped to Sol (8.19 ly) plugin_id="tutorial-jump-log-mbt"
```

1 行目の `events=1` などは manifest の読み取り結果。**宣言したはずの項目が
0 になっていたら、そこが読めていない**(綴り違いなど)。

**`host-log` の `DEBUG` は既定では出ない。** デーモンのログレベルは既定
`info` で、stderr にも GUI にも `INFO` 以上しか出ない。`DEBUG` も
見たいなら `RUST_LOG=debug` を付けてデーモンを起動する(閾値は stderr と GUI で
共通なので GUI の Logs 画面にも出る)。詳しくは
[plugins.md の「ログレベル」](plugins.md#ログレベルhost-log)。
このチュートリアルでは `RUST_LOG` 無しで確認できるよう `INFO` を使う。

### うまくいかないとき

| 症状 | 原因 |
| --- | --- |
| ログや payload が文字化けする | `component embed` に `--encoding utf16` を付けていない、または wit-bindgen が 0.45 系でない(7 章) |
| `moon build` が FFI の警告で落ちる | `ffi/moon.pkg.json` の `warn-list` に `-55` を足していない |
| ビルドは通るが呼ばれた瞬間に trap する | `stub.mbt` の `...` を実装で置き換えていない |
| `manifest loaded` が出ない | ディレクトリ名と `id` が違う / `manifest.toml` が無い |
| `plugin` の起動ログが出ない | `entry` が指す wasm が無い、または component 化に失敗した wasm を置いている。デーモンのログに理由が出る |
| `jumped to` が出ない | `events` に `FSDJump` が入っていない / Journal ファイル名が `Journal.*.log` になっていない |
| 何を書き足しても反応しない | プラグインの入れ替えは**デーモンの再起動が要る**(ロードは起動時に一度だけ) |

Journal の読み取り位置は `--state-dir` に永続化される。**同じ行を二度は
配信しない**ので、確認をやり直すときは新しい行を書き足すこと(あるいは
`--state-dir` を消す)。デーモンが動き出す前に既に書かれていた行も届くが、
その場合は `ev.replay` が `true` になる。

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

`type` は `boolean` / `string` / `number` / `select` / `secret` / `map`。GUI の
Plugins ページはこの宣言だけを見てフォームを描く。値は
`<settings-dir>/<id>.json` に保存され、未保存のキーは `default` に落ちる。

### ロジックは stub の外に置く

`gen/` のパッケージはホスト関数の import(`wasmImport…`)に繋がっており、
wasm ランタイムの外では動かせない — つまり**そのままではテストが書けない**。
判断を持つコードは自分のパッケージへ出しておくと `moon test` で確かめられる。

```
mkdir jumplog
cat > jumplog/moon.pkg.json <<'EOF'
{
  "import": [
    "moonbitlang/core/json",
    "moonbitlang/core/double",
    "moonbitlang/core/encoding/utf8"
  ]
}
EOF
```

`jumplog/jumplog.mbt`:

```moonbit
///| host-settings.get-all() が返す JSON を解釈した設定値。
pub(all) struct Settings {
  enabled : Bool
  min_distance : Double
} derive(Show, Eq)

///| parse_settings は host-settings.get-all() の JSON を解釈する。
/// 壊れた JSON や未設定のキーは既定値へ倒す(プラグインを止めない)。
pub fn parse_settings(raw : String) -> Settings {
  let defaults = Settings::{ enabled: true, min_distance: 0 }
  guard (try? @json.parse(raw)) is Ok(Object(obj)) else { return defaults }
  let enabled = match obj.get("enabled") {
    Some(True) => true
    Some(False) => false
    _ => defaults.enabled
  }
  let min_distance = match obj.get("minDistance") {
    Some(Number(n, ..)) => n
    _ => defaults.min_distance
  }
  Settings::{ enabled, min_distance }
}

///| FSDJump イベントから読み取った値。
pub(all) struct Jump {
  system : String
  distance : Double
} derive(Show, Eq)

///| parse_jump は FSDJump の payload JSON を解釈する。
/// StarSystem が無い・空のときは None。
pub fn parse_jump(payload_json : String) -> Jump? {
  guard (try? @json.parse(payload_json)) is Ok(Object(obj)) else { return None }
  guard obj.get("StarSystem") is Some(String(system)) && system != "" else {
    return None
  }
  let distance = match obj.get("JumpDist") {
    Some(Number(n, ..)) => n
    _ => 0
  }
  Some(Jump::{ system, distance })
}

///| format_ly は跳躍距離を小数 2 桁で整形する(8.19 → "8.19"、3.0 → "3.00")。
pub fn format_ly(distance : Double) -> String {
  let cents = @double.round(distance * 100.0).to_int()
  let whole = cents / 100
  let frac = cents % 100
  let frac_str = if frac < 10 { "0" + frac.to_string() } else { frac.to_string() }
  "\{whole}.\{frac_str}"
}
```

テストは同じディレクトリの `jumplog/jumplog_test.mbt` に書く
(`_test.mbt` はブラックボックステストとして別パッケージ扱いになる):

```moonbit
///|
test "parse_settings: 既定値" {
  assert_eq(
    @jumplog.parse_settings("{}"),
    @jumplog.Settings::{ enabled: true, min_distance: 0 },
  )
}

///|
test "parse_jump: FSDJump の payload を読む" {
  assert_eq(
    @jumplog.parse_jump("{\"StarSystem\":\"Sol\",\"JumpDist\":8.19}"),
    Some(@jumplog.Jump::{ system: "Sol", distance: 8.19 }),
  )
}
```

```
moon test --target wasm    # テストを走らせる
moon check --target wasm   # 型チェックだけ
```

`--target wasm` を付けて、実際にプラグインが動くバックエンドの上で
テストする(既定のターゲットとバックエンドの挙動差を踏まないため)。

### stub 側

`gen/world/plugin/moon.pkg.json` の import に
`{ "path" : "edlr/plugin/interface/edlr/plugin/hostSettings", "alias" : "hostSettings" }` と
`{ "path" : "edlr/plugin/jumplog", "alias" : "jumplog" }` を足し
(`moonbitlang/core/json` はもう stub では使わないので外してよい)、
`on_event` を差し替える:

```moonbit
///|
pub fn on_event(ev : @plugin.Event) -> Unit {
  // manifest の events で絞ってはいるが、購読を増やしたときに壊れない
  // よう、ここでも名前を確かめる。
  guard ev.name is Some("FSDJump") else { return }
  let settings = @jumplog.parse_settings(@hostSettings.get_all())
  if not(settings.enabled) {
    return
  }
  guard @jumplog.parse_jump(ev.payload_json) is Some(jump) else {
    log_warn("could not read StarSystem from the payload")
    return
  }
  if jump.distance < settings.min_distance {
    log_info(
      "skipping \{jump.system} (\{@jumplog.format_ly(jump.distance)} ly < \{@jumplog.format_ly(settings.min_distance)} ly)",
    )
    return
  }
  log_info("jumped to \{jump.system} (\{@jumplog.format_ly(jump.distance)} ly)")
}
```

**`init` で読んで持ち回らないこと。** 設定変更は稼働中のプラグインへ即座に
反映される仕様で、それが効くのは毎回読み直しているからである。

### 確認する

ビルドして配置し直し、デーモンを**再起動**してから GUI の Plugins ページで
`minDistance` を `20` にする(GUI の動かし方は [ui.md](ui.md))。しきい値を
下回るジャンプを書き足すと:

```
INFO edlr_core::host::plugin: skipping Alioth (5.00 ly < 20.00 ly) plugin_id="tutorial-jump-log-mbt"
```

上回るジャンプなら従来どおり `jumped to ...` が出る。
`/tmp/edlr-tutorial/settings/tutorial-jump-log-mbt.json` に保存されているのも
確認できる。

### うまくいかないとき

| 症状 | 原因 |
| --- | --- |
| Plugins ページにフォームが出ない | `[[settings]]` がトップレベルキーより前に書かれていて、そのテーブルの子になっている。`manifest loaded` の `settings=` が 0 になっていないか見る |
| 値を変えても挙動が変わらない | `init` で読んで保持している / ビルドし直した wasm を配置していない |
| `jumplog` が見つからないとコンパイルエラーになる | `gen/world/plugin/moon.pkg.json` の import に足していない(パッケージのパスはモジュール名 `edlr/plugin` から始まる) |

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

`gen/world/plugin/moon.pkg.json` の import に
`{ "path" : "edlr/plugin/interface/edlr/plugin/driverHttp", "alias" : "driverHttp" }`
を足し、`stub.mbt` に問い合わせを書く:

```moonbit
///|
const EDSM = "https://www.edsm.net/api-v1/system"

///| lookup は跳んだ先の星系を EDSM に問い合わせ、結果をログに出す。
fn lookup(jump : @jumplog.Jump) -> Unit {
  let result = @driverHttp.send(@driverHttp.Request::{
    method_: "GET",
    url: @jumplog.edsm_url(EDSM, jump.system),
    headers: [("accept", "application/json")],
    body: None,
  })
  match result {
    Err(e) => log_warn("\{jump.system}: lookup failed: " + driver_error_message(e))
    Ok(resp) => {
      if resp.status != 200 {
        log_warn("\{jump.system}: EDSM returned HTTP \{resp.status}")
        return
      }
      let body = @jumplog.from_utf8(resp.body)
      // 未知の星系では EDSM は空配列 `[]` を返す。
      if body == "[]" {
        log_info("\{jump.system}: not known to EDSM")
      } else {
        log_info("\{jump.system}: EDSM says " + body)
      }
    }
  }
}

///| driver_error_message は variant の中身(理由文字列)を取り出す。
fn driver_error_message(err : @driverHttp.DriverError) -> String {
  match err {
    PermissionDenied(reason) => reason
    InvalidRequest(reason) => reason
    Transport(reason) => reason
  }
}
```

wit の型の MoonBit 側での見え方:

- `result<response, driver-error>` はそのまま `Result[Response, DriverError]`。
  `match` で `Ok` / `Err` を剥がす
- variant(`driver-error` など)は `enum` になり、case ごとに
  パターンマッチできる(Go のようなポインタの nil 判定は要らない)
- record のフィールド名 `method` は MoonBit の予約語を避けて `method_` になる

レスポンスの body は `list<u8>`(= `FixedArray[Byte]`)で届く。文字列との
変換は UTF-8 で行うのが慣例なので、`jumplog` 側にヘルパーを置く
(`moonbitlang/core/encoding/utf8` を使う)。

`jumplog/text.mbt`:

```moonbit
///| to_utf8 は文字列を UTF-8 のバイト列にする。
pub fn to_utf8(s : String) -> FixedArray[Byte] {
  @utf8.encode(s).to_fixedarray()
}

///| from_utf8 は UTF-8 のバイト列を文字列へ戻す。壊れた列は置換文字にする。
pub fn from_utf8(bytes : FixedArray[Byte]) -> String {
  @utf8.decode_lossy(Bytes::from_iter(bytes.iter())[:])
}
```

星系名にはスペースが入る(`Col 285 Sector AA-A a1`)ので、URL に載せる前に
パーセントエンコードが要る。これも `jumplog` 側に置いてテストできるように
しておく。

`jumplog/url.mbt`:

```moonbit
///| edsm_url は EDSM の system API へ問い合わせる URL を組み立てる。
pub fn edsm_url(endpoint : String, system : String) -> String {
  endpoint + "?systemName=" + url_encode(system) + "&showId=1"
}

///| url_encode は文字列を UTF-8 のパーセントエンコーディングにする。
pub fn url_encode(s : String) -> String {
  let out = StringBuilder::new()
  for b in @utf8.encode(s) {
    let c = b.to_int()
    if (c >= 'A'.to_int() && c <= 'Z'.to_int()) ||
      (c >= 'a'.to_int() && c <= 'z'.to_int()) ||
      (c >= '0'.to_int() && c <= '9'.to_int()) ||
      c == '-'.to_int() ||
      c == '_'.to_int() ||
      c == '.'.to_int() ||
      c == '~'.to_int() {
      out.write_char(c.unsafe_to_char())
    } else {
      out.write_char('%')
      out.write_char(hex_digit(c / 16))
      out.write_char(hex_digit(c % 16))
    }
  }
  out.to_string()
}

///|
fn hex_digit(n : Int) -> Char {
  if n < 10 {
    ('0'.to_int() + n).unsafe_to_char()
  } else {
    ('A'.to_int() + n - 10).unsafe_to_char()
  }
}
```

`onEvent` の最後で `lookup(jump)` を呼べば動く。**ただしこれは 5 章で
やめる**(理由もそこで説明する)。

### 確認する

まず**承認しないまま**動かしてみる。ジャンプを書き足すと:

```
WARN edlr_core::host::plugin: Sol: lookup failed: capability not granted plugin_id="tutorial-jump-log-mbt"
```

未承認でもプラグインは止まらず、`driver-http.send` だけが拒否される。
GUI の Plugins ページで宣言した `hosts` と `reason` を確認して承認すると、
再起動しなくても次の呼び出しから通る:

```
INFO edlr_core::host::plugin: Sol: EDSM says {"name":"Sol","id":27,"id64":10477373803} plugin_id="tutorial-jump-log-mbt"
```

### 制約(踏む前に知っておくもの)

- **タイムアウトは 1.5 秒**、プラグインからは変えられない
- **リダイレクトを追わない**。3xx はそのまま返る
- `Host` / `Content-Length` / `Transfer-Encoding` / `Connection` などの
  ヘッダは設定できない(`InvalidRequest` になる)
- リクエスト・レスポンスとも本文は 8 MiB まで
- `send` は同期呼び出しで、**返るまでこのプラグインは次のイベントを読まない**

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
   1.5 秒を使い切ると、残りの処理を足しただけで期限を超えうる
2. **`send` の間、このプラグインはイベントを読まない**。ホスト側の作業キューは
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

キューも `jumplog` 側に置けばテストできる。`jumplog/queue.mbt`:

```moonbit
///| EDSM への問い合わせ待ちのジャンプを溜める FIFO。
/// 上限を超えたら古いものから捨てる。
pub(all) struct Queue {
  capacity : Int
  items : Array[Jump]
}

///|
pub fn Queue::new(capacity : Int) -> Queue {
  Queue::{ capacity, items: [] }
}

///| push は末尾へ積む。上限を超えたら先頭(最も古いもの)を捨てる。
pub fn Queue::push(self : Queue, jump : Jump) -> Unit {
  if self.items.length() >= self.capacity {
    let _ = self.items.remove(0)

  }
  self.items.push(jump)
}

///|
pub fn Queue::pop(self : Queue) -> Jump? {
  if self.items.length() == 0 {
    None
  } else {
    Some(self.items.remove(0))
  }
}

///|
pub fn Queue::length(self : Queue) -> Int {
  self.items.length()
}

///| summary は溜まっているジャンプを 1 行にまとめる(on-stop の報告用)。
pub fn Queue::summary(self : Queue) -> String {
  let parts = self.items.map(fn(j) {
    "\{j.system} (\{format_ly(j.distance)} ly)"
  })
  parts.join(", ")
}
```

`stub.mbt` 側。トップレベルの値は wasm インスタンスが生きている間
(= プラグインが作り直されるまで)保持されるので、呼び出しをまたぐ状態は
ここに置ける:

```moonbit
///| EDSM への問い合わせ待ち。
let pending : @jumplog.Queue = @jumplog.Queue::new(50)
```

`on_event` の最後は `lookup(jump)` をやめて `pending.push(jump)` に差し替え、
問い合わせは `on_schedule` へ移す:

```moonbit
///|
pub fn on_schedule(name : String) -> Unit {
  // 宣言したスケジュールが 1 つでも、名前は確かめておく
  // (増やしたときにここで分岐することになる)。
  guard name == "flush" else {
    log_warn("unknown schedule: " + name)
    return
  }
  match pending.pop() {
    // 呼び出し期限 2 秒 / HTTP タイムアウト 1.5 秒。
    // 1 回の呼び出しで叩けるのは実質 1 回だけ。
    Some(jump) => lookup(jump)
    None => ()
  }
  if pending.length() > 0 {
    log_info("\{pending.length()} jump(s) still queued")
  }
}

///| デーモンの graceful shutdown で一度だけ呼ばれる。
pub fn on_stop() -> Unit {
  if pending.length() == 0 {
    return
  }
  log_info("stopping with \{pending.length()} unflushed: " + pending.summary())
}
```

**`on_stop` で HTTP を叩かないのは意図的。** 猶予は既定 5 秒しかなく、
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
INFO edlr_core::host::plugin: jumped to Sol (28.19 ly)
INFO edlr_core::host::plugin: jumped to Achenar (33.20 ly)
INFO edlr: received SIGINT, shutting down
INFO edlr_core::host::plugin: stopping with 2 unflushed: Sol (28.19 ly), Achenar (33.20 ly)
```

(flush が間に合った分は先に `EDSM says ...` が出る)

### うまくいかないとき

| 症状 | 原因 |
| --- | --- |
| `on-schedule` が呼ばれない | `[[schedule]]` が別テーブルの子になっている(`manifest loaded` の `schedules=` を見る)/ `interval-seconds` と `cron` を両方書いた(ロードに失敗する) |
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

別のプロジェクトとして作り、**バインディングは `--world driver` で生成する**:

```
mkdir tutorial-tracker-mbt && cd tutorial-tracker-mbt

wit-bindgen moonbit <edlr のパス>/core/wit --world driver \
  --derive-show --derive-eq --out-dir .
```

2 章と同じ 2 点(`ffi/moon.pkg.json` の `warn-list`、import の追記)を
直す。実装を書く場所は `gen/world/driver/stub.mbt` で、export は
`init_` と `on_message` の 2 つだけ。import は:

```json
{
  "import": [
    { "path" : "edlr/plugin/interface/edlr/plugin/hostLog", "alias" : "hostLog" },
    { "path" : "edlr/plugin/interface/edlr/plugin/busHost", "alias" : "busHost" },
    "moonbitlang/core/encoding/utf8"
  ]
}
```

`gen/world/driver/stub.mbt`:

```moonbit
///| ドライバは常駐インスタンスが 1 つだけなので、トップレベルの
/// 可変状態がそのまま「全プラグイン共有の集計値」になる。
struct State {
  mut count : Int
}

///|
let state : State = State::{ count: 0 }

///|
fn log_info(message : String) -> Unit {
  @hostLog.log(@hostLog.Level::INFO, message)
}

///|
fn log_warn(message : String) -> Unit {
  @hostLog.log(@hostLog.Level::WARN, message)
}

///|
pub fn init_() -> Unit {
  log_info("tutorial-tracker started")
}

///|
pub fn on_message(from : String, topic : String, payload : FixedArray[Byte]) -> Unit {
  guard topic == "visit" else { return }
  let system = @utf8.decode_lossy(Bytes::from_iter(payload.iter())[:])
  state.count = state.count + 1
  log_info("visit #\{state.count} from \{from}: \{system}")
  let body : Json = Json::object({
    "system": Json::string(system),
    "count": Json::number(state.count.to_double()),
  })
  match @busHost.emit("last-system", @utf8.encode(body.stringify()).to_fixedarray()) {
    Ok(_) => ()
    Err(e) => log_warn("emit failed: \{e}")
  }
}
```

`@busHost.emit` で自分のトピックへ値を流す。ビルドは 2 章の `build.sh` の
`-w plugin-guest` を **`driver-guest`** に、出力を `driver.wasm` に変えるだけ
(生成は `driver`、コンポーネント化は `driver-guest`。2 章と同じ非対称)。

`driver.toml`:

```toml
id = "tutorial-tracker-mbt"
name = "Jump Tracker (MoonBit tutorial)"
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
プラグインが `bus.get` でいつでも読める。`retain = false` は配信専用で、
`bus.get` は常に値なしを返す。

### プラグイン側

manifest に接続先を宣言する:

```toml
[[bus]]
driver = "tutorial-tracker-mbt"
publish = ["visit"]
subscribe = ["last-system"]
reason = "訪問した星系を tracker ドライバへ渡し、集計された最新値を受け取るため"
```

`publish` / `subscribe` に書いていないトピックは、承認済みでも
`PermissionDenied` になる。同じドライバを 2 回宣言することはできない。

`gen/world/plugin/moon.pkg.json` の import に
`{ "path" : "edlr/plugin/interface/edlr/plugin/bus", "alias" : "bus" }` を足す。

送る側(`on_event` の中、`pending.push(jump)` の前):

```moonbit
///|
const TRACKER = "tutorial-tracker-mbt"

  match @bus.publish(TRACKER, "visit", @jumplog.to_utf8(jump.system)) {
    Ok(_) => ()
    Err(e) => log_warn("publish failed: \{e}")
  }
```

受け取る側 — 購読しているトピックへ値が流れると `on_message` が呼ばれる:

```moonbit
///|
pub fn on_message(driver : String, topic : String, payload : FixedArray[Byte]) -> Unit {
  log_info("\{driver}/\{topic} = " + @jumplog.from_utf8(payload))
}
```

retain された値は、配信を待たずいつでも読める(`on_schedule` の中など):

```moonbit
  match @bus.get(TRACKER, "last-system") {
    Ok(Some(v)) => log_info("tracker says: " + @jumplog.from_utf8(v))
    Ok(None) => ()
    Err(e) => log_warn("get failed: \{e}")
  }
```

`bus.get` は `result<option<list<u8>>, bus-error>` なので、**エラーの有無と
値の有無を 2 段階で見る**ことになる(`Ok(None)` は「retain 値がまだ無い」)。
MoonBit ではネストしたパターン 1 発で書ける。

### ビルドして配置

```
./build.sh   # driver-guest 版

mkdir -p /tmp/edlr-tutorial/drivers/tutorial-tracker-mbt
cp driver.wasm driver.toml /tmp/edlr-tutorial/drivers/tutorial-tracker-mbt/
```

### 確認する

デーモンを再起動し、GUI の Plugins ページで bus 接続を承認してから
ジャンプを書き足すと、次のように流れる:

```
INFO edlr_core::host::driver: tutorial-tracker started driver_id="tutorial-tracker-mbt"
INFO edlr_core::host::plugin: jumped to Shinrarta Dezhra (42.30 ly)
INFO edlr_core::host::driver: visit #1 from tutorial-jump-log-mbt: Shinrarta Dezhra driver_id="tutorial-tracker-mbt"
INFO edlr_core::host::plugin: tutorial-tracker-mbt/last-system = {"system":"Shinrarta Dezhra","count":1}
INFO edlr_core::host::plugin: tracker says: {"system":"Shinrarta Dezhra","count":1}
```

上から、ドライバの起動 → プラグインがジャンプを見た → ドライバが受け取った
→ 配り直された値がプラグインへ届いた → 次の定期実行で retain 値を読んだ、
という流れになっている。

### うまくいかないとき

| 症状 | 原因 |
| --- | --- |
| `publish failed: PermissionDenied("bus access to ... is not granted")` | 未承認。GUI の Plugins ページで承認する |
| `unknown driver` | `[[bus]]` の `driver` と `driver.toml` の `id` が違う / ドライバが `drivers-dir` に無い |
| `unknown topic` | `[[topics]]` に無いトピック、または `publish`/`subscribe` に書いていないトピック |
| `on_message` が呼ばれない | `subscribe` に入れていない / ドライバが `emit` していない(ドライバ側のログを見る) |
| `bus.get` が常に `Ok(None)` | そのトピックが `retain = false` |
| ドライバのログが出ない | `--drivers-dir` を渡し忘れている(存在しなくてもエラーにならず、ドライバ 0 件で起動する) |

承認の失効はプラグインの capability と同じで、`publish` / `subscribe` の集合を
変えると以前の承認は無効になる。

## 7. うまくいかないとき

### プラグインが `Disabled` になった

GUI の Plugins ページに理由が出る。原因は 2 つに分かれる:

- **トラップ**(`panic()` / `abort()`、配列の範囲外アクセスなど)— 次に呼んでも
  同じ結果になる決定的な故障なので、**1 回で** `Disabled`
- **呼び出し期限(2 秒)の超過** — 応答しないホストなどプラグインの責任とは
  限らないので、`init` からやり直して処理を続ける。**3 回連続**で超過して
  初めて `Disabled`

作り直しでは wasm の線形メモリ上の状態(この章までに作ったキューなど)が
失われる。長時間の作業を 1 回の呼び出しに詰め込まないこと。

### ロード時に world が合わないと言われる

`core/wit` を更新したのに古いバインディングでビルドした wasm を置いている。
WIT パッケージは `edlr:plugin@0.4.0` で、**旧 world のプラグインは新しい
ホストへロードできない**。バインディングを再生成する:

```
wit-bindgen moonbit <edlr のパス>/core/wit --world plugin \
  --derive-show --derive-eq --ignore-stub --out-dir .
```

(ドライバなら `--world driver`)

**`--ignore-stub` を忘れると `stub.mbt` が雛形で上書きされる。**
また `ffi/moon.pkg.json` の `warn-list` 修正と `moon.pkg.json` への import
追記も再生成で消えるので、`git diff` を見て戻すこと。

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

### MoonBit 固有

| 症状 | 原因 |
| --- | --- |
| ログ・payload・HTTP の中身が全て文字化けする | `component embed` の `--encoding utf16` 忘れ。**または wit-bindgen が 0.60 系**(モジュールポインタと文字列データのオフセット計算が現行 moonc のオブジェクトレイアウトと合わず、先頭 8 バイトずれる)。0.45 系を使う |
| `moon build` が `unannotated_ffi` で落ちる | 生成された `ffi/moon.pkg.json` の `warn-list` に `-55` を足す(2 章) |
| ビルドは通るのに export を呼ぶと trap する | `stub.mbt` の `...`(雛形)が残っている。再生成で上書きされたときも同じ症状になる |
| `@jumplog` などが unresolved になる | そのパッケージの `moon.pkg.json` の import に足していない。パッケージパスはモジュール名 `edlr/plugin` から始まる |
| `Duplicate alias \`plugin\`` の WARN が出る | 生成物の既知の警告。無害なので気にしなくてよい |
| ネイティブと wasm でテスト結果が違う | `moon test --target wasm` で、実際に動くバックエンドの上でテストする(3 章) |

## 8. 次に読むもの

- [plugins.md](plugins.md) — manifest の全フィールド、設定 RPC、無効化の条件
- [capabilities.md](capabilities.md) — HTTP に加えて、サイドカープロセス
  (`driver-process`)とファイルアクセス(`driver-fs`)
- [drivers.md](drivers.md) — ドライバの詳細と承認フロー
- [ui.md](ui.md) — GUI の起動方法。`[[dashboard]]` で自前のウィジェットを
  出すこともできる
- [cli.md](cli.md) — デーモンのフラグ、読み取り位置の永続化、`replay`

サンプル:

- `examples/plugins/tutorial-jump-log-mbt` / `examples/drivers/tutorial-tracker-mbt`
  — このチュートリアルの完成形
- `examples/plugins/inara-uploader` — TinyGo 製の実用寄りのプラグイン
  (`secret` 設定、バッチ送信、`on-schedule` での flush)
- `examples/plugins/state-reader` — Rust だが、bus とダッシュボード
  ウィジェットの最小例

`examples` にあるものは `./scripts/install-examples.sh <名前>` でビルドと配置を
まとめて行える。
