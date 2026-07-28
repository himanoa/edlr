# プラグインスケジューラと停止フック(WIT @0.4.0)設計

2026-07-28。inara-uploader の README「不足している実装」1 番
(定期実行・終了フックが無い)を解消する。

## 背景と目的

プラグインはイベント到着時にしか動けないため、(1) 最後のイベント後に
キューへ溜まった送信分は次のイベントが来るまで送れず、(2) デーモン停止時に
メモリ上のキューが失われる。これを、manifest 宣言によるスケジュール実行
(`on-schedule`)と、正常終了時の停止フック(`on-stop`)で解消する。

検討過程: 素朴な on-tick(manifest で間隔宣言)→ 動的 one-shot タイマー
(host-timer API)→ **スケジューラ(manifest 宣言、cron 式対応)** で確定。
決め手は、プラグイン側に予約状態管理を持ち込まないこと、スケジュールが
静的宣言なのでホストが検証・クランプ・UI 表示できること。

## スコープ外

- ユーザーが UI からスケジュールを停止・変更する機能(表示のみ入れる)
- 動的なタイマー API(host-timer)
- ドライバ(`world driver`)へのフック追加
- 打ち漏らし(デーモン停止中に過ぎた定刻)の追い掛け実行

## WIT の変更(ABI 破壊: @0.3.0 → @0.4.0)

`world plugin` に 2 つの export を追加する:

```wit
export on-schedule: func(name: string);
export on-stop: func();
```

- `name` は manifest の `[[schedule]]` の `name`。1 プラグインが複数
  スケジュールを持てるようにするため
- 引数にイベントは無い。プラグインは自分の内部状態(キュー)を見て動く
- `world driver` は変更しない
- 全プラグイン(hello-logger / state-reader / inara-uploader)の再ビルドと、
  Go バインディング(`gen/`)の再生成が必要。旧 world のプラグインは
  ロードに失敗する(従来のバージョン繰り上げと同じ)

## manifest の `[[schedule]]` 書式

```toml
[[schedule]]
name = "flush"
interval-seconds = 60

[[schedule]]
name = "daily-report"
cron = "0 9 * * *"
```

- `name`: `[a-z0-9-]+`、プラグイン内で一意。必須
- `interval-seconds`(正の整数)と `cron`(標準 5 欄: 分 時 日 月 曜日)は
  **どちらか一方を必ず**指定する。両方指定・両方省略は manifest エラー
  (プラグインは `Disabled`、既存の manifest エラーと同じ扱い)
- `[[schedule]]` が 1 つも無ければ `on-schedule` は一切呼ばれない
  (既存プラグインの挙動は manifest を変えない限り不変)
- cron 式のパースは Rust の `cron` クレート。パース失敗は manifest エラー
- **タイムゾーン**: cron 式はデーモンのローカル時刻で解釈する(「毎日 9 時」が
  ユーザーの体感どおりに動く)。ドキュメントに明記する
- **下限クランプ**: 発火の実効間隔は 5 秒を下限とする。`interval-seconds` が
  5 未満の場合、および cron 式が 5 秒未満の間隔で発火しようとする場合は、
  エラーにせず 5 秒間隔へ丸めて warn ログを出す

## ホスト側の実装(core/src/plugin/)

新しいスレッド・tokio タスクは増やさない。

### スケジュール発火(runner.rs)

- プラグインスレッドのループを `work_rx.recv()` から
  `recv_timeout(次の発火までの残り時間)` に変える。タイムアウトしたら
  期限が来たスケジュールの `call_on_schedule(name)` を呼び、次回時刻を
  進める
- wasm 呼び出し(`on-event` / `on-message` / `on-schedule`)の実行中に
  定刻が過ぎた場合、その呼び出しの後に **1 回だけ** 呼ぶ(積み残しの
  連打はしない)。次回時刻は定義に従って進めるが、過去には残さない —
  ブロック中に定刻が複数回過ぎていても、次回時刻は必ず未来の直近の定刻
  まで進める(interval なら発火系列上の次の未来時刻、cron なら次の定刻。
  「1 回だけ呼ぶ」の帰結)
- 時刻は単調時計(`Instant`)ベースで管理し、cron の定刻計算にだけ
  壁時計を使う
- `on-schedule` の trap / `CALL_DEADLINE`(2 秒)超過は `on-event` と
  同じ扱い: プラグインを `Disabled` にする

### 停止フック(runner.rs + bin/edlr.rs + registry.rs)

- デーモンの正常終了シーケンス(SIGTERM / SIGINT / Tauri 終了。既存の
  `shutdown_bus_subscribers` と同じ契機)で各プラグインスレッドへ停止を
  通知し、スレッドはループを抜けた後 `call_on_stop` を 1 回呼んでから
  終了する
- デーモン側は全プラグインの停止完了を有界時間で待つ
  (上限: `CALL_DEADLINE` × プラグイン数。逐次でこの最悪値を超えない)
- **trap で `Disabled` になったプラグインには呼ばない**(壊れた
  インスタンスへの呼び出しは信用しない)。SIGKILL による即死は保証外
  (サイドカーの停止保証と同じ水準)
- `on-stop` の中でも `driver-http.send`(タイムアウト 1.5 秒)は
  `CALL_DEADLINE`(2 秒)内に収まる。既存のコンパイル時アサーションの
  関係は変わらない
- Tauri 側 `STOP_GRACE`(65 秒)との関係: デーモンの後始末の最悪時間が
  「サイドカー停止 + プラグイン on-stop」の合計になる。
  `ui/src-tauri/src/daemon.rs` のコンパイル時アサーションを新しい最悪値で
  見直す(必要なら `STOP_GRACE` を増やす)

### UI / RPC

- `plugins/list` の応答に各プラグインの `schedules` を追加:
  `[{ "name": "flush", "spec": "every 60s" | "cron: 0 9 * * *", "next": "<ISO8601>" }]`
- Plugins 画面はこれを表示するだけ(操作は無し)

## プラグイン側の変更

- **inara-uploader**: `[[schedule]] name = "flush", interval-seconds = 60` を
  宣言。`uploader` パッケージに `HandleSchedule`(`minIntervalSeconds` を
  尊重してフラッシュ)と `HandleStop`(間隔を無視して即フラッシュ。最後の
  機会のため)を追加し、TDD で検証。`main.go` は配線のみ。
  README の「不足している実装」1 番を解消済みに書き換える
- **hello-logger / state-reader**: 空実装の `on-schedule` / `on-stop` を
  足して再ビルド
- Go バインディング再生成: `wit-bindgen-go generate`(README の手順どおり)

## ドキュメント

- `docs/plugins.md`: WIT バージョン節に `0.4.0` を追記、`[[schedule]]` の
  書式・クランプ・タイムゾーン・`on-stop` の保証(呼ばれる契機と呼ばれない
  ケース)を追加
- `README.md`(リポジトリルート)は変更不要(詳細は docs 側)

## テスト方針

- manifest パース(`[[schedule]]` の必須/排他/字種/クランプ)は
  `manifest.rs` の既存テスト群に追加
- スケジュール発火・停止フックのランナー挙動は `runner.rs` の既存テストの
  形(チャネルを使った単体テスト)に合わせる。cron の定刻計算は壁時計に
  依存するため、次回時刻計算を純粋関数に切り出してテストする
- inara-uploader 側は `uploader` パッケージの表テスト(`HandleSchedule` が
  interval を尊重すること、`HandleStop` が即フラッシュすること、空キューで
  何もしないこと)

## 実装時の変更

- 「時刻は単調時計(`Instant`)ベースで管理し、cron の定刻計算にだけ壁時計を
  使う」という上記の方針は実装しなかった。実際には `core/src/plugin/schedule.rs`
  ・`runner.rs` とも interval・cron の両方を `chrono::Local` の壁時計で統一
  している。理由: 時刻表現をひとつに揃えたほうが実装・テストが単純になり、
  `plugins/list` の `next` フィールド(壁時計の ISO8601)を返すにはどのみち
  壁時計へ変換が要る。トレードオフとして NTP のステップ補正やサスペンド/
  レジュームで前方ジャンプが起きると「1 回だけ早め発火してコアレス」、
  後方ジャンプが起きると「次回発火が遅延する」影響を単調時計より受けやすい
  が、これは許容している(詳細は `schedule.rs` のモジュールドキュメント参照)。
