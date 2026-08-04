# submit/complete プロトコル実装ガイド(issue-sizx)

issue-sizx を人間の手で実装するための着手順。2026-08-04 時点のコード
(async-migration Step 2a 完了後 = `HttpDriver` は async Client + `Handle`)を
基準にしている。設計の背景・決定の経緯は issue-sizx 本文を正とし、ここでは
「どの順で・どのファイルを・どう変えるか」だけを書く。

## 先に読むもの

- `git issues show issue-sizx` — 問題の実在確認と決定済み事項(特に「調査結果」節)
- `docs/async-migration.md` Step 2b と「移行中に壊しやすい不変条件」
- `.claude/rules/` 一式(純粋/命令的境界・procedure-style・testing)

## 決定済み事項(再掲。実装中に迷ったらここへ戻る)

1. ABI 互換は捨てる。`world plugin` を **0.5.0** に上げ、全ゲストを一斉再生成
2. キューは 1 本のまま。`std_mpsc::sync_channel` を
   `Mutex<VecDeque<PluginWork>> + Condvar` の自作キューに置き換え、
   受け入れ判定を**種別ごとの純関数**にする
   - `Event` / `Message`: 容量 64 超は新入りを捨てて `DropCounters` 計上(現状どおり)
   - `JobComplete`: 常に受け入れる(上界は 64 + in-flight 上限で有界)
3. submit は untyped の `submit(kind, payload)` ではなく**型付き**
   (HTTP なら `submit-http(request, ...) -> result<job-id, driver-error>`)
4. 完了通知は既存プラグイン専用スレッドの受信ループに `PluginWork::JobComplete`
   として合流(actor 化はしない。`Store` を 1 スレッドから出さない)
5. 同期 `driver-http.send` は残す。「lifecycle 内で待つのは自己責任」を docs に明記
6. インスタンス再作成に備え、job に**世代番号**を付けて旧世代の完了は捨てる
7. `tokio::spawn` 用の `Handle` は tokio コンテキスト内(`start_plugins` の
   呼び出し元)で捕まえて配る。Step 2a で `PluginHost::new(handle)` になって
   いるので、そこから `HostCtx` へ流すだけでよい

## 実装フェーズ

依存関係の順。各フェーズ末尾の検証が通ってから次へ進むこと。

### Phase 1: work queue の自作化(WIT に触らない準備工事)

**対象**: `core/src/runner/plugin/`(分割済み: `mod.rs` = 共有の
`PluginWork`/容量定数、`start.rs` = ロード・起動、`event_loop.rs` =
受信ループ、`subscriber.rs` = 購読タスク)

- 現状: `std_mpsc::sync_channel::<PluginWork>(PLUGIN_WORK_QUEUE_CAPACITY)`
  (`start.rs:210`)。受信ループは `work_rx.recv_timeout(timeout)`
  (`event_loop.rs:267`)でブロックし、`next_action`/`LoopAction`
  (`event_loop.rs:382` 付近の純関数)が次の動作を決める。
- やること:
  1. 受け入れ判定を純関数で書く:
     `fn admit(queue_len: usize, work: &PluginWork) -> Admit`
     (`Accept` / `DropNewest` の 2 値で足りる)。まずこの関数と
     テストだけ書く(値イン値アウト。→ testing.md)
  2. `Mutex<VecDeque<PluginWork>> + Condvar` の小さなキューを実装する。
     公開面は今の channel と同じ 3 操作に揃えると差し替えが機械的になる:
     `push(work) -> Result<(), Dropped>`(admit を内側で呼ぶ)/
     `recv_timeout(dur)` (Condvar の `wait_timeout` で再現)/ sender 側 clone。
     `Delivery` 側の channel(`start.rs:304`)は当面そのまま
  3. `try_send` 呼び出し箇所(`subscriber.rs` の両購読タスク)を新キューの
     `push` に置換。drop 時の `DropCounters` 計上
     (`mod.rs` の `PLUGIN_WORK_QUEUE_CAPACITY` ドキュメント参照)は
     今と同じ場所で行う
- **触ってはいけないもの**: `next_action` / `LoopAction` の構造、
  `recv_timeout` ベースのループ構造(→ async-migration.md 不変条件 1)。
  bus subscriber の `recv_timeout(200ms)` + `AtomicBool` も見た目が似ているが
  別物。混ぜない。
- 検証: `cargo test -p edlr-core`(特に
  `daemon_signal_shutdown_integration.rs` — SIGTERM ハングの回帰検出)。
  この時点で挙動は完全に現状維持のはず。

### Phase 2: WIT 0.5.0(submit-http + on-job-complete)

**対象**: `core/wit/plugin.wit`

- `package edlr:plugin@0.4.0;` → `@0.5.0`
- `interface driver-http` に追加(型付き submit。決定 3):
  ```wit
  // 即 return する。ホストは中で await しない。
  // timeout-ms: none なら 30_000。上限 60_000 にクランプ。
  submit-http: func(req: request, timeout-ms: option<u32>) -> result<u64, driver-error>;
  ```
- `world plugin`(`:119`)の export に追加:
  ```wit
  // submit 系の結果は別 export で非同期に届く。呼ばれる時点で結果は
  // 揃っているので、これも普通の同期呼び出しでよい。
  export on-job-complete: func(job-id: u64, result-json: string);
  ```
  - **要決断(未決)**: 結果の型。issue の原案は `list<u8>`、上の例は
    JSON 文字列。HTTP 専用の typed record にすると job 種別が増えるたびに
    export が増えるので、「submit は型付き・complete は汎用ペイロード」が
    折衷案。決めたら issue-sizx に追記すること
- `world driver` には**足さない**(ドライバ側は http-driver-9znv の領分)
- この時点で core はコンパイルが通らなくなる(bindgen が新 export の
  実装を要求する)。Phase 3・4 を終えるまで戻れないので、ここから先は
  一息にやるか、WIT 変更を最後に回して 3・4 を trait/型だけ先行させるか、
  好みで選ぶ
- 追従が必要なもの:
  - `core/tests/wit_version_docs_sync.rs`(バージョン表記の同期テスト)
  - docs のバージョン記載(`docs/plugins.md` ほか。テストが教えてくれる)

### Phase 3: ホスト側 submit-http(`HostCtx`)

**対象**: `core/src/host/plugin.rs`、`core/src/runner/plugin/start.rs`(配線元)

- `HostCtx` に足すもの:
  - work queue の送信側クローン(Phase 1 の自作キュー)
  - `tokio::runtime::Handle`(`PluginHost` 経由で受け取る。決定 7)
  - job id カウンタ(`AtomicU64`、1 始まり)+ **世代番号**(決定 6)
  - in-flight カウンタと上限(**8/インスタンス**。旧 send-async 設計から引き継ぎ)
- `submit_http` 実装(bindgen が生やす trait メソッド):
  1. permission チェック: 既存 `check_http_permission`
     (`core/src/host/resolve.rs`)を同期 `send` と同じ位置で呼ぶ。
     拒否は即 `Err(permission-denied)`
  2. in-flight 上限チェック。超過は即 `Err`(transport にキュー満杯の旨。
     ブロックしない。→ issue の backpressure 決定)
  3. job_id 採番 → `handle.spawn(async { ... })` で
     `HttpDriver` の async 送信(2a で入れた async Client をそのまま使う。
     `block_on` しないこと)→ 完了したら `(世代, job_id, 結果)` を
     work queue へ push
  4. 即 `Ok(job_id)` を返す
  - タイムアウト: `timeout-ms` 引数(既定 30s、上限 60s クランプ)。
    同期 send の `HTTP_TIMEOUT`(1.5s)< `CALL_DEADLINE` の const assert は
    **submit には適用されない**(呼び出し自体は即返るため)。ただし
    レスポンスサイズ上限(`HTTP_MAX_BODY`)は同期側と同値を適用
- 結果 → ペイロード(JSON なり `list<u8>` なり)への整形は
  **値イン値アウトの純粋関数**に切り出してテストを書く(→ rules)

### Phase 4: runner 配線(JobComplete の消費)

**対象**: `core/src/runner/plugin/mod.rs`(`PluginWork`)、
`core/src/runner/plugin/event_loop.rs`(受信ループ)

- `PluginWork` に `JobComplete { generation, job_id, payload }` を追加
- `next_action` に腕を足し、`LoopAction` 経由で
  `instance.call_on_job_complete(...)` を呼ぶ(`fire_all_due` と同列の扱い。
  epoch deadline も他の export 呼び出しと同じに掛かる)
- **世代チェック**: インスタンス再作成(deadline strike 復帰)時に世代を
  インクリメントし、古い世代の `JobComplete` は呼ばずに捨てる(決定 6)。
  捨てたことは debug ログに残す
- 検証: `cargo test -p edlr-core`。加えて統合テストを 1 本足す:
  `submit-http → on-job-complete 到着`(`core/tests/driver_http_integration.rs`
  の axum テストサーバの流儀を流用)。「trap で再作成された後、旧 job の
  完了が届かない」も純関数レベル(世代比較)でテストする

### Phase 5: ゲスト再生成と docs

- `examples/plugins/` 全 10 個 + `examples/drivers/` のうち plugin world を
  使うものを再生成・再ビルド(ドライバ world は無変更なので原則不要)。
  MoonBit は wit-bindgen **0.45 ピンのまま**再生成する
  (→ issue moonbit-wit-bindgen-0-45-0-60-na5m)
- `examples/plugins/http-caller` に submit-http の使用例を追加
- `docs/plugins.md`: submit-http / on-job-complete の API とセマンティクス
  (順序保証は 1 本キューの FIFO・in-flight 上限 8・タイムアウト規定・
  旧世代破棄)、および「同期 send を lifecycle 内で待つのは自己責任」
  (決定 5)を明記
- `docs/async-migration.md` Step 2b の状態を更新
- チュートリアル 3 言語の本文更新は任意(export が増えるので、実際には
  再生成手順に触れる箇所だけ要確認)

## 壊しやすいポイント(async-migration.md の不変条件との対応)

- **SIGTERM ハング**(不変条件 1): Phase 1 のキュー差し替えで
  `recv_timeout` 相当を必ず維持。`daemon_signal_shutdown_integration.rs` が検出
- **バックプレッシャ禁止**(不変条件 3): completion の push も
  ノンブロッキング(`JobComplete` は常時受け入れなので実質ブロックしない)
- **`Handle` の取り方**: プラグインスレッド上で `Handle::current()` は
  panic する。必ず注入(決定 7)
- **`block_on` をランタイムスレッドで呼ばない**: submit の spawn 内は
  async のまま完結させる。同期 `send` の `block_on` は
  プラグイン/ドライバスレッド専用
- **デーモン終了時の残タスク**: spawn 済み job は `Runtime::drop` で
  abort されるだけでよい(旧設計の決定を引き継ぐ)。try_send(push)失敗は
  静かに破棄

## 完了条件

- `cargo test`(workspace)+ `cargo clippy --all-targets -- -D warnings` が通る
- SIGTERM でデーモンが grace 内に終了する(手動確認)
- 上記の統合テスト(submit → complete 到着、旧世代破棄)が入っている
- issue-sizx を close し、未決だった「complete の結果型」の決定を本文に追記
- 後続: `docs/async-migration.md` Step 2b は本プロトコル上の実装として
  ほぼ完了扱いになるはず。SDK ヘルパーは issue
  sdk-send-async-response-await-lvn3 へ
