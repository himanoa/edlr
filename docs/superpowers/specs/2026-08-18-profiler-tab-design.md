# プロファイラータブ 設計

2026-08-18。edlr デーモンにプロファイラを内蔵し、UI に Profiler タブを追加する。

## 目的

- どのプラグイン/ドライバが遅いか(wasm 呼び出しの所要時間)を特定できる
- キュー滞留・ドロップ・イベント流量・メモリ使用量を観測できる
  (issue-hdly の delivery ポリシー、issue reliable-zn3c の滞留可視化と地続き)
- 生サンプルをディスクに全部残し、DuckDB で後から任意の切り口で分析できる

## 全体像

```
計測点(各スレッド)
  │ ProfilerSink::record(sample)   … try_send、満杯なら捨てて数える
  ▼
collector スレッド(1 本)
  ├─ 生 JSONL 追記: <state-base>/profiler/YYYY-MM-DD.jsonl
  └─ メモリリング: 直近 3600 秒の 1 秒バケット(subject×id ごと)
        ▲
        │ profiler/summary・profiler/series(RPC、ポーリング)
      UI Profiler タブ
```

- 判断・集計・整形は純粋モジュール `core/src/profiler/` に置く
  (サンプル型、1 秒バケット集計、JSON 整形)。
- チャネル・スレッド・ファイル書き込みは命令的側に置く
  (`.claude/rules/pure-imperative-boundary.md` に従う)。

## サンプルのデータモデル

種別は 2 つだけ。

### Call サンプル(wasm 呼び出し 1 件ごと)

```jsonc
{ "ts": 1755500000.123, "subject": "plugin", "id": "inara-uploader",
  "call": "on-event",            // on-event | on-message | on-schedule | on-job-complete
  "detail": "FSDJump",           // イベント名 / "driver/topic" / スケジュール名
  "duration_us": 1800,
  "outcome": "ok" }              // ok | error | timeout
```

計測点:

- `core/src/runner/plugin/event_loop.rs` の `LoopAction::Handle` / `Fire` の
  各 `instance.call_*`、および `fire_all_due` 内
- ドライバ側の対応する wasm 呼び出し(`runner/driver.rs`)

呼び出しの前後で `Instant::now()` を取るだけ。イベント流量は独立に計測せず、
Call サンプルから導出する(1 秒あたりの on-event 件数)。

### Gauge サンプル(1 秒ごとの状態値)

```jsonc
{ "ts": 1755500000, "subject": "plugin", "id": "inara-uploader",
  "queue_len": 3,
  "dropped_events": 0, "dropped_bus": 0,
  "memory_bytes": 4194304 }
```

sink 満杯で捨てた計測サンプル数(sink 全体の累計)は per-id ではなく、
毎秒 1 行の専用 gauge として記録する:

```jsonc
{ "ts": 1755500000, "subject": "profiler", "id": "sink", "lost": 0 }
```

- `queue_len`: `WorkSender` に `len()` を追加して読む(Mutex 1 回)
- `dropped_*`: 既存 `DropCounters::snapshot()`
- `memory_bytes`: `PluginInstance` はスレッド外に出せないため、プラグイン
  スレッド自身が wasm 呼び出し直後に測って `Arc<AtomicUsize>` へ書き、
  gauge 走査はそれを読むだけ。呼び出しが無い間は据え置き(アイドル中の
  メモリは変わらないので実害なし)

## 収集経路

- `ProfilerSink`: `Arc` で各スレッドへ配る軽量ハンドル。中身は容量固定
  (4096)のチャネルへの `try_send`。**満杯ならサンプルを捨てて捨てた数を
  数える** -- 計測が本体をブロックしない・波及させないことが最優先。
  捨てた数は gauge の `profiler_lost` として観測可能
- 配線しないテスト・環境向けに no-op sink を持つ(`Option` 分岐ではなく
  null オブジェクト)

## collector スレッド

1 本のスレッドが受信・gauge 走査・書き込みを兼ねる:

- **生 JSONL**: `<state-base>/profiler/YYYY-MM-DD.jsonl` に 1 サンプル 1 行で
  追記。日付が変わったらファイルを切り替える。削除・ローテーションはしない
  (全部残す。保持日数設定は必要になったら後付け -- issue 化する)。
  BufWriter + 1 秒ごと flush。DuckDB の `read_json_auto('profiler/*.jsonl')`
  でそのまま読める
- **メモリリング**: 直近 3600 秒の 1 秒バケット。subject×id ごとに
  calls / avg / max / errors + 最新 gauge。集計は純粋関数
  (`bucket::fold(samples) -> Bucket`)、リングは `Mutex<VecDeque>`
- gauge の 1 秒走査は `recv_timeout(次の秒境界までの残り)` で兼ねる
  (`run_plugin_thread` と同じ流儀)。書き込み遅延で周期がずれても、
  バケットのタイムスタンプは実時刻で打つ(グラフが疎になるだけ)
- **シャットダウン**: `AtomicBool` フラグ + センチネルで抜けて最後に flush

設定は追加しない(常時 ON)。通常プレイで日に数 MB、replay バースト時に
数十 MB/日 程度の見込み。

## RPC

`server/` に薄いハンドラ、`rpc/` に純粋な整形関数。メソッドは 2 つ。

### `profiler/summary`(パラメータなし)

直近 60 秒のリングバケットを畳んで、概要テーブル 1 画面ぶんを返す:

```jsonc
{ "profilerLost": 0,             // sink 満杯で捨てた計測サンプル数(累計)
  "subjects": [
  { "subject": "plugin", "id": "inara-uploader",
    "calls_1m": 42, "avg_us_1m": 1800, "max_us_1m": 210000, "errors_1m": 0,
    "queue_len": 3, "dropped": { "events": 0, "busDeliveries": 0 },
    "memory_bytes": 4194304 }
] }
```

### `profiler/series`(`{ subject, id, seconds }`、seconds ≦ 3600)

1 秒バケットの時系列。欠けた秒は `null` で埋める(グラフの切れ目):

```jsonc
{ "from_ts": 1755500000, "step": 1,
  "points": [ { "calls": 3, "avg_us": 1500, "max_us": 9000, "errors": 0,
                "queue_len": 2, "memory_bytes": 4194304 }, null, ... ] }
```

UI はポーリング(summary 2 秒、series は行選択中のみ 2 秒)。WS push は
足さない -- 1 秒粒度の集計値に即時性の意味が無い。

## UI: Profiler タブ

`ui/frontend/src/pages/Profiler.tsx`。既存 5 ページ(Dashboard / Plugins /
Drivers / Logs / Settings)の並びに追加。

- **上半分: 概要テーブル**。列 = 名前(plugin/driver バッジ)/ calls/min /
  avg / max / errors / queue / dropped / memory。閾値超え(max > 1s、
  queue > 48 など)の行は既存 warn 色で強調。ソート可
- **下半分: 行選択で時系列グラフ 3 枚**: ①呼び出しレート+エラー
  ②所要時間 avg/max ③queue_len+memory。表示範囲は 5 分 / 1 時間の
  切り替えのみ
- グラフはチャートライブラリを**入れず**、手書き SVG(polyline の小さな
  コンポーネント 1 個)。要件が軽く、テーマ色にそのまま乗るため。
  凝りたくなったら後から差し替え可能

## テスト戦略(`.claude/rules/testing.md` の二層)

- 純粋テスト: バケット集計(`now` は引数渡し)、summary/series の整形、
  JSONL 1 行のシリアライズ
- 統合テスト: sink 経由でサンプルを流し、JSONL に行が書かれる・リングから
  series が引ける・満杯時に捨てて lost が数えられる・shutdown で flush
  される
- UI: 既存ページと同じ vitest + testing-library(モック summary の
  テーブル描画、行選択 → series 取得)

## 実装順

1. `core/src/profiler/` 純粋部(サンプル型・バケット集計)
2. sink / collector スレッド / JSONL 書き込み
3. 計測点の配線(event_loop・runner/driver・queue の `len()`・memory gauge)
4. RPC(`profiler/summary`・`profiler/series`)
5. UI タブ(④まで済んだ時点で実デーモンの実データを確認してから着手)

## スコープ外(必要になったら別 issue)

- 生 JSONL の保持日数設定・ローテーション
- UI からの過去日データ閲覧(直近 1 時間より前は DuckDB で手動分析)
- プロファイラの ON/OFF 設定
