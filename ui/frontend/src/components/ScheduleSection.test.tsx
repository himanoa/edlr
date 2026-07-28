import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ScheduleSection, formatNext } from "./ScheduleSection";

/** テストの基準時刻。相対表記の期待値はすべてこれを起点にする。 */
const NOW = new Date("2026-07-29T12:00:00");

function at(offsetMs: number): string {
  return new Date(NOW.getTime() + offsetMs).toISOString();
}

afterEach(() => {
  vi.useRealTimers();
});

describe("formatNext", () => {
  it("renders sub-minute deltas in seconds", () => {
    expect(formatNext(at(42_000), NOW)).toBe("in 42s");
  });

  it("renders minutes with zero-padded seconds", () => {
    expect(formatNext(at(3 * 60_000 + 5_000), NOW)).toBe("in 3m 05s");
  });

  it("renders hours with zero-padded minutes", () => {
    expect(formatNext(at(2 * 3_600_000 + 7 * 60_000), NOW)).toBe("in 2h 07m");
  });

  it("marks an already-passed next fire as due rather than a negative delta", () => {
    expect(formatNext(at(-5_000), NOW)).toBe("due");
  });

  it("appends the absolute time when the next fire is not today", () => {
    // 明日の 09:00 — 素の HH:MM だと「5分後」と区別できなかったケース。
    const tomorrow9am = new Date("2026-07-30T09:00:00");
    expect(formatNext(tomorrow9am.toISOString(), NOW)).toBe("in 21h 00m (2026-07-30 09:00)");
  });

  it("falls back to the raw string when next cannot be parsed", () => {
    expect(formatNext("not-a-timestamp", NOW)).toBe("not-a-timestamp");
  });
});

describe("ScheduleSection", () => {
  it("renders nothing when the plugin declares no schedules", () => {
    const { container } = render(<ScheduleSection schedules={[]} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders each schedule's name, spec and next fire", () => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
    render(
      <ScheduleSection
        schedules={[
          { name: "flush", spec: "every 60s", next: at(42_000) },
          { name: "daily-report", spec: "0 9 * * *", next: new Date("2026-07-30T09:00:00").toISOString() },
        ]}
      />,
    );
    expect(screen.getByText(/flush — every 60s \(next in 42s\)/)).toBeInTheDocument();
    expect(
      screen.getByText(/daily-report — 0 9 \* \* \* \(next in 21h 00m \(2026-07-30 09:00\)\)/),
    ).toBeInTheDocument();
  });

  it("ticks so the countdown follows the clock without a new RPC", () => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
    render(<ScheduleSection schedules={[{ name: "flush", spec: "every 60s", next: at(42_000) }]} />);
    expect(screen.getByText(/next in 42s/)).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    expect(screen.getByText(/next in 32s/)).toBeInTheDocument();
  });
});
