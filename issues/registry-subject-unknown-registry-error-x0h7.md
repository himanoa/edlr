---
id: registry-subject-unknown-registry-error-x0h7
title: registry subject の unknown_registry_error 文字列 dispatch と list() の clone 増(kgc6 追い残件)
summary: 
status: open
labels: refactor
created: 2026-07-31T06:32:56Z
updated: 2026-07-31T06:32:56Z
---



## どこで踏んだか

リファクタ起票 issue 一掃(2026-07-31、issue kgc6 の解消コミット e918194)の最終レビューで検出した2点。

1. `core/src/registry/subject.rs` の `unknown_registry_error` デフォルト実装が
   `subject_noun() == "driver"` の**文字列比較**で dispatch し、未知の noun は暗黙に
   `UnknownPlugin` へ fallback する。現行2 impl はテストで pin 済みだが、第3の
   Subject を追加したとき無言で誤った variant に落ちる罠。
2. facade `list()` の service 経由化(kgc6 残件2)で `as_settings_manifest` の
   Manifest deep clone が増えた(plugin 0→2 / driver 1→2)。list() は RPC 単発
   経路なので非ホットだが、subject.rs の容認コメントが書かれた時点より悪化している。

## 直し方の案

1. `unknown_registry_error` を required method 化(plugin/driver の2 impl、各1行)して
   silent fallback を消す。
2. ホットと実証されたら `SettingsService::effective_for` 等に `_with_manifest` 口を
   足して list() で射影を1回に共有する(それまでは容認)。
