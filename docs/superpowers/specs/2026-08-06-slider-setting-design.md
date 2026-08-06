# slider 設定型の追加

音量のような有界な数値設定をスライダーで編集できるようにする。
プラグイン/ドライバのマニフェストに新しい設定型 `slider` を追加し、
設定画面(PluginForm)で range input として描画する。

## マニフェスト(TOML)

```toml
[[settings]]
type = "slider"
key = "volume"
label = "音量"
default = 50
min = 0
max = 100
step = 5      # 任意、省略時 1
```

- `min` / `max` は必須(境界がないとスライダーは描画できない)
- `step` は任意、省略時 `1`
- 値の型は `number` と同じ f64

## core

- `manifest::SettingField` に variant を追加:
  `Slider { key, label, default: f64, min: f64, max: f64, step: f64 (serde default = 1.0) }`
- マニフェストの整合性検証(既存の settings 検証と同じ場所):
  - `min < max`
  - `min <= default <= max`
  - `step > 0`
- `settings/validate.rs` の保存値検証: 数値であること、`min <= v <= max`
  (step の倍数チェックはしない — UI 以外からの保存で丸め誤差の f64 を弾いて
  しまうし、範囲内なら実害がない)
- RPC 応答は enum の serialize でそのまま出る。select のような解決処理は不要

## UI

- `types/plugin.ts` の `SettingField` に
  `{ type: "slider"; key; label; default: number; min: number; max: number; step: number }`
- `PluginForm` の `Field` に case を追加: ネイティブ `<input type="range">` +
  現在値の数値表示。ライブラリは追加しない
- 保存タイミング: ドラッグ中は commit しない。**離したとき(pointerup)/
  キーボード操作後の blur** に、値が変わっていれば 1 回だけ commit
  (既存フィールドの「単位ごとに即保存」の作法に合わせつつ RPC を連打しない)

## テスト

- core: マニフェスト整合性(min>=max / default 範囲外 / step<=0 を弾く)と
  保存値検証(範囲外を弾く)の純粋テスト
- UI: PluginForm テストに slider の描画(min/max/step/現在値)と
  「操作終了で 1 回だけ保存される」ケースを追加
