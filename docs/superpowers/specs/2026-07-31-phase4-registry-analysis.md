# Phase 4 事前分析: Registry 解体(plugin + driver)

日付: 2026-07-31 / base: main @ c607ff0
分析: plugin/registry.rs(3417行; impl 389–1866, tests 1868–3417)、driver/registry.rs(1723行; impl 134–909, tests 911–1723)

> Phase 4 実装計画(`docs/superpowers/plans/2026-07-31-core-refactor-phase4.md`)の
> 根拠資料。行番号は base 時点のもの。

## 1. 責務インベントリ

### Registry(plugin)

| 関心事 | メソッド(行) | 触る共有状態 |
|---|---|---|
| Grants/capabilities | `capabilities` 964, `set_capabilities` 1084, `effective_hosts` 1161(テスト用) | `entries`, `grants_store`, `capabilities_lock`, entry の `capabilities_json`(+`sidecars_json` 読み) |
| Settings/values | `values` 977, `set_values` 1003, `entry_settings` 1793 | `entries`, `settings_store`, `settings_json` |
| Sidecars | `sidecars` 1182, `set_sidecar_config` 1564, `set_sidecar_grant` 1593, `control_sidecar` 1677, `stop_all_sidecars` 1764; private: `sidecar_key` 660, `build_sidecar_infos` 667, `sidecar_info_and_entry` 690, `refresh_sidecar_runtime` 1489 | `entries`, `sidecar_config_store`, `grants_store`, `process_driver`, `sidecar_runtime_locks`, `capabilities_lock`(refresh の step 3), `sidecars_json`+`capabilities_json` |
| Filesystem | `filesystem` 1189, `filesystem_buffer` 1197(テスト用), `set_filesystem_config` 1309, `set_filesystem_grant` 1334; private: `build_filesystem_infos` 726, `refresh_filesystem_runtime` 1262 | `entries`, `filesystem_config_store`, `grants_store`, `filesystem_runtime_locks`, `filesystem_json` |
| Bus | `bus` 1369, `bus_buffer` 1220(テスト用), `set_bus_grant` 1426; private: `build_bus_infos` 755, `refresh_bus_runtime` 1382 | `entries`, `grants_store`, `bus_runtime_locks`, `driver_registry`(resolved), `bus_json` |
| Dashboard | `events_of` 837, `dashboard` 842, `set_dashboard_grant` 850, `dashboard_widgets_for_ui` 869, `dashboard_asset_path` 900; private: `build_dashboard_infos` 778 | `entries`, `grants_store`, `plugins_dir`(直接 `is_file()`) |
| Schedules | `build_schedule_infos` 805(private), `register_schedule_view` 461(pub(crate)) | `schedule_views`, `entries` |
| スレッド監督 | `register_plugin_thread` 436, `shutdown_plugins` 533, `bus_subscriber_shutdown_flag` 582, `shutdown_bus_subscribers` 1787, `register_drop_counters` 470, `dropped_counts` 479(private), `set_disabled` 1825 | `plugin_threads`, `bus_subscriber_shutdown`, `drop_counters`, `entries`(+`sidecar_runtime_locks`, `process_driver`) |
| 一覧/スナップショット | `snapshot` 599, `list` 616, `plugins_dir` 594, `push` 586; private `find_manifest` 1148, `is_disabled` 1137 | ほぼ全部(`list` は select_options::resolve 経由で bus も) |

### DriverRegistry

| 関心事 | メソッド(行) |
|---|---|
| Grants/capabilities | `set_capabilities` 379 |
| Settings | `values` 329, `set_values` 342 |
| Sidecars | `sidecars` 492, `set_sidecar_config` 685, `set_sidecar_grant` 707, `control_sidecar` 750, `stop_all_sidecars` 906; private `sidecar_key` 218, `build_sidecar_infos` 224, `sidecar_info_and_entry` 248, `refresh_sidecar_runtime` 607 |
| Filesystem | `filesystem` 500, `set_filesystem_config` 550, `set_filesystem_grant` 570; private `build_filesystem_infos` 282, `refresh_filesystem_runtime` 509 |
| 一覧 | `list` 181, `drivers_dir` 170, `manifest_of` 319, `push` 162; private `find_manifest` 308, `find_manifest_for_shared` 447, `is_disabled` 459 |
| ライフサイクル | `set_disabled` 864(`&DriverManifest` を取り、bus.disable_driver を先に呼ぶ。entry 未 push でも動く) |

bus-grant / dashboard / schedules / dropped / スレッド監督は無い。`shutdown_drivers` も存在しない(daemon は `stop_all_sidecars` のみ呼ぶ — bin/edlr.rs:381)。

## 2. 共有状態とロック規律

Plugin `Registry` フィールド(248–376): `entries: Arc<Mutex<Vec<PluginEntry>>>`, `_host`, `settings_store`, `grants_store`, `sidecar_config_store`, `filesystem_config_store`, `process_driver: Arc<ProcessDriver>`, `capabilities_lock: Arc<Mutex<()>>`(267), `sidecar_runtime_locks`(302), `filesystem_runtime_locks`(322), `bus_runtime_locks`(334), `driver_registry`, `bus`, `plugins_dir`, `bus_subscriber_shutdown: Arc<AtomicBool>`(349), `plugin_threads`(357), `schedule_views`(366), `drop_counters`(375)。Driver 側(99–132)はその部分集合。

ロック順(289–301 に文書化、一方向厳守): `entries`(manifest clone とバッファハンドル取得のみ保持)→ runtime-lock map の Mutex(lookup/insert のみ)→ per-id runtime lock → `capabilities_lock`。3つの per-id map が別々なのは意図的(fs/bus の取消が `ProcessDriver::stop` のブロックに巻き込まれないため — 303–334)。

分割可能性:
- **Filesystem / Bus / Dashboard / Settings / Schedules**: 自分のロック+store+entries+自分の JSON バッファのみ → きれいに分割可
- **Sidecar ↔ Grant が唯一の実結合**: `refresh_sidecar_runtime`(1539–1555)が `capabilities_lock` を取り `capabilities_json` を書き換え、`set_capabilities`(1116–1124)は live な `sidecars_json` バッファを読んで `implicit_http_hosts` をマージする。両サービスに**同一の** `Arc<Mutex<()>>` を注入し、`id lock → capabilities_lock` の順序を維持すること
- `grants_store` は5関心事で共有(`Arc<G: GrantStorage>` の共有で可)
- `set_disabled` は facade のオーケストレーションに残す(分解しない)
- `list` は全 builder を固定順で回す → facade に残す

## 3. 同型コード対応表

| ペア(plugin行/driver行) | 判定 |
|---|---|
| `push` 586/162, `sidecar_key` 660/218, `is_disabled` 1137/459, `lock_for` 951/482, `stop_all_sidecars` 1764/906 | 同一 → ジェネリック化容易 |
| `sidecar_info_and_entry` 690/248, `build_sidecar_infos` 667/224, `build_filesystem_infos` 726/282 | `manifest.as_settings_manifest()` 挿入以外同一 |
| `refresh_filesystem_runtime` 1262/509, `set_filesystem_config` 1309/550, `set_filesystem_grant` 1334/570 | エラー文字列含め byte 同一(unknown id のエラー enum と projection のみ差)→ **最もきれいなジェネリック化対象** |
| `refresh_sidecar_runtime` 1489/607, `set_sidecar_config` 1564/685, `set_sidecar_grant` 1593/707 | 同一(エラー文字列含む) |
| `control_sidecar` 1677/750 | `"plugin {id} is disabled"` vs `"driver {id} is disabled"`(1705/774)だけ差 → 主語フック要 |
| `set_capabilities` 1084/379 | 2段(persist → バッファ書換+implicit hosts マージ)は同型。projection とエラー enum 差 |
| `set_values` 1003/342, `values` 977/329 | **実分岐**: plugin は `split_secrets`(1035)で secret を戻り値から剥がす。driver は剥がさない(372)。エラー enum も差 |
| `list` 616/181 | 共通パターンはあるが Info 型が違いすぎる → ジェネリック化の益薄 |
| `set_disabled` 1825/864 | **ジェネリック化禁止**。driver 版は意図的な非対称(bus.disable 先行・entry 未 push でも stop — 回帰テスト 1051/1140) |

ジェネリック鍵: crate 内 trait(例 `registry::RegistrySubject`)— `id()` / `sidecars()` / `filesystem()` / `as_settings_manifest() -> Manifest`(plugin は identity)/ `unknown_error(id)`(`UnknownPlugin` vs `UnknownDriver`)/ `subject_noun()`(disabled メッセージ用)。sidecar + filesystem 群を完全にカバー(約1100行が重複解消)。`values`/`set_values`/`set_capabilities` は内部共有+側ごとの薄い wrapper(secret 剥がしとエラー写像は wrapper 側)。

## 4. ThreadSupervisor 境界

runner 向け現行面(全て pub(crate)、runner.rs から): `push`(runner.rs:421)、`register_plugin_thread`(439)、`register_drop_counters`(444)、`bus_subscriber_shutdown_flag`(486)、`register_schedule_view`(578、プラグインスレッド自身から)、`set_disabled`(591 + trap 経路)。daemon 向け(bin/edlr.rs:349–356, 381): `shutdown_plugins`、`shutdown_bus_subscribers`、`stop_all_sidecars`。

提案 `ThreadSupervisor`(registry/supervisor.rs)— `plugin_threads` + `PluginThreadHandle`(379–387)+ `PLUGIN_STOP_JOIN_TIMEOUT`/`POLL_INTERVAL`(38–45)+ `bus_subscriber_shutdown` + `drop_counters` + `schedule_views` を所有:

```
register_thread(id, work_tx, JoinHandle, stop_flag)
register_schedule_view(id, ScheduleView)
register_drop_counters(id, Arc<DropCounters>)
dropped_counts(id) -> DroppedCounts
published_schedule(id) -> HashMap<name, DateTime>
shutdown_all()            // 現 shutdown_plugins の2段
shutdown_bus_subscribers() / shutdown_flag() -> Arc<AtomicBool>
```

trait 化しない(モック consumer が未実証 — trait-di.md)。facade は委譲メソッドを温存し runner.rs / bin の diff はゼロ or import のみ。

## 5. サービス分解案

spec が言っていない前提: 共有の **EntryTable**(`registry/entries.rs`、`EntryTable<E>` over PluginEntry/DriverEntry)— `entries` + `push`/`find_manifest`/`is_disabled`/`set_state`/バッファハンドル clone + `lock_for` map ヘルパー。全サービスが必要とするので最初に切る。

| サービス | 移るもの | ジェネリクス + alias |
|---|---|---|
| `GrantService<G: GrantStorage>` | capabilities / set_capabilities / effective_hosts(+ driver set_capabilities)+ **dashboard 群**(spec に置き場がないが実体は grants + is_file 1発 → ここに置く) | `type DiskGrantService = GrantService<GrantsStore>`; `capabilities_lock` を保持 |
| `SidecarService<G, P: ProcessControl>` | sidecar 群 両側、`RegistrySubject` ジェネリック | Grant と**同一の** `capabilities_lock` Arc を共有。`stop_named(id, names)` を facade の `set_disabled` 用に公開 |
| `FilesystemService<G>` | fs 群 両側 | 独立ロック、最もクリーン |
| `BusService<G>` | plugin 専用: bus / set_bus_grant / refresh / build / bus_buffer | `DriverRegistry` clone 保持(resolved 用)。`shutdown_bus_subscribers` は Supervisor 側 |
| `ThreadSupervisor` | §4 | 具象 |
| facade | `new`(旧シグネチャ)/ `list` / `snapshot` / dirs / `manifest_of` / `push` / `entry_settings` / `set_disabled`(オーケストレーション)/ `stop_all_sidecars` / 委譲群 | 公開シグネチャ・エラー型不変。旧パス pub use |

依存辺: 全サービス → EntryTable。Sidecar → Grant(共有ロック+バッファ)。Bus → DriverRegistry。facade → 全部。

## 6. リスク台帳

| # | リスク | 守るテスト |
|---|---|---|
| 1 | ロック順(entries → map → id-lock → capabilities_lock)の破れ | `concurrent_control_sidecar_start_and_grant_revoke_never_leaves_an_ungranted_instance_running`(≈2938)、`revoking_filesystem_access_is_not_blocked_by_a_sidecar_stop_in_progress`(≈2675)、driver 側 1227+ |
| 2 | `capabilities_json` の二重書き手セマンティクス(set_capabilities は live バッファを読む/refresh は再計算) | `set_capabilities_persists_grant_and_updates_shared_capabilities_json`(≈3022)、`concurrent_set_capabilities_keeps_shared_buffer_consistent_with_disk`(≈3188) |
| 3 | ジェネリック化でのエラー文字列パリティ(unknown plugin/driver、disabled 主語) | `values_for_unknown_plugin_...`(1921)等 + driver 1181+ |
| 4 | driver の values/set_values は secret を剥がさない — **現状テストなし** → 先に pin を足す |
| 5 | 副作用順序(stop→バッファ書換、Disabled→stop、driver は bus.disable 先行・未 push でも stop) | ≈2797, ≈2853, driver 1051, 1140 |
| 6 | list の組み立て順 | rpc_pin_integration.rs + render テスト |
| 7 | `shutdown_plugins` の2段(signal→共有 deadline)移設 | ≈3211, 3249, 3294 + daemon_signal_shutdown_integration.rs |
| 8 | schedule published/estimated フォールバック(805–833) | ≈2410, 2437 |

## 7. タスク系列案(→ 実装計画に反映)

1. [test] 錨: `ScheduleSpec::IntervalSeconds` の render テスト + driver secret 非剥がしの pin(`CapabilityRequest` は Http 1 variant のみで「非 Http」テストは対象外)
2. [move] EntryTable
3. [move] ThreadSupervisor(リスク7)
4. [move+generic] FilesystemService(パターン確立、最低リスク)
5. [move] BusService
6. [move+generic] SidecarService(最高リスク: 1/2/3/5)
7. [move] GrantService(+dashboard、capabilities_lock 共有)
8. [logic] values/set_values/set_capabilities の内部共通化(secret/エラー写像は wrapper)
9. [move] facade を registry/ へ再配置、旧パス pub use
10. [logic] 判断関数抽出 + 初のモック純粋テスト(ProcessControl/GrantStorage の初 consumer)
