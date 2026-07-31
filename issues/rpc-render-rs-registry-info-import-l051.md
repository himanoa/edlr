---
id: rpc-render-rs-registry-info-import-l051
title: rpc/render.rs が registry の *Info 型を import している(純粋→命令的の境界違反)
summary: rpc/render.rs(純粋)が registry(命令的)の *Info 値型を import している既存負債。値型の純粋側移設か rules の明文化で解消 / 未着手
status: open
labels: refactor
created: 2026-07-31T03:23:13Z
updated: 2026-07-31T03:23:39Z
---



## どこで踏んだか

Phase 6 最終レビュー(2026-07-31)で検出。`core/src/rpc/render.rs` の 27/46/87/105/134/161 行付近が
`crate::registry::plugin::{DashboardInfo, BusInfo, ScheduleInfo, SidecarInfo, FilesystemInfo}` を import している。

## なぜ困るか

`.claude/rules/pure-imperative-boundary.md` は純粋モジュール(rpc/)から命令的モジュール(registry/)の
import を禁止している。Phase 6 以前(旧 `crate::plugin::registry::*Info` 時代)からの既存負債で、
sweep はパス改名しただけ。値型の import なので実害は薄いが、rpc/ を値イン値アウトに保つ規約の穴になる。

## 直し方の案

- `*Info` 値型(表示用スナップショット)を純粋側へ移す — rpc/ 自身に置く案と、runtime/ 相当の
  中立純粋モジュールに置く案がある。registry 側は組み立てるだけにする
- あるいは rules 側で「命令的モジュールの**値型のみ**は純粋から import 可」と明文化する
  (issue 99dq『rules 文書と実装の乖離』と合わせて判断するのがよい)
