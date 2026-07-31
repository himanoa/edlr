# プラグインを書く(Rust)

edlr のプラグインを、何も無いところから 1 つ育てていく。最後まで進むと、
FSDJump を拾って設定でふるいに掛け、外部 API を叩き、定期実行で後処理をし、
自分で書いたドライバと会話するプラグインができる。

TinyGo で書きたい場合は [plugin-tutorial-tinygo.md](plugin-tutorial-tinygo.md)、
MoonBit なら [plugin-tutorial-moonbit.md](plugin-tutorial-moonbit.md) を参照
(内容は同じで、コードだけが違う)。書き上げた後に仕様を引くための
リファレンスは [plugins.md](plugins.md) にある。

完成形は `examples/plugins/tutorial-jump-log-rs`(プラグイン)と
`examples/drivers/tutorial-tracker-rs`(6 章で作るドライバ)にある。詰まったら
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

- Rust(stable)と wasm のターゲット

      rustup target add wasm32-wasip2

- edlr のソース。プラグインは `core/wit` の WIT 定義に対してビルドするので、
  リポジトリを手元に置いておく

      git clone <edlr のリポジトリ> && cd edlr

- (任意)`wasm-tools` — ビルドした wasm を自分で覗きたいとき

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

**ビルド時に対象にする world は `plugin-guest`**。`plugin` に WASI の import
一式を足したもので、ゲスト側は言語を問わずこちらを使えばよい。

## 2. FSDJump をログに出す

**この章でできること**: プラグインがロードされ、ジャンプするたびに
ログが出るようになる。

### コード

    cargo new --lib tutorial-jump-log-rs
    cd tutorial-jump-log-rs

`Cargo.toml`:

```toml
[workspace]

[package]
name = "tutorial-jump-log"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.60.0"
serde_json = "1"

[profile.release]
opt-level = "s"
```

- `crate-type = ["cdylib"]` が要る。普通の rlib では wasm にならない
- 先頭の `[workspace]` は、edlr のリポジトリの中に置いても親のワークスペースに
  巻き込まれないようにするためのもの。外に置くなら要らない

`src/lib.rs`:

```rust
wit_bindgen::generate!({
    path: "../../../core/wit",   // edlr の core/wit への相対パス
    world: "plugin-guest",
    generate_all,
});

use edlr::plugin::host_log;

struct Component;

impl Guest for Component {
    fn init() {
        host_log::log(host_log::Level::Info, "tutorial-jump-log started");
    }

    fn on_event(ev: Event) {
        let payload: serde_json::Value =
            serde_json::from_str(&ev.payload_json).unwrap_or(serde_json::Value::Null);
        let system = payload["StarSystem"].as_str().unwrap_or("");
        let distance = payload["JumpDist"].as_f64().unwrap_or(0.0);
        host_log::log(
            host_log::Level::Info,
            &format!("jumped to {system} ({distance:.2} ly)"),
        );
    }

    fn on_message(_driver: String, _topic: String, _payload: Vec<u8>) {}
    fn on_schedule(_name: String) {}
    fn on_stop() {}
}

export!(Component);
```

`generate!` が `Guest` トレイトと `Event` 型を生成し、`export!` がそれを
ホストから見える export として繋ぐ。**5 つの関数は全部実装しないとコンパイル
できない**(使わないものは空でよい)。

`path:` はディレクトリを指しているので、`core/wit` を更新したら次のビルドで
自動的に追随する。生成物をコミットする必要はない。

`manifest.toml`:

```toml
id = "tutorial-jump-log-rs"
name = "Jump Log (Rust tutorial)"
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

```
cargo build --release --target wasm32-wasip2

mkdir -p /tmp/edlr-tutorial/plugins/tutorial-jump-log-rs
cp target/wasm32-wasip2/release/tutorial_jump_log.wasm \
   /tmp/edlr-tutorial/plugins/tutorial-jump-log-rs/plugin.wasm
cp manifest.toml /tmp/edlr-tutorial/plugins/tutorial-jump-log-rs/
```

wasm のファイル名はクレート名からハイフンをアンダースコアに変えたもの
(`tutorial-jump-log` → `tutorial_jump_log.wasm`)。配置後の名前は
`manifest.toml` の `entry` と合っていれば何でもよい。

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
INFO edlr_core::plugin::manifest: plugin manifest loaded id="tutorial-jump-log-rs" events=1 settings=0 capabilities=0 ... schedules=0
INFO edlr_core::plugin::host: tutorial-jump-log started plugin_id="tutorial-jump-log-rs"
INFO edlr_core::plugin::host: jumped to Sol (8.19 ly) plugin_id="tutorial-jump-log-rs"
```

1 行目の `events=1` などは manifest の読み取り結果。**宣言したはずの項目が
0 になっていたら、そこが読めていない**(綴り違いなど)。

**`host-log` の `Debug` は既定では出ない。** デーモンのログレベルは既定 `info`
で、stderr にも GUI にも `Info` 以上しか出ない。`Debug` も見たいなら
`RUST_LOG=debug` を付けてデーモンを起動する(閾値は stderr と GUI で共通なので
GUI の Logs 画面にも出る)。詳しくは
[plugins.md の「ログレベル」](plugins.md#ログレベルhost-log)。
このチュートリアルでは `RUST_LOG` 無しで確認できるよう `Info` を使う。

### うまくいかないとき

| 症状 | 原因 |
| --- | --- |
| `manifest loaded` が出ない | ディレクトリ名と `id` が違う / `manifest.toml` が無い |
| `plugin` の起動ログが出ない | `entry` が指す wasm が無い、または world 違いでロードに失敗している。デーモンのログに失敗の理由が出る |
| `jumped to` が出ない | `events` に `FSDJump` が入っていない / Journal ファイル名が `Journal.*.log` になっていない / 書き足す前にデーモンが読み終えている(後述) |
| 何を書き足しても反応しない | プラグインの入れ替えは**デーモンの再起動が要る**(ロードは起動時に一度だけ) |

Journal の読み取り位置は `--state-dir` に永続化される。**同じ行を二度は
配信しない**ので、確認をやり直すときは新しい行を書き足すこと(あるいは
`--state-dir` を消す)。デーモンが動き出す前に既に書かれていた行も届くが、
その場合は `Event` の `replay` が `true` になる。

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

### コード

```rust
use edlr::plugin::{host_log, host_settings};

struct Settings {
    enabled: bool,
    min_distance: f64,
}

/// 設定は毎回読み直す。GUI で変えた値が次のイベントから効くのはこのため。
fn settings() -> Settings {
    let raw = host_settings::get_all();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    Settings {
        enabled: v["enabled"].as_bool().unwrap_or(true),
        min_distance: v["minDistance"].as_f64().unwrap_or(0.0),
    }
}
```

`get_all()` は defaults をマージ済みの JSON オブジェクト文字列を返す。
`on_event` の頭で使う:

```rust
    fn on_event(ev: Event) {
        // manifest の events で絞ってはいるが、購読を増やしたときに
        // 壊れないよう、ここでも名前を確かめる。
        if ev.name.as_deref() != Some("FSDJump") {
            return;
        }
        let settings = settings();
        if !settings.enabled {
            return;
        }
        // ... payload の取り出しは 2 章と同じ ...
        if distance < settings.min_distance {
            host_log::log(
                host_log::Level::Info,
                &format!("skipping {system} ({distance:.2} ly < {:.2} ly)",
                         settings.min_distance),
            );
            return;
        }
        host_log::log(host_log::Level::Info,
                      &format!("jumped to {system} ({distance:.2} ly)"));
    }
```

**`init` で読んで持ち回らないこと。** 設定変更は稼働中のプラグインへ即座に
反映される仕様で、それが効くのは毎回読み直しているからである。

### 確認する

ビルドして配置し直し、デーモンを**再起動**してから GUI の Plugins ページで
`minDistance` を `20` にする(GUI の動かし方は [ui.md](ui.md))。しきい値を
下回るジャンプを書き足すと:

```
INFO edlr_core::plugin::host: skipping Alioth (5.00 ly < 20.00 ly) plugin_id="tutorial-jump-log-rs"
```

上回るジャンプなら従来どおり `jumped to ...` が出る。
`/tmp/edlr-tutorial/settings/tutorial-jump-log-rs.json` に保存されているのも
確認できる。

### うまくいかないとき

| 症状 | 原因 |
| --- | --- |
| Plugins ページにフォームが出ない | `[[settings]]` がトップレベルキーより前に書かれていて、そのテーブルの子になっている。`manifest loaded` の `settings=` が 0 になっていないか見る |
| 値を変えても挙動が変わらない | `init` で読んで保持している / ビルドし直した wasm を配置していない |
| `unknown key` で保存に失敗する | GUI から送る `key` は manifest の `[[settings]]` にあるものだけ。宣言を消したキーは弾かれる |

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

```rust
use edlr::plugin::driver_http;

const EDSM: &str = "https://www.edsm.net/api-v1/system";

fn lookup(system: &str) {
    let req = driver_http::Request {
        method: "GET".to_string(),
        url: format!("{EDSM}?systemName={}&showId=1", urlencode(system)),
        headers: vec![("accept".to_string(), "application/json".to_string())],
        body: None,
    };

    match driver_http::send(&req) {
        Ok(resp) if resp.status == 200 => {
            let body = String::from_utf8_lossy(&resp.body);
            // 未知の星系では EDSM は空配列 `[]` を返す。
            if body.trim() == "[]" {
                host_log::log(host_log::Level::Info, &format!("{system}: not known to EDSM"));
            } else {
                host_log::log(host_log::Level::Info,
                              &format!("{system}: EDSM says {}", body.trim()));
            }
        }
        Ok(resp) => host_log::log(host_log::Level::Warn,
                                  &format!("{system}: EDSM returned HTTP {}", resp.status)),
        Err(e) => host_log::log(host_log::Level::Warn,
                                &format!("{system}: lookup failed: {e:?}")),
    }
}
```

星系名にはスペースが入る(`Col 285 Sector AA-A a1`)ので、URL に載せる前に
パーセントエンコードが要る。依存を増やしたくなければ手で書ける:

```rust
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' =>
                out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
```

`on_event` の最後で `lookup(&system)` を呼べば動く。**ただしこれは 5 章で
やめる**(理由もそこで説明する)。

### 確認する

まず**承認しないまま**動かしてみる。ジャンプを書き足すと:

```
WARN edlr_core::plugin::host: Sol: lookup failed: DriverError::PermissionDenied("capability not granted") plugin_id="tutorial-jump-log-rs"
```

未承認でもプラグインは止まらず、`driver-http.send` だけが拒否される。
GUI の Plugins ページで宣言した `hosts` と `reason` を確認して承認すると、
再起動しなくても次の呼び出しから通る:

```
INFO edlr_core::plugin::host: Sol: EDSM says {"name":"Sol","id":27,"id64":10477373803} plugin_id="tutorial-jump-log-rs"
```

### 制約(踏む前に知っておくもの)

- **タイムアウトは 1.5 秒**、プラグインからは変えられない
- **リダイレクトを追わない**。3xx はそのまま返る
- `Host` / `Content-Length` / `Transfer-Encoding` / `Connection` などの
  ヘッダは設定できない(`invalid-request` になる)
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
[plugins.md](plugins.md#秘密情報type--secret)。

## 5. 定期実行と終了フック

**この章でできること**: HTTP をイベント処理から追い出し、定期実行の側で
少しずつ流すようになる。停止時には流し残しを報告する。

### なぜイベント処理から追い出すのか

4 章の書き方には 2 つ問題がある。

1. **`on-event` の呼び出し全体に 2 秒の期限がある**。`driver-http.send` の
   1.5 秒を使い切ると、JSON の処理を足しただけで期限を超える
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

キューを持つ。wasm は 1 スレッドなので `thread_local!` + `RefCell` で足りる:

```rust
use std::cell::RefCell;

const QUEUE_CAP: usize = 50;

struct Jump {
    system: String,
    distance: f64,
}

thread_local! {
    static PENDING: RefCell<Vec<Jump>> = const { RefCell::new(Vec::new()) };
}
```

`on_event` の末尾から `lookup(...)` を外し、積むだけにする(上限を超えたら
古いものから捨てる):

```rust
        PENDING.with(|q| {
            let mut q = q.borrow_mut();
            if q.len() >= QUEUE_CAP {
                q.remove(0);
            }
            q.push(Jump { system, distance });
        });
```

`on_schedule` で 1 件だけ流す:

```rust
    fn on_schedule(name: String) {
        // 宣言したスケジュールが 1 つでも、名前は確かめておく
        // (増やしたときにここで分岐することになる)。
        if name != "flush" {
            host_log::log(host_log::Level::Warn, &format!("unknown schedule: {name}"));
            return;
        }

        let Some(jump) = PENDING.with(|q| {
            let mut q = q.borrow_mut();
            if q.is_empty() { None } else { Some(q.remove(0)) }
        }) else {
            return;
        };

        // 呼び出し期限 2 秒 / HTTP タイムアウト 1.5 秒。
        // 1 回の呼び出しで叩けるのは実質 1 回だけ。
        lookup(&jump);

        let remaining = PENDING.with(|q| q.borrow().len());
        if remaining > 0 {
            host_log::log(host_log::Level::Info,
                          &format!("{remaining} jump(s) still queued"));
        }
    }
```

`on_stop` は最後の後始末に使う:

```rust
    fn on_stop() {
        let pending = PENDING.with(|q| {
            q.borrow().iter()
                .map(|j| format!("{} ({:.2} ly)", j.system, j.distance))
                .collect::<Vec<_>>()
        });
        if pending.is_empty() {
            return;
        }
        host_log::log(
            host_log::Level::Info,
            &format!("stopping with {} unflushed: {}", pending.len(), pending.join(", ")),
        );
    }
```

**`on_stop` で HTTP を叩かないのは意図的。** 猶予は既定 5 秒しかなく、
応答しないホストを掴むとそこへ辿り着けないまま終わる。ここでやるべきなのは
「速く終わる後始末」だけ。

`on_stop` について確かなこと:

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

`Cargo.toml` はプラグインとほぼ同じ(`serde_json` は要らない)。
`src/lib.rs`:

```rust
wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "driver-guest",       // プラグインとは違う world
    generate_all,
});

use std::cell::RefCell;
use edlr::plugin::{bus_host, host_log};

thread_local! {
    static COUNT: RefCell<u32> = const { RefCell::new(0) };
}

struct Component;

impl Guest for Component {
    fn init() {
        host_log::log(host_log::Level::Info, "tutorial-tracker started");
    }

    fn on_message(from: String, topic: String, payload: Vec<u8>) {
        if topic != "visit" {
            return;
        }
        let system = String::from_utf8_lossy(&payload).to_string();
        let count = COUNT.with(|c| { let mut c = c.borrow_mut(); *c += 1; *c });
        host_log::log(host_log::Level::Info,
                      &format!("visit #{count} from {from}: {system}"));

        let json = format!("{{\"system\":\"{}\",\"count\":{count}}}",
                           system.replace('\\', "\\\\").replace('"', "\\\""));
        if let Err(e) = bus_host::emit("last-system", json.as_bytes()) {
            host_log::log(host_log::Level::Warn, &format!("emit failed: {e:?}"));
        }
    }
}

export!(Component);
```

export は `init` と `on_message` の 2 つだけ。`bus_host::emit` で自分の
トピックへ値を流す。

`driver.toml`:

```toml
id = "tutorial-tracker-rs"
name = "Jump Tracker (Rust tutorial)"
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
`bus.get` は常に `none` を返す。

### プラグイン側

manifest に接続先を宣言する:

```toml
[[bus]]
driver = "tutorial-tracker-rs"
publish = ["visit"]
subscribe = ["last-system"]
reason = "訪問した星系を tracker ドライバへ渡し、集計された最新値を受け取るため"
```

`publish` / `subscribe` に書いていないトピックは、承認済みでも
`permission-denied` になる。同じドライバを 2 回宣言することはできない。

送る側(`on_event` の中):

```rust
use edlr::plugin::bus;

const TRACKER: &str = "tutorial-tracker-rs";

        if let Err(e) = bus::publish(TRACKER, "visit", system.as_bytes()) {
            host_log::log(host_log::Level::Warn, &format!("publish failed: {e:?}"));
        }
```

受け取る側 — 購読しているトピックへ値が流れると `on_message` が呼ばれる:

```rust
    fn on_message(driver: String, topic: String, payload: Vec<u8>) {
        host_log::log(
            host_log::Level::Info,
            &format!("{driver}/{topic} = {}", String::from_utf8_lossy(&payload)),
        );
    }
```

retain された値は、配信を待たずいつでも読める(`on_schedule` の中など):

```rust
        match bus::get(TRACKER, "last-system") {
            Ok(Some(v)) => host_log::log(
                host_log::Level::Info,
                &format!("tracker says: {}", String::from_utf8_lossy(&v)),
            ),
            Ok(None) => {}
            Err(e) => host_log::log(host_log::Level::Warn, &format!("get failed: {e:?}")),
        }
```

### ビルドして配置

```
cargo build --release --target wasm32-wasip2

mkdir -p /tmp/edlr-tutorial/drivers/tutorial-tracker-rs
cp target/wasm32-wasip2/release/tutorial_tracker.wasm \
   /tmp/edlr-tutorial/drivers/tutorial-tracker-rs/driver.wasm
cp driver.toml /tmp/edlr-tutorial/drivers/tutorial-tracker-rs/
```

### 確認する

デーモンを再起動し、GUI の Plugins ページで bus 接続を承認してから
ジャンプを書き足すと、1 回のジャンプで 4 行出る:

```
INFO edlr_core::driver::host: tutorial-tracker started driver_id="tutorial-tracker-rs"
INFO edlr_core::plugin::host: jumped to Colonia (55.50 ly)
INFO edlr_core::driver::host: visit #1 from tutorial-jump-log-rs: Colonia driver_id="tutorial-tracker-rs"
INFO edlr_core::plugin::host: tutorial-tracker-rs/last-system = {"system":"Colonia","count":1}
INFO edlr_core::plugin::host: tracker says: {"system":"Colonia","count":1}
```

上から、ドライバの起動 → プラグインがジャンプを見た → ドライバが受け取った
→ 配り直された値がプラグインへ届いた → 次の定期実行で retain 値を読んだ、
という流れになっている。

### うまくいかないとき

| 症状 | 原因 |
| --- | --- |
| `publish failed: BusError::PermissionDenied` | 未承認。GUI の Plugins ページで承認する |
| `BusError::UnknownDriver` | `[[bus]]` の `driver` と `driver.toml` の `id` が違う / ドライバが `drivers-dir` に無い |
| `BusError::UnknownTopic` | `[[topics]]` に無いトピック、または `publish`/`subscribe` に書いていないトピック |
| `on_message` が呼ばれない | `subscribe` に入れていない / ドライバが `emit` していない(ドライバ側のログを見る) |
| `bus.get` が常に `None` | そのトピックが `retain = false` |
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

`core/wit` を更新したのに古い world 向けにビルドした wasm を置いている。
WIT パッケージは `edlr:plugin@0.4.0` で、**旧 world のプラグインは新しい
ホストへロードできない**。Rust では `generate!` がパス指定なら自動追随
するので、ビルドし直すだけでよい。

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

## 8. 次に読むもの

- [plugins.md](plugins.md) — manifest の全フィールド、設定 RPC、無効化の条件
- [capabilities.md](capabilities.md) — HTTP に加えて、サイドカープロセス
  (`driver-process`)とファイルアクセス(`driver-fs`)
- [drivers.md](drivers.md) — ドライバの詳細と承認フロー
- [ui.md](ui.md) — GUI の起動方法。`[[dashboard]]` で自前のウィジェットを
  出すこともできる
- [cli.md](cli.md) — デーモンのフラグ、読み取り位置の永続化、`replay`

サンプル:

- `examples/plugins/tutorial-jump-log-rs` / `examples/drivers/tutorial-tracker-rs`
  — このチュートリアルの完成形
- `examples/plugins/state-reader` — bus とダッシュボードウィジェットの最小例
- `examples/plugins/inara-uploader` — TinyGo 製の実用寄りのプラグイン

`examples` にあるものは `./scripts/install-examples.sh <名前>` でビルドと配置を
まとめて行える。
