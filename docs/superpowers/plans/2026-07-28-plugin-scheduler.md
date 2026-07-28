# プラグインスケジューラ + 停止フック 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** WIT `@0.4.0` で `on-schedule(name)` / `on-stop` を追加し、manifest の `[[schedule]]`(interval / cron)に従ってホストがプラグインを起こし、デーモン正常終了時に停止フックを呼ぶ。

**Architecture:** プラグイン専用スレッドのループを `recv()` → `recv_timeout(次の発火までの残り時間)` に変えるだけで、新しいスレッド・tokioタスクは増やさない。停止は `PluginWork::Stop` を同じキューに流す。次回発火時刻の計算は純粋モジュール `schedule.rs` に隔離してテストする。

**Tech Stack:** Rust (wasmtime, chrono 既存), `cron` クレート(新規依存), TinyGo + wit-bindgen-go(inara-uploader)

**仕様書:** `docs/superpowers/specs/2026-07-28-plugin-scheduler-design.md`

## Global Constraints

- WIT パッケージは `edlr:plugin@0.3.0` → `@0.4.0`(ABI 破壊。全プラグイン再ビルド)
- `[[schedule]]`: `name`(`[a-z0-9-]+`、プラグイン内一意、必須)、`interval-seconds`(正整数)と `cron`(標準5欄)は排他必須
- 発火間隔の実効下限 5 秒(下回る指定はエラーにせず 5 秒へ丸めて warn)
- cron はデーモンのローカル時刻で解釈
- ブロック中に定刻が複数回過ぎても発火は 1 回。次回時刻は必ず未来の直近へ進める
- `on-schedule` の trap / CALL_DEADLINE 超過は on-event と同じ(プラグイン Disabled)
- `on-stop` は正常終了時のみ。trap で Disabled のプラグインには呼ばない
- CLAUDE.md: 並列ビルド前に `cargo fetch`、同一 worktree で cargo を並走させない

---

### Task 1: manifest の `[[schedule]]` パース

**Files:**
- Modify: `core/Cargo.toml`(`cron = "0.12"` を追加)
- Modify: `core/src/plugin/manifest.rs`(`ScheduleRequest` 追加)
- Test: 同ファイル内の既存 `#[cfg(test)]` に追加

**Interfaces:**
- Produces: `Manifest.schedules: Vec<ScheduleRequest>`、
  `pub struct ScheduleRequest { pub name: String, pub spec: ScheduleSpec }`、
  `pub enum ScheduleSpec { IntervalSeconds(u64), Cron(String) }`(パース検証済みの cron 式文字列を保持。`cron::Schedule` は `PartialEq` を持たず Manifest の derive を壊すため文字列で持つ)

- [ ] **Step 1: 失敗するテストを書く** — 既存の manifest テスト(`load_manifest` にTOML文字列を食わせる形式)に倣い追加:

```rust
#[test]
fn schedule_with_interval_is_parsed() {
    let m = manifest_from(r#"
        [[schedule]]
        name = "flush"
        interval-seconds = 60
    "#); // manifest_from は既存テストのヘルパに合わせる(無ければ id/name/version/entry を足す小ヘルパを書く)
    assert_eq!(m.schedules.len(), 1);
    assert_eq!(m.schedules[0].name, "flush");
    assert!(matches!(m.schedules[0].spec, ScheduleSpec::IntervalSeconds(60)));
}

#[test]
fn schedule_with_cron_is_parsed() { /* cron = "0 9 * * *" が Cron で入る */ }

#[test]
fn schedule_requires_exactly_one_of_interval_and_cron() {
    // 両方指定 → Err、両方省略 → Err
}

#[test]
fn schedule_rejects_bad_names_and_duplicates() {
    // name が [a-z0-9-]+ 以外 → Err、同名 2 件 → Err(events/sidecar の既存検証に倣う)
}

#[test]
fn schedule_rejects_invalid_cron_expression() {
    // cron = "not a cron" → Err(cron::Schedule::from_str で検証)
}

#[test]
fn schedule_rejects_zero_interval() {
    // interval-seconds = 0 → Err
}
```

- [ ] **Step 2: `cargo test -p edlr-core schedule` で失敗を確認**(`schedules` フィールドが無いのでコンパイルエラー → フィールドとstubを足して assert 失敗まで持っていく)
- [ ] **Step 3: 実装** — `RawManifest` 相当の serde 構造体に `#[serde(default, rename = "schedule")] schedules: Vec<RawSchedule>` を追加し、検証(name 字種・一意、排他必須、interval > 0、`cron::Schedule::from_str` によるパース確認)を既存のバリデーション関数群と同じ場所に足す。5 欄 cron は `cron` クレートが7欄形式のため、パース前に `"0 {5欄} *"` へ正規化する(秒=0、年=*)ヘルパ `normalize_cron(expr: &str) -> String` を書く
- [ ] **Step 4: `cargo test -p edlr-core` 全緑を確認**
- [ ] **Step 5: Commit** — `feat(core): parse [[schedule]] manifest entries`

### Task 2: 次回発火時刻の計算(純粋モジュール)

**Files:**
- Create: `core/src/plugin/schedule.rs`
- Modify: `core/src/plugin/mod.rs`(`pub(crate) mod schedule;`)

**Interfaces:**
- Consumes: `ScheduleSpec`(Task 1)
- Produces:

```rust
pub(crate) const MIN_FIRE_INTERVAL: Duration = Duration::from_secs(5);

/// 1 プラグインの全スケジュールの発火状態。壁時計は引数で受け取り、
/// この型自身は時計を読まない(テストのため)。
pub(crate) struct ScheduleState { /* per-schedule: name, spec, next: DateTime<Local> */ }

impl ScheduleState {
    /// manifest から構築。下限未満の interval は 5 秒へ丸めて warn 済みの状態にする
    pub fn new(schedules: &[ScheduleRequest], now: DateTime<Local>) -> Self;
    /// 次の発火までの残り時間(スケジュールが無ければ None)
    pub fn until_next(&self, now: DateTime<Local>) -> Option<Duration>;
    /// 期限が来ているスケジュール名を最大 1 つ返し、その次回時刻を
    /// 「未来の直近」まで進める(複数期限切れでも 1 回ずつ、呼ぶたびに 1 件)
    pub fn take_due(&mut self, now: DateTime<Local>) -> Option<String>;
}
```

- [ ] **Step 1: 失敗するテストを書く**(`schedule.rs` 内 `#[cfg(test)]`):

```rust
#[test]
fn interval_fires_after_the_interval() {
    let t0 = Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap();
    let mut s = ScheduleState::new(&[req("flush", ScheduleSpec::IntervalSeconds(60))], t0);
    assert_eq!(s.take_due(t0), None);
    assert_eq!(s.until_next(t0), Some(Duration::from_secs(60)));
    assert_eq!(s.take_due(t0 + chrono::Duration::seconds(60)), Some("flush".into()));
}

#[test]
fn missed_fires_are_coalesced_to_one() {
    // 10 分経過後でも take_due は 1 回だけ Some、次回は未来の直近
    // (2 回目の take_due(同時刻) は None、until_next は正の残り時間)
}

#[test]
fn interval_below_minimum_is_clamped_to_five_seconds() { /* interval-seconds = 1 → 実効 5 秒 */ }

#[test]
fn cron_fires_at_the_wall_clock_time() {
    // cron "0 9 * * *"、t0 = 08:59:00 → until_next = 60 秒、09:00 の take_due が Some
}

#[test]
fn two_schedules_fire_independently() { /* 60 秒 interval と cron の混在 */ }
```

- [ ] **Step 2: 失敗を確認**(モジュールが無いのでコンパイルエラーから開始)
- [ ] **Step 3: 実装** — 各スケジュールに `next: DateTime<Local>` を持ち、interval は「発火系列上の次の未来時刻」(`next += interval * ceil((now - next)/interval + 1)` 相当のループで可)、cron は `cron::Schedule::after(&now).next()`。cron 側のクランプは「進めた next が `now + MIN_FIRE_INTERVAL` より近ければ `now + MIN_FIRE_INTERVAL` まで遅らせる」で実装(仕様の「5 秒間隔へ丸める」)
- [ ] **Step 4: `cargo test -p edlr-core schedule` 全緑**
- [ ] **Step 5: Commit** — `feat(core): add pure next-fire computation for plugin schedules`

### Task 3: WIT @0.4.0 と host の呼び出し口 + Rust サンプルプラグイン追随

**Files:**
- Modify: `core/wit/plugin.wit`(パッケージ版数、`world plugin` に 2 export)
- Modify: `core/src/plugin/host.rs`(`call_on_schedule` / `call_on_stop`)
- Modify: `examples/plugins/hello-logger/src/lib.rs`、`examples/plugins/state-reader/src/lib.rs`(空実装)

**Interfaces:**
- Produces: `PluginInstance::call_on_schedule(&mut self, name: &str) -> Result<...>`、`PluginInstance::call_on_stop(&mut self) -> Result<...>`(既存 `call_on_event` と同じ形: epoch deadline を張ってから guest export を呼ぶ)

- [ ] **Step 1: WIT を編集** — `package edlr:plugin@0.4.0;`、`world plugin` に:

```wit
export on-schedule: func(name: string);
export on-stop: func();
```

- [ ] **Step 2: `cargo build -p edlr-core` でビルドを壊す**(bindgen が新 export のトレイトを要求してくることを確認)
- [ ] **Step 3: host.rs に `call_on_schedule` / `call_on_stop` を実装** — `call_on_event` の実装をそのまま雛形に(`set_epoch_deadline` → 生成された `call_on_schedule(&mut store, name)` 呼び出し)。ビルドが通ること
- [ ] **Step 4: Rust サンプル 2 つに空実装を追加** — wit_bindgen の実装トレイトに `fn on_schedule(_name: String) {}` / `fn on_stop() {}` を追加し、`cargo build --release --target wasm32-wasip2` が両方通ることを確認(パスは各 examples ディレクトリ)
- [ ] **Step 5: `cargo test -p edlr-core` 全緑を確認して Commit** — `feat(core)!: add on-schedule/on-stop exports (edlr:plugin@0.4.0)`

### Task 4: ランナーのスケジュール発火

**Files:**
- Modify: `core/src/plugin/runner.rs`(`run_plugin_thread` のループ)
- Test: `runner.rs` 内 `#[cfg(test)]`

**Interfaces:**
- Consumes: `ScheduleState`(Task 2)、`call_on_schedule`(Task 3)

- [ ] **Step 1: ループ骨格を関数に切り出して失敗するテストを書く** — wasm 実体なしでテストするため、ループの「次に何をするか」判定を純粋関数に切り出す:

```rust
/// recv の結果と ScheduleState から次のアクションを決める(テスト可能な芯)。
enum LoopAction { Handle(PluginWork), Fire(String), Stop, Exit }
fn next_action(
    recv: Result<PluginWork, std_mpsc::RecvTimeoutError>,
    due: Option<String>, // state.take_due(now)
) -> LoopAction { ... }
```

テスト: `Timeout` + due あり → `Fire`、`Timeout` + due なし → 何もしない(次の待ちへ)、`Ok(work)` → `Handle`(発火より仕事優先だが、Handle 後に due を確認するのはループ側)、`Disconnected` → `Exit`

- [ ] **Step 2: 失敗を確認**
- [ ] **Step 3: ループを書き換える** — `for work in work_rx` を `loop { match work_rx.recv_timeout(state.until_next(now).unwrap_or(LONG)) }` に。各 wasm 呼び出しの後にも `state.take_due(now)` を確認して期限超過分を 1 回発火(仕様の「ブロック中に過ぎたら後で 1 回」)。`call_on_schedule` の Err は既存の trap 分岐(`set_disabled` + `unsubscribe_plugin` + break)へ合流
- [ ] **Step 4: `cargo test -p edlr-core` 全緑**
- [ ] **Step 5: 実機確認** — hello-logger の manifest に `[[schedule]] name="beat", interval-seconds=5` を足して(配置先 `~/.config/edlr/plugins/hello-logger/manifest.toml` のみ、リポジトリの例は変えない)、`cargo run -p edlr-core --bin edlr` で 5 秒ごとのログを目視。確認後 manifest を戻す
- [ ] **Step 6: Commit** — `feat(core): fire on-schedule from the plugin thread loop`

### Task 5: 停止フック

**Files:**
- Modify: `core/src/plugin/runner.rs`(`PluginWork::Stop`、`run_plugin_thread` の終了パス、`start_plugins` で work_tx / JoinHandle を registry へ登録)
- Modify: `core/src/plugin/registry.rs`(`shutdown_plugins()`)
- Modify: `core/src/bin/edlr.rs`(shutdown シーケンスで `registry.shutdown_plugins()` を `shutdown_bus_subscribers()` の前に呼ぶ)
- Modify: `ui/src-tauri/src/daemon.rs`(STOP_GRACE のコンパイル時アサーションのコメント・式を新しい最悪値で見直す)

**Interfaces:**
- Produces: `Registry::shutdown_plugins(&self)` — 各プラグインへ `PluginWork::Stop` を `try_send` し(満杯なら warn して諦める)、スレッドの JoinHandle を 1 件あたり `CALL_DEADLINE` + 余裕(3 秒)を上限に join する

- [ ] **Step 1: 失敗するテストを書く** — `next_action`(Task 4)に `PluginWork::Stop` → `LoopAction::Stop` を足すテストと、`shutdown_plugins` が「Running のプラグインへ Stop を送る/Disabled へは送らない」ことの registry 単体テスト(既存 registry テストの形に倣う)
- [ ] **Step 2: 失敗を確認**
- [ ] **Step 3: 実装** — `PluginWork::Stop` variant 追加。スレッドは Stop を受けたら `instance.call_on_stop()`(Err は warn ログのみ — もう止まるので Disabled 化に意味はない)して break。`start_plugins` が `work_tx.clone()` と JoinHandle を registry に登録し、`shutdown_plugins` が上記の手順で回す。`bin/edlr.rs` の SIGTERM/SIGINT 処理(既存のサイドカー停止と同じ場所)に `registry.shutdown_plugins()` を追加
- [ ] **Step 4: `cargo test -p edlr-core` 全緑 + 実機確認** — デーモンを起動して Ctrl-C し、hello-logger の on-stop ログ(Step で足す debug 実装でよい)が出てから終了すること、ハングしないことを確認
- [ ] **Step 5: STOP_GRACE のアサーション見直し** — 最悪値 = サイドカー(3 秒 × 20)+ プラグイン on-stop(5 秒 × 想定上限 N)。`ui/src-tauri/src/daemon.rs` の const アサーションの式とコメントを更新(65 秒で足りない計算になるなら値も上げる)。`cargo build -p edlr-ui` が通ること
- [ ] **Step 6: Commit** — `feat(core): call on-stop on graceful daemon shutdown`

### Task 6: RPC / UI にスケジュール表示

**Files:**
- Modify: `core/src/plugin/registry.rs`(`plugins/list` の応答組み立てに `schedules` を追加。next の取得は `Arc<Mutex<>>` で ScheduleState と共有するか、発火のたびに registry へ next を書き戻す — 既存の `settings_json` の共有パターンに倣う)
- Modify: `ui/frontend/src/`(Plugins 画面。既存のプラグインカードに schedules を表示)
- Test: registry のテスト + `ui/frontend` の既存テスト形式(あれば)

- [ ] **Step 1: registry テストを書く** — `plugins/list` 相当の応答に `schedules: [{name, spec, next}]` が載ること(スケジュール無しなら空配列)。spec の文字列表現は `"every 60s"` / `"cron: 0 9 * * *"`
- [ ] **Step 2: 失敗を確認 → 実装 → 緑**
- [ ] **Step 3: フロントエンド** — `pnpm test`(ある場合)と `pnpm build` が通る最小の表示追加(プラグインカード内に「Schedules: flush — every 60s (next HH:MM)」程度の一覧)。凝らない
- [ ] **Step 4: 実機確認** — ブラウザ版 UI で表示を目視
- [ ] **Step 5: Commit** — `feat: show plugin schedules in plugins/list and the Plugins UI`

### Task 7: inara-uploader の追随(Go)+ ドキュメント

**Files:**
- Regenerate: `examples/plugins/inara-uploader/gen/`(`wit-bindgen-go generate --world plugin --out gen ../../../core/wit`)
- Modify: `examples/plugins/inara-uploader/manifest.toml`(`[[schedule]] name="flush", interval-seconds=60`)
- Modify: `examples/plugins/inara-uploader/uploader/uploader.go`(`HandleSchedule` / `HandleStop`)
- Modify: `examples/plugins/inara-uploader/main.go`(配線)
- Modify: `examples/plugins/inara-uploader/README.md`(「不足している実装」1 番を解消済みへ)
- Modify: `docs/plugins.md`(WIT 0.4.0、`[[schedule]]`、on-stop の保証)

**Interfaces:**
- Produces: `func (u *Uploader) HandleSchedule(cfg settings.Settings) Outcome`(`minIntervalSeconds` を尊重してフラッシュ)、`func (u *Uploader) HandleStop(cfg settings.Settings) Outcome`(間隔を無視して即フラッシュ)

- [ ] **Step 1: 失敗するテストを書く**(`uploader_test.go`、既存ヘルパを利用):

```go
func TestScheduleFlushesAPartialBatchAfterTheInterval(t *testing.T) {
    // jump 1 件(batchSize 未満なので送られない)→ c.advance(time.Minute)
    // → HandleSchedule → Sent == 1
}

func TestScheduleRespectsTheMinimumInterval(t *testing.T) {
    // jump 1 件 → 時計を進めずに HandleSchedule → 送られない(Attempted == 0)
}

func TestScheduleWithAnEmptyQueueDoesNothing(t *testing.T) {}

func TestStopFlushesImmediatelyIgnoringTheInterval(t *testing.T) {
    // jump 1 件 → 時計を進めず HandleStop → Sent == 1
}
```

- [ ] **Step 2: `go test ./uploader/` で失敗を確認**
- [ ] **Step 3: 実装** — `HandleSchedule` は `shouldFlush` の interval 判定を通る形で `flush` を呼ぶ(`intervalElapsed(cfg) && queue.len() > 0` のとき送る)。`HandleStop` は `queue.len() > 0` なら無条件に `flush`。`Enabled` チェックは両方に入れる
- [ ] **Step 4: `go test ./...` 全緑**
- [ ] **Step 5: バインディング再生成と配線** — `gen/` を再生成し、`main.go` の `init()` に `plugin.Exports.OnSchedule` / `plugin.Exports.OnStop` を登録(どちらも settings を読み、Outcome を既存 `report(cfg, ...)` へ)。`./build.sh` が通ること
- [ ] **Step 6: 実機確認** — デーモンにロードし、イベント 1 件流して 60 秒後の自動フラッシュ(developer mode ログで確認)と、Ctrl-C 時の即フラッシュを目視
- [ ] **Step 7: README / docs 更新** — inara README の 1 番を「解消済み(`[[schedule]]` と `on-stop` で対応)」へ書き換え、3 番の「キューはメモリ上」記述の関連文も現状に合わせる。`docs/plugins.md` に WIT 0.4.0 の追記と `[[schedule]]` の節を追加
- [ ] **Step 8: Commit** — `feat(inara-uploader): flush on schedule and on stop (edlr:plugin@0.4.0)`

## Self-Review 結果

- 仕様の全要件にタスクが対応(パース=1、計算=2、WIT=3、発火=4、停止=5、UI=6、プラグイン+docs=7)
- 型名は Task 間で一貫(`ScheduleRequest`/`ScheduleSpec`/`ScheduleState`/`PluginWork::Stop`)
- cron クレートが 7 欄形式である点は Task 1 の `normalize_cron` で吸収
