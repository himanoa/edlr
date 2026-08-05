import { createStore } from "jotai";
import { describe, expect, it, vi } from "vitest";
import type { RpcClient } from "@/rpc";
import { createRpcClient$ } from "./rpcClient";

describe("createRpcClient$", () => {
  it("最初の購読で接続し、購読が無くなったら close される", () => {
    const client = { close: vi.fn() } as unknown as RpcClient;
    const makeClient = vi.fn(() => client);
    const rpcClient$ = createRpcClient$(makeClient);
    const store = createStore();

    expect(store.get(rpcClient$)).toBeNull();

    const unsub = store.sub(rpcClient$, () => {});
    expect(makeClient).toHaveBeenCalledTimes(1);
    expect(store.get(rpcClient$)).toBe(client);

    unsub();
    expect(client.close).toHaveBeenCalledTimes(1);
    expect(store.get(rpcClient$)).toBeNull();
  });
});
