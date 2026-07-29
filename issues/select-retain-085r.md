---
id: select-retain-085r
title: select 型設定の候補をドライバの retain トピックから動的に引けるようにする
summary: select の options が manifest 固定のため、インストール環境で決まる候補(COEIROINK の話者一覧など)を設定 UI に出せない / ドライバの retain トピックを候補源にする案 / 未着手
status: closed
labels: ui
created: 2026-07-29T13:03:45Z
updated: 2026-07-29T13:27:20Z
---

## 何が困るか

`select` の候補は manifest に書いた静的な配列で、`SettingField::Select { options: Vec<String> }`
(`core/src/plugin/manifest.rs:29`)がそのまま検証にも使われる
(`core/src/plugin/settings.rs:151`)。

そのため「候補がインストール環境で決まる」設定を UI に出せない。実例が
`edlr-himanoa-coeiroink` の話者指定で、選べる話者は COEIROINK の
`speaker_info/*/metas.json` を読んで初めて分かる。今は `default-speaker` /
`slotN-speaker` を自由入力の `string` にして、綴りを間違えたら既定の話者に
フォールバックする、という運用で逃げている。

## 案

ドライバはすでに `bus-host.emit` で retain トピックへ値を載せられ、ホストは
それを `retained_for` で保持している(`core/src/plugin/runner.rs:947`、
`core/src/driver/host.rs:659`)。候補源としてこれを指せるようにするのが
筋が良さそう:

```toml
{ key = "default-speaker", label = "既定の話者", type = "select",
  options-from = { driver = "coeiroink", topic = "speakers" }, default = "" }
```

考えるべき点:

- **検証をどうするか。** 今の `settings.rs` は書き込み時に options 内かを
  検証する。retained 値が未着(ドライバ起動直後、サイドカー起動前)のときに
  弾くと、設定が保存できない時間帯ができる。未着なら検証を素通しする、
  あるいは自由入力を許して警告に留める、のどちらかが要る。
- **retained のペイロード形式。** 表示名と保存値を分けたい
  (話者一覧なら「アメノちゃん/ギャル」を表示しつつ UUID:styleId を保存する、
  といった要求がありうる)。`list<string>` で済ませるか
  `[{value,label}]` にするか。
- **UI の更新契機。** retained が更新されたとき設定画面が開いていたら
  候補を差し替えるのか、開き直しでよいのか。
- **ドライバが無効化されたときの retained 破棄**
  (`core/src/driver/registry.rs:837`)との兼ね合い。候補が消えた状態で
  既存の設定値をどう扱うか。

## 経緯

`edlr-himanoa-coeiroink` 側では、話者一覧を retain トピック `speakers` に
流すところまでをドライバの責務として作る。この issue が入れば、その値を
設定 UI の候補源としてそのまま繋げられる。
