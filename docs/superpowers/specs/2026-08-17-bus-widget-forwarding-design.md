# bus メッセージのダッシュボードウィジェット転送

2026-08-17

## 目的

ドライバが emit する bus メッセージ（例: eddn ドライバの `upload-status`、
retain = true）をダッシュボードウィジェットでリアルタイム表示できるように
する。2026-07-28 のウィジェット設計で「v1 スコープ外、将来追加」とされていた
bus 転送の実装。最初のユースケースは eddn-sender プラグインの
「EDDN アップロード状況」ウィジェット。

## スコープ

- edlr core: bus → WS 転送 + retained 取得 RPC
- edlr ui: WS フレーム追加 + `WidgetApi` 拡張（`onBus` / `retained`）
- edlr-plugin-eddn-sender: `[[dashboard]]` ウィジェット追加（wasm 無変更）

スコープ外: bus 購読の manifest 宣言・per-widget の配送制御（後述）、
履歴の永続化、upload-status 以外のウィジェット。

## 設計

### core: bus タップ

- `Bus`（`drivers/channel/src/lib.rs`）にタップを 1 本追加する。
  `emit()` が成功したとき（retained 更新後）、WS フレーム
  `{"type":"event","kind":"bus","driver":<id>,"topic":<topic>,"payload":<string>}`
  を流す。
- payload は UTF-8 文字列として送る。非 UTF-8 は lossy 変換で妥協する
  （現行の利用者は全て JSON 文字列）。
- `bin/edlr.rs` で `ServerState::attach_log_stream` と同じ要領で
  ReplayBuffer + broadcast の既存 WS 経路に合流させる。

### core: retained RPC

- 新規 RPC `drivers/bus-retained`（既存の `drivers/*` dispatch に相乗り）。
  params `{driver, topic}`、結果は
  `payload`（文字列）または `null`。実装は `Bus::retained_for` を呼ぶだけの
  read-only ハンドラ。
- 必要な理由: UI を後から開いた場合、最後の emit が ReplayBuffer の窓から
  落ちていると表示が空になる。retain 済みの最新値をマウント時に引くため。

### 承認モデル（割り切り）

- bus フレームは接続中の全 UI クライアントに流れる。UI は既に全ログ・
  全 journal イベントを見られる承認サーフェスであり、新しい権限概念は
  追加しない。
- `[[dashboard]]` への bus 購読宣言も追加しない。ウィジェットはホストと
  同一 DOM で動く信頼済みコードなので、宣言はドキュメント以上の意味を
  持たない。フィルタはウィジェット側で行う。将来 per-widget の配送制御が
  必要になったら宣言式に upgrade する。

### ui: WS パースと LogEntry

- `ws.ts` に `kind: "bus"` フレームのパースを追加し、`LogEntry` に
  `{ type: "event"; kind: "bus"; driver: string; topic: string; payload: string }`
  バリアントを足す。
- Logs 画面は既存の kind スイッチに 1 分岐追加し、
  `driver/topic: payload` の 1 行表示。

### ui: WidgetApi 拡張

`WidgetHost` の `WidgetApi` に 2 メソッド追加:

```ts
/** bus フレームの購読。全 driver/topic が届くのでウィジェット側でフィルタする。 */
onBus(cb: (msg: { driver: string; topic: string; payload: string }) => void): void;
/** retained 値の取得。未保持なら null。 */
retained(driver: string, topic: string): Promise<string | null>;
```

- `onBus` は既存 `onEvent` と同じ listeners 配列方式（mount 中に同期登録、
  mount 完了時に蓄積分から配送）。`matchesEvent` は通さず、bus フレーム
  だけを配る。
- `retained` は `rpc.ts` に足す `busRetained(driver, topic)` の薄いラッパー。
- 配送・リスナー例外の扱いは `onEvent` と同じ（例外は console に残して継続）。

### eddn-sender: ウィジェット

manifest 追加:

```toml
[[dashboard]]
id = "upload-status"
title = "EDDN Upload"
entry = "ui/upload-status/index.js"
size = "small"
```

`ui/upload-status/index.js`（素の ESM、state-reader の last-jump と同形式）:

- mount 時に `api.retained("eddn", "upload-status")` で初期表示
  （`null` なら「アップロード待ち」、RPC 失敗なら「取得失敗」）
- `api.onBus` で `driver === "eddn" && topic === "upload-status"` のみ更新
- 表示: 成功/失敗バッジ + schema 名 + 失敗時は error 文 + 最終更新時刻
- payload の JSON パース失敗時は生文字列をそのまま表示する

wasm（plugin.wasm）は無変更。データは UI ストリーム経由で届くため、
プラグインの `[[bus]]` subscribe も不要。

## エラー処理

- WS 切断・Lagged: 既存経路の挙動（捨てて継続）に乗る。取りこぼしは
  retained RPC で次回マウント時に回復する。
- retained RPC 失敗: ウィジェットが「取得失敗」を表示。
- emit 側: タップへの送信失敗（受信者なし等）は emit の成否に影響させない。

## テスト

- core: `Bus::emit` がタップにフレームを流すこと。`bus/retained` RPC の
  往復（値あり / null）。既存 `server/tests.rs` のパターンに倣う。
- frontend: `ws.ts` の bus フレームパース 1 ケース。`WidgetHost` の
  `onBus` 配送 1 ケース（`WidgetHost.test.tsx` に追加）。
- ウィジェット JS は表示のみのためテストなし。
