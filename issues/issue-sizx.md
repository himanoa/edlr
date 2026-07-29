---
id: issue-sizx
title: プラグインの非同期実行を submit/complete プロトコルで導入する
summary: lifecycle 呼び出しの中で I/O を待つ設計をやめ、submit_job / on-job-complete の2フェーズに倒して deadline とキュー詰まりを解消する / 未着手
status: open
labels: 人間がやる
created: 2026-07-28T14:10:44Z
updated: 2026-07-29T16:02:55Z
---


## 何が困っているか

プラグインの lifecycle export(`on-event` / `on-message` / `on-schedule`)は、
そのハンドラの中で `driver-http.send` などの I/O を**同期的に待てる**。
待っている間、そのプラグイン専用スレッド(`core/src/plugin/runner.rs`)は
次の仕事を一切引けない。結果として:

- `CALL_DEADLINE`(2 秒)を超えると epoch 割り込みで trap し、
  `CALL_DEADLINE_STRIKES` 連続でプラグインが無効化される。
  ホスト側が単に応答しないだけでプラグインが殺される。
- 待っている間 `work_rx` を読まないので `PLUGIN_WORK_QUEUE_CAPACITY`(64)が
  詰まり、journal イベントやバス配信が捨てられる
  (`plugin::dropped::DropCounters` に計上される)。
- `on-schedule` の発火が I/O の完了までまとめて遅延する(`fire_all_due`)。

たとえば COEIROINK に喋らせるプラグインのように、1 回の処理が数百 ms〜数秒
かかる I/O を含むものは、この設計のままではまともに書けない。

## wasmtime の `async_support` は解にならない

一見するとホスト側を非同期実行にすれば解決しそうだが、`async_support` が
解決するのは「**ホストスレッドがブロックされること**」だけで、
**その呼び出し自体のレイテンシは 1 ミリ秒も縮まない**。呼び出し元が戻りを
待っている限り RTT はそのまま乗るので、上に挙げた deadline / キュー詰まり /
発火遅延はどれも消えない。

直すべきは実行方式ではなく**プロトコル**、つまり
「lifecycle call が結果を待たずに即返る設計に変える」ことである。

## 方針: submit / complete の2フェーズ化

I/O を「投げる」と「結果を受け取る」に分割し、その境界を driver 層の
インターフェースとして固定する。

### ゲスト側から見た形(WIT の追加案)

```wit
interface driver-jobs {
  // 即 return する。ホストは中で await しない
  submit: func(kind: string, payload: list<u8>) -> result<u64, driver-error>;
}

world plugin {
  import driver-jobs;
  // 結果は別 export で非同期に届く。呼ばれる時点で結果は揃っているので
  // これも普通の同期呼び出しでよい
  export on-job-complete: func(job-id: u64, result: list<u8>);
}
```

### ホスト側の実装

```rust
// import: spawn して手放すだけなので Linker::func_wrap の同期関数でよい
fn submit_job(kind: &str, payload: Vec<u8>) -> u64 {
    let job_id = next_job_id();
    tokio::spawn(async move {
        let result = run_job(kind, payload).await;   // 実際の I/O
        completion_tx.send((job_id, result)).await;
    });
    job_id
}
```

完了は既存のプラグイン専用スレッドの受信ループに `PluginWork::JobComplete`
として合流させ、イベント/バス配信と同じ 1 本のキューで直列化する
(`PluginInstance` を 1 スレッドの外に出さない現在の性質を保つ)。

これで:

- lifecycle handler は submit して即 return するだけになり、処理時間予算を守れる。
- 実際の I/O 時間が critical path から完全に外れる。
- `async_support` も fiber 機構も不要。普通の同期 import / 同期 export で足りる。

## 「非同期エンジン」は既にある

libuv 相当のもの(reactor + タイマー + ブロッキング処理を逃がす thread pool +
完了イベントのキュー)は tokio という形で既にホストに入っている。
Node.js の「JS シングルスレッド + libuv が裏で I/O を回して callback queue に
積む」構造と、上の submit/complete はそのまま対応する:

| Node.js | edlr |
|---|---|
| `fetch()` を呼んだ瞬間 | `submit` |
| libuv の reactor | tokio |
| callback queue → callback 実行 | completion queue → `on-job-complete` |

足りないのは新しい async engine ではなく、**各プラグインインスタンスを
そのイベントループにどう配線するか**の部分だけ。

## 設計上の検討事項

- **順序保証**: 音声再生順のように順序が要るものがある。completion queue を
  投入順の FIFO にするか、ホスト側でシーケンス番号を振ってゲスト側で並べ替えるか。
  「待って一列に処理する」責務をホスト側の薄いキューに寄せておけば、
  ゲストは常にノンブロッキングでいられる。
- **backpressure**: submit の未完了数に上限を設ける。上限超過は
  `queue-full` 相当のエラーで即座にゲストへ返す(ブロックしない)。
  既存の `DropCounters` と同じく観測可能にすること。
- **Store/Instance のライフタイム**: 完了通知の合流先を現在の専用スレッドの
  ままにするか、`1 tokio task = 1 プラグインのイベントループ`(actor 型)へ
  作り替えるか。後者は `Store` を `Send` にする必要がある。
  まずは前者(既存スレッドに `PluginWork` を 1 種類足すだけ)で十分なはず。
- **既存 API との互換**: 同期の `driver-http.send` を残すか消すか。
  残すなら「lifecycle の中で待つのは自己責任」という契約を
  ドキュメントに明記する必要がある。
- **`async_support` の立ち位置**: submit/complete 化の後でも
  「complete 待ちの間ホストスレッドを他プラグインに使いたい」という
  ホスト内部最適化としては有効。ただし本 issue の主題ではない。

## edlr の設計思想との相性

driver 抽象化の狙い(暗黙のプロトコルドリフト防止)とむしろ噛み合う。
「blocking で暗黙に待つ」という契約ではなく、`submit` / `complete` という
明示的な2フェーズプロトコルを driver 層のインターフェースとして固定できる。

## 関連

- `core/src/plugin/runner.rs`(専用スレッド、`PluginWork`、`fire_all_due`)
- `core/src/plugin/host.rs`(`CALL_DEADLINE`、`HTTP_TIMEOUT`)
- `core/wit/plugin.wit`
- `docs/superpowers/plans/2026-07-28-plugin-scheduler.md`
