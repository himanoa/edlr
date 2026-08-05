import { createStore } from "jotai";
import { describe, expect, it, vi } from "vitest";
import type { PluginsList } from "@/types/plugin";
import { createPluginList$ } from "./pluginList";

const fakeList: PluginsList = { pluginsDir: "/plugins", plugins: [] };

describe("createPluginList$", () => {
  it("読み取りで plugins/list の結果が得られ、取得後に close される", async () => {
    const client = {
      call: vi.fn().mockResolvedValue(fakeList),
      close: vi.fn(),
    };
    const pluginList$ = createPluginList$(() => client);
    const store = createStore();

    await expect(store.get(pluginList$)).resolves.toEqual(fakeList);
    expect(client.call).toHaveBeenCalledWith("plugins/list");
    expect(client.close).toHaveBeenCalledTimes(1);
  });

  it("plugins/list が失敗したら読み取りが reject される", async () => {
    const client = {
      call: vi.fn().mockRejectedValue(new Error("connection refused")),
      close: vi.fn(),
    };
    const pluginList$ = createPluginList$(() => client);
    const store = createStore();

    await expect(store.get(pluginList$)).rejects.toThrow("connection refused");
    expect(client.close).toHaveBeenCalledTimes(1);
  });

  it("書き込みで一覧を差し替えられ、以後の読み取りに反映される", async () => {
    const client = {
      call: vi.fn().mockResolvedValue(fakeList),
      close: vi.fn(),
    };
    const pluginList$ = createPluginList$(() => client);
    const store = createStore();

    await store.set(pluginList$, (prev) => ({ ...prev, pluginsDir: "/patched" }));

    // 上書き後の読み取りは同期値になる(local$ が埋まっているため)
    expect(await store.get(pluginList$)).toEqual({ ...fakeList, pluginsDir: "/patched" });
    expect(client.call).toHaveBeenCalledTimes(1);
  });
});
