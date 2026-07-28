# edlr ダッシュボードウィジェット設計

日付: 2026-07-28
ステータス: 承認待ち(ブレインストーミング済み、実装計画は未作成)

## 目的

プラグインが edlr の Dashboard タブに独自 UI(ウィジェット)を追加できるようにする。
現在 `ui/frontend/src/pages/Dashboard.tsx` はプレースホルダであり、この機能の受け皿として空けてある。

## 決定事項(ブレインストーミングの結論)

- UI の自由度: **プラグイン製 HTML を iframe で埋め込む**(宣言的ウィジェット型は不採用)
- HTML 供給源: **プラグインディレクトリ同梱の静的アセットをデーモンが配信**(サイドカー HTTP サーバ方式は不採用)
- ライブデータ経路: **親ダッシュボードからの postMessage ブリッジ**(iframe から直接 WS 接続はしない)
- レイアウト: **固定 CSS Grid + マニフェストによるサイズ宣言**(並び替え・自由配置はスコープ外)
- 許可モデル: **他 capability と同じ declare → grant → 検証パターン。grant 必須**

## 1. マニフェスト宣言

`manifest.toml` に `[[dashboard]]` を追加する:

```toml
[[dashboard]]
id = "upload-status"                    # プラグイン内で一意な kebab-case
title = "Inara Upload Status"           # ウィジェットカードの見出し
entry = "ui/upload-status/index.html"   # プラグインディレクトリからの相対パス
size = "medium"                         # small | medium | large
```

検証ルール:

- `id`: `[a-z0-9-]+`、同一プラグイン内で一意。
- `entry`: プラグインディレクトリ内に解決されること(`..` などの脱出はロード時に拒否)。
- `size`: `small` / `medium` / `large` のいずれか。
- `entry` のファイルが存在しない場合は起動を止めず、警告ログ + UI バッジ(`resolved: false`)。
  bus の未解決参照と同じセマンティクス。

## 2. アセット配信(デーモン)

- 既存 axum サーバに `GET /plugin-ui/{plugin-id}/{widget-id}/*path` を追加。
- 配信ルートは `entry` のあるディレクトリ。それより外へのパストラバーサルは 404。
- **grant されていないウィジェットのアセットは 404**(配信自体を許可制にする)。
- レスポンスに CSP ヘッダを付与し、外部ネットワークへのサブリソース読み込み・fetch を遮断する。
  許可するのは自ウィジェットのアセットパス配下のみ。
- Content-Type は拡張子ベース。

## 3. iframe サンドボックス

- `sandbox="allow-scripts"` のみ(`allow-same-origin` は付けない)。
  opaque origin になるため、親 DOM・親の WS・localStorage へ構造的に到達不能。
- ウィジェットの通信手段は postMessage のみ。

## 4. postMessage ブリッジ + SDK

親(Dashboard.tsx)が既存の `useEventStream` WS を一本だけ維持し、各 iframe に配る。

プロトコル:

- widget → 親
  - `{type: "edlr:ready"}` — 初期化完了通知。親はこれを受けてから配信を開始する。
  - `{type: "edlr:height", px}` — 高さ自動調整(任意)。
- 親 → widget
  - `{type: "edlr:init", plugin, widget}` — ready 受信後に送る。
  - `{type: "edlr:event", event}` — そのプラグインのマニフェスト `events` にマッチする
    journal/status イベントのみ転送。接続時は既存 replay バッファ分から流す。
    イベントマッチングは `core/src/plugin/manifest.rs` の `matches_event` と同じ規則を TS に実装する。

SDK:

- デーモンが `/plugin-ui-sdk.js` として小さな 1 ファイルを配信。
- API: `edlr.ready()`, `edlr.onEvent(cb)`。
- ウィジェットは `<script src="/plugin-ui-sdk.js">` を読み込むだけで使える。

bus の retained 値の転送(`edlr:bus` メッセージ)は v1 スコープ外。将来追加できる
メッセージ型の余地だけ残す。

## 5. grant / RPC / Plugins 画面

既存の declare → grant → 検証パターンを踏襲する:

- `core/src/plugin/registry.rs` に dashboard 状態を追加。grant は他 capability と同じく永続化。
- マニフェストの `[[dashboard]]` が変わったら `staleGrant`(既存セマンティクス)。
- RPC(`core/src/server.rs`):
  - `plugins/get-dashboard` / `plugins/set-dashboard-grant` — Plugins 画面用。
  - `dashboard/list` — Dashboard 画面用。grant 済みウィジェットの
    `{plugin, widget, title, url, size, events}` を返す。
- Plugins 画面に `DashboardSection.tsx` を追加(`BusSection.tsx` と同様の UI)。

## 6. Dashboard 画面

- `dashboard/list` の結果を CSS Grid に自動配置。
- 3 カラム基調で `small=1 / medium=2 / large=3` カラムスパン。並び順は登録順
  (プラグイン id → マニフェスト内の宣言順)。
- 各ウィジェットはタイトル付きカード内の iframe。
- プラグイン停止・entry 未解決・grant 切れはカード内プレースホルダ表示。
- ウィジェットの並び替え・表示/非表示切替はスコープ外。

## 7. エラーハンドリング

- entry 不在: 起動継続、警告ログ、`resolved: false` バッジ、Dashboard ではプレースホルダ。
- アセットのパストラバーサル / 未 grant: 404。
- ウィジェットが `edlr:ready` を送らない: 親は配信しないだけ(タイムアウト処理は不要)。
- 不正な postMessage(未知の type、origin 不一致): 無視。

## 8. テスト

- Rust ユニットテスト: マニフェスト検証(id/entry/size/トラバーサル)、
  アセット配信ハンドラ(トラバーサル・grant チェック)、RPC ハンドラ。
- vitest: TS 側イベントマッチング、postMessage ブリッジ、Dashboard 描画(grant 済み一覧、
  サイズ→スパン、プレースホルダ)。
- サンプル: `examples/plugins/hello-logger` に直近イベントを表示するだけの
  ウィジェット HTML を追加し、実地確認に使う。
