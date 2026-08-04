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

**2026-08-04 改訂**: 当初ここにあった「`send-async` が u64 の ID を返し、既存
`on-message` に予約ドライバ名 `"http"` で JSON+base64 の payload を配送する」
設計は**破棄した**。あの形の唯一の存在理由は「export を足すと既存ゲストが
全部ロード不能になるので非破壊で済ませる」だったが、issue-sizx
(submit/complete プロトコル)で ABI 破壊 OK・`on-job-complete` export の追加・
`world plugin` 0.5.0 化と全ゲスト一斉再生成が決定済みになったため、完了配送の
仕組みを 2 系統(on-message ハックと専用 export)並べる意味がなくなった。
予約ドライバ名の manifest 検証・payload の JSON 組み立て/パース・base64・
ユーザーの on-message ハンドラとの demux もすべて不要になる。

#### 2a. ホスト内部の async 化(先行可能。issue: http-driver-9znv)

submit/complete と独立に、今すぐ着手できる部分:

- `drivers/http`: `reqwest::blocking::Client` → async `reqwest::Client` +
  `tokio::runtime::Handle` 保持に置換。
- 既存の同期 `send` は `handle.block_on(...)` 実装に変える。呼び出し元は
  プラグイン/ドライバスレッド(非ランタイムスレッド)なので合法。
  タイムアウト 1.5s/25s(`core/src/host/plugin.rs:92`、`driver.rs:55`)と
  epoch `CALL_DEADLINE` の const assert は現状維持。
- blocking Client 生成のスレッドハック(`drivers/http/src/lib.rs:109`)は削除。
- shutdown で abandon された残留スレッドが同期 `send` を呼ぶと
  `Handle::block_on` が panic しうるが、終了間際のデタッチ済みスレッドなので許容。

#### 2b. 非ブロッキング HTTP は submit/complete に乗せる(issue-sizx 待ち)

ゲスト向けのノンブロッキング HTTP API は、issue-sizx の submit/complete
プロトコルの上に**型付き submit** として実装する:

- `submit-http(request, timeout-ms) -> result<job-id, driver-error>`
  (untyped な `submit(kind, payload)` は「暗黙のプロトコルドリフト防止」思想と
  衝突するため。issue-sizx の実装時補足を参照)
- 結果は `on-job-complete` で配送。キューの自作(種別ごとの削除ルール)、
  in-flight 上限、インスタンス世代管理などの設計・決定事項はすべて
  issue-sizx に従う。
- permission チェック(`check_http_permission`、`core/src/host/plugin.rs:357`)と
  in-flight 上限の判定は submit 時に同期に行い、その場で `result` として返す
  (旧設計から引き継ぐ)。
- 既存の同期 `send` は残す。「lifecycle の中で待つのは自己責任」の契約を
  ドキュメントに明記する(issue-sizx の検討事項)。
- ゲスト SDK に job-id → await ヘルパーを載せる:
  issue sdk-send-async-response-await-lvn3。

実装順は「2a(いつでも)→ issue-sizx 本体 → 2b」。

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
