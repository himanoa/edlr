# Phase 5 事前分析: runner + host(plugin + driver)

日付: 2026-07-31 / base: main @ e029b6e
分析: plugin/runner.rs(1407行; impl 39–1057, tests 1059–1406)、driver/runner.rs(487行; impl 19–375, tests 377–486)、plugin/host.rs(1483行; impl 5–983, tests 985–1483)、driver/host.rs(735行; impl 8–652, tests 654–734)

> Phase 5 実装計画(`docs/superpowers/plans/2026-07-31-core-refactor-phase5.md`)の
> 根拠資料。行番号は base 時点のもの。spec の Phase 5 =「ループ判定の関数抽出を
> 拡大(fire_all_due・shutdown 系)、wasmtime 配線と `HostCtx` の分離」。
> shutdown 系は Phase 4 で `ThreadSupervisor` へ移設済みなので、本 Phase の
> 実体は (a) `runner/`・`host/` トップレベルモジュールへの再配置、(b) wasmtime
> 配線(engine/ticker)と HostCtx(判定+共有バッファ)の分離、(c) 判定の純関数
> 抽出、(d) plugin/driver 同型コードの共通化。

## 1. 責務インベントリ

### plugin/runner.rs

| 関心事 | 関数(行) |
|---|---|
| 起動走査 | `start_plugins` 184(dir 走査 → `load_and_run_plugin` 267) |
| 初期バッファ組み立て | `load_and_run_plugin` 内 280–373(settings/sidecars/capabilities/filesystem/bus の各 JSON 初期値) |
| スレッド本体 | `run_plugin_thread` 498(load→init→ループ。`disable_and_break!` 584 / `handle_call_result!` 620 / `stop_and_break!` 660 の3マクロ) |
| 発火 | `fire_all_due` 767、`record_fire` 787 |
| ループ判定(純) | `PluginWork` 805、`LoopAction` 830、`next_action` 843(テスト済み) |
| 購読タスク | `spawn_event_subscriber` 869、`event_params` 913(純)、`subscribe_with_initial_value` 940、`spawn_bus_subscriber` 979(承認再確認 996–1010) |
| 起動時警告 | `warn_unresolved_bus` 1036 |
| 定数 | `PLUGIN_WORK_QUEUE_CAPACITY` 85、`BUS_SUBSCRIBER_SHUTDOWN_POLL_INTERVAL` 110、`SCHEDULE_LESS_FALLBACK_TIMEOUT` 147、`CALL_DEADLINE_STRIKES` 162 |

### driver/runner.rs

| 関心事 | 関数(行) |
|---|---|
| 起動走査 | `start_drivers` 58(ready callback 配線 74–78 → 走査 → `load_and_run_driver` 168) |
| sidecar-ready 転送 | `forward_sidecar_ready` 148(key パースは tests 済み) |
| 初期バッファ組み立て | `load_and_run_driver` 内 179–244(bus 無し、`as_settings_manifest` projection) |
| スレッド本体 | `run_driver_thread` 307(`for message in messages_rx` の単純ループ。strikes 無し・on-stop 無し) |
| 定数 | `DRIVER_MESSAGE_QUEUE_CAPACITY` 47 |

### plugin/host.rs

| 関心事 | 項目(行) |
|---|---|
| 定数 | 73–149(`HTTP_TIMEOUT` 91 + const assert 93、`SIDECAR_SHUTDOWN_GRACE` 121 は `edlr_config` 由来 — ハードコード禁止の注記あり) |
| HostCtx(共有バッファ+判定) | struct 153、`new` 227、WIT impls: log 274 / settings 286 / http `send` 349 / bus 391(`check_bus` 427)/ process 530(`resolve_sidecar` 475、`stop` の非対称 561–595)/ fs 659(`resolve_root` 611) |
| 純ヘルパー | `capabilities_json_string` 305、`parse_capability_hosts` 315、`bus_error_to_wit` 458、`to_wit_instances` 514、`to_wit_fs_error` 642、`to_wit_fs_entry` 651 |
| wasmtime 配線 | `PluginHost` 724(engine + ticker スレッド 757 + driver 3点 Arc)、`load` 804(linker/store/limiter/epoch)、`Drop` 830(ticker停止→`stop_all`)、`deadline_ticks` 842 |
| 呼び出し結果分類 | `PluginCallError` 857(`classify` 877: Interrupt→DeadlineExceeded)、`PluginInstance` 910(`CALL_DEADLINE` 2s、call_* 5本) |
| WIT 再輸出 | 46–70(`WitSidecarError` 等 — driver_http_integration.rs が使う。旧パス温存必須) |

### driver/host.rs

plugin/host.rs と同形。差分だけ列挙:

- `DRIVER_HTTP_TIMEOUT` 25s(55)/ `CALL_DEADLINE` は `edlr_config::DRIVER_CALL_DEADLINE_SECS`(630)
- bus は `BusHostHost::emit` 255 のみ(**check 無し** — 未宣言拒否等は `Bus::emit` の責務)。plugin 側の publish/get + check_bus に相当する判定は無い
- `DriverInstance::call_init`/`call_on_message` は `anyhow::Result`(632–651)— plugin 側の `PluginCallError` 分類(期限超過リトライ)は**無い**(意図的: driver は strikes 復帰をしない)
- WIT 再輸出無し

## 2. 同型コード対応表

| ペア(plugin行/driver行) | 判定 |
|---|---|
| `resolve_sidecar` 475/288、`resolve_root` 611/390、`sidecar_key` 509/322 | エラー文字列含め byte 同一 → **判定を純関数へ抽出して共有**(§4) |
| `send` 349/213 | byte 同一(hosts 空判定+`check_url`+写像)→ 許可判定部を共有 |
| `to_wit_instances` 514/327、`to_wit_fs_error` 642/421、`to_wit_fs_entry` 651/430、`bus_error_to_wit` 458/274 | ロジック同一だが **bindgen! が world ごとに別型を生成する**ため関数は共有不能(driver/host.rs 267–273 の注記どおり)。写像は複製のまま |
| engine+ticker(747–762 / 521–536)、`load` 804/578、`Drop` 830/604、`deadline_ticks` 842/615、`EPOCH_TICK_INTERVAL` 73/46 | バインディング型・ctx 型以外同一 → engine/ticker/deadline_ticks を共通型 `EpochEngine` へ(`load` は per-world のまま) |
| driver 3点 Arc + accessor(731–802 / 505–576) | 同一 → `SharedDrivers` 構造体で共通化可(http timeout は引数) |
| `start_plugins` 184 / `start_drivers` 58 の走査ループ | 同形だが前後配線(ready callback / warn_unresolved_bus / router)が違う → 共通化しない |
| 初期バッファ組み立て(280–373 / 179–244) | `as_settings_manifest` projection と bus 有無以外同一 → Phase 4 の `RegistrySubject`(`sidecars()`/`filesystem()`/`as_settings_manifest()`)で共通化可。**registry の refresh 系とは統合しない**(runner.rs 288–293 の注記: ライフサイクル起点が違う意図的重複) |
| `run_plugin_thread` 498 / `run_driver_thread` 307 | 非対称が本質(strikes 復帰・schedule・stop 経路の有無)→ 共通化しない |

## 3. wasmtime 配線と HostCtx の分離(spec 対応)

現状 `PluginHost`/`DriverHost` は「engine+ticker の所有」「driver 3点の所有」
「load 配線」の3責務を持つ。分離案:

- `host/engine.rs` — `EpochEngine`: `Engine` + ticker スレッド + `stop_ticker()` +
  `deadline_ticks`。**`Drop` は付けない**: 現行 Drop の順序(ticker 停止 →
  `process_driver.stop_all()`)を各 host の `Drop` に明示的に残すため
  (フィールド drop は Drop 本体の後に走るので、EpochEngine に Drop を持たせると
  順序が反転する)
- `host/drivers.rs` — `SharedDrivers`: http/process/fs の Arc 3点 + accessor。
  コンストラクタが `http_timeout` を取る(plugin 1.5s / driver 25s)
- `HostCtx`/`DriverCtx` は共有バッファ+判定(判定は §4 で純関数へ委譲)に痩せる。
  `load`(linker/store/instantiate)は bindgen 型が world 固有なので各 host に残す

## 4. 判定の純関数抽出(host/resolve.rs)

エラー文字列を判定側で組み立てて返し、各 ctx は自 world の WIT variant へ写像
するだけにする(byte 同一の担保を1箇所に集約):

```rust
// 値イン値アウト。エラーは (種別, メッセージ文字列) で返す
pub(crate) enum SidecarResolveError { Unknown(String), NotGranted(String), NotConfigured(String) }
pub(crate) fn resolve_sidecar(entries: &BTreeMap<String, SidecarRuntimeEntry>, name: &str)
    -> Result<ProcessSpec, SidecarResolveError>;
pub(crate) enum RootResolveError { Unknown(String), NotGranted(String), NotConfigured(String), ReadOnly(String) }
pub(crate) fn resolve_root(entries: &BTreeMap<String, FsRuntimeEntry>, root: &str, need_write: bool)
    -> Result<PathBuf, RootResolveError>;
pub(crate) fn check_http_permission(hosts: &[String], url: &str) -> Result<(), String>;  // 空→"capability not granted"、以外は check_url
pub(crate) fn check_bus_permission(entries: &BTreeMap<String, BusRuntimeEntry>, driver: &str, topic: &str, direction: BusDirection)
    -> Result<(), String>;
```

`check_bus_permission` は plugin ctx の `check_bus`(427)と
`spawn_bus_subscriber` の still_granted 判定(996–1010 — 同じ判定材料・同じ規則と
ドキュメントに明記済み)の両方から使う。

runner 側のループ判定拡大: `CALL_DEADLINE_STRIKES` 到達判定
(`handle_call_result!` 626–650 に埋まっている)を
`deadline_verdict(strikes: u32) -> DeadlineVerdict { Restart, GiveUp }` として抽出。
`continue`/`break` の制御フローはマクロに残し、判定だけを純関数にする。

## 5. リスク台帳

| # | リスク | 守るテスト |
|---|---|---|
| 1 | 順序不変条件: driver の `bus.register_driver` はスレッド起動前(246–253)/ Disabled 時 `bus.disable_driver`(285–292)/ `subscribe_with_initial_value` の「登録が先、送信が後」(937–939)/ ループの stop_flag 検査がキュー読みより先(685)/ `take_due` は Timeout 時のみ(698–705) | bus_integration.rs、plugin_runner_integration.rs、runner 内 tests(凍結) |
| 2 | `Drop` の順序(ticker 停止 → `stop_all`)— EpochEngine 分離で反転させない | daemon_signal_shutdown_integration.rs + 目視 |
| 3 | エラー文字列 byte 同一("no such sidecar: {name}" 等 10 種) | plugin/host.rs 内 tests(凍結)+ Task 1 の driver 側錨 |
| 4 | **driver ctx の resolve/permission 判定に direct テストが無い**(tests 654–734 は emit 3本のみ)→ 共通化前に錨を足す(Phase 4 リスク4と同じ手当て) |
| 5 | const assert(`HTTP_TIMEOUT < CALL_DEADLINE`、93/57)と `SIDECAR_SHUTDOWN_GRACE` の `edlr_config` 参照を移動で壊さない | コンパイル時 assert 自体 |
| 6 | `bindgen!({path: "wit"})` は CARGO_MANIFEST_DIR 相対 → ファイル移動で壊れない(要ビルド確認のみ) | cargo build |
| 7 | plugin::host の WIT 再輸出(46–70)と `pub use`(mod.rs 26/36 等)の旧パス温存 | 統合テストが import 無変更で通ること |
| 8 | strikes 復帰(load_instance 作り直し)の挙動 — 判定抽出でロジックを変えない | plugin_runner_integration.rs(あれば)+ 挙動はマクロ内不変 |

## 6. タスク系列案(→ 実装計画に反映)

1. [test] 錨: driver ctx の resolve_sidecar / resolve_root / send 許可判定テスト(リスク4)
2. [move] runner 再配置: plugin/runner.rs → runner/plugin.rs、driver/runner.rs → runner/driver.rs、旧パス pub use
3. [move] host 再配置: plugin/host.rs → host/plugin.rs、driver/host.rs → host/driver.rs、旧パス pub use + WIT 再輸出温存
4. [move→logic] `EpochEngine` + `SharedDrivers`(host/engine.rs, host/drivers.rs): (a) plugin 側 move-only 抽出 (b) driver 側を載せる logic
5. [logic] resolve/check 判定の純関数化(host/resolve.rs)+ 純粋テスト。両 ctx と spawn_bus_subscriber を委譲に置換
6. [logic] runner 初期バッファ組み立ての `RegistrySubject` 共通化(registry の refresh 系とは統合しない)
7. [logic] `deadline_verdict` 抽出 + 純粋テスト
8. 完了ゲート
