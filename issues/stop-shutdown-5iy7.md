---
id: stop-shutdown-5iy7
title: サイドカー本体の死後に残った孫プロセスが stop/shutdown で殺されない
summary: drivers/process の stop 経路は child:Some しか killpg しないため、サイドカー本体が先に死ぬと孤児の孫プロセスが shutdown で回収されない(pgid を reap 後も保持すれば直せる)/ 未着手
status: open
labels: bug
created: 2026-08-05T04:40:26Z
updated: 2026-08-05T04:40:48Z
---



## どこで踏んだか

issue daemon-signal-shutdown-integration-works-kay8(flaky テスト)の調査中に
発見。`core/tests/daemon_signal_shutdown_integration.rs` のフィクスチャ
スクリプトがバグ(サブシェル形)で sh 本体が即死しており、その状態で
デーモンを SIGTERM すると、孤児になった孫(`sleep 60`)が誰にも殺されず
生き残ることが実際に観測できた(これが flake の片方の正体)。

## なぜ困るか

`drivers/process` の停止経路(`take_for_stop` → `kill_and_wait_all` の
`killpg(child.id())`)は **`child: Some` のインスタンスしか対象にしない**
(`drivers/process/src/lib.rs` の `take_for_stop`)。サイドカー本体が
クラッシュ等で先に死んで reap されると、その時点でプロセスグループ id
(pgid = 元の子の pid)を参照する手段が失われ、サイドカーが spawn していた
孫プロセスはデーモンの stop/shutdown で回収されない。孤児プロセスが
デーモン終了後も走り続ける。

## 原因

reap(`child.take()` で `None` 化)した後、pgid をどこにも覚えていない。
pid == pgid(spawn 時に `process_group(0)`)なので、reap 前の pid を
インスタンスに残しておけば killpg は依然可能。

## 直し方の案

- `Instance` に `pgid: i32` を spawn 時に記録し、reap 後も保持する。
  `stop`/`stop_all` は `child` の有無に関わらず(terminating でなければ)
  `killpg(pgid, SIGTERM/SIGKILL)` を送る(既に空のグループへの killpg は
  ESRCH で無害)
- 注意: pid 再利用の窓(reap 後に同じ pid が別プロセスへ再割り当て)を
  踏むと無関係のグループへシグナルを送るリスクがあるため、送る条件や
  タイミング(reap からの経過や、デーモン終了時のみ等)は要検討
