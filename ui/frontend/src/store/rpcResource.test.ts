import { createStore } from "jotai";
import { describe, expect, it, vi } from "vitest";
import { createRpcResource$ } from "./rpcResource";

const makeFake = (impl: () => Promise<{ value: number }>) => {
  const client = { fetch: vi.fn(impl), close: vi.fn() };
  return client;
};

describe("createRpcResource$", () => {
  it("読み取りで fetcher の結果が得られ、取得後に client が close される", async () => {
    const client = makeFake(() => Promise.resolve({ value: 1 }));
    const resource$ = createRpcResource$((c: typeof client) => c.fetch(), () => client);
    const store = createStore();

    await expect(store.get(resource$)).resolves.toEqual({ value: 1 });
    expect(client.fetch).toHaveBeenCalledTimes(1);
    expect(client.close).toHaveBeenCalledTimes(1);
  });

  it("fetcher が失敗しても close され、読み取りは reject される", async () => {
    const client = makeFake(() => Promise.reject(new Error("boom")));
    const resource$ = createRpcResource$((c: typeof client) => c.fetch(), () => client);
    const store = createStore();

    await expect(store.get(resource$)).rejects.toThrow("boom");
    expect(client.close).toHaveBeenCalledTimes(1);
  });

  it("書き込みで値を差し替えられ、以後の読み取りは同期値になる", async () => {
    const client = makeFake(() => Promise.resolve({ value: 1 }));
    const resource$ = createRpcResource$((c: typeof client) => c.fetch(), () => client);
    const store = createStore();

    await store.set(resource$, (prev) => ({ value: prev.value + 10 }));

    expect(await store.get(resource$)).toEqual({ value: 11 });
    // 書き込み時の prev 解決も含めて fetch は 1 回だけ
    expect(client.fetch).toHaveBeenCalledTimes(1);
  });

  it("書き込みは前回の書き込み結果に積み重なる", async () => {
    const client = makeFake(() => Promise.resolve({ value: 1 }));
    const resource$ = createRpcResource$((c: typeof client) => c.fetch(), () => client);
    const store = createStore();

    await store.set(resource$, (prev) => ({ value: prev.value + 1 }));
    await store.set(resource$, (prev) => ({ value: prev.value * 3 }));

    expect(await store.get(resource$)).toEqual({ value: 6 });
  });
});
