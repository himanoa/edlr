import { afterEach, describe, expect, it } from "vitest";
import { invoke, isTauri } from "./tauri";

afterEach(() => {
  delete (globalThis as Record<string, unknown>).__TAURI_INTERNALS__;
});

describe("isTauri", () => {
  it("returns false in a plain browser or jsdom", () => {
    expect(isTauri()).toBe(false);
  });

  it("returns true when the Tauri internals bridge is present", () => {
    (globalThis as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    expect(isTauri()).toBe(true);
  });
});

describe("invoke", () => {
  it("rejects when the Tauri bridge is absent", async () => {
    await expect(invoke("get_config")).rejects.toThrow(/Tauri/);
  });
});
