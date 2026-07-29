# select 型設定の候補をドライバの retain トピックから引く

Issue: `select-retain-085r`

## 背景

`select` の候補は manifest に書いた静的な配列で、`SettingField::Select { options:
Vec<String> }`(`core/src/plugin/manifest.rs:24`)がそのまま保存時の検証にも使われる
(`core/src/plugin/settings.rs:143`)。

そのため「候補がインストール環境で決まる」設定を UI に出せない。実例が
`edlr-himanoa-coeiroink` の話者指定で、選べる話者は COEIROINK の
`speaker_info/*/metas.json` を読んで初めて分かる。現状は自由入力の `string` に
逃がしていて、綴りを間違えると既定の話者に黙ってフォールバックする。

ドライバは既に `bus-host.emit` で retain トピックへ値を載せられ、ホストはそれを
`Bus::retained_for` で保持している(`core/src/plugin/runner.rs:947`、
`core/src/driver/host.rs:659`)。これを候補源として指せるようにする。

## マニフェスト

`SettingField::Select` の `options` を任意にし、`options-from` を追加する。両者は
排他で、どちらか一方が必須。

```toml
# 従来どおり(静的)
{ key = "size", label = "サイズ", type = "select", options = ["small", "large"], default = "small" }

# 追加(動的)
{ key = "default-speaker", label = "既定の話者", type = "select", default = "",
  options-from = { driver = "coeiroink", topic = "speakers" } }
```

新しい型を 2 つ足す:

- `SelectOption { value: String, label: String }` — TOML / JSON のどちらでも
  `"foo"`(`value` も `label` も `"foo"`)と `{ value, label }` の両方を受ける。
  単純な候補を短く書けることと、表示名と保存値を分けられることの両立
- `OptionsFrom { driver: String, topic: String }` — どちらも `[a-z0-9-]+`
  (プラグイン / ドライバ id と同じ字種)

マニフェスト検証で弾くもの:

- `options` と `options-from` の両方指定、および両方省略
- 静的 `options` の空配列
- `options-from` の `driver` / `topic` の字種違反

`options-from` が指すドライバやトピックが存在するかは**検証しない**。`[[bus]]` の
接続先が未解決でもプラグインがロードできるのと同じ理由で、ドライバの後入れ・
入れ替えを許す。

## 保存時の検証

`options-from` の select は **string であることだけ**を見る(候補との照合をしない)。
静的 `options` の select はこれまで通り `NotAnOption` で弾く。

候補は非同期に到着し、ドライバの無効化で消えもする。照合すると同じ操作が
タイミングで成否を変え、「ドライバ起動前は設定を保存できない時間帯」ができる。
UI がドロップダウンである以上、issue が挙げていた「綴りを間違える」経路は
そちらで塞がる。RPC を直接叩けば候補外の値も保存できるが、それは UI を経ない
利用者の責任の範囲とする。

## 候補の解決

`Registry` は現在 `Bus` を持たない(持っているのは `driver_registry`)。`Bus` を
`Registry::new` の引数に足し、`runner::start_plugins` から配線する。

`Registry::list()` が manifest を clone する時点で、`options-from` を持つ select に
ついて:

1. `bus.retained_for(driver, topic)` を読む
2. JSON 配列としてパースできれば `options` を埋める
3. ドライバ未登録・retained 未着・JSON が壊れている → `options` は `None` のまま

`list()` は設定画面を開くたびに呼ばれるので、**未解決をログに出さない**。warn を
出すと、ドライバが起動していない環境で RPC のたびにログが積み上がる。原因は
`options` が `null` であること自体と UI のメッセージから追える。

**ドライバ側の `[[settings]]` にも同じ解決を入れる**(`DriverRegistry::list`)。
COEIROINK の例では話者一覧を emit するのも、話者を選ぶ設定を持つのもドライバ
自身なので、ここが抜けていると主要なユースケースが拾えない。

## RPC の形

`select` の `options` を **常に `{ value, label }` の配列**に統一する(静的な候補も
`value == label` で展開)。未解決は `null`。`options-from` はそのまま返し、UI が
「どのドライバのどのトピックが未着か」を書けるようにする。

```json
{ "type": "select", "key": "default-speaker", "label": "既定の話者", "default": "",
  "optionsFrom": { "driver": "coeiroink", "topic": "speakers" },
  "options": [{ "value": "a1b2:0", "label": "アメノちゃん/ノーマル" }] }
```

静的 select の `options` も形が変わるため、フロントエンドの型と描画は一度更新が
必要になる。静的と動的で描画経路を分けないための意図的な選択。

## UI

`ui/frontend/src/types/plugin.ts` の `options` を `SelectOption[] | null` にし、
`ui/frontend/src/components/PluginForm.tsx` で 3 状態を描き分ける:

- **候補あり** — 通常のドロップダウン(`label` を表示し `value` を保存)
- **候補なし(`null`)** — `<select disabled>` に現在値だけを 1 件出し、下に
  「候補を取得できません(ドライバ `coeiroink` のトピック `speakers` が未着です)」。
  保存済みの設定値は消さない
- **候補はあるが現在値がその中に無い** — 現在値を先頭に足した上で「保存済みの値が
  現在の候補にありません」と警告する

候補の取得契機は既存の `plugins/list` / `drivers/list`(設定画面を開いたとき)だけ。
新しい RPC も WebSocket の push 経路も作らない。候補が変わるのはサイドカーの
起動時くらいで、開き直しで足りる。

## 承認との関係

`options-from` が指すドライバに対する `[[bus]]` の宣言・承認は**要求しない**。
retained を読むのはプラグインではなくユーザーの代理としてのデーモンであり、返す
先も設定画面である。要求すると承認状態によって候補の有無が変わり、「候補が空」の
理由が 2 種類に増えて説明できなくなる。

プラグインが自分で同じトピックを読みたい場合は、これまで通り `[[bus]]` の宣言と
承認が要る。設定 UI 経由の読み取りと、プラグインからの読み取りは別の経路である。

## テスト

- **manifest** — 静的 / 動的の両形式がパースできる、両方指定と両方省略が弾かれる、
  `options-from` の字種違反が弾かれる、`"foo"` と `{value,label}` が同じ
  `SelectOption` になる
- **settings** — `options-from` の select は候補外の文字列でも保存できる、静的
  `options` の select は従来通り弾かれる、型が string でなければ弾かれる
- **registry** — retained あり / 未着 / 壊れた JSON / ドライバ未登録の 4 ケースで
  `list()` が返す `options` が期待通りになる。`Registry` と `DriverRegistry` の両方
- **UI** — `PluginForm` の 3 状態(候補あり / `null` / 現在値が候補外)

## やらないこと

- emit のたびに候補を push 配信すること
- retained のペイロードをスキーマ検証すること(ドライバを信頼する)
- `options-from` を `select` 以外の設定型へ広げること
