import { afterEach, describe, expect, it } from "vitest";
import { isTauri } from "./tauri";

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
