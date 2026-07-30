---
id: rules-capability-grants-rs-i-o-manifest-99dq
title: rules 文書と実装の乖離: capability/ の純粋分類と grants.rs の I/O・manifest との相互参照
summary: Phase 1 で grants.rs(std::fs/Mutex 使用)が純粋分類の capability/ 配下へ移動し .claude/rules/ の記述と矛盾。また validate.rs → manifest::ManifestError の逆辺があり Phase 6 で解消要 / 未着手
status: open
labels: docs, refactor
created: 2026-07-30T13:08:14Z
updated: 2026-07-30T15:36:29Z
---

## どこで踏んだか

core リファクタ Phase 1(plan: `docs/superpowers/plans/2026-07-30-core-refactor-phase0-1.md`)の
最終レビューで2点の「plan と rules の緊張」が見つかった。いずれも実装の欠陥ではなく、
plan/spec が明示的に指示した構成が rules 文書の記述と食い違っている。

1. **capability/ の純粋分類 vs grants.rs の I/O**
   `.claude/rules/module-layout.md` は capability/ を純粋モジュールに分類し、
   `pure-imperative-boundary.md` は純粋モジュールでの `std::fs`/`Mutex` を禁じているが、
   Task 8 で移動した `core/src/capability/grants.rs` は両方を使う
   (grants.rs:2,4,26 — ディスク永続化ストア)。spec は「grants ストアは trait の隣に置く」
   構成を明示しているので、rules 側の追記が必要。Phase 3 で `SettingsStore` を
   settings/ へ動かすときも同じ緊張が再発する。

2. **manifest ⇄ capability の逆辺**
   `core/src/capability/validate.rs:5` が `crate::plugin::manifest::{is_valid_id, ManifestError}` を
   逆参照している(plan が「ManifestError は manifest 側に据え置き」と明示)。
   rules の依存方向(manifest → capability)に対する back-edge であり、Phase 6 で
   ManifestError を動かして解消する想定。ただし ManifestError の `Display` 文字列は
   挙動凍結対象なので、移動時は表示文字列を1バイトも変えないこと。

## なぜ困るか

rules は core を触るエージェント・人間の必読文書なので、実装と矛盾したままだと
「rules に従って grants.rs を capability/ から追い出す」ような誤った修正を誘発する。

## 直し方

- 短期: `module-layout.md` に「capability の grants ディスクストアは公認の例外
  (manifest::load_manifest の "I/O は端に" と同格)」と追記。settings も同様の注記を予約。
- 長期: Phase 6 の計画に ManifestError の所属整理(Display 文字列凍結のまま)を含める。

## 追記(2026-07-30, Phase 2)

Phase 2 でも同じ公認例外を1件追加: `core/src/rpc/render.rs` が
`crate::plugin::registry` 配下のデータ型(`BusInfo`/`DashboardInfo`/
`ScheduleInfo` 等)を import する。rpc/ は純粋モジュールだが、これは
値型のみの参照で副作用はない。型の所属整理は Phase 4 で行う。

## 追記(2026-07-31, Phase 3)

Phase 3 で `core/src/settings/store.rs`(SettingsStore、std::fs + Mutex)も
純粋分類の settings/ 配下へ移動した。grants.rs と同じ公認例外
(spec が「Storage trait + ディスク実装」を同モジュールに置く構成を明示)。

## 追記(2026-07-31, Phase 3 最終レビュー)

逆辺をもう1本記録: `settings/validate.rs`(純)が
`settings/store.rs`(I/O 側)から `SettingsError` を import している
(plan が指示した形)。Phase 6 で `SettingsError` を settings/mod.rs 側へ
移す(または検証系 variant と `Io` を分離する)ことを検討。`Display`
文字列は凍結のまま。あわせて新規テストが旧互換パス
(`crate::plugin::grants::GrantState` 等)を参照している箇所も Phase 6 の
旧パス削除時に正規パスへ置換する。
