# edlr UI フェーズ 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** core に WebSocket サーバ + 静的配信を追加し、React フロントエンド(Logs / Plugins / Dashboard プレースホルダ)と Tauri 2 の薄い皮を実装する。

**Architecture:** core の `server` モジュールが Router を subscribe し、リングバッファ(リプレイ 1000 件)+ broadcast で `/ws` に JSON テキストフレームを配信。フロントは Vite + React + TS の SPA で、`ws://…/ws` に接続する純粋なクライアント。Tauri はフロントをバンドルして表示するだけ。設計書: `docs/superpowers/specs/2026-07-25-edlr-ui-phase1-design.md`

**Tech Stack:** Rust (axum 0.8, tower-http 0.6, tokio-tungstenite dev)、React 18 + TypeScript 5 + Vite 5 + vitest 2 + Testing Library、Tauri 2、pnpm(node は mise 管理)。

## Global Constraints

- 監視系・サーバ系コードは panic しない(`Mutex` の poison は `unwrap_or_else(|e| e.into_inner())` で回復)
- WS プロトコル: 接続直後 `{"type":"hello","protocol":1}`、イベントは `{"type":"event","kind":"journal","timestamp":…,"event":…,"raw":…}` / `{"type":"event","kind":"status","raw":…}`。クライアント→サーバのメッセージは無視(将来の RPC 用予約)
- リプレイバッファ容量は 1000。新規接続にはスナップショット→ライブの順で欠落・重複なく配信する
- CLI 既定: `--listen 127.0.0.1:8137`。`--ui-dir` は任意で、指定時のみ静的配信(SPA フォールバックは index.html)
- stdout への 1 行 1 JSON 出力は従来どおり維持
- フロントの状態は localStorage キー `edlr.plugin-settings.<plugin-id>` に保存
- node が見つからない場合は `mise exec -- pnpm <cmd>` として実行する(node 26 は mise でインストール済み)
- コミットメッセージ末尾に `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

---

### Task 1: core の WebSocket サーバ(server モジュール + 統合テスト)

**Files:**
- Modify: `core/Cargo.toml`(依存追加)
- Create: `core/src/server.rs`
- Modify: `core/src/lib.rs`
- Create: `core/tests/ws_integration.rs`

**Interfaces:**
- Consumes: `Router::subscribe()`, `Event`
- Produces: `pub struct ServerState`(`Clone`)/ `ServerState::new(router: &Router) -> ServerState` / `pub async fn serve(listener: tokio::net::TcpListener, state: ServerState, ui_dir: Option<PathBuf>)` / `pub fn app(state: ServerState, ui_dir: Option<PathBuf>) -> axum::Router` / `pub fn event_to_ws_json(event: &Event) -> String` / `pub fn hello_json() -> String`

- [ ] **Step 1: 依存を追加する**

`core/Cargo.toml` の `[dependencies]` に追加:

```toml
axum = { version = "0.8", features = ["ws"] }
tower-http = { version = "0.6", features = ["fs"] }
```

`[dev-dependencies]` に追加:

```toml
tokio-tungstenite = "0.26"
futures-util = "0.3"
```

- [ ] **Step 2: 失敗する統合テストを書く**(`core/tests/ws_integration.rs`)

```rust
use edlr_core::event::Event;
use edlr_core::router::Router;
use edlr_core::server::{self, ServerState};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

async fn setup(ui_dir: Option<std::path::PathBuf>) -> (Router, SocketAddr) {
    let router = Router::new(64);
    let state = ServerState::new(&router);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server::serve(listener, state, ui_dir));
    (router, addr)
}

async fn connect(addr: SocketAddr) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("connect");
    ws
}

async fn recv_json(ws: &mut Ws) -> serde_json::Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timeout")
            .expect("stream ended")
            .expect("ws error");
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).expect("valid json");
        }
    }
}

fn journal(name: &str) -> Event {
    Event::Journal {
        timestamp: "2026-07-25T12:00:00Z".into(),
        event: name.into(),
        raw: serde_json::json!({"timestamp": "2026-07-25T12:00:00Z", "event": name}),
    }
}

#[tokio::test]
async fn sends_hello_then_live_events() {
    let (router, addr) = setup(None).await;
    let mut ws = connect(addr).await;
    let hello = recv_json(&mut ws).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["protocol"], 1);
    router.publish(journal("FSDJump"));
    let ev = recv_json(&mut ws).await;
    assert_eq!(ev["type"], "event");
    assert_eq!(ev["kind"], "journal");
    assert_eq!(ev["event"], "FSDJump");
    assert_eq!(ev["raw"]["event"], "FSDJump");
    router.publish(Event::Status { raw: serde_json::json!({"Flags": 5}) });
    let ev = recv_json(&mut ws).await;
    assert_eq!(ev["kind"], "status");
    assert_eq!(ev["raw"]["Flags"], 5);
}

#[tokio::test]
async fn replays_buffered_events_to_new_connection() {
    let (router, addr) = setup(None).await;
    router.publish(journal("LoadGame"));
    router.publish(journal("Location"));
    // feeder かクライアント購読のどちらかが必ず拾う設計なので sleep 不要
    let mut ws = connect(addr).await;
    assert_eq!(recv_json(&mut ws).await["type"], "hello");
    assert_eq!(recv_json(&mut ws).await["event"], "LoadGame");
    assert_eq!(recv_json(&mut ws).await["event"], "Location");
}

#[tokio::test]
async fn client_messages_are_ignored_and_connection_survives() {
    let (router, addr) = setup(None).await;
    let mut ws = connect(addr).await;
    assert_eq!(recv_json(&mut ws).await["type"], "hello");
    ws.send(Message::Text("{\"type\":\"rpc\"}".into())).await.unwrap();
    router.publish(journal("Music"));
    assert_eq!(recv_json(&mut ws).await["event"], "Music");
}

#[tokio::test]
async fn multiple_clients_receive_the_same_events() {
    let (router, addr) = setup(None).await;
    let mut a = connect(addr).await;
    let mut b = connect(addr).await;
    assert_eq!(recv_json(&mut a).await["type"], "hello");
    assert_eq!(recv_json(&mut b).await["type"], "hello");
    router.publish(journal("Docked"));
    assert_eq!(recv_json(&mut a).await["event"], "Docked");
    assert_eq!(recv_json(&mut b).await["event"], "Docked");
}

async fn http_get(addr: SocketAddr, path: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).await.unwrap();
    buf
}

#[tokio::test]
async fn serves_static_files_with_spa_fallback() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<html>edlr-ui</html>").unwrap();
    std::fs::write(dir.path().join("app.js"), "console.log(1)").unwrap();
    let (_router, addr) = setup(Some(dir.path().to_path_buf())).await;
    let js = http_get(addr, "/app.js").await;
    assert!(js.contains("console.log(1)"), "got: {js}");
    let fallback = http_get(addr, "/logs").await;
    assert!(fallback.contains("edlr-ui"), "SPA fallback should serve index.html, got: {fallback}");
}
```

- [ ] **Step 3: 失敗確認**

Run: `cargo test -p edlr-core --test ws_integration`
Expected: FAIL(`server` モジュール未定義)

- [ ] **Step 4: 実装する**(`core/src/server.rs`)

```rust
use crate::event::Event;
use crate::router::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tower_http::services::{ServeDir, ServeFile};

const REPLAY_CAPACITY: usize = 1000;

pub fn hello_json() -> String {
    serde_json::json!({"type": "hello", "protocol": 1}).to_string()
}

pub fn event_to_ws_json(event: &Event) -> String {
    match event {
        Event::Journal { timestamp, event, raw } => serde_json::json!({
            "type": "event", "kind": "journal",
            "timestamp": timestamp, "event": event, "raw": raw,
        })
        .to_string(),
        Event::Status { raw } => {
            serde_json::json!({"type": "event", "kind": "status", "raw": raw}).to_string()
        }
    }
}

/// WS 配信用の共有状態。feeder タスクがロックを保持したままリングバッファ追記と
/// broadcast 送信を行うため、新規接続のスナップショット+購読(同じくロック下)
/// との間で欠落も重複も起きない。
#[derive(Clone)]
pub struct ServerState {
    inner: Arc<Mutex<ReplayBuffer>>,
}

struct ReplayBuffer {
    buf: VecDeque<Arc<String>>,
    tx: broadcast::Sender<Arc<String>>,
}

impl ServerState {
    pub fn new(router: &Router) -> Self {
        let (tx, _) = broadcast::channel(256);
        let state = Self {
            inner: Arc::new(Mutex::new(ReplayBuffer { buf: VecDeque::new(), tx })),
        };
        let mut rx = router.subscribe();
        let feeder = state.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => feeder.push(event_to_ws_json(&event)),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("ws feeder lagged, dropped {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        state
    }

    fn push(&self, json: String) {
        let json = Arc::new(json);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.buf.len() == REPLAY_CAPACITY {
            inner.buf.pop_front();
        }
        inner.buf.push_back(json.clone());
        let _ = inner.tx.send(json);
    }

    fn snapshot_and_subscribe(&self) -> (Vec<Arc<String>>, broadcast::Receiver<Arc<String>>) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (inner.buf.iter().cloned().collect(), inner.tx.subscribe())
    }
}

pub fn app(state: ServerState, ui_dir: Option<PathBuf>) -> axum::Router {
    let mut app = axum::Router::new().route("/ws", get(ws_handler)).with_state(state);
    if let Some(dir) = ui_dir {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)));
    }
    app
}

pub async fn serve(listener: TcpListener, state: ServerState, ui_dir: Option<PathBuf>) {
    if let Err(e) = axum::serve(listener, app(state, ui_dir)).await {
        tracing::warn!("http server terminated: {e}");
    }
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<ServerState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| client_loop(socket, state))
}

async fn client_loop(mut socket: WebSocket, state: ServerState) {
    if socket.send(Message::Text(hello_json().into())).await.is_err() {
        return;
    }
    let (snapshot, mut rx) = state.snapshot_and_subscribe();
    for json in snapshot {
        if socket
            .send(Message::Text(json.as_str().to_owned().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(json) => {
                    if socket
                        .send(Message::Text(json.as_str().to_owned().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("ws client lagged, dropped {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                // クライアント→サーバは将来の RPC 用に予約。現状は無視する
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}
```

`core/src/lib.rs` に `pub mod server;` を追加。

- [ ] **Step 5: テストが通ることを確認**

Run: `cargo test -p edlr-core --test ws_integration`
Expected: PASS(5 テスト)

Run: `cargo test --workspace`
Expected: 既存テスト含め全 PASS

- [ ] **Step 6: Commit**

```bash
git add core && git commit -m "feat(core): websocket event server with replay buffer and static serving"
```

---

### Task 2: bin への結線(--listen / --ui-dir)

**Files:**
- Modify: `core/src/bin/edlr.rs`

**Interfaces:**
- Consumes: `server::{ServerState, serve}`
- Produces: CLI フラグ `--listen <ADDR>`(既定 `127.0.0.1:8137`)、`--ui-dir <PATH>`(任意)。bind 失敗・存在しない ui-dir は stderr にメッセージを出して exit 1

- [ ] **Step 1: 実装する**(`core/src/bin/edlr.rs` の `Args` に追加)

```rust
    /// HTTP/WebSocket サーバの listen アドレス
    #[arg(long, default_value = "127.0.0.1:8137")]
    listen: std::net::SocketAddr,

    /// UI 静的ファイルのディレクトリ(指定時のみ配信)
    #[arg(long)]
    ui_dir: Option<PathBuf>,
```

`main` 内、`monitor::run` の spawn 前に追加(`use edlr_core::server;` を追加):

```rust
    if let Some(ui_dir) = &args.ui_dir {
        if !ui_dir.is_dir() {
            eprintln!("error: --ui-dir {} is not a directory", ui_dir.display());
            std::process::exit(1);
        }
    }
    let listener = match tokio::net::TcpListener::bind(args.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: failed to bind {}: {e}", args.listen);
            std::process::exit(1);
        }
    };
    tracing::info!("http/ws server listening on {}", args.listen);
    let state = server::ServerState::new(&router);
    tokio::spawn(server::serve(listener, state, args.ui_dir.clone()));
```

- [ ] **Step 2: ビルド・全テスト確認**

Run: `cargo test --workspace`
Expected: 全 PASS

- [ ] **Step 3: スモークテスト**

Run(scratchpad 配下の任意の空きディレクトリ `$DIR` を作って):
`timeout 5 cargo run -p edlr-core --bin edlr -- --journal-dir "$DIR" --listen 127.0.0.1:18137 & sleep 2; curl -s -o /dev/null -w '%{http_code}' --max-time 2 http://127.0.0.1:18137/ws; wait`
Expected: `/ws` への非 WS リクエストに 4xx(例: 426 Upgrade Required)が返る = サーバが listen している

- [ ] **Step 4: Commit**

```bash
git add core && git commit -m "feat(core): wire --listen and --ui-dir into edlr daemon"
```

---

### Task 3: フロントエンド scaffold(Vite + React + TS + vitest、タブナビ)

**Files:**
- Create: `ui/frontend/package.json`, `ui/frontend/vite.config.ts`, `ui/frontend/tsconfig.json`, `ui/frontend/index.html`
- Create: `ui/frontend/src/main.tsx`, `ui/frontend/src/App.tsx`, `ui/frontend/src/index.css`
- Create: `ui/frontend/src/pages/Dashboard.tsx`
- Create: `ui/frontend/src/test/setup.ts`, `ui/frontend/src/App.test.tsx`
- Modify: `.gitignore`, `ui/README.md`

**Interfaces:**
- Produces: `App` はタブ(Dashboard / Logs / Plugins)を持ち、`Logs`・`Plugins` ページは後続タスクが `src/pages/Logs.tsx`・`src/pages/Plugins.tsx` として実装する。このタスクでは両者を仮ページ(`<p>準備中</p>` を返す同名コンポーネント)として作成し、後続タスクが置き換える

- [ ] **Step 1: ファイル一式を書く**

`ui/frontend/package.json`:

```json
{
  "name": "edlr-frontend",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc -b && vite build",
    "test": "vitest run",
    "preview": "vite preview"
  },
  "dependencies": {
    "react": "^18.3.1",
    "react-dom": "^18.3.1"
  },
  "devDependencies": {
    "@testing-library/jest-dom": "^6.4.8",
    "@testing-library/react": "^16.0.0",
    "@types/react": "^18.3.3",
    "@types/react-dom": "^18.3.0",
    "@vitejs/plugin-react": "^4.3.1",
    "jsdom": "^25.0.0",
    "typescript": "^5.5.3",
    "vite": "^5.4.0",
    "vitest": "^2.0.5"
  }
}
```

`ui/frontend/vite.config.ts`:

```ts
/// <reference types="vitest/config" />
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: "./src/test/setup.ts",
  },
});
```

`ui/frontend/tsconfig.json`:

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "moduleResolution": "bundler",
    "jsx": "react-jsx",
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noEmit": true,
    "skipLibCheck": true,
    "types": ["vitest/globals", "@testing-library/jest-dom"]
  },
  "include": ["src"]
}
```

`ui/frontend/index.html`:

```html
<!doctype html>
<html lang="ja">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>edlr</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
```

`ui/frontend/src/main.tsx`:

```tsx
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./index.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
```

`ui/frontend/src/App.tsx`:

```tsx
import { useState } from "react";
import Dashboard from "./pages/Dashboard";
import Logs from "./pages/Logs";
import Plugins from "./pages/Plugins";

const TABS = ["Dashboard", "Logs", "Plugins"] as const;
type Tab = (typeof TABS)[number];

export default function App() {
  const [tab, setTab] = useState<Tab>("Dashboard");
  return (
    <div className="app">
      <nav className="tabs">
        {TABS.map((t) => (
          <button
            key={t}
            className={t === tab ? "tab active" : "tab"}
            onClick={() => setTab(t)}
          >
            {t}
          </button>
        ))}
      </nav>
      <main className="page">
        {tab === "Dashboard" && <Dashboard />}
        {tab === "Logs" && <Logs />}
        {tab === "Plugins" && <Plugins />}
      </main>
    </div>
  );
}
```

`ui/frontend/src/pages/Dashboard.tsx`:

```tsx
export default function Dashboard() {
  return (
    <section>
      <h1>Dashboard</h1>
      <p>プラグインで拡張可能なダッシュボード(後のフェーズで実装予定)</p>
    </section>
  );
}
```

`ui/frontend/src/pages/Logs.tsx` と `ui/frontend/src/pages/Plugins.tsx`(仮実装、後続タスクが置換):

```tsx
export default function Logs() {
  return <p>準備中</p>;
}
```

```tsx
export default function Plugins() {
  return <p>準備中</p>;
}
```

`ui/frontend/src/index.css`:

```css
:root {
  color-scheme: dark;
  font-family: system-ui, sans-serif;
}
body {
  margin: 0;
  background: #14171c;
  color: #d8dee9;
}
.app {
  display: flex;
  flex-direction: column;
  height: 100vh;
}
.tabs {
  display: flex;
  gap: 0.25rem;
  padding: 0.5rem;
  border-bottom: 1px solid #2b313b;
}
.tab {
  background: none;
  border: none;
  color: #8a93a2;
  padding: 0.4rem 0.9rem;
  cursor: pointer;
  border-radius: 4px;
}
.tab.active {
  background: #2b313b;
  color: #e8edf4;
}
.page {
  flex: 1;
  overflow: auto;
  padding: 1rem;
}
```

`ui/frontend/src/test/setup.ts`:

```ts
import "@testing-library/jest-dom/vitest";
```

`ui/frontend/src/App.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";

// userEvent は @testing-library/user-event。devDependencies に "^14.5.2" で追加すること。
test("shows dashboard placeholder by default and switches tabs", async () => {
  render(<App />);
  expect(screen.getByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Logs" }));
  expect(screen.getByText("準備中")).toBeInTheDocument();
});
```

(`package.json` の devDependencies に `"@testing-library/user-event": "^14.5.2"` も追加する)

`.gitignore`(ルート)に追記:

```
node_modules/
ui/frontend/dist/
```

`ui/README.md` を更新:

```markdown
# edlr ui

デーモンに WebSocket で接続する GUI クライアント。

- `frontend/` — React + TypeScript + Vite の SPA(Logs / Plugins / Dashboard)
- `src-tauri/` — Tauri 2 の薄い皮(ウィンドウ表示のみ)

## 開発

    cd frontend && pnpm install && pnpm dev      # http://localhost:5173
    pnpm test                                    # vitest

デーモン側は `cargo run -p edlr-core --bin edlr -- --journal-dir <PATH>` で起動しておく
(WS 既定: ws://127.0.0.1:8137/ws)。ビルド成果物をデーモンに配信させる場合は
`pnpm build` 後に `--ui-dir ui/frontend/dist` を付ける。
```

- [ ] **Step 2: インストールとテスト実行**

Run: `cd ui/frontend && pnpm install`(node が見つからない場合は `mise exec -- pnpm install`)
Run: `pnpm test`
Expected: App.test.tsx が PASS

Run: `pnpm build`
Expected: `dist/` が生成される(tsc + vite build 成功)

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(ui): scaffold React frontend with tab navigation"
```

---

### Task 4: WS クライアントと Logs 画面

**Files:**
- Create: `ui/frontend/src/ws.ts`, `ui/frontend/src/lib/filter.ts`
- Replace: `ui/frontend/src/pages/Logs.tsx`
- Create: `ui/frontend/src/ws.test.ts`, `ui/frontend/src/lib/filter.test.ts`
- Modify: `ui/frontend/src/index.css`(Logs 用スタイル追記)

**Interfaces:**
- Produces: `parseWsMessage(data: string): WsMessage | null` / `defaultWsUrl(): string` / `useEventStream(url: string): { entries: LogEntry[]; connection: ConnectionState }` / `filterEntries(entries: LogEntry[], query: string): LogEntry[]`
- `LogEntry = { id: number; kind: "journal" | "status"; timestamp?: string; event?: string; raw: unknown }`、`ConnectionState = "connecting" | "open" | "closed"`

- [ ] **Step 1: 失敗するテストを書く**

`ui/frontend/src/ws.test.ts`:

```ts
import { parseWsMessage } from "./ws";

test("parses hello", () => {
  expect(parseWsMessage('{"type":"hello","protocol":1}')).toEqual({
    type: "hello",
    protocol: 1,
  });
});

test("parses journal and status events", () => {
  const j = parseWsMessage(
    '{"type":"event","kind":"journal","timestamp":"t","event":"FSDJump","raw":{}}',
  );
  expect(j).toMatchObject({ type: "event", kind: "journal", event: "FSDJump" });
  const s = parseWsMessage('{"type":"event","kind":"status","raw":{"Flags":1}}');
  expect(s).toMatchObject({ type: "event", kind: "status" });
});

test("returns null for garbage or unknown types", () => {
  expect(parseWsMessage("not json")).toBeNull();
  expect(parseWsMessage('{"type":"mystery"}')).toBeNull();
  expect(parseWsMessage('{"type":"event","kind":"other","raw":{}}')).toBeNull();
});
```

`ui/frontend/src/lib/filter.test.ts`:

```ts
import { filterEntries, type LogEntry } from "./filter";

const entries: LogEntry[] = [
  { id: 1, kind: "journal", timestamp: "t1", event: "FSDJump", raw: { StarSystem: "Sol" } },
  { id: 2, kind: "journal", timestamp: "t2", event: "Docked", raw: { StationName: "Abraham Lincoln" } },
  { id: 3, kind: "status", raw: { Flags: 16777240 } },
];

test("empty query returns everything", () => {
  expect(filterEntries(entries, "")).toHaveLength(3);
  expect(filterEntries(entries, "  ")).toHaveLength(3);
});

test("matches event name case-insensitively", () => {
  expect(filterEntries(entries, "fsdjump").map((e) => e.id)).toEqual([1]);
});

test("matches raw JSON content", () => {
  expect(filterEntries(entries, "lincoln").map((e) => e.id)).toEqual([2]);
  expect(filterEntries(entries, "16777240").map((e) => e.id)).toEqual([3]);
});

test("status entries match by kind name", () => {
  expect(filterEntries(entries, "status").map((e) => e.id)).toEqual([3]);
});
```

- [ ] **Step 2: 失敗確認**

Run: `cd ui/frontend && pnpm test`
Expected: FAIL(モジュール未定義)

- [ ] **Step 3: 実装する**

`ui/frontend/src/lib/filter.ts`:

```ts
export interface LogEntry {
  id: number;
  kind: "journal" | "status";
  timestamp?: string;
  event?: string;
  raw: unknown;
}

export function filterEntries(entries: LogEntry[], query: string): LogEntry[] {
  const q = query.trim().toLowerCase();
  if (!q) return entries;
  return entries.filter((e) => {
    const name = (e.event ?? e.kind).toLowerCase();
    if (name.includes(q)) return true;
    return JSON.stringify(e.raw).toLowerCase().includes(q);
  });
}
```

`ui/frontend/src/ws.ts`:

```ts
import { useEffect, useRef, useState } from "react";
import type { LogEntry } from "./lib/filter";

export type WsMessage =
  | { type: "hello"; protocol: number }
  | { type: "event"; kind: "journal"; timestamp: string; event: string; raw: unknown }
  | { type: "event"; kind: "status"; raw: unknown };

export type ConnectionState = "connecting" | "open" | "closed";

const CLIENT_BUFFER_LIMIT = 2000;
const RECONNECT_DELAY_MS = 1000;

export function parseWsMessage(data: string): WsMessage | null {
  let value: unknown;
  try {
    value = JSON.parse(data);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null) return null;
  const msg = value as Record<string, unknown>;
  if (msg.type === "hello" && typeof msg.protocol === "number") {
    return { type: "hello", protocol: msg.protocol };
  }
  if (msg.type === "event" && msg.kind === "journal") {
    if (typeof msg.timestamp === "string" && typeof msg.event === "string") {
      return { type: "event", kind: "journal", timestamp: msg.timestamp, event: msg.event, raw: msg.raw };
    }
    return null;
  }
  if (msg.type === "event" && msg.kind === "status") {
    return { type: "event", kind: "status", raw: msg.raw };
  }
  return null;
}

export function defaultWsUrl(): string {
  if (window.location.protocol.startsWith("http") && window.location.host) {
    const scheme = window.location.protocol === "https:" ? "wss" : "ws";
    return `${scheme}://${window.location.host}/ws`;
  }
  // Tauri(tauri://)やテスト環境では既定のデーモンアドレスに接続する
  return "ws://127.0.0.1:8137/ws";
}

export function useEventStream(url: string): {
  entries: LogEntry[];
  connection: ConnectionState;
} {
  const [entries, setEntries] = useState<LogEntry[]>([]);
  const [connection, setConnection] = useState<ConnectionState>("connecting");
  const nextId = useRef(1);

  useEffect(() => {
    let ws: WebSocket | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let disposed = false;

    const connect = () => {
      setConnection("connecting");
      ws = new WebSocket(url);
      ws.onopen = () => setConnection("open");
      ws.onmessage = (e) => {
        const msg = parseWsMessage(String(e.data));
        if (!msg || msg.type !== "event") return;
        const entry: LogEntry =
          msg.kind === "journal"
            ? { id: nextId.current++, kind: "journal", timestamp: msg.timestamp, event: msg.event, raw: msg.raw }
            : { id: nextId.current++, kind: "status", raw: msg.raw };
        setEntries((prev) => {
          const next = [...prev, entry];
          return next.length > CLIENT_BUFFER_LIMIT
            ? next.slice(next.length - CLIENT_BUFFER_LIMIT)
            : next;
        });
      };
      ws.onclose = () => {
        setConnection("closed");
        if (!disposed) timer = setTimeout(connect, RECONNECT_DELAY_MS);
      };
      ws.onerror = () => ws?.close();
    };

    connect();
    return () => {
      disposed = true;
      if (timer) clearTimeout(timer);
      ws?.close();
    };
  }, [url]);

  return { entries, connection };
}
```

`ui/frontend/src/pages/Logs.tsx`(仮実装を置き換える):

```tsx
import { useEffect, useRef, useState } from "react";
import { filterEntries, type LogEntry } from "../lib/filter";
import { defaultWsUrl, useEventStream, type ConnectionState } from "../ws";

function ConnectionBadge({ state }: { state: ConnectionState }) {
  const label = { connecting: "接続中…", open: "接続済み", closed: "切断" }[state];
  return <span className={`badge badge-${state}`}>{label}</span>;
}

function Row({ entry }: { entry: LogEntry }) {
  const [open, setOpen] = useState(false);
  return (
    <li className="log-row" onClick={() => setOpen((o) => !o)}>
      <span className="log-time">{entry.timestamp ?? "-"}</span>
      <span className={`log-kind log-kind-${entry.kind}`}>{entry.kind}</span>
      <span className="log-event">{entry.event ?? "Status"}</span>
      {open && <pre className="log-raw">{JSON.stringify(entry.raw, null, 2)}</pre>}
    </li>
  );
}

export default function Logs() {
  const { entries, connection } = useEventStream(defaultWsUrl());
  const [query, setQuery] = useState("");
  const [follow, setFollow] = useState(true);
  const bottomRef = useRef<HTMLDivElement>(null);
  const shown = filterEntries(entries, query);

  useEffect(() => {
    if (follow) bottomRef.current?.scrollIntoView({ behavior: "auto" });
  }, [shown.length, follow]);

  return (
    <section className="logs">
      <div className="logs-toolbar">
        <input
          placeholder="フィルタ(イベント名・内容)"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <label>
          <input
            type="checkbox"
            checked={follow}
            onChange={(e) => setFollow(e.target.checked)}
          />
          自動スクロール
        </label>
        <ConnectionBadge state={connection} />
        <span className="logs-count">{shown.length} / {entries.length} 件</span>
      </div>
      <ul className="log-list">
        {shown.map((e) => (
          <Row key={e.id} entry={e} />
        ))}
      </ul>
      <div ref={bottomRef} />
    </section>
  );
}
```

`ui/frontend/src/index.css` に追記:

```css
.logs-toolbar {
  display: flex;
  gap: 0.75rem;
  align-items: center;
  margin-bottom: 0.5rem;
}
.logs-toolbar input[type="text"],
.logs-toolbar input:not([type]) {
  background: #1c2129;
  border: 1px solid #2b313b;
  color: inherit;
  padding: 0.35rem 0.6rem;
  border-radius: 4px;
  width: 18rem;
}
.badge {
  padding: 0.15rem 0.5rem;
  border-radius: 999px;
  font-size: 0.8rem;
}
.badge-open { background: #1d3b2a; color: #7fd8a2; }
.badge-connecting { background: #3b331d; color: #d8c37f; }
.badge-closed { background: #3b1d1d; color: #d87f7f; }
.log-list {
  list-style: none;
  margin: 0;
  padding: 0;
  font-family: ui-monospace, monospace;
  font-size: 0.85rem;
}
.log-row {
  display: flex;
  flex-wrap: wrap;
  gap: 0.75rem;
  padding: 0.25rem 0.5rem;
  border-bottom: 1px solid #1c2129;
  cursor: pointer;
}
.log-row:hover { background: #1c2129; }
.log-time { color: #6d7787; }
.log-kind-journal { color: #7fb3d8; }
.log-kind-status { color: #b3a1e0; }
.log-raw {
  flex-basis: 100%;
  margin: 0.25rem 0 0;
  padding: 0.5rem;
  background: #10131a;
  border-radius: 4px;
  overflow-x: auto;
}
.logs-count { margin-left: auto; color: #6d7787; }
```

- [ ] **Step 4: テストとビルド確認**

Run: `cd ui/frontend && pnpm test && pnpm build`
Expected: 全テスト PASS、ビルド成功

- [ ] **Step 5: Commit**

```bash
git add ui/frontend && git commit -m "feat(ui): log viewer with ws client, filter, and auto-scroll"
```

---

### Task 5: Plugins 画面(モックマニフェスト + 設定フォーム)

**Files:**
- Create: `ui/frontend/src/mock/plugins.ts`, `ui/frontend/src/lib/settings.ts`
- Create: `ui/frontend/src/components/PluginForm.tsx`
- Replace: `ui/frontend/src/pages/Plugins.tsx`
- Create: `ui/frontend/src/lib/settings.test.ts`, `ui/frontend/src/components/PluginForm.test.tsx`
- Modify: `ui/frontend/src/index.css`(フォーム用スタイル追記)

**Interfaces:**
- Produces: `PluginManifest` / `SettingField` 型(`src/mock/plugins.ts`)、`loadSettings(manifest) -> Record<string, unknown>` / `saveSettings(pluginId, values)`(`src/lib/settings.ts`、localStorage キー `edlr.plugin-settings.<id>`)、`<PluginForm manifest={…} />`

- [ ] **Step 1: 失敗するテストを書く**

`ui/frontend/src/lib/settings.test.ts`:

```ts
import { mockPlugins } from "../mock/plugins";
import { loadSettings, saveSettings } from "./settings";

beforeEach(() => localStorage.clear());

test("returns defaults when nothing is stored", () => {
  const manifest = mockPlugins[0];
  const values = loadSettings(manifest);
  for (const field of manifest.settings) {
    expect(values[field.key]).toEqual(field.default);
  }
});

test("stored values override defaults and survive reload", () => {
  const manifest = mockPlugins[0];
  saveSettings(manifest.id, { volume: 30 });
  const values = loadSettings(manifest);
  expect(values.volume).toBe(30);
  expect(values.enabled).toBe(true); // 未保存のキーは default
});

test("broken stored JSON falls back to defaults", () => {
  const manifest = mockPlugins[0];
  localStorage.setItem(`edlr.plugin-settings.${manifest.id}`, "{broken");
  expect(loadSettings(manifest).enabled).toBe(true);
});
```

`ui/frontend/src/components/PluginForm.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockPlugins } from "../mock/plugins";
import { loadSettings } from "../lib/settings";
import PluginForm from "./PluginForm";

beforeEach(() => localStorage.clear());

test("renders a control per setting field", () => {
  const manifest = mockPlugins[0];
  render(<PluginForm manifest={manifest} />);
  for (const field of manifest.settings) {
    expect(screen.getByLabelText(field.label)).toBeInTheDocument();
  }
});

test("changing a boolean persists to localStorage", async () => {
  const manifest = mockPlugins[0]; // enabled: default true
  render(<PluginForm manifest={manifest} />);
  await userEvent.click(screen.getByLabelText("有効"));
  expect(loadSettings(manifest).enabled).toBe(false);
});

test("changing a number persists to localStorage", async () => {
  const manifest = mockPlugins[0];
  render(<PluginForm manifest={manifest} />);
  const volume = screen.getByLabelText("音量");
  await userEvent.clear(volume);
  await userEvent.type(volume, "42");
  expect(loadSettings(manifest).volume).toBe(42);
});
```

- [ ] **Step 2: 失敗確認**

Run: `cd ui/frontend && pnpm test`
Expected: 新規テストが FAIL(モジュール未定義)

- [ ] **Step 3: 実装する**

`ui/frontend/src/mock/plugins.ts`:

```ts
// プラグイン基盤(wasmtime + マニフェスト)実装までのモックデータ。
// この型が将来のマニフェストスキーマの叩き台になる。
export type SettingField =
  | { key: string; label: string; type: "boolean"; default: boolean }
  | { key: string; label: string; type: "string"; default: string }
  | { key: string; label: string; type: "number"; default: number }
  | { key: string; label: string; type: "select"; options: string[]; default: string };

export interface PluginManifest {
  id: string;
  name: string;
  description: string;
  settings: SettingField[];
}

export const mockPlugins: PluginManifest[] = [
  {
    id: "voice-notify",
    name: "Voice Notify",
    description: "ジャンプ・ドッキング等のイベントを音声で通知する(モック)",
    settings: [
      { key: "enabled", label: "有効", type: "boolean", default: true },
      { key: "voice", label: "音声", type: "select", options: ["Amber", "Blue"], default: "Amber" },
      { key: "volume", label: "音量", type: "number", default: 80 },
    ],
  },
  {
    id: "translator",
    name: "Translator",
    description: "受信テキストを翻訳パイプラインへ送る(モック)",
    settings: [
      { key: "enabled", label: "有効", type: "boolean", default: false },
      { key: "endpoint", label: "エンドポイント", type: "string", default: "http://localhost:5000" },
    ],
  },
];
```

`ui/frontend/src/lib/settings.ts`:

```ts
import type { PluginManifest } from "../mock/plugins";

const KEY_PREFIX = "edlr.plugin-settings.";

export function loadSettings(manifest: PluginManifest): Record<string, unknown> {
  const defaults: Record<string, unknown> = {};
  for (const field of manifest.settings) {
    defaults[field.key] = field.default;
  }
  const stored = localStorage.getItem(KEY_PREFIX + manifest.id);
  if (!stored) return defaults;
  try {
    const parsed = JSON.parse(stored);
    if (typeof parsed !== "object" || parsed === null) return defaults;
    return { ...defaults, ...(parsed as Record<string, unknown>) };
  } catch {
    return defaults;
  }
}

export function saveSettings(pluginId: string, values: Record<string, unknown>): void {
  const key = KEY_PREFIX + pluginId;
  const stored = localStorage.getItem(key);
  let current: Record<string, unknown> = {};
  if (stored) {
    try {
      const parsed = JSON.parse(stored);
      if (typeof parsed === "object" && parsed !== null) {
        current = parsed as Record<string, unknown>;
      }
    } catch {
      // 壊れた保存値は捨てて上書きする
    }
  }
  localStorage.setItem(key, JSON.stringify({ ...current, ...values }));
}
```

`ui/frontend/src/components/PluginForm.tsx`:

```tsx
import { useState } from "react";
import { loadSettings, saveSettings } from "../lib/settings";
import type { PluginManifest, SettingField } from "../mock/plugins";

function Field({
  field,
  value,
  onChange,
}: {
  field: SettingField;
  value: unknown;
  onChange: (v: unknown) => void;
}) {
  const id = `field-${field.key}`;
  switch (field.type) {
    case "boolean":
      return (
        <label htmlFor={id} className="form-row">
          <span>{field.label}</span>
          <input
            id={id}
            type="checkbox"
            checked={Boolean(value)}
            onChange={(e) => onChange(e.target.checked)}
          />
        </label>
      );
    case "string":
      return (
        <label htmlFor={id} className="form-row">
          <span>{field.label}</span>
          <input
            id={id}
            type="text"
            value={String(value ?? "")}
            onChange={(e) => onChange(e.target.value)}
          />
        </label>
      );
    case "number":
      return (
        <label htmlFor={id} className="form-row">
          <span>{field.label}</span>
          <input
            id={id}
            type="number"
            value={Number(value ?? 0)}
            onChange={(e) => onChange(Number(e.target.value))}
          />
        </label>
      );
    case "select":
      return (
        <label htmlFor={id} className="form-row">
          <span>{field.label}</span>
          <select
            id={id}
            value={String(value)}
            onChange={(e) => onChange(e.target.value)}
          >
            {field.options.map((o) => (
              <option key={o} value={o}>
                {o}
              </option>
            ))}
          </select>
        </label>
      );
  }
}

export default function PluginForm({ manifest }: { manifest: PluginManifest }) {
  const [values, setValues] = useState<Record<string, unknown>>(() =>
    loadSettings(manifest),
  );
  const update = (key: string, value: unknown) => {
    const next = { ...values, [key]: value };
    setValues(next);
    saveSettings(manifest.id, next);
  };
  return (
    <form className="plugin-form" onSubmit={(e) => e.preventDefault()}>
      {manifest.settings.map((field) => (
        <Field
          key={field.key}
          field={field}
          value={values[field.key]}
          onChange={(v) => update(field.key, v)}
        />
      ))}
    </form>
  );
}
```

`ui/frontend/src/pages/Plugins.tsx`(仮実装を置き換える):

```tsx
import PluginForm from "../components/PluginForm";
import { mockPlugins } from "../mock/plugins";

export default function Plugins() {
  return (
    <section>
      <h1>Plugins</h1>
      <p className="note">
        ※ 現在はモックデータです。プラグイン基盤の実装後に本物のマニフェストと接続します。
      </p>
      {mockPlugins.map((p) => (
        <article key={p.id} className="plugin-card">
          <h2>{p.name}</h2>
          <p>{p.description}</p>
          <PluginForm manifest={p} />
        </article>
      ))}
    </section>
  );
}
```

`ui/frontend/src/index.css` に追記:

```css
.note { color: #8a93a2; font-size: 0.9rem; }
.plugin-card {
  background: #1c2129;
  border: 1px solid #2b313b;
  border-radius: 8px;
  padding: 1rem 1.25rem;
  margin-bottom: 1rem;
  max-width: 40rem;
}
.plugin-card h2 { margin-top: 0; }
.form-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 1rem;
  padding: 0.35rem 0;
}
.form-row input[type="text"],
.form-row input[type="number"],
.form-row select {
  background: #14171c;
  border: 1px solid #2b313b;
  color: inherit;
  padding: 0.3rem 0.5rem;
  border-radius: 4px;
  width: 14rem;
}
```

- [ ] **Step 4: テストとビルド確認**

Run: `cd ui/frontend && pnpm test && pnpm build`
Expected: 全テスト PASS、ビルド成功

- [ ] **Step 5: Commit**

```bash
git add ui/frontend && git commit -m "feat(ui): plugin settings screen with mock manifests"
```

---

### Task 6: Tauri 2 の薄い皮(scaffold、ビルド検証は条件付き)

**Files:**
- Create: `ui/src-tauri/Cargo.toml`, `ui/src-tauri/build.rs`, `ui/src-tauri/src/main.rs`
- Create: `ui/src-tauri/tauri.conf.json`, `ui/src-tauri/icons/icon.png`
- Modify: `.gitignore`(`ui/src-tauri/target/` 追加)

**Interfaces:**
- Produces: `cargo tauri dev` / `cargo build` 可能な Tauri プロジェクト(ルート workspace からは独立)。フロントは `../frontend/dist` をバンドル、dev 時は `http://localhost:5173`

- [ ] **Step 1: ファイル一式を書く**

`ui/src-tauri/Cargo.toml`(`[workspace]` 空テーブルでルート workspace から除外する — システム依存の無い環境で `cargo test --workspace` を壊さないため):

```toml
[package]
name = "edlr-ui"
version = "0.1.0"
edition = "2021"

# ルート workspace から独立させる(webkit2gtk 等のシステム依存を隔離)
[workspace]

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = [] }
```

`ui/src-tauri/build.rs`:

```rust
fn main() {
    tauri_build::build()
}
```

`ui/src-tauri/src/main.rs`:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // ウィンドウを出してフロントエンドを表示するだけの薄い皮。
    // デーモンへの接続はフロントエンド側の WebSocket が担う。
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

`ui/src-tauri/tauri.conf.json`:

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "edlr",
  "version": "0.1.0",
  "identifier": "dev.himanoa.edlr",
  "build": {
    "beforeDevCommand": "pnpm --dir ../frontend dev",
    "devUrl": "http://localhost:5173",
    "beforeBuildCommand": "pnpm --dir ../frontend build",
    "frontendDist": "../frontend/dist"
  },
  "app": {
    "windows": [{ "title": "edlr", "width": 1100, "height": 750 }],
    "security": { "csp": null }
  },
  "bundle": {
    "active": true,
    "targets": "all",
    "icon": ["icons/icon.png"]
  }
}
```

`ui/src-tauri/icons/icon.png` は python3 で生成する(単色 32x32 RGBA、仮アイコン):

```bash
mkdir -p ui/src-tauri/icons && python3 - <<'EOF'
import struct, zlib
w = h = 32
row = b"\x00" + bytes([232, 130, 60, 255]) * w  # RGBA orange
raw = row * h
def chunk(t, d):
    c = struct.pack(">I", len(d)) + t + d
    return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
png = b"\x89PNG\r\n\x1a\n"
png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(raw))
png += chunk(b"IEND", b"")
open("ui/src-tauri/icons/icon.png", "wb").write(png)
EOF
```

`.gitignore` に `ui/src-tauri/target/` を追記。

- [ ] **Step 2: ビルド検証(条件付き)**

Run: `pkg-config --exists webkit2gtk-4.1 && echo deps-ok || echo deps-missing`

- `deps-ok` の場合: `cd ui/src-tauri && cargo build` を実行し、成功を確認する
- `deps-missing` の場合: ビルドはスキップし、`tauri.conf.json` が正しい JSON であること(`python3 -m json.tool ui/src-tauri/tauri.conf.json`)と、フロントの `pnpm build` が成功していることだけ確認して、レポートに「Tauri ビルドはシステム依存(libwebkit2gtk-4.1-dev 等)未導入のため未検証」と明記し DONE_WITH_CONCERNS で報告する

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(ui): tauri 2 shell bundling the frontend"
```

---

### Task 7: 仕上げ(fmt / clippy / tsc / README)

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Rust 側の検証**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: すべて成功(警告ゼロ)。指摘があれば機械的修正のみ行う

- [ ] **Step 2: フロント側の検証**

Run: `cd ui/frontend && pnpm test && pnpm build`
Expected: 全テスト PASS、ビルド成功

- [ ] **Step 3: ルート `README.md` の「構成」「使い方」を更新する**

「構成」の `ui/` 行を以下に置き換え:

```markdown
- `ui/` — GUI クライアント。`frontend/`(React + Vite の SPA: Logs / Plugins / Dashboard)と
  `src-tauri/`(Tauri 2 の薄い皮)。デーモンとは WebSocket(既定 `ws://127.0.0.1:8137/ws`)で通信
```

「使い方」の末尾に追記:

```markdown
## UI

    # デーモン(WS サーバ込み)を起動
    cargo run -p edlr-core --bin edlr -- --journal-dir <PATH>

    # ブラウザ版(開発)
    cd ui/frontend && pnpm install && pnpm dev   # http://localhost:5173

    # デーモンに静的配信させる場合
    cd ui/frontend && pnpm build
    cargo run -p edlr-core --bin edlr -- --journal-dir <PATH> --ui-dir ui/frontend/dist

    # Tauri(要 libwebkit2gtk-4.1-dev ほかシステム依存)
    cd ui/src-tauri && cargo tauri dev
```

- [ ] **Step 4: 最終確認と Commit**

Run: `cargo test --workspace && cd ui/frontend && pnpm test`
Expected: 全 PASS

```bash
git add -A && git commit -m "chore: fmt, clippy, update README for UI phase 1"
```
