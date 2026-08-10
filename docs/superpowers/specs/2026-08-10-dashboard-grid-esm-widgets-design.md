# ダッシュボード: グリッドレイアウト + iframe 廃止(ESM mount)設計

日付: 2026-08-10
状態: 承認済み

## 目的

1. ダッシュボードウィジェットをグリッド上でリサイズ・ドラッグ再配置可能にし、配置を永続化する
2. iframe 隔離をやめ、プラグインのウィジェットを dynamic import した ESM モジュールとしてホスト DOM に直接マウントし、ホストのスタイル(デザイントークン・実在クラス)をそのまま使えるようにする

## 前提・脅威モデル

- プラグインはユーザーが自分でインストールした時点で**信頼済み**とする。UI 側の sandbox 隔離(iframe + opaque origin + postMessage)は撤廃してよい
- ホストは Tailwind v4 JIT のため、プラグインが使えるのは (a) CSS 変数によるデザイントークン、(b) ホスト側ソースに実在するクラス、に限られる。safelist 等の任意クラス対応はやらない(必要になってから)

## 1. ウィジェット形式: `mount(el, api)` ESM

manifest の `[[dashboard]] entry` を HTML から JS モジュールに変更する(例: `ui/sync/index.js`)。

```js
export default function mount(el, api) {
  el.innerHTML = `<button>Sync</button>`;
  el.querySelector("button").onclick = () => api.action("resync");
  api.onEvent((ev) => { /* manifest の events フィルタ通過分だけ届く */ });
  return () => { /* cleanup(任意) */ };
}
```

- `api = { plugin, widget, action(name), onEvent(cb), context }`
- 現行 postMessage プロトコルとの対応: `edlr:ready/init` → mount 呼び出しそのもの、`edlr:event` → `onEvent`、`edlr:action` → `action`、`edlr:height` → **廃止**(同一 DOM なので高さはグリッドセルに従う)
- `core/src/plugin_ui_sdk.js` と `/plugin-ui-sdk.js` ルートは移行完了後に削除

## 2. ホスト側: WidgetFrame → WidgetHost

- `import(/* @vite-ignore */ daemonHttpUrl(entry.url))` でモジュールを取得し、`div` ref に `mount(el, api)`。アンマウント時に cleanup を呼ぶ
- イベント配信は現行の `useEventStream` + `matchesEvent` フィルタを流用し、iframe 転送の代わりに登録コールバックを呼ぶ
- `action` は `rpc.dashboardAction(plugin, widget, name)` を直接呼ぶ
- mount 失敗・実行時例外は既存の react-error-boundary パターンでウィジェット枠内にエラー表示
- デーモンの配信(axum `/plugin-ui/{plugin}/{widget}/{*path}`、grant 検証・トラバーサル対策)は現状のまま流用

**リスク(実装冒頭で確認)**: Tauri webview の CSP が `http://127.0.0.1:<port>` からの script(dynamic import)を許可するか。必要なら `tauri.conf.json` の CSP に追加。

## 3. グリッド: react-grid-layout

- 依存追加: `react-grid-layout`(ドラッグ・リサイズ・衝突回避が全部入り。自作は割に合わない)
- `Dashboard.tsx` の固定 CSS Grid を `<GridLayout cols={6} rowHeight={80}>` に置換
- manifest `size` は初期幅のみに使う: small=2, medium=4, large=6 cols
- レイアウトは `"{plugin}/{widget}"` → `{x, y, w, h}` のマップを **localStorage** に保存。未知の新ウィジェットは末尾に追加。マシン跨ぎ同期はやらない(必要になったらデーモン settings へ)

## 4. 移行と後方互換

- HTML ウィジェットの iframe フォールバックは**作らない**
- 既存プラグインは examples の 2 つ(inara-uploader / state-reader)のみ。同じ変更内で `index.html` → `index.js` に書き換える
- core 側の entry 検証(プラグインディレクトリ内相対パス限定)は拡張子非依存のはずなので原則変更なし。実装時に確認

## 5. テスト

- WidgetHost: モジュールを mock し、mount / cleanup / イベント配信 / action 中継を vitest で確認
- レイアウト永続化: localStorage 読み書きの薄いユニットテストのみ
- react-grid-layout 本体の挙動はテストしない

## やらないこと

- UI サンドボックスの代替(Shadow DOM 等)
- 任意 Tailwind クラスの safelist 配信
- レイアウトのマシン跨ぎ同期
- React コンポーネント export 形式(ホストとの React 共有が必要になるため見送り)
