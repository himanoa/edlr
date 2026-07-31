# Phase 6 事前分析: 仕上げ(plugin/・driver/ 解体 + 旧パス削除 + journal 作法揃え)

日付: 2026-07-31 / base: main @ 1a5d0eb

> Phase 6 実装計画(`docs/superpowers/plans/2026-07-31-core-refactor-phase6.md`)の
> 根拠資料。spec の Phase 6 =「journal 等の中規模ファイルを同じ作法に揃える。
> 温存していた旧パス pub use を一括削除し、use 文を新パスへ置換。モジュール
> ドキュメント整備」。ユーザー判断により **plugin/・driver/ ディレクトリの
> 解体(実体ファイルの移設)まで本 Phase に含める**(spec 目標アーキテクチャの
> 木に plugin/・driver/ が無いことへの追従)。

## 1. 残存ファイルと移設先対応表

Phase 5 完了時点で plugin/ に残る実体は 9 ファイル + manifest/、driver/ は
manifest.rs のみ。移設先の判断根拠つき一覧:

| 現在 | 移設先 | 根拠 |
|---|---|---|
| `plugin/manifest/mod.rs`(867行)+ `tests.rs`(2036行) | `manifest/mod.rs` + `manifest/tests.rs` | spec 目標木のトップレベル `manifest/`(純粋)。中身はそのまま |
| `driver/manifest.rs`(408行、`DriverManifest`/`load_driver_manifest`) | `manifest/driver.rs` | 同じく純粋パース。plugin/driver の manifest を 1 モジュールに同居 |
| `plugin/bus_runtime.rs` / `fs_runtime.rs` / `sidecar_runtime.rs` | `runtime/bus.rs` / `runtime/fs.rs` / `runtime/sidecar.rs`(**新設・純粋**) | 「`HostCtx` と `Registry` が共有するバッファの組み立てと解釈」(各ファイル冒頭)— host(読み手)・registry(書き手)・runner(初期値)の3命令的モジュールが使う中立の値形式。JSON 文字列の組み立て/パースのみで I/O 無し → 純粋 |
| `plugin/dropped.rs`(`DropCounters`/`DroppedCounts`) | `runtime/dropped.rs` | 共有ランタイム状態(取りこぼしカウンタ)。`Arc<AtomicU64>` のみで Mutex/スレッド/ディスク無し(スレッドはテスト内のみ)→ 純粋の判定基準(module-layout.md)を満たす。**`rpc/render.rs` が `DroppedCounts` を使う**ため命令的モジュールには置けない(純粋→命令的 import 禁止) |
| `plugin/filesystem.rs`(706行、`FilesystemConfigStore`) | `settings/filesystem.rs` | `[[filesystem]]` のユーザー設定の永続化と検証 — `SettingsStore`(settings/store.rs)と同種の「検証(純粋)+ ディスク実装を端に」パターン |
| `plugin/sidecar.rs`(428行、`SidecarConfig(Store)`/`assign_ports`) | `settings/sidecar.rs` | 同上(サイドカーのユーザー設定 + ポート採番) |
| `plugin/allowlist.rs`(`check_url`) | `host/allowlist.rs` | driver-http の URL 許可判定(純関数)。唯一の利用者は `host/resolve.rs` — 使う機能のモジュールに置く(trait-di.md の trait 置き場と同じ考え方) |
| `plugin/select_options.rs`(pub(crate)) | `registry/select_options.rs` | `Bus`(チャネル)を触る命令的コード。利用者は `registry/plugin.rs`・`registry/driver.rs` の `list` のみ。manifest/mod.rs からの参照はドキュメントコメントのみ(import 無し — 確認済み) |
| `plugin/mod.rs` / `driver/mod.rs` | **削除**(Task 6) | 移設後は pub use だけの残骸になる |

移設後の core/src: `manifest/ capability/ settings/ schedule/ rpc/ journal/ runtime/`
(純粋)+ `registry/ runner/ host/ server/`(命令的)+ 単独ファイル
(bin/ event logs monitor router status watch lib.rs)。

## 2. 旧パス pub use の棚卸し(Task 6 で一括削除)

| 場所 | 内容 | 導入 Phase |
|---|---|---|
| `plugin/mod.rs` 6/8/11/13/15/19 | grants→capability::grants、host→host::plugin、registry→registry::plugin、runner→runner::plugin、schedule(+schedule_store)→schedule、settings→settings::store | 1/5/4/5/3/3 |
| `driver/mod.rs` 5/8/10 | host→host::driver、registry→registry::driver、runner→runner::driver | 5/4/5 |
| `plugin/manifest/mod.rs` 182 | capability/request.rs へ移動した型の旧パス互換 | 1 |

利用側の規模(機械的 use 置換の対象): `crate::plugin::*` / `crate::driver::*`
が core/src に約 170 箇所、`edlr_core::plugin::*` / `edlr_core::driver::*` が
core/tests に約 35 箇所。**core 外(ui/ drivers/ config/)に edlr_core 旧パスの
利用は無い**(grep 確認済み)。`registry::plugin` / `registry::driver` /
`runner::plugin` 等の新パスは同綴りの別物なので置換時に巻き込まないこと。

spec の規律: この削除コミットは **use 文置換のみ**(テスト凍結の唯一の例外
として tests の use 行書換が許されるコミット)。

## 3. journal の作法揃え(spec の明示項目)

`journal/tailer.rs` 605 行のうち実装は 15–199(約185行)、テストが 202–605
(約400行)。実装は既におおむね規律内だが:

- **テスト分離**: `journal/tailer.rs` → `journal/tailer/mod.rs` + `journal/tailer/tests.rs`(manifest/ と同じ前例。move-only)
- **判定抽出**(logic、procedure-style.md):
  1. ローテーション消失時のフォールバック判定(78–90: 「次ファイルが無いときは、消えたファイルより厳密に新しい latest だけ採る」)→ 純関数 `rotation_fallback(current: &Path, next: Option<PathBuf>, latest: Option<PathBuf>) -> Option<PathBuf>`
  2. バッファからの完全行切り出し(187–196: `partial` からの drain + trim + replay フラグ付け)→ 純関数 `split_complete_lines(buf: String, caught_up: bool) -> (Vec<JournalLine>, String)`(値イン値アウト、戻りは (行, 残り))
  - どちらも既存テスト(凍結)が挙動を守る。抽出後に純粋テストを追加

他の中規模ファイルの判定: `logs.rs`(245)は `filter_from_env_value` 抽出済みで
規律内。`journal/discovery.rs`(109)/`position.rs`(204)/`parser.rs`(64)は
小さく整っている。追加の作法揃えはしない(必要が実証されたら別途)。

## 4. リスク台帳

| # | リスク | 手当て |
|---|---|---|
| 1 | 一括 use 置換で同綴りの新パス(`registry::plugin` 等)を巻き込む | 置換は `crate::plugin::` → 移設先の対応表ベースで機械的に。置換後 `grep -rn "crate::plugin::\|crate::driver::" core/src core/tests`(コメント除く)が 0 件であることをゲートに |
| 2 | `runtime/` を純粋に分類することの妥当性 | dropped.rs は Atomic のみ(禁止リストの Mutex/チャネル/スレッド/ディスク非該当)。bus/fs/sidecar は文字列整形のみ。mod.rs のドキュメントに判定根拠を書く |
| 3 | rpc(純粋)→ dropped の import が移設後も純粋→純粋であること | 移設先が runtime/(純粋)なので維持される。逆に allowlist(host/ へ)は純粋モジュールから import されていない(利用者は host/resolve.rs のみ)ことを確認済み |
| 4 | tailer の判定抽出で replay 境界・ローテーション挙動を壊す | 既存テスト約400行(凍結)+ 統合テストが錨。抽出は判定のみで I/O・状態更新の順序は不変 |
| 5 | manifest 統合で `Manifest` と `DriverManifest` の型パスが変わる | 旧パスは Task 6 まで pub use 温存 → 影響は Task 6 の置換に集約 |
| 6 | テスト凍結の例外運用(Task 6 のみ use 行書換可) | Task 6 のコミットは use 文以外の diff ゼロを multiset 検証。それ以外のタスクは従来どおり凍結 |
| 7 | rules/CLAUDE.md のモジュール表が古くなる | Task 7 で `.claude/rules/module-layout.md` と `CLAUDE.md` の一覧を更新(runtime/ 追加、plugin/・driver/ 削除、manifest/ トップレベル化) |

## 5. タスク系列案(→ 実装計画に反映)

1. [move+logic] journal/tailer: テスト分離(move-only)→ 判定2抽出 + 純粋テスト(logic)の2コミット
2. [move] plugin/manifest → manifest/、driver/manifest.rs → manifest/driver.rs(旧パス pub use 温存)
3. [move] runtime/ 新設: bus_runtime/fs_runtime/sidecar_runtime/dropped → runtime/{bus,fs,sidecar,dropped}.rs(温存)
4. [move] filesystem.rs/sidecar.rs → settings/、allowlist → host/、select_options → registry/(温存)
5. [sweep] 旧パス pub use 一括削除 + 全 use 文置換(src+tests)。plugin/mod.rs・driver/mod.rs 削除。use 文以外の diff ゼロ
6. [docs] モジュールドキュメント整備: 各 mod.rs、コメント内の旧パス表記更新、module-layout.md / CLAUDE.md の表更新
7. 完了ゲート(リファクタ全体の完了確認を含む)
