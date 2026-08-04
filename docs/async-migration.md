# 非同期化の着手ガイド

edlr を段階的に非同期化するときの、着手順と根拠のメモ。2026-08 時点のコードを基準にしている。

## 現状の構造(前提)

設計は意図的に **async シェル / sync コア**になっている。

- tokio が動いているのは 3 箇所だけ: axum サーバ(`core/src/server/mod.rs`)、journal 監視ループ(`core/src/monitor.rs`)、イベント配送(`core/src/router.rs` + `runner/plugin.rs` の subscriber)。
- wasm・プロセス・ディスクに触る部分はすべて同期で、専用 `std::thread` 上にいる:
  - プラグインごとに 1 スレッド(`core/src/runner/plugin.rs:361`)
  - ドライバごとに 1 スレッド(`core/src/runner/driver.rs:227`)
  - sidecar の ready 監視・stdout/stderr 転送(`drivers/process/src/lib.rs:527,654`)
- 橋渡しは `spawn_blocking` と `std::sync::mpsc`。`block_on` は本番コードに存在しない。

この構造には理由がある: wasmtime の `Store`(= `PluginInstance`)は `Send` にしない前提で
1 プラグイン 1 スレッドに閉じ込めている。これを崩す変更(プラグイン呼び出し自体の async 化)は
最後まで手を付けないこと。

## 着手順

### Step 1: monitor の blocking fs を spawn_blocking に包む(最小・確実)

**唯一の「tokio ワーカー上で blocking I/O をしている」違反箇所。** ここから始める。

- `core/src/monitor.rs:36` の `tailer.poll()` と `:63` 付近の `status.poll()` は
  `std::fs::File::open` / `read_to_string` / ディレクトリスキャン
  (`core/src/journal/tailer/mod.rs:165,183`、`core/src/journal/discovery.rs`、`core/src/status.rs:16`)
  を tokio ワーカー上で直接実行している。
- `tokio::task::spawn_blocking` で包むだけ。tailer / status オブジェクトの所有権を
  クロージャに move して戻す形にする(`Option::take` か、poll 専用の構造にまとめる)。
- position 永続化(`core/src/journal/position.rs:55,63`)も同じタスク上なので一緒に包まれる。

これで定常状態の違反はゼロになる。以降の Step は「改善」であって「修正」ではない。

### Step 2: drivers/http の非同期化 + プラグイン向け非ブロッキング HTTP API

設計は議論済み(2026-08-04)。以下が確定した仕様。手で移植するとき用に
実装順も含めて書いておく。

**前提となる事実**: ホスト内部をいくら async にしても、ゲスト(wasm)から見た
`driver-http.send` は同期呼び出しのまま(素の同期 export/import + epoch 割り込み
の設計であり、component model の async ABI は使っていない)。ゲストに
非ブロッキング HTTP を提供するには API の形を変える必要がある。そこで
コールバック型の `send-async` を**追加**する(既存 `send` は残す)。

#### 2a. WIT(非破壊)

`core/wit/plugin.wit` の `interface driver-http` に関数を 1 つ足すだけ:

```wit
// timeout-ms: none なら 30_000。上限 60_000 にクランプ。
send-async: func(req: request, timeout-ms: option<u32>) -> result<u64, driver-error>;
```

- **触ってはいけないもの**: 既存の `request`/`response` record、`driver-error`
  variant、export 群。record へのフィールド追加も variant へのケース追加も
  構造的型が変わって既存コンパイル済みゲストが壊れる。関数(import)の追加
  だけは非破壊。だから timeout は record ではなく引数、エラーは既存の
  `transport(...)` に載せる。バージョンも `@0.4.0` のまま。
- 対象は plugin world / driver world 両方(どちらも `driver-http` を import
  しているので WIT 変更は上の 1 行で両方に効く)。
- 返り値 `u64` はインスタンスごとの連番リクエスト ID(AtomicU64、1 始まり)。
- permission チェック(`check_http_permission`、`core/src/host/plugin.rs:357`)と
  in-flight 上限(8/インスタンス)の判定は `send-async` 内で同期に行い、
  その場で `result` として返す。

#### 2b. レスポンス配送(既存 on-message に乗せる)

新 export は追加しない(world の export は必須なので、追加すると既存ゲストが
ロード不能になる)。既存の `on-message` に予約ドライバ名 `"http"` で配送する:

- plugin: `on-message(driver: "http", topic: "response", payload: <JSON>)`
- driver: `on-message(from: "http", topic: "response", payload: <JSON>)`
- payload(body は base64):
  - 成功 `{"id": 42, "ok": {"status": 200, "headers": [["k","v"]], "body_b64": "..."}}`
  - 失敗 `{"id": 43, "err": {"kind": "transport", "message": "timeout"}}`
- `"http"` はドライバ ID として予約し、manifest 検証で実ドライバが名乗れない
  ようにする(検証 + テスト)。
- payload の組み立て(response/error → JSON)は値イン値アウトの純粋関数に
  切り出す(`.claude/rules/` の純粋/命令的境界に従う)。

#### 2c. ホスト側

- `drivers/http`: `reqwest::blocking::Client` → async `reqwest::Client` +
  `tokio::runtime::Handle` 保持に置換。
  - 既存の同期 `send` は `handle.block_on(...)` 実装に変える。呼び出し元は
    プラグイン/ドライバスレッド(非ランタイムスレッド)なので合法。
    タイムアウト 1.5s/25s(`core/src/host/plugin.rs:92`、`driver.rs:55`)と
    epoch `CALL_DEADLINE` の const assert は現状維持。
  - `send_async` は Future を返し、呼び出し元が `handle.spawn` する。
  - blocking Client 生成のスレッドハック(`drivers/http/src/lib.rs:109`)は削除。
- `HostCtx`(plugin/driver 両方)に追加:
  - 自分の work queue の送信側クローン
    (plugin: `SyncSender<PluginWork>`(`core/src/runner/plugin.rs:355`)、
    driver: `SyncSender<Message>`(`core/src/runner/driver.rs:213`))
  - `Handle`、リクエスト ID カウンタ、in-flight カウンタ
- 完了したタスクは合成した Delivery/Message(driver=`"http"`)を **`try_send`**
  で積む。Bus は経由しない。レスポンスサイズ上限は同期 send と同じ。

#### 2d. エラー・シャットダウン方針

- queue full: 既存イベントと同じく drop 記録 + warn。ゲストは自前タイムアウトで
  諦める前提(ドキュメントに明記)。`send().await` によるバックプレッシャは
  導入しない(不変条件 3 参照)。
- プラグイン停止/trap 後に完了したレスポンス: try_send 失敗 → 静かに破棄。
- デーモン終了: spawn タスクは `Runtime::drop` で abort されるだけ。
  新しいシャットダウン不変条件は増えない。
- shutdown で abandon された残留スレッドが同期 `send` を呼ぶと
  `Handle::block_on` が panic しうるが、終了間際のデタッチ済みスレッドなので許容。

#### 2e. 手で移植するときの順序

1. `drivers/http` を async Client + `Handle` 化し、同期 `send` を `block_on` で
   維持(既存テストが通ることを確認)。スレッドハック削除。
2. payload JSON 組み立ての純粋関数 + unit テスト。
3. WIT に `send-async` 追加 → ホスト側 bindgen 追従、plugin 側 `HostCtx` 配線
   (ID 採番・in-flight 上限・spawn・try_send)。
4. driver 側 `HostCtx` に同じ配線。
5. manifest の予約名 `"http"` 拒否 + テスト。
6. 統合テスト: `core/tests/driver_http_integration.rs` を拡張し、
   `send-async` → `on-message` 到着を plugin/driver 両方で検証。
7. examples(`http-caller`)に使用例追加、`docs/plugins.md` / `docs/drivers.md` に
   API とセマンティクス(順序保証なし・drop されうる・ID 相関)を追記。
   チュートリアル 3 言語の本文更新は任意(import 追加なので既存生成物は壊れない)。

### Step 3: drivers/process の監視スレッドを tokio 化

sidecar 1 個につきスレッドが 3 本(ready 監視 + stdout/stderr 転送)生えている。

- `watch_ready`(`drivers/process/src/lib.rs:527`): 200ms ごとの `TcpStream::connect_timeout`
  ポーリング → `tokio::net::TcpStream` + `tokio::time::interval` のタスクにできる。
- `forward_output`(`:654`): blocking `BufReader::lines()` →
  `tokio::process` の `ChildStdout` + `AsyncBufReadExt::lines` にできる。
- ただし `std::process::Command` → `tokio::process::Command` に変えると
  `killpg` / `Child::wait` / detached-stop(`:336`)/ grace 付き `stop_all` の
  作り直しが必要。tokio の `process` feature 追加も必要(`core/Cargo.toml:15` には現状ない)。
- crate が tokio 非依存であることが崩れるので、core 側に監視部分だけ引き上げる案も検討。

### Step 4: DI trait の async 化(必要になったときだけ)

sync シグネチャの trait は 4 つ(`.claude/rules/trait-di.md` 準拠、ジェネリクス消費なので
`async fn in trait` は boxing なしで通る):

| trait | 定義 | 判断 |
|---|---|---|
| `registry::ProcessControl` | `core/src/registry/mod.rs:26-35` | async 候補筆頭。`stop`/`stop_all` が grace 3 秒ブロック。ただし Step 3 とセット |
| `settings::Storage` | `core/src/settings/mod.rs:61-77` | `update_and_effective` が「1 ロック区間で両方やる」契約。async 化すると崩れるので据え置き推奨 |
| `capability::GrantStorage` | `core/src/capability/mod.rs:19-50` | 同上のファイル read-modify-write。据え置き推奨 |
| `registry::BusPort` | `core/src/registry/mod.rs:62-64` | 非ブロッキング。sync のまま |

trait を async 化する場合、呼び出し元の多くがプラグイン/ドライバスレッド
(ランタイムハンドルなし)である点に注意。`Handle` を配るか、呼び出し元ごと async 化するか、
先に決めること。

### やらないこと(明示)

- **wasm 呼び出しの async 化**: `PluginInstance::call_*` / `DriverInstance::call_*`
  (`core/src/host/driver.rs:550,558`)を async にするには `Store<T>: Send` が必要で、
  1 プラグイン 1 スレッド設計の存在理由そのものと衝突する。全面再設計になるのでやらない。
- **store 系 Mutex の tokio::sync::Mutex 化**: `settings/store.rs` 等の
  `Mutex<()>` は「read + write + rename を 1 区間で」の直列化。呼び出し元が
  sync スレッドか spawn_blocking 内である限り現状が正しい。

## 移行中に壊しやすい不変条件

1. **bus subscriber のシャットダウン**(`core/src/runner/plugin.rs:87-110`):
   `spawn_bus_subscriber` は `recv_timeout(200ms)` + `AtomicBool` 前提。
   blocking な `for msg in rx` に書き換えると、`Runtime::drop` が spawn_blocking の
   完了を待つ × Bus が sender を握り続ける、で SIGTERM 時に永久ハング。
   `daemon_signal_shutdown_integration.rs` がこの回帰を検出する。
2. **ドライバスレッドが素の `std::thread` である理由**(`core/src/bin/edlr.rs:396-405`):
   `Runtime::drop` に待たれないため。spawn_blocking に移すなら shutdown 順序を再設計すること。
3. **イベント配送は `try_send`**(`runner/plugin.rs:836-845`): キュー満杯時は
   drop 記録して進む。`send().await` に変えて backpressure を導入すると
   broadcast 消費が詰まり lag が連鎖する。
4. **`sidecar_runtime_lock_for` はブロッキング I/O をまたいで保持するのが仕様**
   (`core/src/registry/sidecar.rs:532-543`、TOCTOU ガード)。async 化するとき
   `std::sync::Mutex` を await 越しに持てないので、ここは per-id の
   `tokio::sync::Mutex` に置き換えが必要。
5. **タイムアウト定数の連鎖**: HTTP タイムアウト < epoch `CALL_DEADLINE`、
   `SIDECAR_SHUTDOWN_GRACE_SECS` と Tauri 側 `STOP_GRACE` の結合(`config/src/lib.rs`)。
   定数を動かすときは const assert と ui/src-tauri 側を同時に見る。

## 検証

- 各 Step 後に `cargo test`(特に `daemon_signal_shutdown_integration.rs`)。
- SIGTERM でデーモンが grace 内に終了することを手で確認(ハングが一番出やすい回帰)。
