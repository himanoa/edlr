---
id: stop-shutdown-5iy7
title: サイドカー本体の死後に残った孫プロセスが stop/shutdown で殺されない
summary: drivers/process が pgid を reap 後も保持し、stop/stop_all/respawn/align の全経路で本体死亡後の孤児孫プロセスを killpg で回収するよう修正済み(回帰テスト3本)
status: closed
labels: bug
created: 2026-08-05T04:40:26Z
updated: 2026-08-05T05:08:38Z
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

## 修正(2026-08-05)

案どおり `Instance` に `pgid` を追加(spawn 時に記録、reap 後も保持)し、
回収経路を 3 つとも塞いだ:

- **stop / stop_all**: `take_for_stop` が `StopBatch`(生きている子 +
  孤児グループの pgid)を返し、`kill_and_wait_all` が孤児グループへも
  SIGTERM → 猶予中 `killpg(pgid, 0)` で空を待つ → 残れば SIGKILL を行う
- **respawn**(`ensure_started`): 旧 pgid のグループを SIGKILL してから
  新しい世代を spawn
- **align_instances**(ポート構成変更での作り直し): `terminate` が
  child 無しでも旧 pgid を SIGKILL

pid 再利用の安全性: POSIX/Linux では生きたプロセスグループの id は新しい
pid として再利用されないため、グループに孫が残っている限り killpg は必ず
自分のグループに当たる。空になった後は ESRCH で無害。さらに `reap` が
グループの空を観測した時点で pgid を破棄し、stale な値を持ち続けない。

回帰テスト 3 本(stop_all / stop / respawn の各経路で「本体死亡後の孫が
回収される」)を drivers/process に追加。workspace 全テスト + clippy 全パス。
