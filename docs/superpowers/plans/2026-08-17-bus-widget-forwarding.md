# bus ウィジェット転送 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** ドライバの bus emit をダッシュボードウィジェットへリアルタイム転送し、eddn-sender に EDDN アップロード状況ウィジェットを追加する。

**Architecture:** `Bus::emit` にタップを 1 本追加してデーモンが WS フレーム `kind:"bus"` として既存の ReplayBuffer + broadcast 経路に合流させる。マウント直後の初期表示用に read-only RPC `drivers/bus-retained` を追加。フロントは `WidgetApi` に `onBus` / `retained` を足し、ウィジェット側で driver/topic をフィルタする（manifest 宣言・per-widget 配送制御はしない）。

**Tech Stack:** Rust（edlr workspace: `drivers/channel` は tokio 非依存、core は tokio + serde_json）、React + TypeScript + vitest（ui/frontend）、素の ESM ウィジェット。

**Spec:** `docs/superpowers/specs/2026-08-17-bus-widget-forwarding-design.md`

## Global Constraints

- `drivers/channel` クレートは wasmtime にも tokio にも依存させない（クレート先頭のドキュメントコメント参照）
- payload は UTF-8 文字列として運ぶ。非 UTF-8 は `String::from_utf8_lossy` で妥協
- コメント・テスト名は既存コードの流儀（日本語コメント、挙動を文で説明するテスト名）に合わせる
- Rust テスト: `cargo test -p edlr-driver-channel` / `cargo test -p edlr-core`、フロント: `pnpm -C ui/frontend test`（= `vitest run`）
- Task 1–5 は edlr リポジトリ、Task 6 は `../edlr-plugin-eddn-sender` リポジトリで作業・コミットする

---

### Task 1: Bus タップ（drivers/channel）

**Files:**
- Modify: `drivers/channel/src/lib.rs`（`BusState` 定義付近、`emit` 本体、テストは同ファイル末尾の `mod tests`）

**Interfaces:**
- Produces: `Bus::set_tap(&self, tap: impl Fn(&str, &str, &[u8]) + Send + Sync + 'static)` — emit 成功のたびに `(driver_id, topic, payload)` で呼ばれる。Task 2 の bin 配線が使う。

- [ ] **Step 1: 失敗するテストを書く**

`drivers/channel/src/lib.rs` の `mod tests` に追加（既存テスト `emit_updates_retained_and_delivers_to_subscribers` の隣）:

```rust
#[test]
fn emit_calls_the_tap_with_driver_topic_and_payload() {
    let bus = Bus::new();
    let (tx, _rx) = std::sync::mpsc::sync_channel::<Message>(4);
    bus.register_driver(
        "eddn",
        vec![TopicSpec {
            name: "upload-status".into(),
            retain: true,
            description: String::new(),
        }],
        tx,
    );
    let seen = Arc::new(Mutex::new(Vec::<(String, String, Vec<u8>)>::new()));
    let seen_in_tap = seen.clone();
    bus.set_tap(move |driver, topic, payload| {
        seen_in_tap
            .lock()
            .unwrap()
            .push((driver.to_string(), topic.to_string(), payload.to_vec()));
    });
    bus.emit("eddn", "upload-status", b"{\"ok\":true}".to_vec()).unwrap();
    // 失敗した emit(未宣言トピック)ではタップを呼ばない
    bus.emit("eddn", "nope", b"x".to_vec()).unwrap_err();
    let seen = seen.lock().unwrap();
    assert_eq!(
        *seen,
        vec![("eddn".to_string(), "upload-status".to_string(), b"{\"ok\":true}".to_vec())]
    );
}
```

- [ ] **Step 2: 落ちることを確認**

Run: `cargo test -p edlr-driver-channel emit_calls_the_tap`
Expected: コンパイルエラー（`set_tap` が存在しない）

- [ ] **Step 3: 実装**

`BusState` にフィールド追加と `set_tap`、`emit` の末尾でタップ呼び出し:

```rust
/// emit 成功を外へ知らせるタップ(UI への WS 転送用)。バスのロック外で
/// 呼ぶ。承認判定を持たない本クレートの分担どおり、タップにも判定はない。
type BusTap = Arc<dyn Fn(&str, &str, &[u8]) + Send + Sync>;
```

`BusState`（`#[derive(Default)]` のまま）に `tap: Option<BusTap>` を追加。

```rust
    /// emit 成功のたびに `(driver_id, topic, payload)` で呼ばれるタップを
    /// 設定する。設定は 1 本だけ(2 回目は上書き)。
    pub fn set_tap(&self, tap: impl Fn(&str, &str, &[u8]) + Send + Sync + 'static) {
        self.lock_state().tap = Some(Arc::new(tap));
    }
```

`emit` の末尾（stale 購読の刈り取り後、`Ok(())` の直前）:

```rust
        // タップはロック外で呼ぶ(タップ実装がバスを再度触っても死なない)。
        let tap = state.tap.clone();
        drop(state);
        if let Some(tap) = tap {
            tap(driver_id, topic, &payload);
        }
        Ok(())
```

注意: `emit` 内で `payload` を move している箇所（`retained.insert` / `Delivery`）は全て `payload.clone()` になっているので、末尾でも `&payload` が使えることを確認する。stale 刈り取りが `Ok(())` より後ろにある場合はタップ呼び出しをその後に置く。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-driver-channel`
Expected: 全 PASS

- [ ] **Step 5: コミット**

```bash
git add drivers/channel/src/lib.rs
git commit -m "feat: Bus に emit タップを追加(UI への WS 転送用)"
```

---

### Task 2: bus WS フレームと bin 配線（core）

**Files:**
- Modify: `core/src/server/mod.rs`（`event_to_ws_json` 付近にフレーム組み立て関数を追加）
- Modify: `core/src/bin/edlr.rs`（`Bus::new()` 付近と `attach_log_stream` 付近）
- Test: `core/src/server/tests.rs`

**Interfaces:**
- Consumes: Task 1 の `Bus::set_tap`
- Produces: `pub fn bus_ws_frame(driver: &str, topic: &str, payload: &[u8]) -> String` — WS フレーム JSON。Task 4 のフロントがこの形をパースする: `{"type":"event","kind":"bus","driver":…,"topic":…,"payload":…}`

- [ ] **Step 1: 失敗するテストを書く**

`core/src/server/tests.rs` に追加:

```rust
/// bus フレームは kind="bus" で driver/topic/payload を運ぶ。payload は
/// UTF-8 文字列(非 UTF-8 は lossy)。
#[test]
fn bus_ws_frame_carries_driver_topic_and_lossy_payload() {
    let frame = bus_ws_frame("eddn", "upload-status", b"{\"ok\":true}");
    let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(v["type"], "event");
    assert_eq!(v["kind"], "bus");
    assert_eq!(v["driver"], "eddn");
    assert_eq!(v["topic"], "upload-status");
    assert_eq!(v["payload"], "{\"ok\":true}");

    let frame = bus_ws_frame("eddn", "upload-status", &[0xff, 0xfe]);
    let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(v["payload"], "\u{fffd}\u{fffd}");
}
```

`tests.rs` 冒頭の `use super::*;`（あるいは既存の import 流儀）で `bus_ws_frame` が見えることを確認。

- [ ] **Step 2: 落ちることを確認**

Run: `cargo test -p edlr-core bus_ws_frame`
Expected: コンパイルエラー（`bus_ws_frame` が存在しない）

- [ ] **Step 3: 実装**

`core/src/server/mod.rs` の `event_to_ws_json` の直後に:

```rust
/// bus emit 1 件を WS フレーム JSON にする。UI は既に全ログ・全 journal
/// イベントを見られる承認サーフェスなので、bus フレームも全クライアントへ
/// 流す(設計書の承認モデル参照)。payload は UTF-8 文字列(lossy)。
pub fn bus_ws_frame(driver: &str, topic: &str, payload: &[u8]) -> String {
    serde_json::json!({
        "type": "event",
        "kind": "bus",
        "driver": driver,
        "topic": topic,
        "payload": String::from_utf8_lossy(payload),
    })
    .to_string()
}
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core bus_ws_frame`
Expected: PASS

- [ ] **Step 5: bin 配線**

`core/src/bin/edlr.rs` で:

1. `let bus = edlr_driver_channel::Bus::new();`（263 行付近）の直後に:

```rust
            // bus emit を WS へ転送するタップ。attach は ServerState 構築後
            // (下記)。それまでの emit は broadcast のバッファ(256)に乗る。
            let (bus_frames_tx, bus_frames_rx) =
                tokio::sync::broadcast::channel::<std::sync::Arc<String>>(256);
            {
                let tx = bus_frames_tx.clone();
                bus.set_tap(move |driver, topic, payload| {
                    let _ = tx.send(std::sync::Arc::new(
                        edlr_core::server::bus_ws_frame(driver, topic, payload),
                    ));
                });
            }
```

（`bus_frames_tx` 本体は以後未使用なら `_bus_frames_tx` にせず、クロージャに move した clone だけ残す形で `let (tx, bus_frames_rx) = …` としてもよい。既存コードの命名に合わせる。）

2. `state.attach_log_stream(log_rx);`（321 行付近）の直後に:

```rust
    state.attach_log_stream(bus_frames_rx);
```

`attach_log_stream` は「フレームを ReplayBuffer + broadcast に合流させる」汎用口なのでそのまま使う。`bus_frames_rx` が spawn_blocking ブロックの中で作られてスコープ外なら、チャネル生成を `Bus::new()` と同じスコープの外側（`log_rx` と同様に後で使える位置）へ引き上げる。

- [ ] **Step 6: ビルドとテスト**

Run: `cargo build -p edlr-core && cargo test -p edlr-core`
Expected: ビルド成功、全 PASS

- [ ] **Step 7: コミット**

```bash
git add core/src/server/mod.rs core/src/server/tests.rs core/src/bin/edlr.rs
git commit -m "feat: bus emit を WS フレームとして UI へ転送"
```

---

### Task 3: RPC drivers/bus-retained（core）

**Files:**
- Modify: `core/src/registry/driver.rs`（`pub fn` 群に 1 メソッド追加）
- Modify: `core/src/server/rpc_drivers.rs`（ハンドラ追加）
- Modify: `core/src/server/mod.rs`（`handle_drivers_rpc` の match に 1 行）
- Test: `core/src/server/tests.rs`

**Interfaces:**
- Consumes: 既存 `Bus::retained_for(driver_id, topic) -> Option<Vec<u8>>`
- Produces: RPC `drivers/bus-retained`、params `{driver: string, topic: string}` → 結果 `{"payload": string | null}`。Task 5 の `rpc.ts` が呼ぶ。

- [ ] **Step 1: 失敗するテストを書く**

`core/src/server/tests.rs` に追加:

```rust
/// drivers/bus-retained は retain 済みの最新値を返し、未保持なら null。
#[test]
fn drivers_bus_retained_returns_the_value_or_null() {
    let bus = edlr_driver_channel::Bus::new();
    let (tx, _rx) = std::sync::mpsc::sync_channel::<edlr_driver_channel::Message>(4);
    bus.register_driver(
        "eddn",
        vec![edlr_driver_channel::TopicSpec {
            name: "upload-status".into(),
            retain: true,
            description: String::new(),
        }],
        tx,
    );
    bus.emit("eddn", "upload-status", b"{\"ok\":true}".to_vec()).unwrap();
    let drivers = crate::registry::driver::tests::test_registry(bus);

    let result = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/bus-retained",
        &serde_json::json!({"driver": "eddn", "topic": "upload-status"}),
    )
    .unwrap();
    assert_eq!(result["payload"], "{\"ok\":true}");

    let result = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/bus-retained",
        &serde_json::json!({"driver": "eddn", "topic": "nope"}),
    )
    .unwrap();
    assert_eq!(result["payload"], serde_json::Value::Null);
}
```

（`handle_rpc_with_drivers` は `drivers/` プレフィックスを registry より先に処理するので `registry = None` でよい — dispatch の実装参照。）

- [ ] **Step 2: 落ちることを確認**

Run: `cargo test -p edlr-core drivers_bus_retained`
Expected: FAIL（`unknown method: drivers/bus-retained`）

- [ ] **Step 3: 実装**

`core/src/registry/driver.rs` の `pub fn` 群（`filesystem` の近く）に:

```rust
    /// UI ウィジェット向け: driver/topic の retained 値。未保持・未知の
    /// driver/topic は None(エラーにしない -- 表示側は「値なし」扱い)。
    pub fn bus_retained(&self, driver_id: &str, topic: &str) -> Option<Vec<u8>> {
        self.bus.retained_for(driver_id, topic)
    }
```

`core/src/server/rpc_drivers.rs` に:

```rust
pub(super) fn bus_retained(
    drivers: &DriverRegistry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let driver = param_str(params, "driver")?;
    let topic = param_str(params, "topic")?;
    let payload = drivers
        .bus_retained(driver, topic)
        .map(|p| String::from_utf8_lossy(&p).into_owned());
    Ok(serde_json::json!({ "payload": payload }))
}
```

`core/src/server/mod.rs` の `handle_drivers_rpc` の match に:

```rust
        "bus-retained" => rpc_drivers::bus_retained(drivers, params),
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core`
Expected: 全 PASS

- [ ] **Step 5: コミット**

```bash
git add core/src/registry/driver.rs core/src/server/rpc_drivers.rs core/src/server/mod.rs core/src/server/tests.rs
git commit -m "feat: drivers/bus-retained RPC を追加"
```

---

### Task 4: フロント WS パースと Logs 表示（ui/frontend）

**Files:**
- Modify: `ui/frontend/src/ws.ts`（`WsMessage` 型・`parseWsMessage`・`useEventStream` の変換）
- Modify: `ui/frontend/src/lib/filter.ts`（`LogEntry` 型）
- Modify: `ui/frontend/src/pages/Logs.tsx`（`KINDS` に "bus" 追加、`KIND_STYLE` に 1 エントリ）
- Test: `ui/frontend/src/ws.test.ts`

**Interfaces:**
- Consumes: Task 2 の WS フレーム形
- Produces: `LogEntry` の新バリアント。Task 5 の WidgetHost がこれで bus 配送する:
  `{ kind: "bus"; driver: string; topic: string; payload: string; event: string /* "driver/topic" */; raw: unknown }`

- [ ] **Step 1: 失敗するテストを書く**

`ui/frontend/src/ws.test.ts` に追加（既存の `parseWsMessage` テストの隣）:

```ts
it("parses a bus frame", () => {
  const msg = parseWsMessage(
    JSON.stringify({
      type: "event",
      kind: "bus",
      driver: "eddn",
      topic: "upload-status",
      payload: '{"ok":true}',
    }),
  );
  expect(msg).toEqual({
    type: "event",
    kind: "bus",
    driver: "eddn",
    topic: "upload-status",
    payload: '{"ok":true}',
  });
});

it("rejects a bus frame with a missing field", () => {
  expect(
    parseWsMessage(JSON.stringify({ type: "event", kind: "bus", driver: "eddn" })),
  ).toBeNull();
});
```

- [ ] **Step 2: 落ちることを確認**

Run: `pnpm -C ui/frontend test`
Expected: 追加 2 ケースが FAIL

- [ ] **Step 3: 実装**

`ws.ts` の `WsMessage` union に追加:

```ts
  | { type: "event"; kind: "bus"; driver: string; topic: string; payload: string }
```

`parseWsMessage` の `kind === "log"` 分岐の後に:

```ts
  if (msg.type === "event" && msg.kind === "bus") {
    if (
      typeof msg.driver === "string" &&
      typeof msg.topic === "string" &&
      typeof msg.payload === "string"
    ) {
      return { type: "event", kind: "bus", driver: msg.driver, topic: msg.topic, payload: msg.payload };
    }
    return null;
  }
```

`lib/filter.ts` の `LogEntry`:

```ts
  kind: "journal" | "status" | "log" | "bus";
  /** kind === "bus" のみ: 送信元ドライバ id / トピック名 / UTF-8 payload */
  driver?: string;
  topic?: string;
  payload?: string;
```

`useEventStream` の変換に分岐追加（`kind === "log"` の後）。`event` に `"driver/topic"` を入れると Logs の行表示（`entry.event ?? "Status"`）と検索がそのまま効く:

```ts
              : msg.kind === "bus"
                ? {
                    id: nextId.current++,
                    kind: "bus",
                    driver: msg.driver,
                    topic: msg.topic,
                    payload: msg.payload,
                    event: `${msg.driver}/${msg.topic}`,
                    // raw 展開(クリック時)で payload を JSON として読めるように
                    raw: { driver: msg.driver, topic: msg.topic, payload: msg.payload },
                  }
                : { id: nextId.current++, kind: "status", raw: msg.raw };
```

`Logs.tsx`: `const KINDS = ["journal", "status", "log", "bus"] as const;` に変更し、`KIND_STYLE` に `bus:` のエントリを既存の配色の流儀で 1 つ足す。

- [ ] **Step 4: テストが通ることを確認**

Run: `pnpm -C ui/frontend test`
Expected: 全 PASS（既存テスト含む）

- [ ] **Step 5: コミット**

```bash
git add ui/frontend/src/ws.ts ui/frontend/src/ws.test.ts ui/frontend/src/lib/filter.ts ui/frontend/src/pages/Logs.tsx
git commit -m "feat: bus WS フレームのパースと Logs 表示"
```

---

### Task 5: WidgetApi 拡張 onBus / retained(ui/frontend)

**Files:**
- Modify: `ui/frontend/src/rpc.ts`（`busRetained` 追加）
- Modify: `ui/frontend/src/components/WidgetHost.tsx`（`WidgetApi` + 配送）
- Test: `ui/frontend/src/components/WidgetHost.test.tsx`

**Interfaces:**
- Consumes: Task 3 の RPC `drivers/bus-retained` → `{payload: string | null}`、Task 4 の `LogEntry`（`kind: "bus"`）
- Produces: ウィジェットが使う API（Task 6 が呼ぶ）:
  - `onBus(cb: (msg: { driver: string; topic: string; payload: string }) => void): void`
  - `retained(driver: string, topic: string): Promise<string | null>`

- [ ] **Step 1: 失敗するテストを書く**

`WidgetHost.test.tsx` に追加:

```ts
it("delivers bus frames to onBus listeners regardless of the events filter", async () => {
  const received: Array<{ driver: string; topic: string; payload: string }> = [];
  const load = async () => ({
    default: (el: HTMLElement, api: WidgetApi) => {
      el.textContent = "widget body";
      api.onBus((msg) => received.push(msg));
    },
  });
  const busEntry: LogEntry = {
    id: 1,
    kind: "bus",
    driver: "eddn",
    topic: "upload-status",
    payload: '{"ok":true}',
    event: "eddn/upload-status",
    raw: {},
  };
  // entry.events は ["FSDJump"] のままでも bus フレームは届く
  render(<WidgetHost entry={entry} entries={[busEntry, jump]} load={load} />);
  await screen.findByText("widget body");
  await waitFor(() => expect(received).toHaveLength(1));
  expect(received[0]).toEqual({ driver: "eddn", topic: "upload-status", payload: '{"ok":true}' });
});
```

- [ ] **Step 2: 落ちることを確認**

Run: `pnpm -C ui/frontend test`
Expected: 追加ケースが FAIL（`onBus` が存在しない）

- [ ] **Step 3: rpc.ts に busRetained を追加**

`RpcClient` のメソッド群（`dashboardList` の近く）に:

```ts
  /** driver/topic の retained 値。未保持なら null。 */
  busRetained(driver: string, topic: string): Promise<string | null> {
    return this.call<{ payload: string | null }>("drivers/bus-retained", {
      driver,
      topic,
    }).then((r) => r.payload);
  }
```

- [ ] **Step 4: WidgetHost を実装**

`WidgetApi` に追加:

```ts
  /**
   * bus フレームの購読。全 driver/topic が届くのでウィジェット側でフィルタ
   * する(manifest 宣言はしない -- 設計書の承認モデル参照)。mount 中に
   * 同期的に登録すること(onEvent と同じ)。
   */
  onBus(cb: (msg: { driver: string; topic: string; payload: string }) => void): void;
  /** retained 値の取得。未保持なら null、RPC 失敗は reject。 */
  retained(driver: string, topic: string): Promise<string | null>;
```

実装（`listeners` の隣に `busListeners` を持つ）:

```ts
  const busListeners = useRef<Array<(msg: { driver: string; topic: string; payload: string }) => void>>([]);
```

`api` オブジェクトに:

```ts
      onBus(cb) {
        busListeners.current.push(cb);
      },
      retained(driver, topic) {
        const rpc = rpcRef.current;
        if (!rpc) return Promise.reject(new Error("rpc unavailable"));
        return rpc.busRetained(driver, topic);
      },
```

cleanup（`listeners.current = []` の隣）で `busListeners.current = [];`。

配送 useEffect の for ループ内、`matchesEvent` の前に bus 分岐を追加:

```ts
      if (log.kind === "bus") {
        const msg = { driver: log.driver ?? "", topic: log.topic ?? "", payload: log.payload ?? "" };
        for (const cb of busListeners.current) {
          try {
            cb(msg);
          } catch (err) {
            console.error(`widget ${entry.plugin}/${entry.widget} bus listener failed:`, err);
          }
        }
        continue;
      }
```

- [ ] **Step 5: テストが通ることを確認**

Run: `pnpm -C ui/frontend test`
Expected: 全 PASS

- [ ] **Step 6: コミット**

```bash
git add ui/frontend/src/rpc.ts ui/frontend/src/components/WidgetHost.tsx ui/frontend/src/components/WidgetHost.test.tsx
git commit -m "feat: WidgetApi に onBus / retained を追加"
```

---

### Task 6: eddn-sender ウィジェット（edlr-plugin-eddn-sender リポジトリ）

**Files:**
- Modify: `../edlr-plugin-eddn-sender/manifest.toml`
- Create: `../edlr-plugin-eddn-sender/ui/upload-status/index.js`

**Interfaces:**
- Consumes: Task 5 の `api.onBus` / `api.retained`。eddn ドライバの `upload-status` payload は `{ok: bool, status?: number, error?: string, schema: string}` の JSON 文字列。
- Produces: ダッシュボードウィジェット `eddn-sender/upload-status`

- [ ] **Step 1: manifest に [[dashboard]] を追加**

`manifest.toml` 末尾に:

```toml
[[dashboard]]
id = "upload-status"
title = "EDDN Upload"
entry = "ui/upload-status/index.js"
size = "small"
```

- [ ] **Step 2: ウィジェットを書く**

`ui/upload-status/index.js`（wasm 無変更。データは UI ストリーム経由で届くため `[[bus]]` subscribe も不要）:

```js
// EDDN の upload-status(retain)を表示する。初期値は retained RPC、以降は
// bus フレームで更新。ライブ更新が先に来たら retained の結果で巻き戻さない。
export default function mount(el, api) {
  el.innerHTML = `
    <div><span data-badge class="rounded px-1.5 text-sm font-semibold">—</span>
      <span data-schema class="ml-1 text-sm text-muted-foreground"></span></div>
    <div class="text-sm" data-detail>アップロード待ち</div>
    <div class="text-xs text-muted-foreground" data-time></div>
  `;
  const badge = el.querySelector("[data-badge]");
  const schema = el.querySelector("[data-schema]");
  const detail = el.querySelector("[data-detail]");
  const time = el.querySelector("[data-time]");

  let live = false;
  const render = (payload) => {
    let st;
    try {
      st = JSON.parse(payload);
    } catch {
      detail.textContent = payload; // パース失敗は生文字列をそのまま出す
      return;
    }
    badge.textContent = st.ok ? "OK" : "FAIL";
    badge.className = `rounded px-1.5 text-sm font-semibold ${
      st.ok ? "bg-green-950 text-green-400" : "bg-red-950 text-red-400"
    }`;
    schema.textContent = st.schema ?? "";
    detail.textContent = st.ok ? `HTTP ${st.status ?? "?"}` : (st.error ?? "失敗");
    time.textContent = new Date().toLocaleTimeString();
  };

  api.onBus((msg) => {
    if (msg.driver !== "eddn" || msg.topic !== "upload-status") return;
    live = true;
    render(msg.payload);
  });
  api
    .retained("eddn", "upload-status")
    .then((payload) => {
      if (!live && payload !== null) render(payload);
    })
    .catch(() => {
      if (!live) detail.textContent = "取得失敗";
    });
}
```

- [ ] **Step 3: デプロイして目視確認**

```bash
cp ../edlr-plugin-eddn-sender/manifest.toml ~/.config/edlr/plugins/eddn-sender/
mkdir -p ~/.config/edlr/plugins/eddn-sender/ui/upload-status
cp ../edlr-plugin-eddn-sender/ui/upload-status/index.js ~/.config/edlr/plugins/eddn-sender/ui/upload-status/
```

edlr デーモンを再起動し、UI の Dashboard セクションで `EDDN Upload` を承認して表示されること（retained があれば即表示、なければ「アップロード待ち」）を確認。

- [ ] **Step 4: コミット（edlr-plugin-eddn-sender リポジトリ）**

```bash
cd ../edlr-plugin-eddn-sender
git add manifest.toml ui/upload-status/index.js
git commit -m "feat: upload-status ダッシュボードウィジェットを追加"
```
