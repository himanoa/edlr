# ダッシュボード グリッド化 + ESM ウィジェット Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** ダッシュボードウィジェットを iframe + postMessage から dynamic import した ESM `mount(el, api)` に置き換え、react-grid-layout でリサイズ・再配置可能にする。

**Architecture:** デーモン(axum)の静的配信はそのまま使い、CORS ヘッダだけ足す(cross-origin dynamic import に必須)。UI 側は WidgetFrame を WidgetHost(dynamic import + mount)に置換し、Dashboard の固定 CSS Grid を react-grid-layout に置換、レイアウトは localStorage 永続化。postMessage プロトコルと plugin_ui_sdk.js は削除。

**Tech Stack:** React 18 + Vite + vitest(ui/frontend)、axum(core/src/server = 命令的モジュール)、react-grid-layout(新規依存)

## Global Constraints

- spec: `docs/superpowers/specs/2026-08-10-dashboard-grid-esm-widgets-design.md`
- core を触るタスクは `.claude/rules/` に従う(server/ は命令的モジュール。mut 最小・判断は純関数)
- HTML ウィジェットの iframe フォールバックは作らない
- 既存の統合テストは消さない(iframe 前提で無効になった WidgetFrame.test.tsx の削除は例外 — 対象コンポーネントごと消えるため)
- UI テスト実行: `cd ui/frontend && pnpm test`、core: `cargo test -p edlr-core`(実行前に `cargo fetch` 済みであること)

---

### Task 1: core — plugin-ui 配信に CORS を足し、CSP と SDK を削除

**Files:**
- Modify: `core/src/server/mod.rs:209-303`(`PLUGIN_UI_SDK` / `widget_csp` / `plugin_ui_handler` / `plugin_ui_sdk_handler` / ルート)
- Delete: `core/src/plugin_ui_sdk.js`
- Test: `core/src/server/` 既存テスト(`grep -rn "plugin-ui-sdk\|widget_csp\|CONTENT_SECURITY_POLICY" core/` で発見したもの)

**Interfaces:**
- Consumes: 既存 `Registry::dashboard_asset_path`、`content_type_for`(変更なし)
- Produces: `GET /plugin-ui/{plugin}/{widget}/{*path}` が `Access-Control-Allow-Origin: *` 付きで応答する。`/plugin-ui-sdk.js` ルートは消滅(404)。CSP ヘッダは付かない

- [ ] **Step 1: 既存テストの所在確認**

Run: `grep -rn "plugin-ui-sdk\|widget_csp\|CONTENT_SECURITY_POLICY\|plugin_ui" core/ --include="*.rs" | grep -i test`
ヒットしたテストを読み、Step 3 で更新する対象を確定する。

- [ ] **Step 2: 変更を書く**

`core/src/server/mod.rs`:
- `PLUGIN_UI_SDK` 定数、`plugin_ui_sdk_handler`、`.route("/plugin-ui-sdk.js", ...)` を削除
- `widget_csp` 関数を削除(iframe の opaque origin 対策だったもの。ウィジェットはホストページ内で動くため文書 CSP は不要)
- `plugin_ui_handler` のヘッダを差し替え:

```rust
    match result {
        Ok(Ok((bytes, content_type))) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type.to_string()),
                // ウィジェットはホストページ(tauri://localhost や vite dev の
                // localhost:5173)から cross-origin の dynamic import で読み込む。
                // module 読み込みは CORS 必須なので許可を明示する。プラグインは
                // 信頼済みインストール物で、配信自体が grant 検証済み(未 grant は
                // 404)なので `*` でよい。
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".to_string()),
            ],
            bytes,
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
```

- doc コメント(iframe / opaque origin / postMessage に言及する箇所)を新モデルに合わせて書き直す
- `core/src/plugin_ui_sdk.js` を削除

- [ ] **Step 3: テスト更新**

Step 1 で見つけたテストから CSP / sdk ルートのアサーションを削除し、`/plugin-ui/...` 応答に `access-control-allow-origin: *` が付くアサーションを追加(既存の配信テストに 1 行足す形でよい。なければ追加しない — HTTP 層は薄いので `dashboard_asset_path` の単体テストが本体)。

- [ ] **Step 4: テスト実行**

Run: `cargo test -p edlr-core`
Expected: PASS(コンパイルエラー・削除漏れ参照がないこと)

- [ ] **Step 5: Commit**

```bash
git add -A core/
git commit -m "feat(core): plugin-ui 配信に CORS を付け、CSP と plugin-ui-sdk を削除"
```

---

### Task 2: ui — WidgetHost(dynamic import + mount)を作り WidgetFrame を置換

**Files:**
- Create: `ui/frontend/src/components/WidgetHost.tsx`
- Create: `ui/frontend/src/components/WidgetHost.test.tsx`
- Modify: `ui/frontend/src/pages/Dashboard.tsx:2,80`(WidgetFrame → WidgetHost)
- Delete: `ui/frontend/src/components/WidgetFrame.tsx`, `ui/frontend/src/components/WidgetFrame.test.tsx`

**Interfaces:**
- Consumes: `DashboardListEntry`(types/plugin.ts:130)、`matchesEvent(entry.events, log)`(lib/events)、`rpcClient$`(store/rpcClient)、`daemonHttpUrl(path)`(ws.ts:95)
- Produces:
  - `WidgetHost({ entry: DashboardListEntry; entries: LogEntry[]; load?: (url: string) => Promise<WidgetModule> })` — default export
  - プラグイン向け契約: モジュールは `export default function mount(el: HTMLElement, api: WidgetApi): (() => void) | void`
  - `WidgetApi = { plugin: string; widget: string; action(name: string): void; onEvent(cb: (ev: LogEntry) => void): void }`

- [ ] **Step 1: 失敗するテストを書く**

`ui/frontend/src/components/WidgetHost.test.tsx`:

```tsx
import { act, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import WidgetHost, { type WidgetApi } from "./WidgetHost";
import type { DashboardListEntry } from "../types/plugin";
import type { LogEntry } from "../lib/filter";

const entry: DashboardListEntry = {
  plugin: "widgety",
  pluginName: "W",
  widget: "status",
  title: "Status",
  url: "/plugin-ui/widgety/status/index.js",
  size: "small",
  events: ["FSDJump"],
  resolved: true,
  state: "running",
};
const jump: LogEntry = { id: 1, kind: "journal", timestamp: "t", event: "FSDJump", raw: {} };
const dock: LogEntry = { id: 2, kind: "journal", timestamp: "t", event: "Docked", raw: {} };

/** mount 済み api を捕まえる fake モジュールローダ。 */
function fakeLoader(captured: { api?: WidgetApi; el?: HTMLElement; cleanups: number }) {
  return async (_url: string) => ({
    default: (el: HTMLElement, api: WidgetApi) => {
      captured.el = el;
      captured.api = api;
      el.textContent = "widget body";
      return () => {
        captured.cleanups++;
      };
    },
  });
}

describe("WidgetHost", () => {
  it("loads the module from the daemon url and mounts it", async () => {
    const captured = { cleanups: 0 } as Parameters<typeof fakeLoader>[0];
    const urls: string[] = [];
    const load = (url: string) => (urls.push(url), fakeLoader(captured)(url));
    render(<WidgetHost entry={entry} entries={[]} load={load} />);
    await screen.findByText("widget body");
    // 相対パスのままだと Tauri シェルでは tauri:// origin に解決されるため絶対化される
    expect(urls).toEqual([`http://localhost:3000${entry.url}`]);
    expect(captured.api!.plugin).toBe("widgety");
    expect(captured.api!.widget).toBe("status");
  });

  it("delivers only matching events, including ones from before mount, once each", async () => {
    // 実ウィジェット同様、mount の中で同期的に onEvent を登録する
    const received: LogEntry[] = [];
    const load = async () => ({
      default: (el: HTMLElement, api: WidgetApi) => {
        el.textContent = "widget body";
        api.onEvent((ev) => received.push(ev));
      },
    });
    const { rerender } = render(<WidgetHost entry={entry} entries={[jump, dock]} load={load} />);
    await screen.findByText("widget body");
    // mount 前の蓄積分のうちマッチする FSDJump のみが届く
    await waitFor(() => expect(received).toHaveLength(1));
    expect(received[0].event).toBe("FSDJump");

    const more: LogEntry = { id: 3, kind: "journal", timestamp: "t", event: "FSDJump", raw: {} };
    rerender(<WidgetHost entry={entry} entries={[jump, dock, more, dock]} load={load} />);
    await waitFor(() => expect(received).toHaveLength(2));
  });

  it("calls cleanup on unmount", async () => {
    const captured = { cleanups: 0 } as Parameters<typeof fakeLoader>[0];
    const { unmount } = render(<WidgetHost entry={entry} entries={[]} load={fakeLoader(captured)} />);
    await screen.findByText("widget body");
    unmount();
    expect(captured.cleanups).toBe(1);
  });

  it("shows an error inside the widget frame when the module fails to load", async () => {
    const load = () => Promise.reject(new Error("404"));
    render(<WidgetHost entry={entry} entries={[]} load={load} />);
    await screen.findByText(/読み込みに失敗/);
  });
});
```

注: `api.action` の RPC 中継は `rpcClient$` の jotai 初期値が null のため、上記では検証しない(action 呼び出しが throw しないことは mount 経由で担保)。rpcClient を差し込むテストが既存 store テストにあるパターンなら倣ってよいが、必須にしない。

- [ ] **Step 2: 失敗を確認**

Run: `cd ui/frontend && pnpm test -- WidgetHost`
Expected: FAIL(WidgetHost が存在しない)

- [ ] **Step 3: WidgetHost.tsx を実装**

```tsx
import { useEffect, useRef, useState } from "react";
import { useAtomValue } from "jotai";
import type { DashboardListEntry } from "../types/plugin";
import type { LogEntry } from "../lib/filter";
import { matchesEvent } from "../lib/events";
import { rpcClient$ } from "@/store/rpcClient";
import { daemonHttpUrl } from "../ws";

/** プラグインウィジェットの mount に渡す API。 */
export interface WidgetApi {
  plugin: string;
  widget: string;
  /**
   * プラグインへのアクション要求。plugins/dashboard-action RPC に変換され、
   * on-message(driver="dashboard", topic=name) へ届く。fire-and-forget で、
   * 失敗(未 grant・プラグイン停止・キュー満杯)は console に残すだけ。
   */
  action(name: string): void;
  /** manifest `events` にマッチしたイベントの購読。mount 前の蓄積分も届く。 */
  onEvent(cb: (ev: LogEntry) => void): void;
}

export interface WidgetModule {
  default: (el: HTMLElement, api: WidgetApi) => (() => void) | void;
}

const defaultLoad = (url: string): Promise<WidgetModule> =>
  import(/* @vite-ignore */ url);

/**
 * ダッシュボードウィジェット 1 件のホスト。
 *
 * デーモンが配信する ESM モジュールを dynamic import し、`mount(el, api)` で
 * この DOM に直接マウントする。プラグインは信頼済みインストール物なので
 * サンドボックスは張らない(旧 iframe + postMessage 方式は廃止)。
 * ホストと同一 DOM のため、CSS 変数などホストのスタイルがそのまま当たる。
 */
export function WidgetHost({
  entry,
  entries,
  load = defaultLoad,
}: {
  entry: DashboardListEntry;
  entries: LogEntry[];
  load?: (url: string) => Promise<WidgetModule>;
}) {
  const elRef = useRef<HTMLDivElement | null>(null);
  const listeners = useRef<Array<(ev: LogEntry) => void>>([]);
  const sentUpTo = useRef(0);
  const [mounted, setMounted] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const rpc = useAtomValue(rpcClient$);
  const rpcRef = useRef(rpc);
  rpcRef.current = rpc;

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    let cleanup: (() => void) | void;
    let cancelled = false;
    const api: WidgetApi = {
      plugin: entry.plugin,
      widget: entry.widget,
      action(name) {
        rpcRef.current
          ?.dashboardAction(entry.plugin, entry.widget, String(name))
          .catch((err) => {
            console.error(
              `dashboard action ${entry.plugin}/${entry.widget}/${name} failed:`,
              err,
            );
          });
      },
      onEvent(cb) {
        listeners.current.push(cb);
      },
    };
    load(daemonHttpUrl(entry.url))
      .then((mod) => {
        if (cancelled) return;
        cleanup = mod.default(el, api);
        setMounted(true);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error(`widget ${entry.plugin}/${entry.widget} mount failed:`, err);
        setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
      cleanup?.();
      listeners.current = [];
      setMounted(false);
      sentUpTo.current = 0;
      el.replaceChildren();
    };
    // load は DI 用でレンダー毎に安定な想定。entry の同一性はキー(plugin/widget)で担保
  }, [entry.plugin, entry.widget, entry.url]);

  useEffect(() => {
    if (!mounted) return;
    for (const log of entries.slice(sentUpTo.current)) {
      if (!matchesEvent(entry.events, log)) continue;
      for (const cb of listeners.current) {
        try {
          cb(log);
        } catch (err) {
          console.error(`widget ${entry.plugin}/${entry.widget} listener failed:`, err);
        }
      }
    }
    sentUpTo.current = entries.length;
  }, [mounted, entries, entry.events, entry.plugin, entry.widget]);

  if (error) {
    return (
      <p className="text-sm text-destructive">
        ウィジェットの読み込みに失敗しました: {error}
      </p>
    );
  }
  return <div ref={elRef} className="h-full min-h-0 overflow-auto" />;
}

export default WidgetHost;
```

実装メモ:
- `sentUpTo` は「mount 後に登録されたリスナーへ蓄積分から配る」ために mount 前は進めない(`mounted` ガードがその役)
- 旧実装の「ready 前に届いた分は ready 時にまとめて送る」と同じ意味論
- イベント配送はリスナー例外で他リスナーを止めない(旧 SDK と同じ)

- [ ] **Step 4: Dashboard.tsx の差し替え**

`import WidgetFrame from "../components/WidgetFrame"` → `import WidgetHost from "../components/WidgetHost"`、`<WidgetFrame entry={w} entries={entries} />` → `<WidgetHost entry={w} entries={entries} />`。
`WidgetFrame.tsx` / `WidgetFrame.test.tsx` を削除。

- [ ] **Step 5: テスト実行**

Run: `cd ui/frontend && pnpm test`
Expected: PASS(WidgetHost 4件 + 既存スイート。WidgetFrame のテストは消えている)

- [ ] **Step 6: Commit**

```bash
git add -A ui/frontend/src
git commit -m "feat(ui): iframe をやめ dynamic import の WidgetHost でウィジェットをマウント"
```

---

### Task 3: ui — react-grid-layout でリサイズ・再配置 + localStorage 永続化

**Files:**
- Create: `ui/frontend/src/lib/widgetLayout.ts`
- Create: `ui/frontend/src/lib/widgetLayout.test.ts`
- Modify: `ui/frontend/src/pages/Dashboard.tsx`(WidgetGrid を GridLayout 化)
- Modify: `ui/frontend/package.json`(依存追加)

**Interfaces:**
- Consumes: `WidgetSize`(types/plugin.ts)、Task 2 の `WidgetHost`
- Produces:
  - `LayoutItem = { i: string; x: number; y: number; w: number; h: number }`(react-grid-layout の Layout と構造互換)
  - `mergeLayout(saved: LayoutItem[], widgets: { key: string; size: WidgetSize }[]): LayoutItem[]`
  - `loadLayout(storage?: Pick<Storage, "getItem">): LayoutItem[]`
  - `saveLayout(items: LayoutItem[], storage?: Pick<Storage, "setItem">): void`

- [ ] **Step 1: 依存追加**

Run: `cd ui/frontend && pnpm add react-grid-layout && pnpm add -D @types/react-grid-layout`

- [ ] **Step 2: 失敗するテストを書く**

`ui/frontend/src/lib/widgetLayout.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { loadLayout, mergeLayout, saveLayout, type LayoutItem } from "./widgetLayout";

const saved: LayoutItem[] = [{ i: "p/a", x: 2, y: 0, w: 4, h: 3 }];

describe("mergeLayout", () => {
  it("keeps saved placement for known widgets", () => {
    expect(mergeLayout(saved, [{ key: "p/a", size: "small" }])).toEqual(saved);
  });

  it("appends unknown widgets below with width from manifest size", () => {
    const merged = mergeLayout(saved, [
      { key: "p/a", size: "small" },
      { key: "p/b", size: "large" },
    ]);
    expect(merged[1]).toEqual({ i: "p/b", x: 0, y: 4, w: 6, h: 3 });
  });

  it("drops saved entries for widgets that no longer exist", () => {
    expect(mergeLayout(saved, [])).toEqual([]);
  });
});

describe("load/saveLayout", () => {
  it("round-trips through storage and tolerates garbage", () => {
    const store = new Map<string, string>();
    const storage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
    };
    saveLayout(saved, storage);
    expect(loadLayout(storage)).toEqual(saved);
    store.set("edlr.dashboardLayout", "not json");
    expect(loadLayout(storage)).toEqual([]);
  });
});
```

- [ ] **Step 3: 失敗を確認**

Run: `cd ui/frontend && pnpm test -- widgetLayout`
Expected: FAIL(モジュールが存在しない)

- [ ] **Step 4: widgetLayout.ts を実装**

```ts
import type { WidgetSize } from "../types/plugin";

/** react-grid-layout の Layout と構造互換(必要なキーだけ)。 */
export interface LayoutItem {
  i: string;
  x: number;
  y: number;
  w: number;
  h: number;
}

const STORAGE_KEY = "edlr.dashboardLayout";
/** manifest size → 初期カラム幅(グリッドは 6 カラム)。 */
export const SIZE_COLS: Record<WidgetSize, number> = { small: 2, medium: 4, large: 6 };
export const GRID_COLS = 6;
export const DEFAULT_H = 3; // rowHeight 80px × 3 + マージンで旧 iframe 既定 240px 相当

/**
 * 保存済みレイアウトと現在のウィジェット一覧の突き合わせ。
 * 既知は保存位置を維持、新顔は最下段に manifest size 幅で追加、
 * 消えたウィジェットの保存分は捨てる。
 */
export function mergeLayout(
  saved: LayoutItem[],
  widgets: { key: string; size: WidgetSize }[],
): LayoutItem[] {
  const byKey = new Map(saved.map((it) => [it.i, it]));
  const bottom = saved
    .filter((it) => widgets.some((w) => w.key === it.i))
    .reduce((max, it) => Math.max(max, it.y + it.h), 0);
  return widgets
    .map((w, idx) => byKey.get(w.key) ?? {
      i: w.key,
      x: 0,
      y: bottom + idx, // 正確な詰めは react-grid-layout の compact に任せる
      w: SIZE_COLS[w.size],
      h: DEFAULT_H,
    })
    .map(({ i, x, y, w, h }) => ({ i, x, y, w, h }));
}

export function loadLayout(storage: Pick<Storage, "getItem"> = localStorage): LayoutItem[] {
  try {
    const parsed = JSON.parse(storage.getItem(STORAGE_KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

export function saveLayout(
  items: LayoutItem[],
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  storage.setItem(
    STORAGE_KEY,
    JSON.stringify(items.map(({ i, x, y, w, h }) => ({ i, x, y, w, h }))),
  );
}
```

注: `mergeLayout` のテスト期待値(`y: 4`)は `bottom(=3... saved p/a は h3 y0 → bottom 3)+ idx(1)= 4`。

- [ ] **Step 5: テスト通過を確認**

Run: `cd ui/frontend && pnpm test -- widgetLayout`
Expected: PASS

- [ ] **Step 6: Dashboard.tsx を GridLayout 化**

WidgetGrid を差し替え:

```tsx
import GridLayout, { WidthProvider } from "react-grid-layout";
import "react-grid-layout/css/styles.css";
import "react-resizable/css/styles.css";
import {
  GRID_COLS,
  loadLayout,
  mergeLayout,
  saveLayout,
} from "../lib/widgetLayout";

const Grid = WidthProvider(GridLayout);

function WidgetGrid({ entries }: { entries: LogEntry[] }) {
  const { widgets } = useAtomValue(widgets$);
  const [layout, setLayout] = useState(() =>
    mergeLayout(
      loadLayout(),
      widgets.map((w) => ({ key: `${w.plugin}/${w.widget}`, size: w.size })),
    ),
  );

  if (widgets.length === 0) {
    return (/* 既存の空表示のまま */);
  }

  return (
    <section>
      <Grid
        layout={layout}
        cols={GRID_COLS}
        rowHeight={80}
        draggableHandle=".widget-drag-handle"
        onLayoutChange={(next) => {
          const items = next.map(({ i, x, y, w, h }) => ({ i, x, y, w, h }));
          setLayout(items);
          saveLayout(items);
        }}
      >
        {widgets.map((w) => (
          <article
            key={`${w.plugin}/${w.widget}`}
            className="widget-card flex min-w-0 flex-col overflow-hidden rounded-lg border bg-card px-4 py-3"
          >
            <h2 className="widget-drag-handle mb-2 cursor-move text-base font-semibold">
              {w.title}
            </h2>
            {w.state !== "running" ? (
              <p className="text-muted-foreground">プラグインが停止しています</p>
            ) : !w.resolved ? (
              <p className="text-muted-foreground">entry ファイルが見つかりません</p>
            ) : (
              <WidgetHost entry={w} entries={entries} />
            )}
          </article>
        ))}
      </Grid>
    </section>
  );
}
```

実装メモ:
- ドラッグはタイトル(`.widget-drag-handle`)のみ。カード全体をドラッグ可能にするとウィジェット内のボタン操作と衝突する
- `SPAN` 定数と `min-[900px]:grid-cols-3` の旧グリッドは削除
- widgets が後から増減した場合(このコンポーネントは Suspense 下なので widgets 確定後にマウントされる)、`layout` state 初期値で足りる。動的な再 merge は不要
- `useState` / `WidgetHost` の import 追加を忘れない

- [ ] **Step 7: 全テスト + ビルド確認**

Run: `cd ui/frontend && pnpm test && pnpm build`
Expected: PASS / ビルド成功(型エラーなし)

- [ ] **Step 8: Commit**

```bash
git add -A ui/frontend
git commit -m "feat(ui): ダッシュボードを react-grid-layout 化しレイアウトを localStorage に保存"
```

---

### Task 4: examples — 2 ウィジェットを ESM mount に移行、SDK 参照を掃除

**Files:**
- Create: `examples/plugins/inara-uploader/ui/sync/index.js`
- Create: `examples/plugins/state-reader/ui/last-jump/index.js`
- Delete: `examples/plugins/inara-uploader/ui/sync/index.html`, `examples/plugins/state-reader/ui/last-jump/index.html`
- Modify: `examples/plugins/inara-uploader/manifest.toml:51`, `examples/plugins/state-reader/manifest.toml:16`(entry を .js に)
- Modify: `grep -rln "plugin-ui-sdk\|postMessage" docs/ examples/ --include="*.md"` でヒットした文書

**Interfaces:**
- Consumes: Task 2 の `mount(el, api)` 契約(`api.action(name)` / `api.onEvent(cb)`)

- [ ] **Step 1: inara-uploader の index.js**

```js
// INARA 手動同期ボタン。api.action("resync") で plugin 本体の
// on-message(driver="dashboard", topic="resync") が届く。
export default function mount(el, api) {
  el.innerHTML = `
    <button type="button">現行セッションを再送</button>
    <p class="text-sm text-muted-foreground" data-status>
      未送信分を INARA へ送り直します。進捗はログ画面で確認できます。
    </p>
  `;
  const status = el.querySelector("[data-status]");
  el.querySelector("button").addEventListener("click", () => {
    api.action("resync");
    status.textContent =
      "再送をリクエストしました(" + new Date().toLocaleTimeString() + ")。結果はログ画面で確認できます。";
  });
}
```

manifest: `entry = "ui/sync/index.js"`。`index.html` 削除。

- [ ] **Step 2: state-reader の index.js**

```js
// 直近の FSDJump を表示する。イベントは manifest `events` フィルタ通過分だけ届く。
export default function mount(el, api) {
  el.innerHTML = `
    <div class="text-xl font-semibold" data-system>—</div>
    <div class="text-sm text-muted-foreground" data-time>FSDJump 待ち</div>
  `;
  const system = el.querySelector("[data-system]");
  const time = el.querySelector("[data-time]");
  api.onEvent((event) => {
    if (event.kind !== "journal" || event.event !== "FSDJump") return;
    system.textContent = (event.raw && event.raw.StarSystem) || "?";
    time.textContent = event.timestamp || "";
  });
}
```

manifest: `entry = "ui/last-jump/index.js"`。`index.html` 削除。
(`text-muted-foreground` 等はホストに実在するクラス — spec の「実在クラスのみ」方針どおり)

- [ ] **Step 3: SDK 参照の掃除**

Run: `grep -rn "plugin-ui-sdk\|edlr:ready\|edlr:height\|edlr:init" docs/ examples/ README.md 2>/dev/null`
ヒットした文書(ウィジェット開発ガイド等があれば)を `mount(el, api)` 契約に書き直す。コード側の削除は Task 1/2 で完了している前提で、ここは文書のみ。

- [ ] **Step 4: 動作確認(全体)**

Run: `cargo test -p edlr-core && cd ui/frontend && pnpm test && pnpm build`
Expected: すべて PASS

- [ ] **Step 5: Commit**

```bash
git add -A examples/ docs/ README.md
git commit -m "feat(examples): ウィジェットを ESM mount 形式へ移行"
```

---

### Task 5: 実機確認

- [ ] **Step 1: 起動確認**

Tauri シェルまたは `pnpm dev` + デーモンでダッシュボードを開き、以下を目視:
1. 2 ウィジェットが表示され、state-reader がイベントで更新される(またはモジュールが読み込まれエラー表示がない)
2. タイトルバーのドラッグで再配置、右下ハンドルでリサイズできる
3. リロードで配置が復元される(localStorage)
4. inara-uploader のボタンで dashboard-action が飛ぶ(デーモンログで確認)

確認できない環境要因(デーモン未起動等)があれば、その旨を報告に残す。
