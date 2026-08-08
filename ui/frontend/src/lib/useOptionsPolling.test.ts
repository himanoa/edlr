import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SettingField } from "@/types/plugin";
import { useOptionsPolling } from "./useOptionsPolling";

const select = (options: { value: string; label: string }[] | null): SettingField => ({
  type: "select",
  key: "speaker",
  label: "話者",
  default: "",
  options,
  optionsFrom: { driver: "coeiroink", topic: "speakers" },
});

describe("useOptionsPolling", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("options が未着(null)の間はポーリングし、届いたら止まる", () => {
    const refetch = vi.fn().mockResolvedValue(undefined);
    const { rerender } = renderHook(({ settings }) => useOptionsPolling(settings, refetch, 1000), {
      initialProps: { settings: [select(null)] },
    });

    vi.advanceTimersByTime(2500);
    expect(refetch).toHaveBeenCalledTimes(2);

    rerender({ settings: [select([{ value: "a", label: "A" }])] });
    vi.advanceTimersByTime(5000);
    expect(refetch).toHaveBeenCalledTimes(2);
  });

  it("未着の select が無ければ何もしない", () => {
    const refetch = vi.fn().mockResolvedValue(undefined);
    renderHook(() => useOptionsPolling([select([])], refetch, 1000));
    vi.advanceTimersByTime(5000);
    expect(refetch).not.toHaveBeenCalled();
  });
});
