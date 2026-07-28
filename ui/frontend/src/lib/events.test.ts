import { describe, expect, it } from "vitest";
import { matchesEvent } from "./events";
import type { LogEntry } from "./filter";

const journal = (event: string): LogEntry => ({
  id: 1,
  kind: "journal",
  timestamp: "2026-07-28T00:00:00Z",
  event,
  raw: {},
});
const status: LogEntry = { id: 2, kind: "status", raw: {} };

describe("matchesEvent", () => {
  it("matches exact journal event names", () => {
    expect(matchesEvent(["FSDJump"], journal("FSDJump"))).toBe(true);
    expect(matchesEvent(["FSDJump"], journal("Docked"))).toBe(false);
  });

  it("wildcard matches any journal event but not status", () => {
    expect(matchesEvent(["*"], journal("Docked"))).toBe(true);
    expect(matchesEvent(["*"], status)).toBe(false);
  });

  it("status pattern matches only status events", () => {
    expect(matchesEvent(["status"], status)).toBe(true);
    // Rust 実装(matches_event)では journal 側は完全一致判定なので、
    // journal イベント名が偶然 "status" ならマッチする。同じ規則に揃える。
    expect(matchesEvent(["status"], journal("status"))).toBe(true);
    expect(matchesEvent(["status"], journal("Docked"))).toBe(false);
  });

  it("empty list matches nothing", () => {
    expect(matchesEvent([], journal("Docked"))).toBe(false);
    expect(matchesEvent([], status)).toBe(false);
  });
});
