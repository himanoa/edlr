import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { parseRpcResponse, RpcClient } from "./rpc";

type Listener = (ev: { data: string }) => void;

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  url: string;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: Listener | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }

  send(data: string) {
    this.sent.push(data);
  }

  close() {
    this.onclose?.();
  }

  // test helpers
  triggerOpen() {
    this.onopen?.();
  }

  triggerMessage(data: unknown) {
    this.onmessage?.({ data: JSON.stringify(data) });
  }

  triggerClose() {
    this.onclose?.();
  }
}

beforeEach(() => {
  FakeWebSocket.instances = [];
  vi.stubGlobal("WebSocket", FakeWebSocket);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

test("parseRpcResponse parses rpc-result and rpc-error", () => {
  expect(parseRpcResponse('{"type":"rpc-result","id":1,"result":{"a":1}}')).toEqual({
    type: "rpc-result",
    id: 1,
    result: { a: 1 },
  });
  expect(parseRpcResponse('{"type":"rpc-error","id":2,"error":"boom"}')).toEqual({
    type: "rpc-error",
    id: 2,
    error: "boom",
  });
});

test("parseRpcResponse returns null for events, hello, and malformed json", () => {
  expect(parseRpcResponse('{"type":"event","kind":"status","raw":{}}')).toBeNull();
  expect(parseRpcResponse('{"type":"hello","protocol":1}')).toBeNull();
  expect(parseRpcResponse("not json")).toBeNull();
});

test("call sends an rpc frame and resolves on matching rpc-result", async () => {
  const client = new RpcClient("ws://x/ws");
  const ws = FakeWebSocket.instances[0];
  ws.triggerOpen();

  const promise = client.call<{ ok: boolean }>("ping", { foo: "bar" });

  expect(ws.sent).toHaveLength(1);
  const frame = JSON.parse(ws.sent[0]);
  expect(frame).toEqual({ type: "rpc", id: 1, method: "ping", params: { foo: "bar" } });

  ws.triggerMessage({ type: "rpc-result", id: 1, result: { ok: true } });

  await expect(promise).resolves.toEqual({ ok: true });
});

test("call rejects with Error(message) on rpc-error", async () => {
  const client = new RpcClient("ws://x/ws");
  const ws = FakeWebSocket.instances[0];
  ws.triggerOpen();

  const promise = client.call("fail");
  ws.triggerMessage({ type: "rpc-error", id: 1, error: "boom" });

  await expect(promise).rejects.toThrow("boom");
});

test("concurrent calls resolve to the correct promise even when responses arrive out of order", async () => {
  const client = new RpcClient("ws://x/ws");
  const ws = FakeWebSocket.instances[0];
  ws.triggerOpen();

  const p1 = client.call("first");
  const p2 = client.call("second");

  ws.triggerMessage({ type: "rpc-result", id: 2, result: "second-result" });
  ws.triggerMessage({ type: "rpc-result", id: 1, result: "first-result" });

  await expect(p1).resolves.toBe("first-result");
  await expect(p2).resolves.toBe("second-result");
});

test("call queues before the socket opens and flushes on open", async () => {
  const client = new RpcClient("ws://x/ws");
  const ws = FakeWebSocket.instances[0];

  const promise = client.call("queued");
  expect(ws.sent).toHaveLength(0);

  ws.triggerOpen();
  expect(ws.sent).toHaveLength(1);

  ws.triggerMessage({ type: "rpc-result", id: 1, result: "done" });
  await expect(promise).resolves.toBe("done");
});

test("call rejects with a timeout when no response arrives in time", async () => {
  vi.useFakeTimers();
  const client = new RpcClient("ws://x/ws", 1000);
  const ws = FakeWebSocket.instances[0];
  ws.triggerOpen();

  const promise = client.call("slow");
  const assertion = expect(promise).rejects.toThrow(/timed out/i);

  await vi.advanceTimersByTimeAsync(1000);
  await assertion;
});

test("a remote/unexpected socket close rejects all pending calls with a remote-close message", async () => {
  const client = new RpcClient("ws://x/ws");
  const ws = FakeWebSocket.instances[0];
  ws.triggerOpen();

  const p1 = client.call("a");
  const p2 = client.call("b");

  ws.triggerClose();

  await expect(p1).rejects.toThrow(/websocket closed/i);
  await expect(p2).rejects.toThrow(/websocket closed/i);
});

test("client.close() rejects in-flight calls with a caller-initiated message", async () => {
  const client = new RpcClient("ws://x/ws");
  const ws = FakeWebSocket.instances[0];
  ws.triggerOpen();

  const p1 = client.call("a");
  const p2 = client.call("b");

  client.close();

  await expect(p1).rejects.toThrow(/rpcclient closed/i);
  await expect(p2).rejects.toThrow(/rpcclient closed/i);
});

test("call() after close() rejects immediately and does not send", async () => {
  const client = new RpcClient("ws://x/ws");
  const ws = FakeWebSocket.instances[0];
  ws.triggerOpen();

  client.close();
  const sentBeforeCall = ws.sent.length;

  const promise = client.call("late");

  await expect(promise).rejects.toThrow(/closed/i);
  expect(ws.sent).toHaveLength(sentBeforeCall);
});
