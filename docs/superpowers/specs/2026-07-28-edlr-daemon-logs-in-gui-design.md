# デーモンログの GUI 表示設計

日付: 2026-07-28
ステータス: 承認済み

## 目的

デーモンが stderr に出している tracing ログ(プラグインの `host-log` 出力を含む)を、
GUI の Logs 画面でも時系列で見られるようにする。

## 決定事項

- 対象: **デーモンの全 tracing ログ(INFO 以上)**。プラグイン/ドライバのログは
  既に tracing 経由(`plugin_id` フィールド付き)なので自動的に含まれる。
- 見せ方: **既存の journal/status リストに kind=log として混ぜる**(時系列の
  突き合わせができる)。種別フィルタを追加。

## 1. バックエンド(tracing → WS ブリッジ)

- `core/src/server.rs`(または新モジュール `core/src/logs.rs`)に **LogSink** を追加:
  整形済みフレーム(`Arc<String>`)を受け取り、直近リングバッファ +
  `broadcast::Sender` で配る。`ServerState` の ReplayBuffer と同じ構造・同じ経路に
  合流させる(実装上は既存 ReplayBuffer へ push できる手を優先してよい)。
- カスタム `tracing_subscriber::Layer`(**LogLayer**)を `tracing_subscriber::fmt`
  に重ねる(`edlr.rs` の初期化)。stderr 出力は現状維持。
- LogLayer は INFO 以上のイベントを次の JSON フレームに整形して LogSink へ送る:

```json
{"type":"event","kind":"log","timestamp":"2026-07-28T12:00:00.000Z",
 "level":"info","target":"edlr_core::plugin::host","message":"inara-uploader initialized plugin_id=\"inara-uploader\""}
```

- フィールド(`plugin_id` など)は `message` 末尾に `key=value` 形式で含める。
- **LogLayer 内では一切ログを出さない**(無限ループ防止)。送信は非ブロッキング
  (詰まったら捨てる)。
- 接続直後のクライアントにも replay リングバッファ(既存 1000 件)経由で直近ログが届く。
- `timestamp` は Layer が整形時に付ける(RFC3339)。

## 2. フロントエンド(Logs 画面)

- `WsMessage` / `LogEntry` に `kind: "log"` を追加(`level: string`, `message: string`,
  `target?: string` を持つ)。`parseWsMessage` を拡張。
- Row 表示: 時刻 / `log` バッジ(level 色分け: warn=黄, error=赤)/ message。
  クリックで raw 展開(既存踏襲)。
- ツールバーに種別フィルタ(journal / status / log のトグル、既定すべて ON)を追加。
- テキスト検索(`filterEntries`)は log の `message` にもマッチさせる。
- Dashboard のウィジェット転送(`edlr:event`)には log を流さない
  (`matchesEvent` は journal/status のみ対象、変更なし)。

## 3. エラーハンドリング

- LogSink の受信側がいない/詰まっている場合は黙って捨てる(ログ表示は
  ベストエフォート。デーモン本体の動作に影響させない)。
- 不正な log フレームはフロントの `parseWsMessage` が null を返して無視(既存挙動)。

## 4. テスト

- Rust: LogLayer の整形(INFO 未満は落とす・JSON 形・フィールドの取り込み)、
  tracing 発火 → WS フレームとして届くことの統合テスト(ws_integration の流儀)。
- vitest: `parseWsMessage` の log フレーム、種別フィルタ、Row の level 表示、
  検索が message にマッチすること。

## スコープ外

- ログレベルの動的変更、DEBUG 転送、ログの永続化・エクスポート。
