# ドライバ側 submit/complete 設計(http-driver-9znv)

日付: 2026-08-05 / 状態: 承認済み

## 目的

プラグインに入れた submit/complete プロトコル(issue-sizx、WIT 0.5.0)を
ドライバ world にも広げる。TTS など遅い HTTP を扱うドライバが、1 メッセージの
処理で専用スレッドを `DriverInstance::CALL_DEADLINE` までブロックし、
メッセージキュー(64)を詰まらせる病根を解消する。
(内部 async 化 = async-migration Step 2a はこの issue の前半で実装済み。)

## 決定事項

1. **WIT 0.6.0(ABI 破壊)**: `world driver` に
   `export on-job-complete: func(job-id: u64, result-json: string);` を追加。
   `driver-http.submit-send` 自体は 0.5.0 から interface にあり、ドライバ側
   stub を実装に置き換える。全ゲスト再生成、SDK は 0.6.0 へ追従
   (tag `sdk/v0.6.0` / `sdk/go/v0.6.0`)
2. **キューは `runner::plugin::queue` をジェネリック化して共有**
   (`WorkQueue<T>` + admit 関数注入)。重複実装しない。
   `DriverWork::{Message, JobComplete, Disconnected}`
3. **publish のバックプレッシャ契約を保持**: `Bus::publish` の
   「ドライバキュー満杯 = `queue-full` を呼び出し元へ返す(捨てない)」は
   不変。`edlr-driver-channel` の `Bus::register_driver` を
   `SyncSender<Message>` 固定から sink trait(`try_send` が Full/Closed を
   返す)受け取りへ変更し、core 側でキュー直結の sink を実装する。
   `Message` は満杯 → Full(= `queue-full`)、`JobComplete` は常時受け入れ
   (submit の in-flight 上限 8 で有界)
4. **sink の `Drop` が `Disconnected` センチネルを push** し、bus が sink を
   手放した時点(unregister / disable)でドライバループが終了する
   (従来の「チャネル切断で `for` ループ終了」と同じ挙動の保存)
5. **世代管理は `PluginJobs` を共用**。ドライバはインスタンス再作成が無い
   (呼び出し失敗 = 即 Disabled)ため世代は常に 0 だが、プラグインと同じ
   照合を入れて対称性を保つ。in-flight 上限 8・タイムアウト既定 30s /
   上限 60s も同一。`result-json` の形も同一(`job_result_json` を共用)

## 変更箇所

- `core/wit/plugin.wit`: 0.6.0 + world driver の export
- `core/src/runner/plugin/queue.rs`: ジェネリック化(プラグインの挙動不変)
- `drivers/channel/src/lib.rs`: `MessageSink` trait、`register_driver` の
  シグネチャ変更(`SyncSender<Message>` には trait を実装して既存テストを
  温存)
- `core/src/host/driver.rs`: `DriverCtx` に work_tx + jobs、`submit_send`
  実装、`DriverInstance::call_on_job_complete`
- `core/src/runner/driver.rs`: `DriverWork` キューと sink 配線、ループの
  match 化(Message / JobComplete(世代照合)/ Disconnected)
- ゲスト: ed-state に空 export、SDK 0.6.0(Rust 自動追随・Go gen/wit
  再生成)、MoonBit は wit-bindgen 0.45 ピンで再生成、全 wasm 再ビルド
- docs: drivers.md に API 追記、plugins.md のバージョン表、README、sdk.md
  の tag 表記

## テスト

- queue ジェネリック化: 既存テスト維持 + driver 用 admit の境界
- sink: 満杯 → Full(publish が queue-full を受け取る)、Drop →
  Disconnected 配送
- `DriverCtx::submit_send`: 統合テスト(plugin 側 driver_http_integration
  と同型: 200 到着 / 未承認同期拒否 / transport 失敗の非同期 err)
- 既存の bus_integration / daemon_signal_shutdown_integration が実ドライバ
  (ed-state)のロードを検証

## やらないこと

- ドライバへの deadline-restart(世代が進む経路)の導入
- ドライバ用 SDK(SDK はプラグイン専用のまま)
