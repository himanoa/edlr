# inara-uploader Live 版ゲート設計

日付: 2026-07-28
ステータス: 承認済み

## 目的

Inara API のルール(Live = Odyssey / Horizons 4.0 以降のみ送信、Legacy 3.8 と
beta は送信禁止)に準拠し、Live 版以外のセッションのジャーナルイベントを
Inara へ送らないようにする。

## 決定事項

- 判定基準は **Live(メジャーバージョン >= 4 かつ beta でない)**。厳密な
  Odyssey 限定にはしない(Inara ルール準拠。ユーザー環境は Odyssey なので実質同じ)。
- バージョンは購読済みの `LoadGame` イベントの `gameversion` から学習する。
- **未学習(セッションのバージョン不明)の間は送らない**(安全側)。journal の
  途中から replay した場合、次の LoadGame までのイベントは破棄される。

## 実装

- `mapping` パッケージ:
  - 純関数 `isLiveVersion(gameversion string) bool` — 先頭のメジャーバージョンを
    パースして >= 4、かつ文字列に `beta` を含まない(case-insensitive)とき true。
    パース不能・空は false。
  - `State` に学習済み `gameversion`(または判定済みの allowed bool)を保持。
    `loadGame` 構造体に `GameVersion string \`json:"gameversion"\`` を追加し、
    convert 時に学習。
  - 変換の入口で、学習済みかつ Live のときだけ各イベントを変換する。
    LoadGame 自体もゲート対象(ただしバージョン学習・identity 学習は
    ゲートより先に行う — Legacy セッションでも学習だけはして、送信はしない)。
- 挙動: ゲートで落ちたイベントは黙って捨てる(キューに入れない)。

## テスト

- `isLiveVersion` の表形式テスト: `"4.0.0.1904"` true / `"4.1.2"` true /
  `"3.8.0.404"` false / `"4.0.0.100 beta"`・`"Beta 4.0"` false / `""` false /
  `"garbage"` false。
- mapping テスト: Legacy(3.8)の LoadGame 後は FSDJump 等が一切変換されない /
  Live の LoadGame 後は従来どおり変換される / LoadGame 前(未学習)は変換されない。

## スコープ外

- `Fileheader` の購読追加(LoadGame で十分)、beta サーバ検知の高度化、
  Legacy を検知した旨の UI 表示。
