---
id: manifest-rjoa
title: manifest のキーを置き間違えても黙って無視される
status: closed
labels: bug, dx
created: 2026-07-29T02:39:27Z
updated: 2026-07-29T03:22:47Z
---

`manifest.toml` / `driver.toml` のトップレベルのキーを、TOML のテーブルヘッダより
後ろに書いてしまうと、そのキーは**別のテーブルの子として解釈される**。edlr は
これをエラーにせず、そのままロードを成功させる。結果、宣言したはずの機能が
丸ごと消えたまま「正常に起動している」ように見える。

## 再現

ドライバの `driver.toml` で `settings = [...]` を `[[sidecar]]` より後ろに置いた:

```toml
id = "coeiroink"
name = "COEIROINK"
version = "0.1.0"
entry = "driver.wasm"

[[topics]]
name = "speak"
retain = false

[[sidecar]]
name = "worker"
reason = "..."
args = ["--listen-port", "{port}"]
port = 51000
scalable = true

settings = [                          # ← [[sidecar]] の後ろ
  { key = "default-worker", label = "既定の割当先", type = "select", default = "0", options = ["0", "1"] },
  # ... 全 35 項目
]
```

TOML の仕様どおり、この `settings` は `sidecar[0].settings` になる:

```
$ python3 -c "import tomllib; d=tomllib.load(open('driver.toml','rb')); \
  print('top-level settings:', len(d.get('settings', []))); \
  print('sidecar[0] keys:', sorted(d['sidecar'][0].keys()))"
top-level settings: 0
sidecar[0] keys: ['args', 'name', 'port', 'reason', 'scalable', 'settings']
```

## 実際の挙動

デーモンは何の警告も出さずにドライバをロードする:

```
INFO edlr_core::driver::host: coeiroink driver started driver_id="coeiroink"
```

RPC で見ると設定は空:

```
drivers/list                              → "settings": []
drivers/get-settings {"driver":"coeiroink"} → {}
```

ドライバ自身は `host-settings.get-all()` から `{}` を受け取るため、
「ユーザーがまだ何も設定していない」のか「設定項目の宣言が消えている」のかを
区別できない。このケースではドライバが全リクエストを既定値で処理し続けた。

## なぜ黙って通るのか

`core/src/plugin/manifest.rs` / `core/src/driver/manifest.rs` の
`#[derive(serde::Deserialize)]` はどれも `deny_unknown_fields` を付けていない
(`grep -rn "deny_unknown_fields" core/src` は 0 件)。したがって:

- `SidecarRequest` は知らない `settings` キーを黙って捨てる
- `DriverManifest.settings` は `#[serde(default)]` なので空の `Vec` で通る

どちらの側にも「おかしい」と気づく機会が無い。

## 影響

置き場所を間違えやすいのは `settings` だけではない。`events` / `entry` /
`description` など、**トップレベルのスカラー・配列キーはすべて同じ形で
サイレントに失われうる**。とくに `[[settings]]` や `[[capabilities]]` を
書いたあとに `events = [...]` を足す、というのは自然にやってしまう編集で、
その瞬間にプラグインはイベントを 1 つも受け取らなくなる — ロードは成功し、
ログにも何も出ないまま。

デバッグが難しいのは、失敗が「起動しない」ではなく「起動するが何もしない」
形で出ることによる。今回は edlr の RPC を直接叩いて `"settings": []` を
見るまで原因に辿り着けなかった。

## 提案

**1. ネストされた要求構造体に `#[serde(deny_unknown_fields)]` を付ける**

`SidecarRequest` / `CapabilityRequest` / `FilesystemRequest` / `BusRequest` /
`TopicSpec` / `ScheduleSpec` / `SettingField` あたり。これらは edlr が形を
完全に決めている構造体なので、知らないキーはユーザーの書き間違いと断定できる。
今回のケースは `SidecarRequest` に付けるだけで、次のような明示的な失敗になる:

```
driver coeiroink: unknown field `settings` in [[sidecar]] — トップレベルに
置くつもりでは? TOML はテーブルヘッダより後ろのキーをそのテーブルの子として
解釈します
```

**2. トップレベルの `Manifest` / `DriverManifest` に付けるかは要検討**

こちらに付けると、新しいフィールドを増やしたときに古い edlr が新しい
manifest を読めなくなる。前方互換を捨ててよいかの判断が要るので、1 とは
分けて考えたほうがよい。付けない場合でも「知らないトップレベルキーがあれば
warn ログを出す」だけで、今回の類の事故はかなり減る。

**3. 補足として、ロード時のサマリを info ログに出す**

`driver coeiroink loaded: topics=1 settings=0 sidecars=1` のような 1 行があれば、
`settings=0` を見て気づけた。1 が入れば不要かもしれないが、宣言と実際の
読み取り結果が一致しているかを目視できる価値はある。

## 発見の経緯

[edlr-himanoa-coeiroink](https://github.com/himanoa/edlr-himanoa-coeiroink)
(COEIROINK 読み上げドライバ)の実装中に踏んだ。ドライバ側の回帰テストとしては
「`driver.toml` をパースしてトップレベルの `settings` が期待件数あること」と
「`sidecar[0]` に `settings` キーが存在しないこと」を検証するテストを入れて
対処したが、これは各ドライバ作者がそれぞれ書かないといけない類のもので、
本来は edlr 側で弾けるはずのもの。
