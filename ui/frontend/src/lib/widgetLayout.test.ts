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
