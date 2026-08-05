---
id: issue-sizx
title: プラグインの非同期実行を submit/complete プロトコルで導入する
summary: lifecycle 呼び出しの中で I/O を待つ設計をやめ、submit-send / on-job-complete の2フェーズに倒して deadline とキュー詰まりを解消する / WIT 0.5.0 で実装完了(2026-08-05)・残タスクは SDK ヘルパーとドライバ側 submit の別 issue
status: closed
labels: 人間がやる
created: 2026-07-28T14:10:44Z
updated: 2026-08-05T00:37:50Z
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

- **実装ガイド: `docs/submit-complete-guide.md`**(2026-08-04 作成。フェーズ順の
  着手手順。非ブロッキング HTTP は本プロトコル上の `submit-http` として実装する
  ことが確定 — `docs/async-migration.md` Step 2b 参照)
- `core/src/plugin/runner.rs`(専用スレッド、`PluginWork`、`fire_all_due`)
- `core/src/plugin/host.rs`(`CALL_DEADLINE`、`HTTP_TIMEOUT`)
- `core/wit/plugin.wit`
- `docs/superpowers/plans/2026-07-28-plugin-scheduler.md`

## 調査結果(2026-08-01)

**結論: 実現可能。設計案は現行コードにほぼそのまま載る。** 合流点として
想定した `PluginWork` の一本化キューは既に存在し(リファクタ後の現在地は
`core/src/runner/plugin.rs`、ホストは `core/src/host/plugin.rs`)、追加の芯は
「WIT に submit / on-job-complete を足す + 完了の合流経路を足す」だけ。

問題の実在も全て確認済み:

- `PLUGIN_WORK_QUEUE_CAPACITY = 64`、満杯時 try_send 破棄(`DropCounters` 計上)
- `CALL_DEADLINE = 2s` / `CALL_DEADLINE_STRIKES = 3` で Disabled
- `HTTP_TIMEOUT = 1.5s` が const assert で `CALL_DEADLINE` 未満に固定されて
  おり、**1.5 秒超の I/O を伴うプラグインは現状構造的に書けない**

### issue 本文に無かった要決断ポイントと、その決定

1. **WIT export 追加は既存プラグインを全部壊す** —
   ホストは `PluginBindings::instantiate` で world の export を全解決する
   ため、`export on-job-complete` を足すと未実装 wasm は全てロード失敗する。
   → **決定: 互換を捨てて壊してよい**(未リリース・ゲストは examples 10 個
   のみ)。`world plugin` を 0.5.0 に上げ、全ゲストを一斉再生成する。
   `get_typed_func` の手動解決による互換レイヤは作らない。
   MoonBit は wit-bindgen 0.45 ピン(→ moonbit-wit-bindgen-0-45-0-60-na5m)
   のまま再生成できる。

2. **完了通知を捨てない保証** — 64 枠キューはイベント/バス配信と共有で
   満杯時破棄が方針だが、completion を捨てるとゲストは永遠に結果を待つ。
   → **決定: キューは 1 本のまま、満杯時の削除ルールを種別で変える**
   (完了専用キューを分ける案は不採用 -- 2 本にするとプラグインスレッドの
   起床に select が要る)。`std_mpsc::sync_channel` は削除ルールを
   差し替えられない(try_send は常に新入りを捨てるだけ)ので、
   `Mutex<VecDeque<PluginWork>> + Condvar` の小さな自作キューに置き換え、
   受け入れ判定を種別ごとの純関数にする:
   - `Event` / `Message`: 容量 64 超なら新入りを捨てて `DropCounters` 計上
     (今と同じ)
   - `JobComplete`: **常に受け入れる**。completion は submit 時の in-flight
     上限チェック(超過は `queue-full` 即時エラー)で数が抑えられている
     ので、キュー全体の上界は 64 + in-flight 上限で有界のまま
   1 本のキューなのでイベントと完了通知の FIFO 順序も自然に保たれる。
   `recv_timeout` 相当は Condvar の wait_timeout で再現でき、ループ構造・
   `next_action` はそのまま。

3. **tokio Handle の配線**(実装時に必須) — `submit_job` の
   `tokio::spawn` は、ホスト関数がプラグイン専用 OS スレッド上で走るため
   `Handle::current()` では取れない。tokio コンテキスト内で動いている
   `start_plugins` で `Handle` を捕まえ `HostCtx` に持たせる。

4. **インスタンス再作成と job_id の世代管理** — deadline strike からの
   復帰でインスタンスは作り直されるため、旧インスタンスが submit した
   job の完了が新インスタンスに届きうる。job に世代番号を付けて旧世代の
   完了は捨てる。

### 実装時の補足

- `submit(kind: string, payload: list<u8>)` の untyped な形は「暗黙の
  プロトコルドリフト防止」思想と衝突する。`submit-http(request) -> job-id`
  のような型付き submit の方が思想に合う(要検討)。
- HTTP の実行体は現行 `reqwest::blocking`(内部で自前ランタイム生成)。
  job 実行は async client 新設か `spawn_blocking` で既存 client を包むかの
  二択。どちらでも可。
- ドライバ側(`world driver`)の同種の問題は別 issue http-driver-9znv の領分。
- 規模感: WIT + ホスト + runner 配線 + ゲスト SDK/examples 更新 + テストで
  中規模。技術的な阻害要因はなし。

## 実装完了(2026-08-05)

Phase 1〜5 を実装し main へコミット済み(`223d427` キュー自作、`bd89453`
配線、`4c8e7b5` WIT 0.5.0 + submit/complete 本体、以降 docs/ゲスト追従)。

### 未決だった「complete の結果型」の決定

**「submit は型付き・complete は汎用 JSON 文字列」の折衷案を採用**。

- submit は `driver-http.submit-send(request, timeout-ms: option<u32>)
  -> result<u64, driver-error>`(untyped `submit(kind, payload)` は不採用。
  上記「実装時の補足」の思想どおり)
- complete は `on-job-complete(job-id: u64, result-json: string)`。
  形は `{"ok":{"status","headers","body-base64"}}` /
  `{"err":{"kind","message"}}`(body はバイト列なので base64)。
  job 種別が増えても export は増えない

### 実装が決定からずれた点

- 名前は `submit-http` ではなく `submit-send`(interface が既に
  `driver-http` なので冗長を避けた)
- in-flight 上限(8)超過の同期エラーは `queue-full` variant 追加ではなく
  既存 `transport` にメッセージで載せた(driver-error の variant 追加は
  もう一段の ABI 変更になるため)
- HTTP 実行体は async client(Step 2a で導入済みの `reqwest::Client`)+
  リクエスト単位タイムアウト上書き(既定 30s・上限 60s クランプ)

### 残タスク(別 issue)

- ゲスト SDK の job-id → await ヘルパー: sdk-send-async-response-await-lvn3
- ドライバ側 submit(現状 world driver の trait 上は invalid-request の
  stub): http-driver-9znv
