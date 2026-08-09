# inara-uploader 手動同期(ダッシュボードウィジェット)設計

2026-08-10 承認済み。

## 目的

セッション途中のデーモン再起動などで INARA へ送られなかったイベントを、
ユーザーがボタン一つで**現行セッションの Journal から読み直して再送**できる
ようにする。edlr は読み取り位置を永続化していてイベントの再配信は起きない
(docs/cli.md「再配信されない契約」)ため、再送はプラグイン自身が Journal を
読み直す形を取る。

## 1. ウィジェット→プラグインのアクション経路(core + UI、汎用機構)

- **SDK**(`core/src/plugin_ui_sdk.js`): `edlr.action(name)` を追加。iframe から
  親へ `{type: "edlr:action", name}` を postMessage。
- **WidgetFrame**(`ui/frontend/src/components/WidgetFrame.tsx`): `edlr:action`
  を受けたら新 RPC `plugins/dashboard-action {plugin, widget, name}` を呼ぶ。
  plugin/widget は iframe を作った側が知っている値を使う(iframe の自己申告は
  信用しない)。
- **RPC**(server → registry): ダッシュボード grant が有効なプラグインにのみ
  許可。Registry に `dashboard_action(plugin_id, name)` を追加し、対象
  プラグインの作業キューへ `PluginWork::Message(Delivery{ driver_id:
  "dashboard", topic: name, payload: [] })` を積む(bus 配信と同じ admit 経路。
  溢れたら既存の drop counter に計上し RPC はエラー応答)。
- **WIT は無変更**。プラグインは既存の `on-message(driver="dashboard",
  topic=name)` で受ける。`"dashboard"` は予約 driver 名として docs に明記し、
  実ドライバ id との衝突は検証で拒否する。

## 2. inara-uploader ウィジェット

- manifest に `[[dashboard]] id="sync", title="INARA 同期"`。
- `ui/sync/index.html` 1 枚: 「現行セッションを再送」ボタン + 押下後の簡易
  ステータス表示。`edlr.action("resync")` を呼ぶだけ。進捗の詳細はログで見る。

## 3. 再送エンジン(プラグイン側)

- manifest に `[[filesystem]] name="journal" mode="read-only"`(ユーザーが
  Journal ディレクトリを指定して承認する。log-db と同じ方式)。
- `on-message("dashboard", "resync")` で開始: driver-fs の list で最新
  `Journal.*.log` を特定 → `read_range` でチャンク読み → 行を既存
  `mapping.Convert` で変換 → 非同期送信 `SubmitHTTP` でバッチ送信 →
  `on-job-complete` で次のチャンクへ、を EOF まで数珠つなぎ。1 呼び出しが
  短いので `CALL_DEADLINE`(2 秒)内に収まる。
- 変換は新しい `mapping.State` で行う(セッションファイル先頭の LoadGame から
  gameversion / コマンダー名を学習し直す。Live ゲートは mapping が自然に
  掛ける — Legacy セッションは送らない)。
- minIntervalSeconds は適用しない(ジョブ完了駆動の逐次送信が自然なペーシング
  になる)。`enabled=false`・API キー未設定は開始を拒否して理由をログに出す。
- 再送の進行状態(ファイル名・オフセット・部分行)は**メモリのみ**。再起動
  したらもう一度押す。実行中の再押下は無視(1 本だけ走る)。
- 重複: `set*` 系は冪等。履歴系(`addCommanderTravelDock` 等)は INARA 側の
  重複破棄に委ねる旨を README に明記。

## 4. テスト

- core: `dashboard_action` の判定部(grant 無し / 未知プラグインの拒否)の
  純粋テスト + RPC 応答の pin。キュー積み込みは既存 bus 配信テストの形を流用。
- plugin: 再送ステートマシン(チャンク進行・EOF 終了・実行中の再押下無視)を
  値イン値アウトの純粋テストで。driver-fs / HTTP は注入したモックで代替。
